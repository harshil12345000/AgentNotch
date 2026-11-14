//! Codex adapter (CLI and desktop app).
//!
//! Live signals, best first:
//!  1. Codex hooks (~/.codex/hooks.json, same shape as Claude Code's). Installed with
//!     `notch.exe --install-hooks --codex`; Codex then asks you to review and trust them
//!     (`/hooks`) before they run. This is the only way to see approval requests.
//!  2. Codex `notify` (legacy, turn-complete only): `notch.exe --codex-notify <json>`.
//!  3. Rollout files under ~/.codex/sessions (tailed). Approvals are not written there,
//!     and newer Codex builds may keep sessions in a database instead, in which case
//!     this source sees nothing.
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::{s, AgentAdapter};
use crate::app::{post, UiMsg};
use crate::config::Config;
use crate::model::{Agent, AgentEvent, EventKind};
use crate::util::{clip, parse_rfc3339};
use crate::watch::{watch_dir, Tailer};

pub struct Codex;

pub const HOOK_EVENTS: [&str; 7] = ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"];

pub fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => crate::util::home().join(".codex"),
    }
}

fn tool_step(name: &str, input: &Value) -> String {
    let cmd = input.get("command").map(|c| match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    });
    match cmd {
        Some(c) if !c.is_empty() => clip(&format!("{name}: {c}"), 70),
        _ => clip(name, 70),
    }
}

impl AgentAdapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn agent(&self) -> Agent {
        Agent::Codex
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.adapters.codex
    }

    fn on_push(&self, p: &Value, pids: &[u32], _wsl: &str) -> Vec<AgentEvent> {
        // legacy notify: {"type":"agent-turn-complete","thread-id":"..","cwd":".."}
        if s(p, "type") == "agent-turn-complete" {
            let id = s(p, "thread-id");
            if id.is_empty() {
                return Vec::new();
            }
            return vec![AgentEvent::new(Agent::Codex, id, s(p, "cwd"), EventKind::Done)];
        }
        let sid = s(p, "session_id");
        if sid.is_empty() {
            return Vec::new();
        }
        let tool = s(p, "tool_name");
        let input = p.get("tool_input").unwrap_or(&Value::Null);
        let kind = match s(p, "hook_event_name") {
            "SessionStart" => EventKind::SessionStarted,
            "UserPromptSubmit" => EventKind::Working { step: "Thinking…".into() },
            "PreToolUse" => EventKind::Working { step: tool_step(tool, input) },
            "PostToolUse" => EventKind::Working { step: String::new() },
            "PermissionRequest" => EventKind::NeedsInput { reason: format!("Approval: {}", tool_step(tool, input)) },
            "Stop" => EventKind::Done,
            "SessionEnd" => EventKind::SessionEnded,
            _ => return Vec::new(),
        };
        let mut ev = AgentEvent::new(Agent::Codex, sid, s(p, "cwd"), kind);
        ev.pids = pids.to_vec();
        vec![ev]
    }

    fn start(&self, _cfg: &Config) {
        let root = codex_home().join("sessions");
        if !root.is_dir() {
            return;
        }
        let _ = std::thread::Builder::new().name("codex".into()).stack_size(256 * 1024).spawn(move || {
            let mut tailer = Tailer::default();
            watch_dir(&root, true, 150, move |changed| {
                let mut evs = Vec::new();
                for p in changed {
                    let Some(id) = rollout_id(&p) else { continue };
                    let lines = tailer.read_new(&p, 64 * 1024, 256 * 1024);
                    if lines.is_empty() {
                        continue;
                    }
                    let cwd = head_cwd(&p).unwrap_or_default();
                    evs.extend(lines.iter().filter_map(|l| parse_rollout_line(l, &id, &cwd)));
                }
                if !evs.is_empty() {
                    post(UiMsg::Events(evs));
                }
            });
        });
    }
}

/// rollout-2026-11-02T10-00-00-<uuid>.jsonl -> the uuid
fn rollout_id(p: &Path) -> Option<String> {
    let stem = p.file_stem()?.to_str()?;
    if !stem.starts_with("rollout-") || p.extension()? != "jsonl" || stem.len() < 44 {
        return None;
    }
    Some(stem[stem.len() - 36..].to_string())
}

/// The session's folder, from the session_meta record at the start of the file.
fn head_cwd(p: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(p).ok()?.take(4096).read_to_end(&mut head).ok()?;
    let text = String::from_utf8_lossy(&head);
    let start = text.find("\"cwd\":\"")? + 7;
    let rest = &text[start..];
    let mut end = 0;
    let b = rest.as_bytes();
    while end < b.len() && !(b[end] == b'"' && (end == 0 || b[end - 1] != b'\\')) {
        end += 1;
    }
    serde_json::from_str::<String>(&format!("\"{}\"", &rest[..end])).ok()
}

/// One rollout line: {"timestamp":"..","type":"event_msg"|"response_item",..,"payload":{..}}
pub fn parse_rollout_line(line: &str, id: &str, cwd: &str) -> Option<AgentEvent> {
    if !(line.contains("\"event_msg\"") || line.contains("\"function_call\"")) {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let p = v.get("payload")?;
    let kind = match (s(&v, "type"), s(p, "type")) {
        ("event_msg", "task_started") | ("event_msg", "turn_started") => EventKind::Working { step: "Thinking…".into() },
        ("event_msg", "task_complete") | ("event_msg", "turn_complete") | ("event_msg", "turn_aborted") => EventKind::Done,
        ("response_item", "function_call") => EventKind::Working { step: clip(s(p, "name"), 60) },
        _ => return None,
    };
    let mut ev = AgentEvent::new(Agent::Codex, id, cwd, kind);
    ev.live = false; // hooks win when both are present
    if let Some(t) = parse_rfc3339(s(&v, "timestamp")) {
        ev.at = t;
    }
    Some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hooks_and_notify() {
        let e = Codex.on_push(&json!({"session_id":"c1","cwd":"C:\\p","hook_event_name":"PermissionRequest","tool_name":"shell","tool_input":{"command":["git","push"]}}), &[], "");
        assert_eq!(e[0].kind, EventKind::NeedsInput { reason: "Approval: shell: git push".into() });
        let e = Codex.on_push(&json!({"type":"agent-turn-complete","thread-id":"t9","cwd":"C:\\p","last-assistant-message":null}), &[], "");
        assert_eq!((e[0].session_id.as_str(), &e[0].kind), ("t9", &EventKind::Done));
        assert!(Codex.on_push(&json!({"type":"something-else"}), &[], "").is_empty());
    }

    #[test]
    fn rollouts() {
        let id = "0199a213-81c0-7800-8aa1-bbab2a035a53";
        assert_eq!(rollout_id(Path::new("C:/x/rollout-2026-11-02T10-00-00-0199a213-81c0-7800-8aa1-bbab2a035a53.jsonl")).as_deref(), Some(id));
        assert_eq!(rollout_id(Path::new("C:/x/notes.jsonl")), None);
        let e = parse_rollout_line(r#"{"timestamp":"2026-11-14T10:00:00Z","type":"event_msg","payload":{"type":"task_complete"}}"#, id, "C:\\p").unwrap();
        assert_eq!(e.kind, EventKind::Done);
        assert!(!e.live);
        let e = parse_rollout_line(r#"{"timestamp":"2026-11-14T10:00:00Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}"#, id, "").unwrap();
        assert_eq!(e.kind, EventKind::Working { step: "shell".into() });
        assert!(parse_rollout_line(r#"{"type":"event_msg","payload":{"type":"token_count"}}"#, id, "").is_none());
    }
}
