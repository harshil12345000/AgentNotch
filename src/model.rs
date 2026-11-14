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
    /// Another adapter's name for the same work (T3 Code names the Claude or Codex
    /// session it drives). Sessions with an id found in this text are folded into this one.
    pub alias: Option<String>,
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
            alias: None,
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
    /// Aliases of sessions that a wrapper (T3 Code) already shows.
    wrapped: Vec<String>,
    /// Titles (and Remote Control markers) seen before the session itself showed up.
    early: std::collections::HashMap<String, (String, bool)>,
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
        if let Some(alias) = ev.alias.as_ref().filter(|a| a.len() >= 8) {
            // this event comes from a wrapper: hide the wrapped agent's own row
            self.sessions.retain(|s| s.agent == ev.agent || s.id.len() < 8 || !alias.contains(&s.id));
            if !self.wrapped.contains(alias) {
                if self.wrapped.len() >= 32 {
                    self.wrapped.remove(0);
                }
                self.wrapped.push(alias.clone());
            }
        } else if ev.session_id.len() >= 8 && self.wrapped.iter().any(|a| a.contains(&ev.session_id)) {
            return None;
        }
        let idx = self.sessions.iter().position(|s| s.id == ev.session_id && s.agent == ev.agent);
        let idx = match idx {
            Some(i) => i,
            None => {
                if ev.kind == EventKind::Info {
                    // keep it for when the session appears, so it never shows a folder name first
                    let e = self.early.entry(ev.session_id.clone()).or_default();
                    if let Some(t) = ev.title.as_ref().filter(|t| !t.is_empty()) {
                        e.0 = crate::util::clip(t, 60);
                    }
                    e.1 |= ev.remote;
                    if self.early.len() > 64 {
                        self.early.clear();
                    }
                    return None;
                }
                if ev.kind == EventKind::SessionEnded {
                    return None;
                }
                let (early_title, early_remote) = self.early.remove(&ev.session_id).unwrap_or_default();
                self.sessions.push(Session {
                    id: ev.session_id.clone(),
                    agent: ev.agent,
                    dir: ev.project_dir.clone(),
                    title: early_title,
                    step: String::new(),
                    state: State::Working,
                    pids: Vec::new(),
                    started: ev.at,
                    last_event: ev.at,
                    finished: None,
                    live: false,
                    remote: early_remote,
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
        // Only things that just happened alert: replayed history (startup backfill, the
        // tail of a file seen for the first time) never does.
        if now_unix().saturating_sub(ev.at) > 30 {
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
    fn early_title_is_used_when_the_session_appears() {
        let mut st = Store::default();
        let mut info = ev(EventKind::Info);
        info.title = Some("Real title".into());
        info.remote = true;
        st.apply(info);
        assert!(st.sessions.is_empty());
        st.apply(ev(EventKind::Working { step: "x".into() }));
        assert_eq!(st.sessions[0].name(), "Real title");
        assert!(st.sessions[0].remote);
    }
    #[test]
    fn wrapper_hides_the_wrapped_session() {
        let mut st = Store::default();
        let mut inner = AgentEvent::new(Agent::Claude, "11111111-2222-3333-4444-555555555555", "C:\\p", EventKind::Working { step: "x".into() });
        st.apply(inner.clone());
        let mut t3 = AgentEvent::new(Agent::T3Code, "thread-1", "C:\\p", EventKind::Working { step: "Running".into() });
        t3.alias = Some("claude:acct:11111111-2222-3333-4444-555555555555".into());
        st.apply(t3);
        assert_eq!(st.sessions.len(), 1);
        assert_eq!(st.sessions[0].agent, Agent::T3Code);
        // later events from the wrapped session stay hidden
        inner.kind = EventKind::Done;
        st.apply(inner);
        assert_eq!(st.sessions.len(), 1);
    }
    #[test]
    fn old_events_do_not_alert() {
        let mut st = Store::default();
        st.apply(ev(EventKind::Working { step: "x".into() }));
        let mut old = ev(EventKind::Done);
        old.at = now_unix() - 120;
        assert_eq!(st.apply(old), None);
        assert_eq!(st.sessions[0].state, State::Done);
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
