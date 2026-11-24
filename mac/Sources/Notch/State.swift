import AppKit
import Foundation
import NotchCore

/// Settings, stored as JSON in ~/Library/Application Support/Notch/config.json.
struct Config: Codable {
    var hoverExpand = true
    var hooksPrompted = false
    var keepAwake = false
    var travelMode = false
    /// Off by default: reading the sign-in makes macOS ask for Keychain access.
    var showUsage = false
    var notifications = true
    var sound = true
    var autoExpand = true
    var approveFromNotch = true
    var approveInTerminal = false
    var hotkey = true
    var keepFinishedMinutes = 2
    var adapters = ["claude": true, "codex": true, "t3code": true]

    static func load() -> Config {
        guard let d = try? Data(contentsOf: Paths.config), let c = try? JSONDecoder().decode(Config.self, from: d) else { return Config() }
        return c
    }

    func save() {
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        if let d = try? enc.encode(self) { try? d.write(to: Paths.config, options: .atomic) }
    }

    // tolerate files written by older or newer builds
    init() {}
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func get<T: Decodable>(_ k: CodingKeys, _ d: T) -> T { (try? c.decode(T.self, forKey: k)) ?? d }
        let d = Config()
        hoverExpand = get(.hoverExpand, d.hoverExpand)
        hooksPrompted = get(.hooksPrompted, d.hooksPrompted)
        keepAwake = get(.keepAwake, d.keepAwake)
        travelMode = get(.travelMode, d.travelMode)
        showUsage = get(.showUsage, d.showUsage)
        notifications = get(.notifications, d.notifications)
        sound = get(.sound, d.sound)
        autoExpand = get(.autoExpand, d.autoExpand)
        approveFromNotch = get(.approveFromNotch, d.approveFromNotch)
        approveInTerminal = get(.approveInTerminal, d.approveInTerminal)
        hotkey = get(.hotkey, d.hotkey)
        keepFinishedMinutes = get(.keepFinishedMinutes, d.keepFinishedMinutes)
        adapters = get(.adapters, d.adapters)
    }
}

enum RemoteState {
    case off, starting, ours, external
}

/// Everything the views show. Only touched on the main thread.
final class AppState: ObservableObject {
    @Published var store = Store()
    @Published var usage = Usage()
    @Published var usageLoading = false
    @Published var expanded = false
    @Published var showSettings = false
    @Published var remote = RemoteState.off
    @Published var awake = false
    @Published var hooksInstalled = false
    @Published var config = Config.load()
    /// Bumps once a second while the panel is open, so elapsed times stay current.
    @Published var tick = 0
    /// True on screens with a camera housing: the collapsed notch wraps around it.
    @Published var hardwareNotchWidth: CGFloat = 0
    @Published var collapsedHeight: CGFloat = 32

    /// Permission hooks waiting for Approve / Deny, by token.
    private var replies: [Int: IPC.Reply] = [:]
    private var nextToken = 1
    private var usageNextTry: UInt64 = 0
    var onAlert: ((NotchCore.Alert) -> Void)?
    var onLayoutChange: (() -> Void)?
    var isDemo = false

    func saveConfig() { config.save() }

    func enabled(_ agent: Agent) -> Bool { config.adapters[agent.rawValue] ?? true }

    // MARK: events

    func apply(_ events: [AgentEvent]) {
        var alerts = [NotchCore.Alert]()
        for ev in events where enabled(ev.agent) {
            if ev.live { LiveSessions.mark(ev.sessionId) }
            if let a = store.apply(ev) { alerts.append(a) }
        }
        releaseAnsweredRequests()
        alerts.forEach { onAlert?($0) }
        onLayoutChange?()
    }

    /// A permission hook is waiting. Offer Approve / Deny when this session's own prompt
    /// stays available alongside the hook (desktop app and SDK hosts); otherwise let the
    /// hook go at once so the agent asks as usual.
    func permissionRequest(sessionId: String, reply: IPC.Reply) {
        guard config.approveFromNotch, let s = store.sessions.first(where: { $0.id == sessionId }), s.hosted || config.approveInTerminal || isDemo else {
            reply.send("pass")
            return
        }
        if let old = s.pending { replies.removeValue(forKey: old)?.send("pass") }
        let token = nextToken
        nextToken += 1
        replies[token] = reply
        store.setPending(sessionId, token)
        onLayoutChange?()
    }

    /// Returns false when nothing was waiting (answered elsewhere).
    @discardableResult
    func decide(_ sessionId: String, allow: Bool) -> Bool {
        guard let s = store.sessions.first(where: { $0.id == sessionId }), let token = s.pending, let reply = replies.removeValue(forKey: token) else { return false }
        reply.send(allow ? "allow" : "deny")
        store.setPending(sessionId, nil)
        store.setStep(sessionId, allow ? "Approved" : "Denied")
        logLine("permission: \(allow ? "approved" : "denied") from the notch")
        onLayoutChange?()
        return true
    }

    /// A request that is no longer waiting (answered in the agent, or the turn moved on).
    private func releaseAnsweredRequests() {
        for s in store.sessions {
            if let token = s.pending, s.state != .needsInput {
                replies.removeValue(forKey: token)?.send("pass")
                store.setPending(s.id, nil)
            }
        }
    }

    func dismiss(_ sessionId: String) {
        if let token = store.sessions.first(where: { $0.id == sessionId })?.pending { replies.removeValue(forKey: token)?.send("pass") }
        store.dismiss(sessionId)
        onLayoutChange?()
    }

    func housekeeping() {
        let keep = UInt64(max(0, config.keepFinishedMinutes) * 60)
        if store.expire(keepSecs: keep, staleSecs: 600, alive: CLI.alive) { onLayoutChange?() }
    }

    // MARK: usage

    func refreshUsage(force: Bool) {
        guard config.showUsage, !usageLoading, !isDemo else { return }
        let now = nowUnix()
        if now < usageNextTry { return }
        usageNextTry = now + (force ? 60 : 280)
        usageLoading = true
        DispatchQueue.global(qos: .utility).async {
            let new = UsageAPI.fetch()
            DispatchQueue.main.async {
                self.usageLoading = false
                if new.note.contains("rate limiting") { self.usageNextTry = nowUnix() + 900 }
                // real numbers stay on screen until newer real numbers arrive
                if new.hasNumbers || !self.usage.hasNumbers {
                    self.usage = new
                } else {
                    self.usage.note = new.note
                }
            }
        }
    }
}

/// Sessions that have live hook events: their transcripts only assist.
enum LiveSessions {
    private static let lock = NSLock()
    private static var ids = Set<String>()
    static func mark(_ id: String) {
        lock.lock()
        ids.insert(id)
        lock.unlock()
    }

    static func contains(_ id: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return ids.contains(id)
    }
}

/// Watches a folder with FSEvents and reports the paths that changed.
final class DirWatcher {
    private var stream: FSEventStreamRef?
    private let callback: ([String]) -> Void

    init?(path: String, latency: CFTimeInterval, queue: DispatchQueue, _ callback: @escaping ([String]) -> Void) {
        self.callback = callback
        guard FileManager.default.fileExists(atPath: path) else { return nil }
        var ctx = FSEventStreamContext(version: 0, info: Unmanaged.passUnretained(self).toOpaque(), retain: nil, release: nil, copyDescription: nil)
        let cb: FSEventStreamCallback = { _, info, _, paths, _, _ in
            guard let info else { return }
            let me = Unmanaged<DirWatcher>.fromOpaque(info).takeUnretainedValue()
            let list = unsafeBitCast(paths, to: NSArray.self) as? [String] ?? []
            me.callback(list)
        }
        let flags = UInt32(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagNoDefer)
        guard let s = FSEventStreamCreate(nil, cb, &ctx, [path] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), latency, flags) else { return nil }
        stream = s
        FSEventStreamSetDispatchQueue(s, queue)
        FSEventStreamStart(s)
    }

    deinit {
        if let s = stream {
            FSEventStreamStop(s)
            FSEventStreamInvalidate(s)
            FSEventStreamRelease(s)
        }
    }
}

/// The background sources: registry, transcripts and subagent files, T3 Code, and the socket.
final class Sources {
    private let state: AppState
    private let queue = DispatchQueue(label: "notch.sources")
    private var watchers: [DirWatcher] = []
    private let tailer = Tailer()
    private var registrySeen: [String: Registry.Seen] = [:]
    private var t3Seen: [String: String] = [:]
    private var t3First = true
    var onControl: ((JSON) -> Void)?

    init(state: AppState) { self.state = state }

    private func post(_ events: [AgentEvent]) {
        if events.isEmpty { return }
        DispatchQueue.main.async { self.state.apply(events) }
    }

    func start() {
        queue.async { [self] in
            scanRegistry()
            backfillSubagents()
            scanT3()
        }
        let projects = Claude.configDir.appendingPathComponent("projects")
        if let w = DirWatcher(path: Registry.dir.path, latency: 0.05, queue: queue, { [weak self] _ in self?.scanRegistry() }) { watchers.append(w) }
        if let w = DirWatcher(path: projects.path, latency: 0.12, queue: queue, { [weak self] paths in self?.transcriptsChanged(root: projects, paths) }) { watchers.append(w) }
        let t3dir = T3Code.database.deletingLastPathComponent()
        if let w = DirWatcher(path: t3dir.path, latency: 1.5, queue: queue, { [weak self] paths in
            if paths.contains(where: { URL(fileURLWithPath: $0).lastPathComponent.hasPrefix("statev2.sqlite") }) { self?.scanT3() }
        }) { watchers.append(w) }

        IPC.listen { [weak self] data, reply in
            self?.handle(data, reply)
        }
    }

    /// Called now and then: a process that died leaves its registry file behind.
    func rescan() { queue.async { self.scanRegistry() } }

    private func scanRegistry() {
        post(Registry.diff(seen: &registrySeen, records: Registry.readAll(alive: CLI.alive), now: nowUnix()))
    }

    private func scanT3() {
        guard let runs = T3Code.load() else { return }
        post(T3Code.diff(seen: &t3Seen, runs: runs, first: t3First, now: nowUnix()))
        t3First = false
    }

    private func transcriptsChanged(root: URL, _ paths: [String]) {
        var events = [AgentEvent]()
        for p in Set(paths) {
            let url = URL(fileURLWithPath: p)
            if Claude.isSessionFile(root: root, url) {
                let sid = url.deletingPathExtension().lastPathComponent
                let mode: Claude.Mode = LiveSessions.contains(sid) ? .assist : .full
                for line in tailer.readNew(url, firstTail: 256 * 1024, maxLine: 512 * 1024) {
                    if let ev = Claude.parseLine(line, mode: mode) { events.append(ev) }
                }
            } else if let ev = subagentEvent(root: root, url) {
                events.append(ev)
            }
        }
        post(events)
    }

    /// News about one subagent from its files: the meta file says what it is and whether it
    /// has finished; the newest tool call in its transcript says what it is doing.
    private func subagentEvent(root: URL, _ url: URL) -> AgentEvent? {
        guard let f = Claude.subFile(root: root, url) else { return nil }
        let dir = url.deletingLastPathComponent()
        let meta = (try? Data(contentsOf: dir.appendingPathComponent("agent-\(f.agent).meta.json"))).flatMap(Claude.subMeta)
        var step: String?
        if !f.meta {
            for line in tailer.readNew(url, firstTail: 48 * 1024, maxLine: 512 * 1024) where line.contains("\"tool_use\"") {
                if let tool = JSON.parse(line)?["message"]?["content"]?.array?.last(where: { $0.str("type") == "tool_use" }) {
                    step = Claude.toolStep(tool.str("name"), tool["input"])
                }
            }
        }
        if meta == nil, step == nil { return nil }
        var ev = AgentEvent(.claude, f.session, "", .info)
        ev.live = false
        ev.sub = SubEvent(id: f.agent, kind: meta?.kind ?? "", step: step, ended: meta?.finished ?? false)
        return ev
    }

    /// Subagents running right now: meta files touched in the last half hour, not finished.
    private func backfillSubagents() {
        let root = Claude.configDir.appendingPathComponent("projects")
        let metas = recentFiles(root, depth: 3, maxAge: 1800) { Claude.subFile(root: root, $0)?.meta == true }
        var events = [AgentEvent]()
        for m in metas {
            let transcript = URL(fileURLWithPath: m.path.replacingOccurrences(of: ".meta.json", with: ".jsonl"))
            if let ev = subagentEvent(root: root, transcript) ?? subagentEvent(root: root, m), ev.sub?.ended == false { events.append(ev) }
        }
        post(events)
    }

    private func handle(_ data: Data, _ reply: IPC.Reply) {
        guard let v = JSON.parse(data), let payload = v["p"] else {
            reply.send("pass")
            return
        }
        let agent = v.str("agent")
        if agent == "notch" {
            reply.send("ok")
            DispatchQueue.main.async { self.onControl?(payload) }
            return
        }
        let pid = v["pid"]?.double.map { Int32($0) }
        // Codex hooks run through a shell, so their parent pid is not the agent's
        let events = agent == "codex" ? Codex.onPush(payload, pid: nil) : Claude.onPush(payload, pid: pid)
        let wantsReply = v["reply"]?.bool == true
        let sid = payload.str("session_id")
        DispatchQueue.main.async {
            self.state.apply(events)
            if wantsReply {
                self.state.permissionRequest(sessionId: sid, reply: reply)
            } else {
                reply.send("")
            }
        }
    }
}
