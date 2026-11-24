import Foundation

/// Install / uninstall Notch's hooks in a Claude Code (or Codex) settings file.
///
/// Rules: never overwrite or reorder the user's own hooks, back the file up before the
/// first change, and make uninstall put the file back byte for byte when nothing else
/// changed in between.
public enum Hooks {
    public struct Target {
        public var settings: URL
        /// The program the hook runs.
        public var command: String
        public var args: [String]
        public var events: [String]
        /// Claude Code style exec form (`command` + `args`, `async`). False = one shell string.
        public var execForm: Bool

        public init(settings: URL, command: String, args: [String], events: [String], execForm: Bool) {
            self.settings = settings
            self.command = command
            self.args = args
            self.events = events
            self.execForm = execForm
        }
    }

    public struct Failure: Error, CustomStringConvertible {
        public let description: String
        init(_ d: String) { description = d }
    }

    public static var claudeSettings: URL { Claude.configDir.appendingPathComponent("settings.json") }

    public static func claudeTarget(exe: String, settings: URL? = nil) -> Target {
        Target(settings: settings ?? claudeSettings, command: exe, args: ["--hook"], events: Claude.hookEvents, execForm: true)
    }

    static func backupURL(_ s: URL) -> URL { URL(fileURLWithPath: s.path + ".notch-backup") }
    /// Marker meaning "there was no settings file before we installed".
    static func absentURL(_ s: URL) -> URL { URL(fileURLWithPath: s.path + ".notch-absent") }

    static func isOurs(_ handler: JSON) -> Bool {
        let cmd = handler.str("command").lowercased()
        let inArgs = handler["args"]?.array?.contains { $0.string == "--hook" } ?? false
        return cmd.contains("notch") && (inArgs || cmd.contains("--hook"))
    }

    static func handler(_ t: Target, _ event: String) -> JSON {
        var members: [JSON.Member] = [.init("type", .string("command"))]
        if t.execForm {
            members.append(.init("command", .string(t.command)))
            members.append(.init("args", .array(t.args.map { .string($0) })))
            if event == "PermissionRequest" {
                // the one hook that waits: it can carry an Approve / Deny back to Claude Code
                members.append(.init("timeout", .number("600")))
            } else {
                members.append(.init("async", .bool(true)))
                members.append(.init("timeout", .number("5")))
            }
        } else {
            members.append(.init("command", .string("\"\(t.command)\" " + t.args.joined(separator: " "))))
            members.append(.init("timeout", .number("5")))
        }
        return .object(members)
    }

    static func parse(_ data: Data) throws -> JSON {
        let text = String(decoding: data, as: UTF8.self)
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return .object([]) }
        guard let v = JSON.parse(data) else { throw Failure("the settings file is not valid JSON; left untouched") }
        guard v.members != nil else { throw Failure("the settings file is not a JSON object; left untouched") }
        return v
    }

    /// Pretty JSON, keeping the original's trailing newline.
    static func render(_ v: JSON, like: Data?) -> Data {
        var s = v.pretty()
        if like == nil || like?.last == 0x0A { s += "\n" }
        return Data(s.utf8)
    }

    /// Remove our handlers. Containers that become empty because of it are dropped too.
    static func strip(_ root: inout JSON) -> Bool {
        guard var hooks = root["hooks"]?.members else { return false }
        var removed = false
        var kept = [JSON.Member]()
        for event in hooks {
            guard let groups = event.value.array else { kept.append(event); continue }
            var touched = false
            var newGroups = [JSON]()
            for g in groups {
                guard let hs = g["hooks"]?.array else { newGroups.append(g); continue }
                let left = hs.filter { !isOurs($0) }
                if left.count != hs.count {
                    touched = true
                    if !left.isEmpty {
                        var g2 = g
                        g2["hooks"] = .array(left)
                        newGroups.append(g2)
                    }
                } else {
                    newGroups.append(g)
                }
            }
            if touched {
                removed = true
                if !newGroups.isEmpty { kept.append(.init(event.key, .array(newGroups))) }
            } else {
                kept.append(event)
            }
        }
        hooks = kept
        if removed {
            if hooks.isEmpty { root["hooks"] = nil } else { root["hooks"] = .object(hooks) }
        }
        return removed
    }

    /// The program our installed hooks run, if they are installed.
    public static func installedCommand(_ settings: URL) -> String? {
        guard let data = try? Data(contentsOf: settings), let v = try? parse(data), let hooks = v["hooks"]?.members else { return nil }
        for event in hooks {
            for g in event.value.array ?? [] {
                if let h = (g["hooks"]?.array ?? []).first(where: isOurs) {
                    let c = h.str("command")
                    // exec form holds the path alone; shell form is "\"path\" --hook ..."
                    if c.hasPrefix("\""), let end = c.dropFirst().firstIndex(of: "\"") { return String(c[c.index(after: c.startIndex) ..< end]) }
                    return c
                }
            }
        }
        return nil
    }

    public static func installed(_ settings: URL) -> Bool {
        guard let data = try? Data(contentsOf: settings), let v = try? parse(data), let hooks = v["hooks"]?.members else { return false }
        for event in hooks {
            for g in event.value.array ?? [] {
                if (g["hooks"]?.array ?? []).contains(where: isOurs) { return true }
            }
        }
        return false
    }

    @discardableResult
    public static func install(_ t: Target) throws -> String {
        let fm = FileManager.default
        let original = try? Data(contentsOf: t.settings)
        var root = try original.map(parse) ?? .object([])
        let (bak, absent) = (backupURL(t.settings), absentURL(t.settings))
        try fm.createDirectory(at: t.settings.deletingLastPathComponent(), withIntermediateDirectories: true)
        // keep the first backup: it is the file as it was before Notch ever touched it
        if !fm.fileExists(atPath: bak.path), !fm.fileExists(atPath: absent.path) {
            if let original { try original.write(to: bak) } else { try Data().write(to: absent) }
        }
        _ = strip(&root) // re-install cleanly (for example after the app moved)
        var hooks = root["hooks"]?.members ?? []
        if root["hooks"] != nil, root["hooks"]?.members == nil { throw Failure("\"hooks\" in the settings file is not an object; left untouched") }
        for ev in t.events {
            let group = JSON.object([.init("hooks", .array([handler(t, ev)]))])
            if let i = hooks.firstIndex(where: { $0.key == ev }) {
                guard var groups = hooks[i].value.array else { throw Failure("hooks.\(ev) is not an array; left untouched") }
                groups.append(group)
                hooks[i].value = .array(groups)
            } else {
                hooks.append(.init(ev, .array([group])))
            }
        }
        root["hooks"] = .object(hooks)
        try render(root, like: original).write(to: t.settings, options: .atomic)
        return "Hooks installed in \(t.settings.path) (\(t.events.count) events)."
    }

    @discardableResult
    public static func uninstall(_ settings: URL) throws -> String {
        let fm = FileManager.default
        let (bak, absent) = (backupURL(settings), absentURL(settings))
        guard let current = try? Data(contentsOf: settings) else {
            try? fm.removeItem(at: bak)
            try? fm.removeItem(at: absent)
            return "No settings file: nothing to remove."
        }
        var root = try parse(current)
        guard strip(&root) else { return "Notch hooks are not installed: nothing to remove." }
        var note = "Hooks removed."
        if fm.fileExists(atPath: absent.path), root.isEmptyObject {
            try fm.removeItem(at: settings)
            note = "Hooks removed; the settings file did not exist before, so it was deleted."
        } else if let b = try? Data(contentsOf: bak), let old = try? parse(b), old == root {
            try b.write(to: settings, options: .atomic)
            note = "Hooks removed; the settings file was restored byte for byte from the backup."
        } else {
            try render(root, like: current).write(to: settings, options: .atomic)
        }
        try? fm.removeItem(at: bak)
        try? fm.removeItem(at: absent)
        return note
    }
}
