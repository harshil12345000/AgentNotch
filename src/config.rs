//! Config stored as TOML in %APPDATA%\Notch\config.toml.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// "system" | "dark" | "light"
    pub theme: String,
    /// Global hotkey that hides / shows the notch, e.g. "Ctrl+\\". Empty = none.
    pub hotkey: String,
    /// Keep Awake was switched on: it is applied again at every start and is left in
    /// place when Notch quits, until it is switched off.
    pub keep_awake: bool,
    /// Show Approve / Deny on a session that is asking for permission.
    pub approve_from_notch: bool,
    /// Also do that for terminal sessions. Off by default: there the hook may hold back
    /// Claude Code's own prompt until it is answered in the notch.
    pub approve_in_terminal: bool,
    /// Blur what is behind the island (glass). Off = solid panel.
    pub glass: bool,
    pub hover_expand: bool,
    pub hide_when_idle: bool,
    pub hide_on_fullscreen: bool,
    /// How long finished sessions stay visible.
    pub keep_finished_minutes: u32,
    /// Set once the first-run "install hooks?" question has been asked.
    pub hooks_prompted: bool,
    pub adapters: Adapters,
    pub notify: Notify,
    pub remote_control: RemoteControl,
    pub usage: UsageCfg,
    pub wsl: Wsl,
    /// Pill position per monitor setup (key = monitor layout fingerprint).
    pub positions: BTreeMap<String, Pos>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Adapters {
    pub claude: bool,
    pub codex: bool,
    pub t3code: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Notify {
    pub toast_needs_input: bool,
    pub toast_done: bool,
    pub toast_error: bool,
    pub sound: bool,
    pub auto_expand: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct RemoteControl {
    /// "wt" | "powershell" | "wsl"
    pub terminal: String,
    /// Empty = last-used project folder.
    pub default_dir: String,
    pub wsl_distro: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct UsageCfg {
    pub enabled: bool,
    /// Optional token budgets for the local estimate (Source B). 0 = show tokens only.
    pub estimate_limit_5h_tokens: u64,
    pub estimate_limit_week_tokens: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Wsl {
    /// Scan running WSL distros for ~/.claude at startup.
    pub enabled: bool,
    /// Listen on 127.0.0.1 (random port, token protected) for WSL hooks.
    pub tcp_fallback: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Pos {
    /// Free-floating position (screen pixels). Used when `snap` is empty.
    pub x: i32,
    pub y: i32,
    /// Screen edge the notch is attached to: "top" | "bottom" | "left" | "right" | "" (free).
    pub snap: String,
    /// Device name of the monitor it is attached to.
    pub monitor: String,
    /// Where along that edge, 0.0 (left / top end) to 1.0 (right / bottom end).
    pub t: f32,
}

impl Default for Pos {
    fn default() -> Self {
        Pos { x: 0, y: 0, snap: "top".into(), monitor: String::new(), t: 0.5 }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "dark".into(),
            hotkey: "Ctrl+\\".into(),
            keep_awake: false,
            approve_from_notch: true,
            approve_in_terminal: false,
            glass: false,
            hover_expand: true,
            hide_when_idle: false,
            hide_on_fullscreen: true,
            keep_finished_minutes: 2,
            hooks_prompted: false,
            adapters: Adapters::default(),
            notify: Notify::default(),
            remote_control: RemoteControl::default(),
            usage: UsageCfg::default(),
            wsl: Wsl::default(),
            positions: BTreeMap::new(),
        }
    }
}
impl Default for Adapters {
    fn default() -> Self {
        Adapters { claude: true, codex: true, t3code: true }
    }
}
impl Default for Notify {
    fn default() -> Self {
        Notify { toast_needs_input: true, toast_done: true, toast_error: true, sound: true, auto_expand: true }
    }
}
impl Default for RemoteControl {
    fn default() -> Self {
        RemoteControl { terminal: "wt".into(), default_dir: String::new(), wsl_distro: String::new() }
    }
}
impl Default for UsageCfg {
    fn default() -> Self {
        UsageCfg { enabled: true, estimate_limit_5h_tokens: 0, estimate_limit_week_tokens: 0 }
    }
}

pub fn path() -> std::path::PathBuf {
    crate::util::config_dir().join("config.toml")
}

pub fn load() -> Config {
    match std::fs::read_to_string(path()) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            crate::logln!("config parse error, using defaults: {e}");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(c: &Config) {
    match toml::to_string_pretty(c) {
        Ok(s) => {
            let tmp = path().with_extension("toml.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, path());
            }
        }
        Err(e) => crate::logln!("config save error: {e}"),
    }
}
