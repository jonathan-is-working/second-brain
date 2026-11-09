//! `sb dash`: keyboard-driven terminal dashboard with a vim feel.
//!
//! Views: 1 tableau de bord · 2 activité · 3 thèmes.
//! Tableau de bord panels (Tab / Shift-Tab to move between them):
//!   calendrier du mois (hjkl jour, [ ] mois, t aujourd'hui) · jour sélectionné
//!   tâches (f à faire / terminées, x fait ou rouvrir, c annuler, +/- priorité)
//!   notes récentes (Entrée ouvre dans $EDITOR) · activité · récemment appris
//! Everywhere: : commande (every `sb` command, palette + Tab) · / recherche
//!   u historique et annulation · a tâche · e événement · S Google · ? aide · q quitter
//!
//! Reads the local database only (instant, 0 model calls); Google is contacted at
//! start, on S, and after you change a task.

use crate::brain::Brain;
use crate::calendar::Backend;
use crate::tasks::{self, TaskRow};
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Circle, Line as CLine, Points};
use ratatui::widgets::{
    Bar, BarChart, BarGroup, Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration as StdDuration, Instant, SystemTime};

// ---------------------------------------------------------------------------
// Language: English by default, French with `ui_language = "fr"` or the `L` key.

static EN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn en() -> bool {
    EN.load(std::sync::atomic::Ordering::Relaxed)
}

fn set_en(v: bool) {
    EN.store(v, std::sync::atomic::Ordering::Relaxed)
}

/// Pick the text for the current language.
fn tr(fr: &'static str, en_text: &'static str) -> &'static str {
    if en() { en_text } else { fr }
}

/// `format!` in the current language: `trf!("il y a {} min", "{} min ago", n)`.
macro_rules! trf {
    ($fr:literal, $en:literal $(, $a:expr)* $(,)?) => {
        if en() { format!($en $(, $a)*) } else { format!($fr $(, $a)*) }
    };
}

// Colours come from the active theme (crate::theme), shared with nvim.
fn tc(hex: &str) -> Color {
    let (r, g, b) = crate::theme::hex(hex);
    Color::Rgb(r, g, b)
}
fn tm(a: &str, b: &str, t: f32) -> Color {
    let (r, g, b) = crate::theme::mix(a, b, t);
    Color::Rgb(r, g, b)
}
#[allow(non_snake_case)]
fn BG() -> Color {
    tc(crate::theme::current().bg)
}
#[allow(non_snake_case)]
fn ACCENT() -> Color {
    tc(crate::theme::current().accent)
}
#[allow(non_snake_case)]
fn MUTED() -> Color {
    tc(crate::theme::current().muted)
}
#[allow(non_snake_case)]
fn FAINT() -> Color {
    tc(crate::theme::current().faint)
}
#[allow(non_snake_case)]
fn TEXT() -> Color {
    tc(crate::theme::current().text)
}
#[allow(non_snake_case)]
fn DUE() -> Color {
    tc(crate::theme::current().due)
}
#[allow(non_snake_case)]
fn OK() -> Color {
    tc(crate::theme::current().ok)
}
#[allow(non_snake_case)]
fn WARN() -> Color {
    tc(crate::theme::current().warn)
}
#[allow(non_snake_case)]
fn AREA() -> Color {
    tc(crate::theme::current().area)
}
#[allow(non_snake_case)]
fn THEME() -> Color {
    tc(crate::theme::current().theme)
}
#[allow(non_snake_case)]
fn SESSION() -> Color {
    tc(crate::theme::current().session)
}
#[allow(non_snake_case)]
fn CONCEPT() -> Color {
    tc(crate::theme::current().concept)
}
#[allow(non_snake_case)]
fn RESOURCE() -> Color {
    tc(crate::theme::current().resource)
}
#[allow(non_snake_case)]
fn SEL_BG() -> Color {
    tc(crate::theme::current().sel)
}
/// Contribution levels 0..4: from the background to the theme's green.
#[allow(non_snake_case)]
fn HEAT() -> [Color; 5] {
    let t = crate::theme::current();
    [tm(t.bg, t.text, 0.10), tm(t.bg, t.ok, 0.3), tm(t.bg, t.ok, 0.5), tm(t.bg, t.ok, 0.72), tc(t.ok)]
}

const JOURS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
const MOIS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
const MOIS_COURT: [&str; 12] = ["jan", "fév", "mar", "avr", "mai", "jun", "jul", "aoû", "sep", "oct", "nov", "déc"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_SHORT: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn month_name(m0: u32) -> &'static str {
    if en() { MONTHS[m0 as usize] } else { MOIS[m0 as usize] }
}

fn month_short(m0: u32) -> &'static str {
    if en() { MONTHS_SHORT[m0 as usize] } else { MOIS_COURT[m0 as usize] }
}

/// Commands of the dashboard itself (shown in the palette with the `sb` ones).
fn builtins() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "e",
            tr("écrire le journal du jour dans $EDITOR, puis ingest", "write today's journal in $EDITOR, then ingest"),
        ),
        ("w", tr("ingest (analyser les journaux modifiés)", "ingest (process changed journals)")),
        ("wq", tr("ingest puis quitter", "ingest then quit")),
        ("q", tr("quitter le tableau de bord", "quit the dashboard")),
        (
            "lang",
            tr(
                "passer le tableau de bord en anglais / français (touche L)",
                "switch the dashboard between English / French (key L)",
            ),
        ),
        ("theme", tr("choisir le thème, nvim suit (touche T)", "pick the theme, nvim follows (key T)")),
    ]
}

/// "Sunday 8 November" / "dimanche 8 novembre".
fn fr_date(d: NaiveDate) -> String {
    let wd = d.weekday().num_days_from_monday() as usize;
    if en() {
        format!("{} {} {}", DAYS[wd], d.day(), MONTHS[d.month0() as usize])
    } else {
        format!("{} {} {}", JOURS[wd], d.day(), MOIS[d.month0() as usize])
    }
}

fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    let m0 = d.year() * 12 + d.month0() as i32 + n;
    let (y, m) = (m0.div_euclid(12), m0.rem_euclid(12) as u32 + 1);
    let last = (1..=31).rev().find(|day| NaiveDate::from_ymd_opt(y, m, *day).is_some()).unwrap_or(28);
    NaiveDate::from_ymd_opt(y, m, d.day().min(last)).unwrap_or(d)
}

fn ago(t: SystemTime) -> String {
    let s = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match s {
        0..=59 => tr("à l'instant", "just now").into(),
        60..=3599 => trf!("il y a {} min", "{} min ago", s / 60),
        3600..=86_399 => trf!("il y a {} h", "{} h ago", s / 3600),
        _ => trf!("il y a {} j", "{} d ago", s / 86_400),
    }
}

// ---------------------------------------------------------------------------
// Data

/// What a calendar line is, so it can be deleted.
#[derive(Clone)]
enum Ref {
    None,
    /// Event of "Mon agenda" (needs events access).
    Remote(Box<crate::gcal_admin::Event>),
    /// Blocked period in the Second Brain calendar (event id).
    Block(String),
    Session(i64),
    Task(i64),
    /// Delete the task for good (D), not just cancel it.
    DeleteTask(i64),
}

#[derive(Clone)]
struct DayItem {
    time: String,
    title: String,
    kind: u8, // 0 event, 1 work session, 2 all-day, 3 deadline/due task
    r: Ref,
}

impl DayItem {
    fn key(&self) -> String {
        match &self.r {
            Ref::None => format!("n:{}:{}", self.time, self.title),
            Ref::Remote(e) => format!("r:{}", e.id),
            Ref::Block(id) => format!("b:{id}"),
            Ref::Session(id) => format!("s:{id}"),
            Ref::Task(id) | Ref::DeleteTask(id) => format!("t:{id}"),
        }
    }
}

struct Metric {
    name: &'static str,
    unit: &'static str,
    days: BTreeMap<NaiveDate, u64>,
}

struct TreeRow {
    depth: u8,
    kind: String,
    name: String,
    count: usize,
    entity: Option<i64>,
}

struct NoteFile {
    path: PathBuf,
    rel: String,
    name: String,
    modified: SystemTime,
}

#[derive(Default)]
struct Snapshot {
    open: Vec<TaskRow>,
    finished: Vec<TaskRow>,
    recent: Vec<(String, String, String)>, // (kind, item, text)
    memory: Option<(String, String, String)>,
    notes: Vec<NoteFile>,
    metrics: Vec<Metric>,
    tree: Vec<TreeRow>,
    /// Per day of the shown month: (events, deadlines, sessions).
    month: HashMap<NaiveDate, (usize, usize, usize)>,
    day: Vec<DayItem>,
    today: Vec<DayItem>,
    upcoming: Vec<TaskRow>,
    cost_today: f64,
}

fn day_items(brain: &Brain, d: NaiveDate, remote: &[(NaiveDate, DayItem)], titled: bool) -> Result<Vec<DayItem>> {
    let (from, to) = (brain.day_start(d), brain.day_start(d + Duration::days(1)));
    let mut v: Vec<(String, DayItem)> = vec![];
    let busy: Vec<(String, String, String, Option<String>)> = brain
        .conn
        .prepare("SELECT source, start, end, title FROM busy WHERE end > ?1 AND start < ?2 ORDER BY start")?
        .query_map([crate::util::ts(from), crate::util::ts(to)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (source, st, en, title) in busy {
        let (Ok(a), Ok(b)) = (crate::util::parse_ts(&st), crate::util::parse_ts(&en)) else { continue };
        let whole = a <= from && b >= to;
        // Free/busy blocks have no title: replaced by real events when we can read them.
        if title.is_none() && titled {
            continue;
        }
        let (s, e) = (brain.local(a), brain.local(b));
        v.push((
            if whole { "00".into() } else { s.format("%H:%M").to_string() },
            DayItem {
                time: if whole {
                    tr("journée", "all day").into()
                } else {
                    format!("{}-{}", s.format("%H:%M"), e.format("%H:%M"))
                },
                title: title.unwrap_or_else(|| tr("occupé", "busy").into()),
                kind: if whole { 2 } else { 0 },
                r: source.strip_prefix("block:").map(|id| Ref::Block(id.to_string())).unwrap_or(Ref::None),
            },
        ));
    }
    for (_, l) in remote.iter().filter(|(day, _)| *day == d) {
        v.push((if l.kind == 2 { "00".into() } else { l.time.clone() }, l.clone()));
    }
    for s in crate::planner::sessions_between(brain, from, to, &["planned", "done"])? {
        let (a, b) = (brain.local(s.iv.start), brain.local(s.iv.end));
        v.push((
            a.format("%H:%M").to_string(),
            DayItem {
                time: format!("{}-{}", a.format("%H:%M"), b.format("%H:%M")),
                title: trf!("focus : {}", "focus: {}", crate::planner::task_title(brain, s.task_id)),
                kind: 1,
                r: Ref::Session(s.id),
            },
        ));
    }
    for t in tasks::open_tasks(brain)?.into_iter().filter(|t| t.due == Some(d)) {
        let what = if t.kind == "deadline" { tr("échéance", "deadline") } else { tr("à rendre", "due") };
        v.push(("0".into(), DayItem { time: what.into(), title: t.title, kind: 3, r: Ref::Task(t.id) }));
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(v.into_iter().map(|x| x.1).collect())
}

fn month_marks(
    brain: &Brain,
    first: NaiveDate,
    remote: &[(NaiveDate, DayItem)],
) -> Result<HashMap<NaiveDate, (usize, usize, usize)>> {
    let last = add_months(first, 1) - Duration::days(1);
    let mut m: HashMap<NaiveDate, (usize, usize, usize)> = HashMap::new();
    let (from, to) = (brain.day_start(first), brain.day_start(last + Duration::days(1)));
    for (iv, _) in crate::planner::busy_between(brain, from, to)? {
        let mut d = brain.local(iv.start).date_naive().max(first);
        let end = brain.local(iv.end - Duration::seconds(1)).date_naive().min(last);
        while d <= end {
            m.entry(d).or_default().0 += 1;
            d += Duration::days(1);
        }
    }
    for (d, _) in remote {
        m.entry(*d).or_default().0 += 1;
    }
    for s in crate::planner::sessions_between(brain, from, to, &["planned", "done"])? {
        m.entry(brain.local(s.iv.start).date_naive()).or_default().2 += 1;
    }
    for t in tasks::open_tasks(brain)? {
        if let Some(d) = t.due.filter(|d| *d >= first && *d <= last) {
            m.entry(d).or_default().1 += 1;
        }
    }
    Ok(m)
}

fn per_day(conn: &rusqlite::Connection, sql: &str) -> Result<BTreeMap<NaiveDate, u64>> {
    let mut m = BTreeMap::new();
    let rows: Vec<(Option<String>, i64)> =
        conn.prepare(sql)?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    for (d, n) in rows {
        if let Some(d) = d.as_deref().and_then(crate::util::parse_date) {
            *m.entry(d).or_insert(0) += n.max(0) as u64;
        }
    }
    Ok(m)
}

fn metrics(conn: &rusqlite::Connection) -> Result<Vec<Metric>> {
    let mut words = BTreeMap::new();
    let mut blocks = BTreeMap::new();
    let rows: Vec<(Option<String>, String)> = conn
        .prepare("SELECT s.entry_date, b.text FROM blocks b JOIN sources s ON s.id=b.source_id WHERE b.present=1")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (d, text) in rows {
        if let Some(d) = d.as_deref().and_then(crate::util::parse_date) {
            *words.entry(d).or_insert(0u64) += text.split_whitespace().count() as u64;
            *blocks.entry(d).or_insert(0u64) += 1;
        }
    }
    let links = per_day(
        conn,
        "SELECT coalesce(s.entry_date, date(l.created_at, 'localtime')), count(*) FROM links l
         LEFT JOIN blocks b ON b.id=l.block_id LEFT JOIN sources s ON s.id=b.source_id
         WHERE l.relation NOT IN ('part_of','in_theme','in_area') GROUP BY 1",
    )?;
    let knowledge = per_day(
        conn,
        "SELECT coalesce(s.entry_date, date(m.recorded_at, 'localtime')), count(*) FROM memories m
         LEFT JOIN blocks b ON b.id=m.block_id LEFT JOIN sources s ON s.id=b.source_id
         WHERE m.status='active' AND m.kind != 'profile' GROUP BY 1",
    )?;
    let done = per_day(
        conn,
        "SELECT date(completed_at, 'localtime'), count(*) FROM tasks
         WHERE status='done' AND completed_at IS NOT NULL GROUP BY 1",
    )?;
    let focus = per_day(
        conn,
        "SELECT date(start, 'localtime'), cast(sum((julianday(end) - julianday(start)) * 1440) AS INTEGER)
         FROM sessions WHERE status='done' GROUP BY 1",
    )?;
    let calls = per_day(
        conn,
        "SELECT date(ts, 'localtime'), count(*) FROM llm_calls WHERE provider != 'cache' AND ok=1 GROUP BY 1",
    )?;
    let mut activity = blocks;
    for m in [&knowledge, &links, &done] {
        for (d, n) in m {
            *activity.entry(*d).or_insert(0) += n;
        }
    }
    Ok(vec![
        Metric { name: tr("Activité", "Activity"), unit: tr("contributions", "contributions"), days: activity },
        Metric { name: tr("Mots écrits", "Words written"), unit: tr("mots", "words"), days: words },
        Metric { name: tr("Connexions", "Connections"), unit: tr("liens", "links"), days: links },
        Metric { name: tr("Idées & savoirs", "Ideas & knowledge"), unit: tr("éléments", "items"), days: knowledge },
        Metric { name: tr("Tâches faites", "Tasks done"), unit: tr("tâches", "tasks"), days: done },
        Metric { name: tr("Focus", "Focus"), unit: "min", days: focus },
        Metric { name: tr("Appels IA", "AI calls"), unit: tr("appels", "calls"), days: calls },
    ])
}

fn tree(conn: &rusqlite::Connection) -> Result<Vec<TreeRow>> {
    let ents = crate::connect::entities(conn)?;
    let id_of: HashMap<String, i64> = conn
        .prepare("SELECT name, id FROM entities WHERE status='active'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let tax = crate::connect::taxonomy(conn)?;
    let mut areas: Vec<_> = tax.areas.iter().collect();
    let size = |x: &(usize, BTreeMap<String, usize>)| x.0 + x.1.values().sum::<usize>();
    areas.sort_by(|a, b| size(b.1).cmp(&size(a.1)).then(a.0.cmp(b.0)));
    let row = |depth, kind: &str, name: &str, count| TreeRow {
        depth,
        kind: kind.to_string(),
        name: name.to_string(),
        count,
        entity: id_of.get(name).copied(),
    };
    let mut rows = vec![];
    for (area, x) in areas {
        rows.push(row(0, "area", area, size(x)));
        for (theme, n) in &x.1 {
            rows.push(row(1, "theme", theme, *n));
            for e in ents.iter().filter(|e| e.theme.as_deref() == Some(theme) && e.area.as_deref() == Some(area)) {
                rows.push(row(2, &e.kind, &e.name, 0));
            }
        }
        for e in ents.iter().filter(|e| e.theme.is_none() && e.area.as_deref() == Some(area)) {
            rows.push(row(1, &e.kind, &e.name, 0));
        }
    }
    Ok(rows)
}

/// Most recently modified notes of the vault (journals, knowledge, projects…).
fn recent_notes(brain: &Brain, limit: usize) -> Vec<NoteFile> {
    let vault = brain.vault();
    let mut out = vec![];
    let mut stack = vec![vault.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "archive" {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if name.ends_with(".md")
                && let Ok(m) = e.metadata().and_then(|m| m.modified())
            {
                let rel = p.strip_prefix(&vault).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                out.push(NoteFile { name: name.trim_end_matches(".md").to_string(), rel, path: p, modified: m });
            }
        }
    }
    out.sort_by_key(|n| std::cmp::Reverse(n.modified));
    out.truncate(limit);
    out
}

fn finished_tasks(brain: &Brain) -> Result<Vec<TaskRow>> {
    let mut v = tasks::query(&brain.conn, "t.status IN ('done','cancelled')", &tasks::now_str(brain))?;
    v.sort_by(|a, b| b.completed_at.cmp(&a.completed_at).then(b.id.cmp(&a.id)));
    v.truncate(60);
    Ok(v)
}

/// Events with titles from "Mon agenda" for [from, to] (events access only).
fn fetch_remote(brain: &Brain, be: &Backend, from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, DayItem)> {
    if !brain.cfg.calendar.events_access {
        return vec![];
    }
    let Ok(evs) = crate::gcal_admin::list(brain, be, "primary", from, to) else { return vec![] };
    let mut out = vec![];
    for e in evs {
        for (day, time, kind) in event_days(brain, &e.raw) {
            if day < from || day > to {
                continue;
            }
            out.push((day, DayItem { time, title: e.title.clone(), kind, r: Ref::Remote(Box::new(e.clone())) }));
        }
    }
    out
}

/// Every day an event covers, with the time label for that day:
/// all-day events → "journée" each day; timed ones → "20:00 →", "journée", "→ 16:00".
fn event_days(brain: &Brain, raw: &serde_json::Value) -> Vec<(NaiveDate, String, u8)> {
    let date = |k: &str| raw[k]["date"].as_str().and_then(crate::util::parse_date);
    let time = |k: &str| {
        raw[k]["dateTime"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&brain.tz).naive_local())
    };
    if let Some(a) = date("start") {
        // All-day: Google's end date is exclusive.
        let end = date("end").map(|e| e - Duration::days(1)).unwrap_or(a).max(a);
        return a
            .iter_days()
            .take_while(|d| *d <= end)
            .take(400)
            .map(|d| (d, tr("journée", "all day").to_string(), 2))
            .collect();
    }
    let (Some(a), Some(b)) = (time("start"), time("end")) else { return vec![] };
    let (da, mut db) = (a.date(), b.date());
    if db > da && b.time() == chrono::NaiveTime::MIN {
        db -= Duration::days(1); // ends at midnight: the next day is not really covered
    }
    if da == db {
        return vec![(da, format!("{}-{}", a.format("%H:%M"), b.format("%H:%M")), 0)];
    }
    da.iter_days()
        .take_while(|d| *d <= db)
        .take(400)
        .map(|d| {
            if d == da {
                (d, format!("{} →", a.format("%H:%M")), 0)
            } else if d == db && b.date() == db {
                (d, format!("→ {}", b.format("%H:%M")), 0)
            } else {
                (d, tr("journée", "all day").to_string(), 2)
            }
        })
        .collect()
}

fn kind_label(k: &str) -> &'static str {
    match k {
        "area" => tr("Grand thème", "Grand theme"),
        "theme" => tr("Thème", "Theme"),
        "project" => tr("Projet", "Project"),
        "goal" => tr("Objectif", "Goal"),
        "idea" => tr("Idée", "Idea"),
        "concept" => tr("Concept", "Concept"),
        _ => tr("Élément", "Item"),
    }
}

fn kind_icon(kind: &str) -> (&'static str, Color) {
    match kind {
        "idea" => ("◆", ACCENT()),
        "resource" => ("◇", RESOURCE()),
        "decision" => ("■", WARN()),
        "theme" => ("▸", THEME()),
        "area" => ("■", AREA()),
        "project" => ("◈", WARN()),
        "goal" => ("◎", OK()),
        _ => ("●", CONCEPT()),
    }
}

/// Details of one entity for the Thèmes view.
fn entity_detail(brain: &Brain, id: i64) -> Vec<Line<'static>> {
    let c = &brain.conn;
    let mut out = vec![];
    let Ok((kind, name, summary)) = c.query_row("SELECT kind, name, summary FROM entities WHERE id=?1", [id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
    }) else {
        return out;
    };
    out.push(Line::from(Span::styled(name, Style::new().fg(TEXT()).add_modifier(Modifier::BOLD))));
    out.push(Line::from(Span::styled(kind_label(&kind), Style::new().fg(MUTED()))));
    if let Some(s) = summary {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(s, Style::new().fg(TEXT()))));
    }
    let children: Vec<(String, String)> = c
        .prepare(
            "SELECT e.kind, e.name FROM links l JOIN entities e ON e.id=l.src
             WHERE l.dst=?1 AND l.relation IN ('part_of','in_theme','in_area') AND e.status='active'
             ORDER BY e.kind='theme' DESC, e.name",
        )
        .and_then(|mut st| st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default();
    if !children.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            trf!("Contient ({})", "Contains ({})", children.len()),
            Style::new().fg(ACCENT()),
        )));
        for (k, n) in children {
            let (g, col) = kind_icon(&k);
            out.push(Line::from(vec![
                Span::styled(format!("{g} "), Style::new().fg(col)),
                Span::styled(n, Style::new().fg(TEXT())),
            ]));
        }
    }
    let mems: Vec<String> = c
        .prepare(
            "SELECT m.content FROM memories m JOIN memory_entities me ON me.memory_id=m.id
             WHERE me.entity_id=?1 AND m.status='active' ORDER BY m.id DESC LIMIT 6",
        )
        .and_then(|mut st| st.query_map([id], |r| r.get(0))?.collect())
        .unwrap_or_default();
    if !mems.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            tr("Ce que le cerveau sait", "What the brain knows"),
            Style::new().fg(ACCENT()),
        )));
        for m in mems {
            match crate::util::split_list(&m) {
                Some((intro, items)) => {
                    if !intro.is_empty() {
                        out.push(Line::from(Span::styled(format!("• {intro}"), Style::new().fg(TEXT()))));
                    }
                    for (i, it) in items.iter().enumerate() {
                        out.push(Line::from(Span::styled(format!("   {}. {it}", i + 1), Style::new().fg(TEXT()))));
                    }
                }
                None => out.push(Line::from(Span::styled(format!("• {m}"), Style::new().fg(TEXT())))),
            }
        }
    }
    let links: Vec<(String, Option<String>)> = c
        .prepare(
            "SELECT e.name, l.why FROM links l JOIN entities e ON e.id = CASE WHEN l.src=?1 THEN l.dst ELSE l.src END
             WHERE (l.src=?1 OR l.dst=?1) AND l.relation NOT IN ('part_of','in_theme','in_area') AND e.status='active'",
        )
        .and_then(|mut st| st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
        .unwrap_or_default();
    if !links.is_empty() {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            trf!("Connexions ({})", "Connections ({})", links.len()),
            Style::new().fg(ACCENT()),
        )));
        for (n, why) in links {
            let mut spans = vec![Span::styled(format!("↔ {n}"), Style::new().fg(TEXT()))];
            if let Some(w) = why {
                spans.push(Span::styled(format!("  {w}"), Style::new().fg(MUTED())));
            }
            out.push(Line::from(spans));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// App state

#[derive(Clone, Copy, PartialEq)]
enum View {
    Board,
    Activity,
    Themes,
    Commands,
    Graph,
}

#[derive(Clone, Copy, PartialEq)]
enum Panel {
    // avant
    Activity,
    Done,
    Notes,
    Recent,
    // aujourd'hui
    Today,
    Tasks,
    // à venir
    Calendar,
    Day,
}

/// Tab order: left to right (past → today → future), top to bottom.
const PANELS: [Panel; 8] = [
    Panel::Activity,
    Panel::Done,
    Panel::Notes,
    Panel::Recent,
    Panel::Today,
    Panel::Tasks,
    Panel::Calendar,
    Panel::Day,
];

enum Mode {
    Normal,
    Command(String),
    Search(String),
    AddTask(String),
    AddEvent(String),
    /// Title of a blocked period over the calendar range (weekdays only?).
    Block(String, bool),
    /// Editing one field (index in TASK_FIELDS) of the open task card.
    EditField(usize, String),
}

/// Fields of the task card, in display order.
const TASK_FIELDS: [&str; 6] = ["title", "due", "estimate", "priority", "project", "description"];

fn field_label(f: &str) -> &'static str {
    match f {
        "title" => tr("titre", "title"),
        "due" => tr("échéance", "due"),
        "estimate" => tr("durée estimée", "estimate"),
        "priority" => tr("priorité", "priority"),
        "project" => tr("projet", "project"),
        _ => tr("description", "description"),
    }
}

/// What the project picker is choosing for.
enum ProjTarget {
    /// The task being typed in the "new task" box (its text so far).
    NewTask(String),
    /// The project field of the open task card.
    Card(i64),
}

struct ProjPicker {
    items: Vec<(String, i64)>,
    filter: String,
    state: ListState,
    target: ProjTarget,
}

/// The task card opened with Enter on a task.
struct TaskCard {
    id: i64,
    field: usize,
}

struct Picker {
    title: String,
    items: Vec<(DayItem, NaiveDate, bool)>,
    state: ListState,
}

struct Pager {
    title: String,
    lines: Vec<String>,
    scroll: usize,
}

struct Job {
    label: String,
    rx: Receiver<(bool, String)>,
    started: Instant,
    show: bool,
}

struct App {
    snap: Snapshot,
    view: View,
    focus: Panel,
    tasks: ListState,
    day: ListState,
    notes: ListState,
    recent: ListState,
    tree: ListState,
    history_list: ListState,
    done: ListState,
    today_list: ListState,
    selected_day: NaiveDate,
    remote_month: (NaiveDate, Vec<(NaiveDate, DayItem)>),
    metric: usize,
    mode: Mode,
    palette: usize,
    commands: Vec<(String, String)>,
    /// Full `--help` text per command, for the Commandes page.
    help_text: HashMap<String, String>,
    cmd_list: ListState,
    graph: Option<TGraph>,
    gview: GView,
    pager: Option<Pager>,
    undo_list: Option<Vec<crate::actions::ActionRow>>,
    /// Range start in the calendar (`v`), the end being the selected day.
    range: Option<NaiveDate>,
    picker: Option<Picker>,
    card: Option<TaskCard>,
    proj_picker: Option<ProjPicker>,
    /// Date picker for a task's due date: (task id, highlighted day).
    date_picker: Option<(i64, NaiveDate)>,
    /// Theme picker: (highlighted theme, theme to restore on Esc).
    theme_picker: Option<(ListState, usize)>,
    /// Project of the next task added with `a` (default project unless changed with Tab).
    add_project: String,
    /// Pending destructive action: (question, items to delete).
    confirm: Option<(String, Vec<DayItem>)>,
    help: bool,
    job: Option<Job>,
    status: String,
    google: Option<Backend>,
    cmd_history: Vec<String>,
    hist_pos: Option<usize>,
    last_cmd: Option<String>,
    pending_g: bool,
    quit: bool,
}

impl App {
    fn new(brain: &Brain, commands: Vec<(String, String, String)>) -> App {
        // Language: the `L` choice (data/dash_lang) wins over config `ui_language`.
        let lang = brain.ui_language();
        set_en(!lang.to_lowercase().starts_with("fr"));
        crate::theme::load(brain);
        let cmd_history = std::fs::read_to_string(brain.home.join("data/dash_history"))
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default();
        let mut all: Vec<(String, String)> = builtins().iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let help_text: HashMap<String, String> = commands.iter().map(|(n, _, h)| (n.clone(), h.clone())).collect();
        all.extend(commands.into_iter().map(|(n, a, _)| (n, a)));
        sort_commands(&mut all);
        App {
            snap: Snapshot::default(),
            view: View::Board,
            focus: Panel::Tasks,
            tasks: ListState::default(),
            day: ListState::default(),
            notes: ListState::default(),
            recent: ListState::default(),
            tree: ListState::default(),
            history_list: ListState::default(),
            done: ListState::default(),
            today_list: ListState::default(),
            selected_day: brain.today() + Duration::days(1),
            remote_month: (NaiveDate::MIN, vec![]),
            metric: 0,
            mode: Mode::Normal,
            palette: 0,
            commands: all,
            help_text,
            cmd_list: ListState::default().with_selected(Some(0)),
            graph: None,
            gview: GView::default(),
            pager: None,
            undo_list: None,
            range: None,
            picker: None,
            card: None,
            proj_picker: None,
            date_picker: None,
            theme_picker: None,
            add_project: brain.cfg.default_project.clone(),
            confirm: None,
            help: false,
            job: None,
            status: tr("prêt", "ready").into(),
            google: Backend::google(brain).ok().flatten(),
            cmd_history,
            hist_pos: None,
            last_cmd: None,
            pending_g: false,
            quit: false,
        }
    }

    fn titled(&self, brain: &Brain) -> bool {
        self.google.is_some() && brain.cfg.calendar.events_access
    }

    /// Month events from Google, fetched once per shown month.
    fn ensure_month(&mut self, brain: &Brain, force: bool) {
        let first = month_start(self.selected_day);
        if !force && self.remote_month.0 == first {
            return;
        }
        let last = add_months(first, 1) - Duration::days(1);
        let ev = match &self.google {
            Some(be) => fetch_remote(brain, be, first, last),
            None => vec![],
        };
        self.remote_month = (first, ev);
    }

    fn refresh(&mut self, brain: &Brain) {
        if let Err(e) = self.load(brain) {
            self.status = trf!("erreur : {e:#}", "error: {e:#}");
        }
        let n_tasks = self.snap.open.len();
        let lens = [
            (&mut self.tasks, n_tasks),
            (&mut self.done, self.snap.finished.len()),
            (&mut self.today_list, self.snap.today.len()),
            (&mut self.day, self.snap.day.len()),
            (&mut self.notes, self.snap.notes.len()),
            (&mut self.recent, self.snap.recent.len()),
            (&mut self.tree, self.snap.tree.len()),
        ];
        for (st, n) in lens {
            match st.selected() {
                _ if n == 0 => st.select(None),
                None => st.select(Some(0)),
                Some(i) if i >= n => st.select(Some(n - 1)),
                _ => {}
            }
        }
    }

    fn load(&mut self, brain: &Brain) -> Result<()> {
        let c = &brain.conn;
        self.ensure_month(brain, false);
        let titled = self.titled(brain);
        let s = &mut self.snap;
        s.open = tasks::ranked(brain)?;
        s.open.extend(tasks::open_tasks(brain)?.into_iter().filter(|t| t.kind == "deadline"));
        s.finished = finished_tasks(brain)?;
        let item_sql = "coalesce((SELECT e.name FROM memory_entities me JOIN entities e ON e.id=me.entity_id
                                    WHERE me.memory_id=m.id LIMIT 1), '')";
        s.recent = c
            .prepare(&format!(
                "SELECT m.kind, {item_sql}, m.content FROM memories m
                 WHERE m.status='active' AND m.kind IN ('knowledge','idea','resource','decision') ORDER BY m.id DESC LIMIT 30"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let pool: Vec<(String, String, String)> = c
            .prepare(&format!(
                "SELECT coalesce(m.occurred_on, date(m.recorded_at)), {item_sql}, m.content FROM memories m
                 WHERE m.status='active' AND m.kind IN ('knowledge','idea') ORDER BY m.recorded_at, m.id"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        s.memory = (!pool.is_empty()).then(|| {
            let older = pool.len().div_ceil(2).max(1);
            pool[(brain.today().num_days_from_ce() as usize).wrapping_mul(7919) % older].clone()
        });
        s.notes = recent_notes(brain, 40);
        s.metrics = metrics(c)?;
        s.tree = tree(c)?;
        s.month = month_marks(brain, month_start(self.selected_day), &self.remote_month.1)?;
        s.day = day_items(brain, self.selected_day, &self.remote_month.1, titled)?;
        let today = brain.today();
        let today_titled = titled && month_start(today) == self.remote_month.0;
        s.today = day_items(brain, today, &self.remote_month.1, today_titled)?;
        s.upcoming = tasks::deadlines(brain, 30)?.into_iter().filter(|t| t.due.is_some_and(|d| d > today)).collect();
        s.cost_today = c.query_row(
            "SELECT coalesce(sum(cost_usd), 0) FROM llm_calls WHERE ts >= ?1 AND provider != 'cache'",
            [crate::util::ts(brain.day_start(brain.today()))],
            |r| r.get(0),
        )?;
        Ok(())
    }

    fn selected_task(&self) -> Option<TaskRow> {
        if self.focus == Panel::Done {
            self.done.selected().and_then(|i| self.snap.finished.get(i)).cloned()
        } else {
            self.tasks.selected().and_then(|i| self.snap.open.get(i)).cloned()
        }
    }

    fn sync(&mut self, brain: &Brain, changed: bool) {
        let Some(be) = &self.google else {
            self.status = tr("Google non connecté (:calendar auth)", "Google not connected (:calendar auth)").into();
            return;
        };
        let cal = crate::calendar::sync(brain, be, brain.cfg.autonomy.calendar_writes == "auto");
        self.status = match (cal, crate::daemon::sync_google(brain, be, changed)) {
            (Err(e), _) => trf!("Google Calendar : {e:#}", "Google Calendar: {e:#}"),
            (_, Err(e)) => trf!("Google : {e:#}", "Google: {e:#}"),
            (Ok(_), Ok(m)) if m.is_empty() => tr("Google à jour", "Google up to date").into(),
            (Ok(_), Ok(m)) => m.join(" · "),
        };
        self.ensure_month(brain, true);
    }

    fn remember(&mut self, brain: &Brain, cmd: &str) {
        if cmd.trim().is_empty() {
            return;
        }
        self.cmd_history.retain(|h| h != cmd);
        self.cmd_history.push(cmd.to_string());
        let extra = self.cmd_history.len().saturating_sub(200);
        self.cmd_history.drain(..extra);
        let _ = std::fs::write(brain.home.join("data/dash_history"), self.cmd_history.join("\n"));
    }

    /// Palette entries matching what is typed (prefix first, then by description).
    fn matches(&self, buf: &str) -> Vec<(String, String)> {
        let b = buf.trim_start().to_lowercase();
        let head = b.split_whitespace().next().unwrap_or("");
        if b.contains(' ') && self.commands.iter().any(|(c, _)| c == head || c.starts_with(&format!("{head} "))) {
            // Arguments are being typed: keep the matching command (and its sub-commands) visible.
            return self
                .commands
                .iter()
                .filter(|(c, _)| b.starts_with(&format!("{c} ")) || b.trim_end() == c || c.starts_with(b.trim_end()))
                .cloned()
                .collect();
        }
        let mut v: Vec<(String, String)> = self.commands.iter().filter(|(c, _)| c.starts_with(&b)).cloned().collect();
        if !b.is_empty() {
            v.extend(
                self.commands
                    .iter()
                    .filter(|(c, d)| !c.starts_with(&b) && (c.contains(&b) || d.to_lowercase().contains(&b)))
                    .cloned(),
            );
        }
        v
    }
}

/// Quote-aware split: `task add "review exam notes +inbox"` → [task, add, review exam notes +inbox].
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut quoted = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                quoted = true;
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() || quoted {
                    out.push(std::mem::take(&mut cur));
                }
                quoted = false;
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() || quoted {
        out.push(cur);
    }
    out
}

fn spawn_sb(brain: &Brain, args: Vec<String>, label: String, show: bool) -> Job {
    let (tx, rx) = channel();
    let home = brain.home.clone();
    std::thread::spawn(move || {
        let exe = std::env::current_exe().unwrap_or_else(|_| "sb".into());
        let res = std::process::Command::new(exe)
            .arg("--home")
            .arg(&home)
            .args(&args)
            .env("SB_NO_SPINNER", "1")
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null())
            .output();
        let msg = match res {
            Ok(o) => {
                let mut t = String::from_utf8_lossy(&o.stdout).to_string();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    t.push_str(&format!("\n{err}"));
                }
                (o.status.success(), t)
            }
            Err(e) => (false, trf!("impossible de lancer sb : {e}", "cannot run sb: {e}")),
        };
        let _ = tx.send(msg);
    });
    Job { label, rx, started: Instant::now(), show }
}

/// Leave the TUI, run `f` in the normal terminal, come back.
fn suspended(term: &mut DefaultTerminal, f: impl FnOnce()) -> Result<()> {
    ratatui::restore();
    f();
    *term = ratatui::init();
    term.clear()?;
    Ok(())
}

fn journal_path(brain: &Brain) -> Result<PathBuf> {
    let today = brain.today();
    let p = brain.vault().join(format!("journal/{today}.md"));
    if !p.exists() {
        crate::util::atomic_write(&p, &format!("# {}\n\n", today.format("%A %d %B %Y")))?;
    }
    Ok(p)
}

fn editor() -> String {
    std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "nvim".into())
}

// ---------------------------------------------------------------------------
// Run loop

/// `commands`: every `sb` sub-command with its description (from clap), for the palette.
pub fn run(brain: &Brain, commands: Vec<(String, String, String)>) -> Result<()> {
    let mut app = App::new(brain, commands);
    app.refresh(brain);
    let mut term = ratatui::init();
    let res = (|| -> Result<()> {
        term.draw(|f| draw(f, brain, &mut app))?;
        if app.google.is_some() {
            app.status = tr("synchronisation Google…", "syncing Google…").into();
            term.draw(|f| draw(f, brain, &mut app))?;
            app.sync(brain, false);
            app.refresh(brain);
        }
        main_loop(&mut term, brain, &mut app)
    })();
    ratatui::restore();
    res
}

fn main_loop(term: &mut DefaultTerminal, brain: &Brain, app: &mut App) -> Result<()> {
    let mut last_refresh = Instant::now();
    while !app.quit {
        let done = app.job.as_ref().and_then(|j| j.rx.try_recv().ok());
        if let Some((ok, out)) = done {
            let job = app.job.take().expect("job");
            let secs = job.started.elapsed().as_secs_f32();
            app.status = format!("{} {} ({secs:.1}s)", if ok { "ok" } else { tr("échec", "failed") }, job.label);
            if job.show || !ok {
                let lines: Vec<String> = out.trim_end().lines().map(String::from).collect();
                app.pager = Some(Pager {
                    title: job.label,
                    lines: if lines.is_empty() {
                        vec![tr("(rien à afficher)", "(nothing to show)").into()]
                    } else {
                        lines
                    },
                    scroll: 0,
                });
            }
            app.ensure_month(brain, true);
            app.refresh(brain);
        }
        term.draw(|f| draw(f, brain, app))?;
        let timeout = if app.job.is_some() { StdDuration::from_millis(120) } else { StdDuration::from_secs(5) };
        if !event::poll(timeout)? {
            if last_refresh.elapsed().as_secs() >= 30 {
                app.refresh(brain);
                last_refresh = Instant::now();
            }
            continue;
        }
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            app.quit = true;
        } else if app.help {
            app.help = false;
        } else if app.pager.is_some() {
            pager_key(app, k);
        } else if app.confirm.is_some() {
            confirm_key(term, brain, app, k)?;
        } else if app.picker.is_some() {
            picker_key(app, k);
        } else if app.proj_picker.is_some() {
            proj_picker_key(term, brain, app, k)?;
        } else if app.theme_picker.is_some() {
            theme_key(brain, app, k);
        } else if app.date_picker.is_some() {
            date_picker_key(term, brain, app, k)?;
        } else if app.card.is_some() && matches!(app.mode, Mode::Normal) {
            card_key(term, brain, app, k)?;
        } else if app.undo_list.is_some() {
            undo_key(term, brain, app, k)?;
        } else if !matches!(app.mode, Mode::Normal) {
            line_key(term, brain, app, k)?;
        } else {
            normal_key(term, brain, app, k)?;
        }
    }
    Ok(())
}

fn pager_key(app: &mut App, k: KeyEvent) {
    let Some(p) = &mut app.pager else { return };
    let max = p.lines.len().saturating_sub(1);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => app.pager = None,
        KeyCode::Char('j') | KeyCode::Down => p.scroll = (p.scroll + 1).min(max),
        KeyCode::Char('k') | KeyCode::Up => p.scroll = p.scroll.saturating_sub(1),
        KeyCode::Char('d') if ctrl => p.scroll = (p.scroll + 15).min(max),
        KeyCode::Char('u') if ctrl => p.scroll = p.scroll.saturating_sub(15),
        KeyCode::PageDown | KeyCode::Char(' ') => p.scroll = (p.scroll + 15).min(max),
        KeyCode::PageUp => p.scroll = p.scroll.saturating_sub(15),
        KeyCode::Char('g') | KeyCode::Home => p.scroll = 0,
        KeyCode::Char('G') | KeyCode::End => p.scroll = max,
        _ => {}
    }
}

fn open_undo(brain: &Brain, app: &mut App) {
    match crate::actions::recent(&brain.conn, 200) {
        Ok(rows) => {
            let v: Vec<_> = rows.into_iter().filter(|a| a.undoable && !a.undone).take(40).collect();
            if v.is_empty() {
                app.status = tr("rien à annuler", "nothing to undo").into();
            } else {
                app.history_list.select(Some(0));
                app.undo_list = Some(v);
            }
        }
        Err(e) => app.status = format!("{e:#}"),
    }
}

fn undo_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let n = app.undo_list.as_ref().map_or(0, Vec::len);
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.undo_list = None,
        KeyCode::Char('j') | KeyCode::Down => move_sel(&mut app.history_list, n, 1),
        KeyCode::Char('k') | KeyCode::Up => move_sel(&mut app.history_list, n, -1),
        KeyCode::Enter | KeyCode::Char('u') => {
            let Some(id) = app.history_list.selected().and_then(|i| app.undo_list.as_ref()?.get(i)).map(|a| a.id)
            else {
                return Ok(());
            };
            app.undo_list = None;
            match crate::actions::undo(&brain.conn, id) {
                Ok(s) => {
                    let _ = crate::notes::regenerate_dirty(brain);
                    app.status = trf!("annulé : {s} · synchronisation Google…", "undone: {s} · syncing Google…");
                    term.draw(|f| draw(f, brain, app))?;
                    app.sync(brain, true);
                    app.status = trf!("annulé : {s}", "undone: {s}");
                }
                Err(e) => app.status = format!("{e:#}"),
            }
            app.refresh(brain);
        }
        _ => {}
    }
    Ok(())
}

fn move_sel(st: &mut ListState, n: usize, delta: isize) {
    if n == 0 {
        return;
    }
    let i = (st.selected().unwrap_or(0) as isize).saturating_add(delta);
    st.select(Some(i.clamp(0, n as isize - 1) as usize));
}

fn set_day(brain: &Brain, app: &mut App, d: NaiveDate) {
    app.selected_day = d;
    app.ensure_month(brain, false);
    app.refresh(brain);
    app.day.select(Some(0));
}

/// Inside nvim's terminal ($NVIM is set): ask that nvim to open the file (via the
/// `SbOpen` function of the nvim config) instead of starting a nested editor.
fn open_in_parent_nvim(path: &std::path::Path) -> bool {
    let Ok(server) = std::env::var("NVIM") else { return false };
    let p = path.to_string_lossy().replace('\'', "''");
    std::process::Command::new("nvim")
        .args(["--server", &server, "--remote-expr", &format!("v:lua.SbOpen('{p}')")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn open_in_editor(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, path: PathBuf) -> Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    if open_in_parent_nvim(&path) {
        app.status = trf!("ouvert dans nvim : {}", "opened in nvim: {}", name);
        return Ok(());
    }
    let vault = brain.vault();
    suspended(term, || {
        // Start the editor inside the vault: notes are then "in cwd" for file trees
        // (neo-tree prompts to change cwd otherwise, and its prompt path is buggy: E95).
        let _ = std::process::Command::new(editor()).arg(&path).current_dir(&vault).status();
    })?;
    // Analysing calls the model: never automatic, `:w` (or <leader>si in nvim) when you want.
    app.status = if path.starts_with(brain.vault().join("journal")) {
        tr("journal enregistré · :w pour l'analyser (IA)", "journal saved · :w to analyse it (AI)").into()
    } else {
        trf!("fermé : {}", "closed: {}", name)
    };
    app.refresh(brain);
    Ok(())
}

fn entity_note(brain: &Brain, name: &str) -> Option<PathBuf> {
    let (kind, n): (String, String) = brain
        .conn
        .query_row("SELECT kind, name FROM entities WHERE name=?1 AND status='active'", [name], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .ok()?;
    Some(crate::notes::note_path(brain, &kind, &n))
}

fn normal_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let g = std::mem::take(&mut app.pending_g);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    // Calendar editing keys take precedence when a calendar list has the focus.
    if app.view == View::Board
        && matches!(app.focus, Panel::Calendar | Panel::Day | Panel::Today)
        && matches!(k.code, KeyCode::Char('v' | 'd' | 'b' | 'B' | 'e') | KeyCode::Delete)
    {
        return calendar_edit_key(brain, app, k);
    }
    // Keys that work everywhere.
    match k.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('1') => app.view = View::Board,
        KeyCode::Char('2') => app.view = View::Activity,
        KeyCode::Char('3') => app.view = View::Themes,
        KeyCode::Char('4') => app.view = View::Commands,
        KeyCode::Char('5') => {
            app.ensure_graph(brain);
            app.view = View::Graph;
        }
        KeyCode::Char(':') => {
            app.mode = Mode::Command(String::new());
            app.hist_pos = None;
            app.palette = 0;
        }
        KeyCode::Char('/') => app.mode = Mode::Search(String::new()),
        KeyCode::Char('?') => app.help = true,
        KeyCode::Char('L') => toggle_lang(brain, app),
        KeyCode::Char('T') => open_themes(app),
        // In the graph view these keys move the camera instead.
        KeyCode::Char('a' | 'e' | 'o' | 's' | 'r' | 'u') if app.view == View::Graph => {
            return view_key(term, brain, app, k, g, ctrl);
        }
        KeyCode::Char('u') if !ctrl => open_undo(brain, app),
        KeyCode::Char('a') => app.mode = Mode::AddTask(String::new()),
        KeyCode::Char('e') => app.mode = Mode::AddEvent(String::new()),
        KeyCode::Char('S') => {
            app.status = tr("synchronisation Google…", "syncing Google…").into();
            term.draw(|f| draw(f, brain, app))?;
            app.sync(brain, false);
            app.refresh(brain);
        }
        KeyCode::Char('o') => run_command(term, brain, app, "graph")?,
        KeyCode::Char('s') => match crate::synth_page::write(brain, None) {
            Ok(p) => {
                crate::graph::open(&p);
                app.status = tr("synthèses ouvertes dans le navigateur", "syntheses opened in the browser").into();
            }
            Err(e) => app.status = format!("{e:#}"),
        },
        KeyCode::Char('.') => match app.last_cmd.clone() {
            Some(c) => run_command(term, brain, app, &c)?,
            None => app.status = tr("aucune commande à répéter", "no command to repeat").into(),
        },
        KeyCode::Char('r') if !ctrl => {
            app.ensure_month(brain, true);
            app.refresh(brain);
            app.status = tr("actualisé", "refreshed").into();
        }
        KeyCode::Esc => {
            app.range = None;
            app.status = tr("prêt", "ready").into();
        }
        _ => return view_key(term, brain, app, k, g, ctrl),
    }
    Ok(())
}

fn view_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent, g: bool, ctrl: bool) -> Result<()> {
    if app.view == View::Graph {
        return graph_key(term, brain, app, k);
    }
    let step: isize = match k.code {
        KeyCode::Char('j') | KeyCode::Down => 1,
        KeyCode::Char('k') | KeyCode::Up => -1,
        KeyCode::Char('d') if ctrl => 10,
        KeyCode::Char('u') if ctrl => -10,
        KeyCode::Char('G') => isize::MAX,
        KeyCode::Char('g') if g => isize::MIN,
        KeyCode::Char('g') => {
            app.pending_g = true;
            return Ok(());
        }
        _ => 0,
    };
    match app.view {
        View::Commands => {
            let n = app.commands.len();
            if step != 0 {
                move_sel(&mut app.cmd_list, n, step);
            } else if k.code == KeyCode::Enter
                && let Some((c, _)) = app.cmd_list.selected().and_then(|i| app.commands.get(i)).cloned()
            {
                app.mode = Mode::Command(format!("{c} "));
                app.palette = 0;
            }
        }
        View::Graph => graph_key(term, brain, app, k)?,
        View::Activity => match k.code {
            KeyCode::Tab | KeyCode::Char(']' | 'l') => app.metric = (app.metric + 1) % app.snap.metrics.len().max(1),
            KeyCode::BackTab | KeyCode::Char('[' | 'h') => {
                let m = app.snap.metrics.len().max(1);
                app.metric = (app.metric + m - 1) % m;
            }
            _ => {}
        },
        View::Themes => {
            if step != 0 {
                move_sel(&mut app.tree, app.snap.tree.len(), step);
            } else if k.code == KeyCode::Enter
                && let Some(name) = app.tree.selected().and_then(|i| app.snap.tree.get(i)).map(|r| r.name.clone())
                && let Some(p) = entity_note(brain, &name)
            {
                open_in_editor(term, brain, app, p)?;
            }
        }
        View::Board => {
            if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
                let i = PANELS.iter().position(|p| *p == app.focus).unwrap_or(0);
                let n = PANELS.len();
                app.focus = PANELS[if k.code == KeyCode::BackTab { (i + n - 1) % n } else { (i + 1) % n }];
                return Ok(());
            }
            board_key(term, brain, app, k, step)?;
        }
    }
    Ok(())
}

fn board_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent, step: isize) -> Result<()> {
    match app.focus {
        Panel::Calendar => {
            let d = app.selected_day;
            let nd = match k.code {
                KeyCode::Char('h') | KeyCode::Left => Some(d - Duration::days(1)),
                KeyCode::Char('l') | KeyCode::Right => Some(d + Duration::days(1)),
                KeyCode::Char('j') | KeyCode::Down => Some(d + Duration::days(7)),
                KeyCode::Char('k') | KeyCode::Up => Some(d - Duration::days(7)),
                KeyCode::Char(']' | 'L') => Some(add_months(d, 1)),
                KeyCode::Char('[' | 'H') => Some(add_months(d, -1)),
                KeyCode::Char('t') => Some(brain.today()),
                KeyCode::Enter => {
                    app.focus = Panel::Day;
                    None
                }
                _ => None,
            };
            if let Some(nd) = nd {
                set_day(brain, app, nd);
            }
        }
        Panel::Day => {
            if step != 0 {
                move_sel(&mut app.day, app.snap.day.len(), step);
            } else if k.code == KeyCode::Char('t') {
                set_day(brain, app, brain.today());
            }
        }
        Panel::Today => {
            if step != 0 {
                move_sel(&mut app.today_list, app.snap.today.len(), step);
            }
        }
        Panel::Tasks | Panel::Done => {
            let finished = app.focus == Panel::Done;
            let n = if finished { app.snap.finished.len() } else { app.snap.open.len() };
            if step != 0 {
                move_sel(if finished { &mut app.done } else { &mut app.tasks }, n, step);
                return Ok(());
            }
            match k.code {
                KeyCode::Enter => {
                    if let Some(t) = app.selected_task() {
                        app.card = Some(TaskCard { id: t.id, field: 0 });
                    }
                }
                KeyCode::Char('f') => {
                    app.focus = if finished { Panel::Tasks } else { Panel::Done };
                }
                KeyCode::Char('D') | KeyCode::Delete => {
                    if let Some(t) = app.selected_task() {
                        let it =
                            DayItem { time: String::new(), title: t.title.clone(), kind: 3, r: Ref::DeleteTask(t.id) };
                        app.confirm = Some((describe_delete(std::slice::from_ref(&it)), vec![it]));
                    }
                }
                KeyCode::Char('x' | 'c' | '+' | '-') => {
                    let Some(t) = app.selected_task() else { return Ok(()) };
                    let closed = t.status == "done" || t.status == "cancelled";
                    let r = match k.code {
                        KeyCode::Char('x' | 'c') if closed => tasks::set_status(brain, t.id, "open"),
                        KeyCode::Char('x') => tasks::set_status(brain, t.id, "done"),
                        KeyCode::Char('c') => tasks::set_status(brain, t.id, "cancelled"),
                        KeyCode::Char('+') => {
                            tasks::edit(brain, t.id, None, None, None, Some((t.priority - 1).max(1)), None)
                        }
                        _ => tasks::edit(brain, t.id, None, None, None, Some((t.priority + 1).min(4)), None),
                    };
                    match r {
                        Ok(msg) => {
                            app.status = trf!("{msg} · synchronisation Google…", "{msg} · syncing Google…");
                            term.draw(|f| draw(f, brain, app))?;
                            app.sync(brain, true);
                            app.status = trf!("{msg} · u pour annuler", "{msg} · u to undo");
                        }
                        Err(e) => app.status = format!("{e:#}"),
                    }
                    app.refresh(brain);
                }
                _ => {}
            }
        }
        Panel::Notes => {
            if step != 0 {
                move_sel(&mut app.notes, app.snap.notes.len(), step);
            } else if k.code == KeyCode::Enter
                && let Some(p) = app.notes.selected().and_then(|i| app.snap.notes.get(i)).map(|n| n.path.clone())
            {
                open_in_editor(term, brain, app, p)?;
            }
        }
        Panel::Recent => {
            if step != 0 {
                move_sel(&mut app.recent, app.snap.recent.len(), step);
            } else if k.code == KeyCode::Enter
                && let Some(name) = app.recent.selected().and_then(|i| app.snap.recent.get(i)).map(|r| r.1.clone())
                && let Some(p) = entity_note(brain, &name)
            {
                open_in_editor(term, brain, app, p)?;
            }
        }
        Panel::Activity => {
            if k.code == KeyCode::Enter {
                app.view = View::Activity;
            }
        }
    }
    Ok(())
}

fn line_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let is_cmd = matches!(app.mode, Mode::Command(_));
    let is_add = matches!(app.mode, Mode::AddTask(_));
    let matches = match &app.mode {
        Mode::Command(b) => app.matches(b),
        _ => vec![],
    };
    let buf = match &mut app.mode {
        Mode::Command(b)
        | Mode::Search(b)
        | Mode::AddTask(b)
        | Mode::AddEvent(b)
        | Mode::Block(b, _)
        | Mode::EditField(_, b) => b,
        Mode::Normal => return Ok(()),
    };
    match k.code {
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Backspace if buf.is_empty() => app.mode = Mode::Normal,
        KeyCode::Backspace => {
            buf.pop();
            app.palette = 0;
        }
        KeyCode::Tab if is_add => {
            let text = buf.clone();
            app.mode = Mode::Normal;
            open_proj_picker(brain, app, ProjTarget::NewTask(text));
            return Ok(());
        }
        KeyCode::Char('u') if ctrl => buf.clear(),
        KeyCode::Char('w') if ctrl => {
            let t = buf.trim_end().rfind(' ').map(|i| i + 1).unwrap_or(0);
            buf.truncate(t);
        }
        // Palette: Tab / Shift-Tab cycle through the matches and complete the buffer.
        KeyCode::Tab | KeyCode::BackTab if is_cmd && !matches.is_empty() => {
            let n = matches.len();
            let cur = matches.iter().position(|(c, _)| c.trim_end() == buf.trim_end());
            let next = match (cur, k.code) {
                (None, KeyCode::Tab) => app.palette.min(n - 1),
                (Some(i), KeyCode::Tab) => (i + 1) % n,
                (None, _) => n - 1,
                (Some(i), _) => (i + n - 1) % n,
            };
            app.palette = next;
            *buf = format!("{} ", matches[next].0);
        }
        KeyCode::Char('n' | 'p') if ctrl && is_cmd && !matches.is_empty() => {
            let n = matches.len();
            app.palette = if k.code == KeyCode::Char('n') { (app.palette + 1) % n } else { (app.palette + n - 1) % n };
        }
        KeyCode::Up | KeyCode::Down if is_cmd => {
            let n = app.cmd_history.len();
            if n == 0 {
                return Ok(());
            }
            let pos = match (app.hist_pos, k.code) {
                (None, KeyCode::Up) => Some(n - 1),
                (Some(p), KeyCode::Up) => Some(p.saturating_sub(1)),
                (Some(p), KeyCode::Down) if p + 1 < n => Some(p + 1),
                _ => None,
            };
            app.hist_pos = pos;
            *buf = pos.map(|p| app.cmd_history[p].clone()).unwrap_or_default();
        }
        KeyCode::Char(ch) => {
            buf.push(ch);
            app.palette = 0;
        }
        KeyCode::Enter => {
            let mode = std::mem::replace(&mut app.mode, Mode::Normal);
            match mode {
                Mode::Command(c) => {
                    // A partial word ("sta") runs the highlighted palette entry.
                    let first = c.split_whitespace().next().unwrap_or("").to_string();
                    let known = app.commands.iter().any(|(c, _)| c.split(' ').next() == Some(first.as_str()));
                    let c = if !c.trim().is_empty() && !known && !matches.is_empty() {
                        matches[app.palette.min(matches.len() - 1)].0.clone()
                    } else {
                        c.trim().to_string()
                    };
                    app.remember(brain, &c);
                    run_command(term, brain, app, &c)?;
                }
                Mode::Search(q) if !q.trim().is_empty() && app.view == View::Graph => {
                    let q = crate::gcal_admin::fold(q.trim());
                    let found = app.graph.as_ref().and_then(|g| {
                        g.order.iter().copied().find(|i| crate::gcal_admin::fold(&g.nodes[*i].label).contains(&q))
                    });
                    match found {
                        Some(i) => {
                            app.gview.sel = Some(i);
                            app.gview.anchor = Some(i);
                            app.center_on_selected();
                        }
                        None => app.status = tr("aucun nœud ne correspond", "no matching node").into(),
                    }
                }
                Mode::Search(q) if !q.trim().is_empty() => search(brain, app, q.trim()),
                Mode::AddTask(t) if !t.trim().is_empty() => match add_task_in(brain, &t, &app.add_project) {
                    Ok(_) => {
                        app.status =
                            trf!("tâche ajoutée : {t} · synchronisation Google…", "task added: {t} · syncing Google…");
                        term.draw(|f| draw(f, brain, app))?;
                        app.sync(brain, true);
                        app.status = trf!("tâche ajoutée : {t}", "task added: {t}");
                        app.refresh(brain);
                    }
                    Err(e) => app.status = format!("{e:#}"),
                },
                Mode::EditField(field, value) => {
                    if let Some(id) = app.card.as_ref().map(|c| c.id) {
                        let r = apply_field(brain, id, TASK_FIELDS[field], value.trim());
                        match r {
                            Ok(msg) => {
                                app.status = trf!("{} · synchronisation Google…", "{} · syncing Google…", msg);
                                term.draw(|f| draw(f, brain, app))?;
                                app.sync(brain, true);
                                app.status = trf!("{} · u pour annuler", "{} · u to undo", msg);
                            }
                            Err(e) => app.status = format!("{e:#}"),
                        }
                        app.refresh(brain);
                    }
                }
                Mode::Block(t, weekdays) if !t.trim().is_empty() => {
                    let (a, b) = app.range_bounds();
                    app.range = None;
                    match &app.google {
                        None => app.status = tr("Google non connecté", "Google not connected").into(),
                        Some(be) => match crate::gcal_admin::block(brain, be, t.trim(), a, b, weekdays) {
                            Ok(_) => {
                                app.status =
                                    trf!("« {} » bloqué du {a} au {b}", "“{}” blocked from {a} to {b}", t.trim());
                                app.ensure_month(brain, true);
                            }
                            Err(e) => app.status = format!("{e:#}"),
                        },
                    }
                    app.refresh(brain);
                }
                Mode::AddEvent(t) if !t.trim().is_empty() => {
                    app.last_cmd = Some(format!("event \"{}\"", t.replace('"', "'")));
                    app.job = Some(spawn_sb(
                        brain,
                        vec!["event".into(), t.clone()],
                        trf!("événement : {t}", "event: {t}"),
                        true,
                    ));
                }
                _ => {}
            }
        }
        _ => {}
    }
    Ok(())
}

fn search(brain: &Brain, app: &mut App, q: &str) {
    match crate::retrieval::search(brain, q, 30, None) {
        Ok(hits) => {
            let mut lines: Vec<String> = vec![];
            for h in &hits {
                let kind = match h.kind {
                    crate::db::DocKind::Memory => tr("souvenir", "memory"),
                    crate::db::DocKind::Block => "journal",
                    crate::db::DocKind::Entity => "note",
                    crate::db::DocKind::Task => tr("tâche", "task"),
                };
                lines.push(format!("[{kind}] {}", h.title));
                lines.push(format!("    {}", h.snippet.replace(['\n', '\r'], " ")));
                if let Some(src) = &h.source {
                    lines.push(format!("    ↳ {src}"));
                }
                lines.push(String::new());
            }
            if lines.is_empty() {
                lines.push(tr("aucun résultat", "no results").into());
            }
            app.pager = Some(Pager { title: format!("/{q}"), lines, scroll: 0 });
        }
        Err(e) => app.status = format!("{e:#}"),
    }
}

fn run_command(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, cmd: &str) -> Result<()> {
    let cmd = cmd.trim().trim_start_matches(':').trim();
    let cmd = cmd.strip_prefix("sb ").unwrap_or(cmd);
    if cmd.is_empty() {
        return Ok(());
    }
    if app.job.is_some() {
        app.status = tr("une commande tourne déjà, patiente…", "a command is already running, wait…").into();
        return Ok(());
    }
    let args = split_args(cmd);
    let head = args.first().map(String::as_str).unwrap_or("");
    // A command that needs text/arguments but got none: ask for them instead of failing.
    let name = if head == "task" || head == "calendar" || head == "session" {
        args.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
    } else {
        head.to_string()
    };
    let given = args.len() - name.split(' ').count();
    if given == 0 {
        match name.as_str() {
            "event" => {
                app.mode = Mode::AddEvent(String::new());
                return Ok(());
            }
            "task add" => {
                app.mode = Mode::AddTask(String::new());
                return Ok(());
            }
            _ => {}
        }
        let usage = app
            .help_text
            .get(&name)
            .and_then(|h| h.lines().find(|l| l.starts_with("Usage:")).map(|l| l.trim().to_string()));
        let needs = matches!(name.as_str(), "ask" | "search" | "reprocess" | "unlink" | "undo")
            || usage.as_deref().is_some_and(|u| u.contains('<'));
        if needs && name != "undo" {
            app.mode = Mode::Command(format!("{name} "));
            app.status = trf!("il manque un argument · {}", "missing argument · {}", usage.unwrap_or_default());
            return Ok(());
        }
    }
    match head {
        "q" | "q!" | "quit" | "qa" => app.quit = true,
        "wq" | "x" => {
            let job = spawn_sb(brain, vec!["ingest".into()], "ingest".into(), false);
            app.status = tr("ingest avant de quitter…", "ingest before quitting…").into();
            term.draw(|f| draw(f, brain, app))?;
            let _ = job.rx.recv();
            app.quit = true;
        }
        "e" | "edit" | "journal" => {
            let p = journal_path(brain)?;
            app.last_cmd = Some("e".into());
            open_in_editor(term, brain, app, p)?;
        }
        "w" => {
            app.last_cmd = Some("w".into());
            app.job = Some(spawn_sb(brain, vec!["ingest".into()], "ingest".into(), true));
        }
        "undo" if args.len() == 1 => open_undo(brain, app),
        "dash" => app.status = tr("tu es déjà dans le tableau de bord", "you are already in the dashboard").into(),
        "daemon" => {
            app.status = tr(
                "le daemon tourne en continu : lance `sb daemon` dans un autre terminal",
                "the daemon runs continuously: start `sb daemon` in another terminal",
            )
            .into()
        }
        "help" if args.len() == 1 => app.help = true,
        "lang" | "language" => toggle_lang(brain, app),
        "theme" | "themes" if args.len() == 1 => open_themes(app),
        "theme" => match crate::theme::by_name(&args[1]) {
            Some(i) => apply_theme(brain, app, i),
            None => app.status = trf!("thème inconnu : {}", "unknown theme: {}", args[1]),
        },
        _ => {
            app.last_cmd = Some(cmd.to_string());
            // Interactive commands (browser login, prompts) run in the real terminal.
            let interactive = matches!(args.as_slice(), [a, b, ..] if a == "calendar" && b == "auth")
                || head == "init"
                || (head == "reset" && !args.iter().any(|a| a == "--yes"));
            if interactive {
                let exe = std::env::current_exe().unwrap_or_else(|_| "sb".into());
                let home = brain.home.clone();
                suspended(term, || {
                    let _ = std::process::Command::new(exe).arg("--home").arg(home).args(&args).status();
                    println!(
                        "{}",
                        tr("\n[Entrée pour revenir au tableau de bord]", "\n[Enter to go back to the dashboard]")
                    );
                    let _ = std::io::stdin().read_line(&mut String::new());
                })?;
                app.google = Backend::google(brain).ok().flatten();
                app.refresh(brain);
            } else {
                app.job = Some(spawn_sb(brain, args, cmd.to_string(), true));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Drawing

fn panel(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT() } else { FAINT() }))
        .style(Style::new().bg(BG()).fg(TEXT()))
        .title(Span::styled(
            format!(" {title} "),
            if focused { Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD) } else { Style::new().fg(MUTED()) },
        ))
}

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn draw(f: &mut Frame, brain: &Brain, app: &mut App) {
    // Paint the theme background (popups clear to it as well, through `panel`).
    f.render_widget(Block::new().style(Style::new().bg(BG()).fg(TEXT())), f.area());
    let [head, body, foot] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(10), Constraint::Length(1)]).areas(f.area());
    draw_header(f, brain, app, head);
    match app.view {
        View::Board => draw_board(f, brain, app, body),
        View::Activity => draw_activity(f, brain, app, body),
        View::Themes => draw_themes(f, brain, app, body),
        View::Commands => draw_commands(f, app, body),
        View::Graph => draw_graph(f, brain, app, body),
    }
    draw_footer(f, app, foot);
    draw_prompt(f, app);
    if let Some(p) = &app.pager {
        draw_pager(f, p);
    }
    if app.undo_list.is_some() {
        draw_undo(f, brain, app);
    }
    if app.help {
        draw_help(f);
    }
    if app.card.is_some() {
        draw_card(f, brain, app);
    }
    if app.proj_picker.is_some() {
        draw_proj_picker(f, app);
    }
    if app.theme_picker.is_some() {
        draw_themes_picker(f, app);
    }
    if app.date_picker.is_some() {
        draw_date_picker(f, brain, app);
    }
    if matches!(app.mode, Mode::AddTask(_) | Mode::AddEvent(_) | Mode::Block(..) | Mode::EditField(..)) {
        draw_input(f, app);
    }
    if app.picker.is_some() {
        draw_picker(f, app);
    }
    if let Some((q, _)) = &app.confirm {
        draw_confirm(f, q);
    }
}

fn draw_header(f: &mut Frame, brain: &Brain, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(" Second Brain    ", Style::new().fg(TEXT()).add_modifier(Modifier::BOLD))];
    for (i, (v, name)) in [
        (View::Board, tr("tableau de bord", "dashboard")),
        (View::Activity, tr("activité", "activity")),
        (View::Themes, tr("thèmes", "themes")),
        (View::Commands, tr("commandes", "commands")),
        (View::Graph, tr("graphe", "graph")),
    ]
    .iter()
    .enumerate()
    {
        let on = app.view == *v;
        spans.push(Span::styled(format!("{} ", i + 1), Style::new().fg(FAINT())));
        spans.push(Span::styled(
            format!("{name}    "),
            if on { Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD) } else { Style::new().fg(MUTED()) },
        ));
    }
    let right = fr_date(brain.today());
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(used + right.chars().count() + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::new().fg(MUTED())));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let line = {
        let (mode, color) = match (&app.job, &app.mode) {
            (Some(_), _) => (" RUN ", WARN()),
            (None, Mode::Command(_)) => (tr(" COMMANDE ", " COMMAND "), OK()),
            (None, Mode::Search(_)) => (tr(" RECHERCHE ", " SEARCH "), OK()),
            _ => (" NORMAL ", ACCENT()),
        };
        let mut spans = vec![Span::styled(mode, Style::new().fg(BG()).bg(color).add_modifier(Modifier::BOLD))];
        match &app.job {
            Some(j) => {
                let i = (j.started.elapsed().as_millis() / 100) as usize % SPIN.len();
                spans.push(Span::styled(
                    format!(" {} {} · {:.0}s", SPIN[i], j.label, j.started.elapsed().as_secs_f32()),
                    Style::new().fg(WARN()),
                ));
            }
            None => spans.push(Span::styled(format!(" {}", app.status), Style::new().fg(TEXT()))),
        }
        let keys = match (app.view, app.focus) {
            (View::Board, Panel::Done) => tr(
                "   x rouvrir la tâche · f tâches à faire · Tab panneau · u historique · ? aide",
                "   x reopen task · f open tasks · Tab panel · u history · ? help",
            ),
            (View::Board, Panel::Tasks) => tr(
                "   Entrée fiche · x fait · c annuler · D supprimer · +/- prio · a ajouter · f terminées · ? aide",
                "   Enter card · x done · c cancel · D delete · +/- prio · a add · f finished · ? help",
            ),
            (View::Board, Panel::Calendar) if app.range.is_some() => tr(
                "   hjkl étendre · e événement sur la période · b bloquer (B lun-ven) · d supprimer · Échap annuler",
                "   hjkl extend · e event over range · b block (B mon-fri) · d delete · Esc cancel",
            ),
            (View::Board, Panel::Calendar) => tr(
                "   hjkl jour · [ ] mois · v sélectionner une période · e ajouter · d supprimer · t aujourd'hui",
                "   hjkl day · [ ] month · v select a range · e add · d delete · t today",
            ),
            (View::Board, Panel::Day | Panel::Today) => tr(
                "   j/k choisir · d supprimer l'événement · e ajouter · Tab panneau",
                "   j/k select · d delete event · e add · Tab panel",
            ),
            (View::Board, Panel::Notes | Panel::Recent) => tr(
                "   j/k choisir · Entrée ouvrir dans l'éditeur · Tab panneau",
                "   j/k select · Enter open in editor · Tab panel",
            ),
            (View::Board, _) => tr(
                "   Tab panneau · : commande · / chercher · u historique · ? aide",
                "   Tab panel · : command · / search · u history · ? help",
            ),
            (View::Activity, _) => tr(
                "   Tab ou [ ] changer de mesure · : commande · ? aide",
                "   Tab or [ ] change metric · : command · ? help",
            ),
            (View::Themes, _) => {
                tr("   j/k parcourir · Entrée ouvrir la note · ? aide", "   j/k browse · Enter open note · ? help")
            }
            (View::Commands, _) => tr(
                "   j/k choisir · Entrée préparer la commande · : taper directement · ? aide",
                "   j/k select · Enter prepare command · : type directly · ? help",
            ),
            (View::Graph, _) => tr(
                "   hjkl déplacer · +/- zoom · Tab nœud · n/N voisins · i entrer · b retour · / trouver · t titres · Entrée ouvrir",
                "   hjkl pan · +/- zoom · Tab node · n/N neighbours · i dive · b back · / find · t labels · Enter open",
            ),
        };
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let google = if app.google.is_some() { "google ok " } else { "google - " };
        let room = (area.width as usize).saturating_sub(used + google.len());
        let k: String = keys.chars().take(room).collect();
        let pad = room.saturating_sub(k.chars().count());
        spans.push(Span::styled(k, Style::new().fg(MUTED())));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(google, Style::new().fg(if app.google.is_some() { OK() } else { FAINT() })));
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(line), area);
}

/// Centered command / search box: the input line on top, matching commands below.
fn draw_prompt(f: &mut Frame, app: &App) {
    let (prefix, buf, is_cmd) = match &app.mode {
        Mode::Command(b) => (":", b.as_str(), true),
        Mode::Search(b) => ("/", b.as_str(), false),
        _ => return,
    };
    let m = if is_cmd { app.matches(buf) } else { vec![] };
    let rows = m.len().min(8);
    let w = 64u16.min(f.area().width.saturating_sub(4));
    let h = 3 + if rows > 0 { rows as u16 + 1 } else { 0 };
    let area = centered(f.area(), w, h + 1);
    let area = Rect { height: h, ..area };
    f.render_widget(Clear, area);
    let title = if is_cmd {
        tr("commande · Tab compléter · ↑↓ historique", "command · Tab complete · ↑↓ history")
    } else {
        tr("rechercher · Entrée", "search · Enter")
    };
    let block = panel(title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [input_a, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {prefix} "), Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD)),
            Span::styled(buf.to_string(), Style::new().fg(TEXT())),
            Span::styled("▏", Style::new().fg(ACCENT())),
        ])),
        input_a,
    );
    if rows == 0 {
        return;
    }
    let [sep_a, list_a] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(rest);
    f.render_widget(Paragraph::new(Span::styled("─".repeat(inner.width as usize), Style::new().fg(FAINT()))), sep_a);
    let sel = app.palette.min(m.len() - 1);
    let start = sel.saturating_sub(rows - 1);
    let items: Vec<ListItem> = m
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, (c, d))| {
            let st = if i == sel { Style::new().bg(SEL_BG()) } else { Style::new() };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {c:<18}"), st.fg(TEXT()).add_modifier(Modifier::BOLD)),
                Span::styled(crate::util::truncate_chars(d, (w as usize).saturating_sub(24)), st.fg(MUTED())),
            ]))
        })
        .collect();
    f.render_widget(List::new(items), list_a);
}

fn draw_board(f: &mut Frame, brain: &Brain, app: &mut App, body: Rect) {
    let [labels, cols] = Layout::vertical([Constraint::Length(1), Constraint::Min(5)]).areas(body);
    let [c1, c2, c3] =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(36), Constraint::Percentage(32)])
            .areas(cols);
    // Column labels: past · today · future.
    for (a, txt) in [(c1, tr("avant", "past")), (c2, tr("aujourd'hui", "today")), (c3, tr("à venir", "upcoming"))] {
        let pad = (a.width as usize).saturating_sub(txt.chars().count()) / 2;
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(txt, Style::new().fg(MUTED()).add_modifier(Modifier::BOLD)),
            ])),
            Rect { y: labels.y, height: 1, ..a },
        );
    }
    let [act_a, done_a, notes_a, recent_a] =
        Layout::vertical([Constraint::Length(13), Constraint::Length(7), Constraint::Min(5), Constraint::Min(5)])
            .areas(c1);
    // The day's programme takes the room it needs (up to 45%), tasks get the rest.
    let today_h = (app.snap.today.len() as u16 + 2).clamp(4, c2.height * 45 / 100);
    let [today_a, tasks_a, mem_a] =
        Layout::vertical([Constraint::Length(today_h), Constraint::Min(6), Constraint::Length(6)]).areas(c2);
    let [cal_a, day_a, up_a] =
        Layout::vertical([Constraint::Length(10), Constraint::Min(5), Constraint::Length(9)]).areas(c3);
    draw_mini_activity(f, brain, app, act_a);
    draw_tasks(f, brain, app, done_a, true);
    draw_notes(f, app, notes_a);
    draw_recent(f, app, recent_a);
    draw_today(f, brain, app, today_a);
    draw_tasks(f, brain, app, tasks_a, false);
    draw_memory(f, app, mem_a);
    draw_calendar(f, brain, app, cal_a);
    draw_day(f, brain, app, day_a);
    draw_upcoming(f, brain, app, up_a);
}

fn day_list(items: &[DayItem]) -> Vec<ListItem<'static>> {
    items
        .iter()
        .map(|a| {
            let color = match a.kind {
                1 => SESSION(),
                2 => AREA(),
                3 => DUE(),
                _ => TEXT(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<12}", a.time), Style::new().fg(MUTED())),
                Span::styled(a.title.clone(), Style::new().fg(color)),
            ]))
        })
        .collect()
}

fn draw_today(f: &mut Frame, brain: &Brain, app: &mut App, area: Rect) {
    let focused = app.focus == Panel::Today;
    let title = trf!("programme · {}", "schedule · {}", fr_date(brain.today()));
    if app.snap.today.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                tr(
                    "rien de prévu aujourd'hui · e pour ajouter un événement",
                    "nothing planned today · e to add an event",
                ),
                Style::new().fg(MUTED()),
            ))
            .block(panel(&title, focused))
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let mut list = List::new(day_list(&app.snap.today)).block(panel(&title, focused));
    if focused {
        list = list.highlight_style(Style::new().bg(SEL_BG()));
    }
    f.render_stateful_widget(list, area, &mut app.today_list);
}

fn draw_upcoming(f: &mut Frame, brain: &Brain, app: &App, area: Rect) {
    let today = brain.today();
    let lines: Vec<Line> = if app.snap.upcoming.is_empty() {
        vec![Line::from(Span::styled(
            tr("aucune échéance dans les 30 jours", "no deadline in the next 30 days"),
            Style::new().fg(MUTED()),
        ))]
    } else {
        app.snap
            .upcoming
            .iter()
            .take(area.height.saturating_sub(2) as usize)
            .map(|t| {
                let d = t.due.unwrap_or(today);
                let n = (d - today).num_days();
                let when = if n == 1 { tr("demain", "tomorrow").to_string() } else { trf!("dans {n} j", "in {n} d") };
                Line::from(vec![
                    Span::styled(format!("{:<10}", when), Style::new().fg(if n <= 3 { DUE() } else { WARN() })),
                    Span::styled(format!("{:<7}", d.format("%d/%m")), Style::new().fg(FAINT())),
                    Span::styled(t.title.clone(), Style::new().fg(TEXT())),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).block(panel(tr("échéances", "deadlines"), false)), area);
}

fn draw_calendar(f: &mut Frame, brain: &Brain, app: &App, area: Rect) {
    let today = brain.today();
    let sel = app.selected_day;
    let first = month_start(sel);
    let mut title = format!("{} {}", month_name(first.month0()), first.year());
    if let Some(r) = app.range {
        let (a, b) = (r.min(sel), r.max(sel));
        title.push_str(&format!(" · {} → {} ({} j)", a.format("%d/%m"), b.format("%d/%m"), (b - a).num_days() + 1));
    }
    let block = panel(&title, app.focus == Panel::Calendar);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Center the 35-column grid in the panel.
    let pad = inner.width.saturating_sub(35) / 2;
    let inner = Rect { x: inner.x + pad, width: inner.width - pad, ..inner };
    let mut lines = vec![Line::from(Span::styled(
        tr(" lu   ma   me   je   ve   sa   di", " mo   tu   we   th   fr   sa   su"),
        Style::new().fg(MUTED()),
    ))];
    let offset = first.weekday().num_days_from_monday() as i64;
    let mut d = first - Duration::days(offset);
    for _ in 0..6 {
        let mut spans = vec![];
        for _ in 0..7 {
            let in_month = d.month() == first.month();
            let (ev, due, ses) = app.snap.month.get(&d).copied().unwrap_or_default();
            let weekend = d.weekday().num_days_from_monday() >= 5;
            let fg = match () {
                _ if !in_month => FAINT(),
                _ if due > 0 => DUE(),
                _ if weekend => MUTED(),
                _ => TEXT(),
            };
            let mut st = Style::new().fg(fg);
            let in_range = app.range.is_some_and(|r| (r.min(sel)..=r.max(sel)).contains(&d));
            if in_range {
                st = st.bg(Color::Rgb(70, 62, 96));
            }
            if d == today {
                st = Style::new().fg(BG()).bg(ACCENT()).add_modifier(Modifier::BOLD);
            } else if d == sel {
                st = st.bg(SEL_BG()).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            let mark = match () {
                _ if !in_month => " ",
                _ if due > 0 => "!",
                _ if ses > 0 => "+",
                _ if ev > 0 => "•",
                _ => " ",
            };
            spans.push(Span::styled(format!("{:>2}", d.day()), st));
            spans.push(Span::styled(format!("{mark}  "), Style::new().fg(if due > 0 { DUE() } else { SESSION() })));
            d += Duration::days(1);
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        tr(" • événement  + focus  ! échéance", " • event  + focus  ! deadline"),
        Style::new().fg(FAINT()),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_day(f: &mut Frame, brain: &Brain, app: &mut App, area: Rect) {
    let d = app.selected_day;
    let rel = match (d - brain.today()).num_days() {
        0 => tr(" · aujourd'hui", " · today"),
        1 => tr(" · demain", " · tomorrow"),
        -1 => tr(" · hier", " · yesterday"),
        _ => "",
    };
    let title = format!("{}{rel}", fr_date(d));
    let focused = app.focus == Panel::Day;
    if app.snap.day.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                tr("rien de prévu · e pour ajouter un événement", "nothing planned · e to add an event"),
                Style::new().fg(MUTED()),
            ))
            .block(panel(&title, focused)),
            area,
        );
        return;
    }
    let items = day_list(&app.snap.day);
    let mut list = List::new(items).block(panel(&title, focused));
    if focused {
        list = list.highlight_style(Style::new().bg(SEL_BG()));
    }
    f.render_stateful_widget(list, area, &mut app.day);
}

fn draw_tasks(f: &mut Frame, brain: &Brain, app: &mut App, area: Rect, finished: bool) {
    let today = brain.today();
    let focused = app.focus == if finished { Panel::Done } else { Panel::Tasks };
    let title = if finished {
        trf!("terminées ({}) · x rouvrir", "finished ({}) · x reopen", app.snap.finished.len())
    } else {
        trf!("tâches ({}) · a ajouter", "tasks ({}) · a add", app.snap.open.len())
    };
    let rows: &[TaskRow] = if finished { &app.snap.finished } else { &app.snap.open };
    if rows.is_empty() {
        let msg = if finished {
            tr("aucune tâche terminée", "no finished task")
        } else {
            tr("aucune tâche ouverte · a pour en ajouter", "no open task · a to add one")
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::new().fg(MUTED()))).block(panel(&title, focused)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = rows
        .iter()
        .map(|t| {
            let mut spans = vec![];
            if finished {
                let (mark, col) = if t.status == "done" { ("✓ ", OK()) } else { ("✗ ", MUTED()) };
                spans.push(Span::styled(mark, Style::new().fg(col)));
                spans.push(Span::styled(t.title.clone(), Style::new().fg(MUTED()).add_modifier(Modifier::CROSSED_OUT)));
                if let Some(c) = t.completed_at.as_deref().and_then(|c| crate::util::parse_ts(c).ok()) {
                    spans.push(Span::styled(
                        format!("  {}", brain.local(c).format("%d/%m %H:%M")),
                        Style::new().fg(FAINT()),
                    ));
                }
            } else {
                let pri = match t.priority {
                    1 => DUE(),
                    2 => WARN(),
                    3 => TEXT(),
                    _ => MUTED(),
                };
                spans.push(Span::styled(format!("p{} ", t.priority), Style::new().fg(pri)));
                spans.push(Span::styled(t.title.clone(), Style::new().fg(TEXT())));
                if let Some(d) = t.due {
                    let days = (d - today).num_days();
                    let txt = match days {
                        i64::MIN..=-1 => trf!("  en retard ({})", "  overdue ({})", d.format("%d/%m")),
                        0 => tr("  aujourd'hui", "  today").into(),
                        1 => tr("  demain", "  tomorrow").into(),
                        n => trf!("  dans {n} j", "  in {n} d"),
                    };
                    spans.push(Span::styled(txt, Style::new().fg(if days <= 1 { DUE() } else { WARN() })));
                }
                if let Some(e) = t.estimate_min {
                    spans.push(Span::styled(format!("  ~{}", tasks::fmt_min(e)), Style::new().fg(MUTED())));
                }
            }
            if let Some(p) = &t.project {
                spans.push(Span::styled(format!("  {p}"), Style::new().fg(FAINT())));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut list = List::new(items).block(panel(&title, focused)).highlight_symbol("› ");
    if focused {
        list = list.highlight_style(Style::new().bg(SEL_BG()).add_modifier(Modifier::BOLD));
    }
    f.render_stateful_widget(list, area, if finished { &mut app.done } else { &mut app.tasks });
}

fn draw_notes(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Panel::Notes;
    let w = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .snap
        .notes
        .iter()
        .map(|n| {
            let folder = n.rel.split('/').next().unwrap_or("");
            let (glyph, col) = match folder {
                "journal" => ("▤", WARN()),
                "projects" => ("◈", WARN()),
                "summaries" => ("≡", OK()),
                "knowledge" => ("●", CONCEPT()),
                _ => ("·", MUTED()),
            };
            let when = ago(n.modified);
            let name = crate::util::truncate_chars(&n.name, w.saturating_sub(when.chars().count() + 6));
            let pad = w.saturating_sub(name.chars().count() + when.chars().count() + 3);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::new().fg(col)),
                Span::styled(name, Style::new().fg(TEXT())),
                Span::raw(" ".repeat(pad)),
                Span::styled(when, Style::new().fg(FAINT())),
            ]))
        })
        .collect();
    let mut list =
        List::new(items).block(panel(tr("notes récentes · Entrée ouvrir", "recent notes · Enter open"), focused));
    if focused {
        list = list.highlight_style(Style::new().bg(SEL_BG()));
    }
    f.render_stateful_widget(list, area, &mut app.notes);
}

fn draw_mini_activity(f: &mut Frame, brain: &Brain, app: &App, area: Rect) {
    let today = brain.today();
    let block = panel(tr("activité · Entrée détails", "activity · Enter details"), app.focus == Panel::Activity);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(m) = app.snap.metrics.first() else { return };
    let weeks = (inner.width.saturating_sub(4) / 2).clamp(4, 53) as i64;
    let [map_a, l1, l2, l3] =
        Layout::vertical([Constraint::Length(8), Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
            .areas(inner);
    heatmap(f, map_a, &m.days, today, weeks);
    let (cur, best) = streaks(&m.days, today);
    let get = |name: &str, days: i64| -> u64 {
        app.snap
            .metrics
            .iter()
            .find(|x| x.name == name)
            .map(|x| (0..days).map(|i| x.days.get(&(today - Duration::days(i))).copied().unwrap_or(0)).sum())
            .unwrap_or(0)
    };
    let stat = |label: &str, v: u64| {
        vec![
            Span::styled(format!("{v}"), Style::new().fg(TEXT()).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {label}   "), Style::new().fg(MUTED())),
        ]
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(tr("série ", "streak "), Style::new().fg(MUTED())),
            Span::styled(trf!("{cur} j", "{cur} d"), Style::new().fg(OK()).add_modifier(Modifier::BOLD)),
            Span::styled(trf!("   record {best} j", "   best {best} d"), Style::new().fg(MUTED())),
        ])),
        l1,
    );
    let mut t = vec![Span::styled(tr("aujourd'hui  ", "today        "), Style::new().fg(FAINT()))];
    t.extend(stat(tr("mots", "words"), get(tr("Mots écrits", "Words written"), 1)));
    t.extend(stat(tr("idées", "ideas"), get(tr("Idées & savoirs", "Ideas & knowledge"), 1)));
    t.extend(stat(tr("liens", "links"), get(tr("Connexions", "Connections"), 1)));
    f.render_widget(Paragraph::new(Line::from(t)), l2);
    let mut w = vec![Span::styled(tr("7 jours      ", "7 days       "), Style::new().fg(FAINT()))];
    w.extend(stat(tr("mots", "words"), get(tr("Mots écrits", "Words written"), 7)));
    w.extend(stat(tr("idées", "ideas"), get(tr("Idées & savoirs", "Ideas & knowledge"), 7)));
    w.extend(stat(tr("faites", "done"), get(tr("Tâches faites", "Tasks done"), 7)));
    f.render_widget(Paragraph::new(Line::from(w)), l3);
}

fn draw_recent(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Panel::Recent;
    let room = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .snap
        .recent
        .iter()
        .map(|(kind, item, text)| {
            let (icon, color) = kind_icon(kind);
            let item = crate::util::truncate_chars(item, room / 2);
            let rest = room.saturating_sub(item.chars().count() + 3);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{icon} "), Style::new().fg(color)),
                Span::styled(format!("{item} "), Style::new().fg(TEXT())),
                Span::styled(
                    crate::util::truncate_chars(&text.replace(['\n', '\r'], " "), rest),
                    Style::new().fg(MUTED()),
                ),
            ]))
        })
        .collect();
    let mut list =
        List::new(items).block(panel(tr("récemment appris · Entrée ouvrir", "recently learned · Enter open"), focused));
    if focused {
        list = list.highlight_style(Style::new().bg(SEL_BG()));
    }
    f.render_stateful_widget(list, area, &mut app.recent);
}

fn draw_memory(f: &mut Frame, app: &App, area: Rect) {
    let mem = match &app.snap.memory {
        Some((d, item, text)) => vec![
            Line::from(vec![
                Span::styled(item.clone(), Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {d}"), Style::new().fg(FAINT())),
            ]),
            Line::from(Span::styled(text.replace(['\n', '\r'], " "), Style::new().fg(TEXT()))),
        ],
        None => vec![Line::from(Span::styled(
            tr(
                "écris dans ton journal (:e) pour nourrir tes souvenirs",
                "write in your journal (:e) to feed your memories",
            ),
            Style::new().fg(MUTED()),
        ))],
    };
    f.render_widget(
        Paragraph::new(mem).block(panel(tr("souvenir du jour", "memory of the day"), false)).wrap(Wrap { trim: true }),
        area,
    );
}

fn level(v: u64, max: u64) -> usize {
    if v == 0 || max == 0 {
        return 0;
    }
    ((v as f64 / max as f64 * 4.0).ceil() as usize).clamp(1, 4)
}

/// (current streak, longest streak) of consecutive active days; today may still be empty.
fn streaks(days: &BTreeMap<NaiveDate, u64>, today: NaiveDate) -> (i64, i64) {
    let active = |d: NaiveDate| days.get(&d).is_some_and(|v| *v > 0);
    let mut d = if active(today) { today } else { today - Duration::days(1) };
    let mut cur = 0;
    while active(d) {
        cur += 1;
        d -= Duration::days(1);
    }
    let (mut best, mut run, mut prev): (i64, i64, Option<NaiveDate>) = (0, 0, None);
    for (day, v) in days {
        if *v == 0 {
            continue;
        }
        run = if prev.is_some_and(|p| *day - p == Duration::days(1)) { run + 1 } else { 1 };
        best = best.max(run);
        prev = Some(*day);
    }
    (cur, best.max(cur))
}

/// GitHub-style map: month labels, then 7 rows (Mon..Sun) × `weeks` columns ending this week.
fn heatmap(f: &mut Frame, area: Rect, days: &BTreeMap<NaiveDate, u64>, today: NaiveDate, weeks: i64) {
    let this_monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let first = this_monday - Duration::weeks(weeks - 1);
    let max = days.iter().filter(|(d, _)| **d >= first).map(|(_, v)| *v).max().unwrap_or(0);
    let used = 4 + weeks as u16 * 2;
    let pad = " ".repeat((area.width.saturating_sub(used) / 2) as usize);
    let mut month_row = format!("{pad}    ");
    let mut last_month = 0;
    for w in 0..weeks {
        let d = first + Duration::weeks(w);
        let col = pad.len() + (4 + w * 2) as usize;
        if d.month() != last_month && month_row.chars().count() <= col {
            last_month = d.month();
            while month_row.chars().count() < col {
                month_row.push(' ');
            }
            month_row.push_str(month_short(d.month0()));
        }
    }
    let mut lines = vec![Line::from(Span::styled(month_row, Style::new().fg(MUTED())))];
    for wd in 0..7 {
        let lbl = match wd {
            0 => tr("lun ", "mon "),
            2 => tr("mer ", "wed "),
            4 => tr("ven ", "fri "),
            6 => tr("dim ", "sun "),
            _ => "    ",
        };
        let mut spans = vec![Span::raw(pad.clone()), Span::styled(lbl, Style::new().fg(MUTED()))];
        for w in 0..weeks {
            let d = first + Duration::weeks(w) + Duration::days(wd);
            if d > today {
                spans.push(Span::raw("  "));
                continue;
            }
            let v = days.get(&d).copied().unwrap_or(0);
            spans.push(Span::styled(if d == today { "▣ " } else { "■ " }, Style::new().fg(HEAT()[level(v, max)])));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_activity(f: &mut Frame, brain: &Brain, app: &mut App, body: Rect) {
    let s = &app.snap;
    let today = brain.today();
    let Some(m) = s.metrics.get(app.metric) else { return };
    let [map_a, mid_a, low_a, jal_a] =
        Layout::vertical([Constraint::Length(13), Constraint::Length(9), Constraint::Length(8), Constraint::Min(4)])
            .areas(body);

    let mut tabs = vec![];
    for (i, x) in s.metrics.iter().enumerate() {
        tabs.push(Span::styled(
            format!(" {} ", x.name),
            if i == app.metric {
                Style::new().fg(BG()).bg(OK()).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(MUTED())
            },
        ));
        tabs.push(Span::raw(" "));
    }
    let block = panel(tr("carte d'activité · Tab changer de mesure", "activity map · Tab change metric"), true);
    let inner = block.inner(map_a);
    f.render_widget(block, map_a);
    let [sel_a, _, heat_a, legend_a] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(8), Constraint::Length(1)])
            .areas(inner);
    f.render_widget(Paragraph::new(Line::from(tabs)), sel_a);
    let weeks = (inner.width.saturating_sub(6) / 2).clamp(4, 53) as i64;
    heatmap(f, heat_a, &m.days, today, weeks);
    let first = today - Duration::weeks(weeks);
    let total: u64 = m.days.iter().filter(|(d, _)| **d > first).map(|(_, v)| v).sum();
    let mut legend = vec![Span::styled(
        trf!("    {total} {} sur {weeks} semaines      moins ", "    {total} {} over {weeks} weeks      less ", m.unit),
        Style::new().fg(MUTED()),
    )];
    for c in HEAT() {
        legend.push(Span::styled("■ ", Style::new().fg(c)));
    }
    legend.push(Span::styled(tr("plus", "more"), Style::new().fg(MUTED())));
    f.render_widget(Paragraph::new(Line::from(legend)), legend_a);

    let [cards_a, week_a] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(mid_a);
    let (cur, best) = streaks(&m.days, today);
    let active = m.days.values().filter(|v| **v > 0).count();
    let best_day = m.days.iter().max_by_key(|(_, v)| **v).filter(|(_, v)| **v > 0);
    let week_sum = |k: i64| -> u64 {
        (0..7).map(|i| m.days.get(&(today - Duration::days(i + k * 7))).copied().unwrap_or(0)).sum()
    };
    let (this_w, last_w) = (week_sum(0), week_sum(1));
    let trend = match (this_w, last_w) {
        (a, 0) if a > 0 => Span::styled(tr("  nouveau", "  new"), Style::new().fg(OK())),
        (_, 0) => Span::raw(""),
        (a, b) if a >= b => Span::styled(
            trf!("  ↑ {}% vs semaine passée", "  ↑ {}% vs last week", (a - b) * 100 / b),
            Style::new().fg(OK()),
        ),
        (a, b) => Span::styled(
            trf!("  ↓ {}% vs semaine passée", "  ↓ {}% vs last week", (b - a) * 100 / b),
            Style::new().fg(DUE()),
        ),
    };
    let card = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:<24}"), Style::new().fg(MUTED())),
            Span::styled(value, Style::new().fg(TEXT()).add_modifier(Modifier::BOLD)),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{:<24}", tr("7 derniers jours", "last 7 days")), Style::new().fg(MUTED())),
            Span::styled(format!("{this_w} {}", m.unit), Style::new().fg(TEXT()).add_modifier(Modifier::BOLD)),
            trend,
        ]),
        card(tr("série en cours", "current streak"), trf!("{cur} jour(s)", "{cur} day(s)")),
        card(tr("plus longue série", "longest streak"), trf!("{best} jour(s)", "{best} day(s)")),
        card(tr("jours actifs", "active days"), active.to_string()),
    ];
    if let Some((d, v)) = best_day {
        lines.push(card(tr("meilleur jour", "best day"), format!("{} · {v} {}", fr_date(*d), m.unit)));
    }
    if active > 0 {
        let all: u64 = m.days.values().sum();
        lines.push(card(
            tr("moyenne / jour actif", "average / active day"),
            format!("{:.1} {}", all as f64 / active as f64, m.unit),
        ));
    }
    f.render_widget(Paragraph::new(lines).block(panel(m.name, false)), cards_a);

    let mut by_wd = [0u64; 7];
    for (d, v) in &m.days {
        by_wd[d.weekday().num_days_from_monday() as usize] += v;
    }
    let bars: Vec<Bar> = [
        tr("lun", "mon"),
        tr("mar", "tue"),
        tr("mer", "wed"),
        tr("jeu", "thu"),
        tr("ven", "fri"),
        tr("sam", "sat"),
        tr("dim", "sun"),
    ]
    .iter()
    .enumerate()
    .map(|(i, l)| {
        Bar::default()
            .value(by_wd[i])
            .label(Line::from(*l))
            .style(Style::new().fg(HEAT()[3]))
            .value_style(Style::new().fg(BG()).bg(HEAT()[3]))
    })
    .collect();
    let bw = (week_a.width.saturating_sub(2) / 7).saturating_sub(1).clamp(3, 8);
    f.render_widget(
        BarChart::default()
            .block(panel(tr("par jour de la semaine", "by day of the week"), false))
            .data(BarGroup::default().bars(&bars))
            .bar_width(bw)
            .bar_gap(1),
        week_a,
    );

    let [sp1, sp2] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(low_a);
    let last30 = |days: &BTreeMap<NaiveDate, u64>| -> Vec<u64> {
        (0..30).rev().map(|i| days.get(&(today - Duration::days(i))).copied().unwrap_or(0)).collect()
    };
    let data = last30(&m.days);
    f.render_widget(
        Sparkline::default()
            .block(panel(&trf!("{} · 30 jours", "{} · 30 days", m.name), false))
            .data(&data)
            .style(Style::new().fg(OK())),
        sp1,
    );
    let calls =
        s.metrics.iter().find(|x| x.name == tr("Appels IA", "AI calls")).map(|x| last30(&x.days)).unwrap_or_default();
    f.render_widget(
        Sparkline::default()
            .block(panel(
                &trf!("appels IA · 30 jours · aujourd'hui {:.3} $", "AI calls · 30 days · today ${:.3}", s.cost_today),
                false,
            ))
            .data(&calls)
            .style(Style::new().fg(ACCENT())),
        sp2,
    );
    draw_milestones(f, brain, s, jal_a);
}

/// Totals since the start, and the next round number to reach for each.
fn draw_milestones(f: &mut Frame, brain: &Brain, s: &Snapshot, area: Rect) {
    let total =
        |name: &str| -> u64 { s.metrics.iter().find(|m| m.name == name).map(|m| m.days.values().sum()).unwrap_or(0) };
    let next = |v: u64| -> u64 {
        [10u64, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000]
            .into_iter()
            .find(|m| *m > v)
            .unwrap_or(v + 1)
    };
    let mut lines = vec![];
    if let Some(d) = s.metrics.first().and_then(|m| m.days.keys().next().copied()) {
        lines.push(Line::from(Span::styled(
            trf!("depuis le {} ({} j)", "since {} ({} d)", fr_date(d), (brain.today() - d).num_days() + 1),
            Style::new().fg(MUTED()),
        )));
    }
    for (label, name) in [
        (tr("mots écrits", "words written"), tr("Mots écrits", "Words written")),
        (tr("idées & savoirs", "ideas & knowledge"), tr("Idées & savoirs", "Ideas & knowledge")),
        (tr("connexions", "connections"), tr("Connexions", "Connections")),
        (tr("tâches faites", "tasks done"), tr("Tâches faites", "Tasks done")),
        (tr("minutes de focus", "focus minutes"), tr("Focus", "Focus")),
    ] {
        let v = total(name);
        let n = next(v);
        let filled = ((v * 24 / n.max(1)) as usize).min(24);
        lines.push(Line::from(vec![
            Span::styled(format!("{v:>8} "), Style::new().fg(TEXT()).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{label:<18}"), Style::new().fg(MUTED())),
            Span::styled("▰".repeat(filled), Style::new().fg(OK())),
            Span::styled("▱".repeat(24 - filled), Style::new().fg(FAINT())),
            Span::styled(
                trf!("  prochain palier : {n} (encore {})", "  next milestone: {n} ({} to go)", n - v),
                Style::new().fg(MUTED()),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines).block(panel(tr("jalons", "milestones"), false)), area);
}

fn draw_themes(f: &mut Frame, brain: &Brain, app: &mut App, body: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(body);
    let items: Vec<ListItem> = app
        .snap
        .tree
        .iter()
        .map(|r| {
            let (glyph, color) = kind_icon(&r.kind);
            let mut name_style = Style::new().fg(if r.depth == 0 { color } else { TEXT() });
            if r.depth == 0 {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            let mut spans = vec![
                Span::raw("  ".repeat(r.depth as usize)),
                Span::styled(format!("{glyph} "), Style::new().fg(color)),
                Span::styled(r.name.clone(), name_style),
            ];
            if r.count > 0 {
                spans.push(Span::styled(format!("  {}", r.count), Style::new().fg(MUTED())));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let title = trf!(
        "thèmes · {} grands thèmes",
        "themes · {} grand themes",
        app.snap.tree.iter().filter(|r| r.depth == 0).count()
    );
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                tr("pas encore de thèmes · écris (:e) puis :w", "no themes yet · write (:e) then :w"),
                Style::new().fg(MUTED()),
            ))
            .block(panel(&title, true)),
            left,
        );
    } else {
        f.render_stateful_widget(
            List::new(items)
                .block(panel(&title, true))
                .highlight_style(Style::new().bg(SEL_BG()).add_modifier(Modifier::BOLD))
                .highlight_symbol("› "),
            left,
            &mut app.tree,
        );
    }
    let detail = app
        .tree
        .selected()
        .and_then(|i| app.snap.tree.get(i))
        .and_then(|r| r.entity)
        .map(|id| entity_detail(brain, id))
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(detail)
            .block(panel(tr("détail · Entrée ouvre la note", "details · Enter opens the note"), false))
            .wrap(Wrap { trim: false }),
        right,
    );
}

fn centered(r: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(r.width.saturating_sub(2));
    let h = h.min(r.height.saturating_sub(2));
    Rect { x: r.x + (r.width - w) / 2, y: r.y + r.height.saturating_sub(h) / 3, width: w, height: h }
}

fn draw_pager(f: &mut Frame, p: &Pager) {
    let a = f.area();
    let area = centered(a, a.width.saturating_sub(8).max(40), a.height.saturating_sub(4));
    f.render_widget(Clear, area);
    let lines: Vec<Line> =
        p.lines.iter().skip(p.scroll).map(|l| Line::from(Span::styled(l.clone(), Style::new().fg(TEXT())))).collect();
    let title = trf!(
        "{} · {}/{} · j/k défiler · q fermer",
        "{} · {}/{} · j/k scroll · q close",
        p.title,
        (p.scroll + 1).min(p.lines.len()),
        p.lines.len()
    );
    f.render_widget(Paragraph::new(lines).block(panel(&title, true)).wrap(Wrap { trim: false }), area);
}

fn draw_undo(f: &mut Frame, brain: &Brain, app: &mut App) {
    let Some(rows) = &app.undo_list else { return };
    let area = centered(f.area(), 104, (rows.len() as u16 + 2).min(24));
    f.render_widget(Clear, area);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|a| {
            let when = crate::util::parse_ts(&a.ts)
                .map(|t| brain.local(t).format("%d/%m %H:%M").to_string())
                .unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::styled(format!("{when}  "), Style::new().fg(FAINT())),
                Span::styled(format!("{:<22} ", crate::util::truncate_chars(&a.kind, 22)), Style::new().fg(MUTED())),
                Span::styled(a.summary.clone(), Style::new().fg(TEXT())),
            ]))
        })
        .collect();
    f.render_stateful_widget(
        List::new(items)
            .block(panel(
                tr(
                    "historique · Entrée annuler cette action · Échap fermer",
                    "history · Enter undo this action · Esc close",
                ),
                true,
            ))
            .highlight_style(Style::new().bg(SEL_BG()).add_modifier(Modifier::BOLD))
            .highlight_symbol("› "),
        area,
        &mut app.history_list,
    );
}

fn draw_help(f: &mut Frame) {
    let rows = [
        (tr("Partout", "Everywhere"), ""),
        ("L", tr("anglais / français", "English / French")),
        ("T", tr("thème (aussi appliqué à nvim)", "theme (applied to nvim too)")),
        (
            "1 2 3 4 5",
            tr(
                "tableau de bord · activité · thèmes · commandes · graphe",
                "dashboard · activity · themes · commands · graph",
            ),
        ),
        ("Tab  Shift-Tab", tr("panneau suivant · précédent", "next · previous panel")),
        ("j k  gg G  ^d ^u", tr("se déplacer", "move")),
        (
            ":",
            tr("toutes les commandes sb (palette, Tab pour compléter)", "every sb command (palette, Tab to complete)"),
        ),
        ("/", tr("chercher dans tout le cerveau", "search the whole brain")),
        (
            "u",
            tr(
                "historique : annuler une action (tâche cochée par erreur…)",
                "history: undo an action (task ticked by mistake…)",
            ),
        ),
        ("a  e", tr("ajouter une tâche · un événement", "add a task · an event")),
        ("S  r", tr("synchroniser Google · actualiser", "sync Google · refresh")),
        ("o  s", tr("graphe · synthèses (navigateur)", "graph · syntheses (browser)")),
        (".", tr("répéter la dernière commande", "repeat the last command")),
        ("", ""),
        (tr("Tâches", "Tasks"), ""),
        ("x  c  D  + -", tr("fait · annuler · supprimer · priorité", "done · cancel · delete · priority")),
        ("f", tr("voir les tâches terminées (x pour rouvrir)", "show finished tasks (x to reopen)")),
        (
            tr("Entrée", "Enter"),
            tr(
                "fiche de la tâche : modifier titre, date, durée, description…",
                "task card: edit title, date, estimate, description…",
            ),
        ),
        ("", ""),
        (tr("Calendrier", "Calendar"), ""),
        ("h j k l", tr("jour précédent · suivant · semaine", "previous · next day · week")),
        ("[ ]  t", tr("mois précédent · suivant · aujourd'hui", "previous · next month · today")),
        ("v", tr("sélectionner une période (puis e, b, B ou d)", "select a range (then e, b, B or d)")),
        ("e  b  B", tr("événement · bloquer la période · bloquer lun-ven", "event · block the range · block mon-fri")),
        (
            "d",
            tr(
                "supprimer : l'événement choisi, ou ceux du jour / de la période",
                "delete: the selected event, or those of the day / range",
            ),
        ),
        ("", ""),
        (tr("Commandes", "Commands"), ""),
        (
            ":e  :w  :wq  :q",
            tr("journal · ingest · ingest et quitter · quitter", "journal · ingest · ingest and quit · quit"),
        ),
        ("", ""),
        (tr("Graphe (5)", "Graph (5)"), ""),
        ("hjkl  + -  0", tr("déplacer · zoom · recentrer", "pan · zoom · reset")),
        ("Tab  n N  i  b", tr("nœud suivant · voisins · entrer · retour", "next node · neighbours · dive · back")),
        ("/  t  Entrée", tr("trouver un nœud · titres · ouvrir la note", "find a node · labels · open the note")),
    ];
    let area = centered(f.area(), 78, rows.len() as u16 + 2);
    f.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                Line::from(Span::styled(*k, Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD)))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<20}"), Style::new().fg(TEXT())),
                    Span::styled(*v, Style::new().fg(MUTED())),
                ])
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(panel(tr("aide · une touche pour fermer", "help · any key to close"), true)),
        area,
    );
}

fn draw_input(f: &mut Frame, app: &App) {
    let block_title;
    let (title, buf, hint) = match &app.mode {
        Mode::AddTask(b) => {
            block_title = trf!(
                "nouvelle tâche · projet : {} · Tab changer",
                "new task · project: {} · Tab change",
                app.add_project
            );
            (
                block_title.as_str(),
                b,
                tr("ex : réviser l'examen by friday ~2h !!", "e.g. review exam notes by friday ~2h !!"),
            )
        }
        Mode::AddEvent(b) => (
            tr("nouvel événement", "new event"),
            b,
            tr("ex : hackathon le 20 et 21 nov", "e.g. hackathon on nov 20 and 21"),
        ),
        Mode::EditField(i, b) => {
            block_title = trf!("modifier : {}", "edit: {}", field_label(TASK_FIELDS[*i]));
            let hint = match TASK_FIELDS[*i] {
                "due" => tr("ex : 2026-11-20, friday, next monday, none", "e.g. 2026-11-20, friday, next monday, none"),
                "estimate" => tr("ex : 90, 1h30, 45m", "e.g. 90, 1h30, 45m"),
                "priority" => {
                    tr("1 urgent · 2 important · 3 normal · 4 faible", "1 urgent · 2 important · 3 normal · 4 low")
                }
                "project" => tr("nom du projet (créé s'il n'existe pas)", "project name (created if needed)"),
                _ => "",
            };
            (block_title.as_str(), b, hint)
        }
        Mode::Block(b, weekdays) => {
            let (a, z) = app.range_bounds();
            block_title = trf!(
                "bloquer du {} au {}{}",
                "block {} → {}{}",
                a.format("%d/%m"),
                z.format("%d/%m"),
                if *weekdays { tr(" (lun-ven)", " (mon-fri)") } else { "" }
            );
            (
                block_title.as_str(),
                b,
                tr("titre de la période, ex : Vacances, Stage", "title of the period, e.g. Vacation, Internship"),
            )
        }
        _ => return,
    };
    let area = centered(f.area(), 66, 5);
    f.render_widget(Clear, area);
    let p = Paragraph::new(vec![
        Line::from(Span::styled(format!("{buf}▏"), Style::new().fg(TEXT()))),
        Line::from(""),
        Line::from(Span::styled(
            trf!("{hint} · Entrée valider · Échap annuler", "{hint} · Enter confirm · Esc cancel"),
            Style::new().fg(MUTED()),
        )),
    ])
    .block(panel(title, true));
    f.render_widget(p, area);
}

/// Render one frame as text, for checks without an interactive terminal.
/// `view`: 1-3 the views, 4 command palette, 5 finished tasks, 6 undo history, 7 commands, 8 graph,
/// 9 calendar range with the delete picker open (nothing is deleted).
pub fn preview(brain: &Brain, commands: Vec<(String, String, String)>, view: u8, w: u16, h: u16) -> Result<String> {
    let mut app = App::new(brain, commands);
    app.view = match view {
        2 => View::Activity,
        3 => View::Themes,
        7 => View::Commands,
        8 => View::Graph,
        _ => View::Board,
    };
    match view {
        4 => app.mode = Mode::Command("ta".into()),
        5 => app.focus = Panel::Done,
        _ => {}
    }
    app.refresh(brain);
    if view == 8 {
        app.ensure_graph(brain);
    }
    if view == 13 {
        open_themes(&mut app);
    }
    if view == 12
        && let Some(t) = app.snap.open.first()
    {
        app.card = Some(TaskCard { id: t.id, field: 1 });
        app.date_picker = Some((t.id, brain.today() + Duration::days(3)));
    }
    if view == 11 {
        open_proj_picker(brain, &mut app, ProjTarget::NewTask("review exam notes".into()));
        if let Some(p) = &mut app.proj_picker {
            p.filter = String::new();
        }
    }
    if view == 10 {
        app.card = app.snap.open.first().map(|t| TaskCard { id: t.id, field: 1 });
    }
    if view == 9 {
        // Calendar range + delete picker (nothing is deleted).
        app.focus = Panel::Calendar;
        app.range = Some(brain.today() + Duration::days(1));
        app.selected_day = brain.today() + Duration::days(5);
        app.refresh(brain);
        calendar_edit_key(brain, &mut app, KeyEvent::from(KeyCode::Char('d')))?;
    }
    if view == 6 {
        open_undo(brain, &mut app);
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h))?;
    term.draw(|f| draw(f, brain, &mut app))?;
    let buf = term.backend().buffer().clone();
    if std::env::var("SB_PREVIEW_SVG").is_ok() {
        return Ok(buffer_svg(&buf, w, h));
    }
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    Ok(out)
}

/// The rendered frame as a coloured SVG (README screenshots: `SB_PREVIEW_SVG=1`).
fn buffer_svg(buf: &ratatui::buffer::Buffer, w: u16, h: u16) -> String {
    const CW: f32 = 8.4;
    const CH: f32 = 17.0;
    let hex = |c: Color, fallback: Color| -> String {
        match if c == Color::Reset { fallback } else { c } {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            _ => "#c5c9c7".into(),
        }
    };
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let bg0 = hex(BG(), BG());
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" font-family=\"JetBrains Mono, DejaVu Sans Mono, monospace\" font-size=\"14\">\n<rect width=\"100%\" height=\"100%\" fill=\"{bg0}\"/>\n",
        (w as f32 * CW).ceil(),
        (h as f32 * CH).ceil()
    );
    for y in 0..h {
        for x in 0..w {
            let c = &buf[(x, y)];
            let bg = hex(c.bg, BG());
            if bg != bg0 {
                out += &format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{CH}\" fill=\"{bg}\"/>\n",
                    x as f32 * CW,
                    y as f32 * CH,
                    CW + 0.4
                );
            }
            let sym = c.symbol();
            if !sym.trim().is_empty() {
                let bold = if c.modifier.contains(Modifier::BOLD) { " font-weight=\"bold\"" } else { "" };
                out += &format!(
                    "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\"{bold}>{}</text>\n",
                    x as f32 * CW,
                    y as f32 * CH + CH * 0.78,
                    hex(c.fg, TEXT()),
                    esc(sym)
                );
            }
        }
    }
    out + "</svg>\n"
}

// ---------------------------------------------------------------------------
// Commandes page

fn category(cmd: &str) -> &'static str {
    let head = cmd.split(' ').next().unwrap_or("");
    match head {
        "e" | "w" | "wq" | "q" | "undo" | "lang" | "theme" => "tableau de bord",
        "journal" | "ingest" | "reprocess" | "search" | "ask" | "memory" => "capture et mémoire",
        "task" | "tasks" | "today" | "plan" | "session" | "briefing" => "tâches et agenda",
        "calendar" | "event" => "google",
        "synth" | "themes" | "connect" | "unlink" | "graph" | "reset" => "connaissances",
        _ => "système",
    }
}

/// Switch English ⇄ French, remember it, and rebuild the translated data.
fn toggle_lang(brain: &Brain, app: &mut App) {
    set_en(!en());
    let _ = std::fs::write(brain.home.join("data/dash_lang"), if en() { "en" } else { "fr" });
    // Builtin descriptions and metric names are language-dependent.
    let sb: Vec<(String, String)> =
        app.commands.iter().filter(|(c, _)| !builtins().iter().any(|(b, _)| b == c)).cloned().collect();
    app.commands = builtins().iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    app.commands.extend(sb);
    sort_commands(&mut app.commands);
    app.refresh(brain);
    app.status = tr("tableau de bord en français", "dashboard in English").into();
}

fn cat_label(c: &str) -> &'static str {
    match c {
        "tableau de bord" => tr("tableau de bord", "dashboard"),
        "capture et mémoire" => tr("capture et mémoire", "capture & memory"),
        "tâches et agenda" => tr("tâches et agenda", "tasks & schedule"),
        "google" => "google",
        "connaissances" => tr("connaissances", "knowledge"),
        _ => tr("système", "system"),
    }
}

const CATEGORIES: [&str; 6] =
    ["tableau de bord", "capture et mémoire", "tâches et agenda", "google", "connaissances", "système"];

fn sort_commands(cmds: &mut [(String, String)]) {
    cmds.sort_by_key(|(c, _)| (CATEGORIES.iter().position(|x| *x == category(c)).unwrap_or(9), c.clone()));
}

fn draw_commands(f: &mut Frame, app: &mut App, body: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(body);
    let mut items = vec![];
    let mut last = "";
    // Category headers are drawn inline as a dim prefix on the first command of each group.
    for (c, d) in &app.commands {
        let cat = category(c);
        let head = if cat != last { format!("{:<20}", cat_label(cat)) } else { " ".repeat(20) };
        last = cat;
        items.push(ListItem::new(Line::from(vec![
            Span::styled(head, Style::new().fg(ACCENT())),
            Span::styled(format!("{c:<20}"), Style::new().fg(TEXT()).add_modifier(Modifier::BOLD)),
            Span::styled(crate::util::truncate_chars(d, 60), Style::new().fg(MUTED())),
        ])));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(panel(&trf!("commandes ({})", "commands ({})", app.commands.len()), true))
            .highlight_style(Style::new().bg(SEL_BG()))
            .highlight_symbol("› "),
        left,
        &mut app.cmd_list,
    );
    let sel = app.cmd_list.selected().and_then(|i| app.commands.get(i));
    let mut lines = vec![];
    if let Some((c, d)) = sel {
        lines.push(Line::from(Span::styled(format!(":{c}"), Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD))));
        lines.push(Line::from(Span::styled(d.clone(), Style::new().fg(TEXT()))));
        lines.push(Line::from(""));
        match app.help_text.get(c) {
            Some(h) => {
                for l in h.lines() {
                    let st = if l.ends_with(':') && !l.starts_with(' ') {
                        Style::new().fg(ACCENT())
                    } else {
                        Style::new().fg(MUTED())
                    };
                    lines.push(Line::from(Span::styled(l.to_string(), st)));
                }
            }
            None => lines.push(Line::from(Span::styled(
                tr("commande du tableau de bord", "dashboard command"),
                Style::new().fg(MUTED()),
            ))),
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            tr(
                "Entrée : préparer la commande dans la barre « : », compléter puis Entrée pour lancer",
                "Enter: prepare the command in the “:” bar, complete it, then Enter to run",
            ),
            Style::new().fg(FAINT()),
        )));
    }
    f.render_widget(Paragraph::new(lines).block(panel(tr("aide", "help"), false)).wrap(Wrap { trim: false }), right);
}

// ---------------------------------------------------------------------------
// Graph view (braille canvas, force layout computed here)

struct TNode {
    label: String,
    kind: String,
    x: f64,
    y: f64,
    deg: usize,
    entity: Option<i64>,
    path: Option<String>,
}

struct TGraph {
    nodes: Vec<TNode>,
    edges: Vec<(usize, usize, bool)>, // (a, b, structural tree edge)
    adj: Vec<Vec<usize>>,
    /// Node indices by importance (degree), for Tab navigation and search.
    order: Vec<usize>,
}

#[derive(Default)]
struct GView {
    cx: f64,
    cy: f64,
    zoom: f64,
    sel: Option<usize>,
    /// Node whose neighbours `n`/`N` walk through (`i` dives into the selection, `b` goes back).
    anchor: Option<usize>,
    /// 0 hubs + selection, 1 all, 2 none
    labels: u8,
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "area" => AREA(),
        "theme" => THEME(),
        "project" => WARN(),
        "goal" => OK(),
        "idea" => ACCENT(),
        "resource" => RESOURCE(),
        "task" | "deadline" => SESSION(),
        _ => CONCEPT(),
    }
}

fn hash01(s: &str, salt: u64) -> f64 {
    let mut h: u64 = 1469598103934665603 ^ salt;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    (h % 10_000) as f64 / 10_000.0
}

/// Fruchterman-Reingold on a 100×100 field, deterministic.
fn layout(nodes: &mut [TNode], edges: &[(usize, usize, bool)]) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    for nd in nodes.iter_mut() {
        nd.x = hash01(&nd.label, 1) * 100.0;
        nd.y = hash01(&nd.label, 2) * 100.0;
    }
    let k = (10_000.0 / n as f64).sqrt() * 0.9;
    let mut temp = 12.0;
    for _ in 0..350 {
        let mut d = vec![(0.0f64, 0.0f64); n];
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (nodes[i].x - nodes[j].x, nodes[i].y - nodes[j].y);
                let dist = (dx * dx + dy * dy).sqrt().max(0.01);
                let f = k * k / dist;
                let (ux, uy) = (dx / dist * f, dy / dist * f);
                d[i].0 += ux;
                d[i].1 += uy;
                d[j].0 -= ux;
                d[j].1 -= uy;
            }
        }
        for &(a, b, tree) in edges {
            let (dx, dy) = (nodes[a].x - nodes[b].x, nodes[a].y - nodes[b].y);
            let dist = (dx * dx + dy * dy).sqrt().max(0.01);
            let f = dist * dist / k * if tree { 1.4 } else { 0.6 };
            let (ux, uy) = (dx / dist * f, dy / dist * f);
            d[a].0 -= ux;
            d[a].1 -= uy;
            d[b].0 += ux;
            d[b].1 += uy;
        }
        for (i, nd) in nodes.iter_mut().enumerate() {
            // Gravity keeps isolated pieces on screen.
            d[i].0 += (50.0 - nd.x) * 0.4;
            d[i].1 += (50.0 - nd.y) * 0.4;
            let len = (d[i].0 * d[i].0 + d[i].1 * d[i].1).sqrt().max(0.01);
            nd.x += d[i].0 / len * len.min(temp);
            nd.y += d[i].1 / len * len.min(temp);
        }
        temp = (temp * 0.985).max(0.3);
    }
    // Fit the result into the 5..95 field (aspect kept), whatever the spread.
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for nd in nodes.iter() {
        x0 = x0.min(nd.x);
        x1 = x1.max(nd.x);
        y0 = y0.min(nd.y);
        y1 = y1.max(nd.y);
    }
    let scale = 90.0 / (x1 - x0).max(y1 - y0).max(1e-6);
    let (ox, oy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    for nd in nodes.iter_mut() {
        nd.x = 50.0 + (nd.x - ox) * scale;
        nd.y = 50.0 + (nd.y - oy) * scale;
    }
}

fn build_graph(brain: &Brain) -> Result<TGraph> {
    let g = crate::graph::build(brain)?;
    let keep: Vec<&crate::graph::Node> = g.nodes.iter().filter(|n| n.kind != "journal").collect();
    let index: HashMap<&str, usize> = keep.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let mut nodes: Vec<TNode> = keep
        .iter()
        .map(|n| TNode {
            label: n.label.clone(),
            kind: n.kind.clone(),
            x: 0.0,
            y: 0.0,
            deg: 0,
            entity: n.id.strip_prefix('e').and_then(|x| x.parse().ok()),
            path: n.path.clone(),
        })
        .collect();
    let mut edges = vec![];
    for e in &g.edges {
        if let (Some(&a), Some(&b)) = (index.get(e.s.as_str()), index.get(e.t.as_str())) {
            edges.push((a, b, matches!(e.rel.as_str(), "part_of" | "in_theme" | "in_area")));
        }
    }
    let mut adj = vec![vec![]; nodes.len()];
    for &(a, b, _) in &edges {
        adj[a].push(b);
        adj[b].push(a);
        nodes[a].deg += 1;
        nodes[b].deg += 1;
    }
    layout(&mut nodes, &edges);
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|a, b| nodes[*b].deg.cmp(&nodes[*a].deg).then(nodes[*a].label.cmp(&nodes[*b].label)));
    Ok(TGraph { nodes, edges, adj, order })
}

impl App {
    fn ensure_graph(&mut self, brain: &Brain) {
        if self.graph.is_some() {
            return;
        }
        match build_graph(brain) {
            Ok(g) => {
                let first = g.order.first().copied();
                self.gview = GView { cx: 50.0, cy: 50.0, zoom: 1.0, sel: first, anchor: first, labels: 0 };
                self.graph = Some(g);
            }
            Err(e) => self.status = trf!("graphe : {e:#}", "graph: {e:#}"),
        }
    }

    fn center_on_selected(&mut self) {
        if let (Some(g), Some(i)) = (&self.graph, self.gview.sel) {
            self.gview.cx = g.nodes[i].x;
            self.gview.cy = g.nodes[i].y;
        }
    }
}

fn graph_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let Some(g) = &app.graph else { return Ok(()) };
    let v = &mut app.gview;
    let span = 60.0 / v.zoom;
    match k.code {
        KeyCode::Char('h') | KeyCode::Left => v.cx -= span * 0.12,
        KeyCode::Char('l') | KeyCode::Right => v.cx += span * 0.12,
        KeyCode::Char('k') | KeyCode::Up => v.cy += span * 0.12,
        KeyCode::Char('j') | KeyCode::Down => v.cy -= span * 0.12,
        KeyCode::Char('+' | '=') => v.zoom = (v.zoom * 1.3).min(12.0),
        KeyCode::Char('-') => v.zoom = (v.zoom / 1.3).max(0.4),
        KeyCode::Char('0') => {
            v.cx = 50.0;
            v.cy = 50.0;
            v.zoom = 1.0;
        }
        KeyCode::Char('t') => v.labels = (v.labels + 1) % 3,
        KeyCode::Tab | KeyCode::BackTab => {
            let n = g.order.len();
            if n > 0 {
                let pos = v.sel.and_then(|s| g.order.iter().position(|x| *x == s)).unwrap_or(0);
                let next = if k.code == KeyCode::Tab { (pos + 1) % n } else { (pos + n - 1) % n };
                v.sel = Some(g.order[next]);
                v.anchor = v.sel;
                app.center_on_selected();
            }
        }
        KeyCode::Char('n' | 'N') => {
            let anchor = v.anchor.or(v.sel);
            if let Some(a) = anchor {
                let mut nb = g.adj[a].clone();
                nb.sort_by(|x, y| g.nodes[*y].deg.cmp(&g.nodes[*x].deg).then(x.cmp(y)));
                nb.dedup();
                if !nb.is_empty() {
                    let n = nb.len();
                    let pos = v.sel.and_then(|s| nb.iter().position(|x| *x == s));
                    let next = match (pos, k.code) {
                        (None, KeyCode::Char('n')) => 0,
                        (None, _) => n - 1,
                        (Some(p), KeyCode::Char('n')) => (p + 1) % n,
                        (Some(p), _) => (p + n - 1) % n,
                    };
                    v.anchor = Some(a);
                    v.sel = Some(nb[next]);
                    app.center_on_selected();
                }
            }
        }
        KeyCode::Char('i') => {
            v.anchor = v.sel;
            app.status = tr("voisins de ce nœud : n / N", "neighbours of this node: n / N").into();
        }
        KeyCode::Char('b') => {
            v.sel = v.anchor;
            app.center_on_selected();
        }
        KeyCode::Char('c') => app.center_on_selected(),
        KeyCode::Char('r') => {
            app.graph = None;
            app.ensure_graph(brain);
            app.status = tr("graphe recalculé", "graph recomputed").into();
        }
        KeyCode::Char('o') => run_command(term, brain, app, "graph")?,
        KeyCode::Enter => {
            let path = v.sel.and_then(|s| g.nodes[s].path.clone());
            if let Some(p) = path {
                open_in_editor(term, brain, app, PathBuf::from(p))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn draw_graph(f: &mut Frame, brain: &Brain, app: &mut App, body: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Min(60), Constraint::Length(46)]).areas(body);
    let Some(g) = &app.graph else {
        f.render_widget(
            Paragraph::new(tr("graphe indisponible", "graph unavailable")).block(panel(tr("graphe", "graph"), true)),
            left,
        );
        return;
    };
    let v = &app.gview;
    let inner_w = left.width.saturating_sub(2).max(1) as f64;
    let inner_h = left.height.saturating_sub(2).max(1) as f64;
    // Braille cells are 2×4 dots: keep circles round.
    let span_x = 100.0 / v.zoom;
    let span_y = span_x * (inner_h * 4.0) / (inner_w * 2.0);
    let (x0, x1) = (v.cx - span_x / 2.0, v.cx + span_x / 2.0);
    let (y0, y1) = (v.cy - span_y / 2.0, v.cy + span_y / 2.0);
    let sel = v.sel;
    let near: Vec<usize> = sel.map(|s| g.adj[s].clone()).unwrap_or_default();
    let labels = v.labels;
    let zoom = v.zoom;
    let title = trf!(
        "graphe · {} nœuds · {} liens · zoom {:.1}",
        "graph · {} nodes · {} links · zoom {:.1}",
        g.nodes.len(),
        g.edges.len(),
        zoom
    );
    let canvas = Canvas::default()
        .block(panel(&title, true))
        .marker(Marker::Braille)
        .x_bounds([x0, x1])
        .y_bounds([y0, y1])
        .paint(|ctx| {
            for &(a, b, tree) in &g.edges {
                let hot = sel.is_some_and(|s| s == a || s == b);
                let color = if hot {
                    ACCENT()
                } else if tree {
                    Color::Rgb(62, 64, 72)
                } else {
                    Color::Rgb(84, 80, 104)
                };
                let (na, nb) = (&g.nodes[a], &g.nodes[b]);
                ctx.draw(&CLine { x1: na.x, y1: na.y, x2: nb.x, y2: nb.y, color });
            }
            ctx.layer();
            let mut by_kind: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
            for nd in &g.nodes {
                by_kind.entry(nd.kind.as_str()).or_default().push((nd.x, nd.y));
            }
            for (kind, pts) in &by_kind {
                ctx.draw(&Points { coords: pts, color: kind_color(kind) });
            }
            for nd in g.nodes.iter().filter(|n| matches!(n.kind.as_str(), "area" | "project" | "goal")) {
                let r = (0.6 + nd.deg as f64 * 0.08).min(2.0) / zoom.sqrt();
                ctx.draw(&Circle { x: nd.x, y: nd.y, radius: r, color: kind_color(&nd.kind) });
            }
            if let Some(s) = sel {
                let nd = &g.nodes[s];
                ctx.draw(&Circle { x: nd.x, y: nd.y, radius: 2.2 / zoom.sqrt(), color: TEXT() });
            }
            ctx.layer();
            for (i, nd) in g.nodes.iter().enumerate() {
                let hub = matches!(nd.kind.as_str(), "area" | "project" | "goal");
                let show = match labels {
                    1 => true,
                    2 => Some(i) == sel,
                    _ => hub || Some(i) == sel || near.contains(&i) || (zoom >= 2.5 && nd.kind == "theme"),
                };
                if show {
                    let st = if Some(i) == sel {
                        Style::new().fg(TEXT()).add_modifier(Modifier::BOLD)
                    } else if hub {
                        Style::new().fg(kind_color(&nd.kind)).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(MUTED())
                    };
                    ctx.print(nd.x, nd.y, Line::from(Span::styled(format!(" {}", nd.label), st)));
                }
            }
        });
    f.render_widget(canvas, left);

    // Side panel: selected node.
    let mut lines = vec![];
    if let Some(s) = sel {
        let nd = &g.nodes[s];
        match nd.entity {
            Some(id) => lines = entity_detail(brain, id),
            None => {
                lines.push(Line::from(Span::styled(
                    nd.label.clone(),
                    Style::new().fg(TEXT()).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(nd.kind.clone(), Style::new().fg(MUTED()))));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            trf!("voisins ({})", "neighbours ({})", g.adj[s].len()),
            Style::new().fg(ACCENT()),
        )));
        let mut nb = g.adj[s].clone();
        nb.sort_by(|a, b| g.nodes[*b].deg.cmp(&g.nodes[*a].deg));
        nb.dedup();
        for i in nb.into_iter().take(14) {
            let (glyph, col) = kind_icon(&g.nodes[i].kind);
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::new().fg(col)),
                Span::styled(g.nodes[i].label.clone(), Style::new().fg(TEXT())),
            ]));
        }
    }
    let legend = Line::from(vec![
        Span::styled("■ ", Style::new().fg(AREA())),
        Span::styled(tr("grand thème ", "grand theme "), Style::new().fg(MUTED())),
        Span::styled("■ ", Style::new().fg(THEME())),
        Span::styled(tr("thème ", "theme "), Style::new().fg(MUTED())),
        Span::styled("■ ", Style::new().fg(CONCEPT())),
        Span::styled("concept ", Style::new().fg(MUTED())),
        Span::styled("■ ", Style::new().fg(ACCENT())),
        Span::styled(tr("idée ", "idea "), Style::new().fg(MUTED())),
        Span::styled("■ ", Style::new().fg(RESOURCE())),
        Span::styled(tr("ressource", "resource"), Style::new().fg(MUTED())),
    ]);
    let [detail_a, legend_a] = Layout::vertical([Constraint::Min(5), Constraint::Length(3)]).areas(right);
    f.render_widget(
        Paragraph::new(lines).block(panel(tr("nœud sélectionné", "selected node"), false)).wrap(Wrap { trim: false }),
        detail_a,
    );
    f.render_widget(
        Paragraph::new(legend).block(panel(tr("légende", "legend"), false)).wrap(Wrap { trim: false }),
        legend_a,
    );
}

// ---------------------------------------------------------------------------
// Calendar editing: ranges, delete picker, confirmation

impl App {
    /// (first, last) day of the calendar selection, or the selected day alone.
    fn range_bounds(&self) -> (NaiveDate, NaiveDate) {
        let s = self.selected_day;
        match self.range {
            Some(r) => (r.min(s), r.max(s)),
            None => (s, s),
        }
    }
}

fn deletable(i: &DayItem) -> bool {
    !matches!(i.r, Ref::None)
}

/// Every deletable item between two days (a block spanning several days appears once).
fn items_between(brain: &Brain, app: &mut App, a: NaiveDate, b: NaiveDate) -> Vec<(DayItem, NaiveDate)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    let keep = app.selected_day;
    let mut d = a;
    while d <= b {
        if month_start(d) != app.remote_month.0 {
            // The range crosses a month: fetch that month's events too.
            app.selected_day = d;
            app.ensure_month(brain, false);
        }
        let titled = app.titled(brain);
        for it in day_items(brain, d, &app.remote_month.1, titled).unwrap_or_default() {
            if deletable(&it) && seen.insert(it.key()) {
                out.push((it, d));
            }
        }
        d += Duration::days(1);
    }
    app.selected_day = keep;
    app.ensure_month(brain, false);
    out
}

fn calendar_edit_key(brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    match (app.focus, k.code) {
        (Panel::Calendar, KeyCode::Char('v')) => {
            app.range = if app.range.is_some() { None } else { Some(app.selected_day) };
            app.status = if app.range.is_some() {
                tr("sélection : hjkl pour étendre, puis e / b / d", "range: hjkl to extend, then e / b / d").into()
            } else {
                tr("sélection annulée", "range cleared").into()
            };
        }
        (Panel::Day | Panel::Today, KeyCode::Char('v' | 'b' | 'B')) => {}
        (_, KeyCode::Char('e')) => {
            let (a, b) = if app.focus == Panel::Today { (brain.today(), brain.today()) } else { app.range_bounds() };
            let prefix = if a == b {
                format!("le {} ", a.format("%d/%m/%Y"))
            } else {
                format!("du {} au {} ", a.format("%d/%m/%Y"), b.format("%d/%m/%Y"))
            };
            app.range = None;
            app.mode = Mode::AddEvent(prefix);
        }
        (Panel::Calendar, KeyCode::Char('b' | 'B')) => {
            app.mode = Mode::Block(String::new(), k.code == KeyCode::Char('B'));
        }
        (Panel::Calendar, KeyCode::Char('d') | KeyCode::Delete) => {
            let (a, b) = app.range_bounds();
            let items = items_between(brain, app, a, b);
            if items.is_empty() {
                app.status = tr("rien à supprimer sur cette période", "nothing to delete in this range").into();
                return Ok(());
            }
            let title = if a == b {
                trf!("supprimer · {}", "delete · {}", fr_date(a))
            } else {
                trf!("supprimer · du {} au {}", "delete · {} → {}", a.format("%d/%m"), b.format("%d/%m"))
            };
            app.picker = Some(Picker {
                title,
                items: items.into_iter().map(|(i, d)| (i, d, false)).collect(),
                state: ListState::default().with_selected(Some(0)),
            });
        }
        (Panel::Day | Panel::Today, KeyCode::Char('d') | KeyCode::Delete) => {
            let (items, st) =
                if app.focus == Panel::Today { (&app.snap.today, &app.today_list) } else { (&app.snap.day, &app.day) };
            match st.selected().and_then(|i| items.get(i)).cloned() {
                Some(it) if deletable(&it) => {
                    app.confirm = Some((describe_delete(std::slice::from_ref(&it)), vec![it]));
                }
                Some(_) => {
                    app.status = tr(
                        "cet élément est en lecture seule (créneau occupé sans détail)",
                        "read-only item (busy slot without details)",
                    )
                    .into()
                }
                None => {}
            }
        }
        _ => {}
    }
    Ok(())
}

fn describe_delete(items: &[DayItem]) -> String {
    match items {
        [one] => match &one.r {
            Ref::Remote(_) => trf!("Supprimer « {} » de Mon agenda ?", "Delete “{}” from your calendar?", one.title),
            Ref::Block(_) => {
                trf!("Supprimer la période « {} » (en entier) ?", "Delete the period “{}” (entirely)?", one.title)
            }
            Ref::Session(_) => trf!("Retirer la session « {} » ?", "Remove the session “{}”?", one.title),
            Ref::Task(_) => trf!("Annuler la tâche « {} » ?", "Cancel the task “{}”?", one.title),
            Ref::DeleteTask(_) => trf!(
                "Supprimer définitivement la tâche « {} » ? (u pour la remettre)",
                "Delete the task “{}” for good? (u brings it back)",
                one.title
            ),
            Ref::None => String::new(),
        },
        many => trf!("Supprimer ces {} éléments ?", "Delete these {} items?", many.len()),
    }
}

fn picker_key(app: &mut App, k: KeyEvent) {
    let Some(p) = &mut app.picker else { return };
    let n = p.items.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.picker = None,
        KeyCode::Char('j') | KeyCode::Down => move_sel(&mut p.state, n, 1),
        KeyCode::Char('k') | KeyCode::Up => move_sel(&mut p.state, n, -1),
        KeyCode::Char(' ' | 'x') => {
            if let Some(i) = p.state.selected() {
                p.items[i].2 = !p.items[i].2;
                move_sel(&mut p.state, n, 1);
            }
        }
        KeyCode::Char('a') => {
            let all = p.items.iter().all(|x| x.2);
            for x in p.items.iter_mut() {
                x.2 = !all;
            }
        }
        KeyCode::Enter => {
            let mut chosen: Vec<DayItem> = p.items.iter().filter(|x| x.2).map(|x| x.0.clone()).collect();
            if chosen.is_empty()
                && let Some(i) = p.state.selected()
            {
                chosen.push(p.items[i].0.clone());
            }
            app.picker = None;
            if !chosen.is_empty() {
                app.confirm = Some((describe_delete(&chosen), chosen));
            }
        }
        _ => {}
    }
}

fn confirm_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    match k.code {
        KeyCode::Char('o' | 'y' | 'O' | 'Y') | KeyCode::Enter => {
            let Some((_, items)) = app.confirm.take() else { return Ok(()) };
            app.status = tr("suppression…", "deleting…").into();
            term.draw(|f| draw(f, brain, app))?;
            app.status = match delete_items(brain, app, &items) {
                Ok(m) => m,
                Err(e) => format!("{e:#}"),
            };
            app.range = None;
            app.ensure_month(brain, true);
            app.refresh(brain);
        }
        KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => {
            app.confirm = None;
            app.status = tr("rien n'a été supprimé", "nothing was deleted").into();
        }
        _ => {}
    }
    Ok(())
}

fn delete_items(brain: &Brain, app: &mut App, items: &[DayItem]) -> Result<String> {
    let mut remote = vec![];
    let mut done = 0;
    let mut notes = vec![];
    if items.iter().any(|i| matches!(i.r, Ref::Remote(_) | Ref::Block(_))) && app.google.is_none() {
        anyhow::bail!(tr("Google non connecté", "Google not connected"));
    }
    for it in items {
        match &it.r {
            Ref::Remote(e) => remote.push((**e).clone()),
            Ref::Block(id) => {
                if let Some(be) = &app.google {
                    let cal = crate::calendar::agent_calendar(brain, be)?;
                    be.delete_event(&cal, id)?;
                }
                brain.conn.execute("DELETE FROM busy WHERE source=?1", [format!("block:{id}")])?;
                crate::actions::log(
                    &brain.conn,
                    "user",
                    "calendar_unblock",
                    &format!("période « {} » supprimée", it.title),
                    None,
                )?;
                done += 1;
            }
            Ref::Session(id) => {
                // Same as deleting it in Google: cancel it and keep that slot free from re-planning.
                let (st, en): (String, String) =
                    brain.conn.query_row("SELECT start, end FROM sessions WHERE id=?1", [id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
                brain.conn.execute("UPDATE sessions SET status='cancelled' WHERE id=?1", [id])?;
                brain.conn.execute(
                    "INSERT INTO busy(source, start, end, title) VALUES ('declined', ?1, ?2, 'declined work block')",
                    rusqlite::params![st, en],
                )?;
                crate::actions::log(&brain.conn, "user", "session_removed", &format!("session #{id} retirée"), None)?;
                done += 1;
            }
            Ref::Task(id) => {
                crate::tasks::set_status(brain, *id, "cancelled")?;
                done += 1;
            }
            Ref::DeleteTask(id) => {
                crate::tasks::delete(brain, *id)?;
                done += 1;
            }
            Ref::None => {}
        }
    }
    if !remote.is_empty() {
        if !brain.cfg.calendar.events_access {
            anyhow::bail!(tr(
                "supprimer dans Mon agenda demande `:calendar auth --events`",
                "deleting in your calendar needs `:calendar auth --events`"
            ));
        }
        if let Some(be) = &app.google {
            let file = crate::gcal_admin::delete(brain, be, &remote)?;
            notes.push(trf!(
                "restaurer : :calendar restore {}",
                "restore: :calendar restore {}",
                file.strip_prefix(&brain.home).unwrap_or(&file).display()
            ));
            done += remote.len();
        }
    }
    // Push the change (removed sessions disappear from Google, cancelled tasks too).
    app.sync(brain, true);
    let mut msg = trf!("{done} élément(s) supprimé(s)", "{done} item(s) deleted");
    if !notes.is_empty() {
        msg.push_str(&format!(" · {}", notes.join(" · ")));
    }
    Ok(msg)
}

fn draw_picker(f: &mut Frame, app: &mut App) {
    let Some(p) = &mut app.picker else { return };
    let area = centered(f.area(), 84, (p.items.len() as u16 + 4).min(22));
    f.render_widget(Clear, area);
    let block = panel(&p.title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [list_a, hint_a] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let items: Vec<ListItem> = p
        .items
        .iter()
        .map(|(it, d, on)| {
            let what = match it.r {
                Ref::Remote(_) => tr("agenda", "calendar"),
                Ref::Block(_) => tr("période", "period"),
                Ref::Session(_) => "focus",
                Ref::Task(_) | Ref::DeleteTask(_) => tr("tâche", "task"),
                Ref::None => "",
            };
            ListItem::new(Line::from(vec![
                Span::styled(if *on { "[x] " } else { "[ ] " }, Style::new().fg(if *on { DUE() } else { MUTED() })),
                Span::styled(format!("{} {:<12}", d.format("%d/%m"), it.time), Style::new().fg(MUTED())),
                Span::styled(format!("{what:<8}"), Style::new().fg(FAINT())),
                Span::styled(it.title.clone(), Style::new().fg(TEXT())),
            ]))
        })
        .collect();
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().bg(SEL_BG())).highlight_symbol("› "),
        list_a,
        &mut p.state,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            tr(
                "Espace cocher · a tout · Entrée supprimer (les cochés, sinon la ligne) · Échap fermer",
                "Space tick · a all · Enter delete (ticked, else the line) · Esc close",
            ),
            Style::new().fg(MUTED()),
        )),
        hint_a,
    );
}

fn draw_confirm(f: &mut Frame, question: &str) {
    let area = centered(f.area(), 64, 5);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(question.to_string(), Style::new().fg(TEXT()).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(vec![
                Span::styled("o", Style::new().fg(DUE()).add_modifier(Modifier::BOLD)),
                Span::styled(tr(" confirmer    ", " confirm    "), Style::new().fg(MUTED())),
                Span::styled("n", Style::new().fg(OK()).add_modifier(Modifier::BOLD)),
                Span::styled(tr(" annuler", " cancel"), Style::new().fg(MUTED())),
            ]),
        ])
        .block(panel(tr("confirmation", "confirmation"), true))
        .wrap(Wrap { trim: true }),
        area,
    );
}

// ---------------------------------------------------------------------------
// Task card: see and edit every field, description in $EDITOR

/// "90", "1h30", "2h", "45m", "1.5h" → minutes.
fn parse_minutes(s: &str) -> Option<i64> {
    let s = s.trim().to_lowercase().replace(' ', "");
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    if let Some(m) = s.strip_suffix("min").or_else(|| s.strip_suffix('m')) {
        return m.parse().ok();
    }
    if let Some((h, m)) = s.split_once('h') {
        let h: f64 = h.replace(',', ".").parse().ok()?;
        let m: i64 = if m.is_empty() { 0 } else { m.trim_end_matches("min").parse().ok()? };
        return Some((h * 60.0).round() as i64 + m);
    }
    None
}

fn apply_field(brain: &Brain, id: i64, field: &str, v: &str) -> Result<String> {
    match field {
        "title" => {
            anyhow::ensure!(!v.is_empty(), "{}", tr("titre vide", "empty title"));
            tasks::edit(brain, id, Some(v.to_string()), None, None, None, None)
        }
        "due" => tasks::edit(
            brain,
            id,
            None,
            Some(if v.is_empty() { "none".into() } else { v.to_string() }),
            None,
            None,
            None,
        ),
        "estimate" => {
            let m = parse_minutes(v)
                .ok_or_else(|| anyhow::anyhow!("{}", tr("durée non comprise", "estimate not understood")))?;
            tasks::edit(brain, id, None, None, Some(m), None, None)
        }
        "priority" => {
            let p: i64 = v.trim_start_matches('p').parse().map_err(|_| anyhow::anyhow!("1-4"))?;
            tasks::edit(brain, id, None, None, None, Some(p), None)
        }
        "project" => tasks::edit(brain, id, None, None, None, None, Some(v.to_string())),
        _ => tasks::set_notes(brain, id, v),
    }
}

fn card_task(brain: &Brain, id: i64) -> Option<TaskRow> {
    tasks::query(&brain.conn, &format!("t.id = {id}"), &tasks::now_str(brain)).ok()?.into_iter().next()
}

fn field_value(t: &TaskRow, f: &str) -> String {
    match f {
        "title" => t.title.clone(),
        "due" => t.due.map(|d| d.to_string()).unwrap_or_default(),
        "estimate" => t.estimate_min.map(tasks::fmt_min).unwrap_or_default(),
        "priority" => t.priority.to_string(),
        "project" => t.project.clone().unwrap_or_default(),
        _ => t.notes.clone().unwrap_or_default(),
    }
}

fn card_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let Some(card) = &mut app.card else { return Ok(()) };
    let id = card.id;
    let n = TASK_FIELDS.len();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.card = None,
        KeyCode::Char('u') => {
            app.card = None;
            open_undo(brain, app);
        }
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => card.field = (card.field + 1) % n,
        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => card.field = (card.field + n - 1) % n,
        KeyCode::Char('E') => {
            card.field = n - 1;
            edit_description(term, brain, app, id)?;
        }
        KeyCode::Char('D') | KeyCode::Delete => {
            if let Some(t) = card_task(brain, id) {
                app.card = None;
                let it = DayItem { time: String::new(), title: t.title, kind: 3, r: Ref::DeleteTask(id) };
                app.confirm = Some((describe_delete(std::slice::from_ref(&it)), vec![it]));
            }
        }
        KeyCode::Enter | KeyCode::Char('e' | 'i') => {
            let field = card.field;
            if TASK_FIELDS[field] == "description" {
                edit_description(term, brain, app, id)?;
            } else if TASK_FIELDS[field] == "project" {
                open_proj_picker(brain, app, ProjTarget::Card(id));
            } else if TASK_FIELDS[field] == "due" && k.code != KeyCode::Char('i') {
                // Calendar picker (i types the date instead).
                let start = card_task(brain, id).and_then(|t| t.due).unwrap_or(brain.today());
                app.date_picker = Some((id, start));
            } else if let Some(t) = card_task(brain, id) {
                app.mode = Mode::EditField(field, field_value(&t, TASK_FIELDS[field]));
            }
        }
        KeyCode::Char('x') => {
            if let Some(t) = card_task(brain, id) {
                let closed = t.status == "done" || t.status == "cancelled";
                let r = tasks::set_status(brain, id, if closed { "open" } else { "done" });
                app.status = match r {
                    Ok(m) => m,
                    Err(e) => format!("{e:#}"),
                };
                term.draw(|f| draw(f, brain, app))?;
                app.sync(brain, true);
                app.refresh(brain);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Description in the real editor (vim experience), saved on quit.
fn edit_description(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, id: i64) -> Result<()> {
    let current = card_task(brain, id).and_then(|t| t.notes).unwrap_or_default();
    let path = brain.home.join(format!("data/task-{id}.md"));
    crate::util::atomic_write(&path, &current)?;
    let vault = brain.vault();
    suspended(term, || {
        // Start the editor inside the vault: notes are then "in cwd" for file trees
        // (neo-tree prompts to change cwd otherwise, and its prompt path is buggy: E95).
        let _ = std::process::Command::new(editor()).arg(&path).current_dir(&vault).status();
    })?;
    let text = std::fs::read_to_string(&path).unwrap_or(current);
    let _ = std::fs::remove_file(&path);
    match tasks::set_notes(brain, id, &text) {
        Ok(msg) => {
            app.status = trf!("{} · synchronisation Google…", "{} · syncing Google…", msg);
            term.draw(|f| draw(f, brain, app))?;
            app.sync(brain, true);
            app.status = msg;
        }
        Err(e) => app.status = format!("{e:#}"),
    }
    app.refresh(brain);
    Ok(())
}

fn draw_card(f: &mut Frame, brain: &Brain, app: &App) {
    let Some(card) = &app.card else { return };
    let Some(t) = card_task(brain, card.id) else { return };
    let area = centered(f.area(), 84, 24);
    f.render_widget(Clear, area);
    let state = match t.status.as_str() {
        "done" => tr("faite", "done"),
        "cancelled" => tr("annulée", "cancelled"),
        "proposed" => tr("à confirmer", "to confirm"),
        _ => tr("à faire", "open"),
    };
    let title = format!("#{} · {state}", t.id);
    let block = panel(&title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [fields_a, _, desc_a, hint_a] =
        Layout::vertical([Constraint::Length(6), Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
            .areas(inner);
    let mut lines = vec![];
    for (i, fld) in TASK_FIELDS.iter().enumerate().take(5) {
        let sel = i == card.field;
        let v = field_value(&t, fld);
        let shown = if v.is_empty() { tr("(vide)", "(empty)").to_string() } else { v };
        let st = if sel { Style::new().bg(SEL_BG()).add_modifier(Modifier::BOLD) } else { Style::new() };
        lines.push(Line::from(vec![
            Span::styled(if sel { "› " } else { "  " }, st.fg(ACCENT())),
            Span::styled(format!("{:<16}", field_label(fld)), st.fg(MUTED())),
            Span::styled(shown, st.fg(TEXT())),
        ]));
    }
    let mut work = String::new();
    if t.done_min + t.planned_min > 0 {
        work = trf!(
            "travail : {} fait, {} planifié",
            "work: {} done, {} planned",
            tasks::fmt_min(t.done_min),
            tasks::fmt_min(t.planned_min)
        );
    }
    lines.push(Line::from(Span::styled(work, Style::new().fg(FAINT()))));
    f.render_widget(Paragraph::new(lines), fields_a);
    let sel_desc = card.field == TASK_FIELDS.len() - 1;
    let desc_title = if sel_desc {
        tr("› description · Entrée ouvre l'éditeur", "› description · Enter opens the editor")
    } else {
        tr("description · E pour modifier", "description · E to edit")
    };
    let body: Vec<Line> = match t.notes.as_deref() {
        Some(n) => n.lines().map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(TEXT())))).collect(),
        None => vec![Line::from(Span::styled(
            tr("aucune description · E pour en écrire une", "no description · E to write one"),
            Style::new().fg(MUTED()),
        ))],
    };
    f.render_widget(Paragraph::new(body).block(panel(desc_title, sel_desc)).wrap(Wrap { trim: false }), desc_a);
    f.render_widget(
        Paragraph::new(Span::styled(
            tr(
                "j/k champ · Entrée modifier · E description · x fait · D supprimer · u annuler · Échap",
                "j/k field · Enter edit · E description · x done · D delete · u undo · Esc",
            ),
            Style::new().fg(MUTED()),
        )),
        hint_a,
    );
}

// ---------------------------------------------------------------------------
// Project picker: every project, filter by typing, unknown name = new project

fn open_proj_picker(brain: &Brain, app: &mut App, target: ProjTarget) {
    let items = tasks::projects(brain).unwrap_or_default();
    let current = match &target {
        ProjTarget::NewTask(_) => Some(app.add_project.clone()),
        ProjTarget::Card(id) => card_task(brain, *id).and_then(|t| t.project),
    };
    let sel = current.and_then(|c| items.iter().position(|(n, _)| n.eq_ignore_ascii_case(&c))).unwrap_or(0);
    app.proj_picker =
        Some(ProjPicker { items, filter: String::new(), state: ListState::default().with_selected(Some(sel)), target });
}

/// Projects matching the filter, plus a "create" row when the name is new.
fn picker_rows(p: &ProjPicker) -> Vec<(String, Option<i64>)> {
    let f = crate::gcal_admin::fold(p.filter.trim());
    let mut rows: Vec<(String, Option<i64>)> = p
        .items
        .iter()
        .filter(|(n, _)| f.is_empty() || crate::gcal_admin::fold(n).contains(&f))
        .map(|(n, c)| (n.clone(), Some(*c)))
        .collect();
    let exact = p.items.iter().any(|(n, _)| crate::gcal_admin::fold(n) == f);
    if !f.is_empty() && !exact {
        rows.push((p.filter.trim().to_string(), None));
    }
    rows
}

/// Add a task from the "new task" box into `project` (unless the text names one with +project).
fn add_task_in(brain: &Brain, text: &str, project: &str) -> Result<()> {
    let before: i64 = brain.conn.query_row("SELECT coalesce(max(id), 0) FROM tasks", [], |r| r.get(0))?;
    tasks::add(brain, text)?;
    let id: i64 = brain.conn.query_row("SELECT coalesce(max(id), 0) FROM tasks", [], |r| r.get(0))?;
    let named = text.split_whitespace().any(|w| w.starts_with('+') && w.len() > 1);
    if id > before && !named && !project.trim().is_empty() && !project.eq_ignore_ascii_case(&brain.cfg.default_project)
    {
        tasks::edit(brain, id, None, None, None, None, Some(project.trim().to_string()))?;
    }
    Ok(())
}

fn proj_picker_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let Some(p) = &mut app.proj_picker else { return Ok(()) };
    let n = picker_rows(p).len();
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Esc => {
            let p = app.proj_picker.take().expect("picker");
            if let ProjTarget::NewTask(text) = p.target {
                app.mode = Mode::AddTask(text);
            }
        }
        KeyCode::Down | KeyCode::Tab => move_sel(&mut p.state, n, 1),
        KeyCode::Char('n') if ctrl => move_sel(&mut p.state, n, 1),
        KeyCode::Up | KeyCode::BackTab => move_sel(&mut p.state, n, -1),
        KeyCode::Char('p') if ctrl => move_sel(&mut p.state, n, -1),
        KeyCode::Backspace => {
            p.filter.pop();
            p.state.select(Some(0));
        }
        KeyCode::Char(c) if !ctrl => {
            p.filter.push(c);
            p.state.select(Some(0));
        }
        KeyCode::Enter => {
            let rows = picker_rows(p);
            let Some((name, _)) = p.state.selected().and_then(|i| rows.get(i)).cloned() else { return Ok(()) };
            let p = app.proj_picker.take().expect("picker");
            match p.target {
                ProjTarget::NewTask(text) => {
                    app.add_project = name;
                    app.mode = Mode::AddTask(text);
                }
                ProjTarget::Card(id) => match tasks::edit(brain, id, None, None, None, None, Some(name)) {
                    Ok(msg) => {
                        app.status = trf!("{} · synchronisation Google…", "{} · syncing Google…", msg);
                        term.draw(|f| draw(f, brain, app))?;
                        app.sync(brain, true);
                        app.status = msg;
                        app.refresh(brain);
                    }
                    Err(e) => app.status = format!("{e:#}"),
                },
            }
        }
        _ => {}
    }
    Ok(())
}

fn draw_proj_picker(f: &mut Frame, app: &mut App) {
    let Some(p) = &mut app.proj_picker else { return };
    let rows = picker_rows(p);
    let h = (rows.len() as u16).clamp(1, 12) + 5;
    let area = centered(f.area(), 60, h);
    f.render_widget(Clear, area);
    let block = panel(tr("projet · tape pour filtrer ou créer", "project · type to filter or create"), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [input_a, sep_a, list_a, hint_a] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
            .areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" + ", Style::new().fg(ACCENT()).add_modifier(Modifier::BOLD)),
            Span::styled(p.filter.clone(), Style::new().fg(TEXT())),
            Span::styled("▏", Style::new().fg(ACCENT())),
        ])),
        input_a,
    );
    f.render_widget(Paragraph::new(Span::styled("─".repeat(inner.width as usize), Style::new().fg(FAINT()))), sep_a);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|(name, count)| match count {
            Some(c) => ListItem::new(Line::from(vec![
                Span::styled("◈ ", Style::new().fg(WARN())),
                Span::styled(format!("{name:<36}"), Style::new().fg(TEXT())),
                Span::styled(trf!("{} ouverte(s)", "{} open", c), Style::new().fg(FAINT())),
            ])),
            None => ListItem::new(Line::from(vec![
                Span::styled("+ ", Style::new().fg(OK())),
                Span::styled(
                    trf!("créer « {} »", "create “{}”", name),
                    Style::new().fg(OK()).add_modifier(Modifier::BOLD),
                ),
            ])),
        })
        .collect();
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().bg(SEL_BG())).highlight_symbol("› "),
        list_a,
        &mut p.state,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            tr("↑↓ choisir · Entrée valider · Échap retour", "↑↓ select · Enter confirm · Esc back"),
            Style::new().fg(MUTED()),
        )),
        hint_a,
    );
}

// ---------------------------------------------------------------------------
// Date picker (due date of a task)

fn date_picker_key(term: &mut DefaultTerminal, brain: &Brain, app: &mut App, k: KeyEvent) -> Result<()> {
    let Some((id, d)) = app.date_picker else { return Ok(()) };
    let nd = match k.code {
        KeyCode::Char('h') | KeyCode::Left => Some(d - Duration::days(1)),
        KeyCode::Char('l') | KeyCode::Right => Some(d + Duration::days(1)),
        KeyCode::Char('j') | KeyCode::Down => Some(d + Duration::days(7)),
        KeyCode::Char('k') | KeyCode::Up => Some(d - Duration::days(7)),
        KeyCode::Char(']' | 'L') | KeyCode::PageDown => Some(add_months(d, 1)),
        KeyCode::Char('[' | 'H') | KeyCode::PageUp => Some(add_months(d, -1)),
        KeyCode::Char('t') => Some(brain.today()),
        _ => None,
    };
    if let Some(nd) = nd {
        app.date_picker = Some((id, nd));
        return Ok(());
    }
    let value = match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.date_picker = None;
            return Ok(());
        }
        KeyCode::Enter | KeyCode::Char(' ') => d.to_string(),
        KeyCode::Char('x' | 'd') | KeyCode::Backspace | KeyCode::Delete => "none".to_string(),
        _ => return Ok(()),
    };
    app.date_picker = None;
    match tasks::edit(brain, id, None, Some(value), None, None, None) {
        Ok(msg) => {
            app.status = trf!("{} · synchronisation Google…", "{} · syncing Google…", msg);
            term.draw(|f| draw(f, brain, app))?;
            app.sync(brain, true);
            app.status = trf!("{} · u pour annuler", "{} · u to undo", msg);
        }
        Err(e) => app.status = format!("{e:#}"),
    }
    app.refresh(brain);
    Ok(())
}

fn draw_date_picker(f: &mut Frame, brain: &Brain, app: &App) {
    let Some((id, sel)) = app.date_picker else { return };
    let today = brain.today();
    let current = card_task(brain, id).and_then(|t| t.due);
    let first = month_start(sel);
    // Show what is already planned that month so the date is chosen in context.
    let marks = month_marks(brain, first, &[]).unwrap_or_default();
    let area = centered(f.area(), 41, 14);
    f.render_widget(Clear, area);
    let title = format!("{} {}", month_name(first.month0()), first.year());
    let block = panel(&title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let pad = " ".repeat((inner.width.saturating_sub(35) / 2) as usize);
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{pad}{}", fr_date(sel)),
            Style::new().fg(TEXT()).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{pad}{}", tr(" lu   ma   me   je   ve   sa   di", " mo   tu   we   th   fr   sa   su")),
            Style::new().fg(MUTED()),
        )),
    ];
    let mut d = first - Duration::days(first.weekday().num_days_from_monday() as i64);
    for _ in 0..6 {
        let mut spans = vec![Span::raw(pad.clone())];
        for _ in 0..7 {
            let in_month = d.month() == first.month();
            let (ev, due, ses) = marks.get(&d).copied().unwrap_or_default();
            let mut st = Style::new().fg(if !in_month {
                FAINT()
            } else if d.weekday().num_days_from_monday() >= 5 {
                MUTED()
            } else {
                TEXT()
            });
            if Some(d) == current {
                st = st.fg(DUE()).add_modifier(Modifier::BOLD);
            }
            if d == today {
                st = st.add_modifier(Modifier::UNDERLINED);
            }
            if d == sel {
                st = Style::new().fg(BG()).bg(ACCENT()).add_modifier(Modifier::BOLD);
            }
            let mark = match () {
                _ if !in_month => " ",
                _ if due > 0 => "!",
                _ if ses > 0 => "+",
                _ if ev > 0 => "•",
                _ => " ",
            };
            spans.push(Span::styled(format!("{:>2}", d.day()), st));
            spans.push(Span::styled(format!("{mark}  "), Style::new().fg(if due > 0 { DUE() } else { SESSION() })));
            d += Duration::days(1);
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        tr(" hjkl jour · [ ] mois · t aujourd'hui", " hjkl day · [ ] month · t today"),
        Style::new().fg(MUTED()),
    )));
    lines.push(Line::from(Span::styled(
        tr(" Entrée choisir · x sans date · Échap", " Enter pick · x no date · Esc"),
        Style::new().fg(MUTED()),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Theme picker (T): live preview, Enter applies to nvim too

fn open_themes(app: &mut App) {
    let cur = crate::theme::index();
    app.theme_picker = Some((ListState::default().with_selected(Some(cur)), cur));
}

fn apply_theme(brain: &Brain, app: &mut App, i: usize) {
    app.status = match crate::theme::set(brain, i) {
        Ok(n) => {
            let t = crate::theme::current();
            if n > 0 {
                trf!("thème : {} · appliqué à nvim", "theme: {} · applied to nvim", t.label)
            } else {
                trf!("thème : {} · nvim le prendra au démarrage", "theme: {} · nvim picks it up at start", t.label)
            }
        }
        Err(e) => format!("{e:#}"),
    };
}

fn theme_key(brain: &Brain, app: &mut App, k: KeyEvent) {
    let Some((st, original)) = &mut app.theme_picker else { return };
    let n = crate::theme::THEMES.len();
    let orig = *original;
    match k.code {
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => move_sel(st, n, 1),
        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => move_sel(st, n, -1),
        KeyCode::Enter => {
            let i = st.selected().unwrap_or(orig);
            app.theme_picker = None;
            apply_theme(brain, app, i);
            return;
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.theme_picker = None;
            // Restore without touching nvim or the saved choice.
            crate::theme::preview(orig);
            return;
        }
        _ => return,
    }
    // Live preview in the dashboard only.
    if let Some(i) = st.selected() {
        crate::theme::preview(i);
    }
}

fn draw_themes_picker(f: &mut Frame, app: &mut App) {
    let Some((st, _)) = &mut app.theme_picker else { return };
    let themes = crate::theme::THEMES;
    let area = centered(f.area(), 58, themes.len() as u16 + 5);
    f.render_widget(Clear, area);
    let block = panel(tr("thème · dashboard + nvim", "theme · dashboard + nvim"), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [list_a, hint_a] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
    let items: Vec<ListItem> = themes
        .iter()
        .map(|t| {
            // A swatch of each theme's own colours.
            let sw = |hex: &str| Span::styled("■", Style::new().fg(tc(hex)).bg(tc(t.bg)));
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<24}", t.label), Style::new().fg(TEXT())),
                Span::styled(" ", Style::new().bg(tc(t.bg))),
                sw(t.accent),
                sw(t.area),
                sw(t.ok),
                sw(t.warn),
                sw(t.concept),
                sw(t.text),
                Span::styled(" ", Style::new().bg(tc(t.bg))),
            ]))
        })
        .collect();
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().bg(SEL_BG()).add_modifier(Modifier::BOLD)).highlight_symbol("› "),
        list_a,
        st,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            tr(
                "j/k aperçu · Entrée appliquer (nvim aussi) · Échap annuler",
                "j/k preview · Enter apply (nvim too) · Esc cancel",
            ),
            Style::new().fg(MUTED()),
        )),
        hint_a,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_months_and_streaks() {
        assert_eq!(
            split_args(r#"task add "review exam notes +inbox" ~2h"#),
            ["task", "add", "review exam notes +inbox", "~2h"]
        );
        let d = |m, n| NaiveDate::from_ymd_opt(2026, m, n).unwrap();
        assert_eq!(add_months(d(1, 31), 1), d(2, 28));
        assert_eq!(add_months(d(11, 8), -11), NaiveDate::from_ymd_opt(2025, 12, 8).unwrap());
        let days: BTreeMap<NaiveDate, u64> =
            [(d(11, 1), 1), (d(11, 2), 3), (d(11, 3), 1), (d(11, 6), 2), (d(11, 7), 1)].into();
        assert_eq!(streaks(&days, d(11, 8)), (2, 3));
        assert_eq!(level(10, 10), 4);
        assert_eq!(parse_minutes("90"), Some(90));
        assert_eq!(parse_minutes("1h30"), Some(90));
        assert_eq!(parse_minutes("2h"), Some(120));
        assert_eq!(parse_minutes("45m"), Some(45));
        assert_eq!(parse_minutes("1.5h"), Some(90));
        assert_eq!(parse_minutes("abc"), None);
    }

    #[test]
    fn multi_day_events_cover_every_day() {
        let dir = tempfile::tempdir().unwrap();
        let brain = crate::brain::Brain::open_with(
            dir.path(),
            crate::config::Config { timezone: "UTC".into(), ..Default::default() },
            crate::inference::Provider::None,
            chrono::Utc::now(),
        )
        .unwrap();
        let all_day = serde_json::json!({"start": {"date": "2026-11-10"}, "end": {"date": "2026-11-16"}});
        let d = event_days(&brain, &all_day);
        assert_eq!(d.len(), 6);
        assert_eq!(d.last().unwrap().0, NaiveDate::from_ymd_opt(2026, 11, 15).unwrap());
        let timed = serde_json::json!({"start": {"dateTime": "2026-11-10T20:00:00Z"}, "end": {"dateTime": "2026-11-12T16:00:00Z"}});
        let labels: Vec<String> = event_days(&brain, &timed).into_iter().map(|x| x.1).collect();
        assert_eq!(labels[0], "20:00 →");
        assert_eq!(labels[2], "→ 16:00");
        assert!(labels[1] == "journée" || labels[1] == "all day");
    }
}
#[cfg(test)]
mod graph_tests {
    #[test]
    fn layout_stays_in_field() {
        let mut nodes: Vec<super::TNode> = (0..30)
            .map(|i| super::TNode {
                label: format!("n{i}"),
                kind: "concept".into(),
                x: 0.0,
                y: 0.0,
                deg: 0,
                entity: None,
                path: None,
            })
            .collect();
        let edges: Vec<(usize, usize, bool)> = (1..30).map(|i| (0, i, true)).collect();
        super::layout(&mut nodes, &edges);
        for n in &nodes {
            assert!(n.x.is_finite() && n.y.is_finite(), "{} {}", n.x, n.y);
            assert!((4.9..95.1).contains(&n.x) && (4.9..95.1).contains(&n.y), "{} {}", n.x, n.y);
        }
    }
}
