import Foundation
import SQLite3

/// Codex: hooks (~/.codex/hooks.json, same shape as Claude Code's) and legacy notify.
public enum Codex {
    public static let hookEvents = ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"]

    public static var home: URL {
        if let d = ProcessInfo.processInfo.environment["CODEX_HOME"], !d.isEmpty { return URL(fileURLWithPath: d) }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".codex")
    }

    public static func target(exe: String, settings: URL? = nil) -> Hooks.Target {
        Hooks.Target(settings: settings ?? home.appendingPathComponent("hooks.json"), command: exe, args: ["--hook", "--agent", "codex"], events: hookEvents, execForm: false)
    }

    static func toolStep(_ name: String, _ input: JSON?) -> String {
        var cmd = input?.str("command") ?? ""
        if cmd.isEmpty, let parts = input?["command"]?.array { cmd = parts.compactMap { $0.string }.joined(separator: " ") }
        return clip(cmd.isEmpty ? name : "\(name): \(cmd)", 70)
    }

    public static func onPush(_ p: JSON, pid: Int32?) -> [AgentEvent] {
        // legacy notify: {"type":"agent-turn-complete","thread-id":"..","cwd":".."}
        if p.str("type") == "agent-turn-complete" {
            let id = p.str("thread-id")
            return id.isEmpty ? [] : [AgentEvent(.codex, id, p.str("cwd"), .done)]
        }
        let sid = p.str("session_id")
        if sid.isEmpty { return [] }
        let tool = p.str("tool_name")
        let kind: EventKind
        switch p.str("hook_event_name") {
        case "SessionStart": kind = .sessionStarted
        case "UserPromptSubmit": kind = .working(step: "Thinking…")
        case "PreToolUse": kind = .working(step: toolStep(tool, p["tool_input"]))
        case "PostToolUse": kind = .working(step: "")
        case "PermissionRequest": kind = .needsInput(reason: "Approval: \(toolStep(tool, p["tool_input"]))")
        case "Stop": kind = .done
        case "SessionEnd": kind = .sessionEnded
        default: return []
        }
        var ev = AgentEvent(.codex, sid, p.str("cwd"), kind)
        ev.pid = pid
        return [ev]
    }
}

/// T3 Code has no hooks: its state database is read (read-only) whenever it changes.
public enum T3Code {
    public struct Run {
        public var threadId: String
        public var status: String
        public var provider: String
        public var requestedAt: UInt64
        public var title: String
        public var root: String
        public var providerThreadId: String
        public var pending: String

        public init(threadId: String, status: String, provider: String = "", requestedAt: UInt64 = nowUnix(), title: String = "", root: String = "", providerThreadId: String = "", pending: String = "") {
            self.threadId = threadId
            self.status = status
            self.provider = provider
            self.requestedAt = requestedAt
            self.title = title
            self.root = root
            self.providerThreadId = providerThreadId
            self.pending = pending
        }
    }

    public static var database: URL {
        let base = ProcessInfo.processInfo.environment["T3CODE_HOME"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) } ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".t3")
        return base.appendingPathComponent("userdata").appendingPathComponent("statev2.sqlite")
    }

    static let sql = """
    select r.thread_id, r.status, r.provider, r.requested_at, ifnull(t.title,''), ifnull(p.workspace_root,''), ifnull(r.provider_thread_id,''), \
    ifnull((select q.kind from orchestration_v2_projection_runtime_requests q where q.thread_id = r.thread_id and q.status = 'pending' limit 1),'') \
    from orchestration_v2_projection_runs r \
    join orchestration_v2_projection_threads t on t.thread_id = r.thread_id \
    left join projection_projects p on p.project_id = t.project_id \
    order by r.requested_at desc limit 60
    """

    /// Latest runs, newest first. Nil if the database cannot be read (not installed, or its layout changed).
    public static func load() -> [Run]? {
        var db: OpaquePointer?
        guard sqlite3_open_v2(database.path, &db, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else {
            sqlite3_close(db)
            return nil
        }
        defer { sqlite3_close(db) }
        sqlite3_busy_timeout(db, 400)
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return nil }
        defer { sqlite3_finalize(stmt) }
        var rows = [Run]()
        func col(_ i: Int32) -> String { sqlite3_column_text(stmt, i).map { String(cString: $0) } ?? "" }
        while sqlite3_step(stmt) == SQLITE_ROW, rows.count < 500 {
            rows.append(Run(threadId: col(0), status: col(1), provider: col(2), requestedAt: parseTimestamp(col(3)) ?? 0, title: col(4), root: col(5), providerThreadId: col(6), pending: col(7)))
        }
        return rows
    }

    static func providerLabel(_ p: String) -> String {
        let l = p.lowercased()
        if l.hasPrefix("claude") { return "Claude" }
        if l.hasPrefix("codex") { return "Codex" }
        return p
    }

    public static func kind(of run: Run) -> EventKind {
        if !run.pending.isEmpty {
            if run.pending.contains("approval") { return .needsInput(reason: "Waiting for your approval") }
            if run.pending.contains("input") { return .needsInput(reason: "Has a question for you") }
            return .needsInput(reason: "Waiting for you")
        }
        switch run.status {
        case "preparing", "queued", "starting", "running": return .working(step: "Running · \(providerLabel(run.provider))")
        case "waiting": return .needsInput(reason: "Waiting for you")
        case "failed": return .error(message: "Run failed")
        default: return .done
        }
    }

    /// Compare each thread's newest run with what was seen before. On the first pass only
    /// threads that are active right now are reported.
    public static func diff(seen: inout [String: String], runs: [Run], first: Bool, now: UInt64) -> [AgentEvent] {
        var out = [AgentEvent]()
        var done = Set<String>()
        for run in runs where done.insert(run.threadId).inserted {
            let sig = "\(run.status)|\(run.pending)"
            if seen[run.threadId] == sig { continue }
            seen[run.threadId] = sig
            let k = kind(of: run)
            var active = false
            switch k {
            case .working, .needsInput: active = true
            default: break
            }
            if (first && !active) || (active && now > run.requestedAt && now - run.requestedAt > 86400) { continue }
            var ev = AgentEvent(.t3code, run.threadId, run.root, k)
            ev.title = run.title.isEmpty ? nil : run.title
            ev.alias = run.providerThreadId.isEmpty ? nil : run.providerThreadId
            if first { ev.at = max(run.requestedAt, 1) }
            out.append(ev)
        }
        return out
    }
}
