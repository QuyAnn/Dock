//! AI usage for Claude, Codex, Cursor and Grok Bot, read from the user's own local sign-ins.
//! Credentials are read at request time only, never logged, cached to disk, or sent anywhere but the provider.
//! Grok Bot's tile (see win::grokbot) only launches the app; its usage uses the Cursor sign-in.
use std::path::{Path, PathBuf};
use std::ffi::{c_char, c_int, c_uchar, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{AiUsage, AiWindow};

/// /api/oauth/usage is aggressively rate limited.
const CLAUDE_MIN_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Last successful Claude result, reused within CLAUDE_MIN_INTERVAL.
static CLAUDE_CACHE: Mutex<Option<(Instant, AiUsage)>> = Mutex::new(None);

/// Settings choice: true reads the rate limits with ANTHROPIC_API_KEY, false uses the Claude Code sign-in.
static CLAUDE_USE_API_KEY: AtomicBool = AtomicBool::new(false);

/// Switches the Claude source from Settings. Drops the cached result so the next refresh uses the new source.
pub fn set_claude_source(use_api_key: bool) {
    CLAUDE_USE_API_KEY.store(use_api_key, Ordering::Relaxed);
    *CLAUDE_CACHE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Usage for each provider. A provider with no data gets `session_pct = NaN`
/// and a short status (sign-in needed, error, ...) in `reset_text`.
pub fn refresh_ai() -> Vec<AiUsage> {
    vec![claude(), codex(), cursor(), grok()]
}

fn no_data(name: &str, status: &str) -> AiUsage {
    AiUsage { name: name.to_owned(), session_pct: f32::NAN, reset_text: status.to_owned(), windows: Vec::new(), cost_usd: None }
}

fn usage(name: &str, windows: Vec<AiWindow>) -> AiUsage {
    let primary = windows.first();
    AiUsage {
        name: name.to_owned(),
        session_pct: primary.map_or(f32::NAN, |w| w.used_pct),
        reset_text: primary.map_or_else(String::new, |w| w.reset_text.clone()),
        windows,
        cost_usd: None,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Provider diagnostics only. Never pass credentials or request headers to this function.
fn log_provider(provider: &str, message: &str) {
    use std::io::Write;
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let dir = PathBuf::from(appdata).join("SmartDock");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
        let _ = writeln!(file, "AI {provider}: {message}");
    }
}

#[link(name = "winsqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut c_void, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut c_void,
        sql: *const c_char,
        bytes: c_int,
        statement: *mut *mut c_void,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(statement: *mut c_void) -> c_int;
    fn sqlite3_column_text(statement: *mut c_void, column: c_int) -> *const c_uchar;
    fn sqlite3_finalize(statement: *mut c_void) -> c_int;
    fn sqlite3_close(db: *mut c_void) -> c_int;
}

/// Reads Cursor's ItemTable through Windows' inbox SQLite implementation. The connection is
/// read-only and the returned map never leaves this process.
fn cursor_auth(path: &Path) -> Result<Vec<(String, String)>, String> {
    const SQLITE_OK: c_int = 0;
    const SQLITE_ROW: c_int = 100;
    const SQLITE_OPEN_READONLY: c_int = 1;
    let filename = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "invalid database path".to_owned())?;
    let query = CString::new("SELECT key, value FROM ItemTable WHERE key LIKE 'cursorAuth/%'").unwrap();
    let mut db = ptr::null_mut();
    let mut statement = ptr::null_mut();
    unsafe {
        if sqlite3_open_v2(filename.as_ptr(), &mut db, SQLITE_OPEN_READONLY, ptr::null()) != SQLITE_OK || db.is_null() {
            return Err("cannot open state.vscdb".to_owned());
        }
        if sqlite3_prepare_v2(db, query.as_ptr(), -1, &mut statement, ptr::null_mut()) != SQLITE_OK || statement.is_null() {
            let _ = sqlite3_close(db);
            return Err("cannot query state.vscdb".to_owned());
        }
        let mut out = Vec::new();
        loop {
            let result = sqlite3_step(statement);
            if result != SQLITE_ROW {
                if result != 101 {
                    let _ = sqlite3_finalize(statement);
                    let _ = sqlite3_close(db);
                    return Err(format!("state.vscdb step error {result}"));
                }
                break;
            }
            let key = sqlite3_column_text(statement, 0);
            let value = sqlite3_column_text(statement, 1);
            if !key.is_null() && !value.is_null() {
                out.push((
                    CStr::from_ptr(key.cast()).to_string_lossy().into_owned(),
                    CStr::from_ptr(value.cast()).to_string_lossy().into_owned(),
                ));
            }
        }
        let _ = sqlite3_finalize(statement);
        let _ = sqlite3_close(db);
        Ok(out)
    }
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

/// Compact reset text used by the TS tiles (`3d`, `4h 05m`).
fn reset_text(reset_ms: Option<i64>) -> String {
    let Some(ms) = reset_ms else { return String::new() };
    let secs = (ms - now_ms()) / 1000;
    if secs <= 0 {
        return "đã reset".to_owned();
    }
    let (days, hours, mins) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if days > 0 {
        format!("{days}d")
    } else {
        format!("{hours}h {mins:02}m")
    }
}

/// No redirects: a bearer token must never be forwarded to another host.
fn agent() -> ureq::Agent {
    crate::data::http::agent_no_redirect()
}

/// Response as JSON, or a short error text that never contains a credential.
fn send(req: ureq::Request) -> Result<Value, String> {
    match req.call() {
        Ok(resp) => resp.into_json::<Value>().map_err(|_| "unreadable response".to_owned()),
        Err(ureq::Error::Status(401 | 403, _)) => Err("sign-in expired, sign in again".to_owned()),
        Err(ureq::Error::Status(429, _)) => Err("rate limited, retrying later".to_owned()),
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(ureq::Error::Transport(e)) => Err(format!("network error: {e}")),
    }
}

fn send_body(req: ureq::Request, body: &str) -> Result<Value, String> {
    match req.send_string(body) {
        Ok(resp) => resp.into_json::<Value>().map_err(|_| "unreadable response".to_owned()),
        Err(ureq::Error::Status(401 | 403, _)) => Err("sign-in expired, sign in again".to_owned()),
        Err(ureq::Error::Status(429, _)) => Err("rate limited, retrying later".to_owned()),
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(ureq::Error::Transport(e)) => Err(format!("network error: {e}")),
    }
}

// ---------------------------------------------------------------- Grok Bot

/// Calls the usage RPC through the inbox `curl.exe`. The Cursor API answers 403 to the ureq
/// request while the same request from curl.exe succeeds. The token goes over stdin as a curl
/// config, so it never appears on the command line.
fn grok_usage_via_curl(token: &str) -> Result<Value, String> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var_os("SystemRoot").ok_or_else(|| "curl.exe not found".to_owned())?;
    let mut child = Command::new(PathBuf::from(root).join("System32").join("curl.exe"))
        .args([
            "-sS", "-m", "15", "-K", "-", "-X", "POST", "-d", "{}", "-w", "\n%{http_code}",
            "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| "curl.exe failed to start".to_owned())?;
    if let Some(mut stdin) = child.stdin.take() {
        let config = format!(
            "header = \"Authorization: Bearer {token}\"\nheader = \"Connect-Protocol-Version: 1\"\nheader = \"Content-Type: application/json\"\n"
        );
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child.wait_with_output().map_err(|_| "curl.exe failed".to_owned())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_ref()));
    match code.trim() {
        "200" => serde_json::from_str(body).map_err(|_| "unreadable response".to_owned()),
        "401" | "403" => Err("sign-in expired, sign in again".to_owned()),
        "429" => Err("rate limited, retrying later".to_owned()),
        other => Err(format!("HTTP {other}")),
    }
}

/// Grok Bot ("sand") usage. Grok Bot signs in through the Cursor account, so the Cursor access
/// token from Cursor's own state DB authorizes the same DashboardService on the Cursor API host.
fn grok() -> AiUsage {
    const NAME: &str = "Grok";
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return no_data(NAME, "sign in to Grok Bot");
    };
    let db_path = PathBuf::from(appdata).join("Cursor").join("User").join("globalStorage").join("state.vscdb");
    if !db_path.is_file() {
        return no_data(NAME, "sign in to Grok Bot");
    }
    let token = match cursor_auth(&db_path) {
        Ok(rows) => match rows.into_iter().find(|(key, _)| key == "cursorAuth/accessToken") {
            Some((_, value)) if !value.is_empty() => value,
            _ => return no_data(NAME, "sign in to Grok Bot"),
        },
        Err(e) => {
            log_provider(NAME, &e);
            return no_data(NAME, "Grok auth error");
        }
    };

    let json = match grok_usage_via_curl(&token) {
        Ok(json) => json,
        Err(e) => {
            log_provider(NAME, &format!("usage endpoint failed: {e}"));
            return no_data(NAME, &e);
        }
    };
    let Some(pct) = json.get("usagePercent").and_then(Value::as_f64) else {
        log_provider(NAME, "usage response has no usagePercent");
        return no_data(NAME, "no usage data");
    };
    let reset = json.get("nextResetTimestampUtc").and_then(to_ms);
    usage(NAME, vec![AiWindow {
        label: "Plan (chu kỳ)".to_owned(),
        used_pct: pct as f32,
        reset_text: reset_text(reset),
    }])
}

// ---------------------------------------------------------------- Claude

/// Claude usage. API key mode shows only the month's API spend; login mode shows the sign-in's
/// limits and adds the spend when an admin key is set.
fn claude() -> AiUsage {
    if CLAUDE_USE_API_KEY.load(Ordering::Relaxed) {
        return match admin_month_cost() {
            // session_pct must be NaN here: the Default value 0.0 would read as 0% used.
            Some(cost) => AiUsage {
                name: "Claude".to_owned(),
                session_pct: f32::NAN,
                cost_usd: Some(cost),
                ..AiUsage::default()
            },
            None => no_data("Claude", "no cost data: check ANTHROPIC_ADMIN_KEY"),
        };
    }
    let mut usage = claude_source_usage();
    usage.cost_usd = admin_month_cost();
    usage
}

/// Month-to-date API spend in USD from the Usage and Cost Admin API. Reads ANTHROPIC_ADMIN_KEY, then
/// ANTHROPIC_ADMIN_KEY_1, and uses the first key that answers. Keys of the same organization report the
/// same totals, so adding them together would count the spend twice.
fn admin_month_cost() -> Option<f64> {
    let keys = ["ANTHROPIC_ADMIN_KEY", "ANTHROPIC_ADMIN_KEY_1"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok().filter(|k| !k.trim().is_empty()).map(|key| (var, key)))
        .collect::<Vec<_>>();
    if keys.is_empty() {
        log_provider("Claude cost", "no admin key in the environment (ANTHROPIC_ADMIN_KEY)");
        return None;
    }
    keys.into_iter().find_map(|(var, key)| match admin_cost_for(&key) {
        Ok(total) => Some(total),
        Err(e) => {
            log_provider("Claude cost", &format!("{var}: {e}"));
            None
        }
    })
}

/// Sums the cost report from the first day of this month (UTC). Amounts are USD in cents.
fn admin_cost_for(key: &str) -> Result<f64, String> {
    let now = chrono::Utc::now();
    let start = now.format("%Y-%m-01T00:00:00Z").to_string();
    let end = (now + chrono::Duration::days(1)).format("%Y-%m-%dT00:00:00Z").to_string();
    let mut total_cents = 0.0;
    let mut page: Option<String> = None;
    for _ in 0..12 {
        let mut url = format!(
            "https://api.anthropic.com/v1/organizations/cost_report?starting_at={start}&ending_at={end}&bucket_width=1d&limit=31"
        );
        if let Some(next) = &page {
            url.push_str("&page=");
            url.push_str(next);
        }
        let json = send(agent().get(&url).set("anthropic-version", "2023-06-01").set("x-api-key", key))?;
        for bucket in json.get("data").and_then(Value::as_array).into_iter().flatten() {
            for result in bucket.get("results").and_then(Value::as_array).into_iter().flatten() {
                total_cents += result.get("amount").and_then(Value::as_str).and_then(|a| a.parse::<f64>().ok()).unwrap_or(0.0);
            }
        }
        let has_more = json.get("has_more").and_then(Value::as_bool).unwrap_or(false);
        match json.get("next_page").and_then(Value::as_str) {
            Some(next) if has_more => page = Some(next.to_owned()),
            _ => break,
        }
    }
    Ok(total_cents / 100.0)
}

fn claude_source_usage() -> AiUsage {
    const NAME: &str = "Claude";
    if let Some((at, usage)) = &*CLAUDE_CACHE.lock().unwrap_or_else(PoisonError::into_inner) {
        if at.elapsed() < CLAUDE_MIN_INTERVAL {
            return usage.clone();
        }
    }
    // Tokens are never refreshed here: rotating Claude Code's refresh token could log it out.
    let oauth = home()
        .and_then(|h| read_json(&h.join(".claude").join(".credentials.json")))
        .and_then(|creds| creds.get("claudeAiOauth").cloned());
    let token = oauth.as_ref().and_then(|o| o.get("accessToken")).and_then(Value::as_str).filter(|t| !t.is_empty());
    let (Some(oauth), Some(token)) = (oauth.as_ref(), token) else {
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
    let specs = [
        ("five_hour", "Session (5h)"),
        ("seven_day", "Weekly"),
        ("seven_day_opus", "Opus (weekly)"),
        ("seven_day_sonnet", "Sonnet (weekly)"),
    ];
    let windows = specs
        .into_iter()
        .filter_map(|(key, label)| {
            let window = j.get(key)?;
            Some(AiWindow {
                label: label.to_owned(),
                used_pct: window.get("utilization")?.as_f64()? as f32,
                reset_text: reset_text(window.get("resets_at").and_then(to_ms)),
            })
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return no_data(NAME, "no usage data");
    }
    let usage = usage(NAME, windows);
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
                    let mut windows = vec![AiWindow {
                        label: "Session (5h)".to_owned(),
                        used_pct: pct as f32,
                        reset_text: reset_text(j.pointer("/rate_limit/primary_window/reset_at").and_then(to_ms)),
                    }];
                    if let Some(pct) = j.pointer("/rate_limit/secondary_window/used_percent").and_then(Value::as_f64) {
                        windows.push(AiWindow {
                            label: "Weekly".to_owned(),
                            used_pct: pct as f32,
                            reset_text: reset_text(j.pointer("/rate_limit/secondary_window/reset_at").and_then(to_ms)),
                        });
                    }
                    return usage(NAME, windows);
                }
                api_err = Some("no usage data".to_owned());
            }
            Err(e) => {
                log_provider(NAME, &format!("usage endpoint failed: {e}"));
                api_err = Some(e);
            }
        }
    }

    // Fallback: the newest rate-limit snapshot Codex wrote into its own session logs. A window
    // whose reset time has passed is stale (its % belongs to the previous window), so it is dropped.
    if let Some((rl, at_ms)) = latest_codex_rate_limits() {
        let now = now_ms();
        let mut windows = Vec::new();
        for (field, label) in [("/primary", "Session (5h)"), ("/secondary", "Weekly")] {
            let Some(window) = rl.pointer(field) else { continue };
            let Some(pct) = window.get("used_percent").and_then(Value::as_f64) else { continue };
            let reset = window.get("resets_at").and_then(to_ms).or_else(|| {
                window
                    .get("resets_in_seconds")
                    .and_then(Value::as_f64)
                    .map(|s| at_ms + (s * 1000.0) as i64)
            });
            if reset.is_some_and(|ms| ms <= now) {
                continue;
            }
            windows.push(AiWindow { label: label.to_owned(), used_pct: pct as f32, reset_text: reset_text(reset) });
        }
        if !windows.is_empty() {
            return usage(NAME, windows);
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

// ---------------------------------------------------------------- Cursor

fn cursor() -> AiUsage {
    const NAME: &str = "Cursor";
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return no_data(NAME, "sign in to Cursor");
    };
    let db_path = PathBuf::from(appdata).join("Cursor").join("User").join("globalStorage").join("state.vscdb");
    if !db_path.is_file() {
        return no_data(NAME, "sign in to Cursor");
    }
    let read_auth = || -> Result<(String, Option<String>), String> {
        let mut token = None;
        let mut plan = None;
        for (key, value) in cursor_auth(&db_path)? {
            match key.as_str() {
                "cursorAuth/accessToken" => token = Some(value),
                "cursorAuth/stripeMembershipType" => plan = Some(value),
                _ => {}
            }
        }
        token.filter(|value| !value.is_empty()).map(|value| (value, plan)).ok_or_else(|| "access token missing".to_owned())
    };
    let (token, plan) = match read_auth() {
        Ok(auth) => auth,
        Err(e) => {
            log_provider(NAME, &e);
            return no_data(NAME, if e == "access token missing" { "sign in to Cursor" } else { "Cursor auth error" });
        }
    };
    let user_id = token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|payload| payload.get("sub").and_then(Value::as_str).map(str::to_owned))
        .and_then(|sub| sub.rsplit('|').next().map(str::to_owned))
        .filter(|id| !id.is_empty());
    let Some(user_id) = user_id else {
        log_provider(NAME, "access token payload has no user id");
        return no_data(NAME, "Cursor auth error");
    };

    let request = agent()
        .post("https://cursor.com/api/dashboard/get-current-period-usage")
        .set("Content-Type", "application/json")
        .set("Origin", "https://cursor.com")
        .set("Referer", "https://cursor.com/dashboard")
        .set("Cookie", &format!("WorkosCursorSessionToken={}%3A%3A{}", user_id, token));
    let json = match send_body(request, "{}") {
        Ok(json) => json,
        Err(e) => {
            log_provider(NAME, &format!("usage endpoint failed: {e}"));
            return no_data(NAME, &e);
        }
    };
    let plan_usage = json.get("planUsage").unwrap_or(&Value::Null);
    let spend = plan_usage.get("totalSpend").and_then(Value::as_f64);
    let limit = plan_usage.get("limit").and_then(Value::as_f64);
    // Prefer $ spend / limit — `totalPercentUsed` from the API can disagree with the dollar
    // amounts (e.g. $37.43/$20 shown as ~45%). Over-limit plans correctly exceed 100%.
    let from_money = match (spend, limit) {
        (Some(s), Some(l)) if l > 0.0 => Some(s / l * 100.0),
        _ => None,
    };
    let from_api = plan_usage.get("totalPercentUsed").and_then(Value::as_f64);
    let total_pct = from_money.or(from_api);
    let end_ms = json.get("billingCycleEnd").and_then(to_ms);
    let money = match (spend, limit) {
        (Some(spend), Some(limit)) => format!("${:.2}/${:.2}", spend / 100.0, limit / 100.0),
        _ => plan.unwrap_or_else(|| "Cursor".to_owned()),
    };
    let reset = end_ms
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|date| date.format("%d/%m").to_string());
    let reset_text = reset.clone().map_or(money.clone(), |date| format!("{money} · {date}"));
    let Some(total_pct) = total_pct else {
        return no_data(NAME, &reset_text);
    };
    let mut windows = vec![AiWindow {
        label: "Plan (chu kỳ)".to_owned(),
        used_pct: total_pct as f32,
        reset_text,
    }];
    // Same two extra limits as Dock-Rc ai.ts: planUsage.autoPercentUsed ("Auto + Composer") and
    // planUsage.apiPercentUsed ("API models"), both resetting at billingCycleEnd. Only shown when
    // the API returns them; no invented numbers.
    let cycle_reset = reset.as_ref().map_or_else(String::new, |date| format!("reset {date}"));
    for (field, label) in [("autoPercentUsed", "Auto + Composer"), ("apiPercentUsed", "API models")] {
        if let Some(pct) = plan_usage.get(field).and_then(Value::as_f64) {
            windows.push(AiWindow { label: label.to_owned(), used_pct: pct as f32, reset_text: cycle_reset.clone() });
        }
    }
    log_provider(NAME, &format!(
        "usage plan={:.1}% extra_limits=[{}]",
        total_pct,
        windows
            .iter()
            .skip(1)
            .map(|w| format!("{}={:.1}%", w.label, w.used_pct))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    usage(NAME, windows)
}

