//! Agent adapters. Each adapter turns one agent's signals into normalized `AgentEvent`s.
//!
//! To add an agent: create a module with a type implementing `AgentAdapter`, add it to
//! `REGISTRY`, and (if it pushes events) have its hook call `notch.exe --hook --agent <id>`.
use serde_json::Value;

use crate::config::Config;
use crate::model::{Agent, AgentEvent};

pub mod claude;
pub mod claude_hooks;
pub mod codex;
pub mod registry;
pub mod stubs;
pub mod t3code;
pub mod transcript;

pub trait AgentAdapter: Sync {
    /// Short id used on the wire (`--agent <id>`) and in logs.
    fn id(&self) -> &'static str;
    fn agent(&self) -> Agent;
    /// Is this adapter switched on in the config?
    fn enabled(&self, cfg: &Config) -> bool;
    /// Start background sources (file watchers, backfill). Called once at startup, on the
    /// UI thread: spawn a thread for anything that blocks and report with `app::post`.
    fn start(&self, _cfg: &Config) {}
    /// Translate one pushed payload (hook JSON, notify JSON) into events.
    /// `wsl` is the distro name when the payload came from inside WSL.
    fn on_push(&self, _payload: &Value, _pids: &[u32], _wsl: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
}

pub static REGISTRY: [&dyn AgentAdapter; 6] = [&claude::Claude, &codex::Codex, &t3code::T3Code, &stubs::GeminiCli, &stubs::CursorAgent, &stubs::OpenCode];

pub fn on_push(agent_id: &str, payload: &Value, pids: &[u32], wsl: &str) -> Vec<AgentEvent> {
    REGISTRY.iter().find(|a| a.id() == agent_id).map(|a| a.on_push(payload, pids, wsl)).unwrap_or_default()
}

pub fn start_all(cfg: &Config) {
    for a in REGISTRY.iter() {
        if a.enabled(cfg) {
            a.start(cfg);
        }
    }
}

/// Is the adapter that produces events for `agent` enabled?
pub fn agent_enabled(cfg: &Config, agent: Agent) -> bool {
    REGISTRY.iter().find(|a| a.agent() == agent).map(|a| a.enabled(cfg)).unwrap_or(true)
}

pub(crate) fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}
