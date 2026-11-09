//! Configuration loaded from `<home>/config.toml`. Every field has a default so
//! a minimal (even empty) config file works.

use anyhow::{Context, Result, bail};
use chrono::{NaiveTime, Weekday};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// IANA timezone used to interpret journal dates and working hours.
    pub timezone: String,
    /// Language of everything the model writes (names, summaries, syntheses): "fr", "en", …
    pub language: String,
    /// Language of the interface (dashboard, graph page, syntheses page): "en" (default) or "fr".
    /// The dashboard's L key switches it. (Generated content follows `language`.)
    #[serde(alias = "dashboard_language")]
    pub ui_language: String,
    /// Project that receives every task written without `+project` ("" = none).
    pub default_project: String,
    pub provider: ProviderConfig,
    pub budget: BudgetConfig,
    pub ingest: IngestConfig,
    pub schedule: ScheduleConfig,
    pub calendar: CalendarConfig,
    pub autonomy: AutonomyConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            timezone: "UTC".into(),
            language: "en".into(),
            ui_language: "en".into(),
            default_project: String::new(),
            provider: ProviderConfig::default(),
            budget: BudgetConfig::default(),
            ingest: IngestConfig::default(),
            schedule: ScheduleConfig::default(),
            calendar: CalendarConfig::default(),
            autonomy: AutonomyConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// `claude` | `codex` | `anthropic-api` | `none`
    pub kind: String,
    /// Model name passed to the provider (e.g. `haiku` for the Claude CLI).
    pub model: String,
    pub timeout_secs: u64,
    /// Retries after a failed/malformed call (bounded; 0 disables retries).
    pub max_retries: u32,
    /// Override the executable path (defaults to `claude` / `codex` on PATH).
    pub command: Option<String>,
    /// Env var holding the API key for `anthropic-api`.
    pub api_key_env: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig {
            kind: "claude".into(),
            model: "haiku".into(),
            timeout_secs: 120,
            max_retries: 1,
            command: None,
            api_key_env: "ANTHROPIC_API_KEY".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub max_calls_per_day: u32,
    pub max_input_tokens_per_day: u64,
    /// Max estimated tokens of journal text per extraction call (batching).
    pub max_batch_tokens: usize,
    /// Max blocks per extraction call.
    pub max_batch_blocks: usize,
    /// Token budget for `ask` context sections.
    pub profile_tokens: usize,
    pub ops_tokens: usize,
    pub memory_tokens: usize,
    /// Size of the "what I already know" map sent with each extraction.
    pub knowledge_map_tokens: usize,
    /// Size of the map for the `connect` reflection pass.
    pub connect_map_tokens: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        BudgetConfig {
            max_calls_per_day: 40,
            max_input_tokens_per_day: 200_000,
            max_batch_tokens: 12000,
            max_batch_blocks: 300,
            profile_tokens: 200,
            ops_tokens: 400,
            memory_tokens: 800,
            knowledge_map_tokens: 2500,
            connect_map_tokens: 8000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestConfig {
    /// Use the LLM for ambiguous natural language. When false (or provider
    /// `none`) a conservative heuristic extractor is used instead.
    pub llm_extraction: bool,
    /// Blocks with fewer words (and no date) are not sent to the model.
    pub min_block_words: usize,
    /// Extraction attempts per block before it is marked failed.
    pub max_attempts: u32,
    /// After an ingestion that created new items, run the connection agent (1 call).
    pub auto_connect: bool,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig { llm_extraction: true, min_block_words: 1, max_attempts: 3, auto_connect: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    /// e.g. `["mon-fri 09:00-12:30", "mon-fri 14:00-18:00", "sat 10:00-12:00"]`
    pub work_hours: Vec<String>,
    /// Time that must never be scheduled, same syntax.
    pub protected: Vec<String>,
    pub max_focus_minutes_per_day: i64,
    pub min_block_minutes: i64,
    pub max_block_minutes: i64,
    pub break_minutes: i64,
    pub horizon_days: i64,
    /// Used when a task has no stated estimate (flagged as assumed).
    pub default_estimate_minutes: i64,
    /// Max blocks of the same task per day.
    pub max_blocks_per_task_per_day: usize,
    /// Also book work sessions for tasks without a due date (off: only dated tasks reach the calendar).
    pub plan_undated: bool,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        ScheduleConfig {
            work_hours: vec!["mon-fri 09:00-12:30".into(), "mon-fri 14:00-18:00".into()],
            protected: vec![],
            max_focus_minutes_per_day: 300,
            min_block_minutes: 30,
            max_block_minutes: 90,
            break_minutes: 10,
            horizon_days: 14,
            plan_undated: false,
            default_estimate_minutes: 60,
            max_blocks_per_task_per_day: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarConfig {
    pub enabled: bool,
    pub client_id: String,
    /// Prefer env `SB_GOOGLE_CLIENT_SECRET`; kept here only if you accept that.
    pub client_secret: String,
    pub agent_calendar_name: String,
    /// Calendars whose busy times block scheduling (free/busy only).
    pub busy_calendars: Vec<String>,
    pub redirect_port: u16,
    /// Two-way sync of tasks with Google Tasks (phone + Calendar "Tasks" view).
    pub tasks_sync: bool,
    /// Google Tasks list for tasks without a project (projects get their own list).
    pub tasks_list_name: String,
    /// Opt-in: read/delete events in your own calendars (`sb calendar auth --events`).
    pub events_access: bool,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        CalendarConfig {
            enabled: true,
            client_id: String::new(),
            client_secret: String::new(),
            agent_calendar_name: "Second Brain".into(),
            busy_calendars: vec!["primary".into()],
            redirect_port: 8765,
            tasks_sync: true,
            tasks_list_name: "Second Brain".into(),
            events_access: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomyConfig {
    /// `dry-run`: the daemon only proposes plans. `auto`: it applies plans to
    /// the database and the dedicated agent calendar.
    pub calendar_writes: String,
    pub briefing_time: String,
    /// When the daily synthesis is written (weekly on Sundays, monthly on the last day).
    pub synthesis_time: String,
    pub maintenance_time: String,
    pub calendar_sync_minutes: u64,
    pub debounce_secs: u64,
    /// Optional argv for notifications, message appended as last arg,
    /// e.g. `["notify-send", "Second Brain"]`. Never run through a shell.
    pub notify_command: Vec<String>,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        AutonomyConfig {
            calendar_writes: "dry-run".into(),
            briefing_time: "07:00".into(),
            synthesis_time: "21:30".into(),
            maintenance_time: "03:00".into(),
            calendar_sync_minutes: 15,
            debounce_secs: 3,
            notify_command: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        self.tz()?;
        for s in self.schedule.work_hours.iter().chain(&self.schedule.protected) {
            parse_window(s)?;
        }
        parse_hhmm(&self.autonomy.briefing_time)?;
        parse_hhmm(&self.autonomy.synthesis_time)?;
        parse_hhmm(&self.autonomy.maintenance_time)?;
        if !["dry-run", "auto"].contains(&self.autonomy.calendar_writes.as_str()) {
            bail!("autonomy.calendar_writes must be 'dry-run' or 'auto'");
        }
        let s = &self.schedule;
        if s.min_block_minutes <= 0 || s.max_block_minutes < s.min_block_minutes {
            bail!("schedule: need 0 < min_block_minutes <= max_block_minutes");
        }
        Ok(())
    }

    pub fn tz(&self) -> Result<chrono_tz::Tz> {
        self.timezone.parse().map_err(|_| anyhow::anyhow!("unknown timezone '{}'", self.timezone))
    }

    pub fn windows(&self, protected: bool) -> Vec<Window> {
        let src = if protected { &self.schedule.protected } else { &self.schedule.work_hours };
        src.iter().filter_map(|s| parse_window(s).ok()).flatten().collect()
    }
}

/// A weekly recurring time window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub day: Weekday,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

pub fn parse_hhmm(s: &str) -> Result<NaiveTime> {
    let s = s.trim();
    if s == "24:00" {
        return Ok(NaiveTime::from_hms_opt(23, 59, 59).unwrap());
    }
    NaiveTime::parse_from_str(s, "%H:%M").with_context(|| format!("bad time '{s}' (want HH:MM)"))
}

fn parse_day(s: &str) -> Result<Weekday> {
    Ok(match &s.to_ascii_lowercase()[..] {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => bail!("bad weekday '{s}' (use mon..sun)"),
    })
}

/// Parse `"mon-fri 09:00-12:30"`, `"sat 10:00-12:00"`, `"daily 22:00-24:00"`
/// or `"mon,wed 18:00-20:00"` into one window per day.
pub fn parse_window(s: &str) -> Result<Vec<Window>> {
    let (days, times) = s.trim().split_once(' ').with_context(|| format!("bad window '{s}'"))?;
    let (a, b) = times.trim().split_once('-').with_context(|| format!("bad window times '{s}'"))?;
    let (start, end) = (parse_hhmm(a)?, parse_hhmm(b)?);
    if end <= start {
        bail!("window '{s}' ends before it starts");
    }
    let mut out_days = vec![];
    for part in days.split(',') {
        let part = part.trim();
        if part == "daily" {
            out_days.extend([
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ]);
        } else if let Some((x, y)) = part.split_once('-') {
            let (mut d, end_d) = (parse_day(x)?, parse_day(y)?);
            loop {
                out_days.push(d);
                if d == end_d {
                    break;
                }
                d = d.succ();
            }
        } else {
            out_days.push(parse_day(part)?);
        }
    }
    Ok(out_days.into_iter().map(|day| Window { day, start, end }).collect())
}

pub const DEFAULT_CONFIG: &str = r#"# Second Brain configuration. All keys are optional.
# IANA timezone (`sb init` writes the system one here).
timezone = "UTC"
# Language of generated notes, themes and syntheses (proper nouns/tech terms stay as written): "en", "fr", …
language = "en"
# Language of the interface (sb dash, graph and syntheses pages): "en" or "fr" (key L in sb dash).
ui_language = "en"
# Tasks written without +project go to this project, e.g. "inbox" ("" = none).
default_project = ""

[provider]
# claude | codex | anthropic-api | none
kind = "claude"
model = "haiku"
timeout_secs = 120
max_retries = 1

[budget]
max_calls_per_day = 40
max_input_tokens_per_day = 200000
# A whole day's note is read in one call so the model sees full context.
max_batch_tokens = 12000

[ingest]
llm_extraction = true
# After new items appear, a reflection pass links them to everything else (1 call).
auto_connect = true
min_block_words = 1

[schedule]
work_hours = ["mon-fri 09:00-12:30", "mon-fri 14:00-18:00", "sat 10:00-12:00"]
protected = ["sun 00:00-24:00", "daily 20:00-24:00"]
max_focus_minutes_per_day = 300
min_block_minutes = 30
max_block_minutes = 90
break_minutes = 10
horizon_days = 14
default_estimate_minutes = 60
# Only tasks with a due date get work sessions in the calendar.
plan_undated = false

[calendar]
# Connect with `sb calendar auth` (guided). Set to false to pause calendar use.
enabled = true
agent_calendar_name = "Second Brain"
busy_calendars = ["primary"]
# Two-way Google Tasks sync: one list per project + this default list.
tasks_sync = true
tasks_list_name = "Second Brain"

[autonomy]
# dry-run: plans are only proposed. auto: applied to the agent calendar.
calendar_writes = "dry-run"
briefing_time = "07:00"
# Daily synthesis (weekly on Sundays, monthly on the last day of the month).
synthesis_time = "21:30"
maintenance_time = "03:00"
calendar_sync_minutes = 15
notify_command = []
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows() {
        let w = parse_window("mon-fri 09:00-12:30").unwrap();
        assert_eq!(w.len(), 5);
        assert_eq!(w[4].day, Weekday::Fri);
        assert_eq!(parse_window("daily 20:00-24:00").unwrap().len(), 7);
        assert_eq!(parse_window("sat,sun 10:00-11:00").unwrap().len(), 2);
        assert!(parse_window("mon 12:00-10:00").is_err());
    }

    #[test]
    fn default_config_parses() {
        let c: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        c.validate().unwrap();
        assert_eq!(c.windows(true).len(), 8);
    }
}
