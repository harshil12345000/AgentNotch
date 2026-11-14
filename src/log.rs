//! Tiny rotating file logger. Two files of at most 2 MB each (4 MB total, under the 5 MB cap).
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

const MAX_BYTES: u64 = 2 * 1024 * 1024;
static LOCK: Mutex<()> = Mutex::new(());

pub fn dir() -> std::path::PathBuf {
    let d = crate::util::data_dir().join("logs");
    let _ = fs::create_dir_all(&d);
    d
}

pub fn write(msg: &str) {
    let _g = LOCK.lock();
    let d = dir();
    let cur = d.join("notch.log");
    if fs::metadata(&cur).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = fs::remove_file(d.join("notch.1.log"));
        let _ = fs::rename(&cur, d.join("notch.1.log"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&cur) {
        let t = crate::util::now_unix();
        let (h, m, s) = ((t / 3600) % 24, (t / 60) % 60, t % 60);
        let _ = writeln!(f, "{} {:02}:{:02}:{:02}Z {}", t / 86400, h, m, s, msg);
    }
}

#[macro_export]
macro_rules! logln {
    ($($a:tt)*) => { $crate::log::write(&format!($($a)*)) };
}
