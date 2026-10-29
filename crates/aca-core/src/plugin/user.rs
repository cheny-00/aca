//! Your own plugin: `~/.config/aca/user/aca-plugin.toml`. No plugin to create, no link — it's there when the file is,
//! gone when it isn't (like writing a `map` line in kitty.conf). Written like any other plugin; the id is always `user`,
//! name and version can be left out; scripts go next to it. To run in the session's directory (something like lazygit),
//! write `cwd = "session"`.
//!
//! The registry records the manifest's modification time: every read of the registry takes a look, and re-registers it
//! if it changed (an unreadable one is registered empty too, so `aca plugin ls` and the plugins page show why). It doesn't
//! go in plugins.toml: sync neither installs nor removes it, and `[plugins.user]` only honors `enabled`, `tools`, `keys`,
//! `settings`.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::{Entry, MANIFEST};
use crate::paths;

pub const ID: &str = "user";
/// Its source in the registry
pub const SOURCE: &str = "user";

pub fn dir() -> PathBuf {
    paths::config_dir().join("user")
}

/// Whether it's that directory (registered paths have links resolved, so both sides are resolved before comparing)
pub fn is_dir(d: &Path) -> bool {
    if d.file_name().is_none_or(|n| n != "user") {
        return false;
    }
    let mine = dir();
    d == mine || d.canonicalize().ok().zip(mine.canonicalize().ok()).is_some_and(|(a, b)| a == b)
}

/// The manifest's modification time; no manifest = None
fn stamp() -> Option<u64> {
    let t = std::fs::metadata(dir().join(MANIFEST)).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64)
}

/// The registry follows that file. Registry changed = true (the caller has to re-read it)
pub(super) fn refresh(reg: &[Entry]) -> bool {
    let had = reg.iter().find(|e| e.source == SOURCE);
    match (stamp(), had) {
        (None, None) => false,
        (None, Some(_)) => {
            super::drop_user();
            true
        }
        (Some(t), Some(e)) if e.stamp == t => false,
        (Some(t), _) => {
            super::register_user(&dir(), t);
            true
        }
    }
}
