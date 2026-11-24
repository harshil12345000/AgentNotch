import Foundation

/// Local IPC over a Unix domain socket. The app listens; `Notch --hook` is the client.
///
/// One message per connection. The client writes its message and half-closes; the server
/// reads to the end. For a permission request the server keeps the connection open and
/// later writes one word back ("allow", "deny" or "pass"), which the waiting hook reads.
public enum IPC {
    private static func address(_ path: String) -> sockaddr_un? {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { return nil }
        withUnsafeMutableBytes(of: &addr.sun_path) { p in
            for (i, b) in bytes.enumerated() { p[i] = b }
            p[bytes.count] = 0
        }
        return addr
    }

    private static func noSigpipe(_ fd: Int32) {
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
    }

    static func writeAll(_ fd: Int32, _ data: Data) -> Bool {
        var sent = 0
        return data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) -> Bool in
            while sent < raw.count {
                let n = write(fd, raw.baseAddress!.advanced(by: sent), raw.count - sent)
                if n <= 0 { return false }
                sent += n
            }
            return true
        }
    }

    // MARK: client

    /// Send one message. With `waitReply`, block up to that long for the server's answer.
    /// Returns nil when the app is not running or sent nothing back.
    public static func send(_ data: Data, to path: String = Paths.socket, waitReply: TimeInterval? = nil) -> String? {
        guard var addr = address(path) else { return nil }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        noSigpipe(fd)
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard ok == 0, writeAll(fd, data) else { return nil }
        shutdown(fd, SHUT_WR)
        guard let limit = waitReply else { return "" }
        // wait for the answer, or for the server to close the connection
        var pfd = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
        let deadline = Date().addingTimeInterval(limit)
        var out = Data()
        while Date() < deadline {
            let ms = Int32(max(1, min(1000, deadline.timeIntervalSinceNow * 1000)))
            let r = poll(&pfd, 1, ms)
            if r < 0 { return nil }
            if r == 0 { continue }
            var buf = [UInt8](repeating: 0, count: 64)
            let n = read(fd, &buf, buf.count)
            if n <= 0 { break }
            out.append(contentsOf: buf[0 ..< n])
        }
        let word = String(decoding: out, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return word.isEmpty ? nil : word
    }

    // MARK: server

    /// A connection kept open so an answer can be written to it later.
    public final class Reply {
        private let fd: Int32
        private var done = false
        private let lock = NSLock()
        init(fd: Int32) { self.fd = fd }

        /// Write the answer and close. Safe to call more than once.
        public func send(_ word: String) {
            lock.lock()
            defer { lock.unlock() }
            if done { return }
            done = true
            _ = IPC.writeAll(fd, Data(word.utf8))
            close(fd)
        }

        deinit { send("pass") }
    }

    /// Listen on `path`. `handler` gets each message and a `Reply` for that connection;
    /// dropping the reply (or calling `send`) closes it. Returns false if it cannot listen.
    @discardableResult
    public static func listen(on path: String = Paths.socket, handler: @escaping (Data, Reply) -> Void) -> Bool {
        guard var addr = address(path) else { return false }
        unlink(path)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0, Darwin.listen(fd, 64) == 0 else {
            close(fd)
            return false
        }
        chmod(path, 0o600) // only this user
        let queue = DispatchQueue(label: "notch.ipc", attributes: .concurrent)
        Thread.detachNewThread {
            while true {
                let c = accept(fd, nil, nil)
                if c < 0 {
                    if errno == EINTR { continue }
                    break
                }
                noSigpipe(c)
                queue.async {
                    var data = Data()
                    var buf = [UInt8](repeating: 0, count: 65536)
                    while data.count < 4 * 1024 * 1024 {
                        let n = read(c, &buf, buf.count)
                        if n <= 0 { break }
                        data.append(contentsOf: buf[0 ..< n])
                    }
                    handler(data, Reply(fd: c))
                }
            }
        }
        return true
    }
}
