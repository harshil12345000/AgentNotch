//! Install / uninstall Notch's hooks in a Claude Code settings.json.
//!
//! Rules: never overwrite or reorder the user's own hooks, back the file up before the
//! first change, and make uninstall put the file back byte for byte when nothing else
//! changed in between.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use super::claude::HOOK_EVENTS;

pub struct Target {
    pub settings: PathBuf,
    /// Program the hook runs (exec form: no shell, so spaces in the path are fine).
    pub command: String,
    pub args: Vec<String>,
}

/// The current user's settings.json on Windows (honours CLAUDE_CONFIG_DIR).
pub fn default_settings() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("settings.json"),
        _ => crate::util::home().join(".claude").join("settings.json"),
    }
}

pub fn windows_target(settings: Option<PathBuf>) -> Target {
    Target { settings: settings.unwrap_or_else(default_settings), command: crate::util::exe_path().to_string_lossy().replace('\\', "/"), args: vec!["--hook".into()] }
}

fn backup_path(settings: &Path) -> PathBuf {
    let mut s = settings.as_os_str().to_owned();
    s.push(".notch-backup");
    PathBuf::from(s)
}
/// Marker meaning "there was no settings.json before we installed".
fn absent_path(settings: &Path) -> PathBuf {
    let mut s = settings.as_os_str().to_owned();
    s.push(".notch-absent");
    PathBuf::from(s)
}

fn is_ours(handler: &Value) -> bool {
    let cmd = handler.get("command").and_then(|c| c.as_str()).unwrap_or("").to_lowercase();
    let in_args = handler.get("args").and_then(|a| a.as_array()).map(|a| a.iter().any(|x| x.as_str() == Some("--hook"))).unwrap_or(false);
    cmd.contains("notch") && (in_args || cmd.contains("--hook"))
}

fn handler(t: &Target) -> Value {
    // async: Claude Code never waits for us. timeout: belt and braces.
    json!({ "type": "command", "command": t.command, "args": t.args, "async": true, "timeout": 5 })
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn parse(bytes: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "settings.json is not UTF-8".to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("settings.json is not valid JSON ({e}); left untouched"))?;
    if v.is_object() { Ok(v) } else { Err("settings.json is not a JSON object; left untouched".into()) }
}

/// Pretty JSON in the style Claude Code writes (2 spaces), keeping the original's line
/// endings and trailing newline.
fn render(v: &Value, like: Option<&[u8]>) -> Vec<u8> {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    let like = like.unwrap_or(b"\n");
    if like.ends_with(b"\n") {
        s.push('\n');
    }
    if like.windows(2).any(|w| w == b"\r\n") {
        s = s.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    s.into_bytes()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".notch-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

/// Remove our handlers. Returns true if anything was removed. Containers that become empty
/// because of the removal are dropped too; anything the user left empty is kept.
fn strip(root: &mut Value) -> bool {
    let mut removed = false;
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else { return false };
    let mut empty_events = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else { continue };
        let mut touched = false;
        groups.retain_mut(|g| {
            let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) else { return true };
            let before = hs.len();
            hs.retain(|h| !is_ours(h));
            if hs.len() != before {
                touched = true;
                !hs.is_empty()
            } else {
                true
            }
        });
        if touched {
            removed = true;
            if groups.is_empty() {
                empty_events.push(event.clone());
            }
        }
    }
    for e in empty_events {
        hooks.shift_remove(&e);
    }
    if removed && hooks.is_empty() {
        if let Some(o) = root.as_object_mut() {
            o.shift_remove("hooks");
        }
    }
    removed
}

pub fn installed(settings: &Path) -> bool {
    let Ok(Some(bytes)) = read(settings) else { return false };
    let Ok(v) = parse(&bytes) else { return false };
    v.get("hooks").and_then(|h| h.as_object()).map(|h| h.values().filter_map(|g| g.as_array()).flatten().filter_map(|g| g.get("hooks").and_then(|x| x.as_array())).flatten().any(is_ours)).unwrap_or(false)
}

pub fn install(t: &Target) -> Result<String, String> {
    let original = read(&t.settings)?;
    let mut root = match &original {
        Some(b) => parse(b)?,
        None => Value::Object(Map::new()),
    };
    // keep the first backup: it is the file as it was before Notch ever touched it
    let (bak, absent) = (backup_path(&t.settings), absent_path(&t.settings));
    if !bak.exists() && !absent.exists() {
        match &original {
            Some(b) => std::fs::write(&bak, b).map_err(|e| format!("cannot write backup {}: {e}", bak.display()))?,
            None => {
                if let Some(dir) = absent.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                std::fs::write(&absent, b"").map_err(|e| format!("cannot write marker {}: {e}", absent.display()))?
            }
        }
    }
    strip(&mut root); // re-install cleanly (for example after the exe moved)
    let obj = root.as_object_mut().ok_or("settings.json is not a JSON object")?;
    let hooks = obj.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or("\"hooks\" in settings.json is not an object; left untouched")?;
    for ev in HOOK_EVENTS {
        let groups = hooks.entry(ev).or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups.as_array_mut().ok_or(format!("hooks.{ev} is not an array; left untouched"))?;
        groups.push(json!({ "hooks": [handler(t)] }));
    }
    write_atomic(&t.settings, &render(&root, original.as_deref()))?;
    Ok(format!("Hooks installed in {} ({} events). Backup: {}", t.settings.display(), HOOK_EVENTS.len(), if bak.exists() { bak.display().to_string() } else { "none (file did not exist)".into() }))
}

pub fn uninstall(settings: &Path) -> Result<String, String> {
    let (bak, absent) = (backup_path(settings), absent_path(settings));
    let Some(current) = read(settings)? else {
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::remove_file(&absent);
        return Ok("No settings.json: nothing to remove.".into());
    };
    let mut root = parse(&current)?;
    if !strip(&mut root) {
        return Ok("Notch hooks are not installed: nothing to remove.".into());
    }
    let mut note = "Hooks removed.";
    if absent.exists() && root.as_object().map(|o| o.is_empty()).unwrap_or(false) {
        std::fs::remove_file(settings).map_err(|e| format!("cannot remove {}: {e}", settings.display()))?;
        note = "Hooks removed; settings.json did not exist before, so it was deleted.";
    } else {
        let exact = read(&bak)?.filter(|b| parse(b).map(|v| v == root).unwrap_or(false));
        match exact {
            Some(b) => {
                write_atomic(settings, &b)?;
                note = "Hooks removed; settings.json restored byte for byte from the backup.";
            }
            None => write_atomic(settings, &render(&root, Some(&current)))?,
        }
    }
    let _ = std::fs::remove_file(&bak);
    let _ = std::fs::remove_file(&absent);
    Ok(note.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("notch-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("settings.json")
    }
    fn target(p: &Path) -> Target {
        Target { settings: p.to_path_buf(), command: "C:/Tools/notch.exe".into(), args: vec!["--hook".into()] }
    }

    const ORIGINAL: &str = "{\r\n    \"permissions\": {\"allow\": [\"Bash(ls)\"]},\r\n    \"hooks\": {\r\n        \"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"node guard.mjs\", \"timeout\": 10}]}]\r\n    },\r\n    \"zeta\": 1\r\n}";

    #[test]
    fn roundtrip_is_byte_identical() {
        let p = tmp("roundtrip");
        std::fs::write(&p, ORIGINAL).unwrap();
        install(&target(&p)).unwrap();
        assert!(installed(&p));
        let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        // the user's hook is still first, ours is appended after it
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "node guard.mjs");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["args"][0], "--hook");
        // key order is preserved
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["permissions", "hooks", "zeta"]);
        // installing twice does not duplicate
        install(&target(&p)).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        uninstall(&p).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), ORIGINAL.as_bytes());
        assert!(!backup_path(&p).exists());
    }

    #[test]
    fn user_edits_between_install_and_uninstall_survive() {
        let p = tmp("edits");
        std::fs::write(&p, ORIGINAL).unwrap();
        install(&target(&p)).unwrap();
        let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        v["model"] = json!("opus");
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        uninstall(&p).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["model"], "opus");
        assert_eq!(v["hooks"].as_object().unwrap().len(), 1);
        assert!(!installed(&p));
    }

    #[test]
    fn missing_file_is_created_and_removed() {
        let p = tmp("missing");
        install(&target(&p)).unwrap();
        assert!(installed(&p));
        uninstall(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn invalid_json_is_left_alone() {
        let p = tmp("invalid");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(install(&target(&p)).is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"{ not json");
    }
}
