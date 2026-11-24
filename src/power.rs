//! Keep Awake: "when I close the lid: do nothing" on AC and battery, plus a system/display
//! keep-alive while the toggle is on.
//!
//! Uses the power API that `powercfg /setacvalueindex ... LIDACTION` itself calls (no
//! localized text to parse, no child process). The values that were there before are saved
//! to a journal file first and restored on toggle-off, on quit, or on the next start after
//! a crash.
use windows::core::GUID;
use windows::Win32::Foundation::*;
use windows::Win32::System::Power::*;

// powercfg aliases SUB_BUTTONS and LIDACTION
const SUB_BUTTONS: GUID = GUID::from_u128(0x4f971e89_eebd_4455_a8de_9e59040e7347);
const LIDACTION: GUID = GUID::from_u128(0x5ca83367_6e45_459f_a27b_476b1d01c936);
const DO_NOTHING: u32 = 0;
const SLEEP: u32 = 1;

fn journal() -> std::path::PathBuf {
    crate::util::config_dir().join("power-restore.toml")
}

unsafe fn active_scheme() -> Option<GUID> {
    unsafe {
        let mut p: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut p) != ERROR_SUCCESS || p.is_null() {
            return None;
        }
        let g = *p;
        let _ = LocalFree(Some(HLOCAL(p as *mut _)));
        Some(g)
    }
}

/// Current lid-close action as (on AC, on battery). 0 = do nothing, 1 = sleep, 2 = hibernate, 3 = shut down.
pub fn read_lid() -> Option<(u32, u32)> {
    unsafe {
        let scheme = active_scheme()?;
        let (mut ac, mut dc) = (0u32, 0u32);
        if PowerReadACValueIndex(None, Some(&scheme), Some(&SUB_BUTTONS), Some(&LIDACTION), &mut ac) != ERROR_SUCCESS {
            return None;
        }
        if PowerReadDCValueIndex(None, Some(&scheme), Some(&SUB_BUTTONS), Some(&LIDACTION), &mut dc) != ERROR_SUCCESS.0 {
            return None;
        }
        Some((ac, dc))
    }
}

fn write_lid(ac: u32, dc: u32) -> Result<(), String> {
    unsafe {
        let scheme = active_scheme().ok_or("could not read the active power plan")?;
        let a = PowerWriteACValueIndex(None, &scheme, Some(&SUB_BUTTONS), Some(&LIDACTION), ac);
        let d = PowerWriteDCValueIndex(None, &scheme, Some(&SUB_BUTTONS), Some(&LIDACTION), dc);
        let s = PowerSetActiveScheme(None, Some(&scheme));
        if a == ERROR_SUCCESS && d == ERROR_SUCCESS.0 && s == ERROR_SUCCESS {
            return Ok(());
        }
        if a == ERROR_ACCESS_DENIED || d == ERROR_ACCESS_DENIED.0 {
            return write_lid_elevated(ac, dc);
        }
        Err(format!("power plan change failed (codes {}, {d}, {})", a.0, s.0))
    }
}

/// Last resort when the plan is locked down: run powercfg elevated, for this one change only.
fn write_lid_elevated(ac: u32, dc: u32) -> Result<(), String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let args = crate::util::wide(&format!(
        "/c powercfg /setacvalueindex SCHEME_CURRENT SUB_BUTTONS LIDACTION {ac} && powercfg /setdcvalueindex SCHEME_CURRENT SUB_BUTTONS LIDACTION {dc} && powercfg /setactive SCHEME_CURRENT"
    ));
    let r = unsafe { ShellExecuteW(None, w!("runas"), w!("cmd.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_HIDE) };
    if r.0 as isize > 32 { Ok(()) } else { Err("the power plan needs administrator rights and elevation was declined".into()) }
}

fn hold(on: bool) {
    unsafe {
        // must be called on a thread that stays alive: the UI thread
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS });
    }
}

/// Is anything on this PC (AgentNotch or another app, such as the Claude desktop app's own
/// keep-awake setting) currently asking Windows not to sleep?
pub fn system_held_awake() -> bool {
    unsafe {
        // SystemExecutionState: the combined ES_* flags of every power request
        let mut state = 0u32;
        let st = CallNtPowerInformation(POWER_INFORMATION_LEVEL(16), None, 0, Some(&mut state as *mut _ as *mut _), 4);
        st.is_ok() && state & ES_SYSTEM_REQUIRED.0 != 0
    }
}

/// Is the lid already set to "do nothing" on both AC and battery?
pub fn lid_is_do_nothing() -> bool {
    read_lid() == Some((DO_NOTHING, DO_NOTHING))
}

/// True when AgentNotch itself switched Keep Awake on (a journal with the old values exists).
pub fn ours() -> bool {
    journal().exists()
}

pub fn enable() -> Result<(), String> {
    let cur = read_lid().ok_or("could not read the lid setting")?;
    if cur != (DO_NOTHING, DO_NOTHING) {
        if !journal().exists() {
            std::fs::write(journal(), format!("ac = {}\ndc = {}\n", cur.0, cur.1)).map_err(|e| format!("could not save the current power settings ({e}); nothing was changed"))?;
        }
        if let Err(e) = write_lid(DO_NOTHING, DO_NOTHING) {
            let _ = std::fs::remove_file(journal());
            return Err(e);
        }
    }
    hold(true);
    Ok(())
}

fn read_journal() -> Option<(u32, u32)> {
    let s = std::fs::read_to_string(journal()).ok()?;
    let get = |k: &str| s.lines().find_map(|l| l.strip_prefix(k)?.trim().strip_prefix('=')?.trim().parse::<u32>().ok());
    Some((get("ac")?, get("dc")?))
}

/// Where the lid settings go when Keep Awake is switched off.
///
/// `by_user` = the toggle was clicked: both plugged-in and battery must leave "do nothing",
/// so each side goes back to its saved value, or to sleep (the Windows default) if that
/// side was already "do nothing" before AgentNotch touched it or there is nothing saved.
/// Otherwise (AgentNotch is quitting) the saved values are put back exactly as they were.
pub fn off_target(saved: Option<(u32, u32)>, by_user: bool) -> (u32, u32) {
    let (ac, dc) = saved.unwrap_or((SLEEP, SLEEP));
    if by_user {
        let fix = |v: u32| if v == DO_NOTHING { SLEEP } else { v };
        (fix(ac), fix(dc))
    } else {
        (ac, dc)
    }
}

/// Switch Keep Awake off (see `off_target`).
pub fn disable(by_user: bool) -> Result<(), String> {
    hold(false);
    let target = off_target(read_journal(), by_user);
    if read_lid() != Some(target) {
        write_lid(target.0, target.1)?;
    }
    let _ = std::fs::remove_file(journal());
    Ok(())
}

/// Called at startup: a journal left behind means AgentNotch did not exit cleanly while the
/// toggle was on. Restore the saved values.
pub fn recover_after_crash() {
    if let Some((ac, dc)) = read_journal() {
        match write_lid(ac, dc) {
            Ok(_) => crate::logln!("power: restored lid action after an unclean exit (ac={ac}, dc={dc})"),
            Err(e) => crate::logln!("power: could not restore lid action: {e}"),
        }
        let _ = std::fs::remove_file(journal());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_off_changes_both_sides() {
        // battery was already "do nothing" before AgentNotch: a click still turns it off
        assert_eq!(off_target(Some((SLEEP, DO_NOTHING)), true), (SLEEP, SLEEP));
        // other saved actions (2 = hibernate) come back as they were
        assert_eq!(off_target(Some((2, SLEEP)), true), (2, SLEEP));
        // nothing saved: the Windows default
        assert_eq!(off_target(None, true), (SLEEP, SLEEP));
        // quitting restores exactly what was there
        assert_eq!(off_target(Some((SLEEP, DO_NOTHING)), false), (SLEEP, DO_NOTHING));
    }
}
