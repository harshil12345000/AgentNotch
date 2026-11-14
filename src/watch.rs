//! Directory watching (ReadDirectoryChangesW) and incremental reading of append-only files.
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

/// Block on `dir` (and its subfolders when `recursive`) and call `on_change` with the set of changed files.
/// Changes are coalesced for `settle_ms` so a burst of writes costs one read.
/// Returns only if the directory cannot be watched.
pub fn watch_dir(dir: &Path, recursive: bool, settle_ms: u32, mut on_change: impl FnMut(HashSet<PathBuf>)) {
    let wide: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let Ok(h) = CreateFileW(PCWSTR(wide.as_ptr()), FILE_LIST_DIRECTORY.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None) else {
            crate::logln!("watch: cannot open {}", dir.display());
            return;
        };
        // u32-aligned buffer, as FILE_NOTIFY_INFORMATION requires
        let mut buf = vec![0u32; 16 * 1024];
        loop {
            let mut got = 0u32;
            let ok = ReadDirectoryChangesW(
                h,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 4) as u32,
                recursive,
                FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_FILE_NAME,
                Some(&mut got),
                None,
                None,
            );
            if ok.is_err() {
                crate::logln!("watch: stopped watching {}", dir.display());
                let _ = CloseHandle(h);
                return;
            }
            let mut changed = HashSet::new();
            collect(&buf, got, dir, &mut changed);
            if settle_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(settle_ms as u64));
            }
            if !changed.is_empty() {
                on_change(changed);
            }
        }
    }
}

unsafe fn collect(buf: &[u32], got: u32, dir: &Path, out: &mut HashSet<PathBuf>) {
    if got == 0 {
        return; // overflow: the caller will catch up on the next change
    }
    let base = buf.as_ptr() as *const u8;
    let mut off = 0usize;
    loop {
        unsafe {
            let info = base.add(off) as *const FILE_NOTIFY_INFORMATION;
            let len = (*info).FileNameLength as usize / 2;
            let name = std::slice::from_raw_parts((*info).FileName.as_ptr(), len);
            out.insert(dir.join(String::from_utf16_lossy(name)));
            let next = (*info).NextEntryOffset as usize;
            if next == 0 || off + next >= got as usize {
                break;
            }
            off += next;
        }
    }
}

/// Remembers how far each file has been read and returns only complete new lines.
#[derive(Default)]
pub struct Tailer {
    offsets: HashMap<PathBuf, u64>,
}

impl Tailer {
    /// New complete lines since the last call. The first time a file is seen, reading
    /// starts at most `first_tail` bytes before its end (never the whole file).
    pub fn read_new(&mut self, path: &Path, first_tail: u64, max_line: usize) -> Vec<String> {
        let Ok(mut f) = std::fs::OpenOptions::new().read(true).open(path) else { return Vec::new() };
        let Ok(len) = f.metadata().map(|m| m.len()) else { return Vec::new() };
        let known = self.offsets.get(path).copied();
        let mut start = match known {
            Some(o) if o <= len => o,
            Some(_) => 0, // truncated or replaced
            None => len.saturating_sub(first_tail),
        };
        if start == len {
            self.offsets.insert(path.to_path_buf(), len);
            return Vec::new();
        }
        if f.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let mut data = Vec::with_capacity((len - start) as usize);
        if f.take(len - start).read_to_end(&mut data).is_err() {
            return Vec::new();
        }
        // only consume up to the last newline: a partial line is re-read next time
        let Some(last_nl) = data.iter().rposition(|b| *b == b'\n') else {
            if known.is_none() {
                self.offsets.insert(path.to_path_buf(), start);
            }
            return Vec::new();
        };
        let mut body = &data[..last_nl];
        if known.is_none() && start > 0 {
            // we started mid-file: drop the first, probably partial, line
            match body.iter().position(|b| *b == b'\n') {
                Some(i) => body = &body[i + 1..],
                None => body = &[],
            }
        }
        start += last_nl as u64 + 1;
        self.offsets.insert(path.to_path_buf(), start);
        body.split(|b| *b == b'\n').filter(|l| !l.is_empty() && l.len() <= max_line).filter_map(|l| std::str::from_utf8(l).ok()).map(|l| l.trim_end_matches('\r').to_string()).collect()
    }

    pub fn knows(&self, path: &Path) -> bool {
        self.offsets.contains_key(path)
    }

    /// Mark a file as fully read without parsing it.
    pub fn skip_to_end(&mut self, path: &Path) {
        if let Ok(m) = std::fs::metadata(path) {
            self.offsets.insert(path.to_path_buf(), m.len());
        }
    }
}

/// Files directly matching `pred` under `dir` (recursing at most `depth` levels) that were
/// modified within `max_age_secs`.
pub fn recent_files(dir: &Path, depth: u32, max_age_secs: u64, pred: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 0 {
                recent_files(&p, depth - 1, max_age_secs, pred, out);
            }
        } else if pred(&p) {
            let fresh = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs() <= max_age_secs).unwrap_or(false);
            if fresh {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tails_only_new_complete_lines() {
        let p = std::env::temp_dir().join(format!("notch-tail-{}.jsonl", std::process::id()));
        std::fs::write(&p, "old-1\nold-2\nold-3\n").unwrap();
        let mut t = Tailer::default();
        // first sight: only the tail (drops the partial first line)
        assert_eq!(t.read_new(&p, 8, 1000), vec!["old-3"]);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "new-1\nnew-").unwrap();
        assert_eq!(t.read_new(&p, 8, 1000), vec!["new-1"]);
        writeln!(f, "2").unwrap();
        assert_eq!(t.read_new(&p, 8, 1000), vec!["new-2"]);
        assert!(t.read_new(&p, 8, 1000).is_empty());
        let _ = std::fs::remove_file(&p);
    }
}
