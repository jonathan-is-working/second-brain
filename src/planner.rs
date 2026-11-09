//! Deterministic scheduling. The model never picks times: this module computes
//! feasible work blocks from free time (work hours − protected time − busy
//! calendar events − already planned sessions), ordered by earliest deadline
//! then priority score. Replanning is incremental: valid future sessions are
//! kept and only the missing work (or invalidated sessions) is re-placed.

use crate::actions::{self, Undo};
use crate::brain::Brain;
use crate::config::Window;
use crate::tasks::{self, TaskRow};
use crate::util::{parse_ts, ts};
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::params;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Iv {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Iv {
    pub fn minutes(&self) -> i64 {
        (self.end - self.start).num_minutes()
    }
    pub fn overlaps(&self, o: &Iv) -> bool {
        self.start < o.end && o.start < self.end
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub min_block: i64,
    pub max_block: i64,
    pub break_min: i64,
    pub max_daily: i64,
    pub max_per_task_day: usize,
    pub horizon_days: i64,
}

#[derive(Debug, Clone)]
pub struct PlanTask {
    pub id: i64,
    pub due: Option<NaiveDate>,
    pub need_min: i64,
    pub score: i64,
}

pub struct Input<'a> {
    pub now: DateTime<Utc>,
    pub tz: Tz,
    pub work: &'a [Window],
    pub protected: &'a [Window],
    pub busy: Vec<Iv>,
    /// Sessions that stay as they are (task id, interval).
    pub kept: Vec<(i64, Iv)>,
    pub settings: Settings,
}

#[derive(Debug, Default)]
pub struct Output {
    pub blocks: Vec<(i64, Iv)>,
    /// (task id, minutes that could not be placed before the deadline/horizon)
    pub shortfall: Vec<(i64, i64)>,
}

fn local_iv(tz: Tz, date: NaiveDate, w: &Window) -> Option<Iv> {
    let s = tz.from_local_datetime(&date.and_time(w.start)).earliest()?;
    let e = tz.from_local_datetime(&date.and_time(w.end)).latest()?;
    Some(Iv { start: s.with_timezone(&Utc), end: e.with_timezone(&Utc) })
}

pub fn day_end(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    let next = date.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap();
    tz.from_local_datetime(&next)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&next))
}

fn subtract(free: Vec<Iv>, cut: &Iv) -> Vec<Iv> {
    let mut out = vec![];
    for f in free {
        if !f.overlaps(cut) {
            out.push(f);
            continue;
        }
        if f.start < cut.start {
            out.push(Iv { start: f.start, end: cut.start });
        }
        if cut.end < f.end {
            out.push(Iv { start: cut.end, end: f.end });
        }
    }
    out
}

fn round_up_5(t: DateTime<Utc>) -> DateTime<Utc> {
    let secs = t.timestamp();
    let r = (secs + 299) / 300 * 300;
    DateTime::from_timestamp(r, 0).unwrap_or(t)
}

/// Free intervals for one local date.
pub fn free_for_day(input: &Input, date: NaiveDate) -> Vec<Iv> {
    let start_floor = round_up_5(input.now);
    let mut free: Vec<Iv> = input
        .work
        .iter()
        .filter(|w| w.day == date.weekday())
        .filter_map(|w| local_iv(input.tz, date, w))
        .filter_map(|iv| {
            let s = iv.start.max(start_floor);
            (s < iv.end).then_some(Iv { start: s, end: iv.end })
        })
        .collect();
    free.sort();
    for w in input.protected.iter().filter(|w| w.day == date.weekday()) {
        if let Some(p) = local_iv(input.tz, date, w) {
            free = subtract(free, &p);
        }
    }
    let pad = Duration::minutes(input.settings.break_min);
    for b in input.busy.iter().chain(input.kept.iter().map(|(_, iv)| iv)) {
        free = subtract(free, &Iv { start: b.start - pad, end: b.end + pad });
    }
    free
}

pub fn schedule(input: &Input, tasks: &[PlanTask]) -> Output {
    let s = &input.settings;
    let today = input.now.with_timezone(&input.tz).date_naive();
    let days: Vec<NaiveDate> = (0..s.horizon_days).map(|i| today + Duration::days(i)).collect();
    let mut free: HashMap<NaiveDate, Vec<Iv>> = days.iter().map(|d| (*d, free_for_day(input, *d))).collect();
    let mut load: HashMap<NaiveDate, i64> = HashMap::new();
    let mut per_task_day: HashMap<(i64, NaiveDate), usize> = HashMap::new();
    for (tid, iv) in &input.kept {
        let d = iv.start.with_timezone(&input.tz).date_naive();
        *load.entry(d).or_default() += iv.minutes();
        *per_task_day.entry((*tid, d)).or_default() += 1;
    }
    let horizon_end = day_end(input.tz, *days.last().unwrap_or(&today));

    let mut order: Vec<&PlanTask> = tasks.iter().filter(|t| t.need_min > 0).collect();
    order.sort_by_key(|t| (t.due.unwrap_or(NaiveDate::MAX), -t.score, t.id));

    let mut out = Output::default();
    for t in order {
        let deadline = t.due.map(|d| day_end(input.tz, d)).unwrap_or(horizon_end).min(horizon_end);
        let mut remaining = t.need_min;
        'days: for d in &days {
            if remaining <= 0 {
                break;
            }
            let Some(slots) = free.get_mut(d) else { continue };
            let mut i = 0;
            while i < slots.len() && remaining > 0 {
                let day_load = *load.get(d).unwrap_or(&0);
                let count = *per_task_day.get(&(t.id, *d)).unwrap_or(&0);
                if day_load >= s.max_daily || count >= s.max_per_task_day {
                    continue 'days;
                }
                let slot = slots[i];
                if slot.start >= deadline {
                    break 'days;
                }
                let avail = (slot.end.min(deadline) - slot.start).num_minutes();
                let mut len = remaining.min(s.max_block).min(avail).min(s.max_daily - day_load);
                // Don't leave a tail shorter than min_block: shorten this block instead
                // (unless this is the last day before the deadline, where every minute counts).
                let tail = remaining - len;
                let later_days = deadline > day_end(input.tz, *d);
                if later_days && tail > 0 && tail < s.min_block {
                    if len - (s.min_block - tail) >= s.min_block {
                        len -= s.min_block - tail;
                    } else {
                        // Too small to split well: wait for a slot that fits it whole.
                        i += 1;
                        continue;
                    }
                }
                // Never create fragments shorter than min_block unless it finishes the task.
                if len < s.min_block && len < remaining || len <= 0 {
                    i += 1;
                    continue;
                }
                let block = Iv { start: slot.start, end: slot.start + Duration::minutes(len) };
                out.blocks.push((t.id, block));
                remaining -= len;
                *load.entry(*d).or_default() += len;
                *per_task_day.entry((t.id, *d)).or_default() += 1;
                let rest_start = block.end + Duration::minutes(s.break_min);
                if rest_start < slot.end {
                    slots[i] = Iv { start: rest_start, end: slot.end };
                } else {
                    slots.remove(i);
                }
            }
        }
        if remaining > 0 {
            out.shortfall.push((t.id, remaining));
        }
    }
    out.blocks.sort_by_key(|(_, iv)| *iv);
    out
}

// ---------------------------------------------------------------------------
// Workflow: diff against stored sessions, preview, apply.

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: i64,
    pub task_id: i64,
    pub iv: Iv,
    pub status: String,
    pub gcal_id: Option<String>,
}

pub fn sessions_between(
    brain: &Brain,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    statuses: &[&str],
) -> Result<Vec<SessionRow>> {
    let mut st = brain.conn.prepare(
        "SELECT id, task_id, start, end, status, gcal_id FROM sessions WHERE end > ?1 AND start < ?2 ORDER BY start",
    )?;
    let rows: Vec<(i64, i64, String, String, String, Option<String>)> = st
        .query_map([ts(from), ts(to)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = vec![];
    for (id, task_id, s, e, status, gcal_id) in rows {
        if statuses.contains(&status.as_str()) {
            out.push(SessionRow { id, task_id, iv: Iv { start: parse_ts(&s)?, end: parse_ts(&e)? }, status, gcal_id });
        }
    }
    Ok(out)
}

pub fn busy_between(brain: &Brain, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<(Iv, Option<String>)>> {
    let mut st =
        brain.conn.prepare("SELECT start, end, title FROM busy WHERE end > ?1 AND start < ?2 ORDER BY start")?;
    let rows: Vec<(String, String, Option<String>)> = st
        .query_map([ts(from), ts(to)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter().map(|(s, e, t)| Ok((Iv { start: parse_ts(&s)?, end: parse_ts(&e)? }, t))).collect()
}

#[derive(Debug, Default)]
pub struct PlanDiff {
    pub create: Vec<(i64, Iv)>,
    pub cancel: Vec<(i64, String)>,
    pub missed: Vec<i64>,
    pub shortfall: Vec<(i64, i64)>,
    pub assumed_estimates: Vec<i64>,
}

impl PlanDiff {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty() && self.cancel.is_empty() && self.missed.is_empty()
    }
}

pub fn settings(brain: &Brain) -> Settings {
    let s = &brain.cfg.schedule;
    Settings {
        min_block: s.min_block_minutes,
        max_block: s.max_block_minutes,
        break_min: s.break_minutes,
        max_daily: s.max_focus_minutes_per_day,
        max_per_task_day: s.max_blocks_per_task_per_day,
        horizon_days: s.horizon_days,
    }
}

pub fn compute(brain: &Brain) -> Result<PlanDiff> {
    let now = brain.now();
    let today = brain.today();
    let set = settings(brain);
    let horizon_end = day_end(brain.tz, today + Duration::days(set.horizon_days));
    let default_est = brain.cfg.schedule.default_estimate_minutes;
    // Past sessions never confirmed as done are missed (work still needed).
    let missed = sessions_between(brain, now - Duration::days(30), now, &["planned"])?
        .into_iter()
        .filter(|s| s.iv.end <= now)
        .map(|s| s.id)
        .collect();
    let mut diff = PlanDiff { missed, ..Default::default() };

    let open: HashMap<i64, TaskRow> = tasks::open_tasks(brain)?
        .into_iter()
        .filter(|t| t.kind == "task" && t.status == "open")
        .filter(|t| t.due.is_some() || brain.cfg.schedule.plan_undated)
        .map(|t| (t.id, t))
        .collect();
    let busy: Vec<Iv> = busy_between(brain, now, horizon_end)?.into_iter().map(|(iv, _)| iv).collect();
    let work = brain.cfg.windows(false);
    let protected = brain.cfg.windows(true);

    // Validate future sessions.
    let future = sessions_between(brain, now, horizon_end + Duration::days(60), &["planned"])?;
    let mut kept: Vec<(i64, Iv)> = vec![];
    for s in future.into_iter().filter(|s| s.iv.start >= now) {
        let reason = match open.get(&s.task_id) {
            None => Some("task no longer open or has no due date".to_string()),
            Some(t) if t.due.is_some_and(|d| s.iv.end > day_end(brain.tz, d)) => Some("after the due date".into()),
            Some(_) if busy.iter().any(|b| b.overlaps(&s.iv)) => Some("conflicts with a calendar event".into()),
            Some(_) if kept.iter().any(|(_, k)| k.overlaps(&s.iv)) => Some("overlaps another session".into()),
            Some(_)
                if protected.iter().any(|w| {
                    let d = s.iv.start.with_timezone(&brain.tz).date_naive();
                    w.day == d.weekday() && local_iv(brain.tz, d, w).is_some_and(|p| p.overlaps(&s.iv))
                }) =>
            {
                Some("inside protected time".into())
            }
            _ => None,
        };
        match reason {
            Some(r) => diff.cancel.push((s.id, r)),
            None => kept.push((s.task_id, s.iv)),
        }
    }

    // Surplus sessions (estimate lowered / work done): cancel the latest ones.
    let mut plan_tasks = vec![];
    for t in open.values() {
        let mut mine: Vec<(usize, Iv)> =
            kept.iter().enumerate().filter(|(_, (tid, _))| *tid == t.id).map(|(i, (_, iv))| (i, *iv)).collect();
        let planned: i64 = mine.iter().map(|(_, iv)| iv.minutes()).sum();
        let mut need = t.remaining(default_est) - planned;
        if need < 0 {
            mine.sort_by_key(|(_, iv)| std::cmp::Reverse(*iv));
            for (_, iv) in mine {
                if -need >= iv.minutes() {
                    if let Some(pos) = kept.iter().position(|(tid, k)| *tid == t.id && *k == iv) {
                        kept.remove(pos);
                    }
                    let id: i64 = brain.conn.query_row(
                        "SELECT id FROM sessions WHERE task_id=?1 AND start=?2 AND status='planned'",
                        params![t.id, ts(iv.start)],
                        |r| r.get(0),
                    )?;
                    diff.cancel.push((id, "more time planned than needed".into()));
                    need += iv.minutes();
                }
            }
        }
        if t.estimate_assumed() {
            diff.assumed_estimates.push(t.id);
        }
        if need >= 10 {
            plan_tasks.push(PlanTask {
                id: t.id,
                due: t.due,
                need_min: need,
                score: tasks::score(t, today, default_est),
            });
        }
    }
    plan_tasks.sort_by_key(|t| t.id);
    let input = Input { now, tz: brain.tz, work: &work, protected: &protected, busy, kept, settings: set };
    let out = schedule(&input, &plan_tasks);
    diff.create = out.blocks;
    diff.shortfall = out.shortfall;
    diff.assumed_estimates.sort();
    Ok(diff)
}

/// Apply a computed plan to the database (calendar push happens separately).
pub fn apply(brain: &Brain, diff: &PlanDiff, actor: &str) -> Result<Vec<i64>> {
    let tx = brain.conn.unchecked_transaction()?;
    for id in &diff.missed {
        tx.execute("UPDATE sessions SET status='missed' WHERE id=?1 AND status='planned'", [id])?;
    }
    if !diff.missed.is_empty() {
        actions::log(
            &tx,
            actor,
            "sessions_missed",
            &format!("{} past session(s) marked missed", diff.missed.len()),
            None,
        )?;
    }
    let cancel_ids: Vec<i64> = diff.cancel.iter().map(|(id, _)| *id).collect();
    for id in &cancel_ids {
        tx.execute("UPDATE sessions SET status='cancelled' WHERE id=?1 AND status='planned'", [id])?;
    }
    if !cancel_ids.is_empty() {
        let why: Vec<String> = diff.cancel.iter().map(|(id, r)| format!("#{id}: {r}")).collect();
        actions::log(
            &tx,
            actor,
            "sessions_cancelled",
            &why.join("; "),
            Some(Undo::RestoreSessions { ids: cancel_ids }),
        )?;
    }
    let mut created = vec![];
    for (tid, iv) in &diff.create {
        tx.execute(
            "INSERT INTO sessions(task_id, start, end, status, created_at) VALUES (?1, ?2, ?3, 'planned', ?4)",
            params![tid, ts(iv.start), ts(iv.end), ts(Utc::now())],
        )?;
        created.push(tx.last_insert_rowid());
    }
    if !created.is_empty() {
        actions::log(
            &tx,
            actor,
            "sessions_planned",
            &format!("{} work session(s) planned", created.len()),
            Some(Undo::CancelSessions { ids: created.clone() }),
        )?;
    }
    tx.commit()?;
    Ok(created)
}

pub fn task_title(brain: &Brain, id: i64) -> String {
    brain
        .conn
        .query_row("SELECT title FROM tasks WHERE id=?1", [id], |r| r.get(0))
        .unwrap_or_else(|_| format!("task #{id}"))
}

pub fn fmt_iv(brain: &Brain, iv: &Iv) -> String {
    let s = brain.local(iv.start);
    let e = brain.local(iv.end);
    format!("{} {}-{}", s.format("%a %b %d"), s.format("%H:%M"), e.format("%H:%M"))
}

pub fn diff_text(brain: &Brain, diff: &PlanDiff) -> String {
    let mut s = String::new();
    for id in &diff.missed {
        s.push_str(&format!("  ! session #{id} was missed (work will be re-planned)\n"));
    }
    for (id, why) in &diff.cancel {
        s.push_str(&format!("  - cancel session #{id}: {why}\n"));
    }
    for (tid, iv) in &diff.create {
        s.push_str(&format!("  + {}  {} (#{tid})\n", fmt_iv(brain, iv), task_title(brain, *tid)));
    }
    for (tid, m) in &diff.shortfall {
        s.push_str(&format!(
            "  ⚠ #{tid} {}: {} min could not be placed before its deadline\n",
            task_title(brain, *tid),
            m
        ));
    }
    if !diff.assumed_estimates.is_empty() {
        let ids: Vec<String> = diff.assumed_estimates.iter().map(|i| format!("#{i}")).collect();
        s.push_str(&format!(
            "  ℹ default estimate ({} min) assumed for {} - set real ones with `sb task edit <id> --est 90`\n",
            brain.cfg.schedule.default_estimate_minutes,
            ids.join(", ")
        ));
    }
    if s.is_empty() {
        s.push_str("  plan is up to date - nothing to change\n");
    }
    s
}

/// Free working minutes on a date (after busy time, protected time and sessions).
pub fn free_minutes(brain: &Brain, date: NaiveDate) -> Result<i64> {
    let from = brain.day_start(date);
    let to = day_end(brain.tz, date);
    let busy: Vec<Iv> = busy_between(brain, from, to)?.into_iter().map(|(iv, _)| iv).collect();
    let kept: Vec<(i64, Iv)> =
        sessions_between(brain, from, to, &["planned", "done"])?.into_iter().map(|s| (s.task_id, s.iv)).collect();
    let work = brain.cfg.windows(false);
    let protected = brain.cfg.windows(true);
    let now = if date == brain.today() { brain.now() } else { from };
    let input = Input {
        now,
        tz: brain.tz,
        work: &work,
        protected: &protected,
        busy,
        kept,
        settings: Settings { break_min: 0, ..settings(brain) },
    };
    Ok(free_for_day(&input, date).iter().map(|i| i.minutes()).sum())
}

/// Human-readable agenda for a date: calendar busy time, sessions, deadlines.
pub fn agenda_text(brain: &Brain, date: NaiveDate) -> Result<String> {
    let from = brain.day_start(date);
    let to = day_end(brain.tz, date);
    let mut entries: Vec<(DateTime<Utc>, String)> = vec![];
    for (iv, title) in busy_between(brain, from, to)? {
        entries.push((
            iv.start,
            format!(
                "{}-{}  ▪ {}",
                brain.local(iv.start).format("%H:%M"),
                brain.local(iv.end).format("%H:%M"),
                title.unwrap_or("busy".into())
            ),
        ));
    }
    for s in sessions_between(brain, from, to, &["planned", "done", "missed"])? {
        let mark = match s.status.as_str() {
            "done" => "✔",
            "missed" => "✗",
            _ => "◆",
        };
        entries.push((
            s.iv.start,
            format!(
                "{}-{}  {mark} focus: {} (session #{})",
                brain.local(s.iv.start).format("%H:%M"),
                brain.local(s.iv.end).format("%H:%M"),
                task_title(brain, s.task_id),
                s.id
            ),
        ));
    }
    entries.sort();
    let mut out = format!("{}\n", date.format("%A %Y-%m-%d"));
    if entries.is_empty() {
        out.push_str("  (nothing scheduled)\n");
    }
    for (_, e) in entries {
        out.push_str(&format!("  {e}\n"));
    }
    let due: Vec<TaskRow> = tasks::open_tasks(brain)?.into_iter().filter(|t| t.due == Some(date)).collect();
    for t in due {
        out.push_str(&format!("  {} due: {} (#{})\n", if t.kind == "deadline" { "📅" } else { "⏰" }, t.title, t.id));
    }
    out.push_str(&format!("  free focus time left: {}\n", tasks::fmt_min(free_minutes(brain, date)?)));
    Ok(out)
}

pub fn mark_session(brain: &Brain, id: i64, status: &str) -> Result<String> {
    let n = brain
        .conn
        .execute("UPDATE sessions SET status=?2 WHERE id=?1 AND status IN ('planned','missed')", params![id, status])?;
    if n == 0 {
        anyhow::bail!("no planned/missed session #{id}");
    }
    let msg = format!("session #{id} marked {status}");
    actions::log(&brain.conn, "user", "session_status", &msg, None)?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_window;

    fn t(h: u32, m: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, h, m, 0).unwrap()
    }
    fn settings() -> Settings {
        Settings { min_block: 30, max_block: 90, break_min: 10, max_daily: 240, max_per_task_day: 2, horizon_days: 7 }
    }

    #[test]
    fn no_overlaps_respects_busy_and_deadline() {
        let work = parse_window("mon-fri 09:00-12:00").unwrap();
        let protected = vec![];
        // Monday 2026-11-09, 08:00 UTC
        let input = Input {
            now: t(8, 0, 9),
            tz: chrono_tz::UTC,
            work: &work,
            protected: &protected,
            busy: vec![Iv { start: t(10, 0, 9), end: t(11, 0, 9) }],
            kept: vec![],
            settings: settings(),
        };
        let tasks = vec![
            PlanTask { id: 1, due: NaiveDate::from_ymd_opt(2026, 11, 9), need_min: 120, score: 10 },
            PlanTask { id: 2, due: None, need_min: 200, score: 50 },
        ];
        let out = schedule(&input, &tasks);
        for (i, a) in out.blocks.iter().enumerate() {
            assert!(!a.1.overlaps(&input.busy[0]), "overlaps busy: {a:?}");
            for b in &out.blocks[i + 1..] {
                assert!(!a.1.overlaps(&b.1));
            }
            assert!(a.1.minutes() >= 30 && a.1.minutes() <= 90);
        }
        // Task 1 due Monday: Monday has 9:00-9:50 and 11:10-12:00 free → 100 min max.
        let t1: i64 = out.blocks.iter().filter(|b| b.0 == 1).map(|b| b.1.minutes()).sum();
        assert_eq!(t1, 100);
        assert_eq!(out.shortfall, vec![(1, 20)]);
        assert!(out.blocks.iter().filter(|b| b.0 == 1).all(|b| b.1.end <= t(0, 0, 10)));
        let t2: i64 = out.blocks.iter().filter(|b| b.0 == 2).map(|b| b.1.minutes()).sum();
        assert_eq!(t2, 200);
    }

    #[test]
    fn workload_cap_and_protected_time() {
        let work = parse_window("daily 08:00-20:00").unwrap();
        let protected = parse_window("daily 12:00-14:00").unwrap();
        let input = Input {
            now: t(7, 0, 9),
            tz: chrono_tz::UTC,
            work: &work,
            protected: &protected,
            busy: vec![],
            kept: vec![(9, Iv { start: t(8, 0, 9), end: t(9, 0, 9) })],
            settings: Settings { max_daily: 180, ..settings() },
        };
        let out = schedule(&input, &[PlanTask { id: 1, due: None, need_min: 600, score: 1 }]);
        let mut per_day: HashMap<NaiveDate, i64> = HashMap::new();
        for (_, iv) in &out.blocks {
            assert!(!(iv.start < t(14, 0, iv.start.day()) && iv.end > t(12, 0, iv.start.day())), "protected: {iv:?}");
            assert!(!iv.overlaps(&Iv { start: t(8, 0, 9), end: t(9, 10, 9) }));
            *per_day.entry(iv.start.date_naive()).or_default() += iv.minutes();
        }
        // Monday already has 60 min kept → at most 120 more.
        assert!(per_day[&NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()] <= 120);
        assert!(per_day.values().all(|m| *m <= 180));
    }

    #[test]
    fn small_remainders_finish_tasks() {
        let work = parse_window("mon 09:00-10:00").unwrap();
        let input = Input {
            now: t(8, 0, 9),
            tz: chrono_tz::UTC,
            work: &work,
            protected: &[],
            busy: vec![],
            kept: vec![],
            settings: settings(),
        };
        let out = schedule(&input, &[PlanTask { id: 1, due: None, need_min: 15, score: 1 }]);
        assert_eq!(out.blocks.len(), 1);
        assert_eq!(out.blocks[0].1.minutes(), 15);
    }
}
