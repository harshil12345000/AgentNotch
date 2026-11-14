//! Read-only SQLite queries through winsqlite3.dll, the copy of SQLite that ships with
//! Windows 10 and 11. Loaded on first use, so nothing is bundled and nothing is loaded
//! unless an adapter needs it.
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::Path;
use std::sync::OnceLock;

use windows::core::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

struct Api {
    open_v2: unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int,
    close: unsafe extern "system" fn(*mut c_void) -> c_int,
    busy_timeout: unsafe extern "system" fn(*mut c_void, c_int) -> c_int,
    prepare_v2: unsafe extern "system" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int,
    step: unsafe extern "system" fn(*mut c_void) -> c_int,
    column_text: unsafe extern "system" fn(*mut c_void, c_int) -> *const u8,
    column_bytes: unsafe extern "system" fn(*mut c_void, c_int) -> c_int,
    finalize: unsafe extern "system" fn(*mut c_void) -> c_int,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib = LoadLibraryW(w!("winsqlite3.dll")).ok()?;
        macro_rules! sym {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(lib, s!($name))?)
            };
        }
        Some(Api {
            open_v2: sym!("sqlite3_open_v2"),
            close: sym!("sqlite3_close"),
            busy_timeout: sym!("sqlite3_busy_timeout"),
            prepare_v2: sym!("sqlite3_prepare_v2"),
            step: sym!("sqlite3_step"),
            column_text: sym!("sqlite3_column_text"),
            column_bytes: sym!("sqlite3_column_bytes"),
            finalize: sym!("sqlite3_finalize"),
        })
    })
    .as_ref()
}

const OPEN_READONLY: c_int = 1;
const ROW: c_int = 100;

/// Run one SELECT against a database opened read-only and return every row as text.
/// None if SQLite is unavailable, the file cannot be opened, or the statement fails
/// (for example because the schema changed).
pub fn query(db: &Path, sql: &str, cols: usize) -> Option<Vec<Vec<String>>> {
    let api = api()?;
    let path = CString::new(db.to_string_lossy().replace('\\', "/")).ok()?;
    let sql = CString::new(sql).ok()?;
    unsafe {
        let mut handle: *mut c_void = std::ptr::null_mut();
        if (api.open_v2)(path.as_ptr(), &mut handle, OPEN_READONLY, std::ptr::null()) != 0 {
            if !handle.is_null() {
                (api.close)(handle);
            }
            return None;
        }
        (api.busy_timeout)(handle, 400);
        let mut stmt: *mut c_void = std::ptr::null_mut();
        let mut out = None;
        if (api.prepare_v2)(handle, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) == 0 && !stmt.is_null() {
            let mut rows = Vec::new();
            while (api.step)(stmt) == ROW && rows.len() < 500 {
                let mut row = Vec::with_capacity(cols);
                for i in 0..cols as c_int {
                    let p = (api.column_text)(stmt, i);
                    let n = (api.column_bytes)(stmt, i).max(0) as usize;
                    row.push(if p.is_null() { String::new() } else { String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned() });
                }
                rows.push(row);
            }
            out = Some(rows);
        }
        if !stmt.is_null() {
            (api.finalize)(stmt);
        }
        (api.close)(handle);
        out
    }
}
