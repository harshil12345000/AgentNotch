import XCTest
@testable import NotchCore

final class CoreTests: XCTestCase {
    func tmp(_ name: String) -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("notch-test-\(ProcessInfo.processInfo.processIdentifier)-\(name)")
        try? FileManager.default.removeItem(at: d)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    func j(_ s: String) -> JSON { JSON.parse(s)! }

    // MARK: JSON

    func testJSONKeepsOrderAndRoundTrips() {
        let text = "{\n  \"zeta\": 1,\n  \"alpha\": [\n    true,\n    null,\n    \"a\\\"b\\n\"\n  ],\n  \"mid\": {}\n}"
        let v = j(text)
        XCTAssertEqual(v.members?.map { $0.key }, ["zeta", "alpha", "mid"])
        XCTAssertEqual(v.pretty(), text)
        XCTAssertEqual(j("{\"a\":1,\"b\":2}"), j("{\"b\":2,\"a\":1.0}"))
        XCTAssertNil(JSON.parse("{ not json"))
        XCTAssertEqual(j("\"\\u00e9\\ud83d\\ude00\"").string, "é😀")
    }

    // MARK: hooks installer

    let original = "{\n    \"permissions\": {\"allow\": [\"Bash(ls)\"]},\n    \"hooks\": {\n        \"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"node guard.mjs\", \"timeout\": 10}]}]\n    },\n    \"zeta\": 1\n}"

    func testHooksRoundTripIsByteIdentical() throws {
        let file = tmp("roundtrip").appendingPathComponent("settings.json")
        try Data(original.utf8).write(to: file)
        let t = Hooks.claudeTarget(exe: "/Applications/Notch.app/Contents/MacOS/Notch", settings: file)
        try Hooks.install(t)
        XCTAssertTrue(Hooks.installed(file))
        let v = JSON.parse(try Data(contentsOf: file))!
        let pre = v["hooks"]!["PreToolUse"]!.array!
        XCTAssertEqual(pre.count, 2)
        XCTAssertEqual(pre[0]["hooks"]!.array![0].str("command"), "node guard.mjs") // the user's hook is still first
        XCTAssertEqual(v.members?.map { $0.key }, ["permissions", "hooks", "zeta"])
        // PermissionRequest waits; everything else is async
        let perm = v["hooks"]!["PermissionRequest"]!.array![0]["hooks"]!.array![0]
        XCTAssertNil(perm["async"])
        XCTAssertEqual(v["hooks"]!["Stop"]!.array![0]["hooks"]!.array![0]["async"], .bool(true))
        // installing twice does not duplicate
        try Hooks.install(t)
        XCTAssertEqual(JSON.parse(try Data(contentsOf: file))!["hooks"]!["PreToolUse"]!.array!.count, 2)
        try Hooks.uninstall(file)
        XCTAssertEqual(try Data(contentsOf: file), Data(original.utf8))
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path + ".notch-backup"))
    }

    func testHooksKeepUserEditsAndHandleMissingAndInvalidFiles() throws {
        let file = tmp("edits").appendingPathComponent("settings.json")
        try Data(original.utf8).write(to: file)
        let t = Hooks.claudeTarget(exe: "/x/Notch", settings: file)
        try Hooks.install(t)
        var v = JSON.parse(try Data(contentsOf: file))!
        v["model"] = .string("opus")
        try Data(v.pretty().utf8).write(to: file)
        try Hooks.uninstall(file)
        let after = JSON.parse(try Data(contentsOf: file))!
        XCTAssertEqual(after.str("model"), "opus")
        XCTAssertFalse(Hooks.installed(file))

        let missing = tmp("missing").appendingPathComponent("settings.json")
        try Hooks.install(Hooks.claudeTarget(exe: "/x/Notch", settings: missing))
        XCTAssertTrue(Hooks.installed(missing))
        try Hooks.uninstall(missing)
        XCTAssertFalse(FileManager.default.fileExists(atPath: missing.path))

        let bad = tmp("bad").appendingPathComponent("settings.json")
        try Data("{ not json".utf8).write(to: bad)
        XCTAssertThrowsError(try Hooks.install(Hooks.claudeTarget(exe: "/x/Notch", settings: bad)))
        XCTAssertEqual(try Data(contentsOf: bad), Data("{ not json".utf8))
    }

    // MARK: store

    func ev(_ kind: EventKind, _ id: String = "s1") -> AgentEvent { AgentEvent(.claude, id, "/Users/me/dev/demo", kind) }

    func testLifecycleAndAlerts() {
        var st = Store()
        XCTAssertNil(st.apply(ev(.sessionStarted)))
        XCTAssertEqual(st.sessions[0].state, .done) // idle until the first prompt
        XCTAssertNil(st.apply(ev(.working(step: "Bash: ls"))))
        if case .needsInput = st.apply(ev(.needsInput(reason: "permission"))) {} else { XCTFail("expected an alert") }
        XCTAssertNil(st.apply(ev(.needsInput(reason: "permission")))) // not twice
        XCTAssertNil(st.apply(ev(.working(step: "Edit a.rs"))))
        if case .done = st.apply(ev(.done)) {} else { XCTFail("expected an alert") }
        var soft = ev(.needsInput(reason: "idle"))
        soft.soft = true
        XCTAssertNil(st.apply(soft))
        XCTAssertEqual(st.sessions[0].state, .done)
        XCTAssertEqual(st.sessions[0].name, "demo")
    }

    func testFilesDoNotOverrideHooksButTrustedAndTurnEndDo() {
        var st = Store()
        st.apply(ev(.working(step: "x")))
        var file = ev(.done)
        file.live = false
        XCTAssertNil(st.apply(file))
        XCTAssertEqual(st.sessions[0].state, .working)
        file.turnEnd = true
        if case .done = st.apply(file) {} else { XCTFail("turn end should be accepted") }
        st.apply(ev(.working(step: "next")))
        var reg = ev(.needsInput(reason: "approve Bash"))
        reg.live = false
        reg.trusted = true
        reg.open = true
        reg.host = "local_abc"
        st.apply(reg)
        XCTAssertEqual(st.sessions[0].state, .needsInput)
        XCTAssertEqual(st.sessions[0].host, "local_abc")
    }

    func testOpenSessionsStayAndPulseSettles() {
        var st = Store()
        var e = ev(.working(step: "x"))
        e.pid = 7
        e.open = true
        st.apply(e)
        st.apply(ev(.done))
        let now = nowUnix()
        XCTAssertTrue(st.sessions[0].pulsing(now))
        XCTAssertFalse(st.sessions[0].pulsing(now + 120))
        XCTAssertFalse(st.expire(keepSecs: 0, staleSecs: 600, alive: { _ in true })) // open and alive: stays
        XCTAssertEqual(st.sessions.count, 1)
        st.expire(keepSecs: 0, staleSecs: 600, alive: { _ in false })
        XCTAssertTrue(st.sessions.isEmpty)
    }

    func testSubagentsDismissAndWrapper() {
        var st = Store()
        st.apply(ev(.working(step: "x")))
        st.apply(ev(.done))
        var sub = ev(.info)
        sub.sub = SubEvent(id: "a1", kind: "Explore", step: "Grep main")
        st.apply(sub)
        XCTAssertEqual(st.sessions[0].subs.first?.step, "Grep main")
        XCTAssertEqual(st.sessions[0].state, .done) // subagents never change the main state
        sub.sub = SubEvent(id: "a1", kind: "Explore", ended: true)
        st.apply(sub)
        XCTAssertTrue(st.sessions[0].subs.isEmpty)

        st.dismiss("s1")
        st.apply(ev(.done))
        XCTAssertTrue(st.sessions.isEmpty)
        st.apply(ev(.working(step: "again")))
        XCTAssertEqual(st.sessions.count, 1)

        var inner = AgentEvent(.claude, "11111111-2222-3333-4444-555555555555", "/p", .working(step: "x"))
        st.apply(inner)
        var t3 = AgentEvent(.t3code, "thread-1", "/p", .working(step: "Running"))
        t3.alias = "claude:acct:11111111-2222-3333-4444-555555555555"
        st.apply(t3)
        XCTAssertFalse(st.sessions.contains { $0.id == inner.sessionId })
        inner.kind = .done
        st.apply(inner)
        XCTAssertFalse(st.sessions.contains { $0.id == inner.sessionId })
    }

    func testEarlyTitle() {
        var st = Store()
        var info = ev(.info)
        info.title = "Real title"
        st.apply(info)
        XCTAssertTrue(st.sessions.isEmpty)
        st.apply(ev(.working(step: "x")))
        XCTAssertEqual(st.sessions[0].name, "Real title")
    }

    // MARK: Claude hooks and transcripts

    func testClaudeHookEvents() {
        func push(_ s: String) -> [AgentEvent] { Claude.onPush(j(s), pid: 42) }
        var e = push(#"{"session_id":"a","cwd":"/p","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#)
        XCTAssertEqual(e[0].kind, .working(step: "Bash: cargo test"))
        XCTAssertEqual(e[0].pid, 42)
        e = push(#"{"session_id":"a","hook_event_name":"Notification","notification_type":"idle_prompt","message":"x"}"#)
        XCTAssertTrue(e[0].soft)
        XCTAssertTrue(push(#"{"session_id":"a","hook_event_name":"Notification","notification_type":"auth_success"}"#).isEmpty)
        XCTAssertTrue(push(#"{"hook_event_name":"Stop"}"#).isEmpty)
        // subagent tool calls never touch the main session
        e = push(#"{"session_id":"m","agent_id":"sub-7","agent_type":"Explore","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#)
        XCTAssertEqual(e[0].kind, .info)
        XCTAssertEqual(e[0].sub, SubEvent(id: "sub-7", kind: "Explore", step: "Bash: ls"))
        XCTAssertTrue(push(#"{"session_id":"m","agent_id":"sub-7","hook_event_name":"PostToolUse","tool_name":"Bash"}"#).isEmpty)
        XCTAssertEqual(push(#"{"session_id":"m","agent_id":"sub-7","hook_event_name":"SubagentStop"}"#)[0].sub?.ended, true)
        // a turn that ends with a question needs input
        _ = push(#"{"session_id":"q","hook_event_name":"PreToolUse","tool_name":"mcp__ccd_turn__wrap_up","tool_input":{"recap":"r","ask":"Ship it?"}}"#)
        XCTAssertEqual(push(#"{"session_id":"q","hook_event_name":"Stop"}"#)[0].kind, .needsInput(reason: "Ship it?"))
        _ = push(#"{"session_id":"q","hook_event_name":"UserPromptSubmit","prompt":"yes"}"#)
        XCTAssertEqual(push(#"{"session_id":"q","hook_event_name":"Stop"}"#)[0].kind, .done)
        XCTAssertEqual(Claude.toolStep("Edit", j(#"{"file_path":"/x/src/main.rs"}"#)), "Edit main.rs")
        XCTAssertEqual(Claude.toolStep("mcp__github__create_issue", nil), "create_issue (github)")
    }

    func testTranscriptLines() {
        let tool = #"{"type":"assistant","sessionId":"s","cwd":"/p","timestamp":"2026-11-14T10:00:00.000Z","isSidechain":false,"message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#
        let e = Claude.parseLine(tool, mode: .full)!
        XCTAssertEqual(e.kind, .working(step: "Bash: ls"))
        XCTAssertEqual(e.at, 1_794_650_400)
        XCTAssertFalse(e.live)
        XCTAssertNil(Claude.parseLine(tool, mode: .assist))
        let end = #"{"type":"assistant","sessionId":"s","cwd":"/p","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#
        XCTAssertEqual(Claude.parseLine(end, mode: .assist)?.kind, .done)
        XCTAssertEqual(Claude.parseLine(end, mode: .assist)?.turnEnd, true)
        XCTAssertEqual(Claude.parseLine(#"{"type":"system","subtype":"stop_hook_summary","sessionId":"s"}"#, mode: .assist)?.turnEnd, true)
        XCTAssertEqual(Claude.parseLine(#"{"type":"user","sessionId":"s","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#, mode: .assist)?.kind, .done)
        XCTAssertEqual(Claude.parseLine(#"{"type":"user","sessionId":"s","message":{"role":"user","content":"do it"}}"#, mode: .full)?.kind, .working(step: "Thinking…"))
        XCTAssertNil(Claude.parseLine(#"{"type":"assistant","sessionId":"s","isSidechain":true,"message":{"stop_reason":"end_turn","content":[]}}"#, mode: .full))
        XCTAssertEqual(Claude.parseLine(#"{"type":"custom-title","customTitle":"Build notch","sessionId":"s"}"#, mode: .assist)?.title, "Build notch")
        XCTAssertEqual(Claude.parseLine(#"{"type":"bridge-session","bridgeSessionId":"b","sessionId":"s"}"#, mode: .assist)?.remote, true)
        XCTAssertNil(Claude.parseLine(#"{"type":"api-request","sessionId":"s"}"#, mode: .full))
    }

    func testSubagentFilesAndMeta() {
        let root = URL(fileURLWithPath: "/Users/me/.claude/projects")
        let p = root.appendingPathComponent("proj/sess-1/subagents/agent-a1b2.jsonl")
        XCTAssertEqual(Claude.subFile(root: root, p)?.session, "sess-1")
        XCTAssertEqual(Claude.subFile(root: root, p)?.agent, "a1b2")
        XCTAssertEqual(Claude.subFile(root: root, root.appendingPathComponent("proj/sess-1/subagents/agent-a1b2.meta.json"))?.meta, true)
        XCTAssertNil(Claude.subFile(root: root, root.appendingPathComponent("proj/sess-1.jsonl")))
        XCTAssertTrue(Claude.isSessionFile(root: root, root.appendingPathComponent("proj/sess-1.jsonl")))
        XCTAssertFalse(Claude.isSessionFile(root: root, p))
        let m = Claude.subMeta(Data(#"{"agentType":"Explore","description":"Find the bug","finished":true}"#.utf8))
        XCTAssertEqual(m?.kind, "Find the bug (Explore)")
        XCTAssertEqual(m?.finished, true)
    }

    // MARK: registry, usage, other agents

    func testRegistry() {
        let now = nowUnix()
        let data = Data(#"{"pid":4242,"sessionId":"r1","cwd":"/dev/app","name":"Fix login","status":"busy","statusUpdatedAt":\#((now - 600) * 1000),"hostSessionId":"local_abc","entrypoint":"claude-desktop"}"#.utf8)
        var rec = Registry.parse(data)!
        XCTAssertEqual(rec.pid, 4242)
        XCTAssertTrue(rec.hosted)
        XCTAssertNil(Registry.parse(Data(#"{"pid":1,"sessionId":"x","spare":true}"#.utf8)))
        var seen = [String: Registry.Seen]()
        var e = Registry.diff(seen: &seen, records: [rec], now: now)
        XCTAssertEqual(e[0].kind, .working(step: ""))
        XCTAssertEqual(e[0].title, "Fix login")
        XCTAssertEqual(e[0].host, "local_abc")
        XCTAssertTrue(e[0].open && e[0].trusted && now - e[0].at >= 590)
        XCTAssertTrue(Registry.diff(seen: &seen, records: [rec], now: now).isEmpty)
        rec.status = "waiting"
        rec.waitingFor = "approve Bash"
        e = Registry.diff(seen: &seen, records: [rec], now: now)
        XCTAssertEqual(e[0].kind, .needsInput(reason: "approve Bash"))
        XCTAssertEqual(e[0].at, now)
        rec.status = "idle"
        rec.waitingFor = ""
        XCTAssertEqual(Registry.diff(seen: &seen, records: [rec], now: now)[0].kind, .done)
        XCTAssertEqual(Registry.diff(seen: &seen, records: [], now: now)[0].kind, .sessionEnded)
    }

    func testUsageAndCredentials() {
        let u = UsageAPI.parse(Data(#"{"five_hour":{"utilization":42.5,"resets_at":"2026-11-14T15:00:00+00:00"},"seven_day":{"utilization":7,"resets_at":null},"extra_usage":{"is_enabled":false}}"#.utf8))!
        XCTAssertEqual(u.fiveHour, UsageWindow(pct: 42.5, resetsAt: 1_794_668_400))
        XCTAssertEqual(u.week?.resetsAt, nil)
        XCTAssertNil(UsageAPI.parse(Data("{}".utf8)))
        let live = UsageAPI.token(from: Data(#"{"claudeAiOauth":{"accessToken":"abc","expiresAt":\#((nowUnix() + 3600) * 1000)}}"#.utf8))
        XCTAssertEqual(live?.token, "abc")
        XCTAssertEqual(live?.expired, false)
        XCTAssertEqual(UsageAPI.token(from: Data(#"{"claudeAiOauth":{"accessToken":"abc","expiresAt":1000}}"#.utf8))?.expired, true)
    }

    func testCodexAndT3() {
        let e = Codex.onPush(j(#"{"session_id":"c1","cwd":"/p","hook_event_name":"PermissionRequest","tool_name":"shell","tool_input":{"command":["git","push"]}}"#), pid: nil)
        XCTAssertEqual(e[0].kind, .needsInput(reason: "Approval: shell: git push"))
        XCTAssertEqual(Codex.onPush(j(#"{"type":"agent-turn-complete","thread-id":"t9","cwd":"/p"}"#), pid: nil)[0].kind, .done)
        var seen = [String: String]()
        let runs = [T3Code.Run(threadId: "a", status: "running", provider: "claudeAgent_2", title: "Fix"), T3Code.Run(threadId: "b", status: "completed"), T3Code.Run(threadId: "a", status: "completed")]
        let first = T3Code.diff(seen: &seen, runs: runs, first: true, now: nowUnix())
        XCTAssertEqual(first.count, 1)
        XCTAssertEqual(first[0].kind, .working(step: "Running · Claude"))
        XCTAssertTrue(T3Code.diff(seen: &seen, runs: runs, first: false, now: nowUnix()).isEmpty)
        let next = T3Code.diff(seen: &seen, runs: [T3Code.Run(threadId: "a", status: "running", pending: "approval_request")], first: false, now: nowUnix())
        if case .needsInput = next[0].kind {} else { XCTFail("expected needs input") }
    }

    // MARK: files and IPC

    func testTailer() throws {
        let f = tmp("tail").appendingPathComponent("t.jsonl")
        try Data("old-1\nold-2\nold-3\n".utf8).write(to: f)
        let t = Tailer()
        XCTAssertEqual(t.readNew(f, firstTail: 8, maxLine: 1000), ["old-3"])
        let h = try FileHandle(forWritingTo: f)
        _ = try h.seekToEnd()
        try h.write(contentsOf: Data("new-1\nnew-".utf8))
        XCTAssertEqual(t.readNew(f, firstTail: 8, maxLine: 1000), ["new-1"])
        try h.write(contentsOf: Data("2\n".utf8))
        try h.close()
        XCTAssertEqual(t.readNew(f, firstTail: 8, maxLine: 1000), ["new-2"])
        XCTAssertTrue(t.readNew(f, firstTail: 8, maxLine: 1000).isEmpty)
    }

    func testSocketCarriesAMessageAndAnAnswerBack() {
        let path = "/tmp/notch-test-\(ProcessInfo.processInfo.processIdentifier).sock"
        var held: IPC.Reply?
        let got = expectation(description: "message received")
        XCTAssertTrue(IPC.listen(on: path) { data, reply in
            XCTAssertEqual(String(decoding: data, as: UTF8.self), "{\"hello\":1}")
            held = reply
            got.fulfill()
        })
        var answer: String?
        let answered = expectation(description: "answer read")
        DispatchQueue.global().async {
            answer = IPC.send(Data("{\"hello\":1}".utf8), to: path, waitReply: 5)
            answered.fulfill()
        }
        wait(for: [got], timeout: 5)
        held?.send("allow")
        wait(for: [answered], timeout: 5)
        XCTAssertEqual(answer, "allow")
        // nobody listening: returns nil at once
        XCTAssertNil(IPC.send(Data("x".utf8), to: "/tmp/notch-no-such.sock"))
        unlink(path)
    }
}
