//! Interactive knowledge graph: a single self-contained HTML file (inline JS,
//! no network) rendered from SQLite. Nodes: projects, goals, concepts, ideas,
//! journals and open tasks. Edges: explicit relationships (`links`), entities
//! sharing a memory, journal → what was learned from it, task → project.

use crate::brain::Brain;
use crate::util::{atomic_write, truncate_chars};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub(crate) struct Item {
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) list: Vec<String>,
    pub(crate) kind: String,
    pub(crate) date: Option<String>,
    pub(crate) src: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Node {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) summary: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) items: Vec<Item>,
}

#[derive(Serialize)]
pub(crate) struct Edge {
    pub(crate) s: String,
    pub(crate) t: String,
    pub(crate) rel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) why: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Graph {
    pub(crate) lang: String,
    pub(crate) title: String,
    pub(crate) generated: String,
    pub(crate) vault: String,
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
}

pub(crate) fn build(brain: &Brain) -> Result<Graph> {
    let c = &brain.conn;
    let mut nodes: Vec<Node> = vec![];
    let mut edges: BTreeMap<(String, String), (String, Option<String>)> = BTreeMap::new();
    let mut add_edge = |a: String, b: String, rel: &str, why: Option<String>| {
        if a == b {
            return;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        // explicit relations win over weaker "shared memory" ones
        let e = edges.entry(key).or_insert_with(|| (rel.to_string(), why.clone()));
        if e.0 == "co" || e.0 == "source" || e.0 == "about" {
            *e = (rel.to_string(), why.or(e.1.take()));
        } else if e.1.is_none() {
            e.1 = why;
        }
    };

    // Entities with their memories.
    let mut st = c.prepare("SELECT id, kind, name, summary FROM entities WHERE status='active' ORDER BY id")?;
    let ents: Vec<(i64, String, String, Option<String>)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut mem_stmt = c.prepare(
        "SELECT m.content, m.kind, m.occurred_on, s.path || ':' || b.line FROM memories m
         JOIN memory_entities me ON me.memory_id=m.id LEFT JOIN blocks b ON b.id=m.block_id LEFT JOIN sources s ON s.id=b.source_id
         WHERE me.entity_id=?1 AND m.status='active' ORDER BY coalesce(m.occurred_on, m.recorded_at) DESC LIMIT 40",
    )?;
    let mut task_stmt = c.prepare(
        "SELECT title, kind, due FROM tasks WHERE project_id=?1 AND status IN ('open','proposed') ORDER BY due IS NULL, due LIMIT 12",
    )?;
    for (id, kind, name, summary) in &ents {
        let mut items: Vec<Item> = mem_stmt
            .query_map([id], |r| {
                let full: String = r.get(0)?;
                let (text, list) = match crate::util::split_list(&full) {
                    Some((intro, items)) => (intro, items.iter().map(|t| truncate_chars(t, 300)).collect()),
                    None => (truncate_chars(&full, 1500), vec![]),
                };
                Ok(Item { text, list, kind: r.get(1)?, date: r.get(2)?, src: r.get(3)? })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if kind == "project" || kind == "goal" {
            let tasks: Vec<Item> = task_stmt
                .query_map([id], |r| {
                    Ok(Item {
                        text: r.get(0)?,
                        list: vec![],
                        kind: if r.get::<_, String>(1)? == "deadline" { "deadline".into() } else { "task".into() },
                        date: r.get(2)?,
                        src: None,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            items.splice(0..0, tasks);
        }
        nodes.push(Node {
            id: format!("e{id}"),
            label: name.clone(),
            kind: kind.clone(),
            summary: summary.clone(),
            path: Some(crate::notes::note_path(brain, kind, name).display().to_string()),
            items,
        });
    }

    // Explicit relationships.
    let mut st = c.prepare("SELECT src, dst, relation, why FROM links")?;
    for row in st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
    })? {
        let (a, b, rel, why) = row?;
        add_edge(format!("e{a}"), format!("e{b}"), &rel, why);
    }
    // Entities that share a memory (e.g. knowledge point attached to a project).
    let mut st = c.prepare(
        "SELECT a.entity_id, b.entity_id FROM memory_entities a JOIN memory_entities b
         ON a.memory_id=b.memory_id AND a.entity_id < b.entity_id JOIN memories m ON m.id=a.memory_id WHERE m.status='active'",
    )?;
    for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (a, b) = row?;
        add_edge(format!("e{a}"), format!("e{b}"), "co", None);
    }

    // Journals → entities they produced memories for.
    let mut st = c.prepare(
        "SELECT DISTINCT s.id, s.path, s.entry_date, me.entity_id FROM memories m JOIN memory_entities me ON me.memory_id=m.id
         JOIN blocks b ON b.id=m.block_id JOIN sources s ON s.id=b.source_id WHERE m.status='active'",
    )?;
    let mut journals: BTreeMap<i64, (String, Option<String>)> = BTreeMap::new();
    let mut journal_edges: BTreeSet<(i64, i64)> = BTreeSet::new();
    for row in st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?))
    })? {
        let (sid, path, date, eid) = row?;
        journals.insert(sid, (path, date));
        journal_edges.insert((sid, eid));
    }

    // Open tasks (attached to their project and their source journal).
    let mut st = c.prepare(
        "SELECT t.id, t.title, t.kind, t.due, t.project_id, b.source_id, s.path, s.entry_date FROM tasks t
         LEFT JOIN blocks b ON b.id=t.block_id LEFT JOIN sources s ON s.id=b.source_id WHERE t.status IN ('open','proposed')",
    )?;
    type TaskRow = (i64, String, String, Option<String>, Option<i64>, Option<i64>, Option<String>, Option<String>);
    let tasks: Vec<TaskRow> = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (id, title, kind, due, proj, sid, path, date) in tasks {
        let tid = format!("t{id}");
        nodes.push(Node {
            id: tid.clone(),
            label: title.clone(),
            kind: if kind == "deadline" { "deadline".into() } else { "task".into() },
            summary: due.as_ref().map(|d| format!("Due {d}")),
            path: None,
            items: vec![],
        });
        if let Some(p) = proj {
            add_edge(tid.clone(), format!("e{p}"), "task_of", None);
        }
        if let (Some(sid), Some(path)) = (sid, path) {
            journals.insert(sid, (path, date));
            add_edge(tid, format!("j{sid}"), "source", None);
        }
    }

    // Resources ("to explore") as small nodes attached to their area/concept.
    let mut st = c.prepare(
        "SELECT m.id, m.content, m.occurred_on, s.path || ':' || b.line FROM memories m
         LEFT JOIN blocks b ON b.id=m.block_id LEFT JOIN sources s ON s.id=b.source_id
         WHERE m.kind='resource' AND m.status='active'",
    )?;
    let resources: Vec<(i64, String, Option<String>, Option<String>)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut res_ents = c.prepare("SELECT entity_id FROM memory_entities WHERE memory_id=?1")?;
    for (id, content, date, src) in resources {
        let rid = format!("r{id}");
        let label = content.split(" - ").next().unwrap_or(&content).to_string();
        nodes.push(Node {
            id: rid.clone(),
            label: truncate_chars(&label, 60),
            kind: "resource".into(),
            summary: Some(content.clone()),
            path: None,
            items: vec![Item { text: content, list: vec![], kind: "resource".into(), date, src }],
        });
        for e in res_ents.query_map([id], |r| r.get::<_, i64>(0))? {
            add_edge(rid.clone(), format!("e{}", e?), "about", None);
        }
    }

    for (sid, (path, date)) in &journals {
        let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(path.clone());
        nodes.push(Node {
            id: format!("j{sid}"),
            label: stem,
            kind: "journal".into(),
            summary: date.as_ref().map(|d| format!("Journal of {d}")),
            path: Some(brain.vault().join(path).display().to_string()),
            items: vec![],
        });
    }
    for (sid, eid) in journal_edges {
        add_edge(format!("j{sid}"), format!("e{eid}"), "source", None);
    }

    let ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let edges: Vec<Edge> = edges
        .into_iter()
        .filter(|((a, b), _)| ids.contains(a.as_str()) && ids.contains(b.as_str()))
        .map(|((s, t), (rel, why))| Edge { s, t, rel, why })
        .collect();
    Ok(Graph {
        lang: brain.ui_language(),
        title: "Second Brain".into(),
        generated: brain.local(brain.now()).format("%Y-%m-%d %H:%M").to_string(),
        vault: brain.vault().display().to_string(),
        nodes,
        edges,
    })
}

/// Write the graph page. Returns (path, node count, edge count).
pub fn write(brain: &Brain, out: Option<&Path>) -> Result<(PathBuf, usize, usize)> {
    let g = build(brain)?;
    let (n, e) = (g.nodes.len(), g.edges.len());
    // Safe to embed in <script>: no "</" sequences can close the tag.
    let json =
        serde_json::to_string(&g)?.replace("</", "<\\/").replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029");
    let html = TEMPLATE.replace("/*__DATA__*/null", &json);
    let path = out.map(Path::to_path_buf).unwrap_or_else(|| brain.home.join("data/graph.html"));
    atomic_write(&path, &html)?;
    Ok((path, n, e))
}

pub fn open(path: &Path) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Degree per node id (used by tests).
pub fn degrees(brain: &Brain) -> Result<HashMap<String, usize>> {
    let g = build(brain)?;
    let mut d: HashMap<String, usize> = g.nodes.iter().map(|n| (n.label.clone(), 0)).collect();
    let label: HashMap<&str, &str> = g.nodes.iter().map(|n| (n.id.as_str(), n.label.as_str())).collect();
    for e in &g.edges {
        *d.entry(label[e.s.as_str()].to_string()).or_default() += 1;
        *d.entry(label[e.t.as_str()].to_string()).or_default() += 1;
    }
    Ok(d)
}

const TEMPLATE: &str = include_str!("graph.html");
