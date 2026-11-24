//! T3 Code adapter.
//!
//! T3 Code has no hook or notify mechanism, so this reads its own state: the SQLite
//! database at ~/.t3/userdata/statev2.sqlite (event-sourced, with projection tables).
//! AgentNotch opens it read-only whenever the file changes and turns each thread's latest run
//! into a session. Table and column names were checked against a real database
//! (T3 Code, November 2026); if a later version changes them the query simply fails and
//! this adapter stays quiet.
//!
//! Limits: steps inside a run (individual tool calls) are not shown, only its state.
//! Claude sessions that T3 Code starts with the user's normal Claude settings also fire
//! Claude Code hooks; those are folded into the T3 row when T3's run names the session.
use std::collections::HashMap;
use std::path::PathBuf;

use super::AgentAdapter;
use crate::app::{post, UiMsg};
use crate::config::Config;
use crate::model::{Agent, AgentEvent, EventKind};
use crate::util::{now_unix, parse_rfc3339};

pub struct T3Code;

fn userdata_dir() -> PathBuf {
    let base = match std::env::var_os("T3CODE_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => crate::util::home().join(".t3"),
    };
    base.join("userdata")
}

const DB: &str = "statev2.sqlite";

/// Latest runs with their thread title, project folder and any pending request.
const SQL: &str = "select r.thread_id, r.status, r.provider, r.requested_at, ifnull(t.title,''), ifnull(p.workspace_root,''), ifnull(r.provider_thread_id,''), \
     ifnull((select q.kind from orchestration_v2_projection_runtime_requests q where q.thread_id = r.thread_id and q.status = 'pending' limit 1),'') \
     from orchestration_v2_projection_runs r \
     join orchestration_v2_projection_threads t on t.thread_id = r.thread_id \
     left join projection_projects p on p.project_id = t.project_id \
     order by r.requested_at desc limit 60";

pub struct Run {
    pub thread_id: String,
    pub status: String,
    pub provider: String,
    pub requested_at: u64,
    pub title: String,
    pub root: String,
    pub provider_thread_id: String,
    pub pending: String,
}

fn load() -> Option<Vec<Run>> {
    let rows = crate::sqlite::query(&userdata_dir().join(DB), SQL, 8)?;
    Some(
        rows.into_iter()
            .map(|r| Run {
                requested_at: parse_rfc3339(&r[3]).unwrap_or(0),
                thread_id: r[0].clone(),
                status: r[1].clone(),
                provider: r[2].clone(),
                title: r[4].clone(),
                root: r[5].clone(),
                provider_thread_id: r[6].clone(),
                pending: r[7].clone(),
            })
            .collect(),
    )
}

/// One line for `AgentNotch.exe --status`.
pub fn status() -> String {
    if !userdata_dir().join(DB).is_file() {
        return "not installed".into();
    }
    match load() {
        Some(runs) => {
            let active = diff(&mut HashMap::new(), &runs, true, now_unix()).len();
            format!("database readable, {} recent runs, {} thread(s) active now", runs.len(), active)
        }
        None => "database found but could not be read (schema changed, or SQLite unavailable)".into(),
    }
}

/// "claudeAgent_2" -> "Claude", "codex" -> "Codex"
fn provider_label(p: &str) -> &str {
    let l = p.to_ascii_lowercase();
    if l.starts_with("claude") {
        "Claude"
    } else if l.starts_with("codex") {
        "Codex"
    } else if l.starts_with("cursor") {
        "Cursor"
    } else if l.starts_with("opencode") {
        "OpenCode"
    } else {
        p
    }
}

/// The normalized state of one run.
pub fn kind_of(run: &Run) -> EventKind {
    if !run.pending.is_empty() {
        let reason = match run.pending.as_str() {
            k if k.contains("approval") => "Waiting for your approval",
            k if k.contains("input") => "Has a question for you",
            _ => "Waiting for you",
        };
        return EventKind::NeedsInput { reason: reason.into() };
    }
    match run.status.as_str() {
        "preparing" | "queued" | "starting" | "running" => EventKind::Working { step: format!("Running · {}", provider_label(&run.provider)) },
        "waiting" => EventKind::NeedsInput { reason: "Waiting for you".into() },
        "failed" => EventKind::Error { message: "Run failed".into() },
        // completed, interrupted, cancelled, rolled_back
        _ => EventKind::Done,
    }
}

/// Compare the newest run of each thread with what was seen before and produce events for
/// what changed. On the first pass only threads that are active right now are reported.
pub fn diff(seen: &mut HashMap<String, String>, runs: &[Run], first: bool, now: u64) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let mut done: std::collections::HashSet<&str> = Default::default();
    for run in runs {
        // rows are newest first: the first row of a thread is its current run
        if !done.insert(run.thread_id.as_str()) {
            continue;
        }
        let sig = format!("{}|{}", run.status, run.pending);
        if seen.get(&run.thread_id) == Some(&sig) {
            continue;
        }
        seen.insert(run.thread_id.clone(), sig);
        let kind = kind_of(run);
        let active = matches!(kind, EventKind::Working { .. } | EventKind::NeedsInput { .. });
        // history, or a run that has been "running" for a day (T3 was killed mid-run)
        if (first && !active) || (active && now.saturating_sub(run.requested_at) > 86_400) {
            continue;
        }
        let mut ev = AgentEvent::new(Agent::T3Code, &run.thread_id, &run.root, kind);
        ev.title = Some(run.title.clone()).filter(|t| !t.is_empty());
        ev.alias = Some(run.provider_thread_id.clone()).filter(|a| !a.is_empty());
        if first {
            ev.at = run.requested_at.max(1);
        }
        out.push(ev);
    }
    out
}

impl AgentAdapter for T3Code {
    fn id(&self) -> &'static str {
        "t3code"
    }
    fn agent(&self) -> Agent {
        Agent::T3Code
    }
    fn enabled(&self, cfg: &Config) -> bool {
        cfg.adapters.t3code
    }

    fn start(&self, _cfg: &Config) {
        let dir = userdata_dir();
        if !dir.join(DB).is_file() {
            return; // T3 Code is not installed
        }
        let _ = std::thread::Builder::new().name("t3code".into()).stack_size(256 * 1024).spawn(move || {
            let mut seen = HashMap::new();
            let mut first = true;
            let mut poll = move || match load() {
                Some(runs) => {
                    let evs = diff(&mut seen, &runs, first, now_unix());
                    first = false;
                    if !evs.is_empty() {
                        post(UiMsg::Events(evs));
                    }
                }
                None => {
                    if first {
                        crate::logln!("t3code: could not read the state database (schema changed or SQLite unavailable)");
                        first = false;
                    }
                }
            };
            poll();
            // the database (or its write-ahead log) changes whenever a run changes state
            crate::watch::watch_dir(&dir, false, 1500, |changed| {
                if changed.iter().any(|p| p.file_name().map(|n| n.to_string_lossy().starts_with(DB)).unwrap_or(false)) {
                    poll();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(thread: &str, status: &str, pending: &str) -> Run {
        Run { thread_id: thread.into(), status: status.into(), provider: "claudeAgent_2".into(), requested_at: now_unix() - 10, title: "Fix login".into(), root: "C:\\dev\\app".into(), provider_thread_id: "x:abc".into(), pending: pending.into() }
    }

    #[test]
    fn first_pass_reports_only_active_threads() {
        let mut seen = HashMap::new();
        let runs = [run("a", "running", ""), run("b", "completed", ""), run("a", "completed", "")];
        let evs = diff(&mut seen, &runs, true, now_unix());
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].session_id, "a");
        assert_eq!(evs[0].kind, EventKind::Working { step: "Running · Claude".into() });
        assert_eq!(evs[0].title.as_deref(), Some("Fix login"));
        // nothing changed: nothing reported
        assert!(diff(&mut seen, &runs, false, now_unix()).is_empty());
    }

    #[test]
    fn transitions() {
        let mut seen = HashMap::new();
        diff(&mut seen, &[run("a", "running", "")], true, now_unix());
        let evs = diff(&mut seen, &[run("a", "running", "approval_request")], false, now_unix());
        assert!(matches!(evs[0].kind, EventKind::NeedsInput { .. }));
        let evs = diff(&mut seen, &[run("a", "completed", "")], false, now_unix());
        assert_eq!(evs[0].kind, EventKind::Done);
        let evs = diff(&mut seen, &[run("a", "failed", "")], false, now_unix());
        assert!(matches!(evs[0].kind, EventKind::Error { .. }));
    }
}
