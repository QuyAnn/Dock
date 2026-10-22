//! AI usage for Claude and Codex (plus a Grok placeholder), read from the user's own local sign-ins.
//! Credentials are read at request time only, never logged, cached to disk, or sent anywhere but the provider.
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::AiUsage;

const TIMEOUT: Duration = Duration::from_secs(15);
/// /api/oauth/usage is aggressively rate limited.
const CLAUDE_MIN_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Last successful Claude result, reused within CLAUDE_MIN_INTERVAL.
static CLAUDE_CACHE: Mutex<Option<(Instant, AiUsage)>> = Mutex::new(None);

/// Usage for each provider. A provider with no data gets `session_pct = NaN`
/// and a short status (sign-in needed, error, ...) in `reset_text`.
pub fn refresh_ai() -> Vec<AiUsage> {
    vec![claude(), codex(), grok()]
}

fn no_data(name: &str, status: &str) -> AiUsage {
    AiUsage { name: name.to_owned(), session_pct: f32::NAN, reset_text: status.to_owned() }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Epoch seconds or milliseconds, or an RFC 3339 string, as epoch ms.
fn to_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as i64).map(|n| if n < 1_000_000_000_000 { n * 1000 } else { n }),
        Value::String(s) => DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis()),
        _ => None,
    }
}

/// "resets in 2h 05m", or empty when the time is unknown.
fn reset_text(reset_ms: Option<i64>) -> String {
    let Some(ms) = reset_ms else { return String::new() };
    let secs = (ms - now_ms()) / 1000;
    if secs <= 0 {
        return "resets now".to_owned();
    }
    let (days, hours, mins) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if days > 0 {
        format!("resets in {days}d {hours}h")
    } else {
        format!("resets in {hours}h {mins:02}m")
    }
}

/// No redirects: a bearer token must never be forwarded to another host.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(TIMEOUT).redirects(0).build()
}

/// Response as JSON, or a short error text that never contains a credential.
fn send(req: ureq::Request) -> Result<Value, String> {
    match req.call() {
        Ok(resp) => resp.into_json::<Value>().map_err(|_| "unreadable response".to_owned()),
        Err(ureq::Error::Status(401 | 403, _)) => Err("sign-in expired, sign in again".to_owned()),
        Err(ureq::Error::Status(429, _)) => Err("rate limited, retrying later".to_owned()),
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(_) => Err("network error".to_owned()),
    }
}

// ---------------------------------------------------------------- Claude

fn claude() -> AiUsage {
    const NAME: &str = "Claude";
    if let Some((at, usage)) = &*CLAUDE_CACHE.lock().unwrap_or_else(PoisonError::into_inner) {
        if at.elapsed() < CLAUDE_MIN_INTERVAL {
            return usage.clone();
        }
    }
    // Tokens are never refreshed here: rotating Claude Code's refresh token could log it out.
    let Some(creds) = home().and_then(|h| read_json(&h.join(".claude").join(".credentials.json"))) else {
        return no_data(NAME, "sign in: run `claude`, then /login");
    };
    let Some(oauth) = creds.get("claudeAiOauth") else {
        return no_data(NAME, "sign in: run `claude`, then /login");
    };
    let Some(token) = oauth.get("accessToken").and_then(Value::as_str).filter(|t| !t.is_empty()) else {
        return no_data(NAME, "sign in: run `claude`, then /login");
    };
    if oauth.get("expiresAt").and_then(Value::as_f64).is_some_and(|exp| exp < now_ms() as f64) {
        return no_data(NAME, "token expired: open Claude Code to refresh");
    }

    let req = agent()
        .get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("Accept", "application/json")
        .set("User-Agent", "claude-code/2.1.0");
    let j = match send(req) {
        Ok(j) => j,
        Err(e) => return no_data(NAME, &e),
    };
    let Some(window) = j.get("five_hour") else {
        return no_data(NAME, "no usage data");
    };
    let Some(pct) = window.get("utilization").and_then(Value::as_f64) else {
        return no_data(NAME, "no usage data");
    };
    let usage = AiUsage {
        name: NAME.to_owned(),
        session_pct: pct as f32,
        reset_text: reset_text(window.get("resets_at").and_then(to_ms)),
    };
    *CLAUDE_CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), usage.clone()));
    usage
}

// ---------------------------------------------------------------- Codex

fn codex() -> AiUsage {
    const NAME: &str = "Codex";
    let auth = home().and_then(|h| read_json(&h.join(".codex").join("auth.json")));
    let tokens = auth.as_ref().and_then(|a| a.get("tokens"));
    let token = tokens.and_then(|t| t.get("access_token")).and_then(Value::as_str).filter(|t| !t.is_empty());

    let mut api_err = None;
    if let Some(token) = token {
        let mut req = agent()
            .get("https://chatgpt.com/backend-api/wham/usage")
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .set("User-Agent", "codex_cli_rs");
        if let Some(account) = tokens.and_then(|t| t.get("account_id")).and_then(Value::as_str) {
            req = req.set("ChatGPT-Account-Id", account);
        }
        match send(req) {
            Ok(j) => {
                if let Some(pct) = j.pointer("/rate_limit/primary_window/used_percent").and_then(Value::as_f64) {
                    return AiUsage {
                        name: NAME.to_owned(),
                        session_pct: pct as f32,
                        reset_text: reset_text(j.pointer("/rate_limit/primary_window/reset_at").and_then(to_ms)),
                    };
                }
                api_err = Some("no usage data".to_owned());
            }
            Err(e) => api_err = Some(e),
        }
    }

    // Fallback: the newest rate-limit snapshot Codex wrote into its own session logs.
    if let Some((rl, at_ms)) = latest_codex_rate_limits() {
        if let Some(pct) = rl.pointer("/primary/used_percent").and_then(Value::as_f64) {
            let reset = rl.pointer("/primary/resets_at").and_then(to_ms).or_else(|| {
                rl.pointer("/primary/resets_in_seconds")
                    .and_then(Value::as_f64)
                    .map(|s| at_ms + (s * 1000.0) as i64)
            });
            return AiUsage { name: NAME.to_owned(), session_pct: pct as f32, reset_text: reset_text(reset) };
        }
    }

    match (token, api_err) {
        (None, _) => no_data(NAME, "sign in: run `codex login`"),
        (Some(_), err) => no_data(NAME, &err.unwrap_or_else(|| "no usage data".to_owned())),
    }
}

/// Newest `payload.rate_limits` record in ~/.codex/sessions/**.jsonl: (rate_limits, epoch ms).
fn latest_codex_rate_limits() -> Option<(Value, i64)> {
    let dir = home()?.join(".codex").join("sessions");
    let mut files = Vec::new();
    collect_jsonl(&dir, 0, &mut files);
    files.sort_by(|a, b| b.1.cmp(&a.1));

    for (path, mtime_ms) in files.iter().take(5) {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        for line in text.lines().rev().filter(|l| l.contains("\"rate_limits\"")) {
            let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
            let Some(rl) = record.pointer("/payload/rate_limits") else { continue };
            let at = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map_or(*mtime_ms, |d| d.timestamp_millis());
            return Some((rl.clone(), at));
        }
    }
    None
}

/// Collects .jsonl files (path, mtime in epoch ms) up to 4 directory levels deep.
fn collect_jsonl(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, i64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if depth < 4 {
                collect_jsonl(&p, depth + 1, out);
            }
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            out.push((p, mtime));
        }
    }
}

// ---------------------------------------------------------------- Grok

/// xAI exposes no local sign-in or usage API that a desktop app can read.
fn grok() -> AiUsage {
    no_data("Grok", "no usage source")
}
