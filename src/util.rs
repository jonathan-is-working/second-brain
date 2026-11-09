//! Small shared helpers: hashing, normalization, atomic writes, time formats.

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Lowercase, keep alphanumerics, collapse whitespace.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else {
            space = true;
        }
    }
    out
}

/// Normalized entity name: normalize + naive singularization of the last word,
/// so "Vector Embeddings" and "vector embedding" collide.
pub fn norm_name(s: &str) -> String {
    let mut n = normalize(s);
    if n.len() > 4 && n.ends_with('s') && !n.ends_with("ss") && !n.ends_with("us") && !n.ends_with("is") {
        n.pop();
    }
    n
}

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "and", "or", "for", "in", "on", "at", "by", "my", "i", "is", "it", "be", "with",
    "this", "that", "before", "until", "need", "have", "must", "should", "will", "do", "about", "from", "as", "are",
    "was", "me", "we", "our", "your", "can", "could", "would", "some",
];

pub fn content_words(s: &str) -> Vec<String> {
    normalize(s).split(' ').filter(|w| !w.is_empty() && !STOPWORDS.contains(w)).map(String::from).collect()
}

/// Token Jaccard similarity on content words (0..1).
pub fn similarity(a: &str, b: &str) -> f64 {
    let sa: HashSet<_> = content_words(a).into_iter().map(|w| stem(&w)).collect();
    let sb: HashSet<_> = content_words(b).into_iter().map(|w| stem(&w)).collect();
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    sa.intersection(&sb).count() as f64 / sa.union(&sb).count() as f64
}

fn stem(w: &str) -> String {
    let w = w.trim_end_matches("ing").trim_end_matches("ed");
    let w = if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") { &w[..w.len() - 1] } else { w };
    w.to_string()
}

/// Rough token estimate (~4 chars/token) used when providers don't report usage.
pub fn est_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// File name safe on all platforms while staying readable for Obsidian links.
pub fn safe_filename(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if "/\\:*?\"<>|#^[]".contains(c) || c.is_control() { '-' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "untitled".into() } else { truncate_chars(&s, 120) }
}

/// Write via temp file + fsync + rename so readers never see partial content.
pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    // Same content already on disk: don't touch the file. Editors and file trees
    // watching the vault (nvim, neo-tree, Obsidian) then see no event at all.
    if std::fs::read(path).is_ok_and(|old| old == content.as_bytes()) {
        return Ok(());
    }
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.sbtmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub fn ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s).with_context(|| format!("bad timestamp '{s}'"))?.with_timezone(&Utc))
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization() {
        assert_eq!(normalize("  Hello,   World! "), "hello world");
        assert_eq!(norm_name("Vector Embeddings"), norm_name("vector embedding"));
        assert_eq!(norm_name("Business"), "business");
    }

    #[test]
    fn similarity_works() {
        assert!(similarity("Finish the prototype", "finish prototype") > 0.99);
        assert!(similarity("Revise mathematics", "revise maths chapter 3") < 0.5);
        assert_eq!(similarity("", "x"), 0.0);
    }

    #[test]
    fn atomic_write_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/b.md");
        atomic_write(&p, "one").unwrap();
        atomic_write(&p, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }
}

/// Split an inline enumeration ("intro 1) A; 2) B. 3) C") into (intro, items).
/// Markers must count up from 1 and follow a separator, so "v2) x" or "(3)" never
/// match. Returns None unless at least two items are found.
pub fn split_list(s: &str) -> Option<(String, Vec<String>)> {
    let b = s.as_bytes();
    let mut marks: Vec<(usize, usize)> = vec![]; // (marker start, text start)
    let mut want = 1usize;
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0 || matches!(b[i - 1], b' ' | b';' | b'\n' | b'\t' | b':' | b',');
        if boundary && b[i].is_ascii_digit() {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let n: usize = s[i..j].parse().unwrap_or(0);
            let sep = j < b.len() && (b[j] == b')' || (b[j] == b'.' && b.get(j + 1) == Some(&b' ')));
            if n == want && sep && b.get(j + 1).is_none_or(|c| *c == b' ') {
                marks.push((i, j + 1));
                want += 1;
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    if marks.len() < 2 {
        return None;
    }
    let clean = |t: &str| t.trim().trim_end_matches([';', ',']).trim().to_string();
    let intro = clean(&s[..marks[0].0]);
    let mut items = vec![];
    for (k, &(_, start)) in marks.iter().enumerate() {
        let end = marks.get(k + 1).map_or(s.len(), |m| m.0);
        let t = clean(&s[start..end]);
        if !t.is_empty() {
            items.push(t);
        }
    }
    (items.len() >= 2).then_some((intro, items))
}

#[cfg(test)]
mod list_tests {
    use super::split_list;

    #[test]
    fn splits_inline_enumerations() {
        let (intro, items) = split_list("Projets : 1) Lire Linux; 2) Réécrire Git. 3) TCP/IP").unwrap();
        assert_eq!(intro, "Projets :");
        assert_eq!(items, ["Lire Linux", "Réécrire Git.", "TCP/IP"]);
        assert!(split_list("version v2) et 3) rien").is_none());
        assert!(split_list("1) seul élément").is_none());
        let (_, items) = split_list("1. A 2. B 10) non").unwrap();
        assert_eq!(items, ["A", "B 10) non"]);
    }
}
