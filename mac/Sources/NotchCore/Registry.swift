import Foundation

/// Claude Code's own registry of open sessions: ~/.claude/sessions/<pid>.json.
///
/// One small file per running session with its process id, session id, folder, title and a
/// live status Claude Code rewrites on every change: "busy", "idle", or "waiting" (with
/// what it waits for). The layout is internal to Claude Code, so every field is optional.
public enum Registry {
    public struct Record {
        public var pid: Int32
        public var sessionId: String
        public var cwd = ""
        public var name = ""
        public var status = "busy"
        public var waitingFor = ""
        public var statusUpdatedAt: UInt64?
        public var host: String?
        public var remote = false
        public var hosted = false

        public init(pid: Int32, sessionId: String) {
            self.pid = pid
            self.sessionId = sessionId
        }
    }

    public static var dir: URL { Claude.configDir.appendingPathComponent("sessions") }

    public static func parse(_ data: Data) -> Record? {
        guard let j = JSON.parse(data), let pid = j["pid"]?.double, !j.str("sessionId").isEmpty else { return nil }
        // a pre-started idle process with no conversation yet
        if let spare = j["spare"], spare != .null, spare != .bool(false) { return nil }
        var r = Record(pid: Int32(pid), sessionId: j.str("sessionId"))
        r.cwd = j.str("cwd")
        r.name = j.str("name")
        if !j.str("status").isEmpty { r.status = j.str("status") }
        r.waitingFor = j.str("waitingFor")
        r.statusUpdatedAt = j["statusUpdatedAt"]?.double.map { UInt64(max(0, $0 / 1000)) }
        let h = j.str("hostSessionId")
        r.host = h.isEmpty ? nil : h
        r.remote = !j.str("bridgeSessionId").isEmpty
        let e = j.str("entrypoint")
        r.hosted = e == "claude-desktop" || e.hasPrefix("sdk")
        return r
    }

    /// Every record whose process is still alive.
    public static func readAll(alive: (Int32) -> Bool) -> [Record] {
        guard let files = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) else { return [] }
        return files.filter { $0.pathExtension == "json" }.compactMap { (try? Data(contentsOf: $0)).flatMap(parse) }.filter { alive($0.pid) }
    }

    public struct Seen: Equatable {
        var status: String
        var waiting: String
        var name: String
    }

    /// Compare the registry with what was reported before and produce events for the changes.
    public static func diff(seen: inout [String: Seen], records: [Record], now: UInt64) -> [AgentEvent] {
        var out = [AgentEvent]()
        var present = Set<String>()
        for r in records {
            present.insert(r.sessionId)
            let cur = Seen(status: r.status, waiting: r.waitingFor, name: r.name)
            let prev = seen[r.sessionId]
            if prev == cur { continue }
            let stateChanged = prev.map { $0.status != cur.status || $0.waiting != cur.waiting } ?? true
            let kind: EventKind
            if !stateChanged {
                kind = .info // only the title changed
            } else {
                switch r.status {
                case "waiting": kind = .needsInput(reason: r.waitingFor.isEmpty ? "Waiting for you" : clip(r.waitingFor, 80))
                case "idle": kind = Claude.turnEnd(r.sessionId)
                default: kind = .working(step: "")
                }
            }
            var ev = AgentEvent(.claude, r.sessionId, r.cwd, kind)
            ev.title = r.name.isEmpty ? nil : r.name
            ev.pid = r.pid
            ev.host = r.host
            ev.remote = r.remote
            ev.open = true
            ev.hosted = r.hosted
            ev.live = false
            ev.trusted = true
            // first sight reports when the status last changed, so old news does not alert
            if prev == nil, let t = r.statusUpdatedAt, t > 0, t <= now { ev.at = t } else { ev.at = now }
            seen[r.sessionId] = cur
            out.append(ev)
        }
        for sid in Array(seen.keys) where !present.contains(sid) {
            seen[sid] = nil
            out.append(AgentEvent(.claude, sid, "", .sessionEnded))
        }
        return out
    }
}
