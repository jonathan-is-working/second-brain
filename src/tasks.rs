//! Operational memory: task queries, deterministic prioritization, and
//! explicit user commands (done/reopen/edit/add). Task status in SQLite is
//! authoritative.

use crate::actions::{self, TaskSnapshot, Undo};
use crate::brain::Brain;
use crate::memory::{self, ApplyCtx, Item};
use crate::util::{parse_date, ts};
use anyhow::{Result, bail};
use chrono::{NaiveDate, Utc};
use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub status: String,
    pub priority: i64,
    pub due: Option<NaiveDate>,
    pub estimate_min: Option<i64>,
    pub estimate_source: Option<String>,
    pub project: Option<String>,
    pub done_min: i64,
    pub planned_min: i64,
    pub completed_at: Option<String>,
    /// Description (Markdown).
    pub notes: Option<String>,
}

impl TaskRow {
    /// Minutes of work still needed, with the configured default when no estimate exists.
    pub fn remaining(&self, default_est: i64) -> i64 {
        (self.estimate_min.unwrap_or(default_est) - self.done_min).max(0)
    }
    pub fn estimate_assumed(&self) -> bool {
        self.estimate_min.is_none()
    }
}

pub fn query(conn: &Connection, filter: &str, now: &str) -> Result<Vec<TaskRow>> {
    let sql = format!(
        "SELECT t.id, t.kind, t.title, t.status, t.priority, t.due, t.estimate_min, t.estimate_source, e.name,
           coalesce((SELECT sum((julianday(s.end)-julianday(s.start))*1440) FROM sessions s WHERE s.task_id=t.id AND s.status='done'),0),
           coalesce((SELECT sum((julianday(s.end)-julianday(s.start))*1440) FROM sessions s WHERE s.task_id=t.id AND s.status='planned' AND s.end > ?1),0),
           t.completed_at, t.notes
         FROM tasks t LEFT JOIN entities e ON e.id=t.project_id WHERE {filter} ORDER BY t.id"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map([now], |r| {
            let due: Option<String> = r.get(5)?;
            Ok(TaskRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                status: r.get(3)?,
                priority: r.get(4)?,
                due: due.and_then(|d| parse_date(&d)),
                estimate_min: r.get(6)?,
                estimate_source: r.get(7)?,
                project: r.get(8)?,
                done_min: r.get::<_, f64>(9)?.round() as i64,
                planned_min: r.get::<_, f64>(10)?.round() as i64,
                completed_at: r.get(11)?,
                notes: r.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn open_tasks(brain: &Brain) -> Result<Vec<TaskRow>> {
    query(&brain.conn, "t.status IN ('open','proposed')", &ts(brain.now()))
}

/// Deterministic priority score (higher = more pressing). Combines user
/// importance, deadline proximity, and remaining workload vs. time left.
pub fn score(t: &TaskRow, today: NaiveDate, default_est: i64) -> i64 {
    let importance = (5 - t.priority) * 10; // p1=40 .. p4=10
    let urgency = match t.due {
        None => 0,
        Some(d) => {
            let days = (d - today).num_days();
            if days < 0 { 60 } else { (14 - days.min(14)) * 4 }
        }
    };
    // Work that can't fit comfortably in the days left gets a boost.
    let pressure = match t.due {
        Some(d) if t.kind == "task" => {
            let days = (d - today).num_days().max(0) + 1;
            ((t.remaining(default_est) / days) / 30).min(20)
        }
        _ => 0,
    };
    let proposed = if t.status == "proposed" { -15 } else { 0 };
    importance + urgency + pressure + proposed
}

pub fn ranked(brain: &Brain) -> Result<Vec<TaskRow>> {
    let today = brain.today();
    let d = brain.cfg.schedule.default_estimate_minutes;
    let mut v: Vec<TaskRow> = open_tasks(brain)?.into_iter().filter(|t| t.kind == "task").collect();
    v.sort_by_key(|t| (-score(t, today, d), t.due, t.id));
    Ok(v)
}

pub fn deadlines(brain: &Brain, days: i64) -> Result<Vec<TaskRow>> {
    let until = brain.today() + chrono::Duration::days(days);
    let mut v: Vec<TaskRow> = open_tasks(brain)?.into_iter().filter(|t| t.due.is_some_and(|d| d <= until)).collect();
    v.sort_by_key(|t| (t.due, t.priority));
    Ok(v)
}

pub fn overdue(brain: &Brain) -> Result<Vec<TaskRow>> {
    let today = brain.today();
    Ok(open_tasks(brain)?.into_iter().filter(|t| t.kind == "task" && t.due.is_some_and(|d| d < today)).collect())
}

pub fn set_status(brain: &Brain, id: i64, status: &str) -> Result<String> {
    let old = TaskSnapshot::load(&brain.conn, id).map_err(|_| anyhow::anyhow!("no task #{id}"))?;
    if old.status == status {
        bail!("task #{id} is already {status}");
    }
    let tx = brain.conn.unchecked_transaction()?;
    let now = ts(brain.now());
    tx.execute(
        "UPDATE tasks SET status=?2, completed_at=?3, updated_at=?4 WHERE id=?1",
        params![id, status, (status == "done").then(|| now.clone()), now],
    )?;
    if status == "done" || status == "cancelled" {
        tx.execute(
            "UPDATE sessions SET status='cancelled' WHERE task_id=?1 AND status='planned' AND start > ?2",
            params![id, now],
        )?;
    }
    if let Some(p) = old.project_id {
        tx.execute("UPDATE entities SET dirty=1 WHERE id=?1", [p])?;
    }
    let msg = format!("#{id} {} → {status}", old.title);
    actions::log(&tx, "user", "task_status", &msg, Some(Undo::RestoreTask { snapshot: old }))?;
    tx.commit()?;
    crate::notes::regenerate_dirty(brain)?;
    Ok(msg)
}

#[allow(clippy::too_many_arguments)]
pub fn edit(
    brain: &Brain,
    id: i64,
    title: Option<String>,
    due: Option<String>,
    est: Option<i64>,
    priority: Option<i64>,
    project: Option<String>,
) -> Result<String> {
    let old = TaskSnapshot::load(&brain.conn, id).map_err(|_| anyhow::anyhow!("no task #{id}"))?;
    let mut new = old.clone();
    if let Some(t) = title {
        new.title = t;
    }
    if let Some(d) = due {
        new.due = if d == "none" {
            None
        } else {
            let date = parse_date(&d)
                .or_else(|| crate::dates::find_dates(&d, brain.today()).first().map(|m| m.date))
                .ok_or_else(|| anyhow::anyhow!("can't understand date '{d}'"))?;
            Some(date.to_string())
        };
    }
    if let Some(e) = est {
        new.estimate_min = Some(e);
        new.estimate_source = Some("user".into());
    }
    if let Some(p) = priority {
        if !(1..=4).contains(&p) {
            bail!("priority must be 1..4");
        }
        new.priority = p;
    }
    let tx = brain.conn.unchecked_transaction()?;
    if let Some(p) = project {
        let mut st = memory::ApplyStats::default();
        new.project_id = Some(memory::ensure_entity(&tx, "project", &p, &mut st)?);
    }
    tx.execute(
        "UPDATE tasks SET title=?2, norm=?3, due=?4, estimate_min=?5, estimate_source=?6, priority=?7, project_id=?8, updated_at=?9 WHERE id=?1",
        params![id, new.title, crate::util::normalize(&new.title), new.due, new.estimate_min, new.estimate_source, new.priority, new.project_id, ts(brain.now())],
    )?;
    crate::db::fts_upsert(&tx, crate::db::DocKind::Task, id, &new.title, "task")?;
    for p in [old.project_id, new.project_id].into_iter().flatten() {
        tx.execute("UPDATE entities SET dirty=1 WHERE id=?1", [p])?;
    }
    let msg = format!("#{id} edited: {}", new.title);
    actions::log(&tx, "user", "task_edited", &msg, Some(Undo::RestoreTask { snapshot: old }))?;
    tx.commit()?;
    crate::notes::regenerate_dirty(brain)?;
    Ok(msg)
}

/// Everything about one task, for `sb task show` and the dashboard.
pub fn describe(brain: &Brain, id: i64) -> Result<String> {
    let t = query(&brain.conn, &format!("t.id = {id}"), &now_str(brain))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no task #{id}"))?;
    let mut out = format!("#{} {}\n", t.id, t.title);
    out.push_str(&format!("status: {} · priority p{}", t.status, t.priority));
    if let Some(d) = t.due {
        out.push_str(&format!(" · due {d}"));
    }
    match t.estimate_min {
        Some(e) => out.push_str(&format!(" · estimate {}", fmt_min(e))),
        None => out.push_str(" · no estimate"),
    }
    if let Some(p) = &t.project {
        out.push_str(&format!(" · project {p}"));
    }
    out.push('\n');
    if t.done_min + t.planned_min > 0 {
        out.push_str(&format!("work: {} done, {} planned\n", fmt_min(t.done_min), fmt_min(t.planned_min)));
    }
    let source: Option<String> = brain
        .conn
        .query_row(
            "SELECT s.path || ':' || b.line FROM tasks t JOIN blocks b ON b.id=t.block_id JOIN sources s ON s.id=b.source_id WHERE t.id=?1",
            [id],
            |r| r.get(0),
        )
        .ok();
    if let Some(s) = source {
        out.push_str(&format!("from: {s}\n"));
    }
    out.push('\n');
    match t.notes.as_deref() {
        Some(n) => out.push_str(n),
        None => {
            out.push_str("(no description: sb task edit {id} --desc \"…\")".replace("{id}", &id.to_string()).as_str())
        }
    }
    out.push('\n');
    Ok(out)
}

/// Remember a remote object to delete at the next sync (Google Tasks / Calendar).
fn tombstone(conn: &Connection, key: &str, entry: serde_json::Value) -> Result<()> {
    let mut v: Vec<serde_json::Value> =
        crate::db::kv_get(conn, key)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    v.push(entry);
    crate::db::kv_set(conn, key, &serde_json::to_string(&v)?)
}

/// Delete a task for good (with its work sessions), also from Google at the next sync. Undoable.
pub fn delete(brain: &Brain, id: i64) -> Result<String> {
    let c = &brain.conn;
    let task = actions::rows_json(c, "SELECT * FROM tasks WHERE id=?1", id)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no task #{id}"))?;
    let sessions = actions::rows_json(c, "SELECT * FROM sessions WHERE task_id=?1", id)?;
    let (title, project): (String, Option<i64>) =
        c.query_row("SELECT title, project_id FROM tasks WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let tx = c.unchecked_transaction()?;
    if let Ok((list, gid)) = tx.query_row("SELECT list_id, gtask_id FROM gtasks WHERE task_id=?1", [id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    }) {
        tombstone(&tx, "gtasks_tombstones", serde_json::json!([list, gid]))?;
    }
    let events: Vec<String> = tx
        .prepare("SELECT gcal_id FROM sessions WHERE task_id=?1 AND gcal_id IS NOT NULL")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for e in events {
        tombstone(&tx, "gcal_tombstones", serde_json::json!(e))?;
    }
    tx.execute("DELETE FROM gtasks WHERE task_id=?1", [id])?;
    tx.execute("DELETE FROM sessions WHERE task_id=?1", [id])?;
    crate::db::fts_delete(&tx, crate::db::DocKind::Task, id)?;
    tx.execute("DELETE FROM tasks WHERE id=?1", [id])?;
    if let Some(p) = project {
        tx.execute("UPDATE entities SET dirty=1 WHERE id=?1", [p])?;
    }
    let msg = format!("#{id} {title} deleted");
    actions::log(&tx, "user", "task_deleted", &msg, Some(Undo::RestoreDeletedTask { task, sessions }))?;
    tx.commit()?;
    crate::notes::regenerate_dirty(brain)?;
    Ok(msg)
}

/// Run `f` on each pending remote deletion stored under `key`; the ones that fail stay pending.
pub fn process_tombstones(
    conn: &Connection,
    key: &str,
    mut f: impl FnMut(&serde_json::Value) -> Result<()>,
) -> Result<usize> {
    let v: Vec<serde_json::Value> =
        crate::db::kv_get(conn, key)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if v.is_empty() {
        return Ok(0);
    }
    let mut left = vec![];
    let mut done = 0;
    let mut err = None;
    for t in v {
        match f(&t) {
            Ok(()) => done += 1,
            Err(e) => {
                err.get_or_insert(e);
                left.push(t);
            }
        }
    }
    crate::db::kv_set(conn, key, &serde_json::to_string(&left)?)?;
    match err {
        Some(e) => Err(e),
        None => Ok(done),
    }
}

/// Put every task without a project into `default_project` (config). Returns how many moved.
pub fn assign_default_project(brain: &Brain) -> Result<usize> {
    let name = brain.cfg.default_project.trim();
    if name.is_empty() {
        return Ok(0);
    }
    let orphans: i64 = brain.conn.query_row("SELECT count(*) FROM tasks WHERE project_id IS NULL", [], |r| r.get(0))?;
    if orphans == 0 {
        return Ok(0);
    }
    let tx = brain.conn.unchecked_transaction()?;
    let mut st = memory::ApplyStats::default();
    let pid = memory::ensure_entity(&tx, "project", name, &mut st)?;
    let n = tx.execute("UPDATE tasks SET project_id=?1 WHERE project_id IS NULL", [pid])?;
    tx.execute("UPDATE entities SET dirty=1 WHERE id=?1", [pid])?;
    tx.commit()?;
    crate::notes::regenerate_dirty(brain)?;
    Ok(n)
}

/// All active projects with their number of open tasks, the default one first.
pub fn projects(brain: &Brain) -> Result<Vec<(String, i64)>> {
    let mut v: Vec<(String, i64)> = brain
        .conn
        .prepare(
            "SELECT e.name, (SELECT count(*) FROM tasks t WHERE t.project_id=e.id AND t.status IN ('open','proposed'))
             FROM entities e WHERE e.kind='project' AND e.status='active' ORDER BY e.name COLLATE NOCASE",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let def = brain.cfg.default_project.to_lowercase();
    v.sort_by_key(|(n, _)| n.to_lowercase() != def);
    Ok(v)
}

/// Set (or clear with an empty text) the description of a task. Undoable.
pub fn set_notes(brain: &Brain, id: i64, notes: &str) -> Result<String> {
    let old = TaskSnapshot::load(&brain.conn, id).map_err(|_| anyhow::anyhow!("no task #{id}"))?;
    let notes = notes.trim();
    let value = (!notes.is_empty()).then_some(notes);
    if old.notes.as_deref() == value {
        return Ok(format!("#{id}: description unchanged"));
    }
    let tx = brain.conn.unchecked_transaction()?;
    tx.execute("UPDATE tasks SET notes=?2, updated_at=?3 WHERE id=?1", params![id, value, ts(brain.now())])?;
    if let Some(p) = old.project_id {
        tx.execute("UPDATE entities SET dirty=1 WHERE id=?1", [p])?;
    }
    let msg = format!("#{id} {}: description {}", old.title, if value.is_some() { "updated" } else { "removed" });
    actions::log(&tx, "user", "task_notes", &msg, Some(Undo::RestoreTask { snapshot: old }))?;
    tx.commit()?;
    crate::notes::regenerate_dirty(brain)?;
    Ok(msg)
}

/// Add a task from a one-line description using the deterministic parser.
pub fn add(brain: &Brain, text: &str) -> Result<memory::ApplyStats> {
    let spec = crate::journal::parse_task(text, crate::journal::CheckState::Open, brain.today());
    let tx = brain.conn.unchecked_transaction()?;
    let mut proj = None;
    for r in &spec.refs {
        let mut st = memory::ApplyStats::default();
        memory::ensure_entity(&tx, "project", r, &mut st)?;
        proj = Some(r.clone());
    }
    let item = Item::Task {
        b: 1,
        title: spec.title,
        due: spec.due.map(|d| d.to_string()),
        est: spec.estimate_min,
        proj,
        pri: spec.priority,
        firm: true,
        done: false,
        cancelled: false,
        task_ref: None,
    };
    let item = memory::validate(item, 1, brain.today()).map_err(|e| anyhow::anyhow!(e))?;
    let s = memory::apply(&tx, &[item], &ApplyCtx { block_ids: &[0], entry_date: brain.today(), origin: "user" })?;
    tx.commit()?;
    assign_default_project(brain)?;
    crate::notes::regenerate_dirty(brain)?;
    Ok(s)
}

pub fn fmt_row(t: &TaskRow, today: NaiveDate) -> String {
    let due = match t.due {
        None => String::new(),
        Some(d) => {
            let n = (d - today).num_days();
            let rel = match n {
                0 => "today".to_string(),
                1 => "tomorrow".to_string(),
                n if n < 0 => format!("{} d overdue", -n),
                n => format!("in {n} d"),
            };
            format!("  due {} ({rel})", d.format("%a %b %d"))
        }
    };
    let est = match t.estimate_min {
        Some(m) => format!("  ~{}", fmt_min(m)),
        None if t.kind == "task" => "  ~? ".to_string(),
        None => String::new(),
    };
    let proj = t.project.as_ref().map(|p| format!("  [{p}]")).unwrap_or_default();
    let flag = match (t.kind.as_str(), t.status.as_str()) {
        ("deadline", _) => "📅",
        (_, "proposed") => "❔",
        (_, "done") => "✔",
        (_, "cancelled") => "✗",
        _ => "☐",
    };
    format!("{flag} #{:<4} p{} {}{due}{est}{proj}", t.id, t.priority, t.title)
}

pub fn fmt_min(m: i64) -> String {
    if m >= 60 && m % 60 == 0 {
        format!("{}h", m / 60)
    } else if m >= 60 {
        format!("{}h{:02}", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

pub fn now_str(brain: &Brain) -> String {
    ts(brain.now())
}

pub fn completed_since(brain: &Brain, since: chrono::DateTime<Utc>) -> Result<Vec<TaskRow>> {
    query(&brain.conn, &format!("t.status='done' AND t.completed_at >= '{}'", ts(since)), &ts(brain.now()))
}
