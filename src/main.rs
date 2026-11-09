use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use second_brain::ui::Spinner;
use second_brain::{
    Brain, actions, briefing, calendar, connect, consolidate, daemon, graph, ingest, planner, retrieval, tasks,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sb", version, about = "Autonomous, token-efficient second brain")]
struct Cli {
    /// Second Brain home (vault + data + config). Env: SB_HOME. Default: ~/second-brain
    #[arg(long, global = true, env = "SB_HOME")]
    home: Option<PathBuf>,
    /// Verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the vault layout, config and database
    Init,
    /// Print (and create from template) the journal file for today
    Journal,
    /// Process changed journals now (deterministic + batched model extraction)
    Ingest {
        /// Specific files (default: all journals; unchanged files are skipped)
        paths: Vec<PathBuf>,
        /// Use the heuristic extractor instead of the model
        #[arg(long)]
        offline: bool,
    },
    /// LLM synthesis of a day, week or month (built from the level below)
    Synth {
        /// day | week | month
        #[arg(default_value = "day")]
        period: String,
        /// Any date inside the period (YYYY-MM-DD or "yesterday", "last monday"…)
        #[arg(long)]
        date: Option<String>,
        /// Regenerate even if nothing changed
        #[arg(long)]
        force: bool,
        /// Print Markdown in the terminal instead of opening the reading page
        #[arg(long)]
        print: bool,
    },
    /// Show the theme tree: grand themes › themes › items
    Themes,
    /// Forget all learned knowledge and re-read journals from scratch (keeps journals, tasks, calendar)
    Reset {
        /// Confirm
        #[arg(long)]
        yes: bool,
    },
    /// List the interface themes, or apply one (dashboard + nvim): sb theme tokyonight-night
    Theme { name: Option<String> },
    /// Terminal dashboard: agenda, tasks, deadlines, recent knowledge, themes (keyboard-driven)
    Dash {
        /// Print one frame of tab N as text instead of starting the interactive view
        #[arg(long, hide = true)]
        preview: Option<u8>,
    },
    /// Add an event from plain text with one small model call: sb event "hackathon le 20 et 21 nov"
    Event {
        text: Vec<String>,
        /// Target calendar: "primary" (Mon agenda) or "sb" (Second Brain). Default: primary when allowed
        #[arg(long)]
        calendar: Option<String>,
        /// Show what would be added without writing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the connection(s) between two items (undoable with `sb undo`)
    Unlink { a: String, b: String },
    /// Let the model reflect on everything known and link what belongs together
    Connect {
        /// Re-examine the whole graph even if nothing is new
        #[arg(long)]
        full: bool,
    },
    /// Forget what was learned from a journal and extract it again
    Reprocess { path: PathBuf },
    /// Run the background watcher/scheduler
    Daemon,
    /// Full-text search over memories, notes, tasks and journals (no model)
    Search {
        query: Vec<String>,
        #[arg(short, long, default_value_t = 8)]
        limit: usize,
    },
    /// Ask a question; structured questions are answered without a model
    Ask { question: Vec<String> },
    /// List tasks by deterministic priority
    Tasks {
        /// Include done/cancelled
        #[arg(long)]
        all: bool,
        /// Only deadlines in the next N days
        #[arg(long)]
        due: Option<i64>,
    },
    /// Task commands
    #[command(subcommand)]
    Task(TaskCmd),
    /// Show the agenda for today (or --date / tomorrow)
    Today {
        #[arg(long)]
        date: Option<String>,
    },
    /// Compute a schedule; shows a dry-run unless --apply
    Plan {
        #[arg(long)]
        apply: bool,
    },
    /// Generate the daily briefing into vault/summaries
    Briefing {
        #[arg(long)]
        date: Option<String>,
        /// Add a 2-3 sentence model-written focus note (1 call)
        #[arg(long)]
        llm: bool,
    },
    /// Mark a work session done or missed
    Session {
        id: i64,
        #[arg(value_parser = ["done", "missed"])]
        status: String,
    },
    /// Google Calendar
    #[command(subcommand)]
    Calendar(CalCmd),
    /// Inspect memory
    Memory {
        /// profile | episodic | knowledge | decision | idea | goal
        #[arg(long)]
        kind: Option<String>,
        /// Retract a memory by id (explicit correction)
        #[arg(long)]
        retract: Option<i64>,
        #[arg(short, long, default_value_t = 30)]
        limit: usize,
    },
    /// Activity log of autonomous and manual actions
    Log {
        #[arg(short, long, default_value_t = 25)]
        limit: usize,
    },
    /// Undo an action from the log
    Undo { id: i64 },
    /// Memory maintenance: dedupe, repair notes, summaries, backup
    Consolidate {
        #[arg(long)]
        no_llm: bool,
    },
    /// Interactive knowledge graph (like Obsidian's) as a local HTML page
    Graph {
        /// Only write the file, don't open the browser
        #[arg(long)]
        no_open: bool,
        /// Output path (default: data/graph.html)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Token usage and efficiency statistics
    Stats,
    /// Health checks
    Doctor,
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Add a task: "Write report by Friday ~2h !! +project"
    Add { text: Vec<String> },
    /// Mark task <id> done (undo: task reopen <id>, or sb undo)
    Done { id: i64 },
    /// Cancel task <id>
    Cancel { id: i64 },
    /// Reopen a done or cancelled task <id>
    Reopen { id: i64 },
    /// Turn a proposed ("maybe") task into a commitment
    Confirm { id: i64 },
    /// Edit task <id>: --title, --due, --est (minutes), --priority 1-4, --project
    Edit {
        id: i64,
        #[arg(long)]
        title: Option<String>,
        /// YYYY-MM-DD, a phrase like "next friday", or "none"
        #[arg(long)]
        due: Option<String>,
        /// minutes
        #[arg(long)]
        est: Option<i64>,
        /// 1 (urgent) .. 4 (low)
        #[arg(long)]
        priority: Option<i64>,
        #[arg(long)]
        project: Option<String>,
        /// Description (Markdown); "" removes it. Use `-` to type it in $EDITOR
        #[arg(long)]
        desc: Option<String>,
    },
    /// Show everything about task <id>: description, dates, sessions, source
    Show { id: i64 },
    /// Delete task <id> for good (also from Google); `sb undo` brings it back
    Delete { id: i64 },
}

#[derive(Subcommand)]
enum CalCmd {
    /// Connect Google Calendar (guided one-time setup, then browser login)
    Auth {
        /// Forget the stored OAuth client and run the setup again
        #[arg(long)]
        reset: bool,
        /// Also allow reading/deleting events in your own calendars (broad access, opt-in)
        #[arg(long)]
        events: bool,
    },
    /// List events of your calendar in a period (needs `auth --events`)
    Events {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        /// Only events whose title contains this (case/accent-insensitive)
        #[arg(long = "match")]
        pattern: Option<String>,
        #[arg(long, default_value = "primary")]
        calendar: String,
    },
    /// Delete events matching a title in a period: lists first, deletes with --yes (saved for restore)
    Delete {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long = "match")]
        pattern: String,
        #[arg(long, default_value = "primary")]
        calendar: String,
        #[arg(long)]
        yes: bool,
    },
    /// Recreate events saved by `delete`
    Restore { file: std::path::PathBuf },
    /// Add an all-day period (e.g. "Vacation") to the Second Brain calendar; no work is planned then
    Block {
        title: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        /// Monday to Friday only
        #[arg(long)]
        weekdays: bool,
    },
    /// Pull busy time & edits; push planned sessions with --push
    Sync {
        #[arg(long)]
        push: bool,
    },
}

fn main() {
    // Behave like a normal Unix CLI when piped into `head` etc. (no panic on EPIPE).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let filter = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("SB_LOG").unwrap_or_else(|_| filter.into()))
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn home(cli: &Cli) -> Result<PathBuf> {
    match &cli.home {
        Some(h) => Ok(h.clone()),
        None => Ok(PathBuf::from(std::env::var("HOME").context("HOME not set")?).join("second-brain")),
    }
}

fn parse_day(brain: &Brain, s: Option<&str>) -> Result<chrono::NaiveDate> {
    match s {
        None => Ok(brain.today()),
        Some(s) => second_brain::util::parse_date(s)
            .or_else(|| second_brain::dates::find_dates(s, brain.today()).first().map(|m| m.date))
            .with_context(|| format!("can't understand date '{s}'")),
    }
}

fn print_ingest(r: &ingest::IngestReport) {
    println!(
        "{} file(s) scanned, {} changed · {} new block(s), {} removed · {} deterministic, {} skipped · {} model call(s)",
        r.files_scanned,
        r.files_changed,
        r.new_blocks,
        r.removed_blocks,
        r.deterministic_blocks,
        r.skipped_blocks,
        r.llm_calls
    );
    let s = &r.stats;
    println!(
        "tasks: +{} created, {} updated · memories: +{} ({} superseded, {} duplicates skipped) · entities: +{} · notes written: {}",
        s.tasks_created,
        s.tasks_updated,
        s.memories_created,
        s.superseded,
        s.duplicates_skipped,
        s.entities_created,
        r.notes_written
    );
    if r.pending_left > 0 {
        println!("{} block(s) still pending (will retry)", r.pending_left);
    }
    for i in &s.ignored {
        println!("note: {i}");
    }
    for w in &r.warnings {
        println!("warning: {w}");
    }
    for x in &r.rejected {
        println!("rejected: {x}");
    }
}

fn run(cli: Cli) -> Result<()> {
    let home = home(&cli)?;
    if let Cmd::Init = cli.cmd {
        Brain::init(&home)?;
        println!("Initialized second brain at {}", home.display());
        println!("Edit {}/config.toml, then write in {}/vault/journal/YYYY-MM-DD.md", home.display(), home.display());
        return Ok(());
    }
    let mut brain = Brain::open(&home)?;
    let today = brain.today();
    match cli.cmd {
        Cmd::Init => unreachable!(),
        Cmd::Journal => {
            let p = brain.vault().join(format!("journal/{today}.md"));
            if !p.exists() {
                second_brain::util::atomic_write(&p, &format!("# {}\n\n", today.format("%A %d %B %Y")))?;
            }
            println!("{}", p.display());
        }
        Cmd::Ingest { paths, offline } => {
            if offline {
                brain.cfg.ingest.llm_extraction = false;
            }
            let sp = Spinner::start("Reading journals");
            let r = if paths.is_empty() { ingest::ingest_all(&brain)? } else { ingest::ingest_paths(&brain, &paths)? };
            sp.done(&format!("Ingested ({} model call(s))", r.llm_calls));
            print_ingest(&r);
            auto_connect(&brain, r.stats.entities_created);
            if r.stats.tasks_created + r.stats.tasks_updated > 0 {
                google(&brain, true);
            }
        }
        Cmd::Daemon => daemon::run(&brain)?,
        Cmd::Synth { period, date, force, print } => {
            let p = second_brain::synth::Period::parse(&period)?;
            let d = parse_day(&brain, date.as_deref())?;
            let sp = Spinner::start("Synthesizing");
            let out = second_brain::synth::generate(&brain, p, d, force)?;
            drop(sp);
            use second_brain::synth::Outcome;
            let (md, note) = match out {
                Outcome::Written { path, markdown, calls } => {
                    (markdown, format!("written to {} ({calls} model call(s))", path.display()))
                }
                Outcome::Unchanged { path, markdown } => (
                    markdown,
                    format!("unchanged since last time - 0 model calls (--force to rewrite) · {}", path.display()),
                ),
                Outcome::Empty => {
                    println!("nothing captured in that period");
                    return Ok(());
                }
            };
            if print {
                println!("{md}\n\n{note}");
            } else {
                let page = second_brain::synth_page::write(&brain, Some((p, d)))?;
                println!("{note}\nopened {}", page.display());
                graph::open(&page);
            }
        }
        Cmd::Themes => {
            let tax = connect::taxonomy(&brain.conn)?;
            let ents = connect::entities(&brain.conn)?;
            // "To explore" resources hang from hubs (themes/areas) as memories.
            let mut res: std::collections::HashMap<String, Vec<String>> = Default::default();
            let mut st = brain.conn.prepare(
                "SELECT e.name, m.content FROM memories m JOIN memory_entities me ON me.memory_id=m.id
                 JOIN entities e ON e.id=me.entity_id WHERE m.kind='resource' AND m.status='active' AND e.kind IN ('area','theme')",
            )?;
            for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (hub, content) = row?;
                res.entry(hub).or_default().push(content.split(" - ").next().unwrap_or(&content).to_string());
            }
            let explore =
                |hub: &str| -> String { res.get(hub).map(|v| format!("  🔗 {}", v.join(", "))).unwrap_or_default() };
            let under = |a: Option<&str>, t: Option<&str>| -> Vec<String> {
                ents.iter()
                    .filter(|e| e.area.as_deref() == a && e.theme.as_deref() == t)
                    .map(|e| format!("{} ({})", e.name, e.kind))
                    .collect()
            };
            if tax.areas.is_empty() && tax.unplaced == 0 {
                println!("no themes yet - write in your journal and run `sb ingest`");
            }
            for (area, (_, themes)) in &tax.areas {
                println!("■ {area}{}", explore(area));
                for t in themes.keys() {
                    println!("  ├─ {t}{}", explore(t));
                    for i in under(Some(area), Some(t)) {
                        println!("  │    · {i}");
                    }
                }
                for i in under(Some(area), None) {
                    println!("  · {i}");
                }
            }
            for t in tax.orphan_themes.keys() {
                println!("■ (no grand theme) › {t}");
                for i in under(None, Some(t)) {
                    println!("    · {i}");
                }
            }
            let un = under(None, None);
            if !un.is_empty() {
                println!("■ (unplaced) - run `sb connect` to place them");
                for i in un {
                    println!("  · {i}");
                }
            }
        }
        Cmd::Reset { yes } => {
            if !yes {
                println!(
                    "This forgets all learned knowledge (memories, themes, links, syntheses) and archives the generated notes."
                );
                println!(
                    "Kept: journals, tasks, work sessions, calendar connection, action log. A DB backup is made first."
                );
                println!("Run `sb reset --yes` to confirm, then `sb ingest` to re-read your journals.");
                return Ok(());
            }
            let r = consolidate::reset_knowledge(&brain)?;
            println!(
                "knowledge reset · backup: {} · {} note(s) moved to {}",
                r.backup, r.archived_notes, r.archive_dir
            );
            println!("now run `sb ingest` to re-read your journals");
        }
        Cmd::Theme { name } => {
            second_brain::theme::load(&brain);
            match name {
                None => {
                    let cur = second_brain::theme::current().name;
                    for t in second_brain::theme::THEMES {
                        println!("{} {:<18} {}", if t.name == cur { "›" } else { " " }, t.name, t.label);
                    }
                }
                Some(n) => {
                    let i = second_brain::theme::by_name(&n).with_context(|| {
                        let names: Vec<&str> = second_brain::theme::THEMES.iter().map(|t| t.name).collect();
                        format!("unknown theme '{n}' (one of: {})", names.join(", "))
                    })?;
                    let reached = second_brain::theme::set(&brain, i)?;
                    // Give the background notifier a moment before the process exits.
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    println!("theme: {} (nvim instances notified: {reached})", second_brain::theme::current().label);
                }
            }
        }
        Cmd::Dash { preview: None } => second_brain::dash::run(&brain, command_list())?,
        Cmd::Dash { preview: Some(v) } => {
            print!("{}", second_brain::dash::preview(&brain, command_list(), v, 140, 42)?)
        }
        Cmd::Event { text, calendar: target, dry_run } => {
            let text = text.join(" ");
            anyhow::ensure!(!text.trim().is_empty(), "usage: sb event \"city hackathon on 20 and 21 nov 2026\"");
            let sp = Spinner::start("Understanding the date");
            let ev = second_brain::gcal_admin::understand(&brain, &text);
            drop(sp);
            let ev = ev?;
            println!("{}", ev.describe());
            if dry_run {
                println!("(dry run: nothing added)");
                return Ok(());
            }
            let be = calendar::Backend::google(&brain)?.context("calendar not connected: `sb calendar auth`")?;
            let (cal, label) = match target.as_deref() {
                Some("sb") | Some("second-brain") => {
                    (calendar::agent_calendar(&brain, &be)?, brain.cfg.calendar.agent_calendar_name.clone())
                }
                Some(other) if other != "primary" => (other.to_string(), other.to_string()),
                Some(_) => ("primary".to_string(), "Mon agenda".to_string()),
                None if brain.cfg.calendar.events_access => ("primary".to_string(), "Mon agenda".to_string()),
                None => (calendar::agent_calendar(&brain, &be)?, brain.cfg.calendar.agent_calendar_name.clone()),
            };
            second_brain::gcal_admin::create(&brain, &be, &cal, &ev)?;
            println!("added to '{label}'");
        }
        Cmd::Unlink { a, b } => {
            let id = |n: &str| -> Result<i64> {
                Ok(second_brain::memory::find_entity(&brain.conn, n)?
                    .with_context(|| format!("no item named '{n}'"))?
                    .0)
            };
            let (x, y) = (id(&a)?, id(&b)?);
            let tx = brain.conn.unchecked_transaction()?;
            let rows: Vec<second_brain::actions::LinkRow> = tx
                .prepare(
                    "SELECT src, dst, relation, block_id, created_at, why FROM links
                     WHERE (src=?1 AND dst=?2) OR (src=?2 AND dst=?1)",
                )?
                .query_map([x, y], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if rows.is_empty() {
                println!("no connection between '{a}' and '{b}'");
                return Ok(());
            }
            tx.execute("DELETE FROM links WHERE (src=?1 AND dst=?2) OR (src=?2 AND dst=?1)", [x, y])?;
            tx.execute("UPDATE entities SET dirty=1 WHERE id IN (?1, ?2)", [x, y])?;
            second_brain::actions::log(
                &tx,
                "user",
                "unlink",
                &format!("{a} ↮ {b} ({} link(s))", rows.len()),
                Some(second_brain::actions::Undo::RestoreLinks { links: rows.clone() }),
            )?;
            tx.commit()?;
            second_brain::notes::regenerate_dirty(&brain)?;
            println!("removed {} link(s) between '{a}' and '{b}' (sb undo to restore)", rows.len());
        }
        Cmd::Connect { full } => {
            let sp = Spinner::start("Reflecting on your notes");
            let r = connect::run(&brain, full)?;
            if r.skipped {
                drop(sp);
                println!("nothing new to connect (use --full to re-examine everything)");
            } else {
                sp.done(&format!("{} new connection(s), {} item(s) placed in themes", r.links, r.placed));
                for x in &r.rejected {
                    println!("rejected: {x}");
                }
                println!("see them with `sb graph` · undo with `sb log` + `sb undo <id>`");
            }
        }
        Cmd::Reprocess { path } => {
            // reprocess() resolves "x.md", "journal/x.md", "vault/journal/x.md" or a real path.
            let sp = Spinner::start("Reprocessing");
            let r = ingest::reprocess(&brain, &path)?;
            sp.done(&format!("Reprocessed ({} model call(s))", r.ingest.llm_calls));
            println!("{} old memories retracted · {} empty entities archived", r.retracted, r.archived.len());
            print_ingest(&r.ingest);
            auto_connect(&brain, r.ingest.stats.entities_created.max(1));
            if r.ingest.stats.tasks_created + r.ingest.stats.tasks_updated > 0 {
                google(&brain, true);
            }
        }
        Cmd::Search { query, limit } => {
            let q = query.join(" ");
            let hits = retrieval::search(&brain, &q, limit, None)?;
            if hits.is_empty() {
                println!("no matches");
            }
            for h in hits {
                println!(
                    "[{}] {} - {}\n    {}",
                    h.kind.name(),
                    h.title,
                    h.source.unwrap_or_default(),
                    h.snippet.replace('\n', " ")
                );
            }
        }
        Cmd::Ask { question } => {
            let sp = Spinner::start("Thinking");
            let answer = retrieval::ask(&brain, &question.join(" "))?;
            drop(sp);
            print_answer(answer)
        }
        Cmd::Tasks { all, due } => {
            google(&brain, false);
            let rows = if let Some(d) = due {
                tasks::deadlines(&brain, d)?
            } else if all {
                tasks::query(&brain.conn, "1=1", &tasks::now_str(&brain))?
            } else {
                let mut v = tasks::ranked(&brain)?;
                v.extend(tasks::open_tasks(&brain)?.into_iter().filter(|t| t.kind == "deadline"));
                v
            };
            if rows.is_empty() {
                println!("no tasks");
            }
            for t in rows {
                println!("{}", tasks::fmt_row(&t, today));
            }
        }
        Cmd::Task(c) => {
            let msg = match c {
                TaskCmd::Add { text } => {
                    let s = tasks::add(&brain, &text.join(" "))?;
                    if s.tasks_created > 0 {
                        "task added".to_string()
                    } else {
                        format!("matched an existing task ({} updated)", s.tasks_updated)
                    }
                }
                TaskCmd::Done { id } => tasks::set_status(&brain, id, "done")?,
                TaskCmd::Cancel { id } => tasks::set_status(&brain, id, "cancelled")?,
                TaskCmd::Reopen { id } | TaskCmd::Confirm { id } => tasks::set_status(&brain, id, "open")?,
                TaskCmd::Edit { id, title, due, est, priority, project, desc } => {
                    let fields =
                        title.is_some() || due.is_some() || est.is_some() || priority.is_some() || project.is_some();
                    let mut msg = if fields {
                        tasks::edit(&brain, id, title, due, est, priority, project)?
                    } else {
                        String::new()
                    };
                    if let Some(d) = desc {
                        let text = if d == "-" { edit_in_editor(&brain, id)? } else { d };
                        let m = tasks::set_notes(&brain, id, &text)?;
                        msg = if msg.is_empty() { m } else { format!("{msg}\n{m}") };
                    }
                    anyhow::ensure!(
                        !msg.is_empty(),
                        "nothing to change: use --title, --due, --est, --priority, --project or --desc"
                    );
                    msg
                }
                TaskCmd::Show { id } => tasks::describe(&brain, id)?,
                TaskCmd::Delete { id } => tasks::delete(&brain, id)?,
            };
            println!("{msg}");
            google(&brain, true);
        }
        Cmd::Today { date } => {
            google(&brain, false);
            let d = parse_day(&brain, date.as_deref())?;
            print!("{}", planner::agenda_text(&brain, d)?);
        }
        Cmd::Plan { apply } => {
            let diff = planner::compute(&brain)?;
            println!("{}", if apply { "Applying plan:" } else { "Proposed plan (dry run - use --apply to commit):" });
            print!("{}", planner::diff_text(&brain, &diff));
            if apply && !diff.is_empty() {
                let created = planner::apply(&brain, &diff, "user")?;
                println!("saved: {} new session(s); undo with `sb log` + `sb undo <id>`", created.len());
                match calendar::Backend::google(&brain)? {
                    Some(be) => {
                        let sp = Spinner::start("Google Calendar");
                        let r = calendar::sync(&brain, &be, true)?;
                        sp.done("Calendar synced");
                        println!(
                            "calendar: {} event(s) created, {} removed in '{}'",
                            r.pushed, r.removed, brain.cfg.calendar.agent_calendar_name
                        );
                    }
                    None => println!("calendar not connected: sessions stored locally only"),
                }
            }
        }
        Cmd::Briefing { date, llm } => {
            let d = parse_day(&brain, date.as_deref())?;
            let sp = Spinner::start("Building briefing");
            let (p, md) = briefing::generate(&brain, d, llm)?;
            drop(sp);
            println!("{md}\nwritten to {}", p.display());
        }
        Cmd::Session { id, status } => println!("{}", planner::mark_session(&brain, id, &status)?),
        Cmd::Calendar(c) => match c {
            CalCmd::Auth { reset, events } => {
                if events && !brain.cfg.calendar.events_access {
                    set_config_flag(&brain, "calendar", "events_access", "true")?;
                    brain.cfg.calendar.events_access = true;
                }
                calendar::authorize(&brain, reset)?;
                brain.cfg.calendar.enabled = true;
                let be = calendar::Backend::google(&brain)?.context("authorization stored but backend unavailable")?;
                let sp = Spinner::start("Setting up your calendar");
                calendar::agent_calendar(&brain, &be)?;
                let r = calendar::sync(&brain, &be, false)?;
                sp.done("Calendar connected");
                if brain.cfg.calendar.tasks_sync {
                    match second_brain::gtasks::sync(&brain, &be) {
                        Ok(t) => println!("{}", t.summary()),
                        Err(e) => eprintln!("warning: {e:#}"),
                    }
                }
                println!(
                    "Calendar '{}' ready · {} busy period(s) found in the next {} days.\nNext: `sb plan` then `sb plan --apply`.",
                    brain.cfg.calendar.agent_calendar_name, r.busy_intervals, brain.cfg.schedule.horizon_days
                );
            }
            CalCmd::Events { from, to, pattern, calendar: cal } => {
                let be = calendar::Backend::google(&brain)?.context("calendar not connected: `sb calendar auth`")?;
                let (f, t) = (parse_day(&brain, Some(&from))?, parse_day(&brain, Some(&to))?);
                let mut evs = second_brain::gcal_admin::list(&brain, &be, &cal, f, t)?;
                if let Some(p) = &pattern {
                    evs = second_brain::gcal_admin::matching(evs, p);
                }
                print_events(&evs);
            }
            CalCmd::Delete { from, to, pattern, calendar: cal, yes } => {
                let be = calendar::Backend::google(&brain)?.context("calendar not connected: `sb calendar auth`")?;
                let (f, t) = (parse_day(&brain, Some(&from))?, parse_day(&brain, Some(&to))?);
                let evs = second_brain::gcal_admin::matching(
                    second_brain::gcal_admin::list(&brain, &be, &cal, f, t)?,
                    &pattern,
                );
                print_events(&evs);
                if evs.is_empty() {
                    return Ok(());
                }
                if !yes {
                    println!(
                        "
nothing deleted: add --yes to delete these {} event(s)",
                        evs.len()
                    );
                    return Ok(());
                }
                let file = second_brain::gcal_admin::delete(&brain, &be, &evs)?;
                println!(
                    "
{} event(s) deleted · saved to {}
undo: sb calendar restore {}",
                    evs.len(),
                    file.display(),
                    file.display()
                );
            }
            CalCmd::Restore { file } => {
                let be = calendar::Backend::google(&brain)?.context("calendar not connected: `sb calendar auth`")?;
                println!("{} event(s) restored", second_brain::gcal_admin::restore(&brain, &be, &file)?);
            }
            CalCmd::Block { title, from, to, weekdays } => {
                let be = calendar::Backend::google(&brain)?.context("calendar not connected: `sb calendar auth`")?;
                let (f, t) = (parse_day(&brain, Some(&from))?, parse_day(&brain, Some(&to))?);
                second_brain::gcal_admin::block(&brain, &be, &title, f, t, weekdays)?;
                let n = second_brain::gcal_admin::days(f, t, weekdays).len();
                println!(
                    "'{title}' added to '{}' ({f} → {t}{}, {n} day(s) kept free of work sessions)",
                    brain.cfg.calendar.agent_calendar_name,
                    if weekdays { ", Mon-Fri" } else { "" }
                );
                if brain.cfg.autonomy.calendar_writes == "auto" {
                    for m in daemon::replan(&brain, Some(&be))? {
                        println!("{m}");
                    }
                }
            }
            CalCmd::Sync { push } => {
                let be = calendar::Backend::google(&brain)?
                    .context("calendar not connected (enable in config and run `sb calendar auth`)")?;
                let sp = Spinner::start("Google Calendar");
                let r = calendar::sync(&brain, &be, push)?;
                sp.done("Calendar synced");
                println!(
                    "{} busy period(s){} · your edits: {} deleted, {} moved · pushed {} · removed {}",
                    r.busy_intervals,
                    if r.busy_changed { " (changed)" } else { "" },
                    r.user_deleted,
                    r.user_moved,
                    r.pushed,
                    r.removed
                );
                if brain.cfg.calendar.tasks_sync {
                    let t = second_brain::gtasks::sync(&brain, &be)?;
                    println!("{}", t.summary());
                }
            }
        },
        Cmd::Memory { kind, retract, limit } => {
            if let Some(id) = retract {
                let tx = brain.conn.unchecked_transaction()?;
                let n = tx.execute("UPDATE memories SET status='retracted' WHERE id=?1 AND status='active'", [id])?;
                anyhow::ensure!(n == 1, "no active memory #{id}");
                second_brain::db::fts_delete(&tx, second_brain::db::DocKind::Memory, id)?;
                tx.execute("UPDATE entities SET dirty=1 WHERE id IN (SELECT entity_id FROM memory_entities WHERE memory_id=?1)", [id])?;
                second_brain::db::kv_set(&tx, "profile_dirty", "1")?;
                actions::log(&tx, "user", "memory_retracted", &format!("memory #{id} retracted"), None)?;
                tx.commit()?;
                second_brain::notes::regenerate_dirty(&brain)?;
                println!("memory #{id} retracted");
                return Ok(());
            }
            let mut st = brain.conn.prepare(
                "SELECT m.id, m.kind, coalesce(m.key, (SELECT group_concat(e.name, ', ') FROM entities e JOIN memory_entities me ON me.entity_id=e.id WHERE me.memory_id=m.id), ''),
                   m.content, m.epistemic, coalesce(m.occurred_on,''), m.origin FROM memories m
                 WHERE m.status='active' AND (?1 IS NULL OR m.kind=?1) ORDER BY m.id DESC LIMIT ?2",
            )?;
            let rows = st.query_map(rusqlite::params![kind, limit as i64], |r| {
                Ok(format!(
                    "#{:<4} {:<9} [{}] {} ({}, {}, {})",
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?
                ))
            })?;
            for r in rows {
                println!("{}", r?);
            }
        }
        Cmd::Log { limit } => {
            for a in actions::recent(&brain.conn, limit)? {
                println!(
                    "#{:<5} {} {:<6} {:<22} {}{}",
                    a.id,
                    second_brain::util::parse_ts(&a.ts)
                        .map(|t| brain.local(t).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or(a.ts.clone()),
                    a.actor,
                    a.kind,
                    a.summary,
                    if a.undone {
                        "  (undone)"
                    } else if a.undoable {
                        "  ↶"
                    } else {
                        ""
                    }
                );
            }
        }
        Cmd::Undo { id } => {
            let s = actions::undo(&brain.conn, id)?;
            second_brain::notes::regenerate_dirty(&brain)?;
            println!("undone: {s}");
        }
        Cmd::Consolidate { no_llm } => {
            let sp = Spinner::start("Maintaining memory");
            let r = consolidate::run(&brain, !no_llm)?;
            sp.done("Maintenance finished");
            println!(
                "merged {} duplicate(s) · {} summary(ies) · {} note(s) repaired · {} written · backup: {}",
                r.duplicates_merged,
                r.summaries_updated,
                r.notes_repaired,
                r.notes_written,
                r.backup.unwrap_or_default()
            );
        }
        Cmd::Graph { no_open, out } => {
            let sp = Spinner::start("Building knowledge graph");
            let (path, n, e) = graph::write(&brain, out.as_deref())?;
            sp.done(&format!("Graph: {n} nodes, {e} links"));
            println!("{}", path.display());
            if !no_open {
                graph::open(&path);
            }
        }
        Cmd::Stats => stats(&brain)?,
        Cmd::Doctor => doctor(&brain)?,
    }
    Ok(())
}

/// Open the task's description in $EDITOR and return the new text.
fn edit_in_editor(brain: &Brain, id: i64) -> Result<String> {
    let current: Option<String> = brain.conn.query_row("SELECT notes FROM tasks WHERE id=?1", [id], |r| r.get(0))?;
    let path = brain.home.join(format!("data/task-{id}.md"));
    second_brain::util::atomic_write(&path, current.as_deref().unwrap_or(""))?;
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "nvim".into());
    std::process::Command::new(editor).arg(&path).status()?;
    let text = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(text)
}

/// Every sub-command (and nested one, e.g. "task add") with its description and full help,
/// for the dashboard palette and its Commandes page.
fn command_list() -> Vec<(String, String, String)> {
    let mut out = vec![];
    let mut root = Cli::command();
    root.build();
    for c in root.get_subcommands() {
        let name = c.get_name();
        if matches!(name, "help" | "dash" | "daemon") || c.is_hide_set() {
            continue;
        }
        let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
        let subs: Vec<_> = c.get_subcommands().filter(|s| s.get_name() != "help").collect();
        if subs.is_empty() {
            let mut c = c.clone().bin_name(format!("sb {name}"));
            out.push((name.to_string(), about, c.render_long_help().to_string()));
        } else {
            for s in subs {
                let a = s.get_about().map(|a| a.to_string()).unwrap_or_default();
                let full = format!("{name} {}", s.get_name());
                let mut s = s.clone().bin_name(format!("sb {full}"));
                out.push((full, a, s.render_long_help().to_string()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn print_events(evs: &[second_brain::gcal_admin::Event]) {
    if evs.is_empty() {
        println!("no matching events");
    }
    for e in evs {
        println!("  {:<17} {}", e.when, e.title);
    }
    if !evs.is_empty() {
        println!("({} event(s))", evs.len());
    }
}

/// Set `key = value` inside `[section]` of config.toml (adds the line if missing).
fn set_config_flag(brain: &Brain, section: &str, key: &str, value: &str) -> Result<()> {
    let path = brain.home.join("config.toml");
    let text = std::fs::read_to_string(&path)?;
    let head = format!("[{section}]");
    let start = text.find(&head).with_context(|| format!("no {head} in config.toml"))?;
    let body_start = start + head.len();
    let end = text[body_start..].find("\n[").map(|i| body_start + i).unwrap_or(text.len());
    let section_text = &text[body_start..end];
    let new_section = match section_text.lines().position(|l| l.trim_start().starts_with(&format!("{key} "))) {
        Some(_) => section_text
            .lines()
            .map(|l| {
                if l.trim_start().starts_with(&format!("{key} ")) { format!("{key} = {value}") } else { l.to_string() }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => format!("\n{key} = {value}{section_text}"),
    };
    second_brain::util::atomic_write(&path, &format!("{}{}{}", &text[..body_start], new_section, &text[end..]))?;
    Ok(())
}

/// Sync with Google (Tasks two-way, calendar sessions in auto mode) when connected.
/// Never fails the command: problems are shown as a warning.
fn google(brain: &Brain, changed: bool) {
    let be = match calendar::Backend::google(brain) {
        Ok(Some(be)) => be,
        Ok(None) => return,
        Err(e) => {
            eprintln!("warning: Google not available: {e:#}");
            return;
        }
    };
    let sp = Spinner::start("Syncing with Google");
    match daemon::sync_google(brain, &be, changed) {
        Ok(msgs) => {
            drop(sp);
            for m in msgs {
                println!("{m}");
            }
        }
        Err(e) => {
            drop(sp);
            eprintln!("warning: Google sync failed: {e:#}");
        }
    }
}

fn auto_connect(brain: &Brain, new_entities: usize) {
    if !brain.cfg.ingest.auto_connect || new_entities == 0 || !brain.provider.available() {
        return;
    }
    let sp = Spinner::start("Connecting with what you already know");
    match connect::run(brain, false) {
        Ok(r) if !r.skipped => {
            sp.done(&format!("{} new connection(s), {} item(s) placed in themes", r.links, r.placed))
        }
        Ok(_) => drop(sp),
        Err(e) => {
            drop(sp);
            eprintln!("warning: connection pass failed: {e:#}");
        }
    }
}

fn print_answer(answer: retrieval::Answer) {
    match answer {
        retrieval::Answer::Deterministic(s) => println!("{s}\n(answered from structured data - 0 model calls)"),
        retrieval::Answer::Model { text, sources, context_tokens } => {
            println!("{text}");
            if !sources.is_empty() {
                println!("\nsources: {}", sources.join(", "));
            }
            println!("(1 model call, ~{context_tokens} context tokens)");
        }
    }
}

fn stats(brain: &Brain) -> Result<()> {
    let c = &brain.conn;
    let q = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
    println!("Model usage (all time):");
    let mut st = c.prepare(
        "SELECT purpose, provider, count(*), sum(input_tokens), sum(output_tokens), sum(tokens_estimated), avg(context_tokens),
           sum(cost_usd), sum(ok) FROM llm_calls GROUP BY purpose, provider ORDER BY purpose",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, f64>(6)?,
            r.get::<_, Option<f64>>(7)?,
            r.get::<_, i64>(8)?,
        ))
    })?;
    for r in rows {
        let (purpose, provider, n, inp, out, est, ctx, cost, ok) = r?;
        println!(
            "  {purpose:<12} {provider:<14} calls {n:>4} (ok {ok}) · in {inp:>7} · out {out:>6}{} · avg prompt ~{:.0} tok · API-equivalent {}",
            if est > 0 { " (some estimated)" } else { "" },
            ctx,
            cost.map(|c| format!("${c:.4}")).unwrap_or("n/a (provider reports none)".into())
        );
    }
    let files = q("SELECT count(*) FROM sources")?;
    let calls = q("SELECT count(*) FROM llm_calls WHERE purpose='extract' AND provider!='cache'")?;
    let blocks = q("SELECT count(*) FROM blocks")?;
    let det = q("SELECT count(*) FROM blocks WHERE status IN ('deterministic','skipped')")?;
    let cache = q("SELECT count(*) FROM llm_calls WHERE provider='cache'")?;
    println!("Efficiency:");
    println!(
        "  journals: {files} · blocks: {blocks} · handled without model: {det} ({:.0}%)",
        if blocks > 0 { det as f64 * 100.0 / blocks as f64 } else { 0.0 }
    );
    println!(
        "  extraction calls: {calls} · per journal: {:.2} · cache hits: {cache}",
        if files > 0 { calls as f64 / files as f64 } else { 0.0 }
    );
    println!(
        "  memory: {} active memories, {} entities, {} open tasks, {} pending blocks",
        q("SELECT count(*) FROM memories WHERE status='active'")?,
        q("SELECT count(*) FROM entities")?,
        q("SELECT count(*) FROM tasks WHERE status IN ('open','proposed')")?,
        q("SELECT count(*) FROM blocks WHERE status='pending' AND present=1")?
    );
    println!("Note: costs are API-equivalent estimates reported by the CLI, not subscription charges.");
    Ok(())
}

fn doctor(brain: &Brain) -> Result<()> {
    let c = &brain.conn;
    let ok = |b: bool| if b { "ok " } else { "FAIL" };
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    println!("[{}] database integrity: {integrity}", ok(integrity == "ok"));
    let fk: i64 = c.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    println!("[{}] foreign keys: {fk} violation(s)", ok(fk == 0));
    let fts: rusqlite::Result<()> = c.execute("INSERT INTO fts(fts) VALUES('integrity-check')", []).map(|_| ());
    println!("[{}] full-text index", ok(fts.is_ok()));
    for d in second_brain::brain::VAULT_DIRS {
        let p = brain.vault().join(d);
        println!("[{}] vault/{d}", ok(p.is_dir()));
    }
    let dirty: i64 = c.query_row("SELECT count(*) FROM entities WHERE dirty=1", [], |r| r.get(0))?;
    println!("[{}] notes in sync ({dirty} pending regeneration)", ok(dirty == 0));
    let pending: i64 =
        c.query_row("SELECT count(*) FROM blocks WHERE status='pending' AND present=1", [], |r| r.get(0))?;
    let failed: i64 = c.query_row("SELECT count(*) FROM blocks WHERE status='failed'", [], |r| r.get(0))?;
    println!("[{}] extraction queue: {pending} pending, {failed} failed", ok(failed == 0));
    let prov = &brain.provider;
    let avail = match prov {
        second_brain::inference::Provider::ClaudeCli { cmd, .. }
        | second_brain::inference::Provider::CodexCli { cmd, .. } => {
            std::process::Command::new(cmd).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
        }
        second_brain::inference::Provider::AnthropicApi { key_env, .. } => std::env::var(key_env).is_ok(),
        _ => false,
    };
    println!(
        "[{}] model provider: {} {} (deterministic features work without it)",
        ok(avail),
        prov.name(),
        prov.model()
    );
    let cal = brain.cfg.calendar.enabled;
    let tok = brain.secrets_dir().join("google_token.json").exists();
    println!(
        "[{}] calendar: {}",
        if cal && tok { "ok " } else { " -- " },
        if !cal {
            "paused (calendar.enabled = false)"
        } else if tok {
            "connected"
        } else {
            "not connected (optional: `sb calendar auth`)"
        }
    );
    let tasks_ok =
        matches!(calendar::Backend::google(brain), Ok(Some(calendar::Backend::Google(ref g))) if g.has_tasks_scope());
    println!(
        "[{}] google tasks: {}",
        if brain.cfg.calendar.tasks_sync && tasks_ok { "ok " } else { " -- " },
        if !brain.cfg.calendar.tasks_sync {
            "off (calendar.tasks_sync = false)"
        } else if tasks_ok {
            "two-way sync on"
        } else {
            "not authorized: enable the Tasks API, then `sb calendar auth`"
        }
    );
    let mode = brain.cfg.autonomy.calendar_writes.clone();
    println!("[ok ] autonomy: calendar writes = {mode}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn config_flag_is_added_then_replaced_inside_its_section() {
        let dir = tempfile::tempdir().unwrap();
        second_brain::Brain::init(dir.path()).unwrap();
        let brain = super::Brain::open(dir.path()).unwrap();
        super::set_config_flag(&brain, "calendar", "events_access", "true").unwrap();
        super::set_config_flag(&brain, "calendar", "events_access", "false").unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert_eq!(text.matches("events_access").count(), 1, "{text}");
        let cfg: second_brain::config::Config = toml::from_str(&text).unwrap();
        assert!(!cfg.calendar.events_access);
    }
}
