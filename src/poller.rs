//! Credentials, provider network requests, response parsing, time formatting.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::diagnose;
use crate::http;
use crate::models::{AppUsageData, CodexResetCredits, ScopedLimit, UsageData, UsageSection};
use crate::strings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    ClaudeCode,
    Codex,
    Antigravity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    NoCredentials,
    AuthRequired,
    TokenExpired,
    RequestFailed,
}

impl PollError {
    pub fn is_auth_error(self) -> bool {
        matches!(
            self,
            PollError::NoCredentials | PollError::AuthRequired | PollError::TokenExpired
        )
    }
}

/// Poll all enabled providers sequentially (Claude Code -> Codex -> Antigravity).
///
/// Success of at least one provider makes the poll successful. When every
/// enabled provider fails, the first error is returned (it drives the warning
/// text). Individual provider failures are logged only when more than one
/// provider is enabled.
pub fn poll(
    show_claude: bool,
    show_codex: bool,
    show_antigravity: bool,
) -> Result<AppUsageData, (Provider, PollError)> {
    let enabled = [show_claude, show_codex, show_antigravity]
        .iter()
        .filter(|b| **b)
        .count();
    let mut data = AppUsageData::default();
    let mut any_ok = false;
    let mut first_err: Option<(Provider, PollError)> = None;

    if show_claude {
        match poll_claude() {
            Ok((usage, scoped)) => {
                data.claude_code = Some(usage);
                data.claude_scoped = scoped;
                any_ok = true;
            }
            Err(e) => {
                if enabled > 1 {
                    diagnose::log(&format!("claude code poll failed: {e:?}"));
                }
                first_err.get_or_insert((Provider::ClaudeCode, e));
            }
        }
    }
    if show_codex {
        match poll_codex() {
            Ok((usage, available)) => {
                data.codex = Some(usage);
                data.codex_reset_credits_available = available;
                any_ok = true;
            }
            Err(e) => {
                if enabled > 1 {
                    diagnose::log(&format!("codex poll failed: {e:?}"));
                }
                first_err.get_or_insert((Provider::Codex, e));
            }
        }
    }
    if show_antigravity {
        match poll_antigravity() {
            Ok(usage) => {
                data.antigravity = Some(usage);
                any_ok = true;
            }
            Err(e) => {
                if enabled > 1 {
                    diagnose::log(&format!("antigravity poll failed: {e:?}"));
                }
                first_err.get_or_insert((Provider::Antigravity, e));
            }
        }
    }

    if any_ok {
        Ok(data)
    } else {
        Err(first_err.unwrap_or((Provider::ClaudeCode, PollError::RequestFailed)))
    }
}

// --- Child process helpers ---------------------------------------------------

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CLI_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const CLI_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

struct RunOutput {
    success: bool,
    stdout: Vec<u8>,
}

/// Run a child process without a console window, with stdin/stderr on null,
/// waiting no longer than `timeout` (polled at `step`); a stuck process is
/// killed. Returns `None` when the process failed to start or was killed.
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
    step: Duration,
    capture_stdout: bool,
) -> Option<RunOutput> {
    use std::os::windows::process::CommandExt;
    cmd.stdin(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd.stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn().ok()?;
    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(step);
            }
            Err(_) => break None,
        }
    };
    let stdout = reader
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    status.map(|s| RunOutput {
        success: s.success(),
        stdout,
    })
}

/// Decode console output that may be UTF-16LE (`wsl.exe -l -q`) or UTF-8.
/// Heuristic: when at least half of the byte pairs have a zero high byte,
/// the text is UTF-16LE.
fn decode_console_bytes(bytes: &[u8]) -> String {
    let mut b = bytes;
    if b.len() >= 2 && b[0] == 0xFF && b[1] == 0xFE {
        b = &b[2..];
    }
    let pairs = b.len() / 2;
    if pairs > 0 {
        let zero_high = b.chunks_exact(2).filter(|c| c[1] == 0).count();
        if zero_high * 2 >= pairs {
            let units: Vec<u16> = b
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            return String::from_utf16_lossy(&units);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Build a command for a CLI path: `.cmd`/`.bat` go through `cmd.exe /c`,
/// `.ps1` through `powershell.exe -NoProfile -ExecutionPolicy Bypass -File`.
fn build_cli_command(path: &str, args: &[&str]) -> Command {
    let lower = path.to_ascii_lowercase();
    let mut cmd = if lower.ends_with(".cmd") || lower.ends_with(".bat") {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(path);
        c
    } else if lower.ends_with(".ps1") {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(path);
        c
    } else {
        Command::new(path)
    };
    cmd.args(args);
    cmd
}

fn where_exe(name: &str) -> Option<String> {
    let mut cmd = Command::new("where.exe");
    cmd.arg(name);
    let out = run_with_timeout(cmd, CLI_QUERY_TIMEOUT, Duration::from_millis(100), true)?;
    if !out.success {
        return None;
    }
    decode_console_bytes(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(String::from)
}

/// Locate a CLI: try launching each candidate with `--version`, then ask
/// `where.exe` for each; fall back to the first candidate name.
fn find_cli(candidates: &[&str]) -> String {
    for c in candidates {
        let cmd = build_cli_command(c, &["--version"]);
        if run_with_timeout(cmd, CLI_QUERY_TIMEOUT, Duration::from_millis(100), false)
            .map_or(false, |o| o.success)
        {
            return (*c).to_string();
        }
    }
    for c in candidates {
        if let Some(path) = where_exe(c) {
            return path;
        }
    }
    candidates[0].to_string()
}

// --- Time parsing and formatting ---------------------------------------------

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn unix_secs_to_system_time(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Manual ISO 8601 parser. The string is cut at the first `+` and at `Z`;
/// fractional seconds are dropped. Timezone offsets are ignored: everything is
/// treated as UTC (the APIs in use return UTC).
pub fn parse_iso8601(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let s = s.split('+').next().unwrap_or(s);
    let s = s.trim_end_matches('Z');
    let s = s.split('.').next().unwrap_or(s);
    // A negative offset (`-05:00`) is not cut explicitly; truncating to the
    // fixed `YYYY-MM-DDTHH:MM:SS` length drops it as well.
    let s = s.get(..19).unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: usize = dp.next()?.parse().ok()?;
    let day: u64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hour: u64 = tp.next()?.parse().ok()?;
    let minute: u64 = tp.next()?.parse().ok()?;
    let second: u64 = tp.next()?.parse().ok()?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let month_lengths: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    if day < 1 || day > month_lengths[month - 1] {
        return None;
    }
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    days += month_lengths[..month - 1].iter().sum::<u64>();
    days += day - 1;
    Some(unix_secs_to_system_time(
        days * 86400 + hour * 3600 + minute * 60 + second,
    ))
}

/// Countdown text: `4d2h`, `2h11m`, `5m`, `30s`, or `now` once expired.
pub fn format_countdown(until: SystemTime, now: SystemTime) -> String {
    let Ok(rem) = until.duration_since(now) else {
        return "now".to_string();
    };
    let secs = rem.as_secs();
    if secs >= 86400 {
        format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
    } else if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Usage line: `42% · 2h11m`; percent only when there is no reset time;
/// `--` for a window the provider does not report.
pub fn format_usage_text(section: &UsageSection, now: SystemTime) -> String {
    if section.is_unsupported() {
        return strings::PLACEHOLDER_NONE.to_string();
    }
    let pct = format!("{:.0}%", section.percentage);
    match section.resets_at {
        Some(t) => format!("{pct} \u{00B7} {}", format_countdown(t, now)),
        None => pct,
    }
}

/// Seconds until the countdown text changes, used to schedule the countdown
/// timer. One extra second is added so the timer fires after the value has
/// changed, not exactly on the boundary. `None` once the time has passed.
pub fn seconds_until_text_change(until: SystemTime, now: SystemTime) -> Option<u64> {
    let rem = until.duration_since(now).ok()?;
    let secs = rem.as_secs();
    Some(if secs >= 86400 {
        secs % 3600 + 1
    } else if secs >= 60 {
        secs % 60 + 1
    } else {
        1
    })
}

/// Codex reset-credit display lines, recomputed on every countdown tick.
/// Expired credits are dropped here (not at parse time) so an expired line
/// disappears on its own without a new network request. Credits without an
/// expiry cannot be ordered and collapse into a single count line.
pub fn build_reset_lines(credits: &CodexResetCredits, now: SystemTime) -> Vec<String> {
    let mut dated: Vec<SystemTime> = credits
        .expiries
        .iter()
        .flatten()
        .copied()
        .filter(|t| *t > now)
        .collect();
    dated.sort();
    let mut lines: Vec<String> = dated.into_iter().map(|t| format_countdown(t, now)).collect();
    let undated = credits.expiries.iter().filter(|e| e.is_none()).count();
    if undated > 0 {
        lines.push(undated.to_string());
    }
    lines
}

// --- Claude Code -------------------------------------------------------------

const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const CLAUDE_BETA: &str = "oauth-2025-04-20";
const CLAUDE_FALLBACK_MODELS: [&str; 2] = ["claude-3-haiku-20240307", "claude-haiku-4-5-20251001"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeSource {
    WindowsFile(PathBuf),
    Wsl(String),
}

/// The credential source currently in use, kept for credential watching.
static ACTIVE_CLAUDE_SOURCE: Mutex<Option<ClaudeSource>> = Mutex::new(None);

struct ClaudeCreds {
    access_token: String,
    expires_at_ms: Option<u64>,
}

impl ClaudeCreds {
    /// `expiresAt` is Unix time in milliseconds; a missing field means
    /// "not expired".
    fn is_expired(&self) -> bool {
        match self.expires_at_ms {
            Some(ms) => now_ms() >= ms,
            None => false,
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    match std::env::var("USERPROFILE") {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
        _ => {
            let drive = std::env::var("HOMEDRIVE").unwrap_or_default();
            let path = std::env::var("HOMEPATH").unwrap_or_default();
            if drive.is_empty() && path.is_empty() {
                None
            } else {
                Some(PathBuf::from(format!("{drive}{path}")))
            }
        }
    }
}

fn claude_credentials_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude").join(".credentials.json"))
}

fn parse_claude_creds(bytes: &[u8]) -> Option<ClaudeCreds> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let token = oauth.get("accessToken")?.as_str()?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    Some(ClaudeCreds {
        access_token: token,
        expires_at_ms: oauth.get("expiresAt").and_then(|e| e.as_u64()),
    })
}

fn list_wsl_distros() -> Vec<String> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"]);
    let Some(out) = run_with_timeout(cmd, CLI_QUERY_TIMEOUT, Duration::from_millis(100), true)
    else {
        return Vec::new();
    };
    if !out.success {
        return Vec::new();
    }
    decode_console_bytes(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_matches('\0').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn read_claude_source(source: &ClaudeSource) -> Option<ClaudeCreds> {
    match source {
        ClaudeSource::WindowsFile(path) => {
            std::fs::read(path).ok().and_then(|b| parse_claude_creds(&b))
        }
        ClaudeSource::Wsl(distro) => {
            let mut cmd = Command::new("wsl.exe");
            cmd.args([
                "-d",
                distro,
                "--",
                "sh",
                "-lc",
                "cat ~/.claude/.credentials.json",
            ]);
            let out = run_with_timeout(cmd, CLI_QUERY_TIMEOUT, Duration::from_millis(100), true)?;
            if !out.success {
                return None;
            }
            parse_claude_creds(&out.stdout)
        }
    }
}

/// Ordered credential sources: the Windows file first, then WSL distributions
/// (enumerated lazily, only when a next source is actually needed).
struct ClaudeSourceIter {
    yielded_windows: bool,
    wsl: Option<std::vec::IntoIter<String>>,
}

impl ClaudeSourceIter {
    fn new() -> Self {
        Self {
            yielded_windows: false,
            wsl: None,
        }
    }

    fn next(&mut self) -> Option<ClaudeSource> {
        if !self.yielded_windows {
            self.yielded_windows = true;
            if let Some(path) = claude_credentials_path() {
                return Some(ClaudeSource::WindowsFile(path));
            }
        }
        self.wsl
            .get_or_insert_with(|| list_wsl_distros().into_iter())
            .next()
            .map(ClaudeSource::Wsl)
    }
}

/// Ask the CLI to refresh the token. The child environment must not contain
/// `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT`, otherwise the CLI decides it is
/// running inside itself.
fn refresh_claude_token(source: &ClaudeSource) {
    diagnose::log(&format!("refreshing claude token via CLI for {source:?}"));
    match source {
        ClaudeSource::WindowsFile(_) => {
            let cli = find_cli(&["claude.cmd", "claude"]);
            let mut cmd = build_cli_command(&cli, &["-p", "."]);
            cmd.env_remove("CLAUDECODE");
            cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
            run_with_timeout(cmd, CLI_REFRESH_TIMEOUT, Duration::from_millis(500), false);
        }
        ClaudeSource::Wsl(distro) => {
            let script = concat!(
                "if command -v claude >/dev/null 2>&1; then claude -p .; ",
                "elif [ -x \"$HOME/.local/bin/claude\" ]; then \"$HOME/.local/bin/claude\" -p .; ",
                "else exit 127; fi"
            );
            let mut cmd = Command::new("wsl.exe");
            cmd.args(["-d", distro, "--", "bash", "-lic", script]);
            cmd.env_remove("CLAUDECODE");
            cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
            run_with_timeout(cmd, CLI_REFRESH_TIMEOUT, Duration::from_millis(500), false);
        }
    }
}

fn poll_claude() -> Result<(UsageData, Vec<ScopedLimit>), PollError> {
    let mut sources = ClaudeSourceIter::new();
    let mut current: Option<(ClaudeSource, ClaudeCreds)> = None;
    while let Some(source) = sources.next() {
        if let Some(creds) = read_claude_source(&source) {
            current = Some((source, creds));
            break;
        }
    }
    let (mut source, mut creds) = current.ok_or_else(|| {
        diagnose::log("claude code: no credentials found in any source");
        PollError::NoCredentials
    })?;

    // Expired token: ask the CLI to refresh, re-read, otherwise move on to the
    // next credential source; once sources run out, report TokenExpired.
    'expiry: while creds.is_expired() {
        refresh_claude_token(&source);
        if let Some(reread) = read_claude_source(&source) {
            creds = reread;
        }
        if !creds.is_expired() {
            break;
        }
        while let Some(next) = sources.next() {
            if let Some(next_creds) = read_claude_source(&next) {
                source = next;
                creds = next_creds;
                continue 'expiry;
            }
        }
        *active_claude_source() = Some(source);
        diagnose::log("claude code: token still expired after refresh, no more sources");
        return Err(PollError::TokenExpired);
    }

    *active_claude_source() = Some(source);
    fetch_claude_usage(&creds.access_token)
}

fn active_claude_source() -> std::sync::MutexGuard<'static, Option<ClaudeSource>> {
    ACTIVE_CLAUDE_SOURCE.lock().unwrap_or_else(|e| e.into_inner())
}

fn fetch_claude_usage(token: &str) -> Result<(UsageData, Vec<ScopedLimit>), PollError> {
    let auth = format!("Bearer {token}");
    let headers = [
        ("Authorization", auth.as_str()),
        ("anthropic-beta", CLAUDE_BETA),
    ];
    // The Messages API fallback reports only the shared windows: model-scoped
    // limits are available on the JSON endpoint alone.
    let fallback = |token| claude_messages_fallback(token).map(|u| (u, Vec::new()));
    match http::get(CLAUDE_USAGE_URL, &headers) {
        Err(e) => {
            diagnose::log(&format!("claude usage endpoint request failed: {e}"));
            fallback(token)
        }
        Ok(resp) if resp.status == 401 || resp.status == 403 => {
            diagnose::log(&format!("claude usage endpoint auth error: {}", resp.status));
            Err(PollError::AuthRequired)
        }
        Ok(resp) if resp.is_success() => match resp.json().and_then(|v| parse_claude_usage_value(&v)) {
            Some((mut usage, scoped)) => {
                // The endpoint answered but without reset times: pull only the
                // times from the Messages API, keeping the percents.
                if usage.session.resets_at.is_none() || usage.weekly.resets_at.is_none() {
                    if let Ok(times) = claude_messages_fallback(token) {
                        if usage.session.resets_at.is_none() {
                            usage.session.resets_at = times.session.resets_at;
                        }
                        if usage.weekly.resets_at.is_none() {
                            usage.weekly.resets_at = times.weekly.resets_at;
                        }
                    }
                }
                Ok((usage, scoped))
            }
            None => {
                diagnose::log("claude usage endpoint returned unparseable JSON");
                fallback(token)
            }
        },
        Ok(resp) => {
            diagnose::log(&format!("claude usage endpoint status {}", resp.status));
            fallback(token)
        }
    }
}

fn parse_claude_usage_value(v: &serde_json::Value) -> Option<(UsageData, Vec<ScopedLimit>)> {
    let section = |name: &str| -> Option<UsageSection> {
        let s = v.get(name)?;
        Some(UsageSection {
            // `utilization` here is already in percent, taken as is.
            percentage: s.get("utilization")?.as_f64()?,
            resets_at: s
                .get("resets_at")
                .and_then(|r| r.as_str())
                .and_then(parse_iso8601),
        })
    };
    let five = section("five_hour");
    let seven = section("seven_day");
    if five.is_none() && seven.is_none() {
        return None;
    }
    Some((
        UsageData {
            session: five.unwrap_or_default(),
            weekly: seven.unwrap_or_default(),
        },
        parse_claude_scoped_limits(v),
    ))
}

/// Model-scoped weekly limits (e.g. the separate Fable window) from the
/// `limits` array. Entries without a model display name are skipped.
fn parse_claude_scoped_limits(v: &serde_json::Value) -> Vec<ScopedLimit> {
    let Some(limits) = v.get("limits").and_then(|l| l.as_array()) else {
        return Vec::new();
    };
    limits
        .iter()
        .filter_map(|entry| {
            if entry.get("kind")?.as_str()? != "weekly_scoped" {
                return None;
            }
            let label = entry
                .get("scope")?
                .get("model")?
                .get("display_name")?
                .as_str()?
                .trim();
            if label.is_empty() {
                return None;
            }
            Some(ScopedLimit {
                label: label.to_string(),
                section: UsageSection {
                    // `percent` mirrors `utilization`: already in percent.
                    percentage: entry.get("percent")?.as_f64()?,
                    resets_at: entry
                        .get("resets_at")
                        .and_then(|r| r.as_str())
                        .and_then(parse_iso8601),
                },
            })
        })
        .collect()
}

/// Fallback: read the unified rate-limit headers off the Messages API. Even a
/// rejected call (e.g. 429) carries the headers, so the response is accepted
/// as soon as any of the relevant headers is present, regardless of status.
fn claude_messages_fallback(token: &str) -> Result<UsageData, PollError> {
    let auth = format!("Bearer {token}");
    for model in CLAUDE_FALLBACK_MODELS {
        let body = format!(
            "{{\"model\":\"{model}\",\"max_tokens\":1,\"messages\":[{{\"role\":\"user\",\"content\":\".\"}}]}}"
        );
        let headers = [
            ("Authorization", auth.as_str()),
            ("anthropic-version", "2023-06-01"),
            ("anthropic-beta", CLAUDE_BETA),
            ("Content-Type", "application/json"),
        ];
        match http::post(CLAUDE_MESSAGES_URL, &headers, body.as_bytes()) {
            Err(e) => {
                diagnose::log(&format!("claude messages fallback ({model}) failed: {e}"));
            }
            Ok(resp) => {
                if resp.status == 401 || resp.status == 403 {
                    diagnose::log(&format!(
                        "claude messages fallback auth error: {}",
                        resp.status
                    ));
                    return Err(PollError::AuthRequired);
                }
                let has_headers = resp
                    .header("anthropic-ratelimit-unified-5h-utilization")
                    .is_some()
                    || resp
                        .header("anthropic-ratelimit-unified-7d-utilization")
                        .is_some()
                    || resp.header("anthropic-ratelimit-unified-status").is_some();
                if has_headers {
                    return Ok(parse_claude_ratelimit_headers(&resp));
                }
                diagnose::log(&format!(
                    "claude messages fallback ({model}): status {}, no ratelimit headers",
                    resp.status
                ));
            }
        }
    }
    Err(PollError::RequestFailed)
}

fn parse_claude_ratelimit_headers(resp: &http::Response) -> UsageData {
    let fraction = |name: &str| {
        resp.header(name)
            .and_then(|v| v.trim().parse::<f64>().ok())
    };
    let reset = |name: &str| {
        resp.header(name)
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(unix_secs_to_system_time)
    };
    // Unlike the JSON endpoint, the headers carry a fraction (0..1).
    let mut five_pct = fraction("anthropic-ratelimit-unified-5h-utilization").map(|v| v * 100.0);
    let mut seven_pct = fraction("anthropic-ratelimit-unified-7d-utilization").map(|v| v * 100.0);
    let mut five_reset = reset("anthropic-ratelimit-unified-5h-reset");
    let seven_reset = reset("anthropic-ratelimit-unified-7d-reset");

    // Both percents zero on a rejected call: the representative claim tells
    // which window is exhausted and is shown as 100%.
    if five_pct.unwrap_or(0.0) == 0.0
        && seven_pct.unwrap_or(0.0) == 0.0
        && resp.header("anthropic-ratelimit-unified-status") == Some("rejected")
    {
        match resp.header("anthropic-ratelimit-unified-representative-claim") {
            Some("five_hour") => five_pct = Some(100.0),
            Some("seven_day") => seven_pct = Some(100.0),
            _ => {}
        }
        if five_reset.is_none() {
            five_reset = reset("anthropic-ratelimit-unified-reset");
        }
    }

    UsageData {
        session: UsageSection {
            percentage: five_pct.unwrap_or(0.0),
            resets_at: five_reset,
        },
        weekly: UsageSection {
            percentage: seven_pct.unwrap_or(0.0),
            resets_at: seven_reset,
        },
    }
}

// --- Codex -------------------------------------------------------------------

const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CODEX_CREDITS_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

struct CodexCreds {
    access_token: String,
    account_id: Option<String>,
}

fn codex_auth_path() -> Option<PathBuf> {
    match std::env::var("CODEX_HOME") {
        Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p).join("auth.json")),
        _ => home_dir().map(|h| h.join(".codex").join("auth.json")),
    }
}

fn read_codex_creds() -> Option<CodexCreds> {
    let bytes = std::fs::read(codex_auth_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let tokens = v.get("tokens")?;
    let token = tokens.get("access_token")?.as_str()?.trim().to_string();
    // An empty access token counts as absent.
    if token.is_empty() {
        return None;
    }
    let account_id = tokens
        .get("account_id")
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Some(CodexCreds {
        access_token: token,
        account_id,
    })
}

fn codex_get(url: &str, creds: &CodexCreds) -> Result<http::Response, String> {
    let auth = format!("Bearer {}", creds.access_token);
    // The codex-cli user agent is mandatory: the endpoint rejects the default.
    let mut headers = vec![
        ("Authorization", auth.as_str()),
        ("User-Agent", "codex-cli"),
    ];
    if let Some(id) = &creds.account_id {
        headers.push(("ChatGPT-Account-Id", id.as_str()));
    }
    http::get(url, &headers)
}

fn refresh_codex_token() {
    diagnose::log("refreshing codex token via CLI");
    let cli = find_cli(&["codex.cmd", "codex.ps1", "codex.exe", "codex"]);
    let cmd = build_cli_command(&cli, &["exec", "."]);
    run_with_timeout(cmd, CLI_REFRESH_TIMEOUT, Duration::from_millis(500), false);
}

/// GET a Codex endpoint; on 401/403 run `codex exec .`, re-read auth.json and
/// retry exactly once.
fn codex_request(url: &str) -> Result<http::Response, PollError> {
    let creds = read_codex_creds().ok_or(PollError::NoCredentials)?;
    let resp = codex_get(url, &creds).map_err(|e| {
        diagnose::log(&format!("codex request failed: {e}"));
        PollError::RequestFailed
    })?;
    if resp.status != 401 && resp.status != 403 {
        return Ok(resp);
    }
    diagnose::log(&format!("codex auth error {} on {url}", resp.status));
    refresh_codex_token();
    let creds = read_codex_creds().ok_or(PollError::NoCredentials)?;
    let resp = codex_get(url, &creds).map_err(|e| {
        diagnose::log(&format!("codex retry request failed: {e}"));
        PollError::RequestFailed
    })?;
    if resp.status == 401 || resp.status == 403 {
        diagnose::log(&format!("codex auth error {} after refresh", resp.status));
        return Err(PollError::AuthRequired);
    }
    Ok(resp)
}

fn poll_codex() -> Result<(UsageData, Option<usize>), PollError> {
    let resp = codex_request(CODEX_USAGE_URL)?;
    if !resp.is_success() {
        diagnose::log(&format!("codex usage endpoint status {}", resp.status));
        return Err(PollError::RequestFailed);
    }
    let Some(v) = resp.json() else {
        diagnose::log("codex usage endpoint returned unparseable JSON");
        return Err(PollError::RequestFailed);
    };
    Ok(parse_codex_usage(&v))
}

struct CodexWindow {
    section: UsageSection,
    window_seconds: Option<u64>,
}

/// Fields can be either absent or explicit `null`; both must be survived.
fn non_null<'a>(v: Option<&'a serde_json::Value>) -> Option<&'a serde_json::Value> {
    v.filter(|x| !x.is_null())
}

fn parse_codex_window(v: &serde_json::Value) -> Option<CodexWindow> {
    let used = v.get("used_percent")?.as_f64()?;
    Some(CodexWindow {
        section: UsageSection {
            percentage: used,
            // `reset_at` is Unix seconds (Claude's expiresAt is milliseconds).
            resets_at: v
                .get("reset_at")
                .and_then(|r| r.as_u64())
                .map(unix_secs_to_system_time),
        },
        window_seconds: v.get("limit_window_seconds").and_then(|s| s.as_u64()),
    })
}

fn parse_codex_usage(v: &serde_json::Value) -> (UsageData, Option<usize>) {
    let available = v
        .get("rate_limit_reset_credits")
        .and_then(|c| c.get("available_count"))
        .and_then(|n| n.as_u64())
        .map(|n| n as usize);
    let rate_limit = non_null(v.get("rate_limit"));
    let window = |name: &str| {
        rate_limit
            .and_then(|r| non_null(r.get(name)))
            .and_then(parse_codex_window)
    };
    let primary = window("primary_window");
    let secondary = window("secondary_window");
    let usage = match (primary, secondary) {
        (Some(p), Some(s)) => {
            // Primary maps to 5h and secondary to 7d unless primary is longer
            // than 24 hours while secondary is not; unknown lengths keep the
            // historic primary/secondary order.
            let p_long = p.window_seconds.map_or(false, |x| x > 86400);
            let s_long = s.window_seconds.map_or(false, |x| x > 86400);
            let (five, seven) = if p_long && !s_long { (s, p) } else { (p, s) };
            UsageData {
                session: five.section,
                weekly: seven.section,
            }
        }
        // Exactly one window always lands on the 7d row, regardless of its
        // declared length, so it does not jump between rows when the API
        // drops the 5-hour limit.
        (Some(w), None) | (None, Some(w)) => UsageData {
            session: UsageSection::default(),
            weekly: w.section,
        },
        (None, None) => UsageData::default(),
    };
    (usage, available)
}

/// Fetch the Codex reset-credit list.
pub fn poll_codex_reset_credits() -> Result<CodexResetCredits, PollError> {
    let resp = codex_request(CODEX_CREDITS_URL)?;
    if !resp.is_success() {
        diagnose::log(&format!("codex credits endpoint status {}", resp.status));
        return Err(PollError::RequestFailed);
    }
    let Some(v) = resp.json() else {
        diagnose::log("codex credits endpoint returned unparseable JSON");
        return Err(PollError::RequestFailed);
    };
    Ok(parse_codex_reset_credits(&v))
}

fn parse_codex_reset_credits(v: &serde_json::Value) -> CodexResetCredits {
    if let Some(credits) = v.get("credits").and_then(|c| c.as_array()) {
        let expiries = credits
            .iter()
            .filter(|c| {
                let supported =
                    c.get("is_supported_by_plan").and_then(|b| b.as_bool()) != Some(false);
                // A missing status counts as "available".
                let available = match c.get("status").and_then(|s| s.as_str()) {
                    None => true,
                    Some(s) => s == "available",
                };
                supported && available
            })
            .map(|c| {
                c.get("expires_at")
                    .and_then(|e| e.as_str())
                    .and_then(parse_iso8601)
            })
            .collect();
        CodexResetCredits { expiries }
    } else if let Some(n) = v.get("available_count").and_then(|n| n.as_u64()) {
        // No credits array: available_count yields N entries without dates.
        CodexResetCredits {
            expiries: vec![None; n as usize],
        }
    } else {
        CodexResetCredits::default()
    }
}

// --- Antigravity -------------------------------------------------------------

const ANTIGRAVITY_BASES: [&str; 3] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const ANTIGRAVITY_CRED_TARGET: &str = "gemini:antigravity";

mod credman {
    #[repr(C)]
    pub struct Filetime {
        pub low: u32,
        pub high: u32,
    }

    #[repr(C)]
    pub struct CredentialW {
        pub flags: u32,
        pub cred_type: u32,
        pub target_name: *mut u16,
        pub comment: *mut u16,
        pub last_written: Filetime,
        pub credential_blob_size: u32,
        pub credential_blob: *mut u8,
        pub persist: u32,
        pub attribute_count: u32,
        pub attributes: *mut core::ffi::c_void,
        pub target_alias: *mut u16,
        pub user_name: *mut u16,
    }

    pub const CRED_TYPE_GENERIC: u32 = 1;

    #[link(name = "advapi32")]
    extern "system" {
        pub fn CredReadW(
            target_name: *const u16,
            cred_type: u32,
            flags: u32,
            credential: *mut *mut CredentialW,
        ) -> i32;
        pub fn CredFree(buffer: *mut core::ffi::c_void);
    }
}

/// Read the Credential Manager blob for `gemini:antigravity`.
fn read_antigravity_blob() -> Option<Vec<u8>> {
    let target = crate::native_interop::to_wide(ANTIGRAVITY_CRED_TARGET);
    let mut cred: *mut credman::CredentialW = std::ptr::null_mut();
    unsafe {
        if credman::CredReadW(target.as_ptr(), credman::CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
            return None;
        }
        let c = &*cred;
        let blob = if c.credential_blob.is_null() || c.credential_blob_size == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(c.credential_blob, c.credential_blob_size as usize).to_vec()
        };
        credman::CredFree(cred as *mut _);
        Some(blob)
    }
}

fn read_antigravity_token() -> Option<String> {
    let blob = read_antigravity_blob()?;
    let v: serde_json::Value = serde_json::from_slice(&blob).ok()?;
    let token = v.get("token")?.get("access_token")?.as_str()?.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn antigravity_post(
    base: &str,
    path: &str,
    body: &[u8],
    token: &str,
) -> Result<http::Response, String> {
    let auth = format!("Bearer {token}");
    let headers = [
        ("Authorization", auth.as_str()),
        ("Content-Type", "application/json"),
        ("User-Agent", "antigravity"),
    ];
    http::post(&format!("{base}{path}"), &headers, body)
}

fn poll_antigravity() -> Result<UsageData, PollError> {
    let token = read_antigravity_token().ok_or_else(|| {
        diagnose::log("antigravity: no usable credential in Credential Manager");
        PollError::NoCredentials
    })?;
    // Base URLs are tried in order until the first success. An auth error on
    // any of them outranks other errors from the rest.
    let mut auth_seen = false;
    let mut last_err = PollError::RequestFailed;
    for base in ANTIGRAVITY_BASES {
        match antigravity_try_base(base, &token) {
            Ok(usage) => return Ok(usage),
            Err(PollError::AuthRequired) => auth_seen = true,
            Err(e) => last_err = e,
        }
    }
    if auth_seen {
        Err(PollError::AuthRequired)
    } else {
        Err(last_err)
    }
}

fn antigravity_try_base(base: &str, token: &str) -> Result<UsageData, PollError> {
    // Step 1: discover the project.
    let mut project: Option<String> = None;
    match antigravity_post(
        base,
        "/v1internal:loadCodeAssist",
        br#"{"metadata":{"ideType":"ANTIGRAVITY"}}"#,
        token,
    ) {
        Ok(resp) if resp.status == 401 || resp.status == 403 => {
            diagnose::log(&format!("antigravity {base} loadCodeAssist: {}", resp.status));
            return Err(PollError::AuthRequired);
        }
        Ok(resp) if resp.is_success() => {
            project = resp
                .json()
                .as_ref()
                .and_then(|v| v.get("cloudaicompanionProject"))
                .and_then(|p| p.as_str())
                .map(String::from);
        }
        Ok(resp) => {
            diagnose::log(&format!(
                "antigravity {base} loadCodeAssist status {}",
                resp.status
            ));
        }
        Err(e) => {
            diagnose::log(&format!("antigravity {base} loadCodeAssist failed: {e}"));
        }
    }

    // Step 2: quota summary, the preferred source.
    if let Some(p) = &project {
        let body = format!("{{\"project\":{}}}", serde_json::Value::String(p.clone()));
        match antigravity_post(base, "/v1internal:retrieveUserQuotaSummary", body.as_bytes(), token)
        {
            Ok(resp) if resp.status == 401 || resp.status == 403 => {
                diagnose::log(&format!(
                    "antigravity {base} quota summary: {}",
                    resp.status
                ));
                return Err(PollError::AuthRequired);
            }
            Ok(resp) if resp.is_success() => {
                if let Some(usage) = resp.json().as_ref().and_then(parse_antigravity_summary) {
                    return Ok(usage);
                }
                diagnose::log(&format!(
                    "antigravity {base} quota summary had no usable quotas"
                ));
            }
            Ok(resp) => {
                diagnose::log(&format!(
                    "antigravity {base} quota summary status {}",
                    resp.status
                ));
            }
            Err(e) => {
                diagnose::log(&format!("antigravity {base} quota summary failed: {e}"));
            }
        }
    }

    // Step 3: fall back to the model list.
    let body = match &project {
        Some(p) => format!("{{\"project\":{}}}", serde_json::Value::String(p.clone())),
        None => "{}".to_string(),
    };
    match antigravity_post(base, "/v1internal:fetchAvailableModels", body.as_bytes(), token) {
        Ok(resp) if resp.status == 401 || resp.status == 403 => {
            diagnose::log(&format!("antigravity {base} models: {}", resp.status));
            Err(PollError::AuthRequired)
        }
        Ok(resp) if resp.is_success() => resp
            .json()
            .as_ref()
            .and_then(parse_antigravity_models)
            .ok_or_else(|| {
                diagnose::log(&format!("antigravity {base} models had no usable quotas"));
                PollError::RequestFailed
            }),
        Ok(resp) => {
            diagnose::log(&format!("antigravity {base} models status {}", resp.status));
            Err(PollError::RequestFailed)
        }
        Err(e) => {
            diagnose::log(&format!("antigravity {base} models failed: {e}"));
            Err(PollError::RequestFailed)
        }
    }
}

/// The API returns the remaining quota; the UI shows consumption.
fn remaining_fraction_to_percent(fraction: f64) -> f64 {
    (1.0 - fraction.clamp(0.0, 1.0)) * 100.0
}

fn contains_gemini(v: Option<&serde_json::Value>) -> bool {
    v.and_then(|x| x.as_str())
        .map_or(false, |s| s.to_ascii_lowercase().contains("gemini"))
}

fn is_gemini_group(group: &serde_json::Value) -> bool {
    if contains_gemini(group.get("displayName")) || contains_gemini(group.get("description")) {
        return true;
    }
    group
        .get("buckets")
        .and_then(|b| b.as_array())
        .map_or(false, |buckets| {
            buckets.iter().any(|b| {
                b.get("bucketId")
                    .and_then(|id| id.as_str())
                    .map_or(false, |id| id.to_ascii_lowercase().starts_with("gemini-"))
                    || contains_gemini(b.get("displayName"))
            })
        })
}

fn antigravity_group_usage(group: &serde_json::Value) -> Option<UsageData> {
    let buckets = group.get("buckets")?.as_array()?;
    let mut session: Option<UsageSection> = None;
    let mut weekly: Option<UsageSection> = None;
    for bucket in buckets {
        let Some(window) = bucket.get("window").and_then(|w| w.as_str()) else {
            continue;
        };
        let Some(fraction) = bucket.get("remainingFraction").and_then(|f| f.as_f64()) else {
            continue;
        };
        let section = UsageSection {
            percentage: remaining_fraction_to_percent(fraction),
            resets_at: bucket
                .get("resetTime")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601),
        };
        // Window names compare case-insensitively; other windows are ignored.
        match window.to_ascii_lowercase().as_str() {
            "5h" => session = Some(section),
            "weekly" => weekly = Some(section),
            _ => {}
        }
    }
    if session.is_none() && weekly.is_none() {
        return None;
    }
    Some(UsageData {
        session: session.unwrap_or_default(),
        weekly: weekly.unwrap_or_default(),
    })
}

fn parse_antigravity_summary(v: &serde_json::Value) -> Option<UsageData> {
    let groups = v.get("groups")?.as_array()?;
    if let Some(gemini) = groups.iter().find(|g| is_gemini_group(g)) {
        if let Some(usage) = antigravity_group_usage(gemini) {
            return Some(usage);
        }
    }
    // No gemini group: the first group that yields any quotas at all.
    groups.iter().find_map(antigravity_group_usage)
}

fn parse_antigravity_models(v: &serde_json::Value) -> Option<UsageData> {
    const PREFIXES: [&str; 5] = ["gemini", "claude", "gpt", "image", "imagen"];
    let models = v.get("models")?.as_object()?;
    let mut best: Option<UsageSection> = None;
    for (name, model) in models {
        let lower = name.to_ascii_lowercase();
        if !PREFIXES.iter().any(|p| lower.starts_with(p)) {
            continue;
        }
        let Some(quota) = model.get("quotaInfo") else {
            continue;
        };
        let Some(fraction) = quota.get("remainingFraction").and_then(|f| f.as_f64()) else {
            continue;
        };
        let section = UsageSection {
            percentage: remaining_fraction_to_percent(fraction),
            resets_at: quota
                .get("resetTime")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601),
        };
        // The most consumed model wins; ties go to the later reset time.
        let better = match &best {
            None => true,
            Some(cur) => {
                section.percentage > cur.percentage
                    || (section.percentage == cur.percentage && section.resets_at > cur.resets_at)
            }
        };
        if better {
            best = Some(section);
        }
    }
    // The fallback fills only the 5h row; 7d stays empty.
    best.map(|session| UsageData {
        session,
        weekly: UsageSection::default(),
    })
}

// --- Credential watching -----------------------------------------------------

/// What to watch while polling is paused on an auth error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredWatchMode {
    /// Token expired/rejected: watch the source currently in use (or all
    /// sources when there is no active one).
    ActiveSource,
    /// No credentials at all: watch the Windows file plus all WSL distros.
    AllSources,
    /// Auth error with only Antigravity enabled: watch the Credential Manager
    /// entry.
    Antigravity,
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn file_signature(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(md) => {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("win:{}|present|{}|{}", path.display(), md.len(), mtime)
        }
        Err(_) => format!("win:{}|missing", path.display()),
    }
}

fn wsl_signature(distro: &str) -> String {
    let mut cmd = Command::new("wsl.exe");
    cmd.args([
        "-d",
        distro,
        "--",
        "sh",
        "-lc",
        "stat -c 'present|%s|%Y' ~/.claude/.credentials.json 2>/dev/null || echo missing",
    ]);
    let state = run_with_timeout(cmd, CLI_QUERY_TIMEOUT, Duration::from_millis(100), true)
        .filter(|o| o.success)
        .map(|o| decode_console_bytes(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "missing".to_string());
    format!("wsl:{distro}|{state}")
}

/// The Antigravity credential is not a file, so its blob content is hashed.
fn antigravity_signature() -> String {
    match read_antigravity_blob() {
        Some(blob) => format!(
            "{ANTIGRAVITY_CRED_TARGET}|present|{}|{:016x}",
            blob.len(),
            fnv1a64(&blob)
        ),
        None => format!("{ANTIGRAVITY_CRED_TARGET}|missing"),
    }
}

fn claude_source_signature(source: &ClaudeSource) -> String {
    match source {
        ClaudeSource::WindowsFile(path) => file_signature(path),
        ClaudeSource::Wsl(distro) => wsl_signature(distro),
    }
}

fn all_claude_signatures() -> Vec<String> {
    let mut sigs = Vec::new();
    if let Some(path) = claude_credentials_path() {
        sigs.push(file_signature(&path));
    }
    for distro in list_wsl_distros() {
        sigs.push(wsl_signature(&distro));
    }
    sigs
}

/// Collect credential-state signatures for the given watch mode. The list is
/// sorted and deduplicated so comparison does not depend on enumeration order.
pub fn credential_snapshot(
    mode: CredWatchMode,
    show_codex: bool,
    show_antigravity: bool,
) -> Vec<String> {
    let mut sigs = match mode {
        CredWatchMode::Antigravity => vec![antigravity_signature()],
        CredWatchMode::ActiveSource => {
            let active = active_claude_source().clone();
            let mut sigs = match active {
                Some(source) => vec![claude_source_signature(&source)],
                None => all_claude_signatures(),
            };
            append_extra_signatures(&mut sigs, show_codex, show_antigravity);
            sigs
        }
        CredWatchMode::AllSources => {
            let mut sigs = all_claude_signatures();
            append_extra_signatures(&mut sigs, show_codex, show_antigravity);
            sigs
        }
    };
    sigs.sort();
    sigs.dedup();
    sigs
}

fn append_extra_signatures(sigs: &mut Vec<String>, show_codex: bool, show_antigravity: bool) {
    if show_codex {
        if let Some(path) = codex_auth_path() {
            sigs.push(file_signature(&path));
        }
    }
    if show_antigravity {
        sigs.push(antigravity_signature());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        unix_secs_to_system_time(secs)
    }

    #[test]
    fn parses_iso8601_variants() {
        // 2026-03-05T08:00:00 UTC = 1772697600
        let expected = at(1_772_697_600);
        assert_eq!(parse_iso8601("2026-03-05T08:00:00"), Some(expected));
        assert_eq!(parse_iso8601("2026-03-05T08:00:00Z"), Some(expected));
        assert_eq!(
            parse_iso8601("2026-03-05T08:00:00.321598+00:00"),
            Some(expected)
        );
        assert_eq!(
            parse_iso8601("2026-03-05T08:00:00-05:00"),
            Some(expected),
            "negative offsets are truncated and treated as UTC"
        );
        // Leap year: 2024-02-29 exists.
        assert!(parse_iso8601("2024-02-29T00:00:00Z").is_some());
        assert!(parse_iso8601("2023-02-29T00:00:00Z").is_none());
        assert!(parse_iso8601("garbage").is_none());
        // Epoch sanity.
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(at(0)));
    }

    #[test]
    fn formats_countdown() {
        let now = at(1_000_000);
        assert_eq!(format_countdown(at(1_000_000 + 4 * 86400 + 2 * 3600), now), "4d2h");
        assert_eq!(format_countdown(at(1_000_000 + 2 * 3600 + 11 * 60), now), "2h11m");
        assert_eq!(format_countdown(at(1_000_000 + 5 * 60), now), "5m");
        assert_eq!(format_countdown(at(1_000_000 + 30), now), "30s");
        assert_eq!(format_countdown(at(999_999), now), "now");
    }

    #[test]
    fn formats_usage_text() {
        let now = at(1_000_000);
        let section = UsageSection {
            percentage: 42.0,
            resets_at: Some(at(1_000_000 + 2 * 3600 + 11 * 60)),
        };
        assert_eq!(format_usage_text(&section, now), "42% \u{00B7} 2h11m");
        let no_reset = UsageSection {
            percentage: 18.0,
            resets_at: None,
        };
        assert_eq!(format_usage_text(&no_reset, now), "18%");
        // Unsupported window renders as --, not 0%.
        assert_eq!(format_usage_text(&UsageSection::default(), now), "--");
    }

    #[test]
    fn schedules_text_changes() {
        let now = at(1_000_000);
        // >= 1 day: changes on the hour boundary.
        assert_eq!(
            seconds_until_text_change(at(1_000_000 + 86400 + 100), now),
            Some(101)
        );
        // >= 1 minute: changes on the minute boundary.
        assert_eq!(
            seconds_until_text_change(at(1_000_000 + 3600 + 125), now),
            Some(6)
        );
        assert_eq!(seconds_until_text_change(at(1_000_000 + 30), now), Some(1));
        assert_eq!(seconds_until_text_change(at(999_000), now), None);
    }

    #[test]
    fn decodes_console_bytes() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("Ubuntu\r\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode_console_bytes(&utf16), "Ubuntu\r\n");
        assert_eq!(decode_console_bytes("Ubuntu\r\n".as_bytes()), "Ubuntu\r\n");
    }

    #[test]
    fn parses_claude_usage_with_scoped_limits() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"five_hour":{"utilization":5.0,"resets_at":"2026-08-04T13:59:59Z"},
                "seven_day":{"utilization":25.0,"resets_at":"2026-08-09T08:59:59Z"},
                "limits":[
                  {"kind":"session","group":"session","percent":5,"resets_at":"2026-08-04T13:59:59Z","scope":null},
                  {"kind":"weekly_all","group":"weekly","percent":25,"resets_at":"2026-08-09T08:59:59Z","scope":null},
                  {"kind":"weekly_scoped","group":"weekly","percent":17,"resets_at":"2026-08-09T09:00:00Z",
                   "scope":{"model":{"id":null,"display_name":"Fable"},"surface":null}}]}"#,
        )
        .unwrap();
        let (usage, scoped) = parse_claude_usage_value(&v).unwrap();
        assert_eq!(usage.session.percentage, 5.0);
        assert_eq!(usage.weekly.percentage, 25.0);
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].label, "Fable");
        assert_eq!(scoped[0].section.percentage, 17.0);
        assert!(scoped[0].section.resets_at.is_some());

        // No limits array (older responses, other plans): no scoped rows.
        let v: serde_json::Value = serde_json::from_str(
            r#"{"five_hour":{"utilization":5.0,"resets_at":"2026-08-04T13:59:59Z"}}"#,
        )
        .unwrap();
        let (_, scoped) = parse_claude_usage_value(&v).unwrap();
        assert!(scoped.is_empty());
    }

    #[test]
    fn maps_codex_windows() {
        // Both windows, normal order.
        let v: serde_json::Value = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":7,"reset_at":100,"limit_window_seconds":18000},
                "secondary_window":{"used_percent":42,"reset_at":200,"limit_window_seconds":604800}},
                "rate_limit_reset_credits":{"available_count":1}}"#,
        )
        .unwrap();
        let (usage, available) = parse_codex_usage(&v);
        assert_eq!(usage.session.percentage, 7.0);
        assert_eq!(usage.weekly.percentage, 42.0);
        assert_eq!(available, Some(1));

        // Swapped: primary longer than a day, secondary not.
        let v: serde_json::Value = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":42,"limit_window_seconds":604800},
                "secondary_window":{"used_percent":7,"limit_window_seconds":18000}}}"#,
        )
        .unwrap();
        let (usage, _) = parse_codex_usage(&v);
        assert_eq!(usage.session.percentage, 7.0);
        assert_eq!(usage.weekly.percentage, 42.0);

        // A single window always lands on the 7d row.
        let v: serde_json::Value = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":61,"limit_window_seconds":18000},
                "secondary_window":null}}"#,
        )
        .unwrap();
        let (usage, _) = parse_codex_usage(&v);
        assert!(usage.session.is_unsupported());
        assert_eq!(usage.weekly.percentage, 61.0);

        // Absent and null rate_limit both survive.
        let (usage, _) = parse_codex_usage(&serde_json::json!({}));
        assert!(usage.session.is_unsupported() && usage.weekly.is_unsupported());
        let (usage, _) = parse_codex_usage(&serde_json::json!({ "rate_limit": null }));
        assert!(usage.session.is_unsupported() && usage.weekly.is_unsupported());
    }

    #[test]
    fn parses_codex_reset_credits() {
        let v = serde_json::json!({
            "credits": [
                {"status": "available", "expires_at": "2036-08-12T17:25:40Z", "is_supported_by_plan": true},
                {"status": "used", "expires_at": "2036-08-12T17:25:40Z"},
                {"expires_at": "2036-01-01T00:00:00Z"},
                {"status": "available", "is_supported_by_plan": false},
                {"status": "available"}
            ],
            "available_count": 3
        });
        let credits = parse_codex_reset_credits(&v);
        // used and unsupported are filtered out; missing status counts as available.
        assert_eq!(credits.expiries.len(), 3);
        assert_eq!(credits.expiries.iter().filter(|e| e.is_none()).count(), 1);

        // No credits array: available_count yields N undated entries.
        let credits = parse_codex_reset_credits(&serde_json::json!({"available_count": 2}));
        assert_eq!(credits.expiries, vec![None, None]);
    }

    #[test]
    fn builds_reset_lines() {
        let now = at(1_000_000);
        let credits = CodexResetCredits {
            expiries: vec![
                Some(at(1_000_000 + 9 * 86400 + 5 * 3600)), // 9d5h
                Some(at(999_000)),                          // expired: dropped
                None,
                None,
                Some(at(1_000_000 + 3600)), // 1h0m, sorts first
            ],
        };
        let lines = build_reset_lines(&credits, now);
        assert_eq!(lines, vec!["1h0m", "9d5h", "2"]);
    }

    #[test]
    fn parses_antigravity_summary() {
        let v = serde_json::json!({
            "groups": [
                {"displayName": "Other", "buckets": [
                    {"bucketId": "misc", "window": "5h", "remainingFraction": 0.5}
                ]},
                {"displayName": "Gemini Models", "buckets": [
                    {"bucketId": "gemini-5h", "window": "5H", "remainingFraction": 0.958, "resetTime": "2026-03-05T08:00:00Z"},
                    {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.993},
                    {"bucketId": "gemini-other", "window": "monthly", "remainingFraction": 0.5}
                ]}
            ]
        });
        let usage = parse_antigravity_summary(&v).unwrap();
        // Consumption is the inverted remainder; the gemini group wins.
        assert!((usage.session.percentage - 4.2).abs() < 1e-9);
        assert!((usage.weekly.percentage - 0.7).abs() < 1e-9);
        assert!(usage.session.resets_at.is_some());
        assert!(usage.weekly.resets_at.is_none());
    }

    #[test]
    fn parses_antigravity_models_fallback() {
        let v = serde_json::json!({
            "models": {
                "gemini-3-pro": {"quotaInfo": {"remainingFraction": 0.25, "resetTime": "2026-03-05T08:00:00Z"}},
                "gemini-flash": {"quotaInfo": {"remainingFraction": 0.9}},
                "tts-model": {"quotaInfo": {"remainingFraction": 0.0}}
            }
        });
        let usage = parse_antigravity_models(&v).unwrap();
        // The most consumed eligible model lands on the 5h row; 7d stays empty.
        assert!((usage.session.percentage - 75.0).abs() < 1e-9);
        assert!(usage.weekly.is_unsupported());
    }
}
