//! 你自己的插件:`~/.config/aca/user/aca-plugin.toml`。不用建插件、不用 link —— 放了就有,删了就没
//! (像在 kitty.conf 里写一行 `map`)。写法跟别的插件一样,id 总是 `user`,name、version 可以不写;脚本放在旁边。
//! 要在会话的目录里跑(lazygit 这种)写 `cwd = "session"`。
//!
//! 登记表里记着清单的修改时间:每次读登记表看一眼,变了就重新登记(读不懂也登记一个空的,`aca plugin ls`、
//! 插件页里看得到原因)。plugins.toml 里不用写它:sync 不装它也不删它,`[plugins.user]` 里只认
//! `enabled`、`tools`、`keys`、`settings`。

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::{Entry, MANIFEST};
use crate::paths;

pub const ID: &str = "user";
/// 登记表里它的来源
pub const SOURCE: &str = "user";

pub fn dir() -> PathBuf {
    paths::config_dir().join("user")
}

/// 是不是那个目录(登记的路径是解开链接的,两边都解开了比)
pub fn is_dir(d: &Path) -> bool {
    if d.file_name().is_none_or(|n| n != "user") {
        return false;
    }
    let mine = dir();
    d == mine || d.canonicalize().ok().zip(mine.canonicalize().ok()).is_some_and(|(a, b)| a == b)
}

/// 清单的修改时间;没有清单 = None
fn stamp() -> Option<u64> {
    let t = std::fs::metadata(dir().join(MANIFEST)).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64)
}

/// 登记表跟着那个文件走。改了登记表 = true(调的人要重读)
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
