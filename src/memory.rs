//! Memory model: typed extraction items, validation, and the single code path
//! that applies them to SQLite (deduplication, matching, supersession,
//! provenance, audit log). Raw model output never reaches the database
//! without passing through [`validate`] and [`apply`].

use crate::actions::{self, TaskSnapshot, Undo};
use crate::db::{DocKind, fts_upsert, kv_set};
use crate::util::{norm_name, normalize, sha256_hex, similarity, truncate_chars, ts};
use anyhow::Result;
use chrono::{Duration, NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// One unit of extracted information. `b` is the 1-based block index within
/// the batch it came from. Field names are short to keep model output small.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Item {
    Task {
        b: usize,
        title: String,
        #[serde(default)]
        due: Option<String>,
        #[serde(default)]
        est: Option<i64>,
        #[serde(default)]
        proj: Option<String>,
        #[serde(default)]
        pri: Option<i64>,
        #[serde(default = "yes")]
        firm: bool,
        #[serde(default)]
        done: bool,
        #[serde(default)]
        cancelled: bool,
        #[serde(default, rename = "ref")]
        task_ref: Option<i64>,
    },
    Deadline {
        b: usize,
        title: String,
        date: String,
        #[serde(default)]
        proj: Option<String>,
    },
    Knowledge {
        b: usize,
        concept: String,
        point: String,
        #[serde(default)]
        related: Vec<String>,
        #[serde(default)]
        proj: Vec<String>,
        #[serde(default)]
        ep: Option<String>,
        #[serde(default)]
        area: Option<String>,
        #[serde(default)]
        theme: Option<String>,
    },
    Profile {
        b: usize,
        key: String,
        value: String,
    },
    Event {
        b: usize,
        text: String,
        #[serde(default)]
        date: Option<String>,
        #[serde(default)]
        proj: Option<String>,
    },
    Decision {
        b: usize,
        text: String,
        #[serde(default)]
        proj: Option<String>,
    },
    Idea {
        b: usize,
        title: String,
        text: String,
        #[serde(default)]
        proj: Option<String>,
        #[serde(default)]
        area: Option<String>,
        #[serde(default)]
        theme: Option<String>,
    },
    /// Something to explore later: a tool, link, topic, prompt, repo, paper.
    Resource {
        b: usize,
        title: String,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        area: Option<String>,
        #[serde(default)]
        theme: Option<String>,
    },
    Goal {
        b: usize,
        title: String,
        #[serde(default)]
        text: Option<String>,
    },
    Project {
        b: usize,
        name: String,
    },
    /// A meaningful connection between two things (new or already known).
    Link {
        b: usize,
        from: String,
        to: String,
        #[serde(default)]
        why: Option<String>,
    },
}

fn yes() -> bool {
    true
}

impl Item {
    pub fn block(&self) -> usize {
        match self {
            Item::Task { b, .. }
            | Item::Deadline { b, .. }
            | Item::Knowledge { b, .. }
            | Item::Profile { b, .. }
            | Item::Event { b, .. }
            | Item::Decision { b, .. }
            | Item::Idea { b, .. }
            | Item::Goal { b, .. }
            | Item::Project { b, .. }
            | Item::Resource { b, .. }
            | Item::Link { b, .. } => *b,
        }
    }
}

fn clean(s: &str, max: usize) -> Result<String, String> {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        return Err("empty text".into());
    }
    Ok(truncate_chars(&s, max))
}

fn check_date(s: &Option<String>, today: NaiveDate) -> Result<Option<String>, String> {
    match s.as_deref().map(str::trim) {
        None | Some("") | Some("null") => Ok(None),
        Some(d) => {
            let date = crate::util::parse_date(d).ok_or(format!("bad date '{d}'"))?;
            if date < today - Duration::days(400) || date > today + Duration::days(800) {
                return Err(format!("implausible date {d}"));
            }
            Ok(Some(date.to_string()))
        }
    }
}

/// Themes are sub-areas ("Detection engineering"): a few words.
fn opt_theme(t: Option<String>) -> Option<String> {
    t.and_then(|t| clean(&t, 60).ok()).filter(|t| t.split_whitespace().count() <= 6)
}

/// Areas are broad hubs ("AI", "Cybersecurity"): short names only.
fn opt_short(a: Option<String>) -> Option<String> {
    a.and_then(|a| clean(&a, 40).ok()).filter(|a| a.split_whitespace().count() <= 3)
}

/// Validate and normalize one item. Rejects instead of guessing.
pub fn validate(item: Item, nblocks: usize, today: NaiveDate) -> Result<Item, String> {
    if item.block() == 0 || item.block() > nblocks {
        return Err(format!("block index {} out of range", item.block()));
    }
    let opt = |p: Option<String>| p.and_then(|p| clean(&p, 80).ok());
    Ok(match item {
        Item::Task { b, title, due, est, proj, pri, firm, done, cancelled, task_ref } => Item::Task {
            b,
            title: clean(&title, 200)?,
            due: check_date(&due, today)?,
            est: est.filter(|m| (5..=2400).contains(m)),
            proj: opt(proj),
            pri: pri.filter(|p| (1..=4).contains(p)),
            firm,
            done,
            cancelled,
            task_ref,
        },
        Item::Deadline { b, title, date, proj } => Item::Deadline {
            b,
            title: clean(&title, 200)?,
            date: check_date(&Some(date), today)?.ok_or("deadline without date")?,
            proj: opt(proj),
        },
        Item::Knowledge { b, concept, point, related, proj, ep, area, theme } => Item::Knowledge {
            b,
            concept: clean(&concept, 80)?,
            point: clean(&point, 1200)?,
            related: related.into_iter().filter_map(|r| clean(&r, 80).ok()).take(5).collect(),
            proj: proj.into_iter().filter_map(|r| clean(&r, 80).ok()).take(3).collect(),
            ep: ep.filter(|e| ["fact", "opinion", "hypothesis"].contains(&e.as_str())),
            area: opt_short(area),
            theme: opt_theme(theme),
        },
        Item::Profile { b, key, value } => {
            let key = normalize(&key).replace(' ', "_");
            if key.is_empty() || key.len() > 60 {
                return Err("bad profile key".into());
            }
            Item::Profile { b, key, value: clean(&value, 300)? }
        }
        Item::Event { b, text, date, proj } => {
            Item::Event { b, text: clean(&text, 400)?, date: check_date(&date, today)?, proj: opt(proj) }
        }
        Item::Decision { b, text, proj } => Item::Decision { b, text: clean(&text, 400)?, proj: opt(proj) },
        Item::Idea { b, title, text, proj, area, theme } => Item::Idea {
            b,
            title: clean(&title, 80)?,
            text: clean(&text, 1200)?,
            proj: opt(proj),
            area: opt_short(area),
            theme: opt_theme(theme),
        },
        Item::Resource { b, title, note, url, area, theme } => Item::Resource {
            b,
            title: clean(&title, 120)?,
            note: note.and_then(|n| clean(&n, 400).ok()),
            url: url
                .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                .map(|u| truncate_chars(u.trim(), 300)),
            area: opt_short(area),
            theme: opt_theme(theme),
        },
        Item::Goal { b, title, text } => {
            Item::Goal { b, title: clean(&title, 80)?, text: text.and_then(|t| clean(&t, 400).ok()) }
        }
        Item::Project { b, name } => Item::Project { b, name: clean(&name, 80)? },
        Item::Link { b, from, to, why } => {
            Item::Link { b, from: clean(&from, 80)?, to: clean(&to, 80)?, why: why.and_then(|w| clean(&w, 200).ok()) }
        }
    })
}

/// Parse a model's `{"items":[...]}` leniently: each item is decoded and
/// validated independently so one bad item doesn't discard the batch.
pub fn parse_items(v: &serde_json::Value, nblocks: usize, today: NaiveDate) -> (Vec<Item>, Vec<String>) {
    let mut ok = vec![];
    let mut rejected = vec![];
    let Some(arr) = v["items"].as_array() else {
        return (ok, vec!["missing items array".into()]);
    };
    for raw in arr.iter().take(60) {
        match serde_json::from_value::<Item>(raw.clone()) {
            Ok(item) => match validate(item, nblocks, today) {
                Ok(i) => ok.push(i),
                Err(e) => rejected.push(format!("{e}: {}", truncate_chars(&raw.to_string(), 120))),
            },
            Err(e) => rejected.push(format!("{e}: {}", truncate_chars(&raw.to_string(), 120))),
        }
    }
    (ok, rejected)
}

// ---------------------------------------------------------------------------
// Application

pub struct ApplyCtx<'a> {
    /// block id for each 1-based item block index (index 0 unused).
    pub block_ids: &'a [i64],
    pub entry_date: NaiveDate,
    /// deterministic | heuristic | llm | user
    pub origin: &'a str,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ApplyStats {
    pub tasks_created: usize,
    pub tasks_updated: usize,
    pub memories_created: usize,
    pub superseded: usize,
    pub duplicates_skipped: usize,
    pub entities_created: usize,
    pub links_created: usize,
    pub ignored: Vec<String>,
}

impl ApplyStats {
    pub fn add(&mut self, o: &ApplyStats) {
        self.tasks_created += o.tasks_created;
        self.tasks_updated += o.tasks_updated;
        self.memories_created += o.memories_created;
        self.superseded += o.superseded;
        self.duplicates_skipped += o.duplicates_skipped;
        self.entities_created += o.entities_created;
        self.links_created += o.links_created;
        self.ignored.extend(o.ignored.iter().cloned());
    }
}

pub fn find_entity(conn: &Connection, name: &str) -> Result<Option<(i64, String)>> {
    let norm = norm_name(name);
    let mut found = conn
        .query_row("SELECT id, kind FROM entities WHERE norm = ?1", [&norm], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    if found.is_none() {
        found = fuzzy_entity(conn, &norm)?;
    }
    if found.is_none() {
        // "SaaS project" -> "SaaS"
        let short = norm.trim_end_matches(" project").trim_start_matches("project ").to_string();
        if short != norm && !short.is_empty() {
            found = conn
                .query_row("SELECT id, kind FROM entities WHERE norm = ?1", [&short], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
        }
    }
    Ok(found)
}

fn stem_words(norm: &str) -> Vec<String> {
    const SKIP: &[&str] =
        &["on", "of", "the", "a", "an", "and", "for", "to", "in", "de", "la", "le", "les", "des", "du", "et"];
    norm.split(' ')
        .filter(|w| !w.is_empty() && !SKIP.contains(w))
        .map(|w| {
            if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
                w[..w.len() - 1].to_string()
            } else {
                w.to_string()
            }
        })
        .collect()
}

/// "Kant on dignity" ≈ "Kant on human dignity": the shorter name's words (≥2)
/// are all in the longer one, which adds at most 2 words. Concepts/ideas/areas only.
fn fuzzy_entity(conn: &Connection, norm: &str) -> Result<Option<(i64, String)>> {
    let mine = stem_words(norm);
    if mine.is_empty() {
        return Ok(None);
    }
    let mut st = conn
        .prepare("SELECT id, kind, norm FROM entities WHERE kind IN ('concept','idea','area') AND status='active'")?;
    let rows: Vec<(i64, String, String)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    for (id, kind, other) in rows {
        let theirs = stem_words(&other);
        let (short, long) = if mine.len() <= theirs.len() { (&mine, &theirs) } else { (&theirs, &mine) };
        if short.len() >= 2 && long.len() - short.len() <= 2 && short.iter().all(|w| long.contains(w)) {
            return Ok(Some((id, kind)));
        }
    }
    Ok(None)
}

pub fn ensure_entity(conn: &Connection, kind: &str, name: &str, stats: &mut ApplyStats) -> Result<i64> {
    if let Some((id, found_kind)) = find_entity(conn, name)? {
        // Reused after being archived (e.g. by `sb reprocess`): bring it back.
        let archived: bool =
            conn.query_row("SELECT status='archived' FROM entities WHERE id=?1", [id], |r| r.get(0))?;
        if archived {
            conn.execute("UPDATE entities SET status='active', dirty=1 WHERE id=?1", [id])?;
            let ename: String = conn.query_row("SELECT name FROM entities WHERE id=?1", [id], |r| r.get(0))?;
            fts_upsert(conn, DocKind::Entity, id, &ename, &found_kind)?;
        }
        // A concept named like a broad area becomes the area hub.
        if (kind == "area" || kind == "theme") && found_kind == "concept" {
            conn.execute("UPDATE entities SET kind=?2, dirty=1 WHERE id=?1", params![id, kind])?;
        }
        return Ok(id);
    }
    let name = if kind == "project" {
        let n = name.trim();
        n.strip_suffix(" project").or(n.strip_suffix(" Project")).unwrap_or(n).to_string()
    } else {
        name.trim().to_string()
    };
    let now = ts(Utc::now());
    conn.execute(
        "INSERT INTO entities(kind, name, norm, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![kind, name, norm_name(&name), now],
    )?;
    let id = conn.last_insert_rowid();
    fts_upsert(conn, DocKind::Entity, id, &name, kind)?;
    stats.entities_created += 1;
    Ok(id)
}

/// Place an item in the taxonomy: grand theme (area) › theme › item.
/// Returns the hub the item hangs from (theme, else area).
pub fn place(
    conn: &Connection,
    ent: i64,
    area: &Option<String>,
    theme: &Option<String>,
    block_id: Option<i64>,
    st: &mut ApplyStats,
) -> Result<Option<i64>> {
    let aid = match area {
        Some(a) => Some(ensure_entity(conn, "area", a, st)?),
        None => None,
    };
    let tid = match theme {
        Some(t) if area.as_deref().is_none_or(|a| norm_name(a) != norm_name(t)) => {
            let tid = ensure_entity(conn, "theme", t, st)?;
            if let Some(a) = aid {
                // A theme belongs to one grand theme: keep the first one.
                let has: bool =
                    conn.query_row("SELECT count(*)>0 FROM links WHERE src=?1 AND relation='part_of'", [tid], |r| {
                        r.get(0)
                    })?;
                if !has {
                    link(conn, tid, a, "part_of", block_id)?;
                }
            }
            Some(tid)
        }
        _ => None,
    };
    match (tid, aid) {
        (Some(t), _) if t != ent => {
            link(conn, ent, t, "in_theme", block_id)?;
            // The theme carries the grand theme now: drop a direct area link.
            conn.execute("DELETE FROM links WHERE src=?1 AND relation='in_area'", [ent])?;
            Ok(Some(t))
        }
        (_, Some(a)) if a != ent => {
            link(conn, ent, a, "in_area", block_id)?;
            Ok(Some(a))
        }
        _ => Ok(None),
    }
}

fn project_id(conn: &Connection, name: &Option<String>) -> Result<Option<i64>> {
    let Some(n) = name else { return Ok(None) };
    Ok(find_entity(conn, n)?.filter(|(_, k)| k == "project" || k == "goal").map(|(id, _)| id))
}

fn touch(conn: &Connection, entity: Option<i64>) -> Result<()> {
    if let Some(e) = entity {
        conn.execute("UPDATE entities SET dirty=1, updated_at=?2 WHERE id=?1", params![e, ts(Utc::now())])?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_memory(
    conn: &Connection,
    kind: &str,
    key: Option<&str>,
    content: &str,
    epistemic: &str,
    occurred_on: Option<&str>,
    block_id: Option<i64>,
    origin: &str,
    entities: &[i64],
    stats: &mut ApplyStats,
) -> Result<Option<i64>> {
    let norm_hash = sha256_hex(&format!("{kind}:{}", normalize(content)));
    let dup: Option<i64> = conn
        .query_row("SELECT id FROM memories WHERE norm_hash = ?1 AND status = 'active'", [&norm_hash], |r| r.get(0))
        .optional()?;
    // Near-duplicate within the same entity (rephrasings of the same point).
    let near = if dup.is_none() && !entities.is_empty() && kind != "profile" {
        let mut st = conn.prepare(
            "SELECT m.id, m.content FROM memories m JOIN memory_entities me ON me.memory_id = m.id
             WHERE me.entity_id = ?1 AND m.kind = ?2 AND m.status = 'active'",
        )?;
        let rows = st.query_map(params![entities[0], kind], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut best = None;
        for row in rows {
            let (id, c) = row?;
            if similarity(&c, content) >= 0.8 {
                best = Some(id);
                break;
            }
        }
        best
    } else {
        None
    };
    if let Some(existing) = dup.or(near) {
        for e in entities {
            conn.execute("INSERT OR IGNORE INTO memory_entities VALUES (?1, ?2)", params![existing, e])?;
        }
        stats.duplicates_skipped += 1;
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO memories(kind, key, content, norm_hash, epistemic, occurred_on, recorded_at, block_id, origin)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![kind, key, content, norm_hash, epistemic, occurred_on, ts(Utc::now()), block_id, origin],
    )?;
    let id = conn.last_insert_rowid();
    for e in entities {
        conn.execute("INSERT OR IGNORE INTO memory_entities VALUES (?1, ?2)", params![id, e])?;
        touch(conn, Some(*e))?;
    }
    reindex_memory(conn, id)?;
    stats.memories_created += 1;
    Ok(Some(id))
}

/// (Re)build the FTS document for a memory: title = linked entity names or key.
pub fn reindex_memory(conn: &Connection, id: i64) -> Result<()> {
    let (kind, key, content): (String, Option<String>, String) =
        conn.query_row("SELECT kind, key, content FROM memories WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let mut st = conn.prepare(
        "SELECT e.name FROM entities e JOIN memory_entities me ON me.entity_id = e.id WHERE me.memory_id = ?1",
    )?;
    let names: Vec<String> = st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let title = if names.is_empty() { key.unwrap_or(kind) } else { names.join(", ") };
    fts_upsert(conn, DocKind::Memory, id, &title, &content)
}

pub fn link(conn: &Connection, src: i64, dst: i64, relation: &str, block_id: Option<i64>) -> Result<()> {
    link_why(conn, src, dst, relation, block_id, None).map(|_| ())
}

/// Create a link (with an optional human-readable justification). A
/// `related` link counts in both directions. Returns whether it was new.
pub fn link_why(
    conn: &Connection,
    src: i64,
    dst: i64,
    relation: &str,
    block_id: Option<i64>,
    why: Option<&str>,
) -> Result<bool> {
    if src == dst {
        return Ok(false);
    }
    let existing: Option<(i64, i64)> = conn
        .query_row(
            "SELECT src, dst FROM links WHERE relation=?3 AND ((src=?1 AND dst=?2) OR (src=?2 AND dst=?1 AND ?3='related'))",
            params![src, dst, relation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((s, d)) = existing {
        if let Some(w) = why {
            conn.execute(
                "UPDATE links SET why=?4 WHERE src=?1 AND dst=?2 AND relation=?3 AND why IS NULL",
                params![s, d, relation, w],
            )?;
        }
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO links(src, dst, relation, block_id, created_at, why) VALUES (?1,?2,?3,?4,?5,?6)",
        params![src, dst, relation, block_id, ts(Utc::now()), why],
    )?;
    touch(conn, Some(src))?;
    touch(conn, Some(dst))?;
    Ok(true)
}

/// Find a task the new item refers to: explicit id, else same/similar title.
fn match_task(conn: &Connection, kind: &str, title: &str, task_ref: Option<i64>) -> Result<Option<TaskSnapshot>> {
    if let Some(id) = task_ref
        && let Ok(t) = TaskSnapshot::load(conn, id)
    {
        return Ok(Some(t));
    }
    let norm = normalize(title);
    let mut st = conn.prepare(
        "SELECT id, title FROM tasks WHERE kind = ?1 AND status != 'cancelled' ORDER BY (status IN ('open','proposed')) DESC, updated_at DESC LIMIT 500",
    )?;
    let rows: Vec<(i64, String)> =
        st.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let best = rows
        .iter()
        .find(|(_, t)| normalize(t) == norm)
        .or_else(|| {
            rows.iter()
                .map(|r| (r, similarity(&r.1, title)))
                .filter(|(_, s)| *s >= 0.75)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(r, _)| r)
        })
        .map(|(id, _)| *id);
    best.map(|id| TaskSnapshot::load(conn, id)).transpose()
}

#[allow(clippy::too_many_arguments)]
fn apply_task(
    conn: &Connection,
    kind: &str,
    title: &str,
    due: Option<String>,
    est: Option<i64>,
    project: Option<i64>,
    pri: Option<i64>,
    status: &str,
    task_ref: Option<i64>,
    block_id: Option<i64>,
    origin: &str,
    actor: &str,
    stats: &mut ApplyStats,
) -> Result<()> {
    let now = ts(Utc::now());
    if let Some(old) = match_task(conn, kind, title, task_ref)? {
        // Completed/cancelled tasks are authoritative: never reactivated by text.
        if old.status == "done" || old.status == "cancelled" {
            if status == "open" || status == "proposed" {
                stats.ignored.push(format!("#{} '{}' is already {}; not reactivated", old.id, old.title, old.status));
            }
            return Ok(());
        }
        let mut new = old.clone();
        let mut changes = vec![];
        if status == "done" || status == "cancelled" {
            new.status = status.to_string();
            new.completed_at = (status == "done").then(|| now.clone());
            changes.push(status.to_string());
        } else if old.status == "proposed" && status == "open" {
            new.status = "open".into();
            changes.push("confirmed".into());
        }
        if due.is_some() && due != old.due {
            changes.push(format!("due {} → {}", old.due.as_deref().unwrap_or("none"), due.as_deref().unwrap_or("")));
            new.due = due;
        }
        if est.is_some() && est != old.estimate_min {
            new.estimate_min = est;
            new.estimate_source = Some("user".into());
            changes.push(format!("estimate {}m", est.unwrap_or(0)));
        }
        if pri.is_some_and(|p| p != old.priority) {
            new.priority = pri.unwrap_or(3);
            changes.push(format!("priority p{}", new.priority));
        }
        if new.project_id.is_none() && project.is_some() {
            new.project_id = project;
            changes.push("project linked".into());
        }
        if changes.is_empty() {
            stats.duplicates_skipped += 1;
            return Ok(());
        }
        conn.execute(
            "UPDATE tasks SET status=?2, due=?3, estimate_min=?4, estimate_source=?5, priority=?6, project_id=?7,
               completed_at=?8, updated_at=?9 WHERE id=?1",
            params![
                old.id,
                new.status,
                new.due,
                new.estimate_min,
                new.estimate_source,
                new.priority,
                new.project_id,
                new.completed_at,
                now
            ],
        )?;
        if new.status == "done" || new.status == "cancelled" {
            conn.execute(
                "UPDATE sessions SET status='cancelled' WHERE task_id=?1 AND status='planned' AND start > ?2",
                params![old.id, now],
            )?;
        }
        touch(conn, old.project_id.or(new.project_id))?;
        actions::log(
            conn,
            actor,
            "task_updated",
            &format!("#{} {}: {}", old.id, old.title, changes.join(", ")),
            Some(Undo::RestoreTask { snapshot: old }),
        )?;
        stats.tasks_updated += 1;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO tasks(kind, title, norm, project_id, status, priority, due, estimate_min, estimate_source,
           block_id, origin, created_at, updated_at, completed_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12,?13)",
        params![
            kind,
            title,
            normalize(title),
            project,
            status,
            pri.unwrap_or(3),
            due,
            est,
            est.map(|_| "user"),
            block_id,
            origin,
            now,
            (status == "done").then(|| now.clone())
        ],
    )?;
    let id = conn.last_insert_rowid();
    fts_upsert(conn, DocKind::Task, id, title, kind)?;
    touch(conn, project)?;
    actions::log(
        conn,
        actor,
        if kind == "deadline" { "deadline_created" } else { "task_created" },
        &format!("#{id} {title}{} [{status}]", due.map(|d| format!(" (due {d})")).unwrap_or_default()),
        Some(Undo::CancelTask { id }),
    )?;
    stats.tasks_created += 1;
    Ok(())
}

/// Apply validated items in the caller's transaction.
pub fn apply(conn: &Connection, items: &[Item], ctx: &ApplyCtx) -> Result<ApplyStats> {
    let mut st = ApplyStats::default();
    let actor = if ctx.origin == "llm" { "llm" } else { "system" };
    let entry = ctx.entry_date.to_string();
    let bid = |b: usize| ctx.block_ids.get(b).copied();

    // Pass 1: entities explicitly introduced in this batch.
    for it in items {
        match it {
            Item::Project { name, .. } => {
                ensure_entity(conn, "project", name, &mut st)?;
            }
            Item::Goal { title, .. } => {
                ensure_entity(conn, "goal", title, &mut st)?;
            }
            Item::Knowledge { concept, .. } => {
                ensure_entity(conn, "concept", concept, &mut st)?;
            }
            Item::Idea { title, .. } => {
                ensure_entity(conn, "idea", title, &mut st)?;
            }
            Item::Resource { area: Some(a), .. } => {
                ensure_entity(conn, "area", a, &mut st)?;
            }
            Item::Resource { theme: Some(t), area: None, .. } => {
                ensure_entity(conn, "theme", t, &mut st)?;
            }
            _ => {}
        }
    }

    // Pass 2: everything else.
    for it in items {
        let block_id = bid(it.block());
        match it {
            Item::Task { title, due, est, proj, pri, firm, done, cancelled, task_ref, .. } => {
                let status = if *done {
                    "done"
                } else if *cancelled {
                    "cancelled"
                } else if *firm {
                    "open"
                } else {
                    "proposed"
                };
                let p = project_id(conn, proj)?;
                apply_task(
                    conn,
                    "task",
                    title,
                    due.clone(),
                    *est,
                    p,
                    *pri,
                    status,
                    *task_ref,
                    block_id,
                    ctx.origin,
                    actor,
                    &mut st,
                )?;
            }
            Item::Deadline { title, date, proj, .. } => {
                let p = project_id(conn, proj)?;
                apply_task(
                    conn,
                    "deadline",
                    title,
                    Some(date.clone()),
                    None,
                    p,
                    None,
                    "open",
                    None,
                    block_id,
                    ctx.origin,
                    actor,
                    &mut st,
                )?;
            }
            Item::Knowledge { concept, point, related, proj, ep, area, theme, .. } => {
                let cid = ensure_entity(conn, "concept", concept, &mut st)?;
                let mut ents = vec![cid];
                place(conn, cid, area, theme, block_id, &mut st)?;
                for p in proj {
                    if let Some(pid) = project_id(conn, &Some(p.clone()))? {
                        ents.push(pid);
                        link(conn, cid, pid, "used_in", block_id)?;
                    }
                }
                for r in related {
                    // Only link to concepts that already exist: precision over quantity.
                    if let Some((rid, _)) = find_entity(conn, r)? {
                        link(conn, cid, rid, "related", block_id)?;
                    }
                }
                let ep = ep.as_deref().unwrap_or("fact");
                if let Some(id) = insert_memory(
                    conn,
                    "knowledge",
                    None,
                    point,
                    ep,
                    Some(&entry),
                    block_id,
                    ctx.origin,
                    &ents,
                    &mut st,
                )? {
                    actions::log(
                        conn,
                        actor,
                        "knowledge_added",
                        &format!("[{concept}] {}", truncate_chars(point, 80)),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Profile { key, value, .. } => {
                let old: Option<(i64, String)> = conn
                    .query_row(
                        "SELECT id, content FROM memories WHERE kind='profile' AND key=?1 AND status='active' ORDER BY id DESC LIMIT 1",
                        [key],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((_, c)) = &old
                    && normalize(c) == normalize(value)
                {
                    st.duplicates_skipped += 1;
                    continue;
                }
                if let Some((old_id, _)) = old {
                    // Retire the old fact before inserting so dedupe never matches it.
                    conn.execute("UPDATE memories SET status='superseded' WHERE id=?1", [old_id])?;
                }
                let Some(id) = insert_memory(
                    conn,
                    "profile",
                    Some(key),
                    value,
                    "fact",
                    Some(&entry),
                    block_id,
                    ctx.origin,
                    &[],
                    &mut st,
                )?
                else {
                    if let Some((old_id, _)) = old {
                        conn.execute("UPDATE memories SET status='active' WHERE id=?1", [old_id])?;
                    }
                    continue;
                };
                kv_set(conn, "profile_dirty", "1")?;
                if let Some((old_id, _)) = old {
                    conn.execute("UPDATE memories SET superseded_by=?2 WHERE id=?1", params![old_id, id])?;
                    crate::db::fts_delete(conn, DocKind::Memory, old_id)?;
                    st.superseded += 1;
                    actions::log(
                        conn,
                        actor,
                        "profile_updated",
                        &format!("{key} = {value} (supersedes #{old_id})"),
                        Some(Undo::Unsupersede { old: old_id, new: id }),
                    )?;
                } else {
                    actions::log(
                        conn,
                        actor,
                        "profile_added",
                        &format!("{key} = {value}"),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Event { text, date, proj, .. } => {
                let ents: Vec<i64> = project_id(conn, proj)?.into_iter().collect();
                let when = date.clone().unwrap_or(entry.clone());
                if let Some(id) = insert_memory(
                    conn,
                    "episodic",
                    None,
                    text,
                    "experience",
                    Some(&when),
                    block_id,
                    ctx.origin,
                    &ents,
                    &mut st,
                )? {
                    actions::log(
                        conn,
                        actor,
                        "event_recorded",
                        &truncate_chars(text, 90),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Decision { text, proj, .. } => {
                let ents: Vec<i64> = project_id(conn, proj)?.into_iter().collect();
                if let Some(id) = insert_memory(
                    conn,
                    "decision",
                    None,
                    text,
                    "fact",
                    Some(&entry),
                    block_id,
                    ctx.origin,
                    &ents,
                    &mut st,
                )? {
                    actions::log(
                        conn,
                        actor,
                        "decision_recorded",
                        &truncate_chars(text, 90),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Idea { title, text, proj, area, theme, .. } => {
                let iid = ensure_entity(conn, "idea", title, &mut st)?;
                let mut ents = vec![iid];
                place(conn, iid, area, theme, block_id, &mut st)?;
                if let Some(p) = project_id(conn, proj)? {
                    ents.push(p);
                    link(conn, iid, p, "for_project", block_id)?;
                }
                if let Some(id) = insert_memory(
                    conn,
                    "idea",
                    None,
                    text,
                    "hypothesis",
                    Some(&entry),
                    block_id,
                    ctx.origin,
                    &ents,
                    &mut st,
                )? {
                    actions::log(
                        conn,
                        actor,
                        "idea_recorded",
                        &format!("[{title}] {}", truncate_chars(text, 70)),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Goal { title, text, .. } => {
                let gid = ensure_entity(conn, "goal", title, &mut st)?;
                let content = text.clone().unwrap_or_else(|| title.clone());
                if let Some(id) = insert_memory(
                    conn,
                    "goal",
                    None,
                    &content,
                    "fact",
                    Some(&entry),
                    block_id,
                    ctx.origin,
                    &[gid],
                    &mut st,
                )? {
                    actions::log(
                        conn,
                        actor,
                        "goal_recorded",
                        &truncate_chars(&content, 90),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Resource { title, note, url, area, theme, .. } => {
                // Attach to the most specific hub (theme, else area), and to a matching concept.
                let mut ents = vec![];
                let aid = match area {
                    Some(a) => Some(ensure_entity(conn, "area", a, &mut st)?),
                    None => None,
                };
                let tid = match theme {
                    Some(t) => {
                        let tid = ensure_entity(conn, "theme", t, &mut st)?;
                        if let Some(a) = aid
                            && tid != a
                        {
                            let has: bool = conn.query_row(
                                "SELECT count(*)>0 FROM links WHERE src=?1 AND relation='part_of'",
                                [tid],
                                |r| r.get(0),
                            )?;
                            if !has {
                                link(conn, tid, a, "part_of", block_id)?;
                            }
                        }
                        Some(tid)
                    }
                    None => None,
                };
                if let Some(h) = tid.or(aid) {
                    ents.push(h);
                }
                if let Some((cid, k)) = find_entity(conn, title)?
                    && k != "area"
                    && !ents.contains(&cid)
                {
                    ents.push(cid);
                }
                let content = [Some(title.clone()), note.clone(), url.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" - ");
                if let Some(id) = insert_memory(
                    conn,
                    "resource",
                    None,
                    &content,
                    "hypothesis",
                    Some(&entry),
                    block_id,
                    ctx.origin,
                    &ents,
                    &mut st,
                )? {
                    actions::log(
                        conn,
                        actor,
                        "resource_saved",
                        &truncate_chars(&content, 90),
                        Some(Undo::RetractMemories { ids: vec![id] }),
                    )?;
                }
            }
            Item::Project { .. } | Item::Link { .. } => {}
        }
    }

    // Pass 3: explicit links, once every entity of the batch exists.
    for it in items {
        if let Item::Link { from, to, why, .. } = it {
            match (find_entity(conn, from)?, find_entity(conn, to)?) {
                (Some((a, _)), Some((b, _))) => {
                    if link_why(conn, a, b, "related", bid(it.block()), why.as_deref())? {
                        st.links_created += 1;
                    }
                }
                _ => st.ignored.push(format!("link '{from}' ↔ '{to}': unknown name")),
            }
        }
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()
    }

    #[test]
    fn fuzzy_concepts_and_area_hubs() {
        let c = crate::db::open_memory().unwrap();
        let mut st = ApplyStats::default();
        let kant = ensure_entity(&c, "concept", "Kant on dignity", &mut st).unwrap();
        assert_eq!(ensure_entity(&c, "concept", "Kant on human dignity", &mut st).unwrap(), kant);
        assert_ne!(ensure_entity(&c, "concept", "Kant", &mut st).unwrap(), kant, "one word is too vague");
        assert_ne!(ensure_entity(&c, "concept", "Kant on dignity in modern AI ethics debates", &mut st).unwrap(), kant);

        let items = vec![
            Item::Resource {
                b: 1,
                title: "crt.sh".into(),
                note: Some("certificate search".into()),
                url: None,
                area: Some("Cybersecurity".into()),
                theme: Some("Reconnaissance".into()),
            },
            Item::Idea {
                b: 1,
                title: "SOC labs".into(),
                text: "1) SSH brute force; 2) port scans".into(),
                proj: None,
                area: Some("Cybersecurity".into()),
                theme: Some("Detection engineering".into()),
            },
            Item::Idea {
                b: 1,
                title: "Cyber reading list".into(),
                text: "books".into(),
                proj: None,
                area: Some("Cybersecurity".into()),
                theme: None,
            },
        ];
        let s = apply(&c, &items, &ApplyCtx { block_ids: &[0], entry_date: today(), origin: "llm" }).unwrap();
        assert_eq!(s.memories_created, 3);
        let q = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        // Tree: Cybersecurity ⊃ {Reconnaissance, Detection engineering}; items hang from themes.
        assert_eq!(
            q(
                "SELECT count(*) FROM links l JOIN entities t ON t.id=l.src JOIN entities a ON a.id=l.dst WHERE l.relation='part_of' AND t.kind='theme' AND a.name='Cybersecurity'"
            ),
            2
        );
        assert_eq!(
            q(
                "SELECT count(*) FROM links l JOIN entities i ON i.id=l.src JOIN entities t ON t.id=l.dst WHERE l.relation='in_theme' AND i.name='SOC labs' AND t.name='Detection engineering'"
            ),
            1
        );
        assert_eq!(
            q(
                "SELECT count(*) FROM links l JOIN entities i ON i.id=l.src WHERE l.relation='in_area' AND i.name='Cyber reading list'"
            ),
            1
        );
        assert_eq!(
            q(
                "SELECT count(*) FROM memories m JOIN memory_entities me ON me.memory_id=m.id JOIN entities e ON e.id=me.entity_id WHERE m.kind='resource' AND e.name='Reconnaissance'"
            ),
            1
        );
        // A theme stays under its first grand theme.
        let mut st2 = ApplyStats::default();
        let soc = find_entity(&c, "SOC labs").unwrap().unwrap().0;
        place(&c, soc, &Some("AI".into()), &Some("Detection engineering".into()), None, &mut st2).unwrap();
        assert_eq!(
            q(
                "SELECT count(*) FROM links l JOIN entities t ON t.id=l.src WHERE l.relation='part_of' AND t.name='Detection engineering'"
            ),
            1
        );
    }

    #[test]
    fn validation_rejects_bad_items() {
        let bad = Item::Task {
            b: 3,
            title: "x".into(),
            due: None,
            est: None,
            proj: None,
            pri: None,
            firm: true,
            done: false,
            cancelled: false,
            task_ref: None,
        };
        assert!(validate(bad, 2, today()).is_err());
        let v = serde_json::json!({"items":[
            {"t":"task","b":1,"title":"Finish prototype","due":"2026-11-08","est":99999},
            {"t":"task","b":1,"title":"   "},
            {"t":"deadline","b":1,"title":"Exam","date":"not a date"},
            {"t":"launch_missiles","b":1},
            {"t":"profile","b":1,"key":"Productive Time","value":"mornings"}
        ]});
        let (ok, rej) = parse_items(&v, 1, today());
        assert_eq!(ok.len(), 2);
        assert_eq!(rej.len(), 3);
        assert!(matches!(&ok[0], Item::Task { est: None, .. }));
        assert!(matches!(&ok[1], Item::Profile { key, .. } if key == "productive_time"));
    }
}
