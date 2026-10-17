//! `d` 删掉、还能 `^Z` 撤回的会话。`d` 先归档(程序和 tmux 会话停掉,登记和对话都留着),记在这里,
//! 列表里就不列了;撤回 = 取消归档。删它的那个界面退出时才真删:退出时起一个脱离终端的 `aca ui --purge` 去删,不耽误退出。
//! 界面被杀掉 / 崩了留下的,下次打开时删。
//! 存在 ~/.local/state/aca/ui/trash,一行一个:`<删它的界面的 pid>\t<profile>\t<会话 id>`;pid 0 = 那个界面已经退出。
//! 和置顶一样每次刷新都重读、改之前先重读,手机和桌面各开一个界面都不列它。

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::state::{state_file, write_atomic};

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub pid: u32,
    pub profile: String,
    pub id: String,
}

#[derive(Default)]
pub struct Trash {
    /// None = 只在内存里(单元测试、--snapshot)
    path: Option<PathBuf>,
    items: Vec<Item>,
}

impl Trash {
    pub fn default_path() -> Option<PathBuf> {
        state_file("trash")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut t = Trash { path, items: vec![] };
        t.reload();
        t
    }

    /// 读完就和文件断开,之后的改动只在内存里
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        self.items = std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let (pid, profile, id) = (f.next()?.parse().ok()?, f.next()?, f.next()?);
                (!id.is_empty()).then(|| Item { pid, profile: profile.into(), id: id.into() })
            })
            .collect();
    }

    fn save(&self) -> std::io::Result<()> {
        match &self.path {
            Some(p) => write_atomic(p, &self.items.iter().map(|i| format!("{}\t{}\t{}\n", i.pid, i.profile, i.id)).collect::<String>()),
            None => Ok(()),
        }
    }

    pub fn has(&self, profile: &str, id: &str) -> bool {
        self.items.iter().any(|i| i.profile == profile && i.id == id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|i| i.id.as_str())
    }

    /// 刚归档的:记成这个 aca 删的
    pub fn add(&mut self, profile: &str, id: &str) -> std::io::Result<()> {
        self.reload();
        self.items.retain(|i| !(i.profile == profile && i.id == id));
        self.items.push(Item { pid: std::process::id(), profile: profile.into(), id: id.into() });
        self.save()
    }

    /// 撤回来了
    pub fn forget(&mut self, profile: &str, id: &str) -> std::io::Result<()> {
        self.reload();
        self.items.retain(|i| !(i.profile == profile && i.id == id));
        self.save()
    }

    /// 这个 aca 不再管它们了(退出 / 撤销记录挤掉了这一条):pid 记成 0,等着真删。one = 只放这一个。返回放了几个
    pub fn release(&mut self, one: Option<(&str, &str)>) -> std::io::Result<usize> {
        self.reload();
        let me = std::process::id();
        let mut n = 0;
        for i in self.items.iter_mut().filter(|i| i.pid == me && one.is_none_or(|(p, id)| i.profile == p && i.id == id)) {
            i.pid = 0;
            n += 1;
        }
        if n > 0 {
            self.save()?;
        }
        Ok(n)
    }

    /// 该真删的(已经放了的、删它的 aca 不在了的)从文件里拿走
    fn take_due(&mut self, alive: impl Fn(u32) -> bool) -> std::io::Result<Vec<Item>> {
        self.reload();
        let (due, keep): (Vec<Item>, Vec<Item>) = std::mem::take(&mut self.items).into_iter().partition(|i| i.pid == 0 || !alive(i.pid));
        self.items = keep;
        if !due.is_empty() {
            self.save()?;
        }
        Ok(due)
    }
}

/// 那个 pid 的进程还在(发 0 号信号试试)
fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// 真删该删的。还归档着才删:期间用 `aca unarchive` 取消了归档的,说明又想要了,不删
pub fn purge() {
    let Ok(due) = Trash::open(Trash::default_path()).take_due(alive) else { return };
    let Ok(store) = aca_core::store::Store::open() else { return };
    for i in due {
        if store.get(&i.id).is_ok_and(|s| s.id == i.id && s.archived > 0) {
            let _ = aca_core::session::remove(&store, &i.id);
        }
    }
}

/// 界面退出时:起一个 `aca ui --purge` 去真删。放进自己的进程组、不接终端,弹窗关了 / 终端断了也照样删完
pub fn purge_detached() {
    use std::os::unix::process::CommandExt;
    let Some(me) = aca_core::agent::self_path() else { return };
    let _ =
        Command::new(me).args(["ui", "--purge"]).process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_admins_share_it_and_only_the_gone_ones_are_due() {
        let dir = std::env::temp_dir().join(format!("aca-trash-test-{}", std::process::id()));
        let path = dir.join("trash");
        let mut desk = Trash::open(Some(path.clone()));
        let mut phone = Trash::open(Some(path.clone()));
        desk.add("default", "a").unwrap();
        desk.add("work", "b").unwrap();
        phone.reload();
        assert!(phone.has("default", "a") && phone.has("work", "b") && !phone.has("work", "a"));
        // 别的 aca(pid 42)删的、它已经不在了
        std::fs::write(&path, format!("{}\n42\tdefault\tc\n", std::fs::read_to_string(&path).unwrap().trim_end())).unwrap();
        let me = std::process::id();
        assert_eq!(phone.take_due(|p| p == me).unwrap().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["c"]);
        // 撤回来的不删;放了的下次就删
        desk.forget("default", "a").unwrap();
        assert_eq!(desk.release(None).unwrap(), 1);
        assert_eq!(desk.release(None).unwrap(), 0);
        assert_eq!(Trash::open(Some(path.clone())).take_due(|_| true).unwrap(), [Item { pid: 0, profile: "work".into(), id: "b".into() }]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_one_leaves_the_rest() {
        let mut t = Trash::default();
        t.add("default", "a").unwrap();
        t.add("default", "b").unwrap();
        assert_eq!(t.release(Some(("default", "a"))).unwrap(), 1);
        assert_eq!(t.items.iter().map(|i| (i.id.as_str(), i.pid == 0)).collect::<Vec<_>>(), [("a", true), ("b", false)]);
        // 坏行扔掉
        let dir = std::env::temp_dir().join(format!("aca-trash-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("trash"), "x\tdefault\ta\n0\tdefault\n0\tdefault\t\n7\tp\tok\n").unwrap();
        assert_eq!(Trash::open(Some(dir.join("trash"))).items, [Item { pid: 7, profile: "p".into(), id: "ok".into() }]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
