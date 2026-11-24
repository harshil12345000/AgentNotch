//! Notification-area icon and its menu.
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub enum Cmd {
    None,
    ShowHide,
    Settings,
    Autostart,
    ClearFinished,
    Travel,
    Quit,
}

/// A 32x32 icon drawn in code: dark pill with a blue status dot.
pub fn make_icon() -> HICON {
    const N: i32 = 32;
    let mut px = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // capsule from (9,16) to (23,16), radius 8
            let cx = fx.clamp(9.0, 23.0);
            let d = ((fx - cx).powi(2) + (fy - 16.0).powi(2)).sqrt();
            let a = (8.5 - d).clamp(0.0, 1.0);
            let dd = ((fx - 21.0).powi(2) + (fy - 16.0).powi(2)).sqrt();
            let dot = (3.6 - dd).clamp(0.0, 1.0);
            let mix = |base: f32, top: f32| (base + (top - base) * dot) as u8;
            let i = ((y * N + x) * 4) as usize;
            px[i] = mix(22.0, 255.0);
            px[i + 1] = mix(18.0, 155.0);
            px[i + 2] = mix(18.0, 59.0);
            px[i + 3] = (a * 255.0) as u8;
        }
    }
    let mask = vec![0u8; (N * N / 8) as usize];
    unsafe { CreateIcon(None, N, N, 1, 32, mask.as_ptr(), px.as_ptr()).unwrap_or_default() }
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() };
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.uCallbackMessage = crate::app::WM_TRAY;
    for (i, c) in "AgentNotch".encode_utf16().enumerate() {
        d.szTip[i] = c;
    }
    d
}

pub fn add(hwnd: HWND) {
    let mut d = data(hwnd);
    d.hIcon = make_icon();
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
    }
}

pub fn remove(hwnd: HWND) {
    let d = data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &d);
    }
}

pub fn popup(hwnd: HWND, shown: bool, travel: bool) -> Cmd {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return Cmd::None };
        let _ = AppendMenuW(menu, MF_STRING, 1, if shown { w!("Hide") } else { w!("Show") });
        let _ = AppendMenuW(menu, MF_STRING, 5, w!("Clear finished sessions"));
        let _ = AppendMenuW(menu, MF_STRING | if travel { MF_CHECKED } else { MF_UNCHECKED }, 6, w!("Travel mode"));
        let _ = AppendMenuW(menu, MF_STRING, 2, w!("Settings"));
        let auto = if crate::winutil::autostart_enabled() { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(menu, MF_STRING | auto, 3, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 4, w!("Quit"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 {
            1 => Cmd::ShowHide,
            2 => Cmd::Settings,
            3 => Cmd::Autostart,
            4 => Cmd::Quit,
            5 => Cmd::ClearFinished,
            6 => Cmd::Travel,
            _ => Cmd::None,
        }
    }
}
