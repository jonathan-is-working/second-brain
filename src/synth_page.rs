//! Reading view for syntheses: one self-contained HTML page (no network) with
//! every daily/weekly/monthly synthesis, rendered from Markdown in Rust.
//! Model-written Markdown is untrusted: raw HTML is shown as text, scripts and
//! `javascript:`/`data:` links are neutralised, remote images are dropped, and
//! `[[wikilinks]]` become links to the matching Obsidian notes.

use crate::brain::Brain;
use crate::synth::{Period, path, title};
use crate::util::{atomic_write, parse_date};
use anyhow::Result;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use serde::Serialize;
use std::path::PathBuf;

/// Percent-encode for a URI component.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `[[Name]]` / `[[Name|alias]]` → Markdown link to the Obsidian note, or bold text.
fn wikilinks(brain: &Brain, md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(s) = rest.find("[[") {
        out.push_str(&rest[..s]);
        let after = &rest[s + 2..];
        let Some(e) = after.find("]]") else {
            out.push_str(&rest[s..]);
            return out;
        };
        let inner = &after[..e];
        let (name, label) = inner.split_once('|').unwrap_or((inner, inner));
        let (name, label) = (name.trim(), label.trim().replace(['[', ']'], ""));
        let target = crate::memory::find_entity(&brain.conn, name).ok().flatten().and_then(|(id, kind)| {
            let n: String = brain.conn.query_row("SELECT name FROM entities WHERE id=?1", [id], |r| r.get(0)).ok()?;
            Some(crate::notes::note_path(brain, &kind, &n))
        });
        match target {
            Some(p) => out.push_str(&format!("[{label}](obsidian://open?path={})", encode(&p.to_string_lossy()))),
            None => out.push_str(&format!("**{label}**")),
        }
        rest = &after[e + 2..];
    }
    out.push_str(rest);
    out
}

fn safe_url(u: &str) -> bool {
    let l = u.trim().to_ascii_lowercase();
    !(l.starts_with("javascript:") || l.starts_with("data:") || l.starts_with("vbscript:"))
}

/// Markdown → sanitised HTML.
pub fn render(brain: &Brain, md: &str) -> String {
    let md = wikilinks(brain, md);
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(&md, opts).filter_map(|ev| match ev {
        // Raw HTML from the model is displayed, never executed.
        Event::Html(h) | Event::InlineHtml(h) => Some(Event::Text(h)),
        // No remote images (privacy/tracking): keep only their alt text.
        Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => None,
        Event::Start(Tag::Link { link_type, dest_url, title, id }) if !safe_url(&dest_url) => {
            Some(Event::Start(Tag::Link { link_type, dest_url: CowStr::Borrowed("#"), title, id }))
        }
        other => Some(other),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

#[derive(Serialize)]
struct Entry {
    id: String,
    period: &'static str,
    title: String,
    start: String,
    end: String,
    html: String,
    markdown: String,
    file: String,
}

#[derive(Serialize)]
struct Page {
    lang: String,
    selected: Option<String>,
    generated: String,
    entries: Vec<Entry>,
}

pub fn entry_id(p: Period, start: &str) -> String {
    format!("{}-{start}", p.key())
}

/// Write `data/syntheses.html` with every synthesis; `selected` opens first.
pub fn write(brain: &Brain, selected: Option<(Period, chrono::NaiveDate)>) -> Result<PathBuf> {
    let mut st = brain.conn.prepare(
        "SELECT period, start, end, content FROM syntheses
         ORDER BY CASE period WHEN 'day' THEN 0 WHEN 'week' THEN 1 ELSE 2 END, start DESC",
    )?;
    let rows: Vec<(String, String, String, String)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut entries = vec![];
    for (period, start, end, md) in rows {
        let p = Period::parse(&period)?;
        let (Some(s), Some(e)) = (parse_date(&start), parse_date(&end)) else { continue };
        entries.push(Entry {
            id: entry_id(p, &start),
            period: p.key(),
            title: title(brain, p, s, e),
            start,
            end,
            html: render(brain, &md),
            markdown: md,
            file: path(brain, p, s).display().to_string(),
        });
    }
    let page = Page {
        lang: brain.ui_language(),
        selected: selected.map(|(p, d)| entry_id(p, &crate::synth::bounds(p, d).0.to_string())),
        generated: brain.local(brain.now()).format("%Y-%m-%d %H:%M").to_string(),
        entries,
    };
    let json = serde_json::to_string(&page)?
        .replace("</", "<\\/")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let out = brain.home.join("data/syntheses.html");
    atomic_write(&out, &TEMPLATE.replace("/*__DATA__*/null", &json))?;
    Ok(out)
}

const TEMPLATE: &str = include_str!("synth.html");
