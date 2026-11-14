//! `notch.exe --hook`: read the hook JSON from stdin, forward it to the island, exit 0.
//! Runs before any window or graphics setup, prints nothing, and never fails the caller.
use std::io::Read;

const MAX_STDIN: u64 = 4 * 1024 * 1024;

pub fn run(args: &[String]) {
    let after = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let agent = after("--agent").unwrap_or_else(|| "claude".into());
    let wsl = after("--wsl").unwrap_or_default();

    // Codex `notify` passes the JSON as the last argument instead of stdin.
    let payload: Vec<u8> = if let Some(json) = after("--codex-notify") {
        json.into_bytes()
    } else {
        let mut buf = Vec::new();
        let _ = std::io::stdin().lock().take(MAX_STDIN).read_to_end(&mut buf);
        buf
    };
    let trimmed = trim_ascii(&payload);
    if trimmed.first() != Some(&b'{') {
        return;
    }
    // If the island is not running there is nothing to do (and no process snapshot is taken).
    let Some(mut pipe) = crate::ipc::connect() else { return };
    // The process tree is only needed to find the session's window, so it is skipped for
    // the high-volume tool events (the island keeps the pids from the last other event).
    let head = &trimmed[..trimmed.len().min(4096)];
    let noisy = [&b"\"PreToolUse\""[..], &b"\"PostToolUse\""[..]].iter().any(|n| head.windows(n.len()).any(|w| w == *n));
    let pids = if wsl.is_empty() && !noisy { crate::proc::agent_chain() } else { Vec::new() };
    let pid_list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let mut msg = Vec::with_capacity(trimmed.len() + 96);
    // A permission request can be answered from the notch: open a pipe for the answer.
    let is_permission = wsl.is_empty() && agent == "claude" && head.windows(19).any(|w| w == b"\"PermissionRequest\"");
    let reply = if is_permission { Reply::create() } else { None };
    let reply_name = reply.as_ref().map(|r| r.name.clone()).unwrap_or_default();
    msg.extend_from_slice(format!("{{\"v\":1,\"agent\":{},\"wsl\":{},\"pids\":[{}],\"reply\":{},\"p\":", json_str(&agent), json_str(&wsl), pid_list, json_str(&reply_name)).as_bytes());
    msg.extend_from_slice(trimmed);
    msg.push(b'}');
    use std::io::Write;
    let _ = pipe.write_all(&msg);
    drop(pipe); // the island reads to end of stream

    if let Some(reply) = reply {
        // Blocks until the notch answers, the notch goes away, or Claude Code ends this
        // process because the prompt was answered somewhere else.
        let out = match reply.wait(std::time::Duration::from_secs(590)).as_deref() {
            Some("allow") => Some(r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#),
            Some("deny") => Some(r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"Denied from Notch"}}}"#),
            _ => None, // "pass" or nothing: leave the decision to Claude Code's own prompt
        };
        if let Some(json) = out {
            let _ = writeln!(std::io::stdout(), "{json}");
        }
    }
}

/// A one-shot pipe the island writes "allow", "deny" or "pass" to.
struct Reply {
    name: String,
    handle: windows::Win32::Foundation::HANDLE,
}

impl Reply {
    fn create() -> Option<Reply> {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
        use windows::Win32::System::Pipes::*;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let name = format!(r"\\.\pipe\notch-reply-{}-{nanos}", std::process::id());
        let wide = crate::util::wide(&name);
        let handle = unsafe { CreateNamedPipeW(PCWSTR(wide.as_ptr()), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS, 1, 0, 256, 0, None) };
        (!handle.is_invalid()).then_some(Reply { name, handle })
    }

    fn wait(&self, limit: std::time::Duration) -> Option<String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::*;
        use windows::Win32::Storage::FileSystem::ReadFile;
        use windows::Win32::System::Pipes::*;
        let start = std::time::Instant::now();
        let island = crate::util::wide(&crate::ipc::pipe_name());
        let mut buf = [0u8; 64];
        let mut ticks = 0u32;
        while start.elapsed() < limit {
            unsafe {
                let _ = ConnectNamedPipe(self.handle, None); // non-blocking: just lets a writer in
                let mut n = 0u32;
                match ReadFile(self.handle, Some(&mut buf), Some(&mut n), None) {
                    Ok(_) if n > 0 => return Some(String::from_utf8_lossy(&buf[..n as usize]).trim().to_string()),
                    Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => return None, // writer came and went
                    _ => {}
                }
                ticks += 1;
                // every two seconds: is the notch still there?
                if ticks % 20 == 0 && !WaitNamedPipeW(PCWSTR(island.as_ptr()), 1).as_bool() && GetLastError() == ERROR_FILE_NOT_FOUND {
                    return None;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    }
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let s = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let e = b.iter().rposition(|c| !c.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(s);
    // skip a UTF-8 byte order mark
    let s = if b[s..e].starts_with(&[0xEF, 0xBB, 0xBF]) { s + 3 } else { s };
    &b[s..e]
}

fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}
