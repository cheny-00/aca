//! Seen / unread (`u`). The core's "finished, not seen" (◐) is about whether anyone attached after it finished, pressed
//! keys in it, or marked it "seen"; plain tmux sessions don't have this, and the core has no "mark unread" either, so the
//! interface layers its own record on top:
//!   seen: entered from aca / marked with u, the time is recorded. With no new output after that it counts as seen (○);
//!        if it ran another round and finished, it's ◐ again.
//!        When u marks a registered session seen, it is also recorded on the core's copy (same as `aca seen`): with only
//!        the time here, "is there new output" looks at the window's last output time, and a screen redraw (the computer
//!        waking up) would make it unread again.
//!   unread: marked with u, ◐ until you go in.
//! Stored in ~/.local/state/aca/ui/read, one per line: `<key>\t<time>` is seen, `<key>\t-` is marked unread.
//! Keys as for notes (`notes::key`): a registered session by its id, so renames and restarts follow it.
//! Like pins, reread on every refresh and before every change, so an aca open on the phone and one on the desktop don't
//! overwrite each other.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::state::{state_file, write_atomic};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// Seen at this time
    Read(u64),
    Unread,
}

#[derive(Default)]
pub struct Reads {
    /// None = in memory only (unit tests, --snapshot)
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

    /// Cut off from the file once read; later changes stay in memory (--snapshot feeding keys must not write the real file)
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    /// Backed by the file (not the in-memory kind used by unit tests and --snapshot)
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

    /// When it was seen (marked unread, or never seen = None)
    pub fn read_at(&self, key: &str) -> Option<u64> {
        match self.marks.get(key) {
            Some(&Mark::Read(t)) => Some(t),
            _ => None,
        }
    }

    pub fn unread(&self, key: &str) -> bool {
        self.marks.get(key) == Some(&Mark::Unread)
    }

    /// Record it as seen at `at`. keep: sessions still around; the others are cleared along the way (deleted sessions don't
    /// stay in the file)
    pub fn mark_read(&mut self, key: &str, at: u64, keep: &dyn Fn(&str) -> bool) -> std::io::Result<()> {
        self.set(key, Mark::Read(at), keep)
    }

    pub fn mark_unread(&mut self, key: &str, keep: &dyn Fn(&str) -> bool) -> std::io::Result<()> {
        self.set(key, Mark::Unread, keep)
    }

    /// What the file records (nothing = None); undoing u with `^Z` puts it back with `put`
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
        desk.mark_unread("b", &all).unwrap(); // desk has no a in memory yet; it rereads before writing, so a isn't lost
        let r = Reads::open(Some(path.clone()));
        assert_eq!((r.read_at("a"), r.unread("a")), (Some(100), false));
        assert_eq!((r.read_at("b"), r.unread("b")), (None, true));
        phone.mark_unread("a", &|k| k != "b").unwrap(); // b was deleted
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
