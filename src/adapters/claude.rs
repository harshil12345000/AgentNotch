//! Claude Code adapter: hook payloads (primary) and transcript backfill (see transcript.rs).
//!
//! Hook names and fields follow https://code.claude.com/docs/en/hooks (checked 2026-11-14).
use serde_json::Value;

use super::{s, AgentAdapter};
use crate::config::Config;
use crate::model::{Agent, AgentEvent, EventKind, SubEvent};
use crate::util::{base_name, clip};

pub struct Claude;

/// The hook events AgentNotch registers for.
pub const HOOK_EVENTS: [&str; 13] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "SessionEnd",
];

/// A question the agent left for the user when it wrapped up its turn (session id -> text).
/// A turn that ends with one is "needs input", not just "done".
static PENDING_ASK: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> = std::sync::Mutex::new(None);

/// Call for every tool use of a session: remembers a wrap-up question, if this is one.
pub fn note_tool(session_id: &str, tool: &str, input: &Value) {
    if !tool.ends_with("wrap_up") {
        return;
    }
    let ask = input.get("ask").and_then(|a| a.as_str()).unwrap_or("").trim();
    if let Ok(mut g) = PENDING_ASK.lock() {
        let map = g.get_or_insert_with(Default::default);
        if ask.is_empty() {
            map.remove(session_id);
        } else {
            map.insert(session_id.to_string(), clip(ask, 80));
        }
    }
}

/// A new prompt arrived: any earlier question is answered.
pub fn clear_ask(session_id: &str) {
    if let Ok(mut g) = PENDING_ASK.lock() {
        if let Some(map) = g.as_mut() {
            map.remove(session_id);
        }
    }
}

/// What the end of a turn means for this session: waiting on a question, or finished.
pub fn turn_end(session_id: &str) -> EventKind {
    let ask = PENDING_ASK.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(session_id).cloned()));
    match ask {
        Some(q) => EventKind::NeedsInput { reason: q },
        None => EventKind::Done,
    }
}

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
        crate::adapters::registry::start();
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
        // Hooks from a subagent or teammate carry agent_id. Their tool calls must not change
        // the main session's state: a background agent can still be working after the main
        // turn has ended and is waiting for the user.
        let from_subagent = !s(p, "agent_id").is_empty();
        let event = s(p, "hook_event_name");
        if from_subagent || matches!(event, "SubagentStart" | "SubagentStop") {
            let news = |step: Option<String>, ended: bool| SubEvent { id: s(p, "agent_id").to_string(), kind: s(p, "agent_type").to_string(), step, ended };
            let sub = match event {
                "SubagentStart" => Some(news(None, false)),
                "SubagentStop" => Some(news(None, true)),
                "PreToolUse" => Some(news(Some(tool_step(tool, input)), false)),
                "PostToolUse" | "PostToolUseFailure" | "Stop" | "PreCompact" | "UserPromptSubmit" => return Vec::new(),
                // a permission prompt or notification from a subagent still needs the user
                _ => None,
            };
            if let Some(sub) = sub {
                if sub.id.is_empty() {
                    return Vec::new();
                }
                let mut ev = AgentEvent::new(Agent::Claude, sid, "", EventKind::Info);
                ev.sub = Some(sub);
                return vec![ev];
            }
        }
        let kind = match event {
            "SessionStart" => EventKind::SessionStarted,
            "UserPromptSubmit" => {
                clear_ask(sid);
                EventKind::Working { step: "Thinking…".into() }
            }
            "PreToolUse" if tool.ends_with("wrap_up") => {
                note_tool(sid, tool, input);
                EventKind::Working { step: "Wrapping up".into() }
            }
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
            "Stop" => turn_end(sid),
            "StopFailure" => EventKind::Error { message: clip(if s(p, "error").is_empty() { s(p, "last_assistant_message") } else { s(p, "error") }, 80) },
            // a subagent finishing says nothing about the main session's state
            "SubagentStop" => return Vec::new(),
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
    fn subagent_tools_do_not_touch_the_main_session() {
        let e = push(json!({"session_id":"m1","agent_id":"sub-7","agent_type":"general-purpose","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}));
        assert_eq!(e[0].kind, EventKind::Info); // no state change for the main session
        assert_eq!(e[0].sub, Some(SubEvent { id: "sub-7".into(), kind: "general-purpose".into(), step: Some("Bash: ls".into()), ended: false }));
        assert!(push(json!({"session_id":"m1","agent_id":"sub-7","hook_event_name":"PostToolUse","tool_name":"Bash"})).is_empty());
        let e = push(json!({"session_id":"m1","hook_event_name":"SubagentStop","agent_id":"sub-7"}));
        assert!(e[0].sub.as_ref().unwrap().ended);
        let e = push(json!({"session_id":"m1","hook_event_name":"SubagentStart","agent_id":"sub-8","agent_type":"Explore"}));
        assert_eq!(e[0].sub.as_ref().unwrap().kind, "Explore");
        // but a subagent that needs permission still needs the user
        let e = push(json!({"session_id":"m1","agent_id":"sub-7","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"git push"}}));
        assert!(matches!(e[0].kind, EventKind::NeedsInput { .. }));
    }

    #[test]
    fn a_turn_that_ends_with_a_question_needs_input() {
        let wrap = |ask: &str| json!({"session_id":"q1","hook_event_name":"PreToolUse","tool_name":"mcp__ccd_turn__wrap_up","tool_input":{"recap":"done","ask":ask}});
        push(wrap("Should I add the line now?"));
        let e = push(json!({"session_id":"q1","hook_event_name":"Stop"}));
        assert_eq!(e[0].kind, EventKind::NeedsInput { reason: "Should I add the line now?".into() });
        // answering clears it; a wrap-up without a question is a plain finish
        push(json!({"session_id":"q1","hook_event_name":"UserPromptSubmit","prompt":"yes"}));
        push(wrap(""));
        assert_eq!(push(json!({"session_id":"q1","hook_event_name":"Stop"}))[0].kind, EventKind::Done);
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
