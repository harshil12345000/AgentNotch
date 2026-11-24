import Foundation

/// A JSON value that keeps object keys in their original order.
///
/// Foundation's JSON types lose key order, which would reshuffle a user's settings file
/// every time Notch touched it. This small parser and printer keep it as the user wrote it.
public indirect enum JSON: Equatable {
    case null
    case bool(Bool)
    /// Kept as written, so 1, 1.0 and 1e3 round-trip unchanged.
    case number(String)
    case string(String)
    case array([JSON])
    case object([Member])

    public struct Member: Equatable {
        public var key: String
        public var value: JSON
        public init(_ key: String, _ value: JSON) {
            self.key = key
            self.value = value
        }
    }

    /// Objects compare without regard to key order; everything else compares exactly.
    public static func == (a: JSON, b: JSON) -> Bool {
        switch (a, b) {
        case (.null, .null): return true
        case let (.bool(x), .bool(y)): return x == y
        case let (.number(x), .number(y)): return x == y || Double(x) == Double(y)
        case let (.string(x), .string(y)): return x == y
        case let (.array(x), .array(y)): return x == y
        case let (.object(x), .object(y)):
            guard x.count == y.count else { return false }
            for m in x {
                guard let other = y.first(where: { $0.key == m.key }), other.value == m.value else { return false }
            }
            return true
        default: return false
        }
    }

    // MARK: reading

    public subscript(key: String) -> JSON? {
        get {
            if case let .object(members) = self { return members.first(where: { $0.key == key })?.value }
            return nil
        }
        set {
            guard case var .object(members) = self else { return }
            if let i = members.firstIndex(where: { $0.key == key }) {
                if let v = newValue { members[i].value = v } else { members.remove(at: i) }
            } else if let v = newValue {
                members.append(Member(key, v))
            }
            self = .object(members)
        }
    }

    public var string: String? {
        if case let .string(s) = self { return s }
        return nil
    }

    public var array: [JSON]? {
        if case let .array(a) = self { return a }
        return nil
    }

    public var members: [Member]? {
        if case let .object(m) = self { return m }
        return nil
    }

    public var double: Double? {
        if case let .number(n) = self { return Double(n) }
        return nil
    }

    public var bool: Bool? {
        if case let .bool(b) = self { return b }
        return nil
    }

    /// The string at `key`, or "" when it is missing or not a string.
    public func str(_ key: String) -> String { self[key]?.string ?? "" }

    public var isEmptyObject: Bool { members?.isEmpty ?? false }

    // MARK: parsing

    public static func parse(_ text: String) -> JSON? {
        var p = Parser(Array(text.utf8))
        p.skipSpace()
        guard let v = p.value() else { return nil }
        p.skipSpace()
        return p.i == p.b.count ? v : nil
    }

    public static func parse(_ data: Data) -> JSON? {
        var bytes = Array(data)
        if bytes.starts(with: [0xEF, 0xBB, 0xBF]) { bytes.removeFirst(3) }
        var p = Parser(bytes)
        p.skipSpace()
        guard let v = p.value() else { return nil }
        p.skipSpace()
        return p.i == p.b.count ? v : nil
    }

    private struct Parser {
        let b: [UInt8]
        var i = 0
        var depth = 0
        init(_ bytes: [UInt8]) { b = bytes }

        mutating func skipSpace() {
            while i < b.count, b[i] == 0x20 || b[i] == 0x0A || b[i] == 0x0D || b[i] == 0x09 { i += 1 }
        }

        mutating func literal(_ word: String) -> Bool {
            let w = Array(word.utf8)
            guard i + w.count <= b.count, Array(b[i ..< i + w.count]) == w else { return false }
            i += w.count
            return true
        }

        mutating func value() -> JSON? {
            guard i < b.count, depth < 200 else { return nil }
            switch b[i] {
            case UInt8(ascii: "{"): return object()
            case UInt8(ascii: "["): return array()
            case UInt8(ascii: "\""): return string().map { .string($0) }
            case UInt8(ascii: "t"): return literal("true") ? .bool(true) : nil
            case UInt8(ascii: "f"): return literal("false") ? .bool(false) : nil
            case UInt8(ascii: "n"): return literal("null") ? .null : nil
            default: return number()
            }
        }

        mutating func number() -> JSON? {
            let start = i
            while i < b.count, "+-0123456789.eE".utf8.contains(b[i]) { i += 1 }
            guard i > start, let s = String(bytes: b[start ..< i], encoding: .utf8), Double(s) != nil else { return nil }
            return .number(s)
        }

        mutating func string() -> String? {
            guard i < b.count, b[i] == UInt8(ascii: "\"") else { return nil }
            i += 1
            var out = [UInt8]()
            while i < b.count {
                let c = b[i]
                if c == UInt8(ascii: "\"") {
                    i += 1
                    return String(decoding: out, as: UTF8.self)
                }
                if c == UInt8(ascii: "\\") {
                    i += 1
                    guard i < b.count else { return nil }
                    switch b[i] {
                    case UInt8(ascii: "n"): out.append(0x0A)
                    case UInt8(ascii: "t"): out.append(0x09)
                    case UInt8(ascii: "r"): out.append(0x0D)
                    case UInt8(ascii: "b"): out.append(0x08)
                    case UInt8(ascii: "f"): out.append(0x0C)
                    case UInt8(ascii: "u"):
                        guard var unit = hex4() else { return nil }
                        // surrogate pair
                        if (0xD800 ... 0xDBFF).contains(unit), i + 2 < b.count, b[i + 1] == UInt8(ascii: "\\"), b[i + 2] == UInt8(ascii: "u") {
                            i += 2
                            guard let low = hex4() else { return nil }
                            unit = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                        }
                        out.append(contentsOf: Array(String(Unicode.Scalar(unit) ?? "\u{FFFD}").utf8))
                    default: out.append(b[i]) // \" \\ \/
                    }
                    i += 1
                } else {
                    out.append(c)
                    i += 1
                }
            }
            return nil
        }

        /// Reads the four hex digits after "\u" and leaves `i` on the last one.
        mutating func hex4() -> UInt32? {
            guard i + 4 < b.count, let s = String(bytes: b[i + 1 ... i + 4], encoding: .utf8), let v = UInt32(s, radix: 16) else { return nil }
            i += 4
            return v
        }

        mutating func array() -> JSON? {
            i += 1
            depth += 1
            defer { depth -= 1 }
            var items = [JSON]()
            skipSpace()
            if i < b.count, b[i] == UInt8(ascii: "]") {
                i += 1
                return .array(items)
            }
            while true {
                skipSpace()
                guard let v = value() else { return nil }
                items.append(v)
                skipSpace()
                guard i < b.count else { return nil }
                if b[i] == UInt8(ascii: ",") { i += 1; continue }
                if b[i] == UInt8(ascii: "]") { i += 1; return .array(items) }
                return nil
            }
        }

        mutating func object() -> JSON? {
            i += 1
            depth += 1
            defer { depth -= 1 }
            var members = [Member]()
            skipSpace()
            if i < b.count, b[i] == UInt8(ascii: "}") {
                i += 1
                return .object(members)
            }
            while true {
                skipSpace()
                guard let k = string() else { return nil }
                skipSpace()
                guard i < b.count, b[i] == UInt8(ascii: ":") else { return nil }
                i += 1
                skipSpace()
                guard let v = value() else { return nil }
                members.append(Member(k, v))
                skipSpace()
                guard i < b.count else { return nil }
                if b[i] == UInt8(ascii: ",") { i += 1; continue }
                if b[i] == UInt8(ascii: "}") { i += 1; return .object(members) }
                return nil
            }
        }
    }

    // MARK: printing

    /// Pretty JSON with two-space indentation (the style Claude Code writes).
    public func pretty(indent: Int = 0) -> String {
        let pad = String(repeating: "  ", count: indent)
        let inner = String(repeating: "  ", count: indent + 1)
        switch self {
        case .null: return "null"
        case let .bool(v): return v ? "true" : "false"
        case let .number(n): return n
        case let .string(s): return JSON.quote(s)
        case let .array(items):
            if items.isEmpty { return "[]" }
            return "[\n" + items.map { inner + $0.pretty(indent: indent + 1) }.joined(separator: ",\n") + "\n" + pad + "]"
        case let .object(members):
            if members.isEmpty { return "{}" }
            return "{\n" + members.map { inner + JSON.quote($0.key) + ": " + $0.value.pretty(indent: indent + 1) }.joined(separator: ",\n") + "\n" + pad + "}"
        }
    }

    /// One line, no spaces: for messages on the wire.
    public func compact() -> String {
        switch self {
        case .null: return "null"
        case let .bool(v): return v ? "true" : "false"
        case let .number(n): return n
        case let .string(s): return JSON.quote(s)
        case let .array(items): return "[" + items.map { $0.compact() }.joined(separator: ",") + "]"
        case let .object(members): return "{" + members.map { JSON.quote($0.key) + ":" + $0.value.compact() }.joined(separator: ",") + "}"
        }
    }

    static func quote(_ s: String) -> String {
        var out = "\""
        for u in s.unicodeScalars {
            switch u {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            default:
                if u.value < 0x20 {
                    out += String(format: "\\u%04x", u.value)
                } else {
                    out.unicodeScalars.append(u)
                }
            }
        }
        return out + "\""
    }
}
