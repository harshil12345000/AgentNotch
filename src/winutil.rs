//! Win32 helpers: monitors, registry, theme, fullscreen detection, processes.
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::util::wide;

#[derive(Clone, Default, Debug)]
pub struct Mon {
    pub rc: RECT,
    pub work: RECT,
    pub scale: f32,
    pub name: String,
    pub primary: bool,
}

unsafe extern "system" fn mon_cb(h: HMONITOR, _dc: HDC, _rc: *mut RECT, data: LPARAM) -> BOOL {
    unsafe {
        let out = &mut *(data.0 as *mut Vec<Mon>);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(h, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let len = mi.szDevice.iter().position(|c| *c == 0).unwrap_or(mi.szDevice.len());
            out.push(Mon {
                rc: mi.monitorInfo.rcMonitor,
                work: mi.monitorInfo.rcWork,
                scale: dx as f32 / 96.0,
                name: String::from_utf16_lossy(&mi.szDevice[..len]),
                primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
    }
    TRUE
}

pub fn monitors() -> Vec<Mon> {
    let mut v: Vec<Mon> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(mon_cb), LPARAM(&mut v as *mut _ as isize));
    }
    if v.is_empty() {
        v.push(Mon {
            rc: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            work: RECT { left: 0, top: 0, right: 1920, bottom: 1040 },
            scale: 1.0,
            name: "default".into(),
            primary: true,
        });
    }
    v
}

/// The monitor containing the point, or the closest one.
pub fn nearest(mons: &[Mon], x: f32, y: f32) -> Mon {
    let dist = |m: &Mon| {
        let dx = (m.rc.left as f32 - x).max(0.0).max(x - m.rc.right as f32);
        let dy = (m.rc.top as f32 - y).max(0.0).max(y - m.rc.bottom as f32);
        dx * dx + dy * dy
    };
    mons.iter().min_by(|a, b| dist(a).partial_cmp(&dist(b)).unwrap_or(std::cmp::Ordering::Equal)).cloned().unwrap_or_default()
}

// ------------------------------------------------------------------ registry (HKCU only)

pub fn reg_get_dword(path: &str, name: &str) -> Option<u32> {
    let mut val = 0u32;
    let mut size = 4u32;
    let (p, n) = (wide(path), wide(name));
    unsafe {
        let r = RegGetValueW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), RRF_RT_REG_DWORD, None, Some(&mut val as *mut _ as *mut _), Some(&mut size));
        (r == ERROR_SUCCESS).then_some(val)
    }
}

pub fn reg_get_string(path: &str, name: &str) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let (p, n) = (wide(path), wide(name));
    unsafe {
        let r = RegGetValueW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size));
        if r != ERROR_SUCCESS {
            return None;
        }
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn reg_set_string(path: &str, name: &str, value: &str) -> bool {
    let (p, n, v) = (wide(path), wide(name), wide(value));
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), REG_SZ.0, Some(v.as_ptr() as *const _), (v.len() * 2) as u32) == ERROR_SUCCESS }
}

pub fn reg_delete_value(path: &str, name: &str) {
    let (p, n) = (wide(path), wide(name));
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()));
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

const RUN_NAME: &str = "AgentNotch";
/// The entry's name before the rename.
const RUN_NAME_OLD: &str = "Notch";

pub fn autostart_enabled() -> bool {
    reg_get_string(RUN_KEY, RUN_NAME).is_some() || reg_get_string(RUN_KEY, RUN_NAME_OLD).is_some()
}

/// Start `exe` with Windows.
pub fn set_autostart_to(exe: &std::path::Path) {
    reg_set_string(RUN_KEY, RUN_NAME, &format!("\"{}\"", exe.display()));
    reg_delete_value(RUN_KEY, RUN_NAME_OLD);
}

pub fn set_autostart(on: bool) {
    if on {
        set_autostart_to(&crate::util::exe_path());
    } else {
        reg_delete_value(RUN_KEY, RUN_NAME);
        reg_delete_value(RUN_KEY, RUN_NAME_OLD);
    }
}

/// Windows "app mode" theme. Defaults to dark when the value is missing.
pub fn system_dark() -> bool {
    reg_get_dword("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize", "AppsUseLightTheme").map(|v| v == 0).unwrap_or(true)
}

// ------------------------------------------------------------------ windows / processes

/// True when the foreground window is a borderless window covering the whole of `mon`.
pub fn foreground_is_fullscreen(own: HWND, mon: &RECT) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == own {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls) as usize;
        let cls = String::from_utf16_lossy(&cls[..n]);
        if matches!(
            cls.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "XamlExplorerHostIslandWindow" | "MultitaskingViewFrame" | "Windows.UI.Core.CoreWindow"
        ) {
            return false;
        }
        let style = GetWindowLongW(fg, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == WS_CAPTION.0 {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        r.left <= mon.left && r.top <= mon.top && r.right >= mon.right && r.bottom >= mon.bottom
    }
}

/// Modal message box. With `ask`, shows Yes/No and returns true for Yes.
pub fn message_box(owner: HWND, title: &str, text: &str, ask: bool) -> bool {
    let (t, x) = (wide(title), wide(text));
    let style = if ask { MB_YESNO | MB_ICONQUESTION } else { MB_OK | MB_ICONWARNING };
    unsafe { MessageBoxW(Some(owner), PCWSTR(x.as_ptr()), PCWSTR(t.as_ptr()), style | MB_SETFOREGROUND | MB_TOPMOST) == IDYES }
}

pub fn beep() {
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONASTERISK);
    }
}

/// Give a windows-subsystem process a console to print to when started from a terminal.
pub fn attach_console() {
    use windows::Win32::System::Console::*;
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.map(|h| !h.is_invalid() && !h.0.is_null()).unwrap_or(false) {
            return; // already redirected to a pipe or file
        }
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
            use windows::Win32::Storage::FileSystem::*;
            if let Ok(h) = CreateFileW(w!("CONOUT$"), (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) {
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
                let _ = SetStdHandle(STD_ERROR_HANDLE, h);
            }
        }
    }
}

/// Open a link with whatever app is registered for it.
pub fn open_url(url: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    let u = wide(url);
    unsafe { ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL).0 as isize > 32 }
}

/// Release pages that are no longer needed back to the system.
pub fn trim_working_set() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Seconds since the last keyboard or mouse input.
pub fn idle_secs() -> u64 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    unsafe {
        let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut li).as_bool() { (GetTickCount().wrapping_sub(li.dwTime) / 1000) as u64 } else { 0 }
    }
}

/// Folder picker. None if cancelled.
pub fn pick_folder(owner: HWND) -> Option<String> {
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        s
    }
}

/// Let the running notch take the foreground. Called by the short-lived helper that a
/// toast click starts (which Windows trusts to do so), so the session's window can be
/// raised.
pub fn allow_running_instance_foreground() {
    let title = wide(&window_title());
    unsafe {
        if let Ok(h) = FindWindowW(w!("NotchIsland"), PCWSTR(title.as_ptr())) {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid != 0 {
                let _ = AllowSetForegroundWindow(pid);
            }
        }
    }
}

pub fn window_title() -> String {
    format!("AgentNotch{}", crate::util::instance_suffix())
}

/// Ask an already running island to close cleanly.
pub fn close_running_instance() {
    let title = wide(&window_title());
    unsafe {
        if let Ok(h) = FindWindowW(w!("NotchIsland"), PCWSTR(title.as_ptr())) {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn pid_alive(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => {
                let mut code = 0u32;
                let ok = GetExitCodeProcess(h, &mut code).is_ok();
                let _ = CloseHandle(h);
                ok && code == 259 // STILL_ACTIVE
            }
            Err(e) => e.code() == E_ACCESSDENIED, // exists but protected
        }
    }
}
