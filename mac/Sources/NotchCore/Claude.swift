import Foundation

/// Claude Code: hook payloads, transcripts, subagent files and the registry of open sessions.
/// Hook names and fields follow https://code.claude.com/docs/en/hooks.
public enum Claude {
    /// The hook events AgentNotch registers for.
    public static let hookEvents = [
        "SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure", "PermissionRequest",
        "Notification", "Stop", "StopFailure", "SubagentStart", "SubagentStop", "PreCompact", "SessionEnd",
    ]

    public static var configDir: URL {
        if let d = ProcessInfo.processInfo.environment["CLAUDE_CONFIG_DIR"], !d.isEmpty { return URL(fileURLWithPath: d) }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".claude")
    }

    // MARK: the question a turn ended with

    private static let lock = NSLock()
    private static var pendingAsk: [String: String] = [:]

    /// Call for every tool use of a session: remembers a wrap-up question, if this is one.
    public static func noteTool(_ sessionId: String, _ tool: String, _ input: JSON?) {
        guard tool.hasSuffix("wrap_up") else { return }
        let ask = (input?.str("ask") ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        lock.lock()
        defer { lock.unlock() }
        if ask.isEmpty { pendingAsk[sessionId] = nil } else { pendingAsk[sessionId] = clip(ask, 80) }
    }

    public static func clearAsk(_ sessionId: String) {
        lock.lock()
        defer { lock.unlock() }
        pendingAsk[sessionId] = nil
    }

    /// What the end of a turn means: waiting on a question, or finished.
    public static func turnEnd(_ sessionId: String) -> EventKind {
        lock.lock()
        defer { lock.unlock() }
        if let q = pendingAsk[sessionId] { return .needsInput(reason: q) }
        return .done
    }

    // MARK: tool steps

    /// Short description of a tool call, e.g. "Bash: npm test" or "Edit main.rs".
    public static func toolStep(_ name: String, _ input: JSON?) -> String {
        func arg(_ k: String) -> String { input?.str(k) ?? "" }
        let out: String
        switch name {
        case "Bash", "PowerShell": out = "\(name): \(arg("command"))"
        case "Read", "Edit", "Write", "MultiEdit", "NotebookEdit":
            let p = arg("file_path").isEmpty ? arg("notebook_path") : arg("file_path")
            out = "\(name) \(baseName(p))"
        case "Grep", "Glob": out = "\(name) \(arg("pattern"))"
        case "Task", "Agent": out = "Agent: \(arg("description"))"
        case "WebFetch": out = "Fetch \(arg("url"))"
        case "WebSearch": out = "Search: \(arg("query"))"
        case "TodoWrite", "TaskCreate", "TaskUpdate": out = "Updating task list"
        default:
            if name.hasPrefix("mcp__") {
                let parts = name.components(separatedBy: "__")
                let server = parts.count > 1 ? parts[1] : ""
                let tool = parts.count > 2 ? parts[2...].joined(separator: "__") : ""
                out = "\(tool) (\(server))"
            } else {
                out = name
            }
        }
        return clip(out.trimmingCharacters(in: .whitespaces), 70)
    }

    // MARK: hooks

    /// Translate one hook payload into events.
    public static func onPush(_ p: JSON, pid: Int32?) -> [AgentEvent] {
        let sid = p.str("session_id")
        if sid.isEmpty { return [] }
        let tool = p.str("tool_name")
        let input = p["tool_input"]
        let event = p.str("hook_event_name")
        var soft = false

        // Hooks from a subagent carry agent_id. Their tool calls must not change the main
        // session's state: a background agent can still be working after the main turn ended.
        let agentId = p.str("agent_id")
        if !agentId.isEmpty || event == "SubagentStart" || event == "SubagentStop" {
            var sub: SubEvent?
            switch event {
            case "SubagentStart": sub = SubEvent(id: agentId, kind: p.str("agent_type"))
            case "SubagentStop": sub = SubEvent(id: agentId, kind: p.str("agent_type"), ended: true)
            case "PreToolUse": sub = SubEvent(id: agentId, kind: p.str("agent_type"), step: toolStep(tool, input))
            case "PostToolUse", "PostToolUseFailure", "Stop", "PreCompact", "UserPromptSubmit": return []
            default: break // a permission prompt from a subagent still needs the user
            }
            if let sub {
                if sub.id.isEmpty { return [] }
                var ev = AgentEvent(.claude, sid, "", .info)
                ev.sub = sub
                return [ev]
            }
        }

        let kind: EventKind
        switch event {
        case "SessionStart": kind = .sessionStarted
        case "UserPromptSubmit":
            clearAsk(sid)
            kind = .working(step: "Thinking…")
        case "PreToolUse":
            if tool.hasSuffix("wrap_up") {
                noteTool(sid, tool, input)
                kind = .working(step: "Wrapping up")
            } else if tool == "AskUserQuestion" {
                kind = .needsInput(reason: "Has a question for you")
            } else if tool == "ExitPlanMode" {
                kind = .needsInput(reason: "Plan ready for review")
            } else {
                kind = .working(step: toolStep(tool, input))
            }
        case "PostToolUse": kind = .working(step: "")
        case "PostToolUseFailure": kind = .working(step: "\(tool) failed")
        case "PermissionRequest": kind = .needsInput(reason: "Permission: \(toolStep(tool, input))")
        case "Notification":
            switch p.str("notification_type") {
            case "permission_prompt", "elicitation_dialog": kind = .needsInput(reason: clip(p.str("message"), 80))
            case "idle_prompt":
                soft = true
                kind = .needsInput(reason: "Waiting for your input")
            default: return []
            }
        case "Stop": kind = turnEnd(sid)
        case "StopFailure":
            let e = p.str("error")
            kind = .error(message: clip(e.isEmpty ? p.str("last_assistant_message") : e, 80))
        case "PreCompact": kind = .working(step: "Compacting context")
        case "SessionEnd": kind = .sessionEnded
        default: return []
        }
        var ev = AgentEvent(.claude, sid, p.str("cwd"), kind)
        ev.soft = soft
        ev.pid = pid
        let title = p.str("session_title")
        if !title.isEmpty { ev.title = title }
        return [ev]
    }

    // MARK: transcripts

    public enum Mode {
        /// No hooks for this session: every state line counts.
        case full
        /// Hooks are live: only titles, the Remote Control marker and the end of a turn.
        case assist
    }

    private static func interrupted(_ content: JSON?) -> Bool {
        func starts(_ t: String) -> Bool { t.trimmingCharacters(in: .whitespaces).hasPrefix("[Request interrupted") }
        if let s = content?.string { return starts(s) }
        if let a = content?.array { return a.contains { $0.str("type") == "text" && starts($0.str("text")) } }
        return false
    }

    /// Turn one transcript line into an event.
    public static func parseLine(_ line: String, mode: Mode) -> AgentEvent? {
        // cheap pre-filter: most lines (request records, attachments) are not state
        let info = line.contains("\"type\":\"custom-title\"") || line.contains("\"type\":\"bridge-session\"")
        let state = line.contains("\"type\":\"assistant\"") || line.contains("\"type\":\"user\"") || line.contains("\"stop_hook_summary\"")
        guard info || state, let l = JSON.parse(line) else { return nil }
        let sid = l.str("sessionId")
        if sid.isEmpty { return nil }
        let type = l.str("type")
        if type == "custom-title" || type == "bridge-session" {
            var ev = AgentEvent(.claude, sid, "", .info)
            let t = l.str("customTitle")
            ev.title = t.isEmpty ? nil : t
            ev.remote = type == "bridge-session"
            ev.live = false
            return ev
        }
        if l["isSidechain"]?.bool == true { return nil } // subagent chatter
        var turnEnd = false
        let kind: EventKind
        let msg = l["message"]
        switch type {
        case "system":
            guard l.str("subtype") == "stop_hook_summary" else { return nil }
            turnEnd = true
            kind = Claude.turnEnd(sid)
        case "assistant":
            let tool = msg?["content"]?.array?.last(where: { $0.str("type") == "tool_use" })
            let stop = msg?.str("stop_reason") ?? ""
            if let tool {
                if mode == .assist { return nil }
                let name = tool.str("name")
                noteTool(sid, name, tool["input"])
                switch name {
                case "AskUserQuestion": kind = .needsInput(reason: "Has a question for you")
                case "ExitPlanMode": kind = .needsInput(reason: "Plan ready for review")
                default: kind = .working(step: toolStep(name, tool["input"]))
                }
            } else if stop == "end_turn" || stop == "stop_sequence" {
                turnEnd = true
                kind = Claude.turnEnd(sid)
            } else if mode == .assist {
                return nil
            } else if stop == "refusal" || stop == "max_tokens" {
                kind = .error(message: stop)
            } else {
                kind = .working(step: "")
            }
        case "user":
            let content = msg?["content"]
            if interrupted(content) {
                turnEnd = true
                kind = .done
            } else if mode == .assist {
                return nil
            } else {
                let isPrompt: Bool
                if content?.string != nil {
                    isPrompt = true
                } else if let a = content?.array {
                    isPrompt = !a.contains { $0.str("type") == "tool_result" }
                } else {
                    isPrompt = false
                }
                if isPrompt { clearAsk(sid) }
                kind = .working(step: isPrompt ? "Thinking…" : "")
            }
        default: return nil
        }
        var ev = AgentEvent(.claude, sid, l.str("cwd"), kind)
        ev.live = false
        ev.turnEnd = turnEnd
        if let t = parseTimestamp(l.str("timestamp")) { ev.at = t }
        return ev
    }

    /// projects/<project>/<session>/subagents/agent-<id>.jsonl (or .meta.json)
    /// -> (session id, agent id, is the meta file)
    public static func subFile(root: URL, _ p: URL) -> (session: String, agent: String, meta: Bool)? {
        let name = p.lastPathComponent
        guard name.hasPrefix("agent-") else { return nil }
        let rest = String(name.dropFirst(6))
        let id: String
        let meta: Bool
        if rest.hasSuffix(".meta.json") {
            id = String(rest.dropLast(10))
            meta = true
        } else if rest.hasSuffix(".jsonl") {
            id = String(rest.dropLast(6))
            meta = false
        } else {
            return nil
        }
        let dir = p.deletingLastPathComponent()
        guard dir.lastPathComponent == "subagents" else { return nil }
        let session = dir.deletingLastPathComponent()
        guard session.deletingLastPathComponent().deletingLastPathComponent().standardizedFileURL.path == root.standardizedFileURL.path else { return nil }
        return (session.lastPathComponent, id, meta)
    }

    /// The label and finished flag from a subagent's meta file.
    public static func subMeta(_ data: Data) -> (kind: String, finished: Bool)? {
        guard let m = JSON.parse(data) else { return nil }
        let d = m.str("description")
        let t = m.str("agentType")
        let label = d.isEmpty ? t : (t.isEmpty ? d : "\(d) (\(t))")
        return (clip(label, 60), m["finished"]?.bool == true)
    }

    /// Only main session files: projects/<project>/<session>.jsonl
    public static func isSessionFile(root: URL, _ p: URL) -> Bool {
        p.pathExtension == "jsonl" && p.deletingLastPathComponent().deletingLastPathComponent().standardizedFileURL.path == root.standardizedFileURL.path
    }
}

/// Parse an ISO 8601 timestamp ("2026-11-14T10:20:30.123Z" or with an offset) to unix seconds.
public func parseTimestamp(_ s: String) -> UInt64? {
    guard s.count >= 19 else { return nil }
    let f = ISO8601DateFormatter()
    f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let d = f.date(from: s) { return UInt64(max(0, d.timeIntervalSince1970)) }
    f.formatOptions = [.withInternetDateTime]
    if let d = f.date(from: s) { return UInt64(max(0, d.timeIntervalSince1970)) }
    return nil
}
