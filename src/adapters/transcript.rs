//! Claude Code transcript watcher: fallback and backfill for sessions that have no hooks
//! (or that started before AgentNotch did).
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
use crate::model::{Agent, AgentEvent, EventKind, SubEvent};
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
    subtype: Option<String>,
    message: Option<Msg>,
}
#[derive(Deserialize)]
struct Msg {
    content: Option<Value>,
    stop_reason: Option<String>,
}

/// How much of a transcript drives a session's state.
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// No hooks for this session: every state line counts.
    Full,
    /// Hooks are live: the transcript only supplies titles, the Remote Control marker and
    /// the end of a turn (a second witness, in case the Stop hook is missed).
    Assist,
}

fn interrupted(content: &Option<Value>) -> bool {
    let starts = |t: &str| t.trim_start().starts_with("[Request interrupted");
    match content {
        Some(Value::String(t)) => starts(t),
        Some(Value::Array(a)) => a.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("text") && b.get("text").and_then(|t| t.as_str()).map(starts).unwrap_or(false)),
        _ => false,
    }
}

/// Turn one transcript line into an event. `wsl` is the distro when the file is inside WSL.
pub fn parse_line(line: &str, wsl: &str, mode: Mode) -> Option<AgentEvent> {
    // cheap pre-filter: most lines (request records, attachments, snapshots) are not state
    let info = line.contains("\"type\":\"custom-title\"") || line.contains("\"type\":\"bridge-session\"");
    let state = line.contains("\"type\":\"assistant\"") || line.contains("\"type\":\"user\"") || line.contains("\"stop_hook_summary\"");
    if !info && !state {
        return None;
    }
    let l: Line = serde_json::from_str(line).ok()?;
    let sid = l.session_id?;
    let mut turn_end = false;
    let kind = match l.kind.as_deref()? {
        k @ ("custom-title" | "bridge-session") => {
            let mut ev = AgentEvent::new(Agent::Claude, &sid, "", EventKind::Info);
            ev.title = l.custom_title;
            // a bridge-session record means the session is attached to Remote Control
            ev.remote = k == "bridge-session";
            ev.live = false;
            return Some(ev);
        }
        _ if l.is_sidechain == Some(true) => return None, // subagent chatter
        // written when the Stop hooks have run: the turn is over
        "system" if l.subtype.as_deref() == Some("stop_hook_summary") => {
            turn_end = true;
            super::claude::turn_end(&sid)
        }
        "assistant" => {
            let m = l.message?;
            let tool = m.content.as_ref().and_then(|c| c.as_array()).and_then(|a| a.iter().rev().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")));
            match (tool, m.stop_reason.as_deref()) {
                (Some(_), _) if mode == Mode::Assist => return None,
                (Some(t), _) => {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let input = t.get("input").unwrap_or(&Value::Null);
                    super::claude::note_tool(&sid, name, input);
                    match name {
                        "AskUserQuestion" => EventKind::NeedsInput { reason: "Has a question for you".into() },
                        "ExitPlanMode" => EventKind::NeedsInput { reason: "Plan ready for review".into() },
                        _ => EventKind::Working { step: tool_step(name, input) },
                    }
                }
                (None, Some("end_turn")) | (None, Some("stop_sequence")) => {
                    turn_end = true;
                    super::claude::turn_end(&sid)
                }
                _ if mode == Mode::Assist => return None,
                (None, Some("refusal")) | (None, Some("max_tokens")) => EventKind::Error { message: clip(m.stop_reason.as_deref().unwrap_or(""), 40) },
                _ => EventKind::Working { step: String::new() },
            }
        }
        "user" => {
            let m = l.message?;
            if interrupted(&m.content) {
                // Esc: the turn stopped without a normal ending
                turn_end = true;
                EventKind::Done
            } else if mode == Mode::Assist {
                return None;
            } else {
                let is_prompt = match &m.content {
                    Some(Value::String(_)) => true,
                    Some(Value::Array(a)) => !a.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
                    _ => false,
                };
                if is_prompt {
                    super::claude::clear_ask(&sid);
                }
                EventKind::Working { step: if is_prompt { "Thinking…".into() } else { String::new() } }
            }
        }
        _ => return None,
    };
    let mut dir = l.cwd.unwrap_or_default();
    if !wsl.is_empty() && dir.starts_with('/') {
        dir = format!("\\\\wsl$\\{wsl}{}", dir.replace('/', "\\"));
    }
    let mut ev = AgentEvent::new(Agent::Claude, &sid, &dir, kind);
    ev.live = false;
    ev.turn_end = turn_end;
    if let Some(t) = l.timestamp.as_deref().and_then(parse_rfc3339) {
        ev.at = t;
    }
    Some(ev)
}

/// projects/<project>/<session>/subagents/agent-<id>.jsonl (or .meta.json)
/// -> (session id, agent id, is the meta file)
fn sub_file(root: &Path, p: &Path) -> Option<(String, String, bool)> {
    let rest = p.file_name()?.to_str()?.strip_prefix("agent-")?;
    let (id, meta) = match rest.strip_suffix(".meta.json") {
        Some(id) => (id, true),
        None => (rest.strip_suffix(".jsonl")?, false),
    };
    let dir = p.parent()?;
    if dir.file_name()? != "subagents" {
        return None;
    }
    let session = dir.parent()?;
    (session.parent()?.parent()? == root).then(|| (session.file_name().unwrap_or_default().to_string_lossy().to_string(), id.to_string(), meta))
}

#[derive(Deserialize)]
struct SubMeta {
    #[serde(rename = "agentType")]
    agent_type: Option<String>,
    description: Option<String>,
    finished: Option<bool>,
}

/// News about one subagent from its files: the meta file says what it is and whether it
/// has finished; the newest tool call in its transcript says what it is doing.
fn read_sub(tailer: &mut Tailer, root: &Path, p: &Path, out: &mut Vec<AgentEvent>) {
    let Some((sid, id, is_meta)) = sub_file(root, p) else { return };
    let meta: Option<SubMeta> = p.parent().and_then(|d| std::fs::read(d.join(format!("agent-{id}.meta.json"))).ok()).and_then(|b| serde_json::from_slice(&b).ok());
    let mut step = None;
    if !is_meta {
        for line in tailer.read_new(p, 48 * 1024, MAX_LINE) {
            if !line.contains("\"tool_use\"") {
                continue;
            }
            let Ok(l) = serde_json::from_str::<Line>(&line) else { continue };
            let tool = l.message.as_ref().and_then(|m| m.content.as_ref()).and_then(|c| c.as_array()).and_then(|a| a.iter().rev().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")).cloned());
            if let Some(t) = tool {
                step = Some(tool_step(t.get("name").and_then(|n| n.as_str()).unwrap_or(""), t.get("input").unwrap_or(&Value::Null)));
            }
        }
    }
    if meta.is_none() && step.is_none() {
        return;
    }
    let (kind, ended) = match &meta {
        Some(m) => {
            let label = match (m.description.as_deref().unwrap_or(""), m.agent_type.as_deref().unwrap_or("")) {
                ("", t) => t.to_string(),
                (d, "") => d.to_string(),
                (d, t) => format!("{d} ({t})"),
            };
            (clip(&label, 60), m.finished == Some(true))
        }
        None => (String::new(), false),
    };
    let mut ev = AgentEvent::new(Agent::Claude, &sid, "", EventKind::Info);
    ev.live = false;
    ev.sub = Some(SubEvent { id, kind, step, ended });
    out.push(ev);
}

/// Subagents that are running right now: meta files touched in the last half hour that
/// are not marked finished.
fn backfill_subs(root: &Path, tailer: &mut Tailer) {
    let mut metas = Vec::new();
    recent_files(root, 3, 1800, &|p| sub_file(root, p).map(|s| s.2).unwrap_or(false), &mut metas);
    let mut evs = Vec::new();
    for m in metas {
        let transcript = PathBuf::from(m.to_string_lossy().replace(".meta.json", ".jsonl"));
        let mut one = Vec::new();
        read_sub(tailer, root, &transcript, &mut one);
        if one.is_empty() {
            read_sub(tailer, root, &m, &mut one);
        }
        evs.extend(one.into_iter().filter(|e| e.sub.as_ref().map(|s| !s.ended).unwrap_or(false)));
    }
    if !evs.is_empty() {
        post(UiMsg::Events(evs));
    }
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
    let mode = if is_live(&sid) { Mode::Assist } else { Mode::Full };
    let first_sight = !tailer.knows(path);
    for line in tailer.read_new(path, FIRST_TAIL, MAX_LINE) {
        if let Some(ev) = parse_line(&line, wsl, mode) {
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
            backfill_subs(&root, &mut tailer);
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
                } else {
                    read_sub(&mut tailer, &r2, &p, &mut evs);
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
        let e = parse_line(tool, "", Mode::Full).unwrap();
        assert_eq!(e.kind, EventKind::Working { step: "Bash: ls".into() });
        assert!(!e.live && !e.turn_end);
        assert_eq!(e.at, 1_794_650_400);
        assert!(parse_line(tool, "", Mode::Assist).is_none());
        let end = r#"{"type":"assistant","sessionId":"s","cwd":"C:\\p","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#;
        for mode in [Mode::Full, Mode::Assist] {
            let e = parse_line(end, "", mode).unwrap();
            assert_eq!(e.kind, EventKind::Done);
            assert!(e.turn_end);
        }
        let hook = r#"{"type":"system","subtype":"stop_hook_summary","sessionId":"s","cwd":"C:\\p","stopReason":""}"#;
        assert!(parse_line(hook, "", Mode::Assist).unwrap().turn_end);
        let esc = r#"{"type":"user","sessionId":"s","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        assert_eq!(parse_line(esc, "", Mode::Assist).unwrap().kind, EventKind::Done);
        let prompt = r#"{"type":"user","sessionId":"s","cwd":"C:\\p","message":{"role":"user","content":"do it"}}"#;
        assert_eq!(parse_line(prompt, "", Mode::Full).unwrap().kind, EventKind::Working { step: "Thinking…".into() });
        assert!(parse_line(prompt, "", Mode::Assist).is_none());
        let side = r#"{"type":"assistant","sessionId":"s","isSidechain":true,"message":{"stop_reason":"end_turn","content":[]}}"#;
        assert!(parse_line(side, "", Mode::Full).is_none());
        let title = r#"{"type":"custom-title","customTitle":"Build notch","sessionId":"s"}"#;
        assert_eq!(parse_line(title, "", Mode::Assist).unwrap().title.as_deref(), Some("Build notch"));
        assert!(parse_line(r#"{"type":"bridge-session","bridgeSessionId":"b","sessionId":"s"}"#, "", Mode::Assist).unwrap().remote);
        assert!(parse_line(r#"{"type":"api-request","sessionId":"s"}"#, "", Mode::Full).is_none());
    }

    #[test]
    fn subagent_files() {
        let root = Path::new("C:/u/.claude/projects");
        let p = root.join("proj").join("sess-1").join("subagents").join("agent-a1b2.jsonl");
        assert_eq!(sub_file(root, &p), Some(("sess-1".into(), "a1b2".into(), false)));
        let m = root.join("proj").join("sess-1").join("subagents").join("agent-a1b2.meta.json");
        assert_eq!(sub_file(root, &m), Some(("sess-1".into(), "a1b2".into(), true)));
        assert_eq!(sub_file(root, &root.join("proj").join("sess-1.jsonl")), None);
        assert_eq!(sub_file(root, &root.join("proj").join("sess-1").join("tool-results").join("agent-x.jsonl")), None);
    }

    #[test]
    fn a_question_at_the_end_of_a_turn_is_needs_input() {
        let wrap = r#"{"type":"assistant","sessionId":"ask1","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","name":"mcp__ccd_turn__wrap_up","input":{"recap":"r","ask":"Ship it?"}}]}}"#;
        parse_line(wrap, "", Mode::Full);
        let end = r#"{"type":"assistant","sessionId":"ask1","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"x"}]}}"#;
        assert_eq!(parse_line(end, "", Mode::Full).unwrap().kind, EventKind::NeedsInput { reason: "Ship it?".into() });
        // the next prompt answers it
        parse_line(r#"{"type":"user","sessionId":"ask1","message":{"role":"user","content":"yes"}}"#, "", Mode::Full);
        assert_eq!(parse_line(end, "", Mode::Full).unwrap().kind, EventKind::Done);
    }
}
