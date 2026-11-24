import AppKit
import CoreText
import NotchCore
import ServiceManagement
import SwiftUI

/// A borderless panel that takes clicks without activating the app or stealing focus.
final class NotchPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// Hosts the SwiftUI notch and reports when the cursor comes over it.
final class NotchHostView<Content: View>: NSHostingView<Content> {
    var onEnter: (() -> Void)?
    private var area: NSTrackingArea?

    override func acceptsFirstMouse(for _: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let a = area { removeTrackingArea(a) }
        let a = NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self, userInfo: nil)
        addTrackingArea(a)
        area = a
    }

    override func mouseEntered(with _: NSEvent) { onEnter?() }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    let state = AppState()
    private var panel: NotchPanel!
    private var sources: Sources!
    private let notifier = Notifier()
    private var statusItem: NSStatusItem?
    private var hoverTimer: Timer?
    private var tickTimer: Timer?
    private var awayTicks = 0
    private var holdUntil: Date?
    private var userHidden = false
    private var panelHeight: CGFloat = 320
    private var remoteDeadline: Date?
    var demo = false
    var startExpanded = false

    // MARK: launch

    func applicationDidFinishLaunching(_: Notification) {
        NSApp.setActivationPolicy(.accessory)
        registerFont()
        readScreenMetrics()
        state.isDemo = demo
        if demo { Demo.fill(state) }

        let host = NotchHostView(rootView: NotchView(state: state, actions: actions))
        host.onEnter = { [weak self] in self?.hoverBegan() }
        panel = NotchPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.level = .statusBar
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.isMovable = false
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.becomesKeyOnlyIfNeeded = true
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        panel.contentView = host
        layoutWindow()
        panel.orderFrontRegardless()

        state.onLayoutChange = { [weak self] in self?.contentChanged() }
        state.onAlert = { [weak self] in self?.alert($0) }
        buildStatusItem()

        NotificationCenter.default.addObserver(forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main) { [weak self] _ in
            self?.readScreenMetrics()
            self?.layoutWindow()
        }
        Timer.scheduledTimer(withTimeInterval: 15, repeats: true) { [weak self] _ in self?.housekeeping() }

        if !demo {
            sources = Sources(state: state)
            sources.onControl = { [weak self] in self?.control($0) }
            sources.start()
            notifier.onOpen = { [weak self] sid in self?.openSession(id: sid) }
            notifier.onDecide = { [weak self] sid, allow in
                if self?.state.decide(sid, allow: allow) == false { self?.openSession(id: sid) }
            }
            notifier.setup()
            state.hooksInstalled = Hooks.installed(Hooks.claudeSettings)
            if state.config.keepAwake { KeepAwake.hold(true) }
            refreshToggles(scan: true)
            Hotkey.action = { [weak self] in self?.toggleHidden() }
            Hotkey.set(state.config.hotkey)
            state.refreshUsage(force: true)
            Timer.scheduledTimer(withTimeInterval: 300, repeats: true) { [weak self] _ in self?.state.refreshUsage(force: false) }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in self?.offerHooks() }
        }
        if startExpanded { setExpanded(true, pinned: true) }
        logLine("notch started")
    }

    func applicationWillTerminate(_: Notification) {
        KeepAwake.hold(false)
        Travel.closeReminder()
        state.saveConfig()
        unlink(Paths.socket)
    }

    private func registerFont() {
        var url = Bundle.main.url(forResource: "DMSans", withExtension: "ttf")
        if url == nil, let i = CommandLine.arguments.firstIndex(of: "--font"), i + 1 < CommandLine.arguments.count {
            url = URL(fileURLWithPath: CommandLine.arguments[i + 1])
        }
        if let url { CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil) }
    }

    // MARK: geometry

    private var screen: NSScreen { NSScreen.screens.first ?? NSScreen.main! }

    /// On a Mac with a camera housing the collapsed notch wraps around it; elsewhere it is
    /// a small tab hanging from the top of the screen.
    private func readScreenMetrics() {
        let s = screen
        if s.safeAreaInsets.top > 0, let l = s.auxiliaryTopLeftArea, let r = s.auxiliaryTopRightArea {
            state.hardwareNotchWidth = max(0, s.frame.width - l.width - r.width)
            state.collapsedHeight = s.safeAreaInsets.top
        } else {
            state.hardwareNotchWidth = 0
            state.collapsedHeight = 30
        }
    }

    private var collapsedSize: CGSize {
        CGSize(width: Metrics.wing * 2 + state.hardwareNotchWidth + Metrics.ear * 2, height: state.collapsedHeight)
    }

    private func measurePanel() {
        let probe = NSHostingController(rootView: PanelView(state: state, actions: Actions()).environment(\.colorScheme, .dark))
        let size = probe.sizeThatFits(in: CGSize(width: Metrics.panelWidth, height: 2000))
        panelHeight = min(max(size.height, 120), screen.frame.height - 80)
    }

    private func layoutWindow() {
        let s = screen
        let size: CGSize
        if state.expanded {
            measurePanel()
            size = CGSize(width: Metrics.panelWidth + Metrics.ear * 2, height: panelHeight)
        } else {
            size = collapsedSize
        }
        panel.setFrame(NSRect(x: (s.frame.midX - size.width / 2).rounded(), y: s.frame.maxY - size.height, width: size.width, height: size.height), display: true)
    }

    private var contentTimer: Timer?
    /// Sessions changed: the open panel may need a different height (coalesced).
    private func contentChanged() {
        guard state.expanded, contentTimer == nil else { return }
        contentTimer = Timer.scheduledTimer(withTimeInterval: 0.12, repeats: false) { [weak self] _ in
            self?.contentTimer = nil
            if self?.state.expanded == true { self?.layoutWindow() }
        }
    }

    // MARK: open / close

    private var pinned = false

    private func setExpanded(_ on: Bool, pinned pin: Bool = false) {
        pinned = on && pin
        if on == state.expanded { return }
        if on {
            state.expanded = true
            layoutWindow() // grow the window first, then the shape animates into it
            state.refreshUsage(force: true)
            refreshToggles(scan: true)
            awayTicks = 0
            hoverTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: true) { [weak self] _ in self?.hoverWatch() }
            tickTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.state.tick += 1 }
        } else {
            state.expanded = false
            state.showSettings = false
            holdUntil = nil
            hoverTimer?.invalidate()
            tickTimer?.invalidate()
            hoverTimer = nil
            tickTimer = nil
            // shrink the window once the collapse animation has played
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [weak self] in
                if self?.state.expanded == false { self?.layoutWindow() }
            }
        }
    }

    private func hoverBegan() {
        guard state.config.hoverExpand, !state.expanded else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
            guard let self, !self.state.expanded, self.cursorOver(margin: 2) else { return }
            self.setExpanded(true)
        }
    }

    private func cursorOver(margin: CGFloat) -> Bool {
        panel.frame.insetBy(dx: -margin, dy: -margin).contains(NSEvent.mouseLocation)
    }

    /// Runs a few times a second while open; closes once the cursor has been away a moment.
    /// Asked directly instead of trusting enter / exit events.
    private func hoverWatch() {
        guard state.expanded else { return }
        if cursorOver(margin: 8) {
            awayTicks = 0
            holdUntil = nil
            if pinned, state.config.hoverExpand { pinned = false }
            return
        }
        if pinned || (holdUntil.map { Date() < $0 } ?? false) { return }
        awayTicks += 1
        if awayTicks >= 2 { setExpanded(false) }
    }

    /// Open for a few seconds to show an event.
    private func peek() {
        guard state.config.autoExpand, !userHidden else { return }
        if !cursorOver(margin: 8) { holdUntil = Date().addingTimeInterval(4.5) }
        setExpanded(true)
    }

    private func toggleHidden() {
        userHidden.toggle()
        if userHidden {
            setExpanded(false)
            panel.orderOut(nil)
        } else {
            panel.orderFrontRegardless()
        }
    }

    private func summon() {
        userHidden = false
        panel.orderFrontRegardless()
        holdUntil = Date().addingTimeInterval(5)
        setExpanded(true)
    }

    // MARK: actions

    private var actions: Actions {
        Actions(
            open: { [weak self] s in self?.openSession(id: s.id) },
            dismiss: { [weak self] id in self?.state.dismiss(id) },
            decide: { [weak self] id, allow in self?.state.decide(id, allow: allow) },
            toggleRemote: { [weak self] in self?.toggleRemote() },
            toggleAwake: { [weak self] in self?.toggleAwake() },
            toggleTravel: { [weak self] in self?.toggleTravel() },
            toggleHooks: { [weak self] in self?.toggleHooks() },
            refreshUsage: { [weak self] in self?.state.refreshUsage(force: true) },
            setLoginItem: { on in
                if on { try? SMAppService.mainApp.register() } else { try? SMAppService.mainApp.unregister() }
            },
            quit: { NSApp.terminate(nil) }
        )
    }

    private func openSession(id: String) {
        if let s = state.store.sessions.first(where: { $0.id == id }) { Focus.open(s) } else { summon() }
    }

    private func alert(_ a: NotchCore.Alert) {
        let c = state.config
        let (title, body): (String, String)
        var canDecide = false
        switch a {
        case let .needsInput(name, agent, reason, id):
            (title, body) = ("\(name) needs input", "\(agent.label) · \(reason)")
            canDecide = c.approveFromNotch && reason.hasPrefix("Permission") && state.store.sessions.contains { $0.id == id && ($0.hosted || c.approveInTerminal) }
        case let .done(name, agent, _): (title, body) = ("\(name) finished", agent.label)
        case let .error(name, agent, message, _): (title, body) = ("\(name) hit an error", "\(agent.label) · \(message)")
        }
        if c.notifications, !demo { notifier.show(title: title, body: body, sessionId: a.sessionId, canDecide: canDecide, sound: c.sound) }
        peek()
    }

    // MARK: toggles

    /// Work out what Remote Control and Keep Awake really are right now.
    private func refreshToggles(scan: Bool) {
        if demo { return }
        state.awake = KeepAwake.held
        if RemoteControl.ourPid() != nil {
            state.remote = .ours
            remoteDeadline = nil
        } else if let d = remoteDeadline, Date() < d {
            state.remote = .starting
        } else {
            remoteDeadline = nil
            let viaSession = state.store.sessions.contains { $0.remote && $0.active }
            if scan {
                DispatchQueue.global(qos: .utility).async {
                    let ext = RemoteControl.externalRunning()
                    DispatchQueue.main.async { [weak self] in
                        guard let self, RemoteControl.ourPid() == nil, self.remoteDeadline == nil else { return }
                        self.state.remote = ext || viaSession ? .external : .off
                    }
                }
            } else if state.remote != .external || !viaSession {
                state.remote = viaSession ? .external : (state.remote == .external ? .external : .off)
            }
        }
    }

    private func remoteDir() -> String {
        if !state.store.lastDir.isEmpty { return state.store.lastDir }
        return state.store.sessions.first(where: { !$0.dir.isEmpty })?.dir ?? FileManager.default.homeDirectoryForCurrentUser.path
    }

    private func toggleRemote() {
        switch state.remote {
        case .ours:
            RemoteControl.stop()
            state.remote = .off
        case .off:
            if RemoteControl.start(dir: remoteDir()) {
                remoteDeadline = Date().addingTimeInterval(25)
                state.remote = .starting
                logLine("remote control: starting")
            }
        case .starting, .external: break // nothing of ours to stop
        }
    }

    private func setAwake(_ on: Bool) {
        KeepAwake.hold(on)
        // lid-close sleep is a system-wide switch behind an administrator password
        DispatchQueue.global(qos: .userInitiated).async {
            if KeepAwake.lidSleepDisabled() != on { KeepAwake.setLidSleepDisabled(on) }
        }
        state.awake = KeepAwake.held
        state.config.keepAwake = on
        state.saveConfig()
        logLine("keep awake: \(on ? "on" : "off")")
    }

    private func toggleAwake() { setAwake(!state.awake) }

    private func toggleTravel() {
        let on = !state.config.travelMode
        state.config.travelMode = on
        if on {
            if !state.awake { setAwake(true) }
            if state.remote == .off { toggleRemote() }
            Travel.showReminder(on: screen)
        } else {
            Travel.closeReminder()
            if state.awake { setAwake(false) }
            if state.remote == .ours { toggleRemote() }
        }
        state.saveConfig()
        logLine("travel mode: \(on ? "on" : "off")")
    }

    private func toggleHooks() {
        do {
            if state.hooksInstalled { try Hooks.uninstall(Hooks.claudeSettings) } else { try Hooks.install(Hooks.claudeTarget(exe: CLI.executable)) }
        } catch {
            let a = NSAlert()
            a.messageText = "Could not change the Claude Code hooks"
            a.informativeText = "\(error)"
            a.runModal()
        }
        state.hooksInstalled = Hooks.installed(Hooks.claudeSettings)
    }

    /// First run: offer to install the hooks. Asked once.
    private func offerHooks() {
        guard !state.config.hooksPrompted else { return }
        state.config.hooksPrompted = true
        state.saveConfig()
        if state.hooksInstalled { return }
        let a = NSAlert()
        a.messageText = "Install Claude Code hooks so sessions update live?"
        a.informativeText = "This adds entries to ~/.claude/settings.json after your existing hooks. A backup is saved first, and you can remove them again in Notch's settings."
        a.addButton(withTitle: "Install")
        a.addButton(withTitle: "Not now")
        NSApp.activate(ignoringOtherApps: true)
        if a.runModal() == .alertFirstButtonReturn { toggleHooks() }
    }

    // MARK: control messages, housekeeping, menu

    private func control(_ p: JSON) {
        if p["quit"]?.bool == true { NSApp.terminate(nil) }
        if p["summon"]?.bool == true { summon() }
        let sid = p.str("decide")
        if !sid.isEmpty, !state.decide(sid, allow: p["allow"]?.bool == true) { openSession(id: sid) }
    }

    private func housekeeping() {
        state.housekeeping()
        sources?.rescan()
        refreshToggles(scan: true)
    }

    private func buildStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item.button?.image = NSImage(systemSymbolName: "rectangle.topthird.inset.filled", accessibilityDescription: "Notch")
        let menu = NSMenu()
        menu.addItem(withTitle: "Show / Hide", action: #selector(menuToggle), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Clear finished sessions", action: #selector(menuClear), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Travel mode", action: #selector(menuTravel), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Start at login", action: #selector(menuLogin), keyEquivalent: "").target = self
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit Notch", action: #selector(menuQuit), keyEquivalent: "q").target = self
        item.menu = menu
        statusItem = item
    }

    @objc private func menuToggle() { toggleHidden() }
    @objc private func menuClear() {
        state.store.clearFinished()
        contentChanged()
    }

    @objc private func menuTravel() { toggleTravel() }
    @objc private func menuLogin() {
        let on = SMAppService.mainApp.status != .enabled
        if on { try? SMAppService.mainApp.register() } else { try? SMAppService.mainApp.unregister() }
    }

    @objc private func menuQuit() { NSApp.terminate(nil) }
}

/// Made-up sessions for screenshots and demos.
enum Demo {
    static func fill(_ state: AppState) {
        let now = nowUnix()
        func ev(_ agent: Agent, _ id: String, _ dir: String, _ kind: EventKind, _ ago: UInt64) -> AgentEvent {
            var e = AgentEvent(agent, id, dir, kind)
            e.at = now - ago
            return e
        }
        var events = [
            ev(.claude, "d1", "/Users/you/dev/webshop", .working(step: "start"), 754),
            ev(.claude, "d1", "/Users/you/dev/webshop", .working(step: "Bash: npx vitest run"), 3),
            ev(.claude, "d2", "/Users/you/dev/notch", .working(step: "start"), 190),
            ev(.claude, "d2", "/Users/you/dev/notch", .needsInput(reason: "Permission: Bash(swift build)"), 12),
            ev(.codex, "d3", "/Users/you/dev/api-server", .working(step: "apply_patch src/routes.ts"), 61),
            ev(.t3code, "d4", "/Users/you/dev/landing", .working(step: "start"), 2400),
            ev(.t3code, "d4", "/Users/you/dev/landing", .done, 95),
        ]
        for (id, kind, step) in [("s1", "Explore", "Grep \"checkout\" in src"), ("s2", "general-purpose", "Bash: npx tsc --noEmit")] {
            var e = AgentEvent(.claude, "d1", "", .info)
            e.sub = SubEvent(id: id, kind: kind, step: step)
            events.append(e)
        }
        for e in events { state.store.apply(e) }
        state.config.showUsage = true
        var u = Usage()
        u.fiveHour = UsageWindow(pct: 72, resetsAt: now + 2 * 3600 + 13 * 60)
        u.week = UsageWindow(pct: 34, resetsAt: now + 3 * 86400 + 5 * 3600)
        u.fetchedAt = now
        state.usage = u
    }
}

@MainActor
enum AppMain {
    static func run(_ args: [String]) {
        func after(_ flag: String) -> String? {
            guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
            return args[i + 1]
        }
        if let out = after("--snapshot") {
            snapshot(to: out, expanded: args.contains("--expanded"), settings: args.contains("--settings"), font: after("--font"))
            return
        }
        // already running: ask that one to show itself instead of starting twice
        if !args.contains("--demo"), CLI.control(.object([.init("summon", .bool(true))])) { return }
        let app = NSApplication.shared
        let delegate = AppDelegate()
        delegate.demo = args.contains("--demo")
        delegate.startExpanded = args.contains("--expanded")
        app.delegate = delegate
        app.run()
    }

    /// Render the notch with demo data to a PNG, without showing a window.
    static func snapshot(to path: String, expanded: Bool, settings: Bool, font: String?) {
        if let f = font ?? Bundle.main.url(forResource: "DMSans", withExtension: "ttf")?.path {
            CTFontManagerRegisterFontsForURL(URL(fileURLWithPath: f) as CFURL, .process, nil)
        }
        let state = AppState()
        state.isDemo = true
        Demo.fill(state)
        state.expanded = expanded || settings
        state.showSettings = settings
        let view = NotchView(state: state, actions: Actions())
            .frame(width: 480, height: state.expanded ? 520 : 90)
            .background(LinearGradient(colors: [Color(red: 0.38, green: 0.44, blue: 0.53), Color(red: 0.56, green: 0.59, blue: 0.67)], startPoint: .top, endPoint: .bottom))
        let renderer = ImageRenderer(content: view)
        renderer.scale = 2
        guard let image = renderer.nsImage, let tiff = image.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff), let png = rep.representation(using: .png, properties: [:]) else {
            print("snapshot failed")
            exit(1)
        }
        do {
            try png.write(to: URL(fileURLWithPath: path))
            print("wrote \(path)")
        } catch {
            print("snapshot failed: \(error)")
            exit(1)
        }
    }
}
