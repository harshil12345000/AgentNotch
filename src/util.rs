//! Small shared helpers: wide strings, well-known folders, time formatting.
use std::path::PathBuf;

/// UTF-16, NUL terminated.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
}

/// Our folder under the given base. A folder from before the rename (Notch) is taken
/// over; if it cannot be moved right now (an old copy still has files open) it is used as
/// it is and moved on a later start.
fn named_dir(base: &str) -> PathBuf {
    let root = PathBuf::from(std::env::var_os(base).unwrap_or_default());
    let (new, old) = (root.join("AgentNotch"), root.join("Notch"));
    if !new.exists() && old.is_dir() && std::fs::rename(&old, &new).is_err() {
        return old;
    }
    let _ = std::fs::create_dir_all(&new);
    new
}

/// %APPDATA%\AgentNotch (config). Created on demand.
pub fn config_dir() -> PathBuf {
    named_dir("APPDATA")
}

/// %LOCALAPPDATA%\AgentNotch (logs, runtime files). Created on demand.
pub fn data_dir() -> PathBuf {
    named_dir("LOCALAPPDATA")
}

/// Suffix that separates a test instance from the real one. Set NOTCH_INSTANCE=name to run
/// a second, isolated notch (own pipe, own single-instance lock, own window title); hook
/// helpers started with the same variable talk to that instance.
pub fn instance_suffix() -> String {
    match std::env::var("NOTCH_INSTANCE") {
        Ok(n) if !n.is_empty() => format!("-{}", n.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect::<String>()),
        _ => String::new(),
    }
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("AgentNotch.exe"))
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "42s", "3m", "1h 02m"
pub fn fmt_dur(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// "1.2M", "340k", "912"
pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// Last path component of a Windows or POSIX path.
pub fn base_name(p: &str) -> &str {
    let t = p.trim_end_matches(['/', '\\']);
    t.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(t)
}

/// Truncate to at most `max` chars, on a char boundary, single line.
pub fn clip(s: &str, max: usize) -> String {
    let one: String = s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
    if one.chars().count() <= max {
        one
    } else {
        let mut t: String = one.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Parse an RFC 3339 timestamp ("2026-11-14T10:20:30.123Z" or with +hh:mm) to unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut t = days * 86400 + h * 3600 + mi * 60 + sec;
    // offset
    let rest = &s[19..];
    if let Some(i) = rest.find(['+', '-']) {
        let off = &rest[i..];
        if off.len() >= 6 {
            let oh = off[1..3].parse::<i64>().ok()?;
            let om = off[4..6].parse::<i64>().ok()?;
            let o = oh * 3600 + om * 60;
            t += if off.starts_with('+') { -o } else { o };
        }
    }
    u64::try_from(t).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-14T00:00:00.000Z"), Some(1_794_614_400));
        assert_eq!(parse_rfc3339("2026-11-14T05:30:00+05:30"), Some(1_794_614_400));
    }
    #[test]
    fn names() {
        assert_eq!(base_name("C:\\a\\b\\"), "b");
        assert_eq!(base_name("/home/u/proj"), "proj");
        assert_eq!(fmt_dur(3725), "1h 02m");
    }
}
