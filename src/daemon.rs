//! Event-driven background process. One thread, blocking on filesystem events
//! with a timeout; timers are checked on wake-up and persisted in SQLite so a
//! restart neither repeats nor skips daily jobs. The model is only invoked by
//! ingestion of *changed* journal blocks (and optional consolidation).

use crate::brain::Brain;
use crate::config::parse_hhmm;
use crate::db::{kv_get, kv_set};
use crate::util::{parse_ts, sha256_hex, ts};
use crate::{briefing, calendar, consolidate, ingest, planner};
use anyhow::{Result, bail};
use chrono::Duration;
use notify::{RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

/// What changed, so only the relevant downstream workflows run.
#[derive(Debug, Default)]
pub struct Outcome {
    pub replan: bool,
    pub messages: Vec<String>,
}

/// Plan workflow honouring the autonomy policy.
pub fn replan(brain: &Brain, be: Option<&calendar::Backend>) -> Result<Vec<String>> {
    let diff = planner::compute(brain)?;
    if diff.is_empty() {
        return Ok(vec![]);
    }
    if brain.cfg.autonomy.calendar_writes == "auto" {
        let created = planner::apply(brain, &diff, "system")?;
        let mut msgs = vec![format!("Plan updated: {} new session(s), {} cancelled", created.len(), diff.cancel.len())];
        if let Some(be) = be {
            match calendar::sync(brain, be, true) {
                Ok(r) => msgs.push(format!("Calendar: {} created, {} removed", r.pushed, r.removed)),
                Err(e) => msgs.push(format!("Calendar push failed, will retry: {e:#}")),
            }
        }
        Ok(msgs)
    } else {
        // Dry-run policy: propose once per distinct plan.
        let text = planner::diff_text(brain, &diff);
        let fp = sha256_hex(&text);
        if kv_get(&brain.conn, "proposed_plan_fp")?.as_deref() == Some(fp.as_str()) {
            return Ok(vec![]);
        }
        kv_set(&brain.conn, "proposed_plan_fp", &fp)?;
        Ok(vec![format!(
            "Plan proposal ({} new, {} cancelled) - review `sb plan`, apply `sb plan --apply`",
            diff.create.len(),
            diff.cancel.len()
        )])
    }
}

/// Keep Google in step with the brain: two-way Google Tasks sync, then (after a
/// local change, in `auto` mode) re-plan and push work sessions to the calendar.
pub fn sync_google(brain: &Brain, be: &calendar::Backend, changed: bool) -> Result<Vec<String>> {
    let mut msgs = vec![];
    let mut changed = changed;
    if brain.cfg.calendar.tasks_sync {
        let r = crate::gtasks::sync(brain, be)?;
        if r.changes() > 0 {
            msgs.push(r.summary());
        }
        changed |= r.done_remote + r.reopened_remote + r.edited_remote + r.deleted_remote + r.imported > 0;
    }
    if changed && brain.cfg.autonomy.calendar_writes == "auto" {
        let planned = replan(brain, Some(be))?;
        if planned.is_empty() {
            // Nothing to re-plan, but cancelled sessions may still need removing.
            let r = calendar::sync(brain, be, true)?;
            if r.pushed + r.removed > 0 {
                msgs.push(format!("Calendar: {} created, {} removed", r.pushed, r.removed));
            }
        }
        msgs.extend(planned);
    }
    Ok(msgs)
}

fn lease(brain: &Brain) -> Result<()> {
    let me = std::process::id().to_string();
    if let (Some(hb), Some(pid)) = (kv_get(&brain.conn, "daemon_heartbeat")?, kv_get(&brain.conn, "daemon_pid")?)
        && pid != me
        && parse_ts(&hb).is_ok_and(|t| chrono::Utc::now() - t < Duration::seconds(150))
    {
        bail!("another daemon (pid {pid}) is running; refusing to start a duplicate");
    }
    kv_set(&brain.conn, "daemon_pid", &me)?;
    kv_set(&brain.conn, "daemon_heartbeat", &ts(chrono::Utc::now()))?;
    Ok(())
}

fn due_today(brain: &Brain, key: &str, at: &str) -> Result<bool> {
    let now_local = brain.local(brain.now());
    let today = now_local.date_naive().to_string();
    Ok(now_local.time() >= parse_hhmm(at)? && kv_get(&brain.conn, key)?.as_deref() != Some(today.as_str()))
}

/// Run until SIGINT/SIGTERM (installs the signal handler itself).
pub fn run(brain: &Brain) -> Result<()> {
    lease(brain)?;
    let journal_dir = brain.vault().join("journal");
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let stop = Arc::new(AtomicBool::new(false));
    {
        // Wake the loop immediately on Ctrl-C/SIGTERM via an empty path.
        let (stop, wake) = (stop.clone(), tx.clone());
        ctrlc::set_handler(move || {
            stop.store(true, Ordering::SeqCst);
            let _ = wake.send(PathBuf::new());
        })?;
    }
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && matches!(ev.kind, notify::EventKind::Create(_) | notify::EventKind::Modify(_))
        {
            for p in ev.paths {
                let _ = tx.send(p);
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("file watcher unavailable ({e}); falling back to polling every 30s");
            None
        }
    };
    if let Some(w) = watcher.as_mut()
        && let Err(e) = w.watch(&journal_dir, RecursiveMode::Recursive)
    {
        tracing::warn!("cannot watch {} ({e}); polling instead", journal_dir.display());
        watcher = None;
    }
    let be = calendar::Backend::google(brain).unwrap_or_else(|e| {
        tracing::warn!("calendar disabled: {e:#}");
        None
    });
    tracing::info!("daemon started (pid {}), watching {}", std::process::id(), journal_dir.display());

    // Catch up on anything that changed while we were down (+ retry pending blocks).
    let mut out = Outcome::default();
    after_ingest(brain, ingest::ingest_all(brain)?, &mut out);
    let mut last_poll = Instant::now();
    let mut last_hourly = Instant::now() - std::time::Duration::from_secs(3600);
    let mut last_cal = Instant::now() - std::time::Duration::from_secs(86_400);
    let mut last_heartbeat = Instant::now();
    let mut dirty: BTreeSet<PathBuf> = BTreeSet::new();
    let mut last_event = Instant::now();
    let debounce = std::time::Duration::from_secs(brain.cfg.autonomy.debounce_secs);

    while !stop.load(Ordering::SeqCst) {
        // Sleep until an event arrives or the next timer tick (cheap when idle).
        match rx.recv_timeout(std::time::Duration::from_secs(if dirty.is_empty() { 20 } else { 1 })) {
            Ok(p) if p.as_os_str().is_empty() => continue,
            Ok(p) => {
                if p.extension().is_some_and(|x| x == "md")
                    && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
                {
                    dirty.insert(p);
                    last_event = Instant::now();
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                watcher = None;
            }
        }

        if !dirty.is_empty() && last_event.elapsed() >= debounce {
            let paths: Vec<PathBuf> = std::mem::take(&mut dirty).into_iter().filter(|p| p.exists()).collect();
            match ingest::ingest_paths(brain, &paths) {
                Ok(r) => after_ingest(brain, r, &mut out),
                Err(e) => tracing::error!("ingest failed: {e:#}"),
            }
        }
        if watcher.is_none() && last_poll.elapsed().as_secs() >= 30 {
            last_poll = Instant::now();
            match ingest::ingest_all(brain) {
                Ok(r) => after_ingest(brain, r, &mut out),
                Err(e) => tracing::error!("poll ingest failed: {e:#}"),
            }
        }

        if let Some(be) = &be
            && last_cal.elapsed().as_secs() >= brain.cfg.autonomy.calendar_sync_minutes * 60
        {
            last_cal = Instant::now();
            match calendar::sync(brain, be, brain.cfg.autonomy.calendar_writes == "auto") {
                Ok(r) => {
                    if r.busy_changed || r.user_deleted > 0 || r.user_moved > 0 {
                        out.replan = true;
                    }
                }
                Err(e) => tracing::warn!("calendar sync failed (will retry): {e:#}"),
            }
            if brain.cfg.calendar.tasks_sync {
                match crate::gtasks::sync(brain, be) {
                    Ok(r) if r.changes() > 0 => {
                        tracing::info!("{}", r.summary());
                        out.replan = true;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("Google Tasks sync failed (will retry): {e:#}"),
                }
            }
        }

        if last_hourly.elapsed().as_secs() >= 3600 {
            last_hourly = Instant::now();
            // Syntheses: idempotent and free when nothing changed.
            match crate::synth::scheduled(brain) {
                Ok(paths) => out.messages.extend(paths.iter().map(|p| format!("Synthesis ready: {}", p.display()))),
                Err(e) => tracing::warn!("synthesis failed: {e:#}"),
            }
            // Missed sessions & new pending work trigger a replan; alerts are de-duplicated.
            if !planner::compute(brain)?.missed.is_empty() {
                out.replan = true;
            }
            out.messages.extend(briefing::alerts(brain)?);
            let pending: i64 =
                brain
                    .conn
                    .query_row("SELECT count(*) FROM blocks WHERE status='pending' AND present=1", [], |r| r.get(0))?;
            if pending > 0 {
                let mut rep = ingest::IngestReport::default();
                if ingest::process_pending(brain, &mut rep).is_ok() {
                    rep.notes_written = crate::notes::regenerate_dirty(brain).unwrap_or(0);
                    after_ingest(brain, rep, &mut out);
                }
            }
        }

        if due_today(brain, "last_briefing_date", &brain.cfg.autonomy.briefing_time)? {
            match briefing::generate(brain, brain.today(), false) {
                Ok((p, _)) => out.messages.push(format!("Daily briefing ready: {}", p.display())),
                Err(e) => tracing::error!("briefing failed: {e:#}"),
            }
        }
        if due_today(brain, "last_maintenance_date", &brain.cfg.autonomy.maintenance_time)? {
            match consolidate::run(brain, true) {
                Ok(r) => tracing::info!("maintenance: {r:?}"),
                Err(e) => {
                    tracing::error!("maintenance failed: {e:#}");
                    kv_set(&brain.conn, "last_maintenance_date", &brain.today().to_string())?;
                }
            }
        }

        if out.replan {
            out.replan = false;
            match replan(brain, be.as_ref()) {
                Ok(m) => out.messages.extend(m),
                Err(e) => tracing::error!("replan failed: {e:#}"),
            }
            // New or changed tasks reach the phone right away, not at the next 15-min tick.
            if let Some(be) = &be
                && brain.cfg.calendar.tasks_sync
                && let Err(e) = crate::gtasks::sync(brain, be)
            {
                tracing::warn!("Google Tasks sync failed (will retry): {e:#}");
            }
        }
        if !out.messages.is_empty() {
            briefing::notify(brain, &std::mem::take(&mut out.messages));
        }
        if last_heartbeat.elapsed().as_secs() >= 60 {
            last_heartbeat = Instant::now();
            kv_set(&brain.conn, "daemon_heartbeat", &ts(chrono::Utc::now()))?;
        }
    }
    kv_set(&brain.conn, "daemon_heartbeat", "1970-01-01T00:00:00Z")?;
    tracing::info!("daemon stopped cleanly");
    Ok(())
}

fn after_ingest(brain: &Brain, r: ingest::IngestReport, out: &mut Outcome) {
    if r.files_changed == 0 && r.llm_calls == 0 && r.stats.tasks_created == 0 {
        return;
    }
    if brain.cfg.ingest.auto_connect && r.stats.entities_created > 0 {
        match crate::connect::run(brain, false) {
            Ok(c) if !c.skipped => {
                tracing::info!("connected ideas: {} link(s), {} placed in themes", c.links, c.placed)
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("connection pass failed: {e:#}"),
        }
    }
    tracing::info!(
        "ingested {} changed file(s): {} new blocks, {} model call(s), {} task(s) created, {} updated, {} memories",
        r.files_changed,
        r.new_blocks,
        r.llm_calls,
        r.stats.tasks_created,
        r.stats.tasks_updated,
        r.stats.memories_created
    );
    for w in &r.warnings {
        tracing::warn!("{w}");
    }
    if r.stats.tasks_created + r.stats.tasks_updated > 0 {
        out.replan = true;
    }
    out.messages.extend(r.stats.ignored.iter().map(|s| format!("Note: {s}")));
}
