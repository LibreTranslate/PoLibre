// SPDX-License-Identifier: AGPL-3.0-or-later
//! Translation backends: hosted LLM APIs, locally installed LLM CLIs and
//! LibreTranslate.

use crate::config::{ProviderConfig, ProviderKind};
use crate::langs;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub type Cancel = Arc<AtomicBool>;

/// Receives (title, details) for the debug console / `--debug`.
pub type DebugSink = Arc<dyn Fn(String, String) + Send + Sync>;

/// Receives short messages the user should see even without debugging
/// (e.g. "the server asked to slow down; waiting 60 s").
pub type NoticeSink = Arc<dyn Fn(String) + Send + Sync>;

thread_local! {
    static DEBUG: std::cell::RefCell<Option<DebugSink>> = const { std::cell::RefCell::new(None) };
    static NOTICE: std::cell::RefCell<Option<NoticeSink>> = const { std::cell::RefCell::new(None) };
}

/// Sets where notices from provider calls made on the current thread go.
pub fn set_notice(sink: Option<NoticeSink>) {
    NOTICE.with(|d| *d.borrow_mut() = sink);
}

fn notice_sink() -> Option<NoticeSink> {
    NOTICE.with(|d| d.borrow().clone())
}

/// Enables debug output for provider calls made from the current thread.
pub fn set_debug(sink: Option<DebugSink>) {
    DEBUG.with(|d| *d.borrow_mut() = sink);
}

fn debug_sink() -> Option<DebugSink> {
    DEBUG.with(|d| d.borrow().clone())
}

/// Sends a debug entry if debugging is on; `body` is only built when needed.
pub fn debug(title: impl Into<String>, body: impl FnOnce() -> String) {
    if let Some(s) = debug_sink() {
        s(title.into(), body());
    }
}

/// Hides all but the last 4 characters of a secret.
pub fn mask(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 8 {
        "••••".into()
    } else {
        format!(
            "••••{} ({} characters)",
            secret.chars().skip(n - 4).collect::<String>(),
            n
        )
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// POST with retries on a helper thread, logging the exact request and response.
fn post_logged(
    cancel: &Cancel,
    url: String,
    headers: Vec<(&'static str, String)>,
    body: Value,
    secret_fields: &[&str],
) -> Result<Value, String> {
    let sink = debug_sink();
    if let Some(s) = &sink {
        let mut shown = body.clone();
        for f in secret_fields {
            if let Some(v) = shown.get_mut(*f) {
                *v = json!(mask(v.as_str().unwrap_or("")));
            }
        }
        let hdrs: String = headers
            .iter()
            .map(|(k, v)| {
                let secret = matches!(
                    k.to_ascii_lowercase().as_str(),
                    "x-api-key" | "authorization" | "x-goog-api-key"
                );
                format!("{}: {}\n", k, if secret { mask(v) } else { v.clone() })
            })
            .collect();
        s(
            format!("HTTP request: POST {}", url),
            format!(
                "POST {}\nContent-Type: application/json\n{}\n{}",
                url,
                hdrs,
                pretty(&shown)
            ),
        );
    }
    let started = std::time::Instant::now();
    let s2 = sink.clone();
    let n2 = notice_sink();
    let r = cancellable(cancel, move || {
        post_json(&url, &headers, &body, s2.as_ref(), n2.as_ref())
    })?;
    if let Some(s) = &sink {
        match &r {
            Ok(v) => s(
                format!("HTTP response ({:.1}s)", started.elapsed().as_secs_f32()),
                pretty(v),
            ),
            Err(e) => s(
                format!(
                    "HTTP request couldn't be completed ({:.1}s)",
                    started.elapsed().as_secs_f32()
                ),
                e.clone(),
            ),
        }
    }
    r
}

pub const ANTHROPIC_VERSION: &str = "2026-10-21";

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("PoLibre/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Run a blocking closure on a helper thread, returning early if the user
/// cancels. (The helper thread is abandoned and finishes in the background.)
pub fn cancellable<T: Send + 'static>(
    cancel: &Cancel,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(v) => return Ok(v),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
            }
            Err(_) => return Err("worker thread failed".into()),
        }
    }
}

fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            status_err(code, &resp.into_string().unwrap_or_default())
        }
        ureq::Error::Transport(t) => format!("network error: {}", t),
    }
}

fn status_err(code: u16, body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.get("error"))
                .map(|m| {
                    m.as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| m.to_string())
                })
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    format!("HTTP {}: {}", code, msg)
}

// ---------------------------------------------------------------------------
// Rate limits
//
// A server that is overloaded or rate limited answers HTTP 429. LibreTranslate
// does so with {"error": "Slowdown: 30 per 1 minute"} (its request limit per
// API key or address); some servers also send a Retry-After header. PoLibre
// then waits before asking again, and spaces out its later requests to the
// same server so it stays under the limit.
// ---------------------------------------------------------------------------

/// Longest single wait; longer limits (e.g. per day) end the run instead.
const MAX_WAIT: Duration = Duration::from_secs(10 * 60);
/// How many "slow down" answers in a row are tolerated for one request.
const MAX_SLOWDOWNS: u32 = 8;

/// Minimum spacing between requests, and when the next one may start, per URL.
static PACE: std::sync::Mutex<Vec<(String, Duration, std::time::Instant)>> =
    std::sync::Mutex::new(Vec::new());

/// Waits until the next request to `url` is allowed by the learned pace.
fn pace_wait(url: &str) {
    let wait = {
        let mut p = PACE.lock().unwrap();
        match p.iter_mut().find(|e| e.0 == url) {
            Some(e) => {
                let now = std::time::Instant::now();
                let start = e.2.max(now);
                e.2 = start + e.1;
                start - now
            }
            None => Duration::ZERO,
        }
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
}

/// Remembers that requests to `url` should be at least `gap` apart.
fn pace_set(url: &str, gap: Duration) {
    let mut p = PACE.lock().unwrap();
    let now = std::time::Instant::now();
    match p.iter_mut().find(|e| e.0 == url) {
        Some(e) => e.1 = e.1.max(gap),
        None => p.push((url.to_string(), gap, now)),
    }
}

/// Parses a rate limit such as "30 per 1 minute", "10/second" or "Slowdown: 100 per day"
/// into (requests, period).
pub fn parse_rate_limit(text: &str) -> Option<(u32, Duration)> {
    let t = text.to_ascii_lowercase().replace('/', " per ");
    // Words only: drops JSON quotes and braces, "Slowdown:" etc.
    let words: Vec<&str> = t
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let i = words.iter().position(|w| *w == "per")?;
    let n: u32 = words.get(i.checked_sub(1)?)?.parse().ok()?;
    let (k, unit) = match words.get(i + 1)?.parse::<u64>() {
        Ok(k) => (k, *words.get(i + 2)?),
        Err(_) => (1, words[i + 1]),
    };
    let secs = match unit.trim_end_matches('s') {
        "second" | "sec" => 1,
        "minute" | "min" => 60,
        "hour" => 3600,
        "day" => 86400,
        _ => return None,
    };
    (n > 0 && k > 0).then(|| (n, Duration::from_secs(secs * k)))
}

/// Retry-After as seconds (the HTTP-date form is not used by these servers).
fn retry_after(resp: &ureq::Response) -> Option<Duration> {
    let v: f64 = resp.header("retry-after")?.trim().parse().ok()?;
    (v >= 0.0).then(|| Duration::from_secs_f64(v.max(1.0)))
}

fn secs(d: Duration) -> String {
    let s = d.as_secs_f32().round() as u64;
    if s >= 120 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} s", s)
    }
}

/// POST JSON. Retries network errors and 5xx a few times, and waits as asked
/// when the server says to slow down (HTTP 429).
fn post_json(
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
    dbg: Option<&DebugSink>,
    notice: Option<&NoticeSink>,
) -> Result<Value, String> {
    let a = agent();
    let mut delay = 2u64;
    let mut failures = 0u32;
    let mut slowdowns = 0u32;
    loop {
        pace_wait(url);
        let mut req = a.post(url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let err = match req.send_json(body.clone()) {
            Ok(resp) => {
                return resp
                    .into_json::<Value>()
                    .map_err(|e| format!("invalid JSON response: {}", e))
            }
            Err(e) => e,
        };
        if let ureq::Error::Status(429, resp) = err {
            slowdowns += 1;
            let header = retry_after(&resp);
            let text = resp.into_string().unwrap_or_default();
            let msg = status_err(429, &text);
            let limit = parse_rate_limit(&text);
            if let Some((n, period)) = limit {
                // Stay under the limit from now on (with a 10% margin).
                pace_set(url, period.mul_f32(1.1) / n);
            }
            // Wait what the server asks; else, once the limit's window has
            // passed; else back off: 5 s, 10 s, 20 s ...
            let wait = header
                .or_else(|| {
                    limit.map(|(_, p)| {
                        if slowdowns == 1 {
                            (p / 4).max(Duration::from_secs(2))
                        } else {
                            p
                        }
                    })
                })
                .unwrap_or_else(|| Duration::from_secs(5 << (slowdowns - 1).min(6)));
            if wait > MAX_WAIT {
                return Err(format!(
                    "{} — the server's limit was reached; it would take {} before it accepts more requests. Try again later.",
                    msg,
                    secs(wait)
                ));
            }
            if slowdowns > MAX_SLOWDOWNS {
                return Err(format!(
                    "{} — the server kept asking to slow down; giving up.",
                    msg
                ));
            }
            if let Some(d) = dbg {
                d(
                    format!("Server asked to slow down; waiting {}", secs(wait)),
                    msg.clone(),
                );
            }
            if let Some(n) = notice {
                n(format!(
                    "The server asked to slow down ({}). Waiting {} before continuing…",
                    msg.trim_start_matches("HTTP 429: "),
                    secs(wait)
                ));
            }
            std::thread::sleep(wait);
            continue;
        }
        let retry = match &err {
            ureq::Error::Status(code, _) => *code >= 500,
            ureq::Error::Transport(_) => true,
        };
        let last = http_err(err);
        failures += 1;
        if !retry || failures == 4 {
            return Err(last);
        }
        if let Some(d) = dbg {
            d(
                format!("Retrying in {}s (attempt {} of 4)", delay, failures + 1),
                last.clone(),
            );
        }
        std::thread::sleep(Duration::from_secs(delay));
        delay *= 2;
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[test]
    fn parses_libretranslate_limits() {
        let m = Duration::from_secs(60);
        assert_eq!(parse_rate_limit("Slowdown: 30 per 1 minute"), Some((30, m)));
        assert_eq!(parse_rate_limit("Slowdown: 80 per minute"), Some((80, m)));
        assert_eq!(
            parse_rate_limit("10/second"),
            Some((10, Duration::from_secs(1)))
        );
        assert_eq!(
            parse_rate_limit("Slowdown: 1000 per 1 day"),
            Some((1000, Duration::from_secs(86400)))
        );
        assert_eq!(parse_rate_limit("Too many request limits violations"), None);
        assert_eq!(
            parse_rate_limit(r#"{"error":"Slowdown: 4 per 4 second"}"#),
            Some((4, Duration::from_secs(4)))
        );
    }
}

fn get_json(url: &str, headers: &[(&str, String)]) -> Result<Value, String> {
    let mut req = agent().get(url).timeout(Duration::from_secs(30));
    for (k, v) in headers {
        req = req.set(k, v);
    }
    req.call()
        .map_err(http_err)?
        .into_json::<Value>()
        .map_err(|e| format!("invalid JSON response: {}", e))
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

pub fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    // GUI apps on macOS often get a minimal PATH; check common install dirs too.
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".npm-global/bin"));
        dirs.push(h.join(".claude/local"));
        dirs.push(h.join(".bun/bin"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    for d in dirs {
        for name in [
            bin.to_string(),
            format!("{}.exe", bin),
            format!("{}.cmd", bin),
        ] {
            let p = d.join(&name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Detected {
    pub kind: ProviderKind,
    pub how: String,
}

/// Providers usable right now without further configuration.
pub fn detect() -> Vec<Detected> {
    let mut out = Vec::new();
    for k in [
        ProviderKind::ClaudeCli,
        ProviderKind::GeminiCli,
        ProviderKind::CodexCli,
    ] {
        if let Some(p) = find_in_path(k.cli_binary().unwrap()) {
            out.push(Detected {
                kind: k,
                how: format!("found {}", p.display()),
            });
        }
    }
    for k in [
        ProviderKind::Anthropic,
        ProviderKind::Google,
        ProviderKind::Openai,
        ProviderKind::Libretranslate,
    ] {
        for e in k.key_env() {
            if std::env::var(e)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            {
                out.push(Detected {
                    kind: k,
                    how: format!("${} is set", e),
                });
                break;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Model listing
// ---------------------------------------------------------------------------

pub fn list_models(p: &ProviderConfig) -> Result<Vec<String>, String> {
    let mut models: Vec<String> = match p.kind {
        ProviderKind::Anthropic => {
            let v = get_json(
                &format!("{}/v1/models?limit=100", p.url),
                &[
                    ("x-api-key", p.api_key.clone()),
                    ("anthropic-version", ANTHROPIC_VERSION.into()),
                ],
            )?;
            v["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect()
        }
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => {
            let mut h = vec![];
            if !p.api_key.is_empty() {
                h.push(("Authorization", format!("Bearer {}", p.api_key)));
            }
            let v = get_json(&format!("{}/models", p.url), &h)?;
            let mut m: Vec<String> = v["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["id"].as_str().map(String::from))
                .collect();
            if p.kind == ProviderKind::Openai {
                m.retain(|id| {
                    (id.starts_with("gpt") || id.starts_with('o'))
                        && ![
                            "audio",
                            "realtime",
                            "tts",
                            "transcribe",
                            "image",
                            "search",
                            "embedding",
                        ]
                        .iter()
                        .any(|x| id.contains(x))
                });
            }
            m
        }
        ProviderKind::Google => {
            let v = get_json(
                &format!("{}/v1beta/models?pageSize=200&key={}", p.url, p.api_key),
                &[],
            )?;
            v["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| {
                    m["supportedGenerationMethods"]
                        .as_array()
                        .map(|a| a.iter().any(|x| x == "generateContent"))
                        .unwrap_or(false)
                })
                .filter_map(|m| {
                    m["name"]
                        .as_str()
                        .map(|s| s.trim_start_matches("models/").to_string())
                })
                .filter(|s| s.starts_with("gemini"))
                .collect()
        }
        _ => p
            .kind
            .suggested_models()
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    models.sort();
    models.reverse();
    models.dedup();
    Ok(models)
}

// ---------------------------------------------------------------------------
// LLM completion
// ---------------------------------------------------------------------------

/// Send a system + user prompt to an LLM provider and return the text reply.
pub fn complete(
    p: &ProviderConfig,
    system: &str,
    user: &str,
    cancel: &Cancel,
) -> Result<String, String> {
    match p.kind {
        ProviderKind::Anthropic => {
            if p.api_key.is_empty() {
                return Err("no Anthropic API key (set ANTHROPIC_API_KEY or --api-key)".into());
            }
            let body = json!({
                "model": p.model,
                "max_tokens": 16000,
                "system": system,
                "output_config": {"effort": "low"},
                "messages": [{"role": "user", "content": user}],
            });
            let mut body = body;
            let url = format!("{}/v1/messages", p.url);
            let mut headers: Vec<(&'static str, String)> = vec![
                ("x-api-key", p.api_key.clone()),
                ("anthropic-version", ANTHROPIC_VERSION.to_string()),
            ];
            // Server-side fallback on refusals for the models that support it.
            if ["claude-fable-5", "claude-opus-5", "claude-sonnet-5-5"]
                .iter()
                .any(|m| p.model.starts_with(m))
            {
                headers.push((
                    "anthropic-beta",
                    "server-side-fallback-2026-07-01".to_string(),
                ));
                body["fallbacks"] = json!("default");
            }
            let v = post_logged(cancel, url, headers, body, &[])?;
            if v["stop_reason"] == "refusal" {
                return Err("model refused the request".into());
            }
            let text: String = v["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            if v["stop_reason"] == "max_tokens" {
                return Err("response truncated (max_tokens); try a smaller --batch-size".into());
            }
            Ok(text)
        }
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => {
            if p.kind == ProviderKind::Openai && p.api_key.is_empty() {
                return Err("no OpenAI API key (set OPENAI_API_KEY or --api-key)".into());
            }
            let mut body = json!({
                "model": p.model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user}
                ],
            });
            if p.kind == ProviderKind::Openai {
                body["response_format"] = json!({"type": "json_object"});
            }
            let url = format!("{}/chat/completions", p.url);
            let mut headers: Vec<(&'static str, String)> = vec![];
            if !p.api_key.is_empty() {
                headers.push(("Authorization", format!("Bearer {}", p.api_key)));
            }
            let v = post_logged(cancel, url, headers, body, &[])?;
            v.pointer("/choices/0/message/content")
                .and_then(|c| c.as_str())
                .map(String::from)
                .ok_or_else(|| "unexpected response format".into())
        }
        ProviderKind::Google => {
            if p.api_key.is_empty() {
                return Err("no Gemini API key (set GEMINI_API_KEY or --api-key)".into());
            }
            let body = json!({
                "systemInstruction": {"parts": [{"text": system}]},
                "contents": [{"role": "user", "parts": [{"text": user}]}],
                "generationConfig": {"responseMimeType": "application/json"},
            });
            let url = format!("{}/v1beta/models/{}:generateContent", p.url, p.model);
            let headers = vec![("x-goog-api-key", p.api_key.clone())];
            let v = post_logged(cancel, url, headers, body, &[])?;
            let text: String = v
                .pointer("/candidates/0/content/parts")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
                .filter_map(|p| p["text"].as_str())
                .collect();
            if text.is_empty() {
                return Err(format!(
                    "empty response ({})",
                    v.pointer("/candidates/0/finishReason")
                        .unwrap_or(&Value::Null)
                ));
            }
            Ok(text)
        }
        ProviderKind::ClaudeCli | ProviderKind::GeminiCli | ProviderKind::CodexCli => {
            run_cli(p, system, user, cancel)
        }
        ProviderKind::Libretranslate => Err("LibreTranslate is not an LLM provider".into()),
    }
}

fn run_cli(
    p: &ProviderConfig,
    system: &str,
    user: &str,
    cancel: &Cancel,
) -> Result<String, String> {
    let bin = p.kind.cli_binary().unwrap();
    let exe = find_in_path(bin).ok_or_else(|| format!("'{}' not found in PATH", bin))?;
    // Run in an empty temp dir so the CLI doesn't pick up the project's own
    // agent instructions or try to touch files.
    let work = std::env::temp_dir().join(format!("polibre-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    let mut cmd = Command::new(&exe);
    cmd.current_dir(&work);
    let mut stdin_text = String::new();
    let mut out_file = None;
    match p.kind {
        ProviderKind::ClaudeCli => {
            cmd.args([
                "-p",
                "--output-format",
                "text",
                "--tools",
                "",
                "--no-session-persistence",
                "--strict-mcp-config",
            ]);
            cmd.arg("--system-prompt").arg(system);
            if !p.model.is_empty() {
                cmd.arg("--model").arg(&p.model);
            }
            stdin_text.push_str(user);
        }
        ProviderKind::GeminiCli => {
            if !p.model.is_empty() {
                cmd.arg("-m").arg(&p.model);
            }
            cmd.arg("-p")
                .arg("Follow the instructions above. Reply with only the JSON object.");
            stdin_text = format!("{}\n\n{}", system, user);
        }
        ProviderKind::CodexCli => {
            let f = work.join("last-message.txt");
            cmd.args([
                "exec",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--output-last-message",
            ]);
            cmd.arg(&f);
            if !p.model.is_empty() {
                cmd.arg("--model").arg(&p.model);
            }
            cmd.arg("-");
            out_file = Some(f);
            stdin_text = format!("{}\n\n{}", system, user);
        }
        _ => unreachable!(),
    }
    debug(format!("Command: {}", bin), || {
        let quote = |a: &str| {
            if a.is_empty() || a.contains(|c: char| c.is_whitespace() || "\"'$`\\".contains(c)) {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.to_string()
            }
        };
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| quote(&a.to_string_lossy()))
            .collect();
        format!(
            "Working folder: {}\n\n{} {}\n\nInput (stdin):\n{}",
            work.display(),
            exe.display(),
            args.join(" "),
            stdin_text
        )
    });
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start {}: {}", bin, e))?;
    {
        let mut stdin = child.stdin.take().unwrap();
        let t = stdin_text.clone();
        std::thread::spawn(move || {
            let _ = stdin.write_all(t.as_bytes());
        });
    }
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
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
    let started = std::time::Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
        if started.elapsed() > Duration::from_secs(900) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} timed out", bin));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    if let Some(f) = out_file {
        if let Ok(s) = std::fs::read_to_string(&f) {
            out = s;
        }
    }
    debug(
        format!(
            "{} finished: {} ({:.1}s)",
            bin,
            status,
            started.elapsed().as_secs_f32()
        ),
        || {
            format!(
                "Output (stdout):\n{}\n\nMessages (stderr):\n{}",
                out,
                if err.trim().is_empty() {
                    "(none)"
                } else {
                    &err
                }
            )
        },
    );
    if !status.success() {
        let msg = if err.trim().is_empty() {
            out.clone()
        } else {
            err
        };
        return Err(format!(
            "{} exited with {}: {}",
            bin,
            status,
            msg.trim().chars().take(400).collect::<String>()
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// LibreTranslate
// ---------------------------------------------------------------------------

/// Languages known to LibreTranslate/LTEngine, used if /languages can't be reached.
pub const LT_FALLBACK: &[&str] = &[
    "en", "sq", "ar", "az", "eu", "bn", "bg", "ca", "zh-Hans", "zh-Hant", "zh", "zt", "cs", "da",
    "nl", "eo", "et", "fi", "fr", "gl", "de", "el", "he", "hi", "hu", "id", "ga", "it", "ja", "ko",
    "ky", "lv", "lt", "ms", "nb", "fa", "pl", "pt", "pt-BR", "pb", "ro", "ru", "sr", "sk", "sl",
    "es", "sv", "sw", "tl", "th", "tr", "uk", "ur", "vi",
];

/// Candidate LibreTranslate codes for a gettext locale, most specific first.
pub fn lt_candidates(po_code: &str) -> Vec<String> {
    let n = langs::normalize(po_code);
    let lower = n.to_lowercase();
    let b = langs::base(&lower).to_string();
    match lower.as_str() {
        "zh_hans" | "zh_cn" | "zh_sg" | "zh" => return vec!["zh-Hans".into(), "zh".into()],
        "zh_hant" | "zh_tw" | "zh_hk" | "zh_mo" => return vec!["zh-Hant".into(), "zt".into()],
        "pt_br" => return vec!["pt-BR".into(), "pb".into()],
        "nb_no" | "nb" | "no" => return vec!["nb".into(), "no".into()],
        "fil" => return vec!["tl".into()],
        _ => {}
    }
    let mut v = vec![n.replace('_', "-")];
    if b != v[0] {
        v.push(b);
    }
    v
}

#[derive(Debug, Clone)]
pub struct LtLanguages {
    /// target codes supported from the source language
    pub targets: Vec<String>,
}

impl LtLanguages {
    pub fn fetch(url: &str, source: &str) -> Result<LtLanguages, String> {
        let v = get_json(&format!("{}/languages", url), &[])?;
        let arr = v.as_array().ok_or("unexpected /languages response")?;
        let src = arr
            .iter()
            .find(|l| l["code"].as_str() == Some(source))
            .ok_or_else(|| format!("source language '{}' not supported by this server", source))?;
        let mut targets: Vec<String> = src["targets"]
            .as_array()
            .map(|t| {
                t.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if targets.is_empty() {
            targets = arr
                .iter()
                .filter_map(|l| l["code"].as_str().map(String::from))
                .collect();
        }
        Ok(LtLanguages { targets })
    }
    pub fn fallback() -> LtLanguages {
        LtLanguages {
            targets: LT_FALLBACK.iter().map(|s| s.to_string()).collect(),
        }
    }
    /// LibreTranslate code to use for a gettext locale, if supported.
    pub fn map(&self, po_code: &str) -> Option<String> {
        lt_candidates(po_code)
            .into_iter()
            .find(|c| self.targets.iter().any(|t| t.eq_ignore_ascii_case(c)))
    }
}

/// Translate a list of strings with LibreTranslate. Returns one result per input.
pub fn libretranslate(
    p: &ProviderConfig,
    texts: &[String],
    source: &str,
    target: &str,
    html: bool,
    cancel: &Cancel,
) -> Result<Vec<String>, String> {
    let mut body = json!({
        "q": texts,
        "source": source,
        "target": target,
        "format": if html { "html" } else { "text" },
    });
    if !p.api_key.is_empty() {
        body["api_key"] = json!(p.api_key);
    }
    let url = format!("{}/translate", p.url);
    let v = post_logged(cancel, url, vec![], body, &["api_key"])?;
    match &v["translatedText"] {
        Value::Array(a) => {
            let out: Vec<String> = a
                .iter()
                .map(|x| x.as_str().unwrap_or("").to_string())
                .collect();
            if out.len() != texts.len() {
                return Err(format!(
                    "LibreTranslate returned {} results for {} inputs",
                    out.len(),
                    texts.len()
                ));
            }
            Ok(out)
        }
        Value::String(s) if texts.len() == 1 => Ok(vec![s.clone()]),
        _ => Err(format!(
            "unexpected LibreTranslate response: {}",
            v.to_string().chars().take(200).collect::<String>()
        )),
    }
}
