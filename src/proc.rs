//! Process table snapshots and window lookup (click-to-focus).
use std::collections::HashMap;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::wide;

/// pid -> (parent pid, exe name lowercased)
pub fn table() -> HashMap<u32, (u32, String)> {
    let mut map = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return map };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                map.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase()));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    map
}

const SHELLS: [&str; 9] = ["bash.exe", "sh.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "conhost.exe", "env.exe", "notch.exe", "openconsole.exe"];

/// Ancestors of this process, nearest first, with the short-lived shells that sit between a
/// hook and the agent removed from the front. The first entry is the agent process.
pub fn agent_chain() -> Vec<u32> {
    let t = table();
    let mut out = Vec::new();
    let mut pid = std::process::id();
    for _ in 0..12 {
        let Some((parent, _)) = t.get(&pid) else { break };
        let parent = *parent;
        let Some((_, name)) = t.get(&parent) else { break };
        if parent == 0 || out.contains(&parent) {
            break;
        }
        if !(out.is_empty() && SHELLS.contains(&name.as_str())) {
            out.push(parent);
        }
        pid = parent;
    }
    out
}

struct Find {
    rank: HashMap<u32, usize>,
    best: Option<(usize, HWND)>,
    title_hint: String,
    by_title: Option<HWND>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let f = &mut *(lp.0 as *mut Find);
        if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok() {
            return TRUE;
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf) as usize;
        if n == 0 {
            return TRUE;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if let Some(&r) = f.rank.get(&pid) {
            if f.best.map(|b| r < b.0).unwrap_or(true) {
                f.best = Some((r, hwnd));
            }
        }
        if f.by_title.is_none() && !f.title_hint.is_empty() {
            let title = String::from_utf16_lossy(&buf[..n]).to_lowercase();
            let mut cls = [0u16; 64];
            let c = GetClassNameW(hwnd, &mut cls) as usize;
            let cls = String::from_utf16_lossy(&cls[..c]);
            if title.contains(&f.title_hint) && matches!(cls.as_str(), "CASCADIA_HOSTING_WINDOW_CLASS" | "Chrome_WidgetWin_1" | "ConsoleWindowClass") {
                f.by_title = Some(hwnd);
            }
        }
        TRUE
    }
}

/// Bring the window that owns a session to the front. Walks from the session's pids up the
/// process tree to the first ancestor that owns a visible top-level window. Falls back to a
/// terminal/editor window whose title contains the project name, then to opening the folder.
pub fn focus_session(pids: &[u32], dir: &str) -> &'static str {
    let t = table();
    let mut rank = HashMap::new();
    if let Some(&start) = pids.iter().find(|p| t.contains_key(p)) {
        let mut pid = start;
        for i in 0..16 {
            let Some((parent, name)) = t.get(&pid) else { break };
            if name == "explorer.exe" {
                break;
            }
            rank.entry(pid).or_insert(i);
            if *parent == 0 || rank.contains_key(parent) {
                break;
            }
            pid = *parent;
        }
    }
    let mut f = Find { rank, best: None, title_hint: crate::util::base_name(dir).to_lowercase(), by_title: None };
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut f as *mut _ as isize));
    }
    let (target, how) = match (f.best, f.by_title) {
        (Some((_, h)), _) => (Some(h), "window"),
        (None, Some(h)) => (Some(h), "title"),
        _ => (None, ""),
    };
    if let Some(h) = target {
        if activate(h) {
            return how;
        }
    }
    if open_folder(dir) { "folder" } else { "none" }
}

fn activate(h: HWND) -> bool {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
        if GetForegroundWindow() == h {
            return true;
        }
        // Windows refuses foreground changes from a process that is not in the foreground
        // unless it just produced input: tap ALT, then retry.
        let key = |flags: KEYBD_EVENT_FLAGS| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_MENU, dwFlags: flags, ..Default::default() } } };
        SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], std::mem::size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(h);
        let _ = BringWindowToTop(h);
        GetForegroundWindow() == h
    }
}

pub fn open_folder(dir: &str) -> bool {
    if dir.is_empty() || !std::path::Path::new(dir).is_dir() {
        return false;
    }
    let d = wide(dir);
    unsafe { ShellExecuteW(None, w!("open"), PCWSTR(d.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize > 32 }
}
