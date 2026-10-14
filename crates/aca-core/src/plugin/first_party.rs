//! 第一方插件:仓库 `plugins/` 下的几个,编进 aca 里一起发。`aca plugin install aca.delegate` 不用写路径,
//! plugins.toml 里写 `[plugins."aca.core"]`(不写 source)换台机器 sync 也装得上。
//!
//! 装 = 把编进来的文件写到 `~/.local/share/aca/first-party/<名字>/` 再登记(来源记成 [`SOURCE`])。
//! 这个目录归 aca:升级 aca 后内容变了,下次起 agent、sync、开插件页时照新的重写一遍;删插件时连目录删。

use std::path::{Path, PathBuf};

use crate::paths::{self, Res};
use crate::tr;

/// 登记表里第一方插件的来源
pub const SOURCE: &str = "bundled";

pub struct Bundled {
    pub id: &'static str,
    /// `plugins/` 下的目录名
    pub dir: &'static str,
    /// (相对插件目录的路径, 内容)
    pub files: &'static [(&'static str, &'static [u8])],
}

macro_rules! file {
    ($dir:literal, $path:literal) => {
        ($path, include_bytes!(concat!("../../../../plugins/", $dir, "/", $path)) as &[u8])
    };
}

pub const ALL: &[Bundled] = &[
    Bundled { id: "aca.core", dir: "core", files: &[file!("core", "aca-plugin.toml"), file!("core", "core.sh")] },
    Bundled {
        id: "aca.delegate",
        dir: "delegate",
        files: &[
            file!("delegate", "aca-plugin.toml"),
            file!("delegate", "continue.sh"),
            file!("delegate", "claude/.claude-plugin/plugin.json"),
            file!("delegate", "claude/commands/delegate.md"),
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

/// 写出来的目录
pub fn dir_of(id: &str) -> Option<PathBuf> {
    get(id).map(|b| root().join(b.dir))
}

/// 编进来的和目录里的一不一样(多出来的文件也算不一样)
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

/// 写到 [`root`] 下(内容一样就不动),返回目录。先写到旁边的临时目录再换上:写到一半的不会被读到
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
        tr!("写不了 {}:{e}", "can't write {}: {e}", tmp.display())
    })?;
    let old = root().join(format!(".{}-old-{}", b.dir, paths::short_id()));
    if dir.exists() {
        std::fs::rename(&dir, &old).map_err(|e| tr!("换不了 {}:{e}", "can't replace {}: {e}", dir.display()))?;
    }
    std::fs::rename(&tmp, &dir).map_err(|e| tr!("换不了 {}:{e}", "can't replace {}: {e}", dir.display()))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(dir)
}

/// 编进来的清单(不用写出来就能读:插件页列「可装」的时候)
pub fn manifest(id: &str) -> Option<super::Manifest> {
    let b = get(id)?;
    let (_, body) = b.files.iter().find(|(p, _)| *p == super::MANIFEST)?;
    super::parse(std::str::from_utf8(body).ok()?).ok().map(|(m, _)| m)
}

/// 还没装的第一方插件(同一个 id 链接了本地目录、从 git 装了,也算装了)
pub fn available() -> Vec<&'static str> {
    let reg = super::registry();
    ALL.iter().map(|b| b.id).filter(|id| !reg.iter().any(|e| e.id == *id)).collect()
}

/// 登记了的第一方插件,内容和这个 aca 编进来的不一样(刚升级了 aca)的重写一遍。返回重写了的
pub fn refresh() -> Vec<String> {
    super::registry().into_iter().filter(|e| e.source == SOURCE && refresh_one(&e.id)).map(|e| e.id).collect()
}

/// 这一个:写出来的那份和编进来的不一样就重写、重新登记。跑它的命令之前也查(刚升级完,还没开过插件页、
/// 没起过 agent 时,跑的就已经是新的)。只读它自己那几个小文件
pub fn refresh_one(id: &str) -> bool {
    let (Some(b), Some(dir)) = (get(id), dir_of(id)) else { return false };
    stale(b, &dir) && unpack(id).is_ok() && super::reregister(&dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// plugins/ 下第一方插件的每个文件都编进来了,清单读得懂、id 对得上
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
