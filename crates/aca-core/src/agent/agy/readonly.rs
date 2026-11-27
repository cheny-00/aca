//! SQLite's Unix VFS normally opens the WAL with CREATE even for a read-only connection.

use std::ffi::{CStr, OsString, c_int};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, ffi};

const NAME: &CStr = c"aca-agy-readonly";

unsafe extern "C" fn open_file(
    _: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    // The main database and existing WAL are the only files this SELECT needs.
    if flags & (ffi::SQLITE_OPEN_MAIN_DB | ffi::SQLITE_OPEN_WAL) == 0 {
        unsafe { (*file).pMethods = std::ptr::null() };
        return ffi::SQLITE_CANTOPEN;
    }
    let flags = (flags & !(ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE)) | ffi::SQLITE_OPEN_READONLY;
    unsafe {
        let unix = ffi::sqlite3_vfs_find(c"unix".as_ptr());
        ((*unix).xOpen.unwrap())(unix, name, file, flags, out)
    }
}

unsafe extern "C" fn deny_delete(_: *mut ffi::sqlite3_vfs, _: ffi::sqlite3_filename, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}

fn registered() -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| unsafe {
        let unix = ffi::sqlite3_vfs_find(c"unix".as_ptr());
        if unix.is_null() {
            return false;
        }
        // SQLite retains the registered VFS for the process lifetime. Keep its native locking methods.
        let vfs = ffi::sqlite3_vfs {
            zName: NAME.as_ptr(),
            pNext: std::ptr::null_mut(),
            xOpen: Some(open_file),
            xDelete: Some(deny_delete),
            ..*unix
        };
        ffi::sqlite3_vfs_register(Box::leak(Box::new(vfs)), 0) == ffi::SQLITE_OK
    })
}

fn uri(path: &Path, query: &[u8]) -> OsString {
    let mut uri = b"file:".to_vec();
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'%' => uri.extend(b"%25"),
            b'?' => uri.extend(b"%3f"),
            b'#' => uri.extend(b"%23"),
            _ => uri.push(b),
        }
    }
    uri.extend(query);
    OsString::from_vec(uri)
}

pub(super) fn title(path: &Path, id: &str) -> Option<String> {
    let read = |vfs: &CStr, query: &[u8]| {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
        let db = Connection::open_with_flags_and_vfs(uri(path, query), flags, vfs)?;
        db.busy_timeout(Duration::from_millis(100))?;
        db.query_row("SELECT title FROM conversation_summaries WHERE conversation_id = ?1 AND title <> ''", [id], |row| row.get(0))
            .optional()
    };
    // A successful WAL lookup is authoritative, including a missing or empty title.
    if registered()
        && let Ok(title) = read(NAME, b"?readonly_shm=1")
    {
        return title;
    }
    // agy closes its writer between updates, leaving just the checkpointed main database.
    read(c"unix", b"?immutable=1").ok().flatten()
}
