//! Pins: kept by session key (registered id / ordinary tmux session's name) in ~/.local/state/aca/ui/pinned, one per line,
//! earliest pinned on top. Reread on every refresh, so an aca open on the phone and one on the desktop agree; reread before
//! each change, so the two never overwrite each other.

use std::path::PathBuf;

use crate::state::{state_file, write_atomic};

#[derive(Default)]
pub struct Pins {
    /// None = in memory only (unit tests, --snapshot)
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

    /// Disconnect from the file after reading; later changes stay in memory (--snapshot feeding keys must not write the real file)
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

    /// Its position among the pinned; not pinned = None
    pub fn rank(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// The session's tmux name changed (restart): the pin follows, same place
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

    /// Put it back at position at (None = unpinned): for `^Z` undoing p, back to where it was
    pub fn put(&mut self, name: &str, at: Option<usize>) -> std::io::Result<()> {
        self.reload();
        self.names.retain(|n| n != name);
        if let Some(i) = at {
            self.names.insert(i.min(self.names.len()), name.to_string());
        }
        self.save()
    }

    /// Toggle the pin; returns whether it's pinned afterwards. A new pin goes after the existing ones, whose positions don't change
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
        assert!(desk.toggle("b").unwrap()); // desk doesn't have a in memory yet; it rereads before writing, so a isn't lost
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
        p.rename("zz", "yy").unwrap(); // not pinned: left alone
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
        p.put("z", Some(9)).unwrap(); // fewer before it: goes last
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
