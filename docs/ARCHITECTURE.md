# Architecture & internals

Details behind the [README](../README.md): how `sb` understands, stores, plans and stays cheap.

## How notes are understood

**Hierarchical themes (scales to thousands of items).** Everything is organised
as *grand theme › theme › item* (e.g. Cybersécurité › Detection engineering ›
"Labos SIEM"). The taxonomy itself stays small, so it is always in the prompt; item
detail is loaded only where needed: while the whole map fits `knowledge_map_tokens`
one call does everything, beyond that a routing call first picks the grand
themes/themes a note touches, then extraction goes deep into just those. The
reflection agent works one grand theme at a time.

**Syntheses.** Daily (from that day's memories, tasks and new links), weekly (from
the dailies) and monthly (from the weeklies) syntheses are written to
`vault/summaries/{daily,weekly,monthly}/` with `[[links]]` to your notes, in the
configured `language`. The daemon writes them at `synthesis_time` (weekly on
Sundays, monthly on the last day) and catches up missed ones; unchanged periods
cost nothing.

**The model does the understanding.** A changed note is read in one call
(up to `max_batch_tokens`, 12k by default), together with a compact map of everything
the brain already knows grouped by area, so it can group what belongs together and
link new notes to old ones (`link` items, each with a short *why*). After new items
appear, a reflection pass (`sb connect`) looks at the whole graph and adds missing
connections and areas. Rust only validates names, dates and ranges and applies the
result transactionally; every link is undoable. Measured on a dense 200-line clipped
note: 1 extraction call (6.6k in / 10k out) + 1 connect call (1.7k / 1.8k), about
$0.008 API-equivalent.

Lists are understood as a whole: numbered items (`1.`, `1)`, `1.Text`, `1️⃣`, even
separated by blank lines), adjacent bullets, and an intro line ending with `:` form
one block, so "7 date ideas" becomes one idea, not seven fragments. Pasted
content works too: articles/posts are stored as opinions or claims (not facts),
short lines and links (`crt.sh`, `SSRF dorks`) become **to-explore resources**, and
everything is attached to broad **area hubs** (AI, Cybersecurity, Learning,
Relationships…) that act as the backbone of the graph.

Everything else (e.g. *"I have a mathematics exam next Thursday"*, *"I'm more
productive in the morning"*, *"This could be useful for my SaaS project"*) is batched
per file into one model call that returns compact JSON, which Rust validates
before anything is stored.

## Architecture

One controller (`Brain`) with deterministic Rust components; the model is a
function that turns text into validated JSON. No agent loop, no tools for the model.

```
src/
  brain.rs        controller handle: config + SQLite + provider + clock
  journal.rs      line-based Markdown block parser, checkbox/prefix extraction, heuristics
  dates.rs        relative date resolution (also fed to the model as hints)
  ingest.rs       change detection (file hash → block hashes), batching, extraction prompt
  inference.rs    providers (Claude CLI, Codex CLI, Anthropic API, mock), budget, cache, retries, timeouts
  memory.rs       typed items, validation, dedupe, task matching, supersession, provenance
  notes.rs        Obsidian notes generated from SQLite (user text outside markers preserved)
  retrieval.rs    L0 SQL answers, L1 FTS5/BM25, L3 bounded context pack
  tasks.rs        operational memory, priority score, task commands
  planner.rs      deterministic scheduler + incremental replanning diff
  calendar.rs     Google OAuth (PKCE loopback), free/busy, dedicated calendar sync
  briefing.rs     daily briefing, proactive alerts (shown once per state)
  consolidate.rs  deferred maintenance, VACUUM INTO backups
  gtasks.rs       two-way Google Tasks sync (signatures, tombstones)
  gcal_admin.rs   events of your own calendar: list, delete/restore, periods, plain-text events
  graph.rs        self-contained HTML knowledge graph (graph.html template, no network)
  dash.rs         terminal dashboard (ratatui), English/French
  theme.rs        interface themes shared with nvim (data/theme.json)
  ui.rs           terminal spinner/progress (TTY only)
  daemon.rs       event loop: notify watcher (+ polling fallback), persisted timers
  actions.rs      audit log + undo
  db.rs           schema/migrations (PRAGMA user_version), FTS helpers
```

### Memory model

| Category | Storage | Notes |
|---|---|---|
| Profile | `memories(kind='profile', key)` | same key → old fact `superseded_by` new; shown in `summaries/Profile.md` |
| Episodic | `memories(kind='episodic', occurred_on, recorded_at)` | when it happened ≠ when written |
| Knowledge | `memories(kind='knowledge')` + concept `entities` + `links` | Markdown notes in `knowledge/`, `epistemic` = fact / opinion / hypothesis |
| Decisions, ideas, goals | `memories` + entities | ideas are stored as hypotheses |
| Operational | `tasks`, `sessions`, `busy` | status is authoritative; proposed (“maybe”) ≠ open; planned ≠ done |

Every memory and task keeps `block_id` → `blocks` → `sources` (file + line):
`sb search` shows `journal/2026-11-07.md:3`, notes link `[[2026-11-07]]`.

### Ingestion guarantees

- Unchanged file (hash) → zero work. Changed file → only new blocks are processed.
- Same content twice → no duplicates (block hash per source, memory `norm_hash`,
  near-duplicate check per entity, task title matching).
- Editing a checkbox/date updates the existing task; done/cancelled tasks are never
  reactivated by old text (`ignored: … already done` is reported).
- Removing text never deletes memories (the block is marked `present=0`, kept for provenance).
- Each batch is applied in one SQLite transaction; malformed output, timeouts and
  provider outages leave blocks `pending` (bounded attempts, then `failed`).
- Original journals are never written. Generated notes are atomic writes; entities stay
  `dirty` until written, so a crash between DB commit and file write self-heals.

### Scheduling

`planner::schedule` is a pure function: free time = work hours − protected time −
busy events (padded by breaks) − kept sessions; tasks in earliest-deadline order,
then priority score; blocks between `min_block` and `max_block`, daily focus cap,
max blocks per task per day, no tiny tail fragments. Replanning keeps valid future
sessions and only cancels/re-places invalid ones (task done, conflicts, after a
moved deadline, surplus) and missing minutes. Unestimated tasks use the default
estimate and are flagged in the plan and briefing. Calendar writes are limited to
the dedicated calendar by OAuth scope, honour `dry-run`/`auto`, and are undoable.

## Model providers

| `provider.kind` | How | Measured overhead |
|---|---|---|
| `claude` (default) | `claude -p --output-format json --tools "" --system-prompt … --no-session-persistence --setting-sources "" --strict-mcp-config` | ~1.0-1.5k input tokens per call |
| `codex` | `codex exec --ephemeral --sandbox read-only --json -` | **~17.5k** input tokens per call (agent harness prompt) |
| `anthropic-api` | Messages API with `ANTHROPIC_API_KEY` | prompt only (not live-tested here) |
| `none` | heuristic commitment extraction only | 0 |

The CLI adapters use your existing CLI login; `sb` does not assume API access.
Check that unattended use of your plan is allowed by your provider's terms; the
daily budget (`[budget] max_calls_per_day`, `max_input_tokens_per_day`) caps usage,
and when it is exhausted work is queued, never dropped, and all deterministic
features keep working. Subprocesses run without a shell, with a timeout, exit-code
checks and at most `max_retries` retries; the model gets no tools.

## Token efficiency (measured, 2026-11-07, Claude CLI `haiku`)

| Workflow | Calls | Input tokens | Notes |
|---|---|---|---|
| Example journal, 7 blocks | 1 | 1,549 | 1 checkbox handled deterministically; API-equivalent $0.0011 |
| Re-ingest unchanged journal | 0 | 0 | file hash short-circuit |
| Append 2 sentences + tick a checkbox (daemon) | 1 | ~1.5k | only the new block is sent |
| `sb ask` free-form question | 1 | 1,089 | prompt ~245 tokens: profile + top tasks + top memories |
| `sb ask "what are my deadlines?"`, `tasks`, `today`, `plan`, `briefing`, `search` | 0 | 0 | |
| Same `ask` repeated | 0 | 0 | prompt-hash cache |
| Same 1-block journal via Codex CLI | 1 | 17,958 | why Claude is the default |

The CLI reports more output tokens (~1.2-1.6k) than the visible JSON (~200
tokens); that overhead is on the provider side. `sb stats` shows per-purpose calls,
tokens (flagged when estimated), average prompt size, cache hits and
API-equivalent cost (an estimate from the CLI, not a subscription charge).

## Privacy & safety

- Only the new journal blocks plus a few known names/IDs relevant to them are sent
  to the model - never the vault. Retrieval for `ask` is capped by token budgets.
- Journal text is wrapped as data (`<journal>…</journal>`, closing tag neutralized);
  the model has no tools; its output can only produce whitelisted item types that are
  validated (types, ranges, dates, block indices) before a transaction applies them.
  Unknown "actions" are rejected and reported.
- Calendar writes go to a dedicated "Second Brain" calendar and are dry-run by
  default (`autonomy.calendar_writes`). Reading/editing your own calendars
  (`calendar.events` scope) is opt-in with `sb calendar auth --events`; deletions
  list first, need `--yes`, and are restorable.
- Secrets: Google token in `data/secrets/` (0600), client secret via env; API key via env.
  Nothing secret is logged.
- Audit log (`sb log`) for every automated change; `sb undo` for tasks, memories,
  supersessions and session plans.
- Backups: `sb consolidate` (run nightly by the daemon) writes a consistent
  `data/backups/brain-YYYY-MM-DD.db` via `VACUUM INTO` and keeps 7. Back up the vault
  with git/Syncthing; `data/secrets` is git-ignored.

## Running in the background

```sh
cp examples/second-brain.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now second-brain
journalctl --user -u second-brain -f
```

The daemon refuses to start twice (SQLite heartbeat lease), catches up on changes
made while it was stopped, and stops cleanly on SIGINT/SIGTERM. Daily jobs
(briefing at `briefing_time`, maintenance at `maintenance_time`) are tracked in
SQLite so restarts don't repeat or skip them. Alerts go to the log and, optionally,
`notify_command` (e.g. `["notify-send", "Second Brain"]`), each shown once per state.

## Tests

```sh
cargo test                              # 70 tests, no network, no paid inference
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Integration tests (`tests/pipeline.rs`) use a mock provider and a fake calendar:
end-to-end extraction of the example journal, idempotent/incremental re-ingestion,
checkbox edits and done-authority, history on deletion, profile supersession + undo,
malformed JSON / timeouts / outages / budget exhaustion, offline heuristics,
zero-call deterministic queries and bounded `ask` context, prompt injection,
scheduler invariants (no overlaps, busy time, deadlines, workload, protected time),
incremental replanning on calendar changes, calendar outage, briefing/alert
de-duplication, note recovery after interruption, and the Claude CLI subprocess
adapter through a fake executable. CI runs the same checks on Linux and macOS.

## Known limitations

- Google needs your own OAuth client ([setup](GOOGLE_CALENDAR.md)). While its
  consent screen is in *Testing*, Google expires the login after 7 days.
- With the minimal `freebusy` scope the agenda shows external events as "busy" without titles.
- `anthropic-api` provider is implemented but was not live-tested (no API key).
- Date parsing and heuristics are English-only.
- Entity matching is lexical (normalized names, "X project" → "X"); no aliases or
  renames yet. Semantic (embedding) search is deliberately not implemented: FTS5 is
  the baseline until retrieval evaluation shows a need.
- Task status changed via the CLI lives in SQLite only; the database is not fully
  rebuildable from the vault (use the daily backups).
- Editing inside a generated `sb:begin/sb:end` block is overwritten; write outside it.
- Missed sessions are inferred (past + not marked done); mark real progress with
  `sb session <id> done`.
- Tested on Linux only. macOS builds in CI; the daemon has no launchd example yet
  and live nvim theme switching only reaches the nvim hosting `sb dash` there.
  Windows is not supported.
- No prebuilt binaries yet, and the `second-brain` name is taken on crates.io:
  install from source.

## Roadmap status

| Phase | Status |
|---|---|
| 1 Core: CLI, SQLite, parser, change detection, providers, extraction, tasks, notes, provenance, FTS5, tests | done |
| 2 Memory: profile/episodic/knowledge/operational, relationships, projects, consolidation, bounded retrieval | done (no embeddings by design) |
| 3 Productivity: Google OAuth, Calendar + Tasks sync, scheduler, conflicts, replanning, briefing | done |
| 5 Interface: terminal dashboard, themes, nvim plugin, English/French UI | done |
| 4 Autonomy: watcher, timers, alerts, maintenance, audit, recovery, token reporting | done |
