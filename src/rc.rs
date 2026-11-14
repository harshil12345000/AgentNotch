//! Claude Code Remote Control: start `claude remote-control` in a terminal, track it, stop it,
//! and notice when Remote Control is already running on this PC.
//!
//! Command checked against https://code.claude.com/docs/en/remote-control (2026-11-14):
//! `claude remote-control` runs a Remote Control server in the current folder.
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::*;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

fn pid_file() -> std::path::PathBuf {
    crate::util::data_dir().join("rc.pid")
}

/// Pid of the terminal shell we started, if it is still alive.
pub fn our_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file()).ok()?.trim().trim_start_matches('\u{feff}').parse().ok()?;
    crate::winutil::pid_alive(pid).then_some(pid)
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Base64 of UTF-16LE text, for `powershell -EncodedCommand` (no quoting pitfalls).
fn encoded(script: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `\\wsl$\Ubuntu\home\u\p` -> (Some("Ubuntu"), "/home/u/p")
fn split_wsl(dir: &str) -> (Option<String>, String) {
    for prefix in ["\\\\wsl$\\", "\\\\wsl.localhost\\"] {
        if let Some(rest) = dir.strip_prefix(prefix) {
            let (distro, path) = rest.split_once('\\').unwrap_or((rest, ""));
            return (Some(distro.to_string()), format!("/{}", path.replace('\\', "/")));
        }
    }
    (None, dir.to_string())
}

/// Open a terminal in `dir` running Remote Control. `terminal` is "wt" | "powershell" | "wsl".
pub fn start(dir: &str, terminal: &str, wsl_distro: &str) -> std::result::Result<(), String> {
    let _ = std::fs::remove_file(pid_file());
    let (dir_distro, linux_dir) = split_wsl(dir);
    let use_wsl = terminal == "wsl" || dir_distro.is_some();
    let run = if use_wsl {
        let distro = dir_distro.unwrap_or_else(|| wsl_distro.to_string());
        let d = if distro.is_empty() { String::new() } else { format!("-d {} ", ps_quote(&distro)) };
        // a login shell so that claude is on PATH
        format!("wsl.exe {d}--cd {} -- bash -lic 'claude remote-control'", ps_quote(&linux_dir))
    } else {
        format!("Set-Location -LiteralPath {}; claude remote-control", ps_quote(dir))
    };
    let script = format!(
        "$PID | Set-Content -LiteralPath {} -Encoding ascii; $host.UI.RawUI.WindowTitle = 'Claude Remote Control'; {run}; if ($LASTEXITCODE -ne 0) {{ Read-Host 'Remote Control exited with an error. Press Enter to close' }}",
        ps_quote(&pid_file().to_string_lossy())
    );
    let enc = encoded(&script);
    let ps_args = ["-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &enc];
    let start_dir = if use_wsl || !std::path::Path::new(dir).is_dir() { crate::util::home() } else { std::path::PathBuf::from(dir) };
    let via_wt = terminal != "powershell";
    let spawn_ps = || Command::new("powershell.exe").args(ps_args).current_dir(&start_dir).creation_flags(CREATE_NEW_CONSOLE).spawn();
    let res = if via_wt {
        Command::new("wt.exe").args(["-w", "0", "new-tab", "--title", "Claude Remote Control", "-d"]).arg(&start_dir).arg("powershell.exe").args(ps_args).creation_flags(CREATE_NO_WINDOW).spawn().or_else(|_| spawn_ps())
    } else {
        spawn_ps()
    };
    res.map(|_| ()).map_err(|e| format!("could not open a terminal: {e}"))
}

/// Stop the Remote Control we started (the shell and everything under it).
pub fn stop() {
    if let Some(pid) = our_pid() {
        let _ = Command::new("taskkill.exe").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(CREATE_NO_WINDOW).status();
    }
    let _ = std::fs::remove_file(pid_file());
}

/// Command line of another process of this user (None if it cannot be read).
fn command_line(pid: u32) -> Option<String> {
    unsafe {
        let ntdll = GetModuleHandleW(w!("ntdll.dll")).ok()?;
        let f = GetProcAddress(ntdll, s!("NtQueryInformationProcess"))?;
        let f: unsafe extern "system" fn(HANDLE, u32, *mut core::ffi::c_void, u32, *mut u32) -> i32 = std::mem::transmute(f);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // ProcessCommandLineInformation = 60: a UNICODE_STRING followed by its characters
        let mut buf = vec![0u64; 2048];
        let mut len = 0u32;
        let mut st = f(h, 60, buf.as_mut_ptr() as *mut _, (buf.len() * 8) as u32, &mut len);
        if st as u32 == 0xC000_0004 && len > 0 {
            buf = vec![0u64; len as usize / 8 + 2];
            st = f(h, 60, buf.as_mut_ptr() as *mut _, (buf.len() * 8) as u32, &mut len);
        }
        let _ = CloseHandle(h);
        if st != 0 {
            return None;
        }
        let base = buf.as_ptr() as *const u8;
        let chars = *(base as *const u16) as usize / 2;
        let ptr = *(base.add(8) as *const *const u16);
        if ptr.is_null() || chars == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, chars)))
    }
}

/// Is a Claude Code process on this PC running with Remote Control switched on from its
/// command line (`claude remote-control`, `--remote-control`, `--rc`)?
pub fn external_running() -> bool {
    let ours = our_pid();
    crate::proc::table().iter().any(|(pid, (parent, name))| {
        if !matches!(name.as_str(), "claude.exe" | "node.exe" | "bun.exe") || Some(*parent) == ours {
            return false;
        }
        command_line(*pid).map(|c| is_rc_command(&c)).unwrap_or(false)
    })
}

fn is_rc_command(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    c.contains("claude") && (c.contains(" remote-control") || c.contains("--remote-control") || c.split_whitespace().any(|a| a == "--rc"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helpers() {
        assert_eq!(encoded("hi"), "aABpAA==");
        assert_eq!(split_wsl("\\\\wsl$\\Ubuntu\\home\\u\\p"), (Some("Ubuntu".into()), "/home/u/p".into()));
        assert_eq!(split_wsl("C:\\p").0, None);
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert!(is_rc_command("\"C:\\x\\node.exe\" C:\\npm\\claude\\cli.js remote-control --name x"));
        assert!(is_rc_command("claude.exe --rc"));
        assert!(!is_rc_command("node server.js --rcfile x"));
    }
}
