//! Backdrop blur ("glass") behind the island.
//!
//! A per-pixel-alpha layered window gets no system backdrop, so we ask DWM for an accent
//! blur through user32's SetWindowCompositionAttribute and clip it to the panel with a
//! window region. That call is undocumented: if it is missing or fails we simply draw a
//! more opaque panel instead.
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

#[repr(C)]
struct AccentPolicy {
    state: i32,
    flags: i32,
    gradient_color: u32,
    animation_id: i32,
}

#[repr(C)]
struct WcaData {
    attribute: u32,
    data: *mut core::ffi::c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: i32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;

/// Turn the blur on or off. `tint` is 0xAABBGGRR. Returns false when unsupported.
pub fn set_blur(hwnd: HWND, on: bool, tint: u32) -> bool {
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return false };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return false };
        let f: unsafe extern "system" fn(HWND, *mut WcaData) -> i32 = std::mem::transmute(f);
        let mut policy = AccentPolicy { state: if on { ACCENT_ENABLE_ACRYLICBLURBEHIND } else { ACCENT_DISABLED }, flags: 0, gradient_color: tint, animation_id: 0 };
        let mut data = WcaData { attribute: WCA_ACCENT_POLICY, data: &mut policy as *mut _ as *mut _, size: std::mem::size_of::<AccentPolicy>() };
        f(hwnd, &mut data) != 0
    }
}

/// Clip the window (and so the blur) to a rounded rectangle, in window pixels.
pub fn set_region(hwnd: HWND, x: i32, y: i32, w: i32, h: i32, radius: i32) {
    unsafe {
        let rgn = CreateRoundRectRgn(x, y, x + w + 1, y + h + 1, radius * 2, radius * 2);
        // the system owns the region after this call
        SetWindowRgn(hwnd, Some(rgn), false);
    }
}

pub fn clear_region(hwnd: HWND) {
    unsafe {
        SetWindowRgn(hwnd, None, false);
    }
}
