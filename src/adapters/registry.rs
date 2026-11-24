//! Claude Code's own registry of open sessions: ~/.claude/sessions/<pid>.json.
//!
//! Claude Code (terminal and desktop app) keeps one small file per running session with
//! its process id, session id, folder, title and a live status that it rewrites whenever
//! the status changes: "busy", "idle", or "waiting" (with what it is waiting for).
//! This is the most reliable signal there is for *which sessions exist* and *whether one
//! is waiting on the user*, and it needs no hooks. AgentNotch watches the folder and turns
//! every change into an event. Hooks and transcripts still add the detail (current step,
//! subagents, the question a turn ended with).
//!
//! The file layout is internal to Claude Code (checked against 2.1.319 and 2.1.325). Every
//! field is optional here; if the layout changes, this source goes quiet and the others
//! carry on.
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::{post, UiMsg};
use crate::model::{Agent, AgentEvent, EventKind};
use crate::util::now_unix;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub pid: Option<u32>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub waiting_for: Option<String>,
    pub status_updated_at: Option<f64>,
    /// The desktop app's id for this session ("local_..."), when it runs inside the app.
    pub host_session_id: Option<String>,
    pub bridge_session_id: Option<String>,
    /// How the session was started: "cli", "claude-desktop", "sdk-cli", ...
    pub entrypoint: Option<String>,
    /// Pre-started idle process that has no conversation yet.
    pub spare: Option<serde_json::Value>,
}

pub fn dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("sessions"),
        _ => crate::util::home().join(".claude").join("sessions"),
    }
}

/// Every record whose process is still alive.
pub fn read_all(alive: &dyn Fn(u32) -> bool) -> Vec<Record> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir()) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().map(|x| x != "json").unwrap_or(true) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(r) = serde_json::from_slice::<Record>(&bytes) else { continue };
        let is_spare = r.spare.as_ref().map(|s| !s.is_null() && s != &serde_json::Value::Bool(false)).unwrap_or(false);
        if is_spare || r.session_id.as_deref().unwrap_or("").is_empty() {
            continue;
        }
        if r.pid.map(alive).unwrap_or(false) {
            out.push(r);
        }
    }
    out
}

/// What was last reported for a session: status, what it waits for, title.
#[derive(Default, PartialEq, Clone)]
pub struct Seen {
    status: String,
    waiting: String,
    name: String,
}

/// Compare the registry with what was reported before and produce events for the changes.
pub fn diff(seen: &mut HashMap<String, Seen>, records: &[Record], now: u64) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let mut present = std::collections::HashSet::new();
    for r in records {
        let sid = r.session_id.clone().unwrap_or_default();
        present.insert(sid.clone());
        let cur = Seen { status: r.status.clone().unwrap_or_else(|| "busy".into()), waiting: r.waiting_for.clone().unwrap_or_default(), name: r.name.clone().unwrap_or_default() };
        let prev = seen.get(&sid);
        if prev == Some(&cur) {
            continue;
        }
        let state_changed = prev.map(|p| p.status != cur.status || p.waiting != cur.waiting).unwrap_or(true);
        let kind = if !state_changed {
            EventKind::Info // only the title changed
        } else {
            match cur.status.as_str() {
                "waiting" => EventKind::NeedsInput { reason: if cur.waiting.is_empty() { "Waiting for you".into() } else { crate::util::clip(&cur.waiting, 80) } },
                // the turn is over: finished, or waiting on a question it ended with
                "idle" => super::claude::turn_end(&sid),
                _ => EventKind::Working { step: String::new() },
            }
        };
        let mut ev = AgentEvent::new(Agent::Claude, &sid, r.cwd.as_deref().unwrap_or(""), kind);
        ev.title = r.name.clone().filter(|n| !n.is_empty());
        ev.pids = r.pid.into_iter().collect();
        ev.host = r.host_session_id.clone().filter(|h| !h.is_empty());
        ev.remote = r.bridge_session_id.as_deref().map(|b| !b.is_empty()).unwrap_or(false);
        ev.open = true;
        ev.hosted = r.entrypoint.as_deref().map(|e| e == "claude-desktop" || e.starts_with("sdk")).unwrap_or(false);
        // Claude Code's own word on the state: accepted even when hooks are live
        ev.live = false;
        ev.trusted = true;
        // a session seen for the first time reports when its status last changed, so old
        // news does not alert; a change we watched happen is "now"
        ev.at = if prev.is_none() { r.status_updated_at.map(|t| (t / 1000.0) as u64).filter(|t| *t > 0 && *t <= now).unwrap_or(now) } else { now };
        seen.insert(sid, cur);
        out.push(ev);
    }
    // sessions that are gone from the registry (closed, or their process died)
    let gone: Vec<String> = seen.keys().filter(|k| !present.contains(*k)).cloned().collect();
    for sid in gone {
        seen.remove(&sid);
        out.push(AgentEvent::new(Agent::Claude, &sid, "", EventKind::SessionEnded));
    }
    out
}

pub fn start() {
    let root = dir();
    if !root.is_dir() {
        return;
    }
    let _ = std::thread::Builder::new().name("registry".into()).stack_size(256 * 1024).spawn(move || {
        let mut seen = HashMap::new();
        let mut scan = move || {
            let evs = diff(&mut seen, &read_all(&crate::winutil::pid_alive), now_unix());
            if !evs.is_empty() {
                post(UiMsg::Events(evs));
            }
        };
        scan();
        crate::watch::watch_dir(&root, false, 40, |_| scan());
    });
}

/// Called now and then by the UI thread's housekeeping: a process that died without
/// cleaning up leaves its file behind, which no file event will ever report.
pub fn status_line() -> String {
    let recs = read_all(&crate::winutil::pid_alive);
    let count = |s: &str| recs.iter().filter(|r| r.status.as_deref() == Some(s)).count();
    format!("{} open ({} busy, {} waiting, {} idle)", recs.len(), count("busy"), count("waiting"), count("idle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(sid: &str, status: &str, waiting: Option<&str>) -> Record {
        Record {
            pid: Some(4242),
            session_id: Some(sid.into()),
            cwd: Some("C:\\dev\\app".into()),
            name: Some("Fix login".into()),
            status: Some(status.into()),
            waiting_for: waiting.map(|w| w.to_string()),
            status_updated_at: Some((now_unix() as f64 - 600.0) * 1000.0),
            host_session_id: Some("local_abc".into()),
            bridge_session_id: None,
            entrypoint: Some("claude-desktop".into()),
            spare: None,
        }
    }

    #[test]
    fn follows_the_registry() {
        let now = now_unix();
        let mut seen = HashMap::new();
        // first sight: reported with the time the status last changed (so no alert)
        let e = diff(&mut seen, &[rec("r1", "busy", None)], now);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, EventKind::Working { step: String::new() });
        assert_eq!(e[0].title.as_deref(), Some("Fix login"));
        assert_eq!(e[0].host.as_deref(), Some("local_abc"));
        assert!(e[0].open && e[0].trusted && e[0].hosted && now - e[0].at >= 590);
        // nothing changed
        assert!(diff(&mut seen, &[rec("r1", "busy", None)], now).is_empty());
        // a permission prompt
        let e = diff(&mut seen, &[rec("r1", "waiting", Some("approve Bash"))], now);
        assert_eq!(e[0].kind, EventKind::NeedsInput { reason: "approve Bash".into() });
        assert_eq!(e[0].at, now);
        // turn over
        let e = diff(&mut seen, &[rec("r1", "idle", None)], now);
        assert_eq!(e[0].kind, EventKind::Done);
        // closed
        let e = diff(&mut seen, &[], now);
        assert_eq!(e[0].kind, EventKind::SessionEnded);
        assert!(seen.is_empty());
    }
}
