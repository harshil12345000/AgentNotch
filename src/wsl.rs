//! WSL support: find ~/.claude inside running distros and install hooks there.
//!
//! Only distros that are already running are touched: opening \\wsl$\<distro> would
//! otherwise boot the distro's VM just to look for a folder.
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn run(args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("wsl.exe").args(args).creation_flags(CREATE_NO_WINDOW).output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// Names of the distros that are running right now.
pub fn running_distros() -> Vec<String> {
    // wsl.exe prints UTF-16LE
    let Some(bytes) = run(&["--list", "--running", "--quiet"]) else { return Vec::new() };
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units).lines().map(|l| l.trim().trim_start_matches('\u{feff}').to_string()).filter(|l| !l.is_empty()).collect()
}

/// $HOME of the default user in a distro, e.g. "/home/you".
pub fn home_of(distro: &str) -> Option<String> {
    let out = run(&["-d", distro, "-e", "sh", "-c", "printf %s \"$HOME\""])?;
    let h = String::from_utf8_lossy(&out).trim().to_string();
    h.starts_with('/').then_some(h)
}

/// Windows path of a Linux path inside a distro.
pub fn unc(distro: &str, linux_path: &str) -> PathBuf {
    PathBuf::from(format!("\\\\wsl$\\{distro}{}", linux_path.replace('/', "\\")))
}

/// (distro, \\wsl$\<distro>\home\<user>\.claude) for every running distro that has one.
pub fn claude_dirs() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for d in running_distros() {
        if let Some(home) = home_of(&d) {
            let p = unc(&d, &format!("{home}/.claude"));
            if p.is_dir() {
                out.push((d, p));
            }
        }
    }
    out
}

/// "C:\Tools\AgentNotch.exe" -> "/mnt/c/Tools/AgentNotch.exe" (the default WSL drive mount).
pub fn to_mnt_path(win: &str) -> Option<String> {
    let b = win.as_bytes();
    if b.len() < 3 || b[1] != b':' {
        return None;
    }
    Some(format!("/mnt/{}{}", (b[0] as char).to_ascii_lowercase(), win[2..].replace('\\', "/")))
}

/// Hook target for a distro: settings.json inside the distro, command = this exe through
/// WSL interop, tagged with the distro so paths can be mapped back.
pub fn hook_target(distro: &str) -> Result<crate::adapters::claude_hooks::Target, String> {
    if !running_distros().iter().any(|d| d.eq_ignore_ascii_case(distro)) {
        return Err(format!("WSL distro '{distro}' is not running (start it first; AgentNotch never boots a distro itself)"));
    }
    let home = home_of(distro).ok_or("could not read $HOME inside the distro")?;
    let exe = to_mnt_path(&crate::util::exe_path().to_string_lossy()).ok_or("AgentNotch.exe is not on a drive WSL can mount")?;
    Ok(crate::adapters::claude_hooks::Target { settings: unc(distro, &format!("{home}/.claude/settings.json")), command: exe, args: vec!["--hook".into(), "--wsl".into(), distro.to_string()], events: &crate::adapters::claude::HOOK_EVENTS, exec_form: true })
}

#[cfg(test)]
mod tests {
    #[test]
    fn mnt_paths() {
        assert_eq!(super::to_mnt_path("C:\\Tools\\AgentNotch.exe").as_deref(), Some("/mnt/c/Tools/AgentNotch.exe"));
        assert_eq!(super::to_mnt_path("relative"), None);
    }
}
