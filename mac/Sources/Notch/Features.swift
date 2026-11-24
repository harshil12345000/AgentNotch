import AppKit
import Carbon.HIToolbox
import IOKit.pwr_mgt
import NotchCore
import SwiftUI
import UserNotifications

/// Run a tool and return its output (nil on failure or timeout).
func runTool(_ path: String, _ args: [String], timeout: TimeInterval = 10) -> String? {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: path)
    p.arguments = args
    let out = Pipe()
    p.standardOutput = out
    p.standardError = FileHandle.nullDevice
    p.standardInput = FileHandle.nullDevice
    guard (try? p.run()) != nil else { return nil }
    let deadline = Date().addingTimeInterval(timeout)
    while p.isRunning, Date() < deadline { Thread.sleep(forTimeInterval: 0.03) }
    if p.isRunning {
        p.terminate()
        return nil
    }
    let data = out.fileHandleForReading.readDataToEndOfFile()
    return p.terminationStatus == 0 ? String(decoding: data, as: UTF8.self) : nil
}

/// Keep Awake: hold the Mac and its display awake, and stop a closed lid from sleeping it.
///
/// Idle sleep is prevented with a power assertion (no privileges needed, released when
/// AgentNotch quits). Sleeping on lid close can only be switched off system-wide with
/// `pmset disablesleep`, which needs an administrator password; macOS asks for it.
enum KeepAwake {
    private static var assertion: IOPMAssertionID = 0
    private(set) static var held = false

    static func hold(_ on: Bool) {
        if on, !held {
            let r = IOPMAssertionCreateWithName(kIOPMAssertPreventUserIdleDisplaySleep as CFString, IOPMAssertionLevel(kIOPMAssertionLevelOn), "AgentNotch Keep Awake" as CFString, &assertion)
            held = r == kIOReturnSuccess
        } else if !on, held {
            IOPMAssertionRelease(assertion)
            held = false
        }
    }

    /// Is lid-close sleep switched off system-wide?
    static func lidSleepDisabled() -> Bool {
        guard let out = runTool("/usr/bin/pmset", ["-g"]) else { return false }
        return out.split(separator: "\n").contains { $0.contains("SleepDisabled") && $0.trimmingCharacters(in: .whitespaces).hasSuffix("1") }
    }

    /// Switch lid-close sleep off or back on. macOS asks for an administrator password.
    /// Returns false if the user cancelled.
    @discardableResult
    static func setLidSleepDisabled(_ disabled: Bool) -> Bool {
        let script = "do shell script \"/usr/bin/pmset -a disablesleep \(disabled ? 1 : 0)\" with administrator privileges"
        return runTool("/usr/bin/osascript", ["-e", script], timeout: 120) != nil
    }
}

/// Claude Code Remote Control: `claude remote-control` in a Terminal window.
enum RemoteControl {
    static var pidFile: URL { Paths.support.appendingPathComponent("rc.pid") }

    static func ourPid() -> Int32? {
        guard let s = try? String(contentsOf: pidFile, encoding: .utf8), let pid = Int32(s.trimmingCharacters(in: .whitespacesAndNewlines)), CLI.alive(pid) else { return nil }
        return pid
    }

    static func start(dir: String) -> Bool {
        try? FileManager.default.removeItem(at: pidFile)
        let q = { (s: String) in "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'" }
        // a login shell so that `claude` is on PATH; exec makes the recorded pid Claude's own
        let script = "#!/bin/zsh -l\ncd \(q(dir)) 2>/dev/null || cd ~\necho $$ > \(q(pidFile.path))\nexec claude remote-control\n"
        let url = Paths.support.appendingPathComponent("Claude Remote Control.command")
        guard (try? Data(script.utf8).write(to: url)) != nil else { return false }
        try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return NSWorkspace.shared.open(url)
    }

    static func stop() {
        if let pid = ourPid() { kill(pid, SIGTERM) }
        try? FileManager.default.removeItem(at: pidFile)
    }

    /// Is a Claude Code process running with Remote Control switched on from its command line?
    static func externalRunning() -> Bool {
        guard let out = runTool("/usr/bin/pgrep", ["-f", "claude.*(remote-control|--rc)"], timeout: 3) else { return false }
        let ours = ourPid()
        return out.split(separator: "\n").compactMap { Int32($0.trimmingCharacters(in: .whitespaces)) }.contains { $0 != ours && $0 != getpid() }
    }
}

/// Global hide/show hotkey: Ctrl+\.
enum Hotkey {
    private static var ref: EventHotKeyRef?
    private static var installed = false
    static var action: (() -> Void)?

    static func set(_ on: Bool) {
        if let r = ref {
            UnregisterEventHotKey(r)
            ref = nil
        }
        guard on else { return }
        if !installed {
            installed = true
            var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
            InstallEventHandler(GetApplicationEventTarget(), { _, _, _ in
                DispatchQueue.main.async { Hotkey.action?() }
                return noErr
            }, 1, &spec, nil, nil)
        }
        let id = EventHotKeyID(signature: OSType(0x4E54_4348), id: 1) // "NTCH"
        RegisterEventHotKey(UInt32(kVK_ANSI_Backslash), UInt32(controlKey), id, GetApplicationEventTarget(), 0, &ref)
    }
}

/// System notifications, with Approve / Deny buttons on permission requests.
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    var onOpen: ((String) -> Void)?
    var onDecide: ((String, Bool) -> Void)?
    /// Notifications need a real app bundle; a bare binary (tests, snapshots) has none.
    private let available = Bundle.main.bundleIdentifier != nil

    func setup() {
        guard available else { return }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        let approve = UNNotificationAction(identifier: "APPROVE", title: "Approve", options: [])
        let deny = UNNotificationAction(identifier: "DENY", title: "Deny", options: [.destructive])
        center.setNotificationCategories([
            UNNotificationCategory(identifier: "PERMISSION", actions: [approve, deny], intentIdentifiers: [], options: []),
            UNNotificationCategory(identifier: "INFO", actions: [], intentIdentifiers: [], options: []),
        ])
    }

    func show(title: String, body: String, sessionId: String, canDecide: Bool, sound: Bool) {
        guard available else { return }
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert, .sound]) { granted, _ in
            guard granted else { return }
            let content = UNMutableNotificationContent()
            content.title = title
            content.body = body
            content.categoryIdentifier = canDecide ? "PERMISSION" : "INFO"
            content.userInfo = ["session": sessionId]
            if sound { content.sound = .default }
            center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
    }

    func userNotificationCenter(_: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler done: @escaping () -> Void) {
        let sid = response.notification.request.content.userInfo["session"] as? String ?? ""
        let action = response.actionIdentifier
        DispatchQueue.main.async {
            switch action {
            case "APPROVE": self.onDecide?(sid, true)
            case "DENY": self.onDecide?(sid, false)
            default: self.onOpen?(sid)
            }
            done()
        }
    }

    func userNotificationCenter(_: UNUserNotificationCenter, willPresent _: UNNotification, withCompletionHandler done: @escaping (UNNotificationPresentationOptions) -> Void) {
        done([.banner, .sound])
    }
}

/// Bring a session to the user: its page in the Claude desktop app, else the app that owns
/// its process, else its folder.
enum Focus {
    static func open(_ s: Session) {
        if s.host.hasPrefix("local_"), let url = URL(string: "claude://claude.ai/epitaxy/\(s.host)"), NSWorkspace.shared.open(url) {
            logLine("focus: app link")
            return
        }
        if let pid = s.pid, activateOwner(of: pid) {
            logLine("focus: window")
            return
        }
        if !s.dir.isEmpty, FileManager.default.fileExists(atPath: s.dir) {
            NSWorkspace.shared.open(URL(fileURLWithPath: s.dir))
            logLine("focus: folder")
        }
    }

    /// Walk up from a process to the first ancestor that is a regular app, and activate it.
    private static func activateOwner(of pid: Int32) -> Bool {
        var cur = pid
        for _ in 0 ..< 12 {
            if let app = NSRunningApplication(processIdentifier: cur), app.activationPolicy == .regular {
                return app.activate(options: [.activateAllWindows])
            }
            guard let out = runTool("/bin/ps", ["-o", "ppid=", "-p", "\(cur)"], timeout: 2), let parent = Int32(out.trimmingCharacters(in: .whitespacesAndNewlines)), parent > 1 else { return false }
            cur = parent
        }
        return false
    }
}

/// The full-screen reminder travel mode puts up.
struct TravelOverlay: View {
    let dismiss: () -> Void
    let openSettings: () -> Void

    var body: some View {
        ZStack {
            Color.black.opacity(0.96).ignoresSafeArea()
            VStack(spacing: 18) {
                Text("Turn On Mobile Hotspot").font(.ui(60, .bold)).foregroundColor(.white).multilineTextAlignment(.center)
                Text("Travel mode is on: Remote Control and Keep Awake are set.").font(.ui(19)).foregroundColor(Palette.dim)
                Button(action: openSettings) {
                    Text("Open Wi-Fi settings").font(.ui(17, .semibold)).foregroundColor(.white).padding(.horizontal, 26).padding(.vertical, 11)
                        .background(Capsule().fill(Palette.blue.opacity(0.25))).overlay(Capsule().stroke(Palette.blue, lineWidth: 1))
                }.buttonStyle(.plain).padding(.top, 14)
            }
            VStack {
                Spacer()
                Text("Click anywhere or press Esc to dismiss").font(.ui(14)).foregroundColor(Palette.dim).padding(.bottom, 44)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture(perform: dismiss)
    }
}

final class OverlayWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}

enum Travel {
    private static var window: NSWindow?
    private static var monitor: Any?

    static func showReminder(on screen: NSScreen) {
        closeReminder()
        let w = OverlayWindow(contentRect: screen.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        w.level = .screenSaver
        w.isOpaque = false
        w.backgroundColor = .clear
        w.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        w.isReleasedWhenClosed = false
        w.contentView = NSHostingView(rootView: TravelOverlay(dismiss: closeReminder, openSettings: {
            if let url = URL(string: "x-apple.systempreferences:com.apple.wifi-settings-extension") { NSWorkspace.shared.open(url) }
            closeReminder()
        }))
        w.setFrame(screen.frame, display: true)
        NSApp.activate(ignoringOtherApps: true)
        w.makeKeyAndOrderFront(nil)
        window = w
        // Esc, Return or Space dismisses
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { e in
            if [53, 36, 49].contains(Int(e.keyCode)) {
                closeReminder()
                return nil
            }
            return e
        }
    }

    static func closeReminder() {
        if let m = monitor {
            NSEvent.removeMonitor(m)
            monitor = nil
        }
        window?.orderOut(nil)
        window = nil
    }
}
