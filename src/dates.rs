//! Deterministic resolution of date phrases ("next Thursday", "before Sunday",
//! "2026-11-12", "Nov 20", "in 3 days"). Used directly for checkbox tasks and
//! as hints in LLM prompts so the model never has to do calendar arithmetic.
//!
//! Conventions: a bare/“next”/“by”/“on” weekday is its next occurrence after
//! today; “before X” means the day before X; “last X” is the previous one.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

#[derive(Debug, Clone, PartialEq)]
pub struct DateMatch {
    pub phrase: String,
    pub date: NaiveDate,
    /// Byte range of the phrase (including a leading preposition) in the text.
    pub start: usize,
    pub end: usize,
}

struct Tok<'a> {
    s: &'a str,
    lower: String,
    start: usize,
    end: usize,
}

fn tokenize(text: &str) -> Vec<Tok<'_>> {
    let mut out = vec![];
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        let is_word = c.is_alphanumeric() || c == '-';
        match (start, is_word) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                let w = text[s..i].trim_end_matches('-');
                if !w.is_empty() {
                    out.push(Tok { s: w, lower: w.to_lowercase(), start: s, end: s + w.len() });
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn weekday(w: &str, original: &str, has_prefix: bool) -> Option<Weekday> {
    let full = match w {
        "monday" => Some(Weekday::Mon),
        "tuesday" => Some(Weekday::Tue),
        "wednesday" => Some(Weekday::Wed),
        "thursday" => Some(Weekday::Thu),
        "friday" => Some(Weekday::Fri),
        "saturday" => Some(Weekday::Sat),
        "sunday" => Some(Weekday::Sun),
        _ => None,
    };
    if full.is_some() {
        return full;
    }
    // Abbreviations collide with words ("sat", "sun", "wed"): require a
    // capital letter or a preceding preposition.
    let capital = original.chars().next().is_some_and(|c| c.is_uppercase());
    if !(capital || has_prefix) {
        return None;
    }
    match w {
        "mon" => Some(Weekday::Mon),
        "tue" | "tues" => Some(Weekday::Tue),
        "wed" => Some(Weekday::Wed),
        "thu" | "thur" | "thurs" => Some(Weekday::Thu),
        "fri" => Some(Weekday::Fri),
        "sat" => Some(Weekday::Sat),
        "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

fn month(w: &str) -> Option<u32> {
    const M: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    if w.len() < 3 {
        return None;
    }
    let idx = M.iter().position(|m| w.starts_with(m))?;
    const FULL: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    // accept "nov", "novem", "november", "sept" - but not random words like "mayor"
    if FULL[idx].starts_with(w) || w == "sept" { Some(idx as u32 + 1) } else { None }
}

fn day_number(w: &str) -> Option<u32> {
    let digits = w.trim_end_matches("st").trim_end_matches("nd").trim_end_matches("rd").trim_end_matches("th");
    let n: u32 = digits.parse().ok()?;
    (1..=31).contains(&n).then_some(n)
}

fn small_number(w: &str) -> Option<i64> {
    if let Ok(n) = w.parse::<i64>() {
        return (0..=365).contains(&n).then_some(n);
    }
    const WORDS: [&str; 11] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    WORDS.iter().position(|x| *x == w).map(|i| i as i64).or(match w {
        "a" | "an" => Some(1),
        "couple" => Some(2),
        _ => None,
    })
}

fn next_weekday(today: NaiveDate, wd: Weekday) -> NaiveDate {
    let diff = (wd.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64 + 7) % 7;
    today + Duration::days(if diff == 0 { 7 } else { diff })
}

fn prev_weekday(today: NaiveDate, wd: Weekday) -> NaiveDate {
    let diff = (today.weekday().num_days_from_monday() as i64 - wd.num_days_from_monday() as i64 + 7) % 7;
    today - Duration::days(if diff == 0 { 7 } else { diff })
}

fn month_day(today: NaiveDate, m: u32, d: u32, year: Option<i32>) -> Option<NaiveDate> {
    if let Some(y) = year {
        return NaiveDate::from_ymd_opt(y, m, d);
    }
    let this = NaiveDate::from_ymd_opt(today.year(), m, d)?;
    // Dates more than ~2 months in the past are assumed to mean next year.
    if this < today - Duration::days(60) { NaiveDate::from_ymd_opt(today.year() + 1, m, d) } else { Some(this) }
}

const PREFIXES: &[&str] =
    &["by", "before", "on", "until", "till", "due", "next", "this", "coming", "last", "in", "for"];

pub fn find_dates(text: &str, today: NaiveDate) -> Vec<DateMatch> {
    let toks = tokenize(text);
    let mut out = vec![];
    let mut i = 0;
    while i < toks.len() {
        let w = toks[i].lower.as_str();
        let prev = i.checked_sub(1).map(|p| toks[p].lower.as_str());
        let has_prefix = prev.is_some_and(|p| PREFIXES.contains(&p));
        let mut hit: Option<(NaiveDate, usize)> = None; // (date, last token index)

        if let Ok(d) = NaiveDate::parse_from_str(w, "%Y-%m-%d") {
            hit = Some((d, i));
        } else if w == "today" || w == "tonight" {
            hit = Some((today, i));
        } else if w == "tomorrow" {
            hit = Some((today + Duration::days(1), i));
        } else if w == "yesterday" {
            hit = Some((today - Duration::days(1), i));
        } else if let Some(wd) = weekday(w, toks[i].s, has_prefix) {
            let d = if prev == Some("last") {
                prev_weekday(today, wd)
            } else if prev == Some("this") && today.weekday() == wd {
                today
            } else {
                next_weekday(today, wd)
            };
            hit = Some((d, i));
        } else if w == "week" && matches!(prev, Some("next")) {
            hit = Some((next_weekday(today, Weekday::Mon), i));
        } else if w == "weekend" && matches!(prev, Some("this") | Some("next") | Some("the")) {
            hit = Some((if today.weekday() == Weekday::Sat { today } else { next_weekday(today, Weekday::Sat) }, i));
        } else if w == "end" && toks.get(i + 1).is_some_and(|t| t.lower == "of") {
            let j = if toks.get(i + 2).is_some_and(|t| t.lower == "the") { i + 3 } else { i + 2 };
            if toks.get(j).is_some_and(|t| t.lower == "week") {
                let d = if today.weekday() == Weekday::Sun { today } else { next_weekday(today, Weekday::Sun) };
                hit = Some((d, j));
            }
        } else if w == "in" {
            let mut j = i + 1;
            if toks.get(j).is_some_and(|t| t.lower == "a") && toks.get(j + 1).is_some_and(|t| t.lower == "couple") {
                j += 1;
            }
            if let Some(n) = toks.get(j).and_then(|t| small_number(&t.lower)) {
                let k = if toks.get(j + 1).is_some_and(|t| t.lower == "of") { j + 2 } else { j + 1 };
                match toks.get(k).map(|t| t.lower.as_str()) {
                    Some("day") | Some("days") => hit = Some((today + Duration::days(n), k)),
                    Some("week") | Some("weeks") => hit = Some((today + Duration::days(7 * n), k)),
                    _ => {}
                }
            }
        } else if let Some(m) = month(w) {
            // "Nov 20", "November 20th", "Nov 20 2027"
            if let Some(d) = toks.get(i + 1).and_then(|t| day_number(&t.lower)) {
                let year =
                    toks.get(i + 2).and_then(|t| t.lower.parse::<i32>().ok()).filter(|y| (2000..2100).contains(y));
                if let Some(date) = month_day(today, m, d, year) {
                    hit = Some((date, if year.is_some() { i + 2 } else { i + 1 }));
                }
            }
        } else if let Some(d) = day_number(w) {
            // "20 November", "20th Nov"
            let j = if toks.get(i + 1).is_some_and(|t| t.lower == "of") { i + 2 } else { i + 1 };
            if let Some(m) = toks.get(j).and_then(|t| month(&t.lower)) {
                let year =
                    toks.get(j + 1).and_then(|t| t.lower.parse::<i32>().ok()).filter(|y| (2000..2100).contains(y));
                if let Some(date) = month_day(today, m, d, year) {
                    hit = Some((date, if year.is_some() { j + 1 } else { j }));
                }
            }
        }

        if let Some((mut date, last)) = hit {
            let mut first = i;
            if w != "in" && w != "end" && has_prefix && prev != Some("for") {
                first = i - 1;
                if prev == Some("before") {
                    date -= Duration::days(1);
                }
                // "by next Friday", "before this Sunday"
                if matches!(prev, Some("next") | Some("this") | Some("coming"))
                    && i >= 2
                    && ["by", "before", "on", "until", "till", "due"].contains(&toks[i - 2].lower.as_str())
                {
                    first = i - 2;
                    if toks[i - 2].lower == "before" {
                        date -= Duration::days(1);
                    }
                }
            }
            let (start, end) = (toks[first].start, toks[last].end);
            out.push(DateMatch { phrase: text[start..end].to_string(), date, start, end });
            i = last + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// "Thu 2026-11-12" style label.
pub fn label(d: NaiveDate) -> String {
    d.format("%a %Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 7).unwrap() // Saturday
    }
    fn first(t: &str) -> NaiveDate {
        find_dates(t, today())[0].date
    }

    #[test]
    fn weekdays() {
        assert_eq!(first("exam next Thursday"), NaiveDate::from_ymd_opt(2026, 11, 12).unwrap());
        assert_eq!(first("by Sunday"), NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
        assert_eq!(first("before Sunday"), today());
        assert_eq!(first("on Saturday"), NaiveDate::from_ymd_opt(2026, 11, 14).unwrap());
        assert_eq!(first("this Saturday"), today());
        assert_eq!(first("last Monday"), NaiveDate::from_ymd_opt(2026, 11, 2).unwrap());
        assert_eq!(first("before next Friday"), NaiveDate::from_ymd_opt(2026, 11, 12).unwrap());
    }

    #[test]
    fn misc() {
        assert_eq!(first("tomorrow"), NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
        assert_eq!(first("due:2026-12-01"), NaiveDate::from_ymd_opt(2026, 12, 1).unwrap());
        assert_eq!(first("in 3 days"), NaiveDate::from_ymd_opt(2026, 11, 10).unwrap());
        assert_eq!(first("in two weeks"), NaiveDate::from_ymd_opt(2026, 11, 21).unwrap());
        assert_eq!(first("next week"), NaiveDate::from_ymd_opt(2026, 11, 9).unwrap());
        assert_eq!(first("Nov 20"), NaiveDate::from_ymd_opt(2026, 11, 20).unwrap());
        assert_eq!(first("on 3rd of January"), NaiveDate::from_ymd_opt(2027, 1, 3).unwrap());
        assert_eq!(first("end of the week"), NaiveDate::from_ymd_opt(2026, 11, 8).unwrap());
    }

    #[test]
    fn no_false_positives() {
        assert!(find_dates("I sat in the sun and may go", today()).is_empty());
        assert!(find_dates("the mayor marched", today()).is_empty());
    }

    #[test]
    fn phrase_span() {
        let m = &find_dates("finish prototype before Sunday!", today())[0];
        assert_eq!(m.phrase, "before Sunday");
    }
}
