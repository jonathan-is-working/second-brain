//! Audit log of every autonomous or user-triggered mutation, with undo.

use crate::util::ts;
use anyhow::{Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// A full `links` row: (src, dst, relation, block_id, created_at, why).
pub type LinkRow = (i64, i64, String, Option<i64>, String, Option<String>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op")]
pub enum Undo {
    /// Undo a task creation: cancel it (rows are never hard-deleted).
    CancelTask {
        id: i64,
    },
    /// Restore the previous task fields.
    RestoreTask {
        snapshot: TaskSnapshot,
    },
    RetractMemories {
        ids: Vec<i64>,
    },
    /// Undo a supersession: reactivate `old`, retract `new`.
    Unsupersede {
        old: i64,
        new: i64,
    },
    CancelSessions {
        ids: Vec<i64>,
    },
    /// Remove links created by the connection agent: (src, dst, relation).
    RemoveLinks {
        links: Vec<(i64, i64, String)>,
    },
    /// Put back links removed by `sb unlink`: (src, dst, relation, block_id, created_at, why).
    RestoreLinks {
        links: Vec<LinkRow>,
    },
    /// Re-plan sessions that were cancelled.
    RestoreSessions {
        ids: Vec<i64>,
    },
    /// Put back a deleted task (all its columns) and its work sessions.
    RestoreDeletedTask {
        task: Vec<(String, serde_json::Value)>,
        sessions: Vec<Vec<(String, serde_json::Value)>>,
    },
}

/// Rows of `sql` (one `?1` parameter) as (column, value) lists, to snapshot and re-insert them.
pub fn rows_json(conn: &Connection, sql: &str, id: i64) -> Result<Vec<Vec<(String, serde_json::Value)>>> {
    use rusqlite::types::ValueRef;
    let mut st = conn.prepare(sql)?;
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = st.query([id])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        let mut row = vec![];
        for (i, n) in names.iter().enumerate() {
            let v = match r.get_ref(i)? {
                ValueRef::Null => serde_json::Value::Null,
                ValueRef::Integer(x) => serde_json::json!(x),
                ValueRef::Real(x) => serde_json::json!(x),
                ValueRef::Text(t) => serde_json::json!(String::from_utf8_lossy(t)),
                ValueRef::Blob(_) => serde_json::Value::Null,
            };
            row.push((n.clone(), v));
        }
        out.push(row);
    }
    Ok(out)
}

fn insert_row(conn: &Connection, table: &str, row: &[(String, serde_json::Value)]) -> Result<()> {
    let cols: Vec<&str> = row.iter().map(|(c, _)| c.as_str()).collect();
    let marks: Vec<String> = (1..=row.len()).map(|i| format!("?{i}")).collect();
    let vals: Vec<rusqlite::types::Value> = row
        .iter()
        .map(|(_, v)| match v {
            serde_json::Value::Null => rusqlite::types::Value::Null,
            serde_json::Value::Number(n) if n.is_i64() => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)),
            serde_json::Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => rusqlite::types::Value::Text(s.clone()),
            other => rusqlite::types::Value::Text(other.to_string()),
        })
        .collect();
    conn.execute(
        &format!("INSERT OR REPLACE INTO {table}({}) VALUES ({})", cols.join(","), marks.join(",")),
        rusqlite::params_from_iter(vals),
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskSnapshot {
    pub id: i64,
    pub title: String,
    pub status: String,
    pub priority: i64,
    pub due: Option<String>,
    pub estimate_min: Option<i64>,
    pub estimate_source: Option<String>,
    pub project_id: Option<i64>,
    pub completed_at: Option<String>,
    /// Description; `has_notes` is false in snapshots taken before descriptions existed.
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub has_notes: bool,
}

impl TaskSnapshot {
    pub fn load(conn: &Connection, id: i64) -> Result<TaskSnapshot> {
        Ok(conn.query_row(
            "SELECT id, title, status, priority, due, estimate_min, estimate_source, project_id, completed_at, notes
             FROM tasks WHERE id = ?1",
            [id],
            |r| {
                Ok(TaskSnapshot {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    status: r.get(2)?,
                    priority: r.get(3)?,
                    due: r.get(4)?,
                    estimate_min: r.get(5)?,
                    estimate_source: r.get(6)?,
                    project_id: r.get(7)?,
                    completed_at: r.get(8)?,
                    notes: r.get(9)?,
                    has_notes: true,
                })
            },
        )?)
    }
}

pub fn log(conn: &Connection, actor: &str, kind: &str, summary: &str, undo: Option<Undo>) -> Result<i64> {
    let undo = undo.map(|u| serde_json::to_string(&u)).transpose()?;
    conn.execute(
        "INSERT INTO actions(ts, actor, kind, summary, undo) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ts(Utc::now()), actor, kind, summary, undo],
    )?;
    Ok(conn.last_insert_rowid())
}

#[derive(Debug)]
pub struct ActionRow {
    pub id: i64,
    pub ts: String,
    pub actor: String,
    pub kind: String,
    pub summary: String,
    pub undoable: bool,
    pub undone: bool,
}

pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<ActionRow>> {
    let mut st = conn.prepare(
        "SELECT id, ts, actor, kind, summary, undo IS NOT NULL, undone_at IS NOT NULL FROM actions ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = st
        .query_map([limit as i64], |r| {
            Ok(ActionRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                actor: r.get(2)?,
                kind: r.get(3)?,
                summary: r.get(4)?,
                undoable: r.get(5)?,
                undone: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Apply the undo operation of action `id` in one transaction.
pub fn undo(conn: &Connection, id: i64) -> Result<String> {
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row("SELECT summary, undo, undone_at FROM actions WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let Some((summary, undo, undone)) = row else { bail!("no action #{id}") };
    if undone.is_some() {
        bail!("action #{id} was already undone");
    }
    let Some(undo) = undo else { bail!("action #{id} ({summary}) is not undoable") };
    let op: Undo = serde_json::from_str(&undo)?;
    let tx = conn.unchecked_transaction()?;
    let now = ts(Utc::now());
    match &op {
        Undo::CancelTask { id } => {
            tx.execute("UPDATE tasks SET status='cancelled', updated_at=?2 WHERE id=?1", params![id, now])?;
            tx.execute("UPDATE sessions SET status='cancelled' WHERE task_id=?1 AND status='planned'", [id])?;
        }
        Undo::RestoreTask { snapshot: s } => {
            tx.execute(
                "UPDATE tasks SET title=?2, norm=?3, status=?4, priority=?5, due=?6, estimate_min=?7,
                   estimate_source=?8, project_id=?9, completed_at=?10, updated_at=?11 WHERE id=?1",
                params![
                    s.id,
                    s.title,
                    crate::util::normalize(&s.title),
                    s.status,
                    s.priority,
                    s.due,
                    s.estimate_min,
                    s.estimate_source,
                    s.project_id,
                    s.completed_at,
                    now
                ],
            )?;
            if s.has_notes {
                tx.execute("UPDATE tasks SET notes=?2 WHERE id=?1", params![s.id, s.notes])?;
            }
        }
        Undo::RetractMemories { ids } => {
            for m in ids {
                tx.execute("UPDATE memories SET status='retracted' WHERE id=?1", [m])?;
                crate::db::fts_delete(&tx, crate::db::DocKind::Memory, *m)?;
            }
        }
        Undo::Unsupersede { old, new } => {
            tx.execute("UPDATE memories SET status='active', superseded_by=NULL WHERE id=?1", [old])?;
            tx.execute("UPDATE memories SET status='retracted' WHERE id=?1", [new])?;
            crate::db::fts_delete(&tx, crate::db::DocKind::Memory, *new)?;
            crate::memory::reindex_memory(&tx, *old)?;
        }
        Undo::CancelSessions { ids } => {
            for s in ids {
                tx.execute("UPDATE sessions SET status='cancelled' WHERE id=?1 AND status='planned'", [s])?;
            }
        }
        Undo::RemoveLinks { links } => {
            for (s, d, r) in links {
                tx.execute("DELETE FROM links WHERE src=?1 AND dst=?2 AND relation=?3", params![s, d, r])?;
            }
        }
        Undo::RestoreLinks { links } => {
            for (s, d, r, b, c, w) in links {
                tx.execute(
                    "INSERT OR IGNORE INTO links(src, dst, relation, block_id, created_at, why) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![s, d, r, b, c, w],
                )?;
            }
        }
        Undo::RestoreSessions { ids } => {
            for s in ids {
                tx.execute("UPDATE sessions SET status='planned' WHERE id=?1 AND status='cancelled'", [s])?;
            }
        }
        Undo::RestoreDeletedTask { task, sessions } => {
            insert_row(&tx, "tasks", task)?;
            let id = task.iter().find(|(c, _)| c == "id").and_then(|(_, v)| v.as_i64()).unwrap_or(0);
            let title = task.iter().find(|(c, _)| c == "title").and_then(|(_, v)| v.as_str()).unwrap_or("");
            crate::db::fts_upsert(&tx, crate::db::DocKind::Task, id, title, "task")?;
            for s in sessions {
                // Its calendar event was removed: a fresh one is pushed at the next sync.
                let row: Vec<(String, serde_json::Value)> = s
                    .iter()
                    .map(
                        |(c, v)| {
                            if c == "gcal_id" { (c.clone(), serde_json::Value::Null) } else { (c.clone(), v.clone()) }
                        },
                    )
                    .collect();
                insert_row(&tx, "sessions", &row)?;
            }
        }
    }
    // Make every affected note regenerate.
    tx.execute("UPDATE entities SET dirty=1 WHERE status='active'", [])?;
    crate::db::kv_set(&tx, "profile_dirty", "1")?;
    tx.execute("UPDATE actions SET undone_at=?2 WHERE id=?1", params![id, now])?;
    log(&tx, "user", "undo", &format!("undid #{id}: {summary}"), None)?;
    tx.commit()?;
    Ok(summary)
}
