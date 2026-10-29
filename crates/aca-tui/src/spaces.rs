//! Workspaces: sessions are split across a few workspaces, each named by one key (a lowercase letter / digit), and you see one
//! at a time: work and life kept apart.
//! `q`+key switches there, `Q`+key sends the session there (on a fold row = the whole group), `[` `]` go back and forth,
//! `` ` `` returns to the previous one, `S` lists them (`^R` renames).
//! `h` is hidden: not listed in `S`, skipped by `[` `]`; you enter it with `q h` (after leaving, `` ` `` gets back too).
//! Where a session is: its own assignment → a subsession follows its parent session → its group's (recorded by `Q` on a fold
//! row) → `1` (see `ui::space_of`).
//! Groups are recorded too so that sessions opened later in that group (including ones opened by `aca new` in a terminal) land
//! there directly, and a group hidden in h doesn't leak out.
//! Keyed by `notes::key` (registered sessions by id, so renames and restarts follow along).
//! Stored in ~/.local/state/aca/ui/spaces, one entry per line: `in\t<session key>\t<workspace>`,
//! `group\t<profile>\t<group>\t<workspace>`, `name\t<workspace>\t<name>` (renamed ones). Unrecognized lines are kept as they
//! are (written by a later version, not lost on save).
//! Like pins, reread on every refresh and before every change, so an aca open on the phone and one on the desktop don't
//! overwrite each other.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::state::{state_file, write_atomic};

/// Sessions never assigned go here
pub const HOME: char = '1';
/// The hidden workspace
pub const HIDDEN: char = 'h';

/// Keys that can name a workspace: lowercase letters and digits. Not q (`q q` quits)
pub fn is_key(c: char) -> bool {
    (c.is_ascii_lowercase() || c.is_ascii_digit()) && c != 'q'
}

/// A group: (profile, group). Registered sessions all have profile `data::REGISTERED`; plain tmux sessions are ("", "-")
pub type Group = (String, String);

#[derive(Clone, Default)]
pub struct Spaces {
    /// None = in memory only (unit tests, --snapshot)
    path: Option<PathBuf>,
    of: BTreeMap<String, char>,
    groups: BTreeMap<Group, char>,
    names: BTreeMap<char, String>,
    /// Unrecognized lines
    other: Vec<String>,
}

impl Spaces {
    pub fn default_path() -> Option<PathBuf> {
        state_file("spaces")
    }

    pub fn open(path: Option<PathBuf>) -> Self {
        let mut s = Spaces { path, ..Spaces::default() };
        s.reload();
        s
    }

    /// Cut loose from the file after reading; later changes stay in memory only (--snapshot feeding keys mustn't write the real file)
    pub fn detached(mut self) -> Self {
        self.path = None;
        self
    }

    pub fn reload(&mut self) {
        let Some(p) = &self.path else { return };
        let (mut of, mut groups, mut names, mut other) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), vec![]);
        let one = |s: &str| {
            let mut cs = s.chars();
            cs.next().filter(|&c| is_key(c) && cs.next().is_none())
        };
        for l in std::fs::read_to_string(p).unwrap_or_default().lines() {
            let f: Vec<&str> = l.split('\t').collect();
            match f.as_slice() {
                ["in", k, c] if !k.is_empty() => {
                    if let Some(c) = one(c) {
                        of.insert(k.to_string(), c);
                    }
                }
                ["group", p, g, c] if !g.is_empty() => {
                    if let Some(c) = one(c) {
                        groups.insert((p.to_string(), g.to_string()), c);
                    }
                }
                ["name", c, n] if !n.is_empty() => {
                    if let Some(c) = one(c) {
                        names.insert(c, n.to_string());
                    }
                }
                // A recognized line that's malformed is dropped; unrecognized ones are kept
                ["in" | "group" | "name", ..] | [""] => {}
                _ => other.push(l.to_string()),
            }
        }
        (self.of, self.groups, self.names, self.other) = (of, groups, names, other);
    }

    /// Where the session itself was assigned (never = None; the caller decides whether it follows the parent session, the group,
    /// or stays in 1)
    pub fn of(&self, key: &str) -> Option<char> {
        self.of.get(key).copied()
    }

    /// Where the group is recorded (never recorded = None)
    pub fn group_of(&self, profile: &str, group: &str) -> Option<char> {
        self.groups.get(&(profile.to_string(), group.to_string())).copied()
    }

    /// Changes memory only (to work out "where each one is after the change"): Some = record it there, None = don't record
    pub fn set(&mut self, key: &str, c: Option<char>) {
        match c {
            Some(c) => self.of.insert(key.to_string(), c),
            None => self.of.remove(key),
        };
    }

    pub fn set_group(&mut self, g: &Group, c: Option<char>) {
        match c {
            Some(c) => self.groups.insert(g.clone(), c),
            None => self.groups.remove(g),
        };
    }

    /// The name shown: the new name if renamed, otherwise the key itself
    pub fn name(&self, c: char) -> String {
        self.names.get(&c).cloned().unwrap_or_else(|| c.to_string())
    }

    pub fn renamed(&self, c: char) -> bool {
        self.names.contains_key(&c)
    }

    /// Change the sessions' and groups' records (Some = record it there, None = don't record); reread first, then save.
    /// keep: the sessions still around; other sessions' records are cleared along the way (deleted sessions don't stay in the
    /// file); groups' aren't, since sessions opened later in a group that went empty still need to land back there
    pub fn assign(
        &mut self,
        sessions: &[(String, Option<char>)],
        groups: &[(Group, Option<char>)],
        keep: &dyn Fn(&str) -> bool,
    ) -> std::io::Result<()> {
        self.reload();
        self.of.retain(|k, _| keep(k));
        for (k, c) in sessions {
            self.set(k, *c);
        }
        for (g, c) in groups {
            self.set_group(g, *c);
        }
        self.save()
    }

    /// These groups were deleted: their records go (a group created later with the same name shouldn't land in the old place);
    /// reread first, then save
    pub fn forget_groups(&mut self, gone: &dyn Fn(&str, &str) -> bool) -> std::io::Result<()> {
        self.reload();
        let n = self.groups.len();
        self.groups.retain(|(p, g), _| !gone(p, g));
        if self.groups.len() == n {
            return Ok(());
        }
        self.save()
    }

    /// A group was renamed (with its subgroups): the recorded groups follow; reread first, then save
    pub fn rename_group(&mut self, profile: &str, from: &str, to: &str) -> std::io::Result<()> {
        self.reload();
        let moved: Vec<(Group, char)> = self
            .groups
            .iter()
            .filter(|((p, g), _)| p == profile && crate::groups::renamed(g, from, to).is_some())
            .map(|(g, c)| (g.clone(), *c))
            .collect();
        if moved.is_empty() {
            return Ok(());
        }
        for ((p, g), c) in moved {
            self.groups.remove(&(p.clone(), g.clone()));
            self.groups.insert((p, crate::groups::renamed(&g, from, to).unwrap()), c);
        }
        self.save()
    }

    /// Rename; empty = back to using the key as the name
    pub fn rename(&mut self, c: char, name: &str) -> std::io::Result<()> {
        self.reload();
        let name = name.trim();
        if name.is_empty() || name == c.to_string() {
            self.names.remove(&c);
        } else {
            // A name can't contain newlines / tabs (one entry per line)
            self.names.insert(c, name.replace(['\t', '\n'], " "));
        }
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        let mut text = String::new();
        for (c, n) in &self.names {
            text.push_str(&format!("name\t{c}\t{n}\n"));
        }
        for ((p, g), c) in &self.groups {
            text.push_str(&format!("group\t{p}\t{g}\t{c}\n"));
        }
        for (k, c) in &self.of {
            text.push_str(&format!("in\t{k}\t{c}\n"));
        }
        for l in &self.other {
            text.push_str(l);
            text.push('\n');
        }
        write_atomic(p, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_instances_share_the_file_and_unknown_lines_survive() {
        let dir = std::env::temp_dir().join(format!("aca-spaces-test-{}", std::process::id()));
        let path = dir.join("spaces");
        let all = |_: &str| true;
        let mut phone = Spaces::open(Some(path.clone()));
        let mut desk = Spaces::open(Some(path.clone()));
        let fun: Group = ("default".into(), "fun".into());
        phone.assign(&[("a".into(), Some('l')), ("b".into(), Some('l'))], &[(fun.clone(), Some('h'))], &all).unwrap();
        desk.rename('l', "生活").unwrap(); // desk doesn't have a b in memory yet; it rereads before writing, so they aren't lost
        let s = Spaces::open(Some(path.clone()));
        assert_eq!((s.of("a"), s.of("b"), s.of("c")), (Some('l'), Some('l'), None));
        assert_eq!((s.group_of("default", "fun"), s.group_of("other", "fun")), (Some('h'), None));
        assert_eq!((s.name('l'), s.name('2')), ("生活".to_string(), "2".to_string()));
        phone.assign(&[("a".into(), None)], &[], &|k| k != "b").unwrap(); // a no longer recorded; b deleted; the group's record stays
        let s = Spaces::open(Some(path.clone()));
        assert_eq!((s.of("a"), s.of("b"), s.group_of("default", "fun")), (None, None, Some('h')));
        phone.rename('l', " ").unwrap(); // empty name = back to the key
        assert!(!Spaces::open(Some(path.clone())).renamed('l'));
        // A group deleted: its records go, other groups' stay
        phone
            .assign(&[], &[(("default".into(), "fun/app".into()), Some('h')), (("default".into(), "life".into()), Some('l'))], &all)
            .unwrap();
        assert_eq!((phone.group_of("default", "fun"), phone.group_of("default", "fun/app")), (Some('h'), Some('h')));
        desk.forget_groups(&|p, g| p == "default" && g.starts_with("fun")).unwrap();
        let s = Spaces::open(Some(path.clone()));
        assert_eq!(
            (s.group_of("default", "fun"), s.group_of("default", "fun/app"), s.group_of("default", "life")),
            (None, None, Some('l'))
        );
        // Malformed lines are dropped; unrecognized ones (written by a later version) are kept as they are on save
        std::fs::write(&path, "in\tx\tq\nin\ty\tAB\nname\t9\t\ngroup\tp\t\t2\nin\tz\t3\nlater\tv\t1\n").unwrap();
        let mut s = Spaces::open(Some(path.clone()));
        assert_eq!((s.of("x"), s.of("y"), s.of("z"), s.renamed('9')), (None, None, Some('3'), false));
        s.assign(&[("w".into(), Some('2'))], &[], &all).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("later\tv\t1\n") && text.contains("in\tw\t2\n") && !text.contains("AB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_lowercase_letters_and_digits_but_not_q() {
        assert!(is_key('a') && is_key('h') && is_key('0') && is_key('9'));
        assert!(!is_key('q') && !is_key('A') && !is_key('[') && !is_key('中'));
    }
}
