//! Claude Code adapter: hook payloads (primary) and transcript backfill (see transcript.rs).
//!
//! Hook names and fields follow https://code.claude.com/docs/en/hooks (checked 2026-11-14).
use serde_json::Value;

use super::{s, AgentAdapter};
use crate::config::Config;
use crate::model::{Agent, AgentEvent, EventKind};
use crate::util::{base_name, clip};

pub struct Claude;

/// The hook events Notch registers for.
pub const HOOK_EVENTS: [&str; 12] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStop",
    "PreCompact",
    "SessionEnd",
];

/// Short human description of a tool call, e.g. "Bash: npm test" or "Edit main.rs".
pub fn tool_step(name: &str, input: &Value) -> String {
    let arg = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let out = match name {
        "Bash" | "PowerShell" => format!("{name}: {}", arg("command")),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let p = if arg("file_path").is_empty() { arg("notebook_path") } else { arg("file_path") };
            format!("{name} {}", base_name(p))
        }
        "Grep" | "Glob" => format!("{name} {}", arg("pattern")),
        "Task" | "Agent" => format!("Agent: {}", arg("description")),
        "WebFetch" => format!("Fetch {}", arg("url")),
        "WebSearch" => format!("Search: {}", arg("query")),
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => "Updating task list".to_string(),
        n if n.starts_with("mcp__") => {
            let mut parts = n.splitn(3, "__").skip(1);
            let (server, tool) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            format!("{tool} ({server})")
        }
        n => n.to_string(),
    };
    clip(out.trim(), 70)
}

impl AgentAdapter for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn agent(&self) -> Agent {
        Agent::Claude
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.adapters.claude
    }

    fn start(&self, cfg: &Config) {
        crate::adapters::transcript::start(cfg);
    }

    fn on_push(&self, p: &Value, pids: &[u32], wsl: &str) -> Vec<AgentEvent> {
        let sid = s(p, "session_id");
        if sid.is_empty() {
            return Vec::new();
        }
        let tool = s(p, "tool_name");
        let empty = Value::Null;
        let input = p.get("tool_input").unwrap_or(&empty);
        let mut soft = false;
        let kind = match s(p, "hook_event_name") {
            "SessionStart" => EventKind::SessionStarted,
            "UserPromptSubmit" => EventKind::Working { step: "Thinking…".into() },
            "PreToolUse" => match tool {
                // these tools stop and wait for the user
                "AskUserQuestion" => EventKind::NeedsInput { reason: "Has a question for you".into() },
                "ExitPlanMode" => EventKind::NeedsInput { reason: "Plan ready for review".into() },
                _ => EventKind::Working { step: tool_step(tool, input) },
            },
            "PostToolUse" => EventKind::Working { step: String::new() },
            "PostToolUseFailure" => EventKind::Working { step: format!("{tool} failed") },
            "PermissionRequest" => EventKind::NeedsInput { reason: format!("Permission: {}", tool_step(tool, input)) },
            "Notification" => match s(p, "notification_type") {
                "permission_prompt" | "elicitation_dialog" => EventKind::NeedsInput { reason: clip(s(p, "message"), 80) },
                "idle_prompt" => {
                    soft = true;
                    EventKind::NeedsInput { reason: "Waiting for your input".into() }
                }
                "" => {
                    // older builds send no type: classify by the message text
                    let m = s(p, "message");
                    if m.contains("permission") {
                        EventKind::NeedsInput { reason: clip(m, 80) }
                    } else if m.contains("waiting for your input") {
                        soft = true;
                        EventKind::NeedsInput { reason: "Waiting for your input".into() }
                    } else {
                        return Vec::new();
                    }
                }
                _ => return Vec::new(),
            },
            "Stop" => EventKind::Done,
            "StopFailure" => EventKind::Error { message: clip(if s(p, "error").is_empty() { s(p, "last_assistant_message") } else { s(p, "error") }, 80) },
            // a subagent finishing does not end the turn: the main agent carries on
            "SubagentStop" => EventKind::Working { step: "Subagent finished".into() },
            "PreCompact" => EventKind::Working { step: "Compacting context".into() },
            "SessionEnd" => EventKind::SessionEnded,
            _ => return Vec::new(),
        };
        let mut dir = s(p, "cwd").to_string();
        if !wsl.is_empty() && dir.starts_with('/') {
            dir = format!("\\\\wsl$\\{wsl}{}", dir.replace('/', "\\"));
        }
        let mut ev = AgentEvent::new(Agent::Claude, sid, &dir, kind);
        ev.soft = soft;
        ev.pids = pids.to_vec();
        let title = s(p, "session_title");
        if !title.is_empty() {
            ev.title = Some(title.to_string());
        }
        vec![ev]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn push(v: Value) -> Vec<AgentEvent> {
        Claude.on_push(&v, &[42], "")
    }

    #[test]
    fn maps_hook_events() {
        let e = push(json!({"session_id":"a","cwd":"C:\\p","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}));
        assert_eq!(e[0].kind, EventKind::Working { step: "Bash: cargo test".into() });
        assert_eq!(e[0].pids, vec![42]);
        let e = push(json!({"session_id":"a","cwd":"C:\\p","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Bash"}));
        assert!(matches!(e[0].kind, EventKind::NeedsInput { .. }));
        assert!(!e[0].soft);
        let e = push(json!({"session_id":"a","hook_event_name":"Notification","notification_type":"idle_prompt","message":"x"}));
        assert!(e[0].soft);
        let e = push(json!({"session_id":"a","hook_event_name":"Stop","stop_hook_active":false}));
        assert_eq!(e[0].kind, EventKind::Done);
        assert!(push(json!({"session_id":"a","hook_event_name":"Notification","notification_type":"auth_success"})).is_empty());
        assert!(push(json!({"hook_event_name":"Stop"})).is_empty());
    }

    #[test]
    fn tool_steps() {
        assert_eq!(tool_step("Edit", &json!({"file_path":"C:\\x\\src\\main.rs"})), "Edit main.rs");
        assert_eq!(tool_step("mcp__github__create_issue", &json!({})), "create_issue (github)");
    }

    #[test]
    fn wsl_paths() {
        let e = Claude.on_push(&json!({"session_id":"a","cwd":"/home/u/proj","hook_event_name":"Stop"}), &[], "Ubuntu");
        assert_eq!(e[0].project_dir, "\\\\wsl$\\Ubuntu\\home\\u\\proj");
    }
}
