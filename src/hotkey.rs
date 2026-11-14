//! Global hide/show hotkey ("Ctrl+\\" by default), stored in the config as text.
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub const HOTKEY_ID: i32 = 1;

const NAMED: [(&str, u32); 14] = [
    ("/", 0xBF),
    ("\\", 0xDC),
    ("`", 0xC0),
    (";", 0xBA),
    ("'", 0xDE),
    (",", 0xBC),
    (".", 0xBE),
    ("-", 0xBD),
    ("=", 0xBB),
    ("[", 0xDB),
    ("]", 0xDD),
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Enter", 0x0D),
];

/// "Ctrl+Shift+K" -> (modifiers, virtual key)
pub fn parse(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key = None;
    // split on '+', but keep a trailing "+" key out of scope: "=" is used for that key
    for part in s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            p => key = Some(key_code(p)?),
        }
    }
    Some((mods, key?))
}

fn key_code(p: &str) -> Option<u32> {
    if let Some((_, vk)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(p)) {
        return Some(*vk);
    }
    let b = p.as_bytes();
    if b.len() == 1 && b[0].is_ascii_alphanumeric() {
        return Some(b[0].to_ascii_uppercase() as u32);
    }
    // f1..f24
    let n: u32 = p.strip_prefix('f')?.parse().ok()?;
    (1..=24).contains(&n).then_some(0x70 + n - 1)
}

fn key_name(vk: u32) -> Option<String> {
    if let Some((n, _)) = NAMED.iter().find(|(_, v)| *v == vk) {
        return Some(n.to_string());
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some((vk as u8 as char).to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        _ => None,
    }
}

/// Build the text for a key press, reading which modifiers are held right now.
/// None for keys that cannot be a hotkey, or with no Ctrl / Alt / Win held (function keys
/// are allowed on their own).
pub fn from_keypress(vk: u32) -> Option<String> {
    let name = key_name(vk)?;
    let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) < 0 };
    let mut out = String::new();
    for (k, n) in [(VK_CONTROL, "Ctrl+"), (VK_MENU, "Alt+"), (VK_SHIFT, "Shift+")] {
        if down(k) {
            out.push_str(n);
        }
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        out.push_str("Win+");
    }
    let strong = out.contains("Ctrl") || out.contains("Alt") || out.contains("Win");
    if !strong && !(0x70..=0x87).contains(&vk) {
        return None;
    }
    out.push_str(&name);
    Some(out)
}

/// Register `combo` as the global hotkey (replacing the previous one). False if the text is
/// not understood or another program already owns that combination.
pub fn register(hwnd: HWND, combo: &str) -> bool {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
        match parse(combo) {
            Some((mods, vk)) => RegisterHotKey(Some(hwnd), HOTKEY_ID, mods | MOD_NOREPEAT, vk).is_ok(),
            None => false,
        }
    }
}

pub fn unregister(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses() {
        assert_eq!(parse("Ctrl+/"), Some((MOD_CONTROL, 0xBF)));
        assert_eq!(parse(&crate::config::Config::default().hotkey), Some((MOD_CONTROL, 0xDC)));
        assert_eq!(parse("ctrl+shift+k"), Some((MOD_CONTROL | MOD_SHIFT, 0x4B)));
        assert_eq!(parse("Alt+F4"), Some((MOD_ALT, 0x73)));
        assert_eq!(parse("Ctrl"), None);
        assert_eq!(parse("Ctrl+Nope"), None);
        assert_eq!(key_name(0xBF).as_deref(), Some("/"));
    }
}
