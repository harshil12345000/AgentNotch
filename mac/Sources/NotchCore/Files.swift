import Foundation

/// Remembers how far each file has been read and returns only complete new lines.
public final class Tailer {
    private var offsets: [String: UInt64] = [:]
    public init() {}

    public func knows(_ url: URL) -> Bool { offsets[url.path] != nil }

    /// New complete lines since the last call. The first time a file is seen, reading
    /// starts at most `firstTail` bytes before its end (never the whole file).
    public func readNew(_ url: URL, firstTail: UInt64, maxLine: Int) -> [String] {
        guard let h = try? FileHandle(forReadingFrom: url) else { return [] }
        defer { try? h.close() }
        let len = (try? h.seekToEnd()) ?? 0
        let known = offsets[url.path]
        var start: UInt64
        if let k = known { start = k <= len ? k : 0 } else { start = len > firstTail ? len - firstTail : 0 }
        if start == len {
            offsets[url.path] = len
            return []
        }
        guard (try? h.seek(toOffset: start)) != nil, let data = try? h.read(upToCount: Int(len - start)), !data.isEmpty else { return [] }
        // only consume up to the last newline: a partial line is re-read next time
        guard let lastNL = data.lastIndex(of: 0x0A) else {
            if known == nil { offsets[url.path] = start }
            return []
        }
        var body = data[data.startIndex ..< lastNL]
        if known == nil, start > 0 {
            // we started mid-file: drop the first, probably partial, line
            if let i = body.firstIndex(of: 0x0A) { body = body[(i + 1)...] } else { body = Data() }
        }
        start += UInt64(lastNL - data.startIndex) + 1
        offsets[url.path] = start
        return body.split(separator: 0x0A, omittingEmptySubsequences: true).compactMap { line in
            line.count <= maxLine ? String(data: Data(line), encoding: .utf8)?.trimmingCharacters(in: CharacterSet(charactersIn: "\r")) : nil
        }
    }
}

/// Files under `dir` (recursing at most `depth` levels) matching `pred` that were modified
/// within `maxAge` seconds.
public func recentFiles(_ dir: URL, depth: Int, maxAge: TimeInterval, _ pred: (URL) -> Bool) -> [URL] {
    var out = [URL]()
    guard let items = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.isDirectoryKey, .contentModificationDateKey]) else { return out }
    let now = Date()
    for u in items {
        let v = try? u.resourceValues(forKeys: [.isDirectoryKey, .contentModificationDateKey])
        if v?.isDirectory == true {
            if depth > 0 { out += recentFiles(u, depth: depth - 1, maxAge: maxAge, pred) }
        } else if pred(u), let m = v?.contentModificationDate, now.timeIntervalSince(m) <= maxAge {
            out.append(u)
        }
    }
    return out
}

/// Where Notch keeps its own files.
public enum Paths {
    public static var support: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first ?? FileManager.default.homeDirectoryForCurrentUser
        let dir = base.appendingPathComponent("Notch")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// The socket hooks talk to. NOTCH_INSTANCE=name gives an isolated test instance.
    public static var socket: String {
        let suffix = (ProcessInfo.processInfo.environment["NOTCH_INSTANCE"] ?? "").filter { $0.isLetter || $0.isNumber }
        return support.appendingPathComponent(suffix.isEmpty ? "notch.sock" : "notch-\(suffix).sock").path
    }

    public static var config: URL { support.appendingPathComponent("config.json") }
    public static var log: URL { support.appendingPathComponent("notch.log") }
}

public func logLine(_ s: String) {
    let line = "\(Int(Date().timeIntervalSince1970)) \(s)\n"
    let url = Paths.log
    if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path), let size = attrs[.size] as? UInt64, size > 2_000_000 {
        try? FileManager.default.removeItem(at: url)
    }
    if let h = try? FileHandle(forWritingTo: url) {
        defer { try? h.close() }
        _ = try? h.seekToEnd()
        try? h.write(contentsOf: Data(line.utf8))
    } else {
        try? Data(line.utf8).write(to: url)
    }
}
