#![windows_subsystem = "windows"]
//! Notch: a tiny always-on-top island for AI coding agent sessions.
mod adapters;
mod app;
mod bench;
mod config;
mod demo;
mod gfx;
mod glass;
mod hook_client;
mod hotkey;
mod install;
mod ipc;
mod log;
mod logos;
mod model;
mod overlay;
mod power;
mod proc;
mod rc;
mod sqlite;
mod svgpath;
mod toast;
mod tray;
mod ui;
mod usage;
mod util;
mod watch;
mod winutil;
mod wsl;

const HELP: &str = "Notch - a tiny island for AI coding agent sessions

  notch.exe                         start the island
  notch.exe --install               copy to %LOCALAPPDATA%\\Programs\\Notch, add Desktop and Start Menu shortcuts, start it
  notch.exe --uninstall             remove the shortcuts and the Start with Windows entry
  notch.exe --bench [secs]          log working set and CPU every second (default 60), then exit
            [--expanded] [--demo]   ... with the panel open / with five demo sessions
  notch.exe --install-hooks         add Notch hooks to ~/.claude/settings.json (backup first)
  notch.exe --uninstall-hooks       remove them again
  notch.exe --hooks-status          say whether they are installed
            [--settings <file>]     ... act on another settings.json
            [--wsl <distro>]        ... act on a running WSL distro
            [--codex]               ... act on Codex (~/.codex/hooks.json) instead of Claude Code
  notch.exe --status                print what Notch detects (lid action, keep-awake, Remote Control, hooks)
  notch.exe --decide <session> allow|deny   answer a pending permission request
  notch.exe --quit                  ask the running island to exit (restores power settings)
  notch.exe --hook                  (used by the hooks) forward stdin JSON to the island
  notch.exe --snapshot <file.bmp>   render the UI with demo data to a bitmap [--expanded|--settings]
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let after = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();

    // Hook client first: it must be fast and silent.
    if has("--hook") || has("--codex-notify") {
        hook_client::run(&args);
        return;
    }

    // A toast (or one of its buttons) was clicked: tell the running notch.
    // `--decide <session> allow|deny` does the same from a script.
    let link = after("--activate").and_then(|url| toast::parse_link(&url)).or_else(|| {
        let sid = after("--decide")?;
        let verb = args.iter().find(|a| *a == "allow" || *a == "deny")?.clone();
        Some((verb, sid))
    });
    if has("--activate") || has("--decide") {
        if let Some((verb, id)) = link {
            winutil::allow_running_instance_foreground();
            let p = match verb.as_str() {
                "focus" => serde_json::json!({ "focus": id }),
                v => serde_json::json!({ "decide": id, "allow": v == "allow" }),
            };
            if let Some(mut pipe) = ipc::connect() {
                use std::io::Write;
                let _ = pipe.write_all(serde_json::json!({"v": 1, "agent": "notch", "p": p}).to_string().as_bytes());
            }
        }
        return;
    }

    if has("--install") || has("--uninstall") {
        winutil::attach_console();
        match if has("--install") { install::install() } else { install::uninstall() } {
            Ok(m) => println!("{m}"),
            Err(e) => {
                println!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if has("--status") {
        winutil::attach_console();
        let lid = power::read_lid();
        let name = |v: u32| ["do nothing", "sleep", "hibernate", "shut down"].get(v as usize).copied().unwrap_or("?");
        match lid {
            Some((ac, dc)) => println!("lid close action:   plugged in = {}, on battery = {}", name(ac), name(dc)),
            None => println!("lid close action:   could not be read"),
        }
        println!("held awake by an app: {}", power::system_held_awake());
        println!("keep awake by Notch: {}", power::ours());
        println!("remote control:      started by Notch = {}, running elsewhere = {}", rc::our_pid().is_some(), rc::external_running());
        println!("claude sessions:     {}", adapters::registry::status_line());
        println!("t3 code:             {}", adapters::t3code::status());
        let codex_hooks = adapters::codex::codex_home().join("hooks.json");
        println!("codex hooks:         {}", if adapters::claude_hooks::installed(&codex_hooks) { "installed" } else { "not installed" });
        let hooks = adapters::claude_hooks::default_settings();
        println!("claude hooks:        {} ({})", if adapters::claude_hooks::installed(&hooks) { "installed" } else { "not installed" }, hooks.display());
        return;
    }

    if has("--quit") {
        winutil::close_running_instance();
        return;
    }

    let hook_cmd = ["--install-hooks", "--uninstall-hooks", "--hooks-status"].into_iter().find(|c| has(c));
    if hook_cmd.is_some() || has("--help") || has("-h") {
        winutil::attach_console();
        let Some(cmd) = hook_cmd else {
            println!("{HELP}");
            return;
        };
        use adapters::claude_hooks as h;
        let settings = after("--settings").map(std::path::PathBuf::from);
        let target = match after("--wsl") {
            Some(distro) => wsl::hook_target(&distro),
            None if has("--codex") => Ok(h::codex_target(settings)),
            None => Ok(h::windows_target(settings)),
        };
        let result = target.and_then(|t| match cmd {
            "--install-hooks" => h::install(&t),
            "--uninstall-hooks" => h::uninstall(&t.settings),
            _ => Ok(format!("{}: {}", t.settings.display(), if h::installed(&t.settings) { "installed" } else { "not installed" })),
        });
        match result {
            Ok(m) => println!("{m}"),
            Err(e) => {
                println!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let opts = app::Opts {
        bench: has("--bench").then(|| after("--bench").and_then(|s| s.parse().ok()).unwrap_or(60)),
        expanded: has("--expanded"),
        demo: has("--demo"),
        snapshot: after("--snapshot"),
        settings: has("--settings"),
        no_prompt: has("--no-prompt"),
        hold_all: has("--hold-all"),
        overlay_test: has("--overlay-test"),
        edge: after("--edge"),
        at: after("--at").and_then(|s| s.parse().ok()),
    };
    std::process::exit(app::run(opts));
}
