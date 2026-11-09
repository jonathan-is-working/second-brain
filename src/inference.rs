//! Provider-independent model access: Claude CLI, Codex CLI, Anthropic API, a
//! mock for tests, or none. Every call goes through [`call`], which enforces
//! the daily budget, caches by prompt hash, bounds retries, and records usage.
//!
//! The model never gets tools: Claude runs with `--tools ""`, Codex with a
//! read-only sandbox. Output is text that the caller parses and validates.

use crate::config::{BudgetConfig, ProviderConfig};
use crate::util::{est_tokens, sha256_hex, truncate_chars, ts};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type MockFn = Arc<dyn Fn(&Request) -> Result<String> + Send + Sync>;

#[derive(Clone)]
pub enum Provider {
    ClaudeCli { cmd: String, model: String },
    CodexCli { cmd: String, model: String },
    AnthropicApi { model: String, key_env: String },
    Mock(MockFn),
    None,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub purpose: &'static str,
    pub system: String,
    pub prompt: String,
    pub max_output_tokens: u32,
    /// Output must contain a JSON object; malformed output triggers a bounded retry.
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated: bool,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("no model provider configured")]
    Unavailable,
    #[error("model budget exhausted: {0}")]
    Budget(String),
    #[error("model call failed: {0}")]
    Failed(String),
}

impl Provider {
    pub fn from_config(cfg: &ProviderConfig) -> Result<Provider> {
        let model = cfg.model.clone();
        Ok(match cfg.kind.as_str() {
            "claude" => Provider::ClaudeCli { cmd: cfg.command.clone().unwrap_or("claude".into()), model },
            "codex" => Provider::CodexCli {
                cmd: cfg.command.clone().unwrap_or("codex".into()),
                // Claude aliases make no sense for codex: fall back to its default model.
                model: if ["haiku", "sonnet", "opus"].contains(&model.as_str()) { String::new() } else { model },
            },
            "anthropic-api" => Provider::AnthropicApi {
                model: if model == "haiku" { "claude-haiku-5-5".into() } else { model },
                key_env: cfg.api_key_env.clone(),
            },
            "none" | "" => Provider::None,
            other => anyhow::bail!("unknown provider kind '{other}'"),
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Provider::ClaudeCli { .. } => "claude-cli",
            Provider::CodexCli { .. } => "codex-cli",
            Provider::AnthropicApi { .. } => "anthropic-api",
            Provider::Mock(_) => "mock",
            Provider::None => "none",
        }
    }

    pub fn model(&self) -> String {
        match self {
            Provider::ClaudeCli { model, .. }
            | Provider::CodexCli { model, .. }
            | Provider::AnthropicApi { model, .. } => model.clone(),
            _ => String::new(),
        }
    }

    pub fn available(&self) -> bool {
        !matches!(self, Provider::None)
    }

    fn complete(&self, req: &Request, timeout: Duration, workdir: &Path) -> Result<Response> {
        match self {
            Provider::None => Err(LlmError::Unavailable.into()),
            Provider::Mock(f) => {
                let text = f(req)?;
                Ok(Response {
                    input_tokens: est_tokens(&req.system) as u64 + est_tokens(&req.prompt) as u64,
                    output_tokens: est_tokens(&text) as u64,
                    text,
                    estimated: true,
                    cost_usd: None,
                })
            }
            Provider::ClaudeCli { cmd, model } => {
                let mut args: Vec<String> = [
                    "-p",
                    "--output-format",
                    "json",
                    "--tools",
                    "",
                    "--no-session-persistence",
                    "--setting-sources",
                    "",
                    "--strict-mcp-config",
                    "--disable-slash-commands",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                if !model.is_empty() {
                    args.extend(["--model".into(), model.clone()]);
                }
                args.extend(["--system-prompt".into(), req.system.clone()]);
                let out = run_command(cmd, &args, &req.prompt, timeout, workdir)?;
                parse_claude_json(&out)
            }
            Provider::CodexCli { cmd, model } => {
                let mut args: Vec<String> =
                    ["exec", "--ephemeral", "--skip-git-repo-check", "--sandbox", "read-only", "--json"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                if !model.is_empty() {
                    args.extend(["-m".into(), model.clone()]);
                }
                args.push("-".into());
                // Codex has no system-prompt flag; prepend it.
                let input = format!(
                    "{}\n\nDo not run any commands or read files; answer directly.\n\n{}",
                    req.system, req.prompt
                );
                let out = run_command(cmd, &args, &input, timeout, workdir)?;
                parse_codex_jsonl(&out)
            }
            Provider::AnthropicApi { model, key_env } => {
                let key = std::env::var(key_env).map_err(|_| anyhow!("env {key_env} not set"))?;
                let client = reqwest::blocking::Client::builder().timeout(timeout).build()?;
                let body = serde_json::json!({
                    "model": model,
                    "max_tokens": req.max_output_tokens,
                    "system": req.system,
                    "messages": [{"role": "user", "content": req.prompt}],
                });
                let resp = client
                    .post("https://api.anthropic.com/v1/messages")
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
                    .send()?;
                let status = resp.status();
                let v: Value = resp.json()?;
                if !status.is_success() {
                    anyhow::bail!("API error {status}: {}", truncate_chars(&v.to_string(), 300));
                }
                let text = v["content"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join(""))
                    .unwrap_or_default();
                Ok(Response {
                    text,
                    input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
                    estimated: false,
                    cost_usd: None,
                })
            }
        }
    }
}

/// Run a program (no shell) with stdin input and a hard timeout.
fn run_command(cmd: &str, args: &[String], input: &str, timeout: Duration, cwd: &Path) -> Result<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting '{cmd}' (is it installed and on PATH?)"))?;
    let mut stdin = child.stdin.take().context("stdin")?;
    let input = input.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("'{cmd}' timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = writer.join();
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    if !status.success() {
        anyhow::bail!(
            "'{cmd}' exited with {status}: {}",
            truncate_chars(if err.trim().is_empty() { &out } else { &err }, 400)
        );
    }
    Ok(out)
}

pub fn parse_claude_json(out: &str) -> Result<Response> {
    let v: Value = serde_json::from_str(out.trim()).context("claude CLI did not return JSON")?;
    if v["is_error"].as_bool() == Some(true) {
        anyhow::bail!("claude CLI error: {}", truncate_chars(v["result"].as_str().unwrap_or(""), 300));
    }
    let u = &v["usage"];
    let input = u["input_tokens"].as_u64().unwrap_or(0)
        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
        + u["cache_read_input_tokens"].as_u64().unwrap_or(0);
    Ok(Response {
        text: v["result"].as_str().context("claude CLI JSON has no result")?.to_string(),
        input_tokens: input,
        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
        estimated: u.is_null(),
        cost_usd: v["total_cost_usd"].as_f64(),
    })
}

pub fn parse_codex_jsonl(out: &str) -> Result<Response> {
    let mut text = None;
    let (mut input, mut output, mut seen_usage) = (0, 0, false);
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v["type"].as_str() {
            Some("item.completed") if v["item"]["type"] == "agent_message" => {
                text = v["item"]["text"].as_str().map(String::from);
            }
            Some("turn.completed") => {
                input += v["usage"]["input_tokens"].as_u64().unwrap_or(0);
                output += v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                seen_usage = true;
            }
            Some("turn.failed") | Some("error") => {
                anyhow::bail!("codex error: {}", truncate_chars(&v.to_string(), 300));
            }
            _ => {}
        }
    }
    Ok(Response {
        text: text.context("codex produced no agent message")?,
        input_tokens: input,
        output_tokens: output,
        estimated: !seen_usage,
        cost_usd: None,
    })
}

/// Find the first top-level JSON object in model output (tolerates code fences / prose).
pub fn extract_json(text: &str) -> Result<Value> {
    let start = text.find('{').context("no JSON object in model output")?;
    let mut depth = 0i32;
    let (mut in_str, mut esc) = (false, false);
    for (i, c) in text[start..].char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(serde_json::from_str(&text[start..start + i + 1])?);
                }
            }
            _ => {}
        }
    }
    anyhow::bail!("unterminated JSON object in model output")
}

pub struct CallCtx<'a> {
    pub conn: &'a Connection,
    pub provider: &'a Provider,
    pub cfg: &'a ProviderConfig,
    pub budget: &'a BudgetConfig,
    /// Start of the current budget day (local midnight, in UTC).
    pub day_start: DateTime<Utc>,
    pub workdir: PathBuf,
}

/// Budget-checked, cached, logged, retry-bounded model call.
/// Model text never contains em/en dashes: they become a plain "-".
pub fn no_long_dashes(s: &str) -> String {
    s.replace(['\u{2014}', '\u{2013}', '\u{2012}', '\u{2015}'], "-")
}

pub fn call(ctx: &CallCtx, req: &Request) -> Result<Response, LlmError> {
    if !ctx.provider.available() {
        return Err(LlmError::Unavailable);
    }
    let cache_key =
        sha256_hex(&format!("{}\0{}\0{}\0{}", ctx.provider.name(), ctx.provider.model(), req.system, req.prompt));
    let cached: Option<String> = ctx
        .conn
        .query_row("SELECT response FROM llm_cache WHERE key = ?1", [&cache_key], |r| r.get(0))
        .optional()
        .map_err(|e| LlmError::Failed(e.to_string()))?;
    let context_tokens = (est_tokens(&req.system) + est_tokens(&req.prompt)) as i64;
    if let Some(text) = cached {
        log_call(ctx, req, "cache", 0, 0, true, context_tokens, None, 0, None);
        return Ok(Response {
            text: no_long_dashes(&text),
            input_tokens: 0,
            output_tokens: 0,
            estimated: false,
            cost_usd: Some(0.0),
        });
    }

    let (calls, tokens): (i64, i64) = ctx
        .conn
        .query_row(
            "SELECT count(*), coalesce(sum(input_tokens),0) FROM llm_calls WHERE ts >= ?1 AND provider != 'cache'",
            [ts(ctx.day_start)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| LlmError::Failed(e.to_string()))?;
    if calls >= ctx.budget.max_calls_per_day as i64 {
        return Err(LlmError::Budget(format!("{calls} calls today (limit {})", ctx.budget.max_calls_per_day)));
    }
    if tokens >= ctx.budget.max_input_tokens_per_day as i64 {
        return Err(LlmError::Budget(format!("{tokens} input tokens today")));
    }

    let mut last_err = String::new();
    for _attempt in 0..=ctx.cfg.max_retries {
        let started = Instant::now();
        let res = ctx.provider.complete(req, Duration::from_secs(ctx.cfg.timeout_secs), &ctx.workdir);
        let ms = started.elapsed().as_millis() as i64;
        match res {
            Ok(mut r) => {
                r.text = no_long_dashes(&r.text);
                let json_ok = !req.json || extract_json(&r.text).is_ok();
                let err = (!json_ok).then(|| "malformed JSON output".to_string());
                log_call(
                    ctx,
                    req,
                    ctx.provider.name(),
                    r.input_tokens as i64,
                    r.output_tokens as i64,
                    r.estimated,
                    context_tokens,
                    r.cost_usd,
                    ms,
                    err.as_deref(),
                );
                if json_ok {
                    let _ = ctx.conn.execute(
                        "INSERT OR REPLACE INTO llm_cache(key, response, created_at) VALUES (?1, ?2, ?3)",
                        params![cache_key, r.text, ts(Utc::now())],
                    );
                    return Ok(r);
                }
                last_err = format!("malformed JSON: {}", truncate_chars(&r.text, 200));
            }
            Err(e) => {
                if matches!(e.downcast_ref::<LlmError>(), Some(LlmError::Unavailable)) {
                    return Err(LlmError::Unavailable);
                }
                last_err = e.to_string();
                log_call(ctx, req, ctx.provider.name(), 0, 0, true, context_tokens, None, ms, Some(&last_err));
            }
        }
    }
    Err(LlmError::Failed(last_err))
}

#[allow(clippy::too_many_arguments)]
fn log_call(
    ctx: &CallCtx,
    req: &Request,
    provider: &str,
    input: i64,
    output: i64,
    estimated: bool,
    context_tokens: i64,
    cost: Option<f64>,
    ms: i64,
    error: Option<&str>,
) {
    let r = ctx.conn.execute(
        "INSERT INTO llm_calls(ts, provider, model, purpose, input_tokens, output_tokens, tokens_estimated,
           context_tokens, cost_usd, duration_ms, ok, error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            ts(Utc::now()),
            provider,
            ctx.provider.model(),
            req.purpose,
            input,
            output,
            estimated as i64,
            context_tokens,
            cost,
            ms,
            error.is_none() as i64,
            error
        ],
    );
    if let Err(e) = r {
        tracing::warn!("could not log model call: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_noise() {
        let v = extract_json("Sure!\n```json\n{\"a\": \"}{\", \"b\": {\"c\": 1}}\n```").unwrap();
        assert_eq!(v["b"]["c"], 1);
        assert!(extract_json("no json").is_err());
        assert!(extract_json("{\"a\": ").is_err());
    }

    #[test]
    fn parses_claude_output() {
        let r = parse_claude_json(r#"{"result":"{\"ok\":true}","is_error":false,"total_cost_usd":0.0001,"usage":{"input_tokens":2,"cache_creation_input_tokens":650,"output_tokens":9}}"#).unwrap();
        assert_eq!(r.input_tokens, 652);
        assert_eq!(r.output_tokens, 9);
        assert_eq!(r.text, "{\"ok\":true}");
    }

    #[test]
    fn parses_codex_output() {
        let out = r#"{"type":"thread.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"ok\":true}"}}
{"type":"turn.completed","usage":{"input_tokens":16883,"cached_input_tokens":0,"output_tokens":9}}"#;
        let r = parse_codex_jsonl(out).unwrap();
        assert_eq!(r.input_tokens, 16883);
        assert_eq!(r.text, "{\"ok\":true}");
    }

    #[test]
    fn subprocess_timeout_and_exit_codes() {
        let d = std::env::temp_dir();
        let e = run_command("sleep", &["5".into()], "", Duration::from_millis(200), &d).unwrap_err();
        assert!(e.to_string().contains("timed out"));
        assert!(run_command("false", &[], "", Duration::from_secs(5), &d).is_err());
        assert_eq!(run_command("cat", &[], "hi; $(rm -rf /)", Duration::from_secs(5), &d).unwrap(), "hi; $(rm -rf /)");
    }
}

#[cfg(test)]
mod dash_tests {
    #[test]
    fn long_dashes_become_hyphens() {
        assert_eq!(super::no_long_dashes("A — B – C"), "A - B - C");
    }
}
