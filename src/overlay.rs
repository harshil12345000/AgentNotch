//! A full-screen reminder that stays until it is dismissed (travel mode's "Turn On Mobile
//! Hotspot"). A plain topmost window painted with GDI: it is shown rarely, so it does not
//! need the Direct2D path, and it costs nothing while it is not on screen.
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;

static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static SCALE_BITS: AtomicU32 = AtomicU32::new(0);
static TEXT: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));
/// The "open settings" button, in client pixels (left, top, right, bottom).
static BUTTON: Mutex<(i32, i32, i32, i32)> = Mutex::new((0, 0, 0, 0));

const BUTTON_LABEL: &str = "Open hotspot settings";
const HINT: &str = "Click anywhere or press Esc to dismiss";
const SETTINGS_LINK: &str = "ms-settings:network-mobilehotspot";

fn font(px: f32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW { lfHeight: -(px.round() as i32), lfWeight: weight, lfQuality: CLEARTYPE_QUALITY, ..Default::default() };
    for (i, c) in "Segoe UI".encode_utf16().enumerate() {
        lf.lfFaceName[i] = c;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

unsafe fn draw_line(dc: HDC, text: &str, f: HFONT, color: u32, rc: &mut RECT) {
    unsafe {
        let old = SelectObject(dc, f.into());
        SetTextColor(dc, COLORREF(color));
        let mut w: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(dc, &mut w, rc, DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
        SelectObject(dc, old);
        let _ = DeleteObject(f.into());
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bg = CreateSolidBrush(COLORREF(0x000C0A0A));
        FillRect(dc, &rc, bg);
        let _ = DeleteObject(bg.into());
        SetBkMode(dc, TRANSPARENT);
        let s = f32::from_bits(SCALE_BITS.load(Ordering::Relaxed)).max(1.0);
        let (title, sub) = TEXT.lock().map(|t| t.clone()).unwrap_or_default();
        let mid = (rc.top + rc.bottom) / 2;
        let px = |v: f32| (v * s).round() as i32;

        let mut r = RECT { left: rc.left, right: rc.right, top: mid - px(110.0), bottom: mid - px(20.0) };
        draw_line(dc, &title, font(64.0 * s, 700), 0x00FFFFFF, &mut r);
        let mut r = RECT { left: rc.left, right: rc.right, top: mid - px(10.0), bottom: mid + px(26.0) };
        draw_line(dc, &sub, font(20.0 * s, 400), 0x00C2B6B4, &mut r);

        // button
        let (bw, bh) = (px(250.0), px(46.0));
        let cx = (rc.left + rc.right) / 2;
        let b = (cx - bw / 2, mid + px(64.0), cx + bw / 2, mid + px(64.0) + bh);
        if let Ok(mut g) = BUTTON.lock() {
            *g = b;
        }
        let pen = CreatePen(PS_SOLID, px(1.0).max(1), COLORREF(0x00FF9B3B));
        let fill = CreateSolidBrush(COLORREF(0x00402A14));
        let (op, ob) = (SelectObject(dc, pen.into()), SelectObject(dc, fill.into()));
        let _ = RoundRect(dc, b.0, b.1, b.2, b.3, bh, bh);
        SelectObject(dc, op);
        SelectObject(dc, ob);
        let _ = DeleteObject(pen.into());
        let _ = DeleteObject(fill.into());
        let mut r = RECT { left: b.0, top: b.1, right: b.2, bottom: b.3 };
        draw_line(dc, BUTTON_LABEL, font(18.0 * s, 600), 0x00FFFFFF, &mut r);

        let mut r = RECT { left: rc.left, right: rc.right, top: rc.bottom - px(70.0), bottom: rc.bottom - px(40.0) };
        draw_line(dc, HINT, font(15.0 * s, 400), 0x008C8482, &mut r);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => paint(hwnd),
            WM_ERASEBKGND => return LRESULT(1),
            WM_LBUTTONUP => {
                let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
                let b = BUTTON.lock().map(|b| *b).unwrap_or_default();
                if x >= b.0 && x < b.2 && y >= b.1 && y < b.3 {
                    crate::winutil::open_url(SETTINGS_LINK);
                }
                let _ = DestroyWindow(hwnd);
            }
            // Esc, Enter, Space
            WM_KEYDOWN if matches!(wp.0, 0x1B | 0x0D | 0x20) => {
                let _ = DestroyWindow(hwnd);
            }
            WM_DESTROY => OVERLAY.store(0, Ordering::Relaxed),
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        LRESULT(0)
    }
}

pub fn close() {
    let h = OVERLAY.swap(0, Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(h as _));
        }
    }
}

/// Cover the monitor `mon` with the reminder until the user dismisses it.
pub fn show(mon: RECT, scale: f32, title: &str, subtitle: &str) {
    close();
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
        let class = w!("NotchOverlay");
        REGISTERED.call_once(|| {
            let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
            RegisterClassW(&wc);
        });
        if let Ok(mut t) = TEXT.lock() {
            *t = (title.to_string(), subtitle.to_string());
        }
        SCALE_BITS.store(scale.to_bits(), Ordering::Relaxed);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class,
            w!("AgentNotch reminder"),
            WS_POPUP,
            mon.left,
            mon.top,
            mon.right - mon.left,
            mon.bottom - mon.top,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            crate::logln!("overlay: could not create the window");
            return;
        };
        OVERLAY.store(hwnd.0 as isize, Ordering::Relaxed);
        // a hair see-through, so it reads as a reminder and not a crash
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 250, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
}
