//! Google Calendar integration.
//!
//! Scopes (minimum needed):
//! - `calendar.freebusy`: read busy intervals only (no titles) of your calendars.
//! - `calendar.app.created`: create ONE dedicated calendar and manage events in it.
//! - `tasks`: Google Tasks (no narrower scope exists); `gtasks.rs` only touches
//!   the lists it created itself.
//!
//! The app therefore *cannot* modify your meetings or personal events - that is
//! enforced by Google, not just by this code. Work sessions live in the
//! dedicated calendar; if you delete or move one there, the next sync honours it.

use crate::actions;
use crate::brain::Brain;
use crate::db::{kv_get, kv_set};
use crate::planner::{Iv, sessions_between, task_title};
use crate::util::{parse_ts, sha256_hex, ts};
use anyhow::{Context, Result, bail};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

pub const SCOPES: &str = "https://www.googleapis.com/auth/calendar.app.created https://www.googleapis.com/auth/calendar.freebusy https://www.googleapis.com/auth/tasks";
/// Google Tasks scope (no narrower one exists; the code only touches lists it created).
pub const TASKS_SCOPE: &str = "https://www.googleapis.com/auth/tasks";
const API: &str = "https://www.googleapis.com/calendar/v3";

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEvent {
    pub id: String,
    pub cancelled: bool,
    pub iv: Option<Iv>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: i64,
    /// Granted scopes (space separated); absent in tokens from older versions.
    #[serde(default)]
    scope: Option<String>,
}

pub struct Google {
    http: reqwest::blocking::Client,
    token: RefCell<Token>,
    token_path: PathBuf,
    client_id: String,
    client_secret: String,
}

#[derive(Default)]
pub struct FakeCal {
    pub calendars: Vec<String>,
    pub events: HashMap<String, (String, Iv, bool)>,
    pub busy: Vec<Iv>,
    pub seq: u64,
    pub changed: Vec<(u64, String)>,
    pub fail: bool,
    /// Google Tasks: list id -> name, and task id -> (list id, task).
    pub lists: Vec<(String, String)>,
    pub tasks: HashMap<String, (String, crate::gtasks::RemoteTask)>,
}

pub enum Backend {
    Google(Google),
    Fake(RefCell<FakeCal>),
}

/// OAuth client credentials: config/env first, else `data/secrets/google_client.json`
/// (written by the `sb calendar auth` setup wizard).
fn client_creds(brain: &Brain) -> Option<(String, String)> {
    let c = &brain.cfg.calendar;
    if !c.client_id.is_empty() {
        let secret = std::env::var("SB_GOOGLE_CLIENT_SECRET").unwrap_or_else(|_| c.client_secret.clone());
        return Some((c.client_id.clone(), secret));
    }
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(brain.secrets_dir().join("google_client.json")).ok()?).ok()?;
    Some((v["client_id"].as_str()?.to_string(), v["client_secret"].as_str().unwrap_or("").to_string()))
}

/// Accept Google's downloaded `client_secret_*.json` ({"installed": {...}}) or raw fields.
fn parse_client_json(text: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let inner = if v["installed"].is_object() {
        &v["installed"]
    } else if v["web"].is_object() {
        &v["web"]
    } else {
        &v
    };
    Some((inner["client_id"].as_str()?.to_string(), inner["client_secret"].as_str().unwrap_or("").to_string()))
}

fn open_browser(url: &str) -> bool {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
}

/// One-time wizard: create the OAuth client in Google Cloud and store it.
fn setup_client(brain: &Brain) -> Result<(String, String)> {
    println!("\n== One-time Google setup (~3 min) ==");
    println!("Google requires every app to have its own OAuth client. Create yours once:\n");
    let steps = [
        ("Create a project (any name, e.g. second-brain)", "https://console.cloud.google.com/projectcreate"),
        (
            "Enable the Google Calendar API (click 'Enable')",
            "https://console.cloud.google.com/apis/library/calendar-json.googleapis.com",
        ),
        (
            "Enable the Google Tasks API (click 'Enable')",
            "https://console.cloud.google.com/apis/library/tasks.googleapis.com",
        ),
        (
            "Configure the consent screen: 'Get started', app name, your email, audience 'External'",
            "https://console.cloud.google.com/auth/overview",
        ),
        (
            "Add YOUR Gmail address as a test user ('Audience' > 'Add users')",
            "https://console.cloud.google.com/auth/audience",
        ),
        (
            "Create client: type 'Desktop app' > Create > 'Download JSON'",
            "https://console.cloud.google.com/auth/clients/create",
        ),
    ];
    for (i, (what, url)) in steps.iter().enumerate() {
        println!("{}. {what}\n   {url}", i + 1);
    }
    if prompt("\nPress Enter to open these pages in your browser (or type 's' to skip) ")? != "s" {
        for (_, url) in steps.iter().rev() {
            open_browser(url);
        }
    }
    let creds = loop {
        let input = prompt("\nPath to the downloaded JSON (drag & drop the file here), or the client ID: ")?;
        let path = std::path::Path::new(&input);
        let parsed = if path.is_file() {
            parse_client_json(&std::fs::read_to_string(path)?)
        } else if input.ends_with(".apps.googleusercontent.com") {
            let secret = prompt("Client secret: ")?;
            Some((input.clone(), secret))
        } else {
            None
        };
        match parsed {
            Some((id, secret))
                if secret.is_empty() || secret == id || secret.contains(".apps.googleusercontent.com") =>
            {
                println!(
                    "That secret looks wrong (it is empty or is the client ID). Easiest: give the path of the downloaded JSON file."
                )
            }
            Some(c) => break c,
            None => println!(
                "Not recognised: expected the downloaded client_secret_*.json or an ID ending in .apps.googleusercontent.com"
            ),
        }
    };
    write_secret(
        &brain.secrets_dir().join("google_client.json"),
        &serde_json::to_string(&json!({"client_id": creds.0, "client_secret": creds.1}))?,
    )?;
    println!("Saved client credentials to data/secrets/google_client.json (mode 600).");
    Ok(creds)
}

/// Turn `[calendar] enabled = false` on in config.toml after a successful auth.
fn enable_in_config(brain: &Brain) -> Result<()> {
    let path = brain.home.join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(()) };
    if let Some(sec) = text.find("[calendar]")
        && let Some(rel) = text[sec..].find("enabled = false")
    {
        let at = sec + rel;
        let next_section = text[sec + 1..].find("\n[").map(|i| sec + 1 + i).unwrap_or(text.len());
        if at < next_section {
            let new = format!("{}enabled = true{}", &text[..at], &text[at + "enabled = false".len()..]);
            crate::util::atomic_write(&path, &new)?;
        }
    }
    Ok(())
}

fn token_path(brain: &Brain) -> PathBuf {
    brain.secrets_dir().join("google_token.json")
}

fn write_secret(path: &std::path::Path, content: &str) -> Result<()> {
    crate::util::atomic_write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        if let Some(d) = path.parent() {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

impl Backend {
    /// Connected Google backend, or None when calendar is disabled / not authorized.
    pub fn google(brain: &Brain) -> Result<Option<Backend>> {
        let p = token_path(brain);
        if !brain.cfg.calendar.enabled || !p.exists() {
            return Ok(None);
        }
        let Some((client_id, client_secret)) = client_creds(brain) else { return Ok(None) };
        let token: Token = serde_json::from_str(&std::fs::read_to_string(&p)?)
            .context("token file corrupt; re-run `sb calendar auth`")?;
        Ok(Some(Backend::Google(Google {
            http: reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?,
            token: RefCell::new(token),
            token_path: p,
            client_id,
            client_secret,
        })))
    }

    pub fn create_calendar(&self, name: &str, tz: &str) -> Result<String> {
        match self {
            Backend::Fake(f) => {
                let mut f = f.borrow_mut();
                let id = format!("fakecal-{}", f.calendars.len());
                f.calendars.push(id.clone());
                Ok(id)
            }
            Backend::Google(g) => {
                let v = g.req("POST", &format!("{API}/calendars"), Some(json!({"summary": name, "timeZone": tz})))?;
                Ok(v["id"].as_str().context("no calendar id")?.to_string())
            }
        }
    }

    pub fn freebusy(&self, calendars: &[String], from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Iv>> {
        match self {
            Backend::Fake(f) => {
                let f = f.borrow();
                if f.fail {
                    bail!("network unreachable");
                }
                Ok(f.busy.iter().filter(|b| b.end > from && b.start < to).copied().collect())
            }
            Backend::Google(g) => {
                let items: Vec<Value> = calendars.iter().map(|c| json!({"id": c})).collect();
                let v = g.req(
                    "POST",
                    &format!("{API}/freeBusy"),
                    Some(json!({"timeMin": ts(from), "timeMax": ts(to), "items": items})),
                )?;
                let mut out = vec![];
                for c in calendars {
                    let cal = &v["calendars"][c];
                    if let Some(errs) = cal["errors"].as_array().filter(|e| !e.is_empty()) {
                        bail!("freeBusy error for {c}: {}", Value::Array(errs.clone()));
                    }
                    for b in cal["busy"].as_array().into_iter().flatten() {
                        out.push(Iv {
                            start: parse_ts(b["start"].as_str().unwrap_or(""))?,
                            end: parse_ts(b["end"].as_str().unwrap_or(""))?,
                        });
                    }
                }
                Ok(out)
            }
        }
    }

    pub fn insert_event(&self, cal: &str, summary: &str, desc: &str, iv: Iv, session_id: i64) -> Result<String> {
        match self {
            Backend::Fake(f) => {
                let mut f = f.borrow_mut();
                if f.fail {
                    bail!("network unreachable");
                }
                f.seq += 1;
                let id = format!("ev{}", f.seq);
                f.events.insert(id.clone(), (cal.to_string(), iv, false));
                let s = f.seq;
                f.changed.push((s, id.clone()));
                Ok(id)
            }
            Backend::Google(g) => {
                let body = json!({
                    "summary": summary, "description": desc,
                    "start": {"dateTime": ts(iv.start)}, "end": {"dateTime": ts(iv.end)},
                    "reminders": {"useDefault": false},
                    "extendedProperties": {"private": {"sb_session": session_id.to_string()}}
                });
                let v = g.req("POST", &events_url(cal, None)?, Some(body))?;
                Ok(v["id"].as_str().context("no event id")?.to_string())
            }
        }
    }

    pub fn delete_event(&self, cal: &str, id: &str) -> Result<()> {
        match self {
            Backend::Fake(f) => {
                let mut f = f.borrow_mut();
                if f.fail {
                    bail!("network unreachable");
                }
                f.seq += 1;
                let s = f.seq;
                if let Some(e) = f.events.get_mut(id) {
                    e.2 = true;
                }
                f.changed.push((s, id.to_string()));
                Ok(())
            }
            Backend::Google(g) => match g.req("DELETE", &events_url(cal, Some(id))?, None) {
                Ok(_) => Ok(()),
                Err(e) if e.to_string().contains("404") || e.to_string().contains("410") => Ok(()),
                Err(e) => Err(e),
            },
        }
    }

    /// Changes in the agent calendar since `sync_token` (None = full listing).
    /// Returns Err containing "410" when the token expired.
    pub fn changes(&self, cal: &str, sync_token: Option<&str>) -> Result<(Vec<RemoteEvent>, String)> {
        match self {
            Backend::Fake(f) => {
                let f = f.borrow();
                if f.fail {
                    bail!("network unreachable");
                }
                let since: u64 = sync_token.and_then(|t| t.parse().ok()).unwrap_or(0);
                let mut ids: Vec<&String> = f.changed.iter().filter(|(s, _)| *s > since).map(|(_, id)| id).collect();
                ids.dedup();
                let evs = ids
                    .into_iter()
                    .filter_map(|id| {
                        f.events.get(id).filter(|e| e.0 == cal).map(|e| RemoteEvent {
                            id: id.clone(),
                            cancelled: e.2,
                            iv: Some(e.1),
                        })
                    })
                    .collect();
                Ok((evs, f.seq.to_string()))
            }
            Backend::Google(g) => {
                let mut out = vec![];
                let mut page: Option<String> = None;
                loop {
                    let mut url = reqwest::Url::parse(&events_url(cal, None)?)?;
                    {
                        let mut q = url.query_pairs_mut();
                        q.append_pair("showDeleted", "true").append_pair("maxResults", "250");
                        if let Some(t) = sync_token {
                            q.append_pair("syncToken", t);
                        }
                        if let Some(p) = &page {
                            q.append_pair("pageToken", p);
                        }
                    }
                    let v = g.req("GET", url.as_str(), None)?;
                    for e in v["items"].as_array().into_iter().flatten() {
                        let iv = match (e["start"]["dateTime"].as_str(), e["end"]["dateTime"].as_str()) {
                            (Some(s), Some(en)) => Some(Iv { start: parse_ts(s)?, end: parse_ts(en)? }),
                            _ => None,
                        };
                        out.push(RemoteEvent {
                            id: e["id"].as_str().unwrap_or("").to_string(),
                            cancelled: e["status"] == "cancelled",
                            iv,
                        });
                    }
                    if let Some(p) = v["nextPageToken"].as_str() {
                        page = Some(p.to_string());
                        continue;
                    }
                    let token = v["nextSyncToken"].as_str().context("no nextSyncToken")?.to_string();
                    return Ok((out, token));
                }
            }
        }
    }
}

fn events_url(cal: &str, event: Option<&str>) -> Result<String> {
    let mut url = reqwest::Url::parse(API)?;
    {
        let mut seg = url.path_segments_mut().map_err(|_| anyhow::anyhow!("bad url"))?;
        seg.push("calendars").push(cal).push("events");
        if let Some(e) = event {
            seg.push(e);
        }
    }
    Ok(url.to_string())
}

impl Google {
    fn access_token(&self) -> Result<String> {
        if self.token.borrow().expires_at > Utc::now().timestamp() + 60 {
            return Ok(self.token.borrow().access_token.clone());
        }
        self.refresh()
    }

    fn refresh(&self) -> Result<String> {
        let refresh = self.token.borrow().refresh_token.clone().context("no refresh token; run `sb calendar auth`")?;
        let resp = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", refresh.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .context("token refresh request failed (network?)")?;
        let status = resp.status();
        let v: Value = resp.json()?;
        if !status.is_success() {
            // Never log token material; error code only.
            bail!(
                "token refresh failed ({status}): {} - run `sb calendar auth`",
                v["error"].as_str().unwrap_or("unknown")
            );
        }
        let mut t = self.token.borrow_mut();
        t.access_token = v["access_token"].as_str().context("no access_token")?.to_string();
        t.expires_at = Utc::now().timestamp() + v["expires_in"].as_i64().unwrap_or(3600);
        if let Some(sc) = v["scope"].as_str() {
            t.scope = Some(sc.to_string());
        }
        write_secret(&self.token_path, &serde_json::to_string(&*t)?)?;
        Ok(t.access_token.clone())
    }

    /// True when the stored authorization includes Google Tasks.
    pub fn has_tasks_scope(&self) -> bool {
        self.has_scope(TASKS_SCOPE)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.token.borrow().scope.as_deref().is_some_and(|s| s.split(' ').any(|x| x == scope))
    }

    pub(crate) fn req(&self, method: &str, url: &str, body: Option<Value>) -> Result<Value> {
        for attempt in 0..2 {
            let tok = if attempt == 0 { self.access_token()? } else { self.refresh()? };
            let rb = match method {
                "GET" => self.http.get(url),
                "POST" => self.http.post(url),
                "DELETE" => self.http.delete(url),
                "PATCH" => self.http.patch(url),
                _ => bail!("bad method"),
            };
            let rb = rb.bearer_auth(tok);
            let rb = if let Some(b) = &body { rb.json(b) } else { rb };
            let resp = rb.send().with_context(|| format!("{method} Google API (network?)"))?;
            let status = resp.status();
            if status.as_u16() == 401 && attempt == 0 {
                continue;
            }
            let text = resp.text().unwrap_or_default();
            if !status.is_success() {
                bail!("Google API {status}: {}", crate::util::truncate_chars(&text, 300));
            }
            return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text)? });
        }
        bail!("calendar API unauthorized after token refresh")
    }
}

// ---------------------------------------------------------------------------
// OAuth (installed app, loopback redirect, PKCE)

fn random_b64(n: usize) -> Result<String> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).map_err(|e| anyhow::anyhow!("rng: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

/// `reset`: forget stored client credentials and run the setup wizard again.
pub fn authorize(brain: &Brain, reset: bool) -> Result<()> {
    let c = &brain.cfg.calendar;
    let creds = if reset { None } else { client_creds(brain) };
    let (client_id, secret) = match creds {
        Some(x) => x,
        None => setup_client(brain)?,
    };
    // The broad "all your events" scope is opt-in (`sb calendar auth --events`).
    let scopes =
        if c.events_access { format!("{SCOPES} {}", crate::gcal_admin::EVENTS_SCOPE) } else { SCOPES.to_string() };
    let verifier = random_b64(48)?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hex::decode(sha256_hex(&verifier))?);
    let state = random_b64(16)?;
    let redirect = format!("http://127.0.0.1:{}", c.redirect_port);
    let url = reqwest::Url::parse_with_params(
        "https://accounts.google.com/o/oauth2/v2/auth",
        &[
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", scopes.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", c.redirect_port))
        .with_context(|| format!("port {} busy", c.redirect_port))?;
    let opened = open_browser(url.as_str());
    println!(
        "\n{}Sign in with Google and click 'Allow'.\n(If no browser opened, open this link:)\n{url}\n\nWaiting...",
        if opened { "Your browser is opening. " } else { "" }
    );
    // Ignore unrelated requests (e.g. /favicon.ico) until the OAuth redirect arrives.
    let (mut stream, q) = loop {
        let (stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let cb = reqwest::Url::parse(&format!("http://localhost{path}"))?;
        let q: HashMap<String, String> = cb.query_pairs().into_owned().collect();
        if q.contains_key("code") || q.contains_key("error") {
            break (stream, q);
        }
    };
    let ok = q.get("state") == Some(&state) && q.contains_key("code");
    let page = if ok {
        "Second Brain is connected. You can close this tab."
    } else {
        "Authorization failed. Check the terminal."
    };
    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{page}");
    if !ok {
        let err = q.get("error").map(String::as_str).unwrap_or("state mismatch");
        if err == "access_denied" {
            bail!("access denied - is your Gmail address listed as a test user? (step 4 of the setup)");
        }
        bail!("authorization failed: {err}");
    }
    let resp = reqwest::blocking::Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", q["code"].as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", secret.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .send()?;
    let status = resp.status();
    let v: Value = resp.json()?;
    if !status.is_success() {
        bail!(
            "token exchange failed ({status}): {} - if the client is wrong, run `sb calendar auth --reset`",
            v["error"].as_str().unwrap_or("unknown")
        );
    }
    let token = Token {
        access_token: v["access_token"].as_str().context("no access_token")?.into(),
        refresh_token: v["refresh_token"].as_str().map(String::from),
        expires_at: Utc::now().timestamp() + v["expires_in"].as_i64().unwrap_or(3600),
        scope: v["scope"].as_str().map(String::from),
    };
    write_secret(&token_path(brain), &serde_json::to_string(&token)?)?;
    enable_in_config(brain)?;
    actions::log(&brain.conn, "user", "calendar_auth", "Google Calendar authorized", None)?;
    if !token.scope.as_deref().unwrap_or(SCOPES).contains(TASKS_SCOPE) {
        println!("note: Google Tasks access was not granted (tick it on the consent screen to sync tasks).");
    }
    println!("Connected to Google Calendar. Token stored in {} (mode 600).", token_path(brain).display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Synchronization

#[derive(Debug, Default)]
pub struct SyncReport {
    pub busy_intervals: usize,
    pub busy_changed: bool,
    pub user_deleted: usize,
    pub user_moved: usize,
    pub pushed: usize,
    pub removed: usize,
}

pub fn agent_calendar(brain: &Brain, be: &Backend) -> Result<String> {
    if let Some(id) = kv_get(&brain.conn, "gcal_agent_calendar")? {
        return Ok(id);
    }
    let id = be.create_calendar(&brain.cfg.calendar.agent_calendar_name, &brain.cfg.timezone)?;
    kv_set(&brain.conn, "gcal_agent_calendar", &id)?;
    actions::log(
        &brain.conn,
        "system",
        "calendar_created",
        &format!("created dedicated calendar '{}'", brain.cfg.calendar.agent_calendar_name),
        None,
    )?;
    Ok(id)
}

/// Pull busy time + user edits of agent events; push planned sessions if `push`.
pub fn sync(brain: &Brain, be: &Backend, push: bool) -> Result<SyncReport> {
    let mut rep = SyncReport::default();
    let now = brain.now();
    let horizon = now + Duration::days(brain.cfg.schedule.horizon_days + 1);
    crate::ui::status("Google Calendar · reading busy times");
    let cal = agent_calendar(brain, be)?;

    // 1. Busy time from the user's calendars (free/busy only, no titles).
    let busy = be.freebusy(&brain.cfg.calendar.busy_calendars, now - Duration::days(1), horizon)?;
    let fp = sha256_hex(&busy.iter().map(|b| format!("{}-{}", ts(b.start), ts(b.end))).collect::<Vec<_>>().join(","));
    rep.busy_intervals = busy.len();
    let tx = brain.conn.unchecked_transaction()?;
    if kv_get(&tx, "gcal_busy_fp")?.as_deref() != Some(fp.as_str()) {
        rep.busy_changed = true;
        tx.execute("DELETE FROM busy WHERE source='gcal'", [])?;
        for b in &busy {
            tx.execute(
                "INSERT INTO busy(source, start, end, title) VALUES ('gcal', ?1, ?2, NULL)",
                params![ts(b.start), ts(b.end)],
            )?;
        }
        kv_set(&tx, "gcal_busy_fp", &fp)?;
    }
    tx.commit()?;

    crate::ui::status("Google Calendar · checking your edits");
    // 2. User edits to agent-managed events (incremental via sync token).
    let token = kv_get(&brain.conn, "gcal_sync_token")?;
    let (events, next) = match be.changes(&cal, token.as_deref()) {
        Err(e) if e.to_string().contains("410") => be.changes(&cal, None)?,
        r => r?,
    };
    let tx = brain.conn.unchecked_transaction()?;
    for ev in events {
        let row: Option<(i64, String, String, String)> = tx
            .query_row("SELECT id, status, start, end FROM sessions WHERE gcal_id=?1", [&ev.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        if ev.cancelled {
            let n = tx.execute("DELETE FROM busy WHERE source=?1", [format!("block:{}", ev.id)])?;
            if n > 0 {
                actions::log(&tx, "user", "calendar_unblock", "blocked period deleted in Google Calendar", None)?;
                rep.busy_changed = true;
            }
        }
        let Some((sid, status, s, e)) = row else { continue };
        if status != "planned" {
            continue;
        }
        if ev.cancelled {
            // User removed a work block: respect it and keep that time free of new blocks.
            tx.execute("UPDATE sessions SET status='cancelled', gcal_id=NULL WHERE id=?1", [sid])?;
            tx.execute(
                "INSERT INTO busy(source, start, end, title) VALUES ('declined', ?1, ?2, 'declined work block')",
                params![s, e],
            )?;
            actions::log(
                &tx,
                "user",
                "session_removed_in_calendar",
                &format!("session #{sid} deleted in Google Calendar"),
                None,
            )?;
            rep.user_deleted += 1;
        } else if let Some(iv) = ev.iv
            && (ts(iv.start) != s || ts(iv.end) != e)
        {
            tx.execute("UPDATE sessions SET start=?2, end=?3 WHERE id=?1", params![sid, ts(iv.start), ts(iv.end)])?;
            actions::log(
                &tx,
                "user",
                "session_moved_in_calendar",
                &format!("session #{sid} moved to {}", crate::planner::fmt_iv(brain, &iv)),
                None,
            )?;
            rep.user_moved += 1;
        }
    }
    kv_set(&tx, "gcal_sync_token", &next)?;
    tx.commit()?;

    if !push {
        return Ok(rep);
    }
    crate::ui::status("Google Calendar · writing work sessions");
    // Events of deleted tasks' sessions.
    rep.removed += crate::tasks::process_tombstones(&brain.conn, "gcal_tombstones", |e| match e.as_str() {
        Some(id) => be.delete_event(&cal, id),
        None => Ok(()),
    })?;
    // 3. Push: remove events of cancelled sessions, create events for new ones.
    let stale: Vec<(i64, String)> = brain
        .conn
        .prepare("SELECT id, gcal_id FROM sessions WHERE gcal_id IS NOT NULL AND status='cancelled'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (sid, gid) in stale {
        be.delete_event(&cal, &gid)?;
        brain.conn.execute("UPDATE sessions SET gcal_id=NULL WHERE id=?1", [sid])?;
        rep.removed += 1;
    }
    for s in sessions_between(brain, now, horizon + Duration::days(60), &["planned"])? {
        if s.gcal_id.is_some() || s.iv.start < now {
            continue;
        }
        let title = task_title(brain, s.task_id);
        let gid = be.insert_event(
            &cal,
            &format!("Focus: {title}"),
            &format!(
                "Planned by Second Brain (session #{}, task #{}).\nDelete or move this event and the plan adapts.",
                s.id, s.task_id
            ),
            s.iv,
            s.id,
        )?;
        brain.conn.execute("UPDATE sessions SET gcal_id=?2 WHERE id=?1", params![s.id, gid])?;
        rep.pushed += 1;
    }
    if rep.pushed + rep.removed > 0 {
        actions::log(
            &brain.conn,
            "system",
            "calendar_push",
            &format!(
                "{} event(s) created, {} removed in '{}'",
                rep.pushed, rep.removed, brain.cfg.calendar.agent_calendar_name
            ),
            None,
        )?;
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_downloaded_client_file() {
        let f = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"p","client_secret":"GOCSPX-x","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(parse_client_json(f), Some(("123-abc.apps.googleusercontent.com".into(), "GOCSPX-x".into())));
        assert_eq!(parse_client_json("{}"), None);
    }
}
