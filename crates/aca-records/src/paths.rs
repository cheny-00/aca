//! Where what's read from the records is kept between runs (quotas, account colors, handoff text): the interface's state
//! directory, shared with what the interface remembers on its own.

use std::path::{Path, PathBuf};

/// ~/.local/state/aca/ui/<name> (`ACA_HOME` moves it: tests can't touch the real one)
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(aca_core::paths::state_dir().join("ui").join(name))
}

/// Write a temporary file, then rename: when the aca on the phone and the one on the desktop write at once, a reader never sees
/// half a file. The original file's permissions are kept: one tightened to owner-only (it holds secrets) doesn't go back to the
/// default on a rewrite
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    std::fs::write(&tmp, text)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
}
