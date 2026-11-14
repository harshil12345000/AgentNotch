//! Normalized agent events and the session store.
use crate::util::{base_name, now_unix};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agent {
    Claude,
    Codex,
    T3Code,
    Gemini,
    Cursor,
    OpenCode,
}

impl Agent {
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::T3Code => "T3 Code",
            Agent::Gemini => "Gemini CLI",
            Agent::Cursor => "Cursor",
            Agent::OpenCode => "OpenCode",
        }
    }
    /// Index into `logos::ALL`.
    pub fn logo(self) -> usize {
        match self {
            Agent::Claude => 0,
            Agent::Codex => 1,
            Agent::T3Code => 2,
            Agent::Gemini => 3,
            Agent::Cursor => 4,
            Agent::OpenCode => 5,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum EventKind {
    SessionStarted,
    Working { step: String },
    NeedsInput { reason: String },
    Done,
    Error { message: String },
    SessionEnded,
    /// Metadata only (title, folder): never changes a session's state or creates one.
    Info,
}

/// What every adapter emits.
#[derive(Clone, Debug)]
pub struct AgentEvent {
    pub session_id: String,
    pub agent: Agent,
    pub project_dir: String,
    pub title: Option<String>,
    /// Process ids, nearest first (the agent process, then its ancestors), when known.
    pub pids: Vec<u32>,
    pub kind: EventKind,
    /// True for push sources (hooks / notify). False for file-watcher backfill.
    pub live: bool,
    /// Low-confidence "waiting" signal (e.g. idle prompt): never overrides Done.
    pub soft: bool,
    /// Event time, unix seconds (backfill uses the file's timestamps).
    pub at: u64,
    /// The session is being followed through Remote Control.
    pub remote: bool,
}

impl AgentEvent {
    pub fn new(agent: Agent, session_id: &str, project_dir: &str, kind: EventKind) -> Self {
        AgentEvent {
            session_id: session_id.to_string(),
            agent,
            project_dir: project_dir.to_string(),
            title: None,
            pids: Vec::new(),
            kind,
            live: true,
            soft: false,
            at: now_unix(),
            remote: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Working,
    NeedsInput,
    Done,
    Error,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub agent: Agent,
    pub dir: String,
    pub title: String,
    pub step: String,
    pub state: State,
    pub pids: Vec<u32>,
    pub started: u64,
    pub last_event: u64,
    pub finished: Option<u64>,
    /// Has received at least one live (hook) event; file backfill no longer drives its state.
    pub live: bool,
    /// Followed through Remote Control (seen in its transcript).
    pub remote: bool,
}

impl Session {
    pub fn name(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.dir.is_empty() {
            base_name(&self.dir)
        } else {
            "session"
        }
    }
    pub fn active(&self) -> bool {
        matches!(self.state, State::Working | State::NeedsInput)
    }
}

/// A state change worth telling the user about.
#[derive(Clone, Debug, PartialEq)]
pub enum Alert {
    NeedsInput { name: String, agent: Agent, reason: String, id: String },
    Done { name: String, agent: Agent, id: String },
    Error { name: String, agent: Agent, message: String, id: String },
}

#[derive(Default)]
pub struct Store {
    pub sessions: Vec<Session>,
    /// Sessions the user dismissed. They stay hidden until they do something new.
    dismissed: std::collections::HashSet<String>,
    /// Most recent project dir seen on a live event (used by Remote Control).
    pub last_dir: String,
}

impl Store {
    /// Apply one event. Returns an alert when the session moved into NeedsInput / Done / Error.
    pub fn apply(&mut self, ev: AgentEvent) -> Option<Alert> {
        if self.dismissed.contains(&ev.session_id) {
            // only fresh live activity brings a dismissed session back
            if ev.live && matches!(ev.kind, EventKind::Working { .. } | EventKind::NeedsInput { .. }) && !ev.soft {
                self.dismissed.remove(&ev.session_id);
            } else {
                return None;
            }
        }
        let idx = self.sessions.iter().position(|s| s.id == ev.session_id && s.agent == ev.agent);
        let idx = match idx {
            Some(i) => i,
            None => {
                if matches!(ev.kind, EventKind::SessionEnded | EventKind::Info) {
                    return None;
                }
                self.sessions.push(Session {
                    id: ev.session_id.clone(),
                    agent: ev.agent,
                    dir: ev.project_dir.clone(),
                    title: String::new(),
                    step: String::new(),
                    state: State::Working,
                    pids: Vec::new(),
                    started: ev.at,
                    last_event: ev.at,
                    finished: None,
                    live: false,
                    remote: false,
                });
                self.sessions.len() - 1
            }
        };
        let fresh = self.sessions[idx].last_event == ev.at && self.sessions[idx].step.is_empty() && !self.sessions[idx].live;
        let s = &mut self.sessions[idx];
        if let Some(t) = &ev.title {
            if !t.is_empty() {
                s.title = crate::util::clip(t, 60);
            }
        }
        if !ev.project_dir.is_empty() && (s.dir.is_empty() || ev.live) {
            s.dir = ev.project_dir.clone();
        }
        if !ev.pids.is_empty() {
            s.pids = ev.pids.clone();
        }
        if ev.remote {
            s.remote = true;
        }
        if ev.kind == EventKind::Info {
            return None;
        }
        // Once hooks are flowing for a session, file backfill must not fight them.
        if !ev.live && s.live {
            return None;
        }
        if ev.live {
            s.live = true;
            if !ev.project_dir.is_empty() {
                self.last_dir = ev.project_dir.clone();
            }
        }
        let s = &mut self.sessions[idx];
        let prev = s.state;
        s.last_event = ev.at.max(s.last_event);
        let mut alert = None;
        match ev.kind {
            EventKind::SessionStarted => {
                if fresh {
                    // A brand new session is idle until the first prompt.
                    s.state = State::Done;
                    s.step = "ready".into();
                    s.finished = Some(ev.at);
                }
            }
            EventKind::Working { step } => {
                s.state = State::Working;
                s.finished = None;
                if !step.is_empty() {
                    s.step = step;
                }
            }
            EventKind::NeedsInput { reason } => {
                if ev.soft && matches!(prev, State::Done | State::Error) {
                    return None;
                }
                s.state = State::NeedsInput;
                s.finished = None;
                s.step = if reason.is_empty() { "needs input".into() } else { reason.clone() };
                if prev != State::NeedsInput {
                    alert = Some(Alert::NeedsInput { name: s.name().to_string(), agent: s.agent, reason, id: s.id.clone() });
                }
            }
            EventKind::Done => {
                s.state = State::Done;
                s.finished = Some(ev.at);
                s.step = "finished".into();
                if matches!(prev, State::Working | State::NeedsInput) && !fresh {
                    alert = Some(Alert::Done { name: s.name().to_string(), agent: s.agent, id: s.id.clone() });
                }
            }
            EventKind::Error { message } => {
                s.state = State::Error;
                s.finished = Some(ev.at);
                s.step = if message.is_empty() { "error".into() } else { crate::util::clip(&message, 80) };
                if prev != State::Error {
                    alert = Some(Alert::Error { name: s.name().to_string(), agent: s.agent, message, id: s.id.clone() });
                }
            }
            EventKind::Info => {}
            EventKind::SessionEnded => {
                if s.active() {
                    s.state = State::Done;
                }
                s.step = "session ended".into();
                s.pids.clear();
                // Ended sessions leave quickly: keep them one minute at most.
                s.finished = Some(s.finished.unwrap_or(ev.at).min(ev.at));
                s.last_event = 0;
            }
        }
        // Backfilled history never alerts.
        if !ev.live {
            alert = None;
        }
        self.sort();
        alert
    }

    fn sort(&mut self) {
        fn rank(s: &Session) -> u8 {
            match s.state {
                State::NeedsInput => 0,
                State::Working => 1,
                State::Error => 2,
                State::Done => 3,
            }
        }
        self.sessions.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.started.cmp(&a.started)));
    }

    /// Drop finished sessions older than `keep_secs`, ended ones after 60 s, and working
    /// sessions that have been silent for `stale_secs`. Returns true if anything changed.
    pub fn expire(&mut self, keep_secs: u64, stale_secs: u64, alive: &dyn Fn(u32) -> bool) -> bool {
        let now = now_unix();
        let before = self.sessions.len();
        let mut changed = false;
        for s in self.sessions.iter_mut() {
            // The agent process is gone but no SessionEnd arrived (killed terminal).
            if s.active() && now.saturating_sub(s.last_event) > 20 {
                if let Some(&pid) = s.pids.first() {
                    if !alive(pid) {
                        s.state = State::Done;
                        s.step = "session ended".into();
                        s.finished = Some(now);
                        s.last_event = 0;
                        s.pids.clear();
                        changed = true;
                    }
                }
            }
        }
        self.sessions.retain(|s| match s.finished {
            Some(f) if !s.active() => {
                let keep = if s.last_event == 0 { 60.min(keep_secs) } else { keep_secs };
                now.saturating_sub(f) < keep
            }
            // still running: keep while its process lives; without a known process
            // (recovered from a transcript) keep while it keeps writing
            _ => s.pids.first().map(|p| alive(*p)).unwrap_or(false) || now.saturating_sub(s.last_event) < stale_secs,
        });
        if changed {
            self.sort();
        }
        changed || self.sessions.len() != before
    }

    /// Hide one session now (the row's close button).
    pub fn dismiss(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            let s = self.sessions.remove(idx);
            self.dismissed.insert(s.id);
        }
    }

    /// Drop every session that is not running.
    pub fn clear_finished(&mut self) {
        self.sessions.retain(|s| s.active());
    }

    pub fn active_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.active()).count()
    }
}

#[derive(Clone, Debug, Default)]
pub struct UsageWindow {
    /// 0..100 when known.
    pub pct: Option<f32>,
    /// Token estimate (Source B).
    pub tokens: Option<u64>,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub five_hour: Option<UsageWindow>,
    pub week: Option<UsageWindow>,
    /// "api" | "estimate" | ""
    pub source: &'static str,
    pub note: String,
    pub fetched_at: u64,
    pub loading: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ev(kind: EventKind) -> AgentEvent {
        AgentEvent::new(Agent::Claude, "s1", "C:\\proj\\demo", kind)
    }
    #[test]
    fn lifecycle_and_alerts() {
        let mut st = Store::default();
        assert_eq!(st.apply(ev(EventKind::SessionStarted)), None);
        assert_eq!(st.sessions[0].state, State::Done); // idle until first prompt
        assert_eq!(st.apply(ev(EventKind::Working { step: "Bash: ls".into() })), None);
        assert_eq!(st.sessions[0].state, State::Working);
        let a = st.apply(ev(EventKind::NeedsInput { reason: "permission".into() }));
        assert!(matches!(a, Some(Alert::NeedsInput { .. })));
        // a second identical event does not alert again
        assert_eq!(st.apply(ev(EventKind::NeedsInput { reason: "permission".into() })), None);
        assert_eq!(st.apply(ev(EventKind::Working { step: "Edit a.rs".into() })), None);
        assert!(matches!(st.apply(ev(EventKind::Done)), Some(Alert::Done { .. })));
        // idle prompt after Done stays Done
        let mut soft = ev(EventKind::NeedsInput { reason: "idle".into() });
        soft.soft = true;
        assert_eq!(st.apply(soft), None);
        assert_eq!(st.sessions[0].state, State::Done);
        assert_eq!(st.sessions[0].name(), "demo");
    }
    #[test]
    fn dismissed_sessions_return_only_on_new_activity() {
        let mut st = Store::default();
        st.apply(ev(EventKind::Working { step: "x".into() }));
        st.apply(ev(EventKind::Done));
        st.dismiss(0);
        assert!(st.sessions.is_empty());
        st.apply(ev(EventKind::Done));
        st.apply(ev(EventKind::SessionEnded));
        assert!(st.sessions.is_empty());
        st.apply(ev(EventKind::Working { step: "again".into() }));
        assert_eq!(st.sessions.len(), 1);
    }
    #[test]
    fn backfill_does_not_override_live() {
        let mut st = Store::default();
        st.apply(ev(EventKind::Working { step: "x".into() }));
        let mut b = ev(EventKind::Done);
        b.live = false;
        assert_eq!(st.apply(b), None);
        assert_eq!(st.sessions[0].state, State::Working);
    }
    #[test]
    fn expiry() {
        let mut st = Store::default();
        st.apply(ev(EventKind::Working { step: "x".into() }));
        st.apply(ev(EventKind::Done));
        st.sessions[0].finished = Some(now_unix() - 700);
        assert!(st.expire(600, 3600, &|_| true));
        assert!(st.sessions.is_empty());
    }
}
