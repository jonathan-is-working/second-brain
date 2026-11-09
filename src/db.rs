//! SQLite persistence: connection setup, schema migrations, FTS helpers, kv.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

/// Ordered migrations; `PRAGMA user_version` records how many were applied.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE sources(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,            -- relative to the vault
  content_hash TEXT,
  entry_date TEXT,                      -- date the journal is about (YYYY-MM-DD)
  updated_at TEXT NOT NULL
);
CREATE TABLE blocks(
  id INTEGER PRIMARY KEY,
  source_id INTEGER NOT NULL REFERENCES sources(id),
  hash TEXT NOT NULL,
  text TEXT NOT NULL,
  line INTEGER NOT NULL,
  section TEXT,
  status TEXT NOT NULL CHECK(status IN ('pending','skipped','deterministic','extracted','failed')),
  present INTEGER NOT NULL DEFAULT 1,   -- 0 once the text was removed from the file (history kept)
  attempts INTEGER NOT NULL DEFAULT 0,
  first_seen TEXT NOT NULL,
  processed_at TEXT,
  UNIQUE(source_id, hash)
);
CREATE INDEX blocks_status ON blocks(status) WHERE status = 'pending';

CREATE TABLE entities(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('concept','project','goal','idea')),
  name TEXT NOT NULL,
  norm TEXT NOT NULL UNIQUE,
  summary TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','archived')),
  dirty INTEGER NOT NULL DEFAULT 1,     -- markdown note needs regeneration
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE memories(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('profile','episodic','knowledge','decision','idea','goal')),
  key TEXT,                             -- profile key (e.g. productive_time)
  content TEXT NOT NULL,
  norm_hash TEXT NOT NULL,              -- dedupe key
  epistemic TEXT NOT NULL DEFAULT 'fact' CHECK(epistemic IN ('fact','experience','opinion','hypothesis')),
  occurred_on TEXT,                     -- when it happened (vs recorded_at: when written down)
  recorded_at TEXT NOT NULL,
  block_id INTEGER REFERENCES blocks(id),
  origin TEXT NOT NULL CHECK(origin IN ('deterministic','heuristic','llm','user')),
  status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','superseded','retracted')),
  superseded_by INTEGER REFERENCES memories(id)
);
CREATE INDEX memories_kind ON memories(kind, status);
CREATE INDEX memories_key ON memories(key) WHERE key IS NOT NULL;

CREATE TABLE memory_entities(
  memory_id INTEGER NOT NULL REFERENCES memories(id),
  entity_id INTEGER NOT NULL REFERENCES entities(id),
  PRIMARY KEY(memory_id, entity_id)
);
CREATE INDEX memory_entities_entity ON memory_entities(entity_id);

CREATE TABLE links(
  src INTEGER NOT NULL REFERENCES entities(id),
  dst INTEGER NOT NULL REFERENCES entities(id),
  relation TEXT NOT NULL,
  block_id INTEGER REFERENCES blocks(id),
  created_at TEXT NOT NULL,
  PRIMARY KEY(src, dst, relation)
);

CREATE TABLE tasks(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL DEFAULT 'task' CHECK(kind IN ('task','deadline')),
  title TEXT NOT NULL,
  norm TEXT NOT NULL,
  project_id INTEGER REFERENCES entities(id),
  status TEXT NOT NULL CHECK(status IN ('proposed','open','done','cancelled')),
  priority INTEGER NOT NULL DEFAULT 3 CHECK(priority BETWEEN 1 AND 4),
  due TEXT,
  estimate_min INTEGER,
  estimate_source TEXT,                 -- user | default
  block_id INTEGER REFERENCES blocks(id),
  origin TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT
);
CREATE INDEX tasks_status ON tasks(status, due);

CREATE TABLE sessions(
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  start TEXT NOT NULL,
  end TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('planned','done','missed','cancelled')),
  gcal_id TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX sessions_time ON sessions(status, start);

CREATE TABLE busy(
  id INTEGER PRIMARY KEY,
  source TEXT NOT NULL,                 -- gcal | declined | manual
  start TEXT NOT NULL,
  end TEXT NOT NULL,
  title TEXT
);

CREATE TABLE actions(
  id INTEGER PRIMARY KEY,
  ts TEXT NOT NULL,
  actor TEXT NOT NULL,                  -- user | system | llm
  kind TEXT NOT NULL,
  summary TEXT NOT NULL,
  undo TEXT,                            -- JSON UndoOp
  undone_at TEXT
);

CREATE TABLE llm_calls(
  id INTEGER PRIMARY KEY,
  ts TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  purpose TEXT NOT NULL,
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  tokens_estimated INTEGER NOT NULL,    -- 1 when provider gave no usage data
  context_tokens INTEGER NOT NULL,      -- our estimate of the prompt we built
  cost_usd REAL,                        -- API-equivalent estimate reported by the provider, not a bill
  duration_ms INTEGER NOT NULL,
  ok INTEGER NOT NULL,
  error TEXT
);

CREATE TABLE llm_cache(key TEXT PRIMARY KEY, response TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE presented(key TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, ts TEXT NOT NULL);

CREATE VIRTUAL TABLE fts USING fts5(title, body, kind UNINDEXED, tokenize='porter unicode61');
"#,
    r#"
CREATE TABLE entities_new(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('concept','project','goal','idea','area')),
  name TEXT NOT NULL,
  norm TEXT NOT NULL UNIQUE,
  summary TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','archived')),
  dirty INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
INSERT INTO entities_new SELECT * FROM entities;
DROP TABLE entities;
ALTER TABLE entities_new RENAME TO entities;

CREATE TABLE memories_new(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('profile','episodic','knowledge','decision','idea','goal','resource')),
  key TEXT,
  content TEXT NOT NULL,
  norm_hash TEXT NOT NULL,
  epistemic TEXT NOT NULL DEFAULT 'fact' CHECK(epistemic IN ('fact','experience','opinion','hypothesis')),
  occurred_on TEXT,
  recorded_at TEXT NOT NULL,
  block_id INTEGER REFERENCES blocks(id),
  origin TEXT NOT NULL CHECK(origin IN ('deterministic','heuristic','llm','user')),
  status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','superseded','retracted')),
  superseded_by INTEGER REFERENCES memories(id)
);
INSERT INTO memories_new SELECT * FROM memories;
DROP TABLE memories;
ALTER TABLE memories_new RENAME TO memories;
CREATE INDEX memories_kind ON memories(kind, status);
CREATE INDEX memories_key ON memories(key) WHERE key IS NOT NULL;
"#,
    r#"
ALTER TABLE links ADD COLUMN why TEXT;
"#,
    r#"
CREATE TABLE entities_new(
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('concept','project','goal','idea','area','theme')),
  name TEXT NOT NULL,
  norm TEXT NOT NULL UNIQUE,
  summary TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','archived')),
  dirty INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
INSERT INTO entities_new SELECT * FROM entities;
DROP TABLE entities;
ALTER TABLE entities_new RENAME TO entities;

-- LLM syntheses (day/week/month), built hierarchically.
CREATE TABLE syntheses(
  id INTEGER PRIMARY KEY,
  period TEXT NOT NULL CHECK(period IN ('day','week','month')),
  start TEXT NOT NULL,                  -- first day (YYYY-MM-DD)
  end TEXT NOT NULL,                    -- last day (inclusive)
  content TEXT NOT NULL,
  input_hash TEXT NOT NULL,             -- skip regeneration when inputs are unchanged
  created_at TEXT NOT NULL,
  UNIQUE(period, start)
);
"#,
    r#"
-- Google Tasks lists created by the app (one per project + a default one).
CREATE TABLE gtask_lists(
  key TEXT PRIMARY KEY,                 -- 'p<project id>' or 'default'
  list_id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  project_id INTEGER REFERENCES entities(id)
);
-- Local task <-> Google task, with each side's change marker at the last sync.
CREATE TABLE gtasks(
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  list_id TEXT NOT NULL,
  gtask_id TEXT NOT NULL UNIQUE,
  local_updated TEXT NOT NULL,
  remote_updated TEXT NOT NULL
);
"#,
    r#"
-- Free-text description of a task (Markdown), synced with Google Tasks notes.
ALTER TABLE tasks ADD COLUMN notes TEXT;
"#,
];

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= MIGRATIONS.len() {
        return Ok(());
    }
    // Table rebuilds need FK enforcement off (it can't change inside a transaction).
    let fk: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys=OFF")?;
    let res = (|| -> Result<()> {
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            let broken: i64 = tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
            anyhow::ensure!(broken == 0, "migration {} would break {broken} foreign key(s)", i + 1);
            tx.commit()?;
        }
        Ok(())
    })();
    if fk == 1 {
        conn.execute_batch("PRAGMA foreign_keys=ON")?;
    }
    res
}

/// FTS documents get a rowid derived from (kind, id) so updates are O(1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Memory = 1,
    Block = 2,
    Entity = 3,
    Task = 4,
}

impl DocKind {
    pub fn name(self) -> &'static str {
        match self {
            DocKind::Memory => "memory",
            DocKind::Block => "journal",
            DocKind::Entity => "note",
            DocKind::Task => "task",
        }
    }
    pub fn from_rowid(rowid: i64) -> (DocKind, i64) {
        let k = match rowid % 8 {
            1 => DocKind::Memory,
            2 => DocKind::Block,
            3 => DocKind::Entity,
            _ => DocKind::Task,
        };
        (k, rowid / 8)
    }
}

pub fn fts_upsert(conn: &Connection, kind: DocKind, id: i64, title: &str, body: &str) -> Result<()> {
    let rowid = id * 8 + kind as i64;
    conn.execute("DELETE FROM fts WHERE rowid = ?1", [rowid])?;
    conn.execute(
        "INSERT INTO fts(rowid, title, body, kind) VALUES (?1, ?2, ?3, ?4)",
        params![rowid, title, body, kind.name()],
    )?;
    Ok(())
}

pub fn fts_delete(conn: &Connection, kind: DocKind, id: i64) -> Result<()> {
    conn.execute("DELETE FROM fts WHERE rowid = ?1", [id * 8 + kind as i64])?;
    Ok(())
}

pub fn kv_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn kv_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.db");
        drop(open(&p).unwrap());
        let c = open(&p).unwrap();
        let v: usize = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len());
    }

    #[test]
    fn migration_2_keeps_data_and_references() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("v1.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch(
                "PRAGMA user_version=1;
              INSERT INTO entities(kind,name,norm,created_at,updated_at) VALUES('concept','X','x','t','t');
              INSERT INTO memories(kind,content,norm_hash,recorded_at,origin) VALUES('knowledge','c','h','t','llm');
              INSERT INTO memory_entities VALUES(1,1);",
            )
            .unwrap();
        }
        let c = open(&p).unwrap();
        let n: i64 = c.query_row("SELECT count(*) FROM memory_entities me JOIN memories m ON m.id=me.memory_id JOIN entities e ON e.id=me.entity_id", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        c.execute("INSERT INTO entities(kind,name,norm,created_at,updated_at) VALUES('area','AI','ai','t','t')", [])
            .unwrap();
        c.execute("INSERT INTO memories(kind,content,norm_hash,recorded_at,origin) VALUES('resource','crt.sh','h2','t','llm')", []).unwrap();
        assert!(c.execute("INSERT INTO memory_entities VALUES(99,1)", []).is_err(), "FKs enforced again");
    }

    #[test]
    fn fts_roundtrip() {
        let c = open_memory().unwrap();
        fts_upsert(&c, DocKind::Memory, 5, "PostgreSQL", "indexes and vacuum").unwrap();
        fts_upsert(&c, DocKind::Memory, 5, "PostgreSQL", "replaced body").unwrap();
        let n: i64 = c.query_row("SELECT count(*) FROM fts WHERE fts MATCH 'replaced'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(DocKind::from_rowid(5 * 8 + 1), (DocKind::Memory, 5));
    }
}
