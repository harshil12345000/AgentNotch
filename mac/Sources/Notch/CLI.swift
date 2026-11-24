import Foundation
import NotchCore

/// `Notch --hook`: read the hook JSON from stdin, hand it to the app, exit 0.
/// Prints nothing unless it is carrying an Approve / Deny back, and never fails the caller.
enum HookClient {
    static func run(_ args: [String]) {
        func after(_ flag: String) -> String? {
            guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
            return args[i + 1]
        }
        let agent = after("--agent") ?? "claude"
        // Codex `notify` passes the JSON as an argument instead of stdin.
        let raw = after("--codex-notify").map { Data($0.utf8) } ?? FileHandle.standardInput.readDataToEndOfFile()
        let text = String(decoding: raw, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard text.hasPrefix("{") else { return }
        let isPermission = agent == "claude" && text.prefix(4096).contains("\"PermissionRequest\"")
        // The hook runs as a child of the agent, so the parent is the agent process.
        let head = "{\"v\":1,\"agent\":\(JSON.string(agent).compact()),\"pid\":\(getppid()),\"reply\":\(isPermission),\"p\":"
        let message = Data((head + text + "}").utf8)
        // A permission request waits for the notch's answer; everything else returns at once.
        let answer = IPC.send(message, waitReply: isPermission ? 590 : nil)
        guard isPermission else { return }
        switch answer {
        case "allow": print(#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#)
        case "deny": print(#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from Notch"}}}"#)
        default: break // "pass" or nothing: leave the decision to Claude Code's own prompt
        }
    }
}

/// Command-line actions that do not need the app's window.
enum CLI {
    static let help = """
    Notch - your AI coding agents at a glance (macOS)

      Notch                              start the app
      Notch --status                     what Notch detects (open sessions, hooks)
      Notch --install-hooks              add the hooks to ~/.claude/settings.json (backup first)
      Notch --uninstall-hooks            remove them again
      Notch --hooks-status               say whether they are installed
            [--codex]                    ... act on Codex (~/.codex/hooks.json)
            [--settings <file>]          ... act on another settings file
      Notch --decide <session> allow|deny    answer a pending permission request
      Notch --quit                       ask the running app to exit
      Notch --snapshot <file.png>        render the panel with demo data [--expanded] [--settings]
      Notch --hook                       (used by the hooks) forward stdin JSON to the app
    """

    /// The path hooks should run: this executable, resolved.
    static var executable: String {
        (Bundle.main.executablePath ?? CommandLine.arguments[0]).replacingOccurrences(of: "//", with: "/")
    }

    static func control(_ p: JSON) -> Bool {
        let msg = JSON.object([.init("v", .number("1")), .init("agent", .string("notch")), .init("p", p)])
        return IPC.send(Data(msg.compact().utf8)) != nil
    }

    static func alive(_ pid: Int32) -> Bool { pid > 0 && (kill(pid, 0) == 0 || errno == EPERM) }

    /// Returns true when the arguments were a command and it has been carried out.
    static func handle(_ args: [String]) -> Bool {
        func has(_ f: String) -> Bool { args.contains(f) }
        func after(_ flag: String) -> String? {
            guard let i = args.firstIndex(of: flag), i + 1 < args.count else { return nil }
            return args[i + 1]
        }
        if has("--help") || has("-h") {
            print(help)
            return true
        }
        if has("--quit") {
            _ = control(.object([.init("quit", .bool(true))]))
            return true
        }
        if let sid = after("--decide") {
            let allow = has("allow")
            guard allow || has("deny") else {
                print("usage: Notch --decide <session> allow|deny")
                return true
            }
            if !control(.object([.init("decide", .string(sid)), .init("allow", .bool(allow))])) { print("Notch is not running.") }
            return true
        }
        if has("--status") {
            let recs = Registry.readAll(alive: alive)
            func n(_ s: String) -> Int { recs.filter { $0.status == s }.count }
            print("claude sessions: \(recs.count) open (\(n("busy")) busy, \(n("waiting")) waiting, \(n("idle")) idle)")
            print("claude hooks:    \(Hooks.installed(Hooks.claudeSettings) ? "installed" : "not installed") (\(Hooks.claudeSettings.path))")
            print("codex hooks:     \(Hooks.installed(Codex.home.appendingPathComponent("hooks.json")) ? "installed" : "not installed")")
            let t3 = FileManager.default.fileExists(atPath: T3Code.database.path) ? (T3Code.load().map { "database readable, \($0.count) recent runs" } ?? "database found but could not be read") : "not installed"
            print("t3 code:         \(t3)")
            return true
        }
        let hookCmd = ["--install-hooks", "--uninstall-hooks", "--hooks-status"].first(where: has)
        if let cmd = hookCmd {
            let settings = after("--settings").map { URL(fileURLWithPath: $0) }
            let target = has("--codex") ? Codex.target(exe: executable, settings: settings) : Hooks.claudeTarget(exe: executable, settings: settings)
            do {
                switch cmd {
                case "--install-hooks": print(try Hooks.install(target))
                case "--uninstall-hooks": print(try Hooks.uninstall(target.settings))
                default: print("\(target.settings.path): \(Hooks.installed(target.settings) ? "installed" : "not installed")")
                }
            } catch {
                print("error: \(error)")
                exit(1)
            }
            return true
        }
        return false
    }
}
