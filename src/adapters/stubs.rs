//! Placeholders for agents that are not implemented yet. They register an id so the wire
//! format and settings are stable, and produce no events.
use super::AgentAdapter;
use crate::config::Config;
use crate::model::Agent;

macro_rules! stub {
    ($name:ident, $id:literal, $agent:expr) => {
        pub struct $name;
        impl AgentAdapter for $name {
            fn id(&self) -> &'static str {
                $id
            }
            fn agent(&self) -> Agent {
                $agent
            }
            fn enabled(&self, _cfg: &Config) -> bool {
                false
            }
        }
    };
}

stub!(GeminiCli, "gemini", Agent::Gemini);
stub!(CursorAgent, "cursor", Agent::Cursor);
stub!(OpenCode, "opencode", Agent::OpenCode);
