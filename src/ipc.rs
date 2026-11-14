//! Local IPC. The island listens on \\.\pipe\notch (and optionally 127.0.0.1 for WSL);
//! `notch.exe --hook` is the client. One message per connection:
//!   {"v":1,"agent":"claude","pids":[..],"wsl":"Ubuntu","tok":"..","p":{ ...raw hook JSON... }}
use std::io::Read;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Pipes::*;

use crate::app::{post, UiMsg};

pub fn pipe_name() -> String {
    format!(r"\\.\pipe\notch{}", crate::util::instance_suffix())
}
const MAX_MSG: usize = 4 * 1024 * 1024;

fn handle(bytes: &[u8], need_token: Option<&str>) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        crate::logln!("ipc: dropped malformed message ({} bytes)", bytes.len());
        return;
    };
    if let Some(tok) = need_token {
        if v.get("tok").and_then(|t| t.as_str()) != Some(tok) {
            return;
        }
    }
    let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("claude");
    if agent == "notch" {
        // from `notch.exe --activate`: a toast was clicked
        if let Some(sid) = v.get("p").and_then(|p| p.get("focus")).and_then(|s| s.as_str()) {
            post(UiMsg::Focus(sid.to_string()));
        }
        return;
    }
    let pids: Vec<u32> = v.get("pids").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect()).unwrap_or_default();
    let wsl = v.get("wsl").and_then(|w| w.as_str()).unwrap_or("");
    let Some(payload) = v.get("p") else { return };
    let evs = crate::adapters::on_push(agent, payload, &pids, wsl);
    if !evs.is_empty() {
        post(UiMsg::Events(evs));
    }
}

fn pipe_thread() {
    unsafe {
        let name = crate::util::wide(&pipe_name());
        let h = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            65536,
            0,
            None,
        );
        if h.is_invalid() {
            crate::logln!("ipc: CreateNamedPipe failed: {:?}", GetLastError());
            return;
        }
        let mut buf = vec![0u8; 65536];
        let mut msg: Vec<u8> = Vec::new();
        loop {
            let connected = match ConnectNamedPipe(h, None) {
                Ok(_) => true,
                Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            if connected {
                msg.clear();
                loop {
                    let mut n = 0u32;
                    if ReadFile(h, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
                        break;
                    }
                    if msg.len() + n as usize > MAX_MSG {
                        msg.clear();
                        break;
                    }
                    msg.extend_from_slice(&buf[..n as usize]);
                }
                if !msg.is_empty() {
                    handle(&msg, None);
                }
                if msg.capacity() > 256 * 1024 {
                    msg = Vec::new(); // do not hold on to a large buffer
                }
            }
            let _ = DisconnectNamedPipe(h);
        }
    }
}

/// Runtime file for the TCP fallback: line 1 = port, line 2 = token.
pub fn tcp_info_path() -> std::path::PathBuf {
    crate::util::data_dir().join("ipc")
}

fn tcp_thread() {
    let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
        crate::logln!("ipc: tcp bind failed");
        return;
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    // token from the clock and process id: it only has to stop other local users guessing
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ ((std::process::id() as u128) << 64);
    let token = format!("{:032x}", seed.wrapping_mul(0x9E3779B97F4A7C15F39CC0605CEDC835));
    let _ = std::fs::write(tcp_info_path(), format!("{port}\n{token}\n"));
    crate::logln!("ipc: tcp fallback listening on 127.0.0.1:{port}");
    for conn in listener.incoming() {
        let Ok(mut c) = conn else { continue };
        let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let mut msg = Vec::new();
        let _ = (&mut c).take(MAX_MSG as u64).read_to_end(&mut msg);
        if !msg.is_empty() {
            handle(&msg, Some(&token));
        }
    }
}

pub fn start(tcp: bool) {
    for _ in 0..2 {
        let _ = std::thread::Builder::new().name("pipe".into()).stack_size(128 * 1024).spawn(pipe_thread);
    }
    if tcp {
        let _ = std::thread::Builder::new().name("tcp".into()).stack_size(128 * 1024).spawn(tcp_thread);
    } else {
        let _ = std::fs::remove_file(tcp_info_path());
    }
}

/// Client side: open the pipe. None when the island is not running. Never waits long.
pub fn connect() -> Option<std::fs::File> {
    for attempt in 0..3 {
        match std::fs::OpenOptions::new().write(true).open(pipe_name()) {
            Ok(f) => return Some(f),
            Err(e) => {
                // 231 = ERROR_PIPE_BUSY: both instances are mid-message
                if e.raw_os_error() != Some(231) || attempt == 2 {
                    return None;
                }
                let name = crate::util::wide(&pipe_name());
                unsafe {
                    let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), 15);
                }
            }
        }
    }
    None
}
