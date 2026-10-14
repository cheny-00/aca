//! 置顶:按会话的键(登记的 id / 普通 tmux 会话的名字)记在 ~/.local/state/aca/ui/pinned,一行一个,先置顶的在上面。
//! 每次刷新都重读,手机和桌面各开一个 aca 也一致;改之前先重读,两边不会互相覆盖。

use std::path::PathBuf;

use crate::state::{state_file, write_atomic};

#[derive(Default)]
pub struct Pins {
    /// None = 只在内存里(单元测试、--snapshot)
    path: Option<PathBuf>,
    names: Vec<String>,
}

impl Pins {
    pub fn default_path() -> Option<PathBuf> {
        state_file("pinned")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut p = Pins { path, names: vec![] };
        p.reload();
        p
    }

    /// 读完就和文件断开,之后的改动只在内存里(--snapshot 喂按键时不能写真文件)
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        self.names.clear();
        for l in std::fs::read_to_string(p).unwrap_or_default().lines().map(str::trim) {
            if !l.is_empty() && !self.names.iter().any(|n| n == l) {
                self.names.push(l.to_string());
            }
        }
    }

    /// 第几个置顶的,没置顶 = None
    pub fn rank(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// 会话换了 tmux 名(重启):置顶跟过去,位置不变
    pub fn rename(&mut self, old: &str, new: &str) -> std::io::Result<()> {
        self.reload();
        let Some(i) = self.rank(old) else { return Ok(()) };
        if self.rank(new).is_some() {
            self.names.remove(i);
        } else {
            self.names[i] = new.to_string();
        }
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        match &self.path {
            Some(p) => write_atomic(p, &self.names.iter().map(|n| format!("{n}\n")).collect::<String>()),
            None => Ok(()),
        }
    }

    /// 放回第 at 个(None = 不置顶):`^Z` 撤销 p 时用,回到原来的位置
    pub fn put(&mut self, name: &str, at: Option<usize>) -> std::io::Result<()> {
        self.reload();
        self.names.retain(|n| n != name);
        if let Some(i) = at {
            self.names.insert(i.min(self.names.len()), name.to_string());
        }
        self.save()
    }

    /// 切换置顶,返回切换后是不是置顶。新置顶的排在已置顶的后面,已有的编号不变
    pub fn toggle(&mut self, name: &str) -> std::io::Result<bool> {
        self.reload();
        let pinned = match self.rank(name) {
            Some(i) => {
                self.names.remove(i);
                false
            }
            None => {
                self.names.push(name.to_string());
                true
            }
        };
        self.save()?;
        Ok(pinned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_instances_share_the_file_without_clobbering() {
        let dir = std::env::temp_dir().join(format!("aca-pins-test-{}", std::process::id()));
        let path = dir.join("pinned");
        let mut phone = Pins::open(Some(path.clone()));
        let mut desk = Pins::open(Some(path.clone()));
        assert!(phone.toggle("a").unwrap());
        assert!(desk.toggle("b").unwrap()); // desk 内存里还没有 a,写之前重读所以 a 不丢
        assert_eq!(Pins::open(Some(path.clone())).names, ["a", "b"]);
        assert!(!phone.toggle("a").unwrap());
        assert_eq!(Pins::open(Some(path.clone())).names, ["b"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_keeps_the_place() {
        let mut p = Pins::default();
        for n in ["a", "b", "c"] {
            p.toggle(n).unwrap();
        }
        p.rename("b", "b2").unwrap();
        p.rename("zz", "yy").unwrap(); // 没置顶的不管
        assert_eq!(p.names, ["a", "b2", "c"]);
    }

    #[test]
    fn put_returns_to_the_old_place() {
        let mut p = Pins::default();
        for n in ["a", "b", "c"] {
            p.toggle(n).unwrap();
        }
        p.toggle("b").unwrap();
        p.put("b", Some(1)).unwrap();
        assert_eq!(p.names, ["a", "b", "c"]);
        p.put("c", None).unwrap();
        p.put("z", Some(9)).unwrap(); // 前面的少了就排最后
        assert_eq!(p.names, ["a", "b", "z"]);
    }

    #[test]
    fn detached_never_writes() {
        let dir = std::env::temp_dir().join(format!("aca-pins-detached-{}", std::process::id()));
        let path = dir.join("pinned");
        let mut p = Pins::open(Some(path.clone())).detached();
        assert!(p.toggle("a").unwrap());
        assert_eq!(p.rank("a"), Some(0));
        assert!(!path.exists());
    }
}
