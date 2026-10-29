//! First-party plugins: the ones under `plugins/` in the repo, built into aca and shipped with it. `aca plugin install
//! aca.delegate` needs no path, and `[plugins."aca.core"]` in plugins.toml (no source) installs on another machine with sync.
//!
//! Installing = writing the built-in files to `~/.local/share/aca/first-party/<name>/`, then registering (source recorded
//! as [`SOURCE`]). aca owns this directory: when an aca upgrade changes the content, it's rewritten the next time an agent
//! starts, sync runs or the plugins page opens; removing the plugin removes the directory too.

use std::path::{Path, PathBuf};

use crate::paths::{self, Res};
use crate::tr;

/// A first-party plugin's source in the registry
pub const SOURCE: &str = "bundled";

pub struct Bundled {
    pub id: &'static str,
    /// The directory name under `plugins/`
    pub dir: &'static str,
    /// (path relative to the plugin directory, content)
    pub files: &'static [(&'static str, &'static [u8])],
}

macro_rules! file {
    ($dir:literal, $path:literal) => {
        ($path, include_bytes!(concat!("../../../../plugins/", $dir, "/", $path)) as &[u8])
    };
}

pub const ALL: &[Bundled] = &[
    Bundled {
        id: "aca.core",
        dir: "core",
        files: &[
            file!("core", "aca-plugin.toml"),
            file!("core", "core.sh"),
            file!("core", "agent/.claude-plugin/plugin.json"),
            file!("core", "agent/skills/wakeup/SKILL.md"),
        ],
    },
    Bundled {
        id: "aca.delegate",
        dir: "delegate",
        files: &[
            file!("delegate", "aca-plugin.toml"),
            file!("delegate", "continue.sh"),
            file!("delegate", "claude/.claude-plugin/plugin.json"),
            file!("delegate", "claude/commands/delegate.md"),
            file!("delegate", "claude/commands/spec.md"),
        ],
    },
    Bundled { id: "aca.gateway", dir: "gateway", files: &[file!("gateway", "aca-plugin.toml")] },
    Bundled { id: "aca.remote", dir: "remote", files: &[file!("remote", "aca-plugin.toml"), file!("remote", "remote.sh")] },
    Bundled { id: "aca.bark", dir: "bark", files: &[file!("bark", "aca-plugin.toml"), file!("bark", "push.sh")] },
    Bundled { id: "aca.discord", dir: "discord", files: &[file!("discord", "aca-plugin.toml"), file!("discord", "post.sh")] },
    Bundled { id: "aca.notify", dir: "notify", files: &[file!("notify", "aca-plugin.toml"), file!("notify", "notify.sh")] },
    Bundled { id: "aca.tailnet", dir: "tailnet", files: &[file!("tailnet", "aca-plugin.toml"), file!("tailnet", "tailnet.sh")] },
    Bundled { id: "aca.copy", dir: "copy", files: &[file!("copy", "aca-plugin.toml"), file!("copy", "copy.sh")] },
];

pub fn known(id: &str) -> bool {
    ALL.iter().any(|b| b.id == id)
}

fn get(id: &str) -> Option<&'static Bundled> {
    ALL.iter().find(|b| b.id == id)
}

pub fn root() -> PathBuf {
    paths::data_dir().join("first-party")
}

/// The written-out directory
pub fn dir_of(id: &str) -> Option<PathBuf> {
    get(id).map(|b| root().join(b.dir))
}

/// Whether the built-in files differ from the directory's (extra files count as different too)
fn stale(b: &Bundled, dir: &Path) -> bool {
    let differs = b.files.iter().any(|(rel, body)| std::fs::read(dir.join(rel)).ok().as_deref() != Some(*body));
    differs || count_files(dir) != b.files.len()
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() { count_files(&p) } else { 1 }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Writes it under [`root`] (left alone if the content is the same) and returns the directory. Written to a temp directory
/// next to it first, then swapped in: a half-written one is never read
pub fn unpack(id: &str) -> Res<PathBuf> {
    let b = get(id).ok_or_else(|| tr!("{id} 不是 aca 自带的插件", "{id} isn't a plugin aca ships with"))?;
    let dir = root().join(b.dir);
    if dir.is_dir() && !stale(b, &dir) {
        return Ok(dir);
    }
    let tmp = root().join(format!(".{}-{}", b.dir, paths::short_id()));
    let write = || -> std::io::Result<()> {
        for (rel, body) in b.files {
            let p = tmp.join(rel);
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(p, body)?;
        }
        Ok(())
    };
    write().map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        tr!("无法写入 {}:{e}", "can't write {}: {e}", tmp.display())
    })?;
    let old = root().join(format!(".{}-old-{}", b.dir, paths::short_id()));
    if dir.exists() {
        std::fs::rename(&dir, &old).map_err(|e| tr!("无法替换 {}:{e}", "can't replace {}: {e}", dir.display()))?;
    }
    std::fs::rename(&tmp, &dir).map_err(|e| tr!("无法替换 {}:{e}", "can't replace {}: {e}", dir.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(dir)
}

/// The built-in manifest (readable without writing it out: for the plugins page's "can install" list)
pub fn manifest(id: &str) -> Option<super::Manifest> {
    let b = get(id)?;
    let (_, body) = b.files.iter().find(|(p, _)| *p == super::MANIFEST)?;
    super::parse(std::str::from_utf8(body).ok()?).ok().map(|(m, _)| m)
}

/// First-party plugins not installed yet (the same id linked from a local directory or installed from git counts as installed)
pub fn available() -> Vec<&'static str> {
    let reg = super::registry();
    ALL.iter().map(|b| b.id).filter(|id| !reg.iter().any(|e| e.id == *id)).collect()
}

/// Rewrites the registered first-party plugins whose content differs from what this aca has built in (aca was just
/// upgraded). Returns the rewritten ones
pub fn refresh() -> Vec<String> {
    super::registry().into_iter().filter(|e| e.source == SOURCE && refresh_one(&e.id)).map(|e| e.id).collect()
}

/// This one: rewrite and re-register if the written copy differs from the built-in one. Also checked before running its
/// commands (right after an upgrade, before the plugins page has been opened or an agent started, what runs is already
/// the new one). Only reads its own few small files
pub fn refresh_one(id: &str) -> bool {
    let (Some(b), Some(dir)) = (get(id), dir_of(id)) else { return false };
    stale(b, &dir) && unpack(id).is_ok() && super::reregister(&dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file of each first-party plugin under plugins/ is built in, the manifest reads, and the id matches
    #[test]
    fn every_file_of_a_first_party_plugin_is_bundled_and_its_manifest_reads() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        for b in ALL {
            let mut on_disk = vec![];
            fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
                for e in std::fs::read_dir(d).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(base, &p, out);
                    } else {
                        out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
                    }
                }
            }
            let base = repo.join(b.dir);
            walk(&base, &base, &mut on_disk);
            on_disk.sort();
            let mut bundled: Vec<String> = b.files.iter().map(|(p, _)| p.to_string()).collect();
            bundled.sort();
            assert_eq!(on_disk, bundled, "plugins/{} 和编进来的文件不一样", b.dir);
            let text = std::str::from_utf8(b.files.iter().find(|(p, _)| *p == super::super::MANIFEST).unwrap().1).unwrap();
            let (m, warn) = super::super::parse(text).unwrap();
            assert_eq!(m.id, b.id);
            assert!(warn.is_empty(), "{}: {warn:?}", b.id);
            assert_eq!(manifest(b.id).map(|m| m.id), Some(b.id.to_string()));
        }
    }
}
