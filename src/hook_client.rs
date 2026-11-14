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
    let pids = if wsl.is_empty() { crate::proc::agent_chain() } else { Vec::new() };
    let pid_list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
    let mut msg = Vec::with_capacity(trimmed.len() + 96);
    msg.extend_from_slice(format!("{{\"v\":1,\"agent\":{},\"wsl\":{},\"pids\":[{}],\"p\":", json_str(&agent), json_str(&wsl), pid_list).as_bytes());
    msg.extend_from_slice(trimmed);
    msg.push(b'}');
    use std::io::Write;
    let _ = pipe.write_all(&msg);
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
