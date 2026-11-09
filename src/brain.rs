//! The single controller handle: config + database + model provider + clock.

use crate::config::{Config, DEFAULT_CONFIG};
use crate::inference::{self, CallCtx, LlmError, Provider, Request, Response};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct Brain {
    pub home: PathBuf,
    pub cfg: Config,
    pub conn: Connection,
    pub provider: Provider,
    pub tz: Tz,
    /// Fixed clock for tests/demos (also settable with env `SB_NOW`).
    pub fixed_now: Option<DateTime<Utc>>,
}

pub const VAULT_DIRS: &[&str] = &["journal", "knowledge", "projects", "summaries", "archive"];

impl Brain {
    /// Create the directory layout and default config (idempotent, never overwrites).
    pub fn init(home: &Path) -> Result<()> {
        for d in VAULT_DIRS {
            std::fs::create_dir_all(home.join("vault").join(d))?;
        }
        std::fs::create_dir_all(home.join("data"))?;
        let cfg = home.join("config.toml");
        if !cfg.exists() {
            let tz = system_timezone().unwrap_or_else(|| "UTC".into());
            let text = DEFAULT_CONFIG.replacen("timezone = \"UTC\"", &format!("timezone = \"{tz}\""), 1);
            crate::util::atomic_write(&cfg, &text)?;
        }
        let gi = home.join(".gitignore");
        if !gi.exists() {
            crate::util::atomic_write(&gi, "data/secrets/\ndata/*.db-wal\ndata/*.db-shm\n")?;
        }
        crate::db::open(&home.join("data/brain.db"))?;
        Ok(())
    }

    pub fn open(home: &Path) -> Result<Brain> {
        if !home.join("data").exists() {
            anyhow::bail!("{} is not initialized; run `sb init` first", home.display());
        }
        let cfg = Config::load(&home.join("config.toml"))?;
        let provider = Provider::from_config(&cfg.provider)?;
        let conn = crate::db::open(&home.join("data/brain.db"))?;
        let fixed_now =
            std::env::var("SB_NOW").ok().map(|s| crate::util::parse_ts(&s)).transpose().context("SB_NOW")?;
        Ok(Brain { home: home.to_path_buf(), tz: cfg.tz()?, cfg, conn, provider, fixed_now })
    }

    /// In-memory brain rooted at `home` (tests).
    pub fn open_with(home: &Path, cfg: Config, provider: Provider, now: DateTime<Utc>) -> Result<Brain> {
        for d in VAULT_DIRS {
            std::fs::create_dir_all(home.join("vault").join(d))?;
        }
        std::fs::create_dir_all(home.join("data"))?;
        let conn = crate::db::open(&home.join("data/brain.db"))?;
        Ok(Brain { home: home.to_path_buf(), tz: cfg.tz()?, cfg, conn, provider, fixed_now: Some(now) })
    }

    pub fn vault(&self) -> PathBuf {
        self.home.join("vault")
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.fixed_now.unwrap_or_else(Utc::now)
    }

    pub fn today(&self) -> NaiveDate {
        self.now().with_timezone(&self.tz).date_naive()
    }

    /// Local midnight of `date` as UTC.
    pub fn day_start(&self, date: NaiveDate) -> DateTime<Utc> {
        let naive = date.and_hms_opt(0, 0, 0).unwrap();
        self.tz
            .from_local_datetime(&naive)
            .earliest()
            .unwrap_or_else(|| self.tz.from_utc_datetime(&naive))
            .with_timezone(&Utc)
    }

    pub fn local(&self, t: DateTime<Utc>) -> DateTime<Tz> {
        t.with_timezone(&self.tz)
    }

    pub fn llm(&self, req: &Request) -> Result<Response, LlmError> {
        let ctx = CallCtx {
            conn: &self.conn,
            provider: &self.provider,
            cfg: &self.cfg.provider,
            budget: &self.cfg.budget,
            day_start: self.day_start(self.today()),
            workdir: self.home.join("data"),
        };
        inference::call(&ctx, req)
    }

    /// Instruction appended to prompts so generated text uses the configured language.
    /// Interface language ("en" / "fr"): the dashboard's L choice (data/dash_lang) wins over config.
    pub fn ui_language(&self) -> String {
        std::fs::read_to_string(self.home.join("data/dash_lang"))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.cfg.ui_language.clone())
    }

    pub fn language_rule(&self) -> String {
        let lang = match self.cfg.language.to_lowercase().as_str() {
            "fr" | "french" | "français" => "French",
            "en" | "english" => "English",
            "es" | "spanish" => "Spanish",
            "de" | "german" => "German",
            other => {
                return format!(
                    "Write all generated text in the language '{other}'; keep proper nouns and technical terms as written. Never use em dashes or en dashes, use a plain hyphen."
                );
            }
        };
        let jargon = if lang == "English" {
            String::new()
        } else {
            format!(
                " Technical jargon: keep in English the software, computing, security and AI terms that \
                 practitioners actually say in English, even if a {lang} translation exists, e.g. garbage collector, linker, \
                 loader, scheduler, debugger, profiler, stack, query planner, rendering engine, reverse shell, beaconing, \
                 prompt, fine-tuning, framework, runtime, pipeline (in French: not ramasse-miettes, éditeur de liens, \
                 ordonnanceur, débogueur, profileur, pile, planificateur de requêtes). But keep {lang} for words with a \
                 natural everyday {lang} form (in French: moteur de recherche, algorithme de consensus, système distribué, \
                 système de logs, noyau, modèle), and write correct {lang} grammar around English terms (in French: \
                 « sa propre stack d'observabilité », « son propre garbage collector »). Acronyms, proper nouns and product \
                 names stay as written (RLHF, SSRF, Splunk, Git)."
            )
        };
        format!(
            "Write all generated text in {lang}: concept/idea titles, summaries and reasons, even when examples are \
             in English; reuse existing names exactly. Grand theme and theme names may be in {lang} or English, \
             whichever is the most natural name for that domain (e.g. IA or AI, Cybersécurité, Detection engineering, \
             Prompt engineering); stay consistent with the existing taxonomy.{jargon} Never use em dashes or en dashes; \
             use a plain hyphen \"-\", a comma or a colon instead."
        )
    }

    pub fn secrets_dir(&self) -> PathBuf {
        self.home.join("data/secrets")
    }
}

/// IANA name of the system timezone: `$TZ`, else the `/etc/localtime` link
/// (`…/zoneinfo/Europe/Paris`). Only names chrono-tz knows are returned.
pub fn system_timezone() -> Option<String> {
    let valid = |n: &str| n.parse::<chrono_tz::Tz>().is_ok().then(|| n.to_string());
    if let Some(tz) = std::env::var("TZ").ok().map(|t| t.trim_start_matches(':').to_string())
        && let Some(v) = valid(&tz)
    {
        return Some(v);
    }
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let s = link.to_string_lossy();
    valid(s.split_once("zoneinfo/")?.1)
}

#[cfg(test)]
mod init_tests {
    #[test]
    fn init_writes_a_valid_timezone() {
        let dir = tempfile::tempdir().unwrap();
        super::Brain::init(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        let c: crate::config::Config = toml::from_str(&text).unwrap();
        assert!(c.timezone.parse::<chrono_tz::Tz>().is_ok(), "{}", c.timezone);
        assert!(c.default_project.is_empty());
        assert_eq!(c.language, "en");
    }
}
