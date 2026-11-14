//! Claude Code transcript watcher: fallback and backfill for sessions that have no hooks
//! (or that started before Notch did).
//!
//! Watches ~/.claude/projects/<project>/<session>.jsonl and reads only appended bytes.
//! The line format is internal to Claude Code and may change between versions, so every
//! field is optional here and unknown lines are skipped. Checked against Claude Code 2.1.319.
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::claude::tool_step;
use crate::app::{post, UiMsg};
use crate::config::Config;
use crate::model::{Agent, AgentEvent, EventKind};
use crate::util::{clip, now_unix, parse_rfc3339};
use crate::watch::{recent_files, watch_dir, Tailer};

/// Read at most this much of a file the first time we see it.
const FIRST_TAIL: u64 = 256 * 1024;
/// Lines longer than this are request blobs, never state: skip them unparsed.
const MAX_LINE: usize = 512 * 1024;
/// A session whose transcript says "working" but has not been written for this long was
/// interrupted or closed.
const STALE_SECS: u64 = 180;

/// Sessions that have live hook events: their transcripts are not parsed at all.
pub static LIVE: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn mark_live(session_id: &str) {
    if let Ok(mut g) = LIVE.lock() {
        g.get_or_insert_with(HashSet::new).insert(session_id.to_string());
    }
}
fn is_live(session_id: &str) -> bool {
    LIVE.lock().map(|g| g.as_ref().map(|s| s.contains(session_id)).unwrap_or(false)).unwrap_or(false)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    is_sidechain: Option<bool>,
    custom_title: Option<String>,
    message: Option<Msg>,
}
#[derive(Deserialize)]
struct Msg {
    content: Option<Value>,
    stop_reason: Option<String>,
}

/// Turn one transcript line into an event. `prefix` converts a WSL path to a Windows one.
pub fn parse_line(line: &str, wsl: &str, info_only: bool) -> Option<AgentEvent> {
    // cheap pre-filter: most lines (request records, attachments, snapshots) are not state
    let info = line.contains("\"type\":\"custom-title\"") || line.contains("\"type\":\"bridge-session\"");
    if !info && (info_only || !(line.contains("\"type\":\"assistant\"") || line.contains("\"type\":\"user\""))) {
        return None;
    }
    let l: Line = serde_json::from_str(line).ok()?;
    let sid = l.session_id?;
    let kind = match l.kind.as_deref()? {
        k @ ("custom-title" | "bridge-session") => {
            let mut ev = AgentEvent::new(Agent::Claude, &sid, "", EventKind::Info);
            ev.title = l.custom_title;
            // a bridge-session record means the session is attached to Remote Control
            ev.remote = k == "bridge-session";
            ev.live = false;
            return Some(ev);
        }
        _ if info_only => return None,
        _ if l.is_sidechain == Some(true) => return None, // subagent chatter
        "assistant" => {
            let m = l.message?;
            let tool = m.content.as_ref().and_then(|c| c.as_array()).and_then(|a| a.iter().rev().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")));
            match (tool, m.stop_reason.as_deref()) {
                (Some(t), _) => {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    match name {
                        "AskUserQuestion" => EventKind::NeedsInput { reason: "Has a question for you".into() },
                        "ExitPlanMode" => EventKind::NeedsInput { reason: "Plan ready for review".into() },
                        _ => EventKind::Working { step: tool_step(name, t.get("input").unwrap_or(&Value::Null)) },
                    }
                }
                (None, Some("end_turn")) | (None, Some("stop_sequence")) => EventKind::Done,
                (None, Some("refusal")) | (None, Some("max_tokens")) => EventKind::Error { message: clip(m.stop_reason.as_deref().unwrap_or(""), 40) },
                _ => EventKind::Working { step: String::new() },
            }
        }
        "user" => {
            let m = l.message?;
            let is_prompt = match &m.content {
                Some(Value::String(_)) => true,
                Some(Value::Array(a)) => !a.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
                _ => false,
            };
            EventKind::Working { step: if is_prompt { "Thinking…".into() } else { String::new() } }
        }
        _ => return None,
    };
    let mut dir = l.cwd.unwrap_or_default();
    if !wsl.is_empty() && dir.starts_with('/') {
        dir = format!("\\\\wsl$\\{wsl}{}", dir.replace('/', "\\"));
    }
    let mut ev = AgentEvent::new(Agent::Claude, &sid, &dir, kind);
    ev.live = false;
    if let Some(t) = l.timestamp.as_deref().and_then(parse_rfc3339) {
        ev.at = t;
    }
    Some(ev)
}

fn head_has_bridge(path: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut head = Vec::with_capacity(32 * 1024);
    let _ = f.take(32 * 1024).read_to_end(&mut head);
    let needle = b"\"type\":\"bridge-session\"";
    head.windows(needle.len()).any(|w| w == needle)
}

/// Only main session files: projects/<project>/<session>.jsonl (subagent files sit deeper).
fn is_session_file(root: &Path, p: &Path) -> bool {
    p.extension().map(|e| e == "jsonl").unwrap_or(false) && p.parent().and_then(|d| d.parent()) == Some(root)
}

fn session_id_of(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn read_file(tailer: &mut Tailer, path: &Path, wsl: &str, out: &mut Vec<AgentEvent>) {
    // sessions with live hooks only need titles and the Remote Control marker from the file
    let sid = session_id_of(path);
    let info_only = is_live(&sid);
    let first_sight = !tailer.knows(path);
    for line in tailer.read_new(path, FIRST_TAIL, MAX_LINE) {
        if let Some(ev) = parse_line(&line, wsl, info_only) {
            out.push(ev);
        }
    }
    // The Remote Control marker is written near the start of the file, which the tail
    // read above may not cover: look at the head once.
    if first_sight && head_has_bridge(path) {
        let mut ev = AgentEvent::new(Agent::Claude, &sid, "", EventKind::Info);
        ev.remote = true;
        ev.live = false;
        out.push(ev);
    }
}

/// Read the tail of every recently written transcript once, to recover running sessions.
fn backfill(root: &Path, wsl: &str, max_age_secs: u64, tailer: &mut Tailer) {
    let mut files = Vec::new();
    recent_files(root, 1, max_age_secs, &|p| is_session_file(root, p), &mut files);
    let mut evs = Vec::new();
    let now = now_unix();
    for f in &files {
        let mut one = Vec::new();
        read_file(tailer, f, wsl, &mut one);
        // Recover only sessions that are running right now: the last state line must say
        // "working" (or waiting) and be recent. Finished or abandoned turns are history.
        let running = one.iter().rev().find(|e| e.kind != EventKind::Info).map(|last| matches!(last.kind, EventKind::Working { .. } | EventKind::NeedsInput { .. }) && now.saturating_sub(last.at) <= STALE_SECS).unwrap_or(false);
        if running {
            evs.extend(one);
        }
    }
    crate::logln!("transcripts: backfilled {} file(s), {} event(s) from {}", files.len(), evs.len(), root.display());
    if !evs.is_empty() {
        post(UiMsg::Events(evs));
    }
}

pub fn projects_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("projects"),
        _ => crate::util::home().join(".claude").join("projects"),
    }
}

pub fn start(cfg: &Config) {
    let keep = STALE_SECS + 60;
    let wsl_enabled = cfg.wsl.enabled;
    let _ = std::thread::Builder::new().name("transcripts".into()).stack_size(512 * 1024).spawn(move || {
        let root = projects_dir();
        let mut tailer = Tailer::default();
        if root.is_dir() {
            backfill(&root, "", keep, &mut tailer);
        }
        if wsl_enabled {
            // WSL file systems do not deliver change notifications, so they are read once
            // at startup; live updates from WSL come through hooks.
            for (distro, dir) in crate::wsl::claude_dirs() {
                let mut t = Tailer::default();
                backfill(&dir.join("projects"), &distro, keep, &mut t);
            }
        }
        if !root.is_dir() {
            return;
        }
        let r2 = root.clone();
        watch_dir(&root, true, 120, move |changed| {
            let mut evs = Vec::new();
            for p in changed {
                if is_session_file(&r2, &p) {
                    read_file(&mut tailer, &p, "", &mut evs);
                }
            }
            if !evs.is_empty() {
                post(UiMsg::Events(evs));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transcript_lines() {
        let tool = r#"{"type":"assistant","sessionId":"s","cwd":"C:\\p","timestamp":"2026-11-14T10:00:00.000Z","isSidechain":false,"message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        let e = parse_line(tool, "", false).unwrap();
        assert_eq!(e.kind, EventKind::Working { step: "Bash: ls".into() });
        assert!(!e.live);
        assert_eq!(e.at, 1_794_650_400);
        let end = r#"{"type":"assistant","sessionId":"s","cwd":"C:\\p","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#;
        assert_eq!(parse_line(end, "", false).unwrap().kind, EventKind::Done);
        assert!(parse_line(end, "", true).is_none());
        let prompt = r#"{"type":"user","sessionId":"s","cwd":"C:\\p","message":{"role":"user","content":"do it"}}"#;
        assert_eq!(parse_line(prompt, "", false).unwrap().kind, EventKind::Working { step: "Thinking…".into() });
        let side = r#"{"type":"assistant","sessionId":"s","isSidechain":true,"message":{"stop_reason":"end_turn","content":[]}}"#;
        assert!(parse_line(side, "", false).is_none());
        let title = r#"{"type":"custom-title","customTitle":"Build notch","sessionId":"s"}"#;
        assert_eq!(parse_line(title, "", true).unwrap().title.as_deref(), Some("Build notch"));
        assert!(parse_line(r#"{"type":"bridge-session","bridgeSessionId":"b","sessionId":"s"}"#, "", true).unwrap().remote);
        assert!(parse_line(r#"{"type":"api-request","sessionId":"s"}"#, "", false).is_none());
    }
}
