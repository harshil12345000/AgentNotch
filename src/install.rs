//! `notch.exe --install`: put Notch in a permanent place and add shortcuts, without an
//! installer. Everything is per-user; nothing needs administrator rights.
//!
//!   exe        %LOCALAPPDATA%\Programs\Notch\notch.exe
//!   shortcuts  Desktop\Notch.lnk and Start Menu\Programs\Notch.lnk
//!
//! Windows does not let programs pin themselves to the taskbar. Pin the shortcut by hand:
//! right-click it (Start Menu or desktop) and choose "Pin to taskbar".
use std::path::{Path, PathBuf};
use windows::core::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;

use crate::adapters::claude_hooks as hooks;
use crate::util::wide;

pub fn install_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Programs").join("Notch")
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

fn shortcut_paths() -> Vec<PathBuf> {
    [known_folder(&FOLDERID_Desktop), known_folder(&FOLDERID_Programs)].into_iter().flatten().map(|d| d.join("Notch.lnk")).collect()
}

fn write_shortcut(lnk: &Path, target: &Path) -> std::result::Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let (t, dir, desc) = (wide(&target.to_string_lossy()), wide(&target.parent().unwrap_or(Path::new(".")).to_string_lossy()), wide("Notch: your AI coding agents at a glance"));
        link.SetPath(PCWSTR(t.as_ptr())).map_err(|e| e.to_string())?;
        let _ = link.SetWorkingDirectory(PCWSTR(dir.as_ptr()));
        let _ = link.SetDescription(PCWSTR(desc.as_ptr()));
        let _ = link.SetIconLocation(PCWSTR(t.as_ptr()), 0);
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        let l = wide(&lnk.to_string_lossy());
        file.Save(PCWSTR(l.as_ptr()), true).map_err(|e| format!("could not write {}: {e}", lnk.display()))
    }
}

/// Copy `from` to `to`, waiting briefly for a running copy to let go of the file.
fn copy_exe(from: &Path, to: &Path) -> std::result::Result<(), String> {
    let mut last = String::new();
    for _ in 0..30 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    Err(format!("could not copy to {}: {last}", to.display()))
}

pub fn install() -> std::result::Result<String, String> {
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let target = dir.join("notch.exe");
    let me = crate::util::exe_path();
    let mut report = Vec::new();
    // a running notch must exit before its exe can be replaced; it restores Keep Awake on the way out
    crate::winutil::close_running_instance();
    if me != target {
        copy_exe(&me, &target)?;
        report.push(format!("Installed to {}", target.display()));
    } else {
        std::thread::sleep(std::time::Duration::from_millis(800));
    }
    for lnk in shortcut_paths() {
        match write_shortcut(&lnk, &target) {
            Ok(_) => report.push(format!("Shortcut: {}", lnk.display())),
            Err(e) => report.push(format!("Shortcut failed: {e}")),
        }
    }
    // hooks and autostart store the exe path: point them at the installed copy
    let command = target.to_string_lossy().replace('\\', "/");
    let claude = hooks::default_settings();
    if hooks::installed(&claude) {
        let t = hooks::Target { command: command.clone(), ..hooks::windows_target(None) };
        report.push(hooks::install(&t).map(|_| "Claude Code hooks now point at the installed copy".to_string()).unwrap_or_else(|e| format!("Claude Code hooks not updated: {e}")));
    }
    let codex = crate::adapters::codex::codex_home().join("hooks.json");
    if hooks::installed(&codex) {
        let t = hooks::Target { command, ..hooks::codex_target(None) };
        report.push(hooks::install(&t).map(|_| "Codex hooks now point at the installed copy".to_string()).unwrap_or_else(|e| format!("Codex hooks not updated: {e}")));
    }
    if crate::winutil::autostart_enabled() {
        crate::winutil::reg_set_string("Software\\Microsoft\\Windows\\CurrentVersion\\Run", "Notch", &format!("\"{}\"", target.display()));
        report.push("Start with Windows now points at the installed copy".into());
    }
    // no inherited handles: a console that ran the installer must not wait for the notch
    let null = std::process::Stdio::null;
    match std::process::Command::new(&target).stdin(null()).stdout(null()).stderr(null()).spawn() {
        Ok(_) => report.push("Started.".into()),
        Err(e) => report.push(format!("Could not start it: {e}")),
    }
    report.push("To pin it to the taskbar: right-click the Notch shortcut and choose \"Pin to taskbar\" (Windows does not let apps pin themselves).".into());
    Ok(report.join("\n"))
}

/// Remove the shortcuts and the autostart entry. The exe, config and hooks are left alone
/// (remove hooks with --uninstall-hooks).
pub fn uninstall() -> std::result::Result<String, String> {
    let mut report = Vec::new();
    for lnk in shortcut_paths() {
        if std::fs::remove_file(&lnk).is_ok() {
            report.push(format!("Removed {}", lnk.display()));
        }
    }
    if crate::winutil::autostart_enabled() {
        crate::winutil::set_autostart(false);
        report.push("Removed the Start with Windows entry".into());
    }
    report.push(format!("The program itself is still in {} (delete the folder to remove it).", install_dir().display()));
    Ok(report.join("\n"))
}
