import Foundation

public func nowUnix() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970)) }

public enum Agent: String, CaseIterable {
    case claude, codex, t3code

    public var label: String {
        switch self {
        case .claude: return "Claude Code"
        case .codex: return "Codex"
        case .t3code: return "T3 Code"
        }
    }
}

public enum EventKind: Equatable {
    case sessionStarted
    case working(step: String)
    case needsInput(reason: String)
    case done
    case error(message: String)
    case sessionEnded
    /// Metadata only (title, subagents): never changes a session's state or creates one.
    case info
}

/// News about a subagent working inside a session.
public struct SubEvent: Equatable {
    public var id: String
    public var kind: String
    public var step: String?
    public var ended: Bool
    public init(id: String, kind: String, step: String? = nil, ended: Bool = false) {
        self.id = id
        self.kind = kind
        self.step = step
        self.ended = ended
    }
}

/// What every adapter emits.
public struct AgentEvent {
    public var sessionId: String
    public var agent: Agent
    public var projectDir: String
    public var kind: EventKind
    public var title: String?
    public var pid: Int32?
    /// True for push sources (hooks). False for files.
    public var live = true
    /// Low-confidence "waiting" signal (idle prompt): never overrides done.
    public var soft = false
    public var at: UInt64 = nowUnix()
    public var remote = false
    /// The desktop app's id for this session, used to jump straight to it.
    public var host: String?
    /// Listed in Claude Code's registry of open sessions.
    public var open = false
    /// Runs inside a host (desktop app, SDK) that shows permission prompts itself.
    public var hosted = false
    /// The agent's own word on its state: accepted even when hooks are live.
    public var trusted = false
    public var turnEnd = false
    public var sub: SubEvent?
    /// Another adapter's name for the same work (T3 Code naming the session it drives).
    public var alias: String?

    public init(_ agent: Agent, _ sessionId: String, _ projectDir: String, _ kind: EventKind) {
        self.agent = agent
        self.sessionId = sessionId
        self.projectDir = projectDir
        self.kind = kind
    }
}

public enum SessionState: Int {
    case working, needsInput, done, error
}

public struct Sub: Identifiable, Equatable {
    public var id: String
    public var kind: String
    public var step: String
    public var last: UInt64
}

public struct Session: Identifiable, Equatable {
    public var id: String
    public var agent: Agent
    public var dir: String
    public var title = ""
    public var step = ""
    public var state = SessionState.working
    public var pid: Int32?
    public var started: UInt64
    public var lastEvent: UInt64
    public var finished: UInt64?
    public var live = false
    public var remote = false
    public var subs: [Sub] = []
    public var host = ""
    public var open = false
    public var stateSince: UInt64
    public var hosted = false
    /// A permission request waiting for Approve / Deny (an id the app maps to a connection).
    public var pending: Int?

    public var name: String {
        if !title.isEmpty { return title }
        if !dir.isEmpty { return baseName(dir) }
        return "session"
    }

    public var active: Bool { state == .working || state == .needsInput }

    /// Needs-input pulses until answered; finished and error pulse for 90 seconds.
    public func pulsing(_ now: UInt64) -> Bool {
        switch state {
        case .working: return false
        case .needsInput: return true
        case .done, .error: return now >= stateSince && now - stateSince < 90
        }
    }
}

public enum Alert: Equatable {
    case needsInput(name: String, agent: Agent, reason: String, id: String)
    case done(name: String, agent: Agent, id: String)
    case error(name: String, agent: Agent, message: String, id: String)

    public var sessionId: String {
        switch self {
        case let .needsInput(_, _, _, id), let .done(_, _, id), let .error(_, _, _, id): return id
        }
    }
}

public func baseName(_ path: String) -> String {
    let trimmed = path.hasSuffix("/") ? String(path.dropLast()) : path
    return trimmed.split(separator: "/").last.map(String.init) ?? trimmed
}

public func clip(_ s: String, _ max: Int) -> String {
    let one = s.replacingOccurrences(of: "\n", with: " ").replacingOccurrences(of: "\r", with: " ").replacingOccurrences(of: "\t", with: " ")
    if one.count <= max { return one }
    return String(one.prefix(max - 1)) + "…"
}

/// The session list and the rules for how events change it.
public struct Store {
    public private(set) var sessions: [Session] = []
    public var lastDir = ""
    private var dismissed = Set<String>()
    private var wrapped: [String] = []
    private var early: [String: (title: String, remote: Bool, subs: [SubEvent])] = [:]

    public init() {}

    /// Apply one event. Returns an alert when the session moved into needs-input, done or error.
    @discardableResult
    public mutating func apply(_ ev: AgentEvent) -> Alert? {
        if dismissed.contains(ev.sessionId) {
            // only fresh live activity brings a dismissed session back
            var revive = false
            if ev.live, !ev.soft {
                switch ev.kind {
                case .working, .needsInput: revive = true
                default: break
                }
            }
            if revive { dismissed.remove(ev.sessionId) } else { return nil }
        }
        if let alias = ev.alias, alias.count >= 8 {
            sessions.removeAll { $0.agent != ev.agent && $0.id.count >= 8 && alias.contains($0.id) }
            if !wrapped.contains(alias) {
                if wrapped.count >= 32 { wrapped.removeFirst() }
                wrapped.append(alias)
            }
        } else if ev.sessionId.count >= 8, wrapped.contains(where: { $0.contains(ev.sessionId) }) {
            return nil
        }

        var idx = sessions.firstIndex { $0.id == ev.sessionId && $0.agent == ev.agent }
        var fresh = false
        if idx == nil {
            if ev.kind == .info {
                // keep it for when the session appears, so it never shows a folder name first
                var e = early[ev.sessionId] ?? ("", false, [])
                if let t = ev.title, !t.isEmpty { e.title = clip(t, 60) }
                e.remote = e.remote || ev.remote
                if let sub = ev.sub {
                    e.subs.removeAll { $0.id == sub.id }
                    if !sub.ended { e.subs.append(sub) }
                }
                if early.count > 64 { early.removeAll() }
                early[ev.sessionId] = e
                return nil
            }
            if ev.kind == .sessionEnded { return nil }
            let e = early.removeValue(forKey: ev.sessionId)
            var s = Session(id: ev.sessionId, agent: ev.agent, dir: ev.projectDir, started: ev.at, lastEvent: ev.at, stateSince: ev.at)
            s.title = e?.title ?? ""
            s.remote = e?.remote ?? false
            s.subs = (e?.subs ?? []).map { Sub(id: $0.id, kind: $0.kind, step: $0.step ?? "", last: ev.at) }
            sessions.append(s)
            idx = sessions.count - 1
            fresh = true
        }
        guard let i = idx else { return nil }

        if let t = ev.title, !t.isEmpty { sessions[i].title = clip(t, 60) }
        if !ev.projectDir.isEmpty, sessions[i].dir.isEmpty || ev.live { sessions[i].dir = ev.projectDir }
        if let pid = ev.pid { sessions[i].pid = pid }
        if let sub = ev.sub {
            if sub.ended {
                sessions[i].subs.removeAll { $0.id == sub.id }
            } else if let k = sessions[i].subs.firstIndex(where: { $0.id == sub.id }) {
                if let step = sub.step { sessions[i].subs[k].step = step }
                // the longer label wins (a description beats a bare agent type)
                if sub.kind.count > sessions[i].subs[k].kind.count { sessions[i].subs[k].kind = sub.kind }
                sessions[i].subs[k].last = ev.at
            } else {
                sessions[i].subs.append(Sub(id: sub.id, kind: sub.kind, step: sub.step ?? "", last: ev.at))
            }
        }
        if ev.remote { sessions[i].remote = true }
        if ev.open { sessions[i].open = true }
        if ev.hosted { sessions[i].hosted = true }
        if let h = ev.host { sessions[i].host = h }
        if ev.kind == .info { return nil }

        // Once hooks are flowing for a session, files must not fight them...
        if !ev.live, sessions[i].live {
            // ...except the agent's own word, or the end of a turn, when nothing live happened since
            if !((ev.turnEnd || ev.trusted) && ev.at >= sessions[i].lastEvent) { return nil }
        }
        if ev.live {
            sessions[i].live = true
            if !ev.projectDir.isEmpty { lastDir = ev.projectDir }
        }

        let prev = sessions[i].state
        sessions[i].lastEvent = max(ev.at, sessions[i].lastEvent)
        var alert: Alert?
        let name = sessions[i].name
        let agent = sessions[i].agent
        let id = sessions[i].id

        switch ev.kind {
        case .sessionStarted:
            if fresh {
                // a brand new session is idle until the first prompt
                sessions[i].state = .done
                sessions[i].step = "ready"
                sessions[i].finished = ev.at
            }
        case let .working(step):
            sessions[i].state = .working
            sessions[i].finished = nil
            if !step.isEmpty { sessions[i].step = step }
        case let .needsInput(reason):
            if ev.soft, prev == .done || prev == .error { return nil }
            sessions[i].state = .needsInput
            sessions[i].finished = nil
            sessions[i].step = reason.isEmpty ? "needs input" : reason
            if prev != .needsInput { alert = .needsInput(name: name, agent: agent, reason: reason, id: id) }
        case .done:
            sessions[i].state = .done
            sessions[i].finished = ev.at
            sessions[i].step = "finished"
            if prev == .working || prev == .needsInput, !fresh { alert = .done(name: name, agent: agent, id: id) }
        case let .error(message):
            sessions[i].state = .error
            sessions[i].finished = ev.at
            sessions[i].step = message.isEmpty ? "error" : clip(message, 80)
            if prev != .error { alert = .error(name: name, agent: agent, message: message, id: id) }
        case .sessionEnded:
            if sessions[i].active { sessions[i].state = .done }
            sessions[i].step = "session ended"
            sessions[i].pid = nil
            sessions[i].subs.removeAll()
            sessions[i].open = false
            sessions[i].finished = min(sessions[i].finished ?? ev.at, ev.at)
            sessions[i].lastEvent = 0 // marks "ended": leaves within a minute
        case .info:
            break
        }
        if sessions[i].state != prev || fresh { sessions[i].stateSince = ev.at }
        // only things that just happened alert: replayed history never does
        let now = nowUnix()
        if now > ev.at, now - ev.at > 30 { alert = nil }
        sort()
        return alert
    }

    private mutating func sort() {
        func rank(_ s: Session) -> Int {
            switch s.state {
            case .needsInput: return 0
            case .working: return 1
            case .error: return 2
            case .done: return 3
            }
        }
        sessions.sort { a, b in
            rank(a) != rank(b) ? rank(a) < rank(b) : a.started > b.started
        }
    }

    /// Drop sessions that are over. Returns true if anything changed.
    @discardableResult
    public mutating func expire(keepSecs: UInt64, staleSecs: UInt64, alive: (Int32) -> Bool) -> Bool {
        let now = nowUnix()
        let before = sessions
        for i in sessions.indices {
            sessions[i].subs.removeAll { now > $0.last && now - $0.last >= 900 }
            // the agent process is gone but no end event arrived
            if sessions[i].active, now > sessions[i].lastEvent, now - sessions[i].lastEvent > 20, let pid = sessions[i].pid, !alive(pid) {
                sessions[i].state = .done
                sessions[i].step = "session ended"
                sessions[i].finished = now
                sessions[i].lastEvent = 0
                sessions[i].pid = nil
                sessions[i].open = false
            }
        }
        sessions.removeAll { s in
            let pidAlive = s.pid.map(alive) ?? false
            if let f = s.finished, !s.active {
                // an open session stays listed while its process lives, even when idle
                if s.open, pidAlive { return false }
                let keep = s.lastEvent == 0 ? min(60, keepSecs) : keepSecs
                return now >= f && now - f >= keep
            }
            return !(pidAlive || (now >= s.lastEvent ? now - s.lastEvent < staleSecs : true))
        }
        sort()
        return sessions != before
    }

    public mutating func dismiss(_ id: String) {
        sessions.removeAll { $0.id == id }
        dismissed.insert(id)
    }

    public mutating func clearFinished() {
        sessions.removeAll { !$0.active }
    }

    public mutating func setPending(_ sessionId: String, _ token: Int?) {
        if let i = sessions.firstIndex(where: { $0.id == sessionId }) { sessions[i].pending = token }
    }

    public mutating func setStep(_ sessionId: String, _ step: String) {
        if let i = sessions.firstIndex(where: { $0.id == sessionId }) { sessions[i].step = step }
    }

    public var activeCount: Int { sessions.filter { $0.active }.count }
}
