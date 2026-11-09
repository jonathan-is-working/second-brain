//! Journal parsing: split Markdown into stable, hashable blocks (paragraphs and
//! list items) and deterministically extract what needs no model: checkbox
//! tasks, explicit prefixes ("Decision:", "Idea:", "TIL:"), dates, estimates,
//! priorities, wikilinks.

use crate::dates::{self, DateMatch};
use crate::util::{normalize, sha256_hex};
use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// 1-based line where the block starts.
    pub line: usize,
    pub section: Option<String>,
    pub text: String,
    pub hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub date: Option<NaiveDate>,
    pub blocks: Vec<Block>,
}

/// Parse a journal file. `file_stem` is used for the entry date when the
/// frontmatter has no `date:`.
pub fn parse(content: &str, file_stem: &str) -> Parsed {
    let mut parsed = Parsed { date: crate::util::parse_date(file_stem), blocks: vec![] };
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    // Frontmatter
    if lines.first().map(|l| l.trim()) == Some("---")
        && let Some(end) = lines.iter().skip(1).position(|l| l.trim() == "---")
    {
        for l in &lines[1..=end] {
            if let Some(v) = l.trim().strip_prefix("date:")
                && let Some(d) = crate::util::parse_date(v.trim().trim_matches('"'))
            {
                parsed.date = Some(d);
            }
        }
        i = end + 2;
    }

    let mut section: Option<String> = None;
    let mut cur: Option<(usize, Vec<String>, usize)> = None; // (line, lines, list indent or usize::MAX for paragraph)
    let mut in_fence = false;

    let flush = |cur: &mut Option<(usize, Vec<String>, usize)>, out: &mut Vec<Block>, section: &Option<String>| {
        if let Some((line, ls, _)) = cur.take() {
            let text = ls.join("\n").trim().to_string();
            if !text.is_empty() {
                out.push(Block { line, section: section.clone(), hash: block_hash(&text), text });
            }
        }
    };

    while i < lines.len() {
        let raw = lines[i];
        let t = raw.trim();
        let lineno = i + 1;
        i += 1;
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut cur, &mut parsed.blocks, &section);
            in_fence = !in_fence;
            continue;
        }
        if in_fence || t.starts_with("<!--") {
            continue;
        }
        if t.is_empty() {
            flush(&mut cur, &mut parsed.blocks, &section);
            continue;
        }
        if let Some(h) = heading(t) {
            flush(&mut cur, &mut parsed.blocks, &section);
            section = Some(h.to_string());
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if is_list_item(t) {
            flush(&mut cur, &mut parsed.blocks, &section);
            cur = Some((lineno, vec![t.to_string()], indent));
            continue;
        }
        match &mut cur {
            // continuation of a list item (indented) or of a paragraph
            Some((_, ls, ind)) if *ind == usize::MAX || indent > *ind => ls.push(t.to_string()),
            _ => {
                flush(&mut cur, &mut parsed.blocks, &section);
                cur = Some((lineno, vec![t.to_string()], usize::MAX));
            }
        }
    }
    flush(&mut cur, &mut parsed.blocks, &section);
    parsed.blocks = group_lists(std::mem::take(&mut parsed.blocks));
    parsed
}

/// Leading ordinal of a list item: `1.`, `2)`, `3.Text`, `4️⃣`. Not `1.5h`.
fn ordinal(text: &str) -> Option<u32> {
    let t = text.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let rest = &t[digits.len()..];
    let ok = if let Some(r) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
        !r.starts_with(|c: char| c.is_ascii_digit())
    } else {
        rest.starts_with('\u{FE0F}') || rest.starts_with('\u{20E3}')
    };
    if ok { digits.parse().ok() } else { None }
}

fn last_ordinal(text: &str) -> Option<u32> {
    text.lines().filter_map(ordinal).next_back()
}

fn is_bullet(text: &str) -> bool {
    let t = text.trim_start();
    (t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")) && !is_checkbox(t)
}

fn is_checkbox(text: &str) -> bool {
    let t = strip_marker(text).to_lowercase();
    t.starts_with("[ ]") || t.starts_with("[x]") || t.starts_with("[-]")
}

const MAX_GROUP_CHARS: usize = 4000;

/// Merge list items into one semantic block so a list ("7 date ideas") is
/// understood as a whole: numbering that continues across blank lines,
/// adjacent bullets, and an intro line ending with ':' before a list.
/// Checkboxes stay separate (each is a task).
fn group_lists(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = vec![];
    for b in blocks {
        let merge = match out.last() {
            Some(prev)
                if !is_checkbox(&prev.text)
                    && !is_checkbox(&b.text)
                    && prev.text.len() + b.text.len() < MAX_GROUP_CHARS =>
            {
                let prev_end = prev.line + prev.text.lines().count() - 1;
                let adjacent = b.line == prev_end + 1;
                let close = b.line <= prev_end + 2;
                let continues =
                    matches!((last_ordinal(&prev.text), ordinal(&b.text)), (Some(p), Some(n)) if n == p + 1);
                let bullets =
                    adjacent && is_bullet(&b.text) && (is_bullet(&prev.text) || prev.text.trim_end().ends_with(':'));
                let intro = close
                    && prev.text.lines().count() == 1
                    && prev.text.trim_end().ends_with(':')
                    && (ordinal(&b.text) == Some(1) || is_bullet(&b.text));
                prev.section == b.section && (continues || bullets || intro)
            }
            _ => false,
        };
        if merge {
            let prev = out.last_mut().unwrap();
            let sep = if b.line == prev.line + prev.text.lines().count() { "\n" } else { "\n\n" };
            prev.text = format!("{}{sep}{}", prev.text, b.text);
            prev.hash = block_hash(&prev.text);
        } else {
            out.push(b);
        }
    }
    out
}

fn heading(t: &str) -> Option<&str> {
    let h = t.trim_start_matches('#');
    (h.len() < t.len() && t.len() - h.len() <= 6 && h.starts_with(' ')).then(|| h.trim())
}

fn is_list_item(t: &str) -> bool {
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// Hash of whitespace-normalized text: reflowing a paragraph is not a change.
pub fn block_hash(text: &str) -> String {
    sha256_hex(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Strip list markers from a block for display/prompting.
pub fn strip_marker(text: &str) -> &str {
    let t = text.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            return r;
        }
    }
    t
}

// ---------------------------------------------------------------------------
// Deterministic extraction

#[derive(Debug, Clone, PartialEq)]
pub enum CheckState {
    Open,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskSpec {
    pub title: String,
    pub state: CheckState,
    pub due: Option<NaiveDate>,
    pub estimate_min: Option<i64>,
    pub priority: Option<i64>,
    /// `[[Name]]` or `+name` references; resolved to projects by the caller.
    pub refs: Vec<String>,
    /// Existing task id given as `#t12`.
    pub task_id: Option<i64>,
    /// `+name` references: explicit project declarations.
    pub new_projects: Vec<String>,
}

/// Explicitly typed lines that need no model.
#[derive(Debug, Clone, PartialEq)]
pub enum Explicit {
    Task(TaskSpec),
    Decision(String),
    Idea(String),
    Knowledge(String),
}

pub fn explicit(text: &str, today: NaiveDate) -> Option<Explicit> {
    let t = strip_marker(text).trim();
    let lower = t.to_lowercase();
    for (marker, state) in [("[ ]", CheckState::Open), ("[x]", CheckState::Done), ("[-]", CheckState::Cancelled)] {
        if lower.starts_with(marker) {
            let body = t[3..].trim();
            if body.is_empty() {
                return None;
            }
            return Some(Explicit::Task(parse_task(body, state, today)));
        }
    }
    if lower.starts_with("todo:") {
        return Some(Explicit::Task(parse_task(t[5..].trim(), CheckState::Open, today)));
    }
    for (p, f) in [
        ("decision:", Explicit::Decision as fn(String) -> Explicit),
        ("decided:", Explicit::Decision),
        ("idea:", Explicit::Idea),
        ("til:", Explicit::Knowledge),
        ("learned:", Explicit::Knowledge),
    ] {
        if lower.starts_with(p) {
            let body = t[p.len()..].trim();
            if !body.is_empty() {
                return Some(f(body.to_string()));
            }
        }
    }
    None
}

pub fn parse_task(body: &str, state: CheckState, today: NaiveDate) -> TaskSpec {
    let mut text = body.trim().to_string();
    if let Some(inner) = text.strip_prefix("~~").and_then(|s| s.strip_suffix("~~")) {
        text = inner.to_string();
    }
    let refs = wikilinks(&text);
    let mut spec = TaskSpec {
        title: String::new(),
        state,
        due: None,
        estimate_min: None,
        priority: None,
        refs,
        task_id: None,
        new_projects: vec![],
    };

    let mut kept = vec![];
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut idx = 0;
    while idx < words.len() {
        let w = words[idx];
        let wl = w.to_lowercase();
        let wl = wl.trim_matches(|c: char| c == '(' || c == ')' || c == ',' || c == ';');
        if let Some(id) = wl.strip_prefix("#t").and_then(|n| n.parse().ok()) {
            spec.task_id = Some(id);
        } else if let Some(p) = priority_token(wl) {
            spec.priority = Some(p);
        } else if let Some(m) = estimate_token(wl, words.get(idx + 1).map(|s| s.to_lowercase())) {
            spec.estimate_min = Some(m.0);
            idx += m.1;
        } else if let Some(p) = w.strip_prefix('+').filter(|p| p.len() > 1) {
            let name = p.replace(['-', '_'], " ");
            spec.refs.push(name.clone());
            spec.new_projects.push(name);
        } else {
            kept.push(w);
        }
        idx += 1;
    }
    let mut title = kept.join(" ");
    if let Some(m) = dates::find_dates(&title, today).into_iter().last() {
        spec.due = Some(m.date);
        title = remove_span(&title, &m);
    }
    if let Some(rest) = title.to_lowercase().find("due:").map(|p| (p, title.clone())) {
        let (p, t) = rest;
        title = format!("{}{}", &t[..p], &t[p + 4..]);
    }
    spec.title = clean_title(&title.replace("[[", "").replace("]]", ""));
    spec
}

fn remove_span(text: &str, m: &DateMatch) -> String {
    format!("{} {}", &text[..m.start], &text[m.end..])
}

fn clean_title(t: &str) -> String {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = t.trim_matches(|c: char| c.is_whitespace() || ",;:-–(".contains(c)).replace(" ,", ",");
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn priority_token(w: &str) -> Option<i64> {
    match w {
        "!!!" | "!1" | "p1" | "#p1" | "#urgent" => Some(1),
        "!!" | "!2" | "p2" | "#p2" | "#important" => Some(2),
        "!3" | "p3" | "#p3" => Some(3),
        "!4" | "p4" | "#p4" | "#someday" => Some(4),
        _ => None,
    }
}

/// `2h`, `90m`, `1.5h`, `~45min`, `est:2h`, `2 hours`. Returns (minutes, extra words consumed).
fn estimate_token(w: &str, next: Option<String>) -> Option<(i64, usize)> {
    let w = w.trim_start_matches("est:").trim_start_matches('~');
    let num_end = w.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(w.len());
    if num_end == 0 {
        return None;
    }
    let n: f64 = w[..num_end].parse().ok()?;
    let (unit, extra) = if num_end == w.len() {
        (next.unwrap_or_default().trim_matches(|c: char| !c.is_alphanumeric()).to_string(), 1)
    } else {
        (w[num_end..].to_string(), 0)
    };
    let mins = match unit.as_str() {
        "h" | "hr" | "hrs" | "hour" | "hours" => n * 60.0,
        "m" | "min" | "mins" | "minute" | "minutes" => n,
        _ => return None,
    };
    let mins = mins.round() as i64;
    (5..=2400).contains(&mins).then_some((mins, extra))
}

pub fn wikilinks(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(s) = rest.find("[[") {
        let after = &rest[s + 2..];
        let Some(e) = after.find("]]") else { break };
        let inner = after[..e].split('|').next().unwrap_or("").trim();
        if !inner.is_empty() && !out.iter().any(|x: &String| x == inner) {
            out.push(inner.to_string());
        }
        rest = &after[e + 2..];
    }
    out
}

// ---------------------------------------------------------------------------
// Heuristic fallback (no model available): conservative commitment detection.

const COMMIT_CUES: &[&str] = &[
    "i need to ",
    "i have to ",
    "i must ",
    "i should ",
    "i've got to ",
    "i have got to ",
    "remember to ",
    "don't forget to ",
];

/// Returns (title, due) for sentences like "I need to finish X by Sunday".
pub fn heuristic_tasks(text: &str, today: NaiveDate) -> Vec<(String, Option<NaiveDate>)> {
    let mut out = vec![];
    for sentence in text.split(['.', '!', '?', '\n']) {
        let s = strip_marker(sentence).trim();
        let lower = s.to_lowercase();
        let Some((cue_pos, cue)) = COMMIT_CUES.iter().filter_map(|c| lower.find(c).map(|p| (p, *c))).min() else {
            continue;
        };
        let body = &s[cue_pos + cue.len()..];
        // split "revise maths before Thursday and finish the prototype by Sunday"
        for part in body.split(" and ") {
            let spec = parse_task(part, CheckState::Open, today);
            if crate::journal::word_count(&spec.title) >= 1 {
                out.push((spec.title, spec.due));
            }
        }
    }
    out
}

/// Cheap triage: does this block plausibly carry information worth a model call?
pub fn worth_llm(text: &str, min_words: usize, today: NaiveDate) -> bool {
    let t = strip_marker(text);
    if word_count(t) >= min_words {
        return true;
    }
    !dates::find_dates(t, today).is_empty()
}

pub fn normalized_title(t: &str) -> String {
    normalize(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()
    }

    #[test]
    fn splits_blocks() {
        let md = "---\ndate: 2026-11-06\n---\n# Day\n\nFirst para\ncontinues here.\n\n- [ ] task one\n  more detail\n- item two\n\n```\ncode - [ ] ignored\n```\nLast.";
        let p = parse(md, "whatever");
        assert_eq!(p.date, NaiveDate::from_ymd_opt(2026, 11, 6));
        let texts: Vec<_> = p.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["First para\ncontinues here.", "- [ ] task one\nmore detail", "- item two", "Last."]);
        assert_eq!(p.blocks[0].line, 6);
        assert_eq!(p.blocks[0].section.as_deref(), Some("Day"));
    }

    #[test]
    fn malformed_markdown_does_not_panic() {
        for md in ["---\nunterminated", "```\nnever closed", "# \n#\n- \n-", "\u{0}\u{feff}[[ [[]] ]]", ""] {
            let _ = parse(md, "2026-01-01");
        }
    }

    #[test]
    fn groups_lists_into_one_block() {
        let md = "Here are the top 3 advice:\n\n1)First one\n\n2)Second one\n\n3)Third\n\nAfter.\n\n\
1. The Real Date:\n   ask questions\n\n2. The Grocery Date:\n   budget\n\n\
- Read the kernel\n- Rewrite Git\n- Build a GC\n\n\
1.SSH lab\nTech: Kali\n\n2.Port scan lab\nTech: Nmap\n\n\
- [ ] task a\n- [ ] task b\n\nTook 1.5h today.\n\n2. not a continuation";
        let p = parse(md, "2026-11-07");
        let t: Vec<&str> = p.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(t.len(), 9, "{t:#?}");
        assert!(t[0].starts_with("Here are") && t[0].ends_with("3)Third"));
        assert_eq!(t[1], "After.");
        assert!(t[2].contains("Real Date") && t[2].contains("Grocery Date"));
        assert!(t[3].contains("kernel") && t[3].contains("GC"));
        assert!(t[4].contains("SSH lab") && t[4].contains("Port scan lab"));
        assert_eq!((t[5], t[6]), ("- [ ] task a", "- [ ] task b"));
        assert_eq!(t[7], "Took 1.5h today.");
        assert_eq!(t[8], "2. not a continuation");
        assert_eq!(ordinal("4\u{FE0F}\u{20E3} Reddit"), Some(4));
        assert_eq!(ordinal("1.5h of work"), None);
    }

    #[test]
    fn hash_ignores_reflow() {
        assert_eq!(block_hash("a  b\nc"), block_hash("a b c"));
    }

    #[test]
    fn checkbox_task() {
        let Some(Explicit::Task(t)) = explicit("- [ ] Finish prototype before Sunday ~3h !! +saas", today()) else {
            panic!()
        };
        assert_eq!(t.title, "Finish prototype");
        assert_eq!(t.due, Some(today()));
        assert_eq!(t.estimate_min, Some(180));
        assert_eq!(t.priority, Some(2));
        assert_eq!(t.refs, vec!["saas"]);
        let Some(Explicit::Task(d)) = explicit("- [x] Send invoice for [[Client Work]] #t4 (30 min)", today()) else {
            panic!()
        };
        assert_eq!(d.state, CheckState::Done);
        assert_eq!(d.task_id, Some(4));
        assert_eq!(d.estimate_min, Some(30));
        assert_eq!(d.refs, vec!["Client Work"]);
        assert_eq!(d.title, "Send invoice for Client Work");
    }

    #[test]
    fn prefixes() {
        assert_eq!(explicit("Decision: use SQLite", today()), Some(Explicit::Decision("use SQLite".into())));
        assert_eq!(explicit("plain text", today()), None);
    }

    #[test]
    fn heuristics() {
        let t = heuristic_tasks(
            "I need to revise mathematics before Thursday and finish the prototype by Sunday.",
            today(),
        );
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].0, "Revise mathematics");
        assert_eq!(t[1].1, NaiveDate::from_ymd_opt(2026, 11, 8));
    }
}
