//! Two-way sync with Google Tasks, so tasks show up on the phone and in the
//! Google Calendar "Tasks" view.
//!
//! - One Google list per project (named like the project), plus a default list
//!   (`calendar.tasks_list_name`) for tasks without a project. The app only
//!   ever reads or writes lists it created itself (ids stored in `gtask_lists`).
//! - Local → Google: open tasks are created; edits, completion and
//!   cancellation are pushed.
//! - Google → local: ticking, unticking, renaming, re-dating or deleting a task
//!   on the phone is applied locally (logged, undoable); tasks added in one of
//!   these lists are imported.
//! - Change detection: `gtasks.local_updated` / `remote_updated` remember what
//!   each side looked like at the last sync, so nothing ping-pongs.

use crate::actions;
use crate::brain::Brain;
use crate::calendar::Backend;
use crate::memory::{self, ApplyCtx, Item};
use crate::util::parse_date;
use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

const API: &str = "https://tasks.googleapis.com/tasks/v1";

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTask {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// "needsAction" | "completed"
    pub status: String,
    pub due: Option<NaiveDate>,
    pub deleted: bool,
    /// Opaque change marker (RFC 3339 time of the last change).
    pub updated: String,
}

#[derive(Debug, Default)]
pub struct TasksReport {
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
    pub done_remote: usize,
    pub reopened_remote: usize,
    pub edited_remote: usize,
    pub deleted_remote: usize,
    pub imported: usize,
}

impl TasksReport {
    pub fn changes(&self) -> usize {
        self.created
            + self.updated
            + self.removed
            + self.done_remote
            + self.reopened_remote
            + self.edited_remote
            + self.deleted_remote
            + self.imported
    }
    pub fn summary(&self) -> String {
        format!(
            "Google Tasks: {} sent, {} updated, {} removed · from your phone: {} done, {} reopened, {} edited, {} deleted, {} new",
            self.created,
            self.updated,
            self.removed,
            self.done_remote,
            self.reopened_remote,
            self.edited_remote,
            self.deleted_remote,
            self.imported
        )
    }
}

fn seg(parts: &[&str]) -> Result<String> {
    let mut url = reqwest::Url::parse(API)?;
    url.path_segments_mut().map_err(|_| anyhow::anyhow!("bad url"))?.extend(parts);
    Ok(url.to_string())
}

fn parse_remote(v: &Value) -> Option<RemoteTask> {
    Some(RemoteTask {
        id: v["id"].as_str()?.to_string(),
        title: v["title"].as_str().unwrap_or("").trim().to_string(),
        notes: v["notes"].as_str().unwrap_or("").to_string(),
        status: v["status"].as_str().unwrap_or("needsAction").to_string(),
        due: v["due"].as_str().and_then(|d| parse_date(d.get(..10)?)),
        deleted: v["deleted"].as_bool().unwrap_or(false),
        updated: v["updated"].as_str().unwrap_or("").to_string(),
    })
}

fn is_not_found(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("404") || s.contains("410")
}

impl Backend {
    fn tasks_ready(&self) -> Result<()> {
        if let Backend::Google(g) = self
            && !g.has_tasks_scope()
        {
            bail!("Google Tasks is not authorized yet: run `sb calendar auth` once and allow Tasks");
        }
        Ok(())
    }

    pub fn create_tasklist(&self, name: &str) -> Result<String> {
        match self {
            Backend::Fake(f) => {
                let mut f = f.borrow_mut();
                let id = format!("list-{}", f.lists.len());
                f.lists.push((id.clone(), name.to_string()));
                Ok(id)
            }
            Backend::Google(g) => {
                let v = g.req("POST", &seg(&["users", "@me", "lists"])?, Some(json!({ "title": name })))?;
                Ok(v["id"].as_str().context("no task list id")?.to_string())
            }
        }
    }

    pub fn list_tasks(&self, list: &str) -> Result<Vec<RemoteTask>> {
        match self {
            Backend::Fake(f) => {
                let f = f.borrow();
                if !f.lists.iter().any(|(id, _)| id == list) {
                    bail!("Google API 404 Not Found: list");
                }
                let mut v: Vec<RemoteTask> =
                    f.tasks.values().filter(|(l, _)| l == list).map(|(_, t)| t.clone()).collect();
                v.sort_by(|a, b| a.id.cmp(&b.id));
                Ok(v)
            }
            Backend::Google(g) => {
                let mut out = vec![];
                let mut page: Option<String> = None;
                loop {
                    let mut url = reqwest::Url::parse(&seg(&["lists", list, "tasks"])?)?;
                    url.query_pairs_mut()
                        .append_pair("showCompleted", "true")
                        .append_pair("showHidden", "true")
                        .append_pair("showDeleted", "true")
                        .append_pair("maxResults", "100");
                    if let Some(p) = &page {
                        url.query_pairs_mut().append_pair("pageToken", p);
                    }
                    let v = g.req("GET", url.as_str(), None)?;
                    out.extend(v["items"].as_array().into_iter().flatten().filter_map(parse_remote));
                    match v["nextPageToken"].as_str() {
                        Some(p) => page = Some(p.to_string()),
                        None => break,
                    }
                }
                Ok(out)
            }
        }
    }

    pub fn put_task(&self, list: &str, id: Option<&str>, body: Value) -> Result<RemoteTask> {
        match self {
            Backend::Fake(f) => {
                let mut f = f.borrow_mut();
                f.seq += 1;
                let seq = f.seq;
                let tid = id.map(String::from).unwrap_or_else(|| format!("t{seq}"));
                let t = RemoteTask {
                    id: tid.clone(),
                    title: body["title"].as_str().unwrap_or("").into(),
                    notes: body["notes"].as_str().unwrap_or("").into(),
                    status: body["status"].as_str().unwrap_or("needsAction").into(),
                    due: body["due"].as_str().and_then(|d| parse_date(d.get(..10)?)),
                    deleted: false,
                    updated: format!("u{seq}"),
                };
                f.tasks.insert(tid, (list.to_string(), t.clone()));
                Ok(t)
            }
            Backend::Google(g) => {
                let v = match id {
                    Some(i) => g.req("PATCH", &seg(&["lists", list, "tasks", i])?, Some(body))?,
                    None => g.req("POST", &seg(&["lists", list, "tasks"])?, Some(body))?,
                };
                parse_remote(&v).context("bad task response")
            }
        }
    }

    pub fn delete_task(&self, list: &str, id: &str) -> Result<()> {
        match self {
            Backend::Fake(f) => {
                f.borrow_mut().tasks.remove(id);
                Ok(())
            }
            Backend::Google(g) => match g.req("DELETE", &seg(&["lists", list, "tasks", id])?, None) {
                Err(e) if is_not_found(&e) => Ok(()),
                r => r.map(|_| ()),
            },
        }
    }
}

struct Local {
    notes: Option<String>,
    id: i64,
    kind: String,
    title: String,
    status: String,
    due: Option<String>,
    priority: i64,
    estimate_min: Option<i64>,
    project_id: Option<i64>,
}

fn load(conn: &rusqlite::Connection, id: i64) -> Result<Local> {
    Ok(conn.query_row(
        "SELECT id, kind, title, status, due, priority, estimate_min, project_id, notes FROM tasks WHERE id=?1",
        [id],
        |r| {
            Ok(Local {
                id: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                status: r.get(3)?,
                due: r.get(4)?,
                priority: r.get(5)?,
                estimate_min: r.get(6)?,
                project_id: r.get(7)?,
                notes: r.get(8)?,
            })
        },
    )?)
}

fn list_key(project: Option<i64>) -> String {
    project.map(|p| format!("p{p}")).unwrap_or_else(|| "default".into())
}

/// The Google list for a project (or the default list), created on first use.
fn ensure_list(brain: &Brain, be: &Backend, project: Option<i64>) -> Result<String> {
    let key = list_key(project);
    if let Some(id) = brain
        .conn
        .query_row("SELECT list_id FROM gtask_lists WHERE key=?1", [&key], |r| r.get::<_, String>(0))
        .optional()?
    {
        return Ok(id);
    }
    let name = match project {
        Some(p) => brain.conn.query_row("SELECT name FROM entities WHERE id=?1", [p], |r| r.get::<_, String>(0))?,
        None => brain.cfg.calendar.tasks_list_name.clone(),
    };
    let id = be.create_tasklist(&name)?;
    brain.conn.execute(
        "INSERT INTO gtask_lists(key, list_id, name, project_id) VALUES (?1, ?2, ?3, ?4)",
        params![key, id, name, project],
    )?;
    actions::log(&brain.conn, "system", "gtasks_list_created", &format!("Google Tasks list '{name}' created"), None)?;
    Ok(id)
}

fn body(brain: &Brain, t: &Local) -> Value {
    let fr = brain.cfg.language.to_lowercase().starts_with("fr");
    let mut notes = vec![format!("Second Brain #{}", t.id), format!("p{}", t.priority)];
    if let Some(e) = t.estimate_min {
        notes.push(format!("~{}", crate::tasks::fmt_min(e)));
    }
    if t.kind == "deadline" {
        notes.push((if fr { "échéance fixe" } else { "fixed deadline" }).into());
    }
    if t.status == "proposed" {
        notes.push((if fr { "à confirmer" } else { "to confirm" }).into());
    }
    // Description first (what you read on the phone), then the Second Brain line.
    let meta = notes.join(" · ");
    let text = match t.notes.as_deref() {
        Some(d) if !d.trim().is_empty() => format!("{}\n\n{meta}", d.trim()),
        _ => meta,
    };
    json!({
        "title": t.title,
        "notes": text,
        "status": if t.status == "done" { "completed" } else { "needsAction" },
        "due": t.due.as_ref().map(|d| format!("{d}T00:00:00.000Z")),
    })
}

/// The user's part of Google notes (without our "Second Brain #id · …" line).
fn description_of(notes: &str) -> String {
    notes.lines().filter(|l| !l.starts_with("Second Brain #")).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Fingerprint of what we send to Google: any change to it means "push again".
/// (A content hash, not updated_at: two edits within the same second must still sync.)
fn signature(t: &Local) -> String {
    crate::util::sha256_hex(&format!(
        "{}\u{1f}{}\u{1f}{:?}\u{1f}{}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{}",
        t.title, t.status, t.due, t.priority, t.estimate_min, t.project_id, t.notes, t.kind
    ))
}

fn remember(brain: &Brain, task: i64, list: &str, r: &RemoteTask) -> Result<()> {
    let local = signature(&load(&brain.conn, task)?);
    brain.conn.execute(
        "INSERT INTO gtasks(task_id, list_id, gtask_id, local_updated, remote_updated) VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(task_id) DO UPDATE SET list_id=?2, gtask_id=?3, local_updated=?4, remote_updated=?5",
        params![task, list, r.id, local, r.updated],
    )?;
    Ok(())
}

/// Apply what changed on Google since the last sync.
fn pull(brain: &Brain, be: &Backend, rep: &mut TasksReport) -> Result<()> {
    // Tasks deleted locally but not yet on Google must not come back as "new from the phone".
    let gone: std::collections::HashSet<String> = crate::db::kv_get(&brain.conn, "gtasks_tombstones")?
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.get(1).and_then(|x| x.as_str()).map(String::from))
        .collect();
    let lists: Vec<(String, Option<i64>)> = brain
        .conn
        .prepare("SELECT list_id, project_id FROM gtask_lists")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (list, project) in lists {
        let remote = match be.list_tasks(&list) {
            Ok(v) => v,
            Err(e) if is_not_found(&e) => {
                // The user deleted the whole list: forget it; open tasks get re-sent to a new one.
                brain.conn.execute("DELETE FROM gtasks WHERE list_id=?1", [&list])?;
                brain.conn.execute("DELETE FROM gtask_lists WHERE list_id=?1", [&list])?;
                continue;
            }
            Err(e) => return Err(e),
        };
        for rt in remote.into_iter().filter(|rt| !gone.contains(&rt.id)) {
            let mapped: Option<(i64, String)> = brain
                .conn
                .query_row("SELECT task_id, remote_updated FROM gtasks WHERE gtask_id=?1", [&rt.id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            match mapped {
                Some((_, seen)) if seen == rt.updated => {}
                Some((tid, _)) => {
                    let t = load(&brain.conn, tid)?;
                    let active = t.status == "open" || t.status == "proposed";
                    if rt.deleted {
                        brain.conn.execute("DELETE FROM gtasks WHERE task_id=?1", [tid])?;
                        if active {
                            crate::tasks::set_status(brain, tid, "cancelled")?;
                            rep.deleted_remote += 1;
                        }
                        continue;
                    }
                    if rt.status == "completed" && active {
                        crate::tasks::set_status(brain, tid, "done")?;
                        rep.done_remote += 1;
                    } else if rt.status == "needsAction" && t.status == "done" {
                        crate::tasks::set_status(brain, tid, "open")?;
                        rep.reopened_remote += 1;
                    }
                    let due = rt.due.map(|d| d.to_string());
                    let new_title = (!rt.title.is_empty() && rt.title != t.title).then(|| rt.title.clone());
                    let new_due = (due != t.due).then(|| due.clone().unwrap_or_else(|| "none".into()));
                    let desc = description_of(&rt.notes);
                    let desc_changed = desc != t.notes.as_deref().unwrap_or("").trim();
                    let fields_changed = new_title.is_some() || new_due.is_some();
                    if fields_changed {
                        crate::tasks::edit(brain, tid, new_title, new_due, None, None, None)?;
                    }
                    if desc_changed {
                        crate::tasks::set_notes(brain, tid, &desc)?;
                    }
                    if fields_changed || desc_changed {
                        rep.edited_remote += 1;
                    }
                    remember(brain, tid, &list, &rt)?;
                }
                None if rt.deleted || rt.status == "completed" || rt.title.is_empty() => {}
                None => {
                    // Added on the phone: import it into this list's project.
                    let tid = import(brain, &rt, project)?;
                    let desc = description_of(&rt.notes);
                    if !desc.is_empty() {
                        crate::tasks::set_notes(brain, tid, &desc)?;
                    }
                    remember(brain, tid, &list, &rt)?;
                    rep.imported += 1;
                }
            }
        }
    }
    Ok(())
}

fn import(brain: &Brain, rt: &RemoteTask, project: Option<i64>) -> Result<i64> {
    let proj = match project {
        Some(p) => Some(brain.conn.query_row("SELECT name FROM entities WHERE id=?1", [p], |r| r.get::<_, String>(0))?),
        None => None,
    };
    let item = Item::Task {
        b: 1,
        title: rt.title.clone(),
        due: rt.due.map(|d| d.to_string()),
        est: None,
        proj,
        pri: None,
        firm: true,
        done: false,
        cancelled: false,
        task_ref: None,
    };
    let item = memory::validate(item, 1, brain.today()).map_err(|e| anyhow::anyhow!(e))?;
    let before: i64 = brain.conn.query_row("SELECT coalesce(max(id), 0) FROM tasks", [], |r| r.get(0))?;
    let tx = brain.conn.unchecked_transaction()?;
    memory::apply(&tx, &[item], &ApplyCtx { block_ids: &[0], entry_date: brain.today(), origin: "gtasks" })?;
    tx.commit()?;
    crate::tasks::assign_default_project(brain)?;
    let mut id: i64 = brain.conn.query_row("SELECT coalesce(max(id), 0) FROM tasks", [], |r| r.get(0))?;
    if id <= before {
        // Same task already exists locally (dedupe): link to it instead.
        id = brain
            .conn
            .query_row(
                "SELECT id FROM tasks WHERE norm=?1 ORDER BY id DESC LIMIT 1",
                [crate::util::normalize(&rt.title)],
                |r| r.get(0),
            )
            .optional()?
            .context("task from Google Tasks could not be created")?;
    }
    crate::notes::regenerate_dirty(brain)?;
    Ok(id)
}

/// Send local changes to Google.
fn push(brain: &Brain, be: &Backend, rep: &mut TasksReport) -> Result<()> {
    rep.removed += crate::tasks::process_tombstones(&brain.conn, "gtasks_tombstones", |t| {
        match (t.get(0).and_then(|x| x.as_str()), t.get(1).and_then(|x| x.as_str())) {
            (Some(list), Some(gid)) => be.delete_task(list, gid),
            _ => Ok(()),
        }
    })?;
    type Row = (i64, Option<String>, Option<String>, Option<String>);
    let rows: Vec<Row> = brain
        .conn
        .prepare(
            "SELECT t.id, g.gtask_id, g.list_id, g.local_updated FROM tasks t LEFT JOIN gtasks g ON g.task_id=t.id
             WHERE t.status IN ('open','proposed') OR g.task_id IS NOT NULL ORDER BY t.id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (tid, gid, list, seen) in rows {
        let t = load(&brain.conn, tid)?;
        match (gid, list) {
            (Some(gid), Some(list)) => {
                if t.status == "cancelled" {
                    be.delete_task(&list, &gid)?;
                    brain.conn.execute("DELETE FROM gtasks WHERE task_id=?1", [tid])?;
                    rep.removed += 1;
                    continue;
                }
                if seen.as_deref() == Some(signature(&t).as_str()) {
                    continue;
                }
                let target = ensure_list(brain, be, t.project_id)?;
                let r = if target != list {
                    // Project changed: move it to the other list.
                    be.delete_task(&list, &gid)?;
                    be.put_task(&target, None, body(brain, &t))?
                } else {
                    be.put_task(&list, Some(&gid), body(brain, &t))?
                };
                remember(brain, tid, &target, &r)?;
                rep.updated += 1;
            }
            _ => {
                if t.status != "open" && t.status != "proposed" {
                    continue;
                }
                let list = ensure_list(brain, be, t.project_id)?;
                let r = be.put_task(&list, None, body(brain, &t))?;
                remember(brain, tid, &list, &r)?;
                rep.created += 1;
            }
        }
    }
    Ok(())
}

/// Full two-way sync: Google changes first (your phone wins on conflicts), then local ones.
pub fn sync(brain: &Brain, be: &Backend) -> Result<TasksReport> {
    be.tasks_ready()?;
    let mut rep = TasksReport::default();
    crate::ui::status("Google Tasks · reading your lists");
    pull(brain, be, &mut rep)?;
    crate::ui::status("Google Tasks · sending your tasks");
    push(brain, be, &mut rep)?;
    if rep.changes() > 0 {
        actions::log(&brain.conn, "system", "gtasks_sync", &rep.summary(), None)?;
    }
    crate::db::kv_set(&brain.conn, "gtasks_last_sync", &crate::util::ts(brain.now()))?;
    Ok(rep)
}
