import Foundation

public struct UsageWindow: Equatable {
    /// 0...100
    public var pct: Double
    public var resetsAt: UInt64?
    public init(pct: Double, resetsAt: UInt64?) {
        self.pct = pct
        self.resetsAt = resetsAt
    }
}

public struct Usage: Equatable {
    public var fiveHour: UsageWindow?
    public var week: UsageWindow?
    public var fetchedAt: UInt64 = 0
    public var note = ""
    public init() {}
    public var hasNumbers: Bool { fiveHour != nil || week != nil }
}

/// Claude usage limits: the figures Claude Code's /usage shows.
///
/// The endpoint is undocumented and rate-limits readily. The sign-in token is read from
/// where Claude Code keeps it (a credentials file, or the login Keychain on macOS), sent
/// only to api.anthropic.com, and never logged, stored or shown.
public enum UsageAPI {
    /// Parse the endpoint's reply.
    public static func parse(_ data: Data) -> Usage? {
        guard let j = JSON.parse(data) else { return nil }
        func window(_ key: String) -> UsageWindow? {
            guard let w = j[key], let u = w["utilization"]?.double else { return nil }
            return UsageWindow(pct: min(100, max(0, u)), resetsAt: parseTimestamp(w.str("resets_at")))
        }
        var u = Usage()
        u.fiveHour = window("five_hour")
        u.week = window("seven_day")
        return u.hasNumbers ? u : nil
    }

    /// Access token and whether it has expired, from a credentials blob.
    public static func token(from data: Data) -> (token: String, expired: Bool)? {
        guard let oauth = JSON.parse(data)?["claudeAiOauth"] else { return nil }
        let t = oauth.str("accessToken")
        if t.isEmpty { return nil }
        let exp = oauth["expiresAt"]?.double ?? 0
        return (t, exp > 0 && exp / 1000 < Double(nowUnix() + 60))
    }

    static func run(_ tool: String, _ args: [String], timeout: TimeInterval = 20) -> Data? {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        p.standardInput = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        let deadline = Date().addingTimeInterval(timeout)
        while p.isRunning, Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        if p.isRunning {
            p.terminate()
            return nil
        }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        return p.terminationStatus == 0 ? data : nil
    }

    /// The credentials Claude Code stores: a file where it uses one, else the Keychain item
    /// it creates on macOS (reading that makes macOS ask the user once).
    static func credentials() -> Data? {
        if let d = try? Data(contentsOf: Claude.configDir.appendingPathComponent(".credentials.json")) { return d }
        return run("/usr/bin/security", ["find-generic-password", "-s", "Claude Code-credentials", "-w"])
    }

    /// Fetch the numbers (blocking; call off the main thread).
    public static func fetch() -> Usage {
        var result = Usage()
        result.fetchedAt = nowUnix()
        guard let creds = credentials(), var tok = token(from: creds) else {
            result.note = "No Claude sign-in found"
            return result
        }
        if tok.expired {
            // Notch never refreshes the token itself: it asks Claude Code to, which renews its own store.
            _ = run("/bin/sh", ["-lc", "claude auth status"], timeout: 25)
            if let again = credentials(), let t2 = token(from: again) { tok = t2 }
            if tok.expired {
                result.note = "Claude sign-in expired: run `claude` once in a terminal"
                return result
            }
        }
        var req = URLRequest(url: URL(string: "https://api.anthropic.com/api/oauth/usage")!)
        req.timeoutInterval = 10
        req.setValue("Bearer \(tok.token)", forHTTPHeaderField: "Authorization")
        req.setValue("oauth-2025-04-20", forHTTPHeaderField: "anthropic-beta")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        let sem = DispatchSemaphore(value: 0)
        var body: Data?
        var status = 0
        URLSession.shared.dataTask(with: req) { d, r, _ in
            body = d
            status = (r as? HTTPURLResponse)?.statusCode ?? 0
            sem.signal()
        }.resume()
        _ = sem.wait(timeout: .now() + 12)
        switch status {
        case 200:
            if let d = body, var u = parse(d) {
                u.fetchedAt = result.fetchedAt
                return u
            }
            result.note = "Usage reply not understood"
        case 401, 403: result.note = "Claude sign-in rejected: run `claude` once in a terminal"
        case 429: result.note = "Usage endpoint is rate limiting"
        case 0: result.note = "Could not reach api.anthropic.com"
        default: result.note = "Usage endpoint returned HTTP \(status)"
        }
        return result
    }
}
