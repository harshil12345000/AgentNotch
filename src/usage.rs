//! Usage limits. Pluggable sources, tried in order; the first that answers wins.
//!
//! Source A (`ClaudeOAuth`): the endpoint Claude Code's /usage uses. It is undocumented and
//!   may change or rate-limit, so every failure falls through to source B.
//! Source B (`ClaudeEstimate`): token totals from the local transcripts, ccusage style.
//!
//! The OAuth token is read from Claude Code's credentials file (read-only), sent only to
//! api.anthropic.com, and never logged, stored or displayed.
use serde::Deserialize;
use std::collections::HashMap;
use std::io::BufRead;

use crate::app::{post, UiMsg};
use crate::config::UsageCfg;
use crate::model::{Usage, UsageWindow};
use crate::util::{now_unix, parse_rfc3339};

pub trait UsageSource: Sync {
    fn id(&self) -> &'static str;
    fn fetch(&self, cfg: &UsageCfg) -> Result<Usage, String>;
}

/// To add another provider (for example Codex limits), implement `UsageSource` and list it here.
pub static SOURCES: [&dyn UsageSource; 2] = [&ClaudeOAuth, &ClaudeEstimate];

/// Fetch on a background thread and post the result to the UI.
pub fn refresh(cfg: UsageCfg) {
    let _ = std::thread::Builder::new().name("usage".into()).stack_size(512 * 1024).spawn(move || {
        let mut notes = Vec::new();
        for src in SOURCES.iter() {
            match src.fetch(&cfg) {
                Ok(mut u) => {
                    if !notes.is_empty() {
                        u.note = notes.join("; ");
                    }
                    u.fetched_at = now_unix();
                    if u.source == "api" {
                        save_last(&u);
                    }
                    post(UiMsg::Usage(u));
                    return;
                }
                Err(e) => {
                    crate::logln!("usage[{}]: {e}", src.id());
                    notes.push(e);
                }
            }
        }
        post(UiMsg::Usage(Usage { note: notes.join("; "), fetched_at: now_unix(), ..Default::default() }));
    });
}

// ------------------------------------------------------------------ last good numbers

fn last_path() -> std::path::PathBuf {
    crate::util::data_dir().join("usage-last.tsv")
}

/// Remember real numbers from the endpoint, so a restart can show them at once instead of
/// asking the (rate-limited) endpoint again.
fn save_last(u: &Usage) {
    let line = |name: &str, w: &Option<UsageWindow>| match w {
        Some(w) => format!("{name}\t{}\t{}\n", w.pct.map(|p| p.to_string()).unwrap_or_default(), w.resets_at.map(|r| r.to_string()).unwrap_or_default()),
        None => String::new(),
    };
    let text = format!("at\t{}\n{}{}", u.fetched_at, line("five", &u.five_hour), line("week", &u.week));
    let _ = std::fs::write(last_path(), text);
}

/// The numbers saved by `save_last`, with the time they were fetched.
pub fn load_last() -> Option<Usage> {
    let text = std::fs::read_to_string(last_path()).ok()?;
    let mut u = Usage { source: "api", ..Default::default() };
    for l in text.lines() {
        let mut p = l.split('\t');
        match (p.next()?, p.next(), p.next()) {
            ("at", Some(t), _) => u.fetched_at = t.parse().ok()?,
            (name @ ("five" | "week"), Some(pct), reset) => {
                let resets_at: Option<u64> = reset.and_then(|r| r.parse().ok());
                let pct: Option<f32> = pct.parse().ok();
                let active = resets_at.is_some() || pct.map(|p| p > 0.0).unwrap_or(false);
                let w = UsageWindow { pct, tokens: None, resets_at, active };
                if name == "five" {
                    u.five_hour = Some(w);
                } else {
                    u.week = Some(w);
                }
            }
            _ => {}
        }
    }
    (u.fetched_at > 0 && (u.five_hour.is_some() || u.week.is_some())).then_some(u)
}

// ------------------------------------------------------------------ source A

pub struct ClaudeOAuth;

fn claude_dir() -> std::path::PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => std::path::PathBuf::from(d),
        _ => crate::util::home().join(".claude"),
    }
}

#[derive(Deserialize)]
struct Creds {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OAuth>,
}
#[derive(Deserialize)]
struct OAuth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<f64>,
}

#[derive(Deserialize)]
struct ApiUsage {
    five_hour: Option<ApiWindow>,
    seven_day: Option<ApiWindow>,
}
#[derive(Deserialize)]
struct ApiWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

fn window(w: Option<ApiWindow>) -> Option<UsageWindow> {
    let w = w?;
    let pct = (w.utilization? as f32).clamp(0.0, 100.0);
    let resets_at = w.resets_at.as_deref().and_then(parse_rfc3339);
    // The endpoint reports a window with no reset time and 0% when no block is running
    // (for example the 5-hour window between sessions). That is "idle", not "0% used".
    let active = resets_at.is_some() || pct > 0.0;
    Some(UsageWindow { pct: Some(pct), tokens: None, resets_at, active })
}

fn read_oauth() -> Result<OAuth, String> {
    let bytes = std::fs::read(claude_dir().join(".credentials.json")).map_err(|_| "no Claude login found".to_string())?;
    let creds: Creds = serde_json::from_slice(&bytes).map_err(|_| "credentials file not understood".to_string())?;
    creds.oauth.ok_or("no Claude subscription login".to_string())
}

fn expired(o: &OAuth) -> bool {
    o.expires_at.map(|exp| exp > 0.0 && (exp / 1000.0) < (now_unix() + 60) as f64).unwrap_or(false)
}

/// The saved login goes stale when Claude Code has not been used from a terminal for a
/// while (for example when sessions run from the desktop app). AgentNotch never refreshes or
/// writes the token itself: it asks Claude Code to, by running `claude auth status`, which
/// renews its own credentials file. Output is discarded. At most once every 20 minutes.
fn ask_claude_to_refresh() {
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = now_unix();
    if now.saturating_sub(LAST.swap(now, Ordering::Relaxed)) < 1200 {
        return;
    }
    let child = std::process::Command::new("cmd.exe")
        .args(["/c", "claude", "auth", "status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000) // no window
        .spawn();
    if let Ok(mut c) = child {
        for _ in 0..100 {
            if matches!(c.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let _ = c.kill();
    }
}

impl UsageSource for ClaudeOAuth {
    fn id(&self) -> &'static str {
        "oauth"
    }
    fn fetch(&self, _cfg: &UsageCfg) -> Result<Usage, String> {
        let (status, body, retry) = fetch_raw()?;
        Self::read(status, &body, retry)
    }
}

/// The endpoint's reply as it came: status, body, Retry-After.
pub fn fetch_raw() -> Result<(u32, Vec<u8>, Option<u32>), String> {
    {
        let mut oauth = read_oauth()?;
        if expired(&oauth) {
            ask_claude_to_refresh();
            oauth = read_oauth()?;
            if expired(&oauth) {
                return Err("Claude sign-in expired: run `claude` once in a terminal".into());
            }
        }
        let mut token = oauth.access_token.filter(|t| !t.is_empty()).ok_or("no access token")?;
        let headers = format!("Authorization: Bearer {token}\r\nanthropic-beta: oauth-2025-04-20\r\nAccept: application/json\r\n");
        // drop our copy of the token as soon as the header is built
        unsafe { token.as_bytes_mut().fill(0) };
        http_get("api.anthropic.com", "/api/oauth/usage", headers)
    }
}

impl ClaudeOAuth {
    fn read(status: u32, body: &[u8], retry: Option<u32>) -> Result<Usage, String> {
        match status {
            200 => {
                let api: ApiUsage = serde_json::from_slice(body).map_err(|_| "usage reply not understood".to_string())?;
                let u = Usage { five_hour: window(api.five_hour), week: window(api.seven_day), source: "api", ..Default::default() };
                if u.five_hour.is_none() && u.week.is_none() {
                    return Err("usage reply had no limits (plan may not report them)".into());
                }
                Ok(u)
            }
            401 | 403 => Err("Claude sign-in rejected: run `claude` once in a terminal".into()),
            429 => Err(format!("usage endpoint rate limited{}", retry.map(|r| format!(" (retry in {r}s)")).unwrap_or_default())),
            s => Err(format!("usage endpoint returned HTTP {s}")),
        }
    }
}

/// Minimal HTTPS GET over WinHTTP (system TLS and proxy settings; nothing bundled).
/// Returns (status, body, Retry-After seconds).
fn http_get(host: &str, path: &str, headers: String) -> Result<(u32, Vec<u8>, Option<u32>), String> {
    use windows::core::*;
    use windows::Win32::Networking::WinHttp::*;
    let (whost, wpath) = (crate::util::wide(host), crate::util::wide(path));
    let mut wheaders: Vec<u16> = headers.encode_utf16().collect();
    drop(headers);
    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }
    unsafe {
        let ses = Handle(WinHttpOpen(w!("notch"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if ses.0.is_null() {
            return Err("network unavailable".into());
        }
        let _ = WinHttpSetTimeouts(ses.0, 5000, 5000, 5000, 8000);
        let con = Handle(WinHttpConnect(ses.0, PCWSTR(whost.as_ptr()), 443, 0));
        if con.0.is_null() {
            return Err("network unavailable".into());
        }
        let req = Handle(WinHttpOpenRequest(con.0, w!("GET"), PCWSTR(wpath.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if req.0.is_null() {
            return Err("network unavailable".into());
        }
        let sent = WinHttpSendRequest(req.0, Some(&wheaders), None, 0, 0, 0);
        wheaders.fill(0);
        sent.map_err(|_| "could not reach api.anthropic.com".to_string())?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|_| "no reply from api.anthropic.com".to_string())?;
        let mut status = 0u32;
        let mut size = 4u32;
        let _ = WinHttpQueryHeaders(req.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut _ as *mut _), &mut size, std::ptr::null_mut());
        let mut retry = 0u32;
        let mut size = 4u32;
        let has_retry = WinHttpQueryHeaders(req.0, WINHTTP_QUERY_RETRY_AFTER | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut retry as *mut _ as *mut _), &mut size, std::ptr::null_mut()).is_ok();
        let mut body = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let mut n = 0u32;
            if WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n).is_err() || n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > 1024 * 1024 {
                break;
            }
        }
        Ok((status, body, has_retry.then_some(retry)))
    }
}

// ------------------------------------------------------------------ source B

pub struct ClaudeEstimate;

#[derive(Deserialize)]
struct TLine {
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<TMsg>,
}
#[derive(Deserialize)]
struct TMsg {
    id: Option<String>,
    usage: Option<TUsage>,
}
#[derive(Deserialize)]
struct TUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

const HOUR: u64 = 3600;
const WEEK: u64 = 7 * 86400;

/// Per-transcript running totals, so each poll only reads bytes appended since the last one.
#[derive(Default)]
struct FileAgg {
    offset: u64,
    /// hour start (unix seconds) -> tokens
    hours: std::collections::BTreeMap<u64, u64>,
}

fn cache_path() -> std::path::PathBuf {
    crate::util::data_dir().join("usage-cache.tsv")
}

fn load_cache() -> HashMap<String, FileAgg> {
    let mut map: HashMap<String, FileAgg> = HashMap::new();
    let Ok(text) = std::fs::read_to_string(cache_path()) else { return map };
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let mut p = line.split('\t');
        match (p.next(), p.next(), p.next()) {
            (Some("F"), Some(path), Some(off)) => {
                map.insert(path.to_string(), FileAgg { offset: off.parse().unwrap_or(0), ..Default::default() });
                cur = Some(path.to_string());
            }
            (Some("H"), Some(h), Some(t)) => {
                if let (Some(c), Ok(h), Ok(t)) = (&cur, h.parse::<u64>(), t.parse::<u64>()) {
                    if let Some(a) = map.get_mut(c) {
                        a.hours.insert(h, t);
                    }
                }
            }
            _ => {}
        }
    }
    map
}

fn save_cache(map: &HashMap<String, FileAgg>) {
    let mut out = String::new();
    for (path, a) in map {
        out.push_str(&format!("F\t{path}\t{}\n", a.offset));
        for (h, t) in &a.hours {
            out.push_str(&format!("H\t{h}\t{t}\n"));
        }
    }
    let _ = std::fs::write(cache_path(), out);
}

/// Read the bytes appended to one transcript since `agg.offset` and add their tokens.
/// Streamed messages are written more than once; within one read the last write wins.
fn scan_file(path: &std::path::Path, agg: &mut FileAgg) {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < agg.offset {
        *agg = FileAgg::default(); // replaced or truncated: start over
    }
    if len == agg.offset || file.seek(SeekFrom::Start(agg.offset)).is_err() {
        return;
    }
    let mut rd = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut seen: HashMap<u64, (u64, u64)> = HashMap::new();
    let mut line = Vec::new();
    let mut consumed = agg.offset;
    loop {
        line.clear();
        match rd.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') {
            break; // partial last line: pick it up next time
        }
        consumed += line.len() as u64;
        if line.len() > 4 * 1024 * 1024 {
            continue;
        }
        // fast text search first; only assistant lines with a usage block are parsed
        let Ok(text) = std::str::from_utf8(&line) else { continue };
        if !text.contains("\"type\":\"assistant\"") || !text.contains("\"output_tokens\"") {
            continue;
        }
        let Ok(l) = serde_json::from_str::<TLine>(text) else { continue };
        let (Some(ts), Some(m)) = (l.timestamp.as_deref().and_then(parse_rfc3339), l.message) else { continue };
        let Some(u) = m.usage else { continue };
        // Cache reads are excluded: they are billed at a fraction and would swamp the total.
        let tokens = u.input_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0) + u.cache_creation_input_tokens.unwrap_or(0);
        seen.insert(hash(m.id.as_deref().unwrap_or(""), l.request_id.as_deref().unwrap_or(""), ts), (ts, tokens));
    }
    for (ts, tokens) in seen.into_values() {
        *agg.hours.entry(ts - ts % HOUR).or_insert(0) += tokens;
    }
    agg.offset = consumed;
}

/// (hour start, tokens) for the last 7 days, oldest first. The first call reads every
/// recent transcript once; later calls read only what was appended.
fn collect_entries(root: &std::path::Path) -> Vec<(u64, u64)> {
    let mut files = Vec::new();
    crate::watch::recent_files(root, 4, WEEK, &|p| p.extension().map(|e| e == "jsonl").unwrap_or(false), &mut files);
    let cutoff = now_unix().saturating_sub(WEEK);
    let mut cache = load_cache();
    let live: std::collections::HashSet<String> = files.iter().map(|f| f.to_string_lossy().to_string()).collect();
    cache.retain(|k, _| live.contains(k)); // forget transcripts older than a week
    let mut total: std::collections::BTreeMap<u64, u64> = Default::default();
    for f in &files {
        let agg = cache.entry(f.to_string_lossy().to_string()).or_default();
        scan_file(f, agg);
        agg.hours.retain(|h, _| *h + HOUR > cutoff);
        for (h, t) in &agg.hours {
            *total.entry(*h).or_insert(0) += t;
        }
    }
    save_cache(&cache);
    total.into_iter().collect()
}

fn hash(a: &str, b: &str, ts: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if a.is_empty() && b.is_empty() {
        ts.hash(&mut h);
    }
    a.hash(&mut h);
    b.hash(&mut h);
    h.finish()
}

/// The 5-hour session block that is active at `now`: a block starts on the hour of the
/// first message after a gap, and lasts five hours. Returns (tokens, block end).
pub fn active_block(entries: &[(u64, u64)], now: u64) -> Option<(u64, u64)> {
    let mut start = 0u64;
    let mut sum = 0u64;
    let mut last = 0u64;
    for &(ts, tok) in entries {
        if start == 0 || ts >= start + 5 * HOUR || ts.saturating_sub(last) >= 5 * HOUR {
            start = ts - ts % HOUR;
            sum = 0;
        }
        sum += tok;
        last = ts;
    }
    (start != 0 && now < start + 5 * HOUR).then_some((sum, start + 5 * HOUR))
}

impl UsageSource for ClaudeEstimate {
    fn id(&self) -> &'static str {
        "estimate"
    }
    fn fetch(&self, cfg: &UsageCfg) -> Result<Usage, String> {
        let root = crate::adapters::transcript::projects_dir();
        if !root.is_dir() {
            return Err("no local transcripts to estimate from".into());
        }
        // the first scan reads a week of transcripts: keep it out of the way
        unsafe {
            use windows::Win32::System::Threading::*;
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
        let entries = collect_entries(&root);
        let now = now_unix();
        let week_tokens: u64 = entries.iter().map(|e| e.1).sum();
        let pct = |tokens: u64, limit: u64| (limit > 0).then(|| (tokens as f32 / limit as f32 * 100.0).min(100.0));
        let five = match active_block(&entries, now) {
            Some((tokens, end)) => UsageWindow { pct: pct(tokens, cfg.estimate_limit_5h_tokens), tokens: Some(tokens), resets_at: Some(end), active: true },
            None => UsageWindow { pct: pct(0, cfg.estimate_limit_5h_tokens), tokens: Some(0), resets_at: None, active: false },
        };
        let week = UsageWindow { pct: pct(week_tokens, cfg.estimate_limit_week_tokens), tokens: Some(week_tokens), resets_at: None, active: true };
        Ok(Usage { five_hour: Some(five), week: Some(week), source: "estimate", ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        let h = HOUR;
        // messages at 10:20 and 11:00 -> block 10:00..15:00
        let e = [(10 * h + 1200, 100), (11 * h, 50)];
        assert_eq!(active_block(&e, 12 * h), Some((150, 15 * h)));
        // the block is over at 15:00
        assert_eq!(active_block(&e, 15 * h), None);
        // a message after the block ends starts a new one
        let e = [(10 * h, 100), (16 * h + 60, 7)];
        assert_eq!(active_block(&e, 17 * h), Some((7, 21 * h)));
    }

    #[test]
    fn api_reply_shape() {
        let api: ApiUsage = serde_json::from_str(r#"{"five_hour":{"utilization":42.5,"resets_at":"2026-11-14T15:00:00+00:00"},"seven_day":{"utilization":7,"resets_at":null},"seven_day_opus":null,"extra_usage":{"is_enabled":false}}"#).unwrap();
        let w = window(api.five_hour).unwrap();
        assert_eq!(w.pct, Some(42.5));
        assert_eq!(w.resets_at, Some(1_794_668_400));
        assert!(w.active);
        assert_eq!(window(api.seven_day).unwrap().resets_at, None);
    }

    #[test]
    fn inactive_window_is_idle() {
        // 0% with no reset time = no block running; shown as idle, not "0% used"
        let api: ApiUsage = serde_json::from_str(r#"{"five_hour":{"utilization":0,"resets_at":null},"seven_day":{"utilization":15,"resets_at":"2026-11-27T09:00:00+00:00"}}"#).unwrap();
        let five = window(api.five_hour).unwrap();
        assert_eq!(five.pct, Some(0.0));
        assert!(!five.active);
        assert!(!five.has_bar());
        assert!(window(api.seven_day).unwrap().active);
    }
}
