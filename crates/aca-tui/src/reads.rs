//! 已读 / 未读(`u`)。核心给的「跑完了没看」(◐)看的是跑完之后有没有人连进去过、在里面按过键、标过「看过」;
//! 普通 tmux 会话没有这个,「标成未读」核心也没有 —— 所以界面在上面再叠一层自己记的:
//!   已读:从 aca 进去过 / 按 u 标的,记下时刻。之后没有新输出就当已读(○);又跑了一轮、跑完了,照样 ◐。
//!        登记的会话按 u 标已读时,同时记到核心那份上(同 `aca seen`):只靠这里的时刻的话,
//!        「有没有新输出」看的是窗口最后有输出的时刻,屏幕一重画(电脑睡醒)就又算未读了。
//!   未读:按 u 标的,进去之前一直 ◐。
//! 存在 ~/.local/state/aca/ui/read,一行一个:`<键>\t<时刻>` 是已读,`<键>\t-` 是标成未读。
//! 键同便签(`notes::key`):登记的会话按它的 id,改名、重启都跟着。
//! 和置顶一样,每次刷新都重读,改之前先重读,手机和桌面各开一个 aca 不会互相覆盖。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::state::{state_file, write_atomic};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// 这个时刻看过
    Read(u64),
    Unread,
}

#[derive(Default)]
pub struct Reads {
    /// None = 只在内存里(单元测试、--snapshot)
    path: Option<PathBuf>,
    marks: BTreeMap<String, Mark>,
}

impl Reads {
    pub fn default_path() -> Option<PathBuf> {
        state_file("read")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut r = Reads { path, marks: BTreeMap::new() };
        r.reload();
        r
    }

    /// 读完就和文件断开,之后的改动只在内存里(--snapshot 喂按键时不能写真文件)
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    /// 连着文件(不是单元测试、--snapshot 那种只在内存里的)
    pub fn live(&self) -> bool {
        self.path.is_some()
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        self.marks = std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('\t')?;
                let m = if v == "-" { Mark::Unread } else { Mark::Read(v.parse().ok()?) };
                Some((k.to_string(), m))
            })
            .collect();
    }

    /// 什么时候看过的(标成未读的、没看过的 = None)
    pub fn read_at(&self, key: &str) -> Option<u64> {
        match self.marks.get(key) {
            Some(&Mark::Read(t)) => Some(t),
            _ => None,
        }
    }

    pub fn unread(&self, key: &str) -> bool {
        self.marks.get(key) == Some(&Mark::Unread)
    }

    /// 记成 at 时看过。keep:还在的会话,别的顺手清掉(删掉的会话不留在文件里)
    pub fn mark_read(&mut self, key: &str, at: u64, keep: &dyn Fn(&str) -> bool) -> std::io::Result<()> {
        self.set(key, Mark::Read(at), keep)
    }

    pub fn mark_unread(&mut self, key: &str, keep: &dyn Fn(&str) -> bool) -> std::io::Result<()> {
        self.set(key, Mark::Unread, keep)
    }

    /// 文件里记的(没记 = None);`^Z` 撤销 u 时用 `put` 放回去
    pub fn mark(&self, key: &str) -> Option<Mark> {
        self.marks.get(key).copied()
    }

    pub fn put(&mut self, key: &str, m: Option<Mark>, keep: &dyn Fn(&str) -> bool) -> std::io::Result<()> {
        self.set(key, m, keep)
    }

    fn set(&mut self, key: &str, m: impl Into<Option<Mark>>, keep: &dyn Fn(&str) -> bool) -> std::io::Result<()> {
        self.reload();
        self.marks.retain(|k, _| keep(k));
        match m.into() {
            Some(m) => self.marks.insert(key.to_string(), m),
            None => self.marks.remove(key),
        };
        let Some(p) = &self.path else { return Ok(()) };
        let text: String = self
            .marks
            .iter()
            .map(|(k, m)| match m {
                Mark::Read(t) => format!("{k}\t{t}\n"),
                Mark::Unread => format!("{k}\t-\n"),
            })
            .collect();
        write_atomic(p, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_instances_share_the_file_and_drop_gone_sessions() {
        let dir = std::env::temp_dir().join(format!("aca-reads-test-{}", std::process::id()));
        let path = dir.join("read");
        let all = |_: &str| true;
        let mut phone = Reads::open(Some(path.clone()));
        let mut desk = Reads::open(Some(path.clone()));
        phone.mark_read("a", 100, &all).unwrap();
        desk.mark_unread("b", &all).unwrap(); // desk 内存里还没有 a,写之前重读所以 a 不丢
        let r = Reads::open(Some(path.clone()));
        assert_eq!((r.read_at("a"), r.unread("a")), (Some(100), false));
        assert_eq!((r.read_at("b"), r.unread("b")), (None, true));
        phone.mark_unread("a", &|k| k != "b").unwrap(); // b 删掉了
        let r = Reads::open(Some(path.clone()));
        assert!(r.unread("a") && !r.unread("b") && r.read_at("a").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_brings_back_what_was_there() {
        let mut r = Reads::default();
        let all = |_: &str| true;
        r.mark_read("a", 7, &all).unwrap();
        let (a, b) = (r.mark("a"), r.mark("b"));
        r.mark_unread("a", &all).unwrap();
        r.mark_unread("b", &all).unwrap();
        r.put("a", a, &all).unwrap();
        r.put("b", b, &all).unwrap();
        assert_eq!((r.read_at("a"), r.unread("b"), r.mark("b")), (Some(7), false, None));
    }

    #[test]
    fn detached_never_writes() {
        let dir = std::env::temp_dir().join(format!("aca-reads-detached-{}", std::process::id()));
        let path = dir.join("read");
        let mut r = Reads::open(Some(path.clone())).detached();
        r.mark_read("a", 5, &|_| true).unwrap();
        assert_eq!(r.read_at("a"), Some(5));
        assert!(!path.exists());
    }
}
