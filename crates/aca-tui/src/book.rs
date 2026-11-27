//! A group's notebook, as the interface holds it. The notes themselves are in the registry (aca-core's store.rs; `aca note`
//! reads and writes the same ones): this keeps what the last refresh read, and is where the interface changes them.
//!
//! What's being written is saved as it's typed (when a phone disconnects aca is killed outright, it never gets to exit), under
//! one of two keys (notes.rs): `note:<id>` is that note; `group:<path>` is a note being started in that group, made at the
//! first thing written and the same one from then on, until [`Book::done`].
//!
//! A group used to have one note, kept with the sessions' in the notes file. Those are moved into the registry, each as a note
//! of that group written by you ([`adopt`]): when the interface opens, on every refresh after (an older interface still open
//! goes on writing them there), and before `aca note` reads.

use std::rc::Rc;

use aca_core::paths::Res;
use aca_core::store::{Note, Store};

use crate::notes::{Notes, group_key, group_of, note_key, note_of};

pub struct Book {
    /// Shared with the trash page, which lists the deleted ones from it (see [`Book::gone`])
    store: Rc<Store>,
    /// It's the registry on disk. Otherwise a copy in memory (unit tests, --snapshot, the printed list): nothing done to it
    /// leaves this process
    disk: bool,
    rows: Vec<Note>,
    /// The note started under a `group:<path>` key: (that key, its id), once something was written
    fresh: Option<(String, String)>,
    /// The key whose last save was blank: when the writing is done, that note goes
    blank: Option<String>,
}

impl Default for Book {
    fn default() -> Self {
        Book { store: Rc::new(Store::memory()), disk: false, rows: vec![], fresh: None, blank: None }
    }
}

impl Book {
    /// The registry's notes, read and written in place. If the registry can't be opened: an empty one in memory
    pub fn open() -> Book {
        let mut b = match Store::open() {
            Ok(store) => Book { store: Rc::new(store), disk: true, ..Book::default() },
            Err(_) => Book::default(),
        };
        b.reload();
        b
    }

    /// The registry's notes as they are now, copied into memory: changes stay in this process
    pub fn copy() -> Book {
        let mut b = Book::default();
        let from = Store::open();
        let read = |f: fn(&Store) -> Res<Vec<Note>>| from.as_ref().ok().and_then(|s| f(s).ok()).unwrap_or_default();
        // Last listed first: ones changed within the same second keep their order (the one written later comes first). The
        // ones deleted on their own come along: the trash page lists them
        for n in read(Store::notes).iter().rev().chain(read(Store::notes_gone).iter().rev()) {
            let _ = b.store.note_put(n);
        }
        // The task tickets too: they're read from the same registry (see tickets.rs)
        for t in from.as_ref().ok().and_then(|s| s.tasks().ok()).unwrap_or_default() {
            let _ = b.store.task_put(&t);
        }
        b.reload();
        b
    }

    /// The registry the notes are in (the one on disk, or the copy in memory): what else the interface reads of it comes
    /// from the same one
    pub fn store(&self) -> Rc<Store> {
        self.store.clone()
    }

    /// Reads the notes deleted on their own, as they are when called (most recently deleted first)
    pub fn gone(&self) -> impl Fn() -> Vec<Note> + 'static {
        let store = self.store.clone();
        move || store.notes_gone().unwrap_or_default()
    }

    pub fn reload(&mut self) {
        // Not readable right now: keep showing what was read last
        if let Ok(v) = self.store.notes() {
            self.rows = v;
        }
    }

    /// One group's notes (not its subgroups'): pinned first, then the most recently changed
    pub fn of<'a>(&'a self, group: &'a str) -> impl Iterator<Item = &'a Note> {
        self.rows.iter().filter(move |n| n.group == group)
    }

    /// Pin a note, or unpin it
    pub fn pin(&mut self, id: &str, on: bool) -> Res<()> {
        self.store.note_pin(id, on)?;
        self.reload();
        Ok(())
    }

    /// Delete a note: out of its notebook, kept a while (writing it again under its key brings it back)
    pub fn bin(&mut self, id: &str) -> Res<()> {
        self.store.note_bin(id)?;
        self.reload();
        Ok(())
    }

    /// Bring a deleted note back, as the trash page's job does in the registry (`ops::restore`)
    #[cfg(test)]
    pub fn unbin(&mut self, id: &str) -> Res<()> {
        self.store.note_unbin(id)?;
        self.reload();
        Ok(())
    }

    /// The note at the top of a group's notebook
    pub fn top(&self, group: &str) -> Option<&Note> {
        self.rows.iter().find(|n| n.group == group)
    }

    /// A note by its id (one that's listed: not deleted)
    pub fn get(&self, id: &str) -> Option<&Note> {
        self.rows.iter().find(|n| n.id == id)
    }

    /// The id of the note a key is of now: `note:<id>` is that one (if it's listed); `group:<path>` the one started under it, if
    /// anything was written yet
    pub fn id_of(&self, key: &str) -> Option<&str> {
        match note_of(key) {
            Some(id) => self.get(id).map(|n| n.id.as_str()),
            None => self.fresh.as_ref().filter(|(k, _)| k == key).map(|(_, id)| id.as_str()),
        }
    }

    /// What the note under this key says now
    pub fn text(&self, key: &str) -> Option<&str> {
        self.id_of(key).and_then(|id| self.get(id)).map(|n| n.text.as_str())
    }

    /// Which group's notebook a key writes in
    #[cfg(test)]
    pub fn group_of<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        match note_of(key) {
            Some(id) => self.get(id).map(|n| n.group.as_str()),
            None => group_of(key),
        }
    }

    /// Save what's written under `key` so far. Blank writes nothing yet: the note stays as it last was until [`Book::done`]
    pub fn save(&mut self, key: &str, text: &str) -> Res<()> {
        if text.trim().is_empty() {
            self.blank = Some(key.to_string());
            return Ok(());
        }
        self.blank = None;
        // Nothing changed (opened and left as it was): not a change, it keeps its place and its age
        if self.text(key) == Some(text) {
            return Ok(());
        }
        let started = self.fresh.as_ref().filter(|(k, _)| k == key).map(|(_, id)| id.clone());
        match (note_of(key), group_of(key), started) {
            // A deleted one written again is back
            (Some(id), ..) => self.store.note_set(id, text)?,
            (None, _, Some(id)) => self.store.note_set(&id, text)?,
            (None, Some(g), None) => self.fresh = Some((key.to_string(), self.store.note_add(g, text, "")?.id)),
            (None, None, None) => {}
        }
        self.reload();
        Ok(())
    }

    /// The writing under `key` is over. Left blank: a note that was there before is deleted (kept a while, as deleted notes
    /// are), one started just now was never there. Returns the key the note goes by from here on; None = there's no note
    pub fn done(&mut self, key: &str) -> Option<String> {
        let blank = self.blank.take().is_some_and(|k| k == key);
        let fresh = self.fresh.take_if(|f| f.0 == key).map(|(_, id)| id);
        let id = match (note_of(key), fresh) {
            (Some(id), _) => {
                if blank {
                    let _ = self.store.note_bin(id);
                }
                id.to_string()
            }
            (None, Some(id)) => {
                if blank {
                    let _ = self.store.note_remove(&id);
                }
                id
            }
            (None, None) => return None,
        };
        self.reload();
        self.get(&id).map(|n| note_key(&n.id))
    }

    /// Write a new note into a group's notebook, by you
    #[cfg(test)]
    pub fn add(&mut self, group: &str, text: &str) -> Res<Note> {
        let n = self.store.note_add(group, text, "")?;
        self.reload();
        Ok(n)
    }

    /// Take in the groups' notes still in the notes file (see [`adopt`])
    pub fn adopt(&mut self, notes: &mut Notes) {
        // A copy in memory doesn't take them out of a file on disk: they'd be in neither
        let take = self.disk || !notes.attached();
        if adopt(&self.store, notes, take) > 0 {
            self.reload();
        }
    }
}

/// A note taken from the file this recently (seconds) may still be being typed there
const TYPING: u64 = 600;

/// Move the groups' notes still in the notes file into the registry, each as a note of that group written by you, pinned:
/// it goes on showing beside the group's row, where the group's one note always showed. One whose
/// text that group already has isn't written twice (the last move may have stopped halfway). An older interface saves a
/// group's note there as it's typed, so what was taken a moment ago may be the start of what's there now (or the other way
/// round, when it's being cut back): that's the same note written on, not another one. `take`: they're taken out of the file
/// once written. Returns how many the file had
pub fn adopt(store: &Store, notes: &mut Notes, take: bool) -> usize {
    let old = notes.of_groups();
    let now = aca_core::paths::now();
    for (group, text) in &old {
        let Ok(there) = store.notes_in(group) else { continue };
        let typed_on = |n: &&Note| {
            n.by.is_empty()
                && now.saturating_sub(n.updated) < TYPING
                && (text.starts_with(n.text.as_str()) || n.text.starts_with(text.as_str()))
        };
        let written = match there.iter().find(|n| n.text == *text) {
            Some(_) => true,
            None => match there.iter().find(typed_on) {
                Some(n) => store.note_set(&n.id, text).is_ok(),
                None => store.note_add(group, text, "").and_then(|n| store.note_pin(&n.id, true)).is_ok(),
            },
        };
        if written && take {
            let _ = notes.set(&group_key(group), "");
        }
    }
    old.len()
}

/// Before `aca note` reads: the groups' notes an interface wrote into the notes file are in the registry too
pub fn adopt_file(store: &Store) {
    let mut notes = Notes::open(Notes::default_path());
    if notes.attached() {
        adopt(store, &mut notes, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(b: &Book, g: &str) -> Vec<String> {
        b.of(g).map(|n| n.text.clone()).collect()
    }

    #[test]
    fn a_groups_old_note_becomes_a_note_in_its_notebook_once() {
        let mut notes = Notes::default();
        notes.set(&group_key("fun"), "整体的想法\n先做左栏").unwrap();
        notes.set(&group_key("fun/aca"), "子组的").unwrap();
        notes.set("id-1", "会话的便签").unwrap();
        let mut b = Book::default();
        b.adopt(&mut notes);
        assert_eq!(texts(&b, "fun"), ["整体的想法\n先做左栏"]);
        assert_eq!(texts(&b, "fun/aca"), ["子组的"]);
        let n = b.top("fun").unwrap();
        assert_eq!((n.name(), n.by.as_str()), ("整体的想法", ""));
        // Out of the file; a session's note stays
        assert!(notes.of_groups().is_empty());
        assert_eq!(notes.get("id-1"), Some("会话的便签"));
        // An older interface wrote the same text there again, and another one: only the other one is new
        notes.set(&group_key("fun"), "整体的想法\n先做左栏").unwrap();
        b.adopt(&mut notes);
        notes.set(&group_key("fun"), "又一条").unwrap();
        b.adopt(&mut notes);
        assert_eq!(b.of("fun").count(), 2);
        assert!(texts(&b, "fun").contains(&"又一条".to_string()));
    }

    /// An older interface still open saves a group's note into the file at every keystroke, and each refresh here takes what's
    /// there: it stays one note
    #[test]
    fn a_note_an_older_interface_is_still_typing_stays_one_note() {
        let mut notes = Notes::default();
        let mut b = Book::default();
        for typed in ["先", "先跑", "先跑测试", "先跑"] {
            notes.set(&group_key("fun"), typed).unwrap();
            b.adopt(&mut notes);
            assert_eq!(texts(&b, "fun"), [typed]);
        }
        // Not a note that only starts the same way but was left alone long ago, nor one a session wrote
        let old = Note { id: "old1".into(), group: "work".into(), text: "发布".into(), updated: 1, created: 1, ..Note::default() };
        b.store.note_put(&old).unwrap();
        b.store.note_add("work", "发布前", "s1").unwrap();
        notes.set(&group_key("work"), "发布前先跑测试").unwrap();
        b.adopt(&mut notes);
        assert_eq!(b.of("work").count(), 3);
    }

    /// The printed list and --snapshot hold a copy in memory: what's in a notes file on disk shows, and stays in the file
    #[test]
    fn a_copy_in_memory_shows_the_files_group_notes_without_taking_them_out() {
        let dir = std::env::temp_dir().join(format!("aca-book-test-{}-{}", std::process::id(), aca_core::paths::short_id()));
        let path = dir.join("notes");
        Notes::open(Some(path.clone())).set(&group_key("fun"), "还在文件里").unwrap();
        let mut notes = Notes::open(Some(path.clone()));
        let mut b = Book::default();
        b.adopt(&mut notes);
        b.adopt(&mut notes);
        assert_eq!(texts(&b, "fun"), ["还在文件里"]);
        assert_eq!(Notes::open(Some(path)).get(&group_key("fun")), Some("还在文件里"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_note_started_in_a_group_is_one_note_however_much_is_typed() {
        let mut b = Book::default();
        let key = group_key("work");
        assert_eq!((b.text(&key), b.group_of(&key)), (None, Some("work")));
        for t in ["先", "先跑", "先跑测试"] {
            b.save(&key, t).unwrap();
        }
        assert_eq!((texts(&b, "work"), b.text(&key)), (vec!["先跑测试".to_string()], Some("先跑测试")));
        // Done: from here on it goes by its own key, and the next one started in this group is another note
        let own = b.done(&key).unwrap();
        assert_eq!((b.text(&own), b.group_of(&own), b.text(&key)), (Some("先跑测试"), Some("work"), None));
        b.save(&key, "第二条").unwrap();
        assert_eq!(b.done(&key).map(|k| k != own), Some(true));
        assert_eq!(b.of("work").count(), 2);
        // Started and left blank: it was never there
        b.save(&key, "写了又删").unwrap();
        b.save(&key, "  ").unwrap();
        assert_eq!(b.done(&key), None);
        assert_eq!(b.of("work").count(), 2);
        // Nothing written at all
        assert_eq!(b.done(&key), None);
    }

    #[test]
    fn a_note_cleared_while_rewriting_is_deleted_when_done_and_writing_it_again_brings_it_back() {
        let mut b = Book::default();
        let id = b.add("work", "原来的").unwrap().id;
        let key = note_key(&id);
        b.save(&key, "改过的").unwrap();
        assert_eq!(b.done(&key), Some(key.clone()));
        assert_eq!(texts(&b, "work"), ["改过的"]);
        // Blank for a moment while retyping: still the same note
        b.save(&key, "").unwrap();
        b.save(&key, "重新写").unwrap();
        assert_eq!((b.done(&key), texts(&b, "work")), (Some(key.clone()), vec!["重新写".to_string()]));
        // Left blank: gone from the notebook
        b.save(&key, "").unwrap();
        assert_eq!(texts(&b, "work"), ["重新写"]);
        assert_eq!((b.done(&key), b.text(&key)), (None, None));
        assert_eq!(b.of("work").count(), 0);
        // What ^Z does: write the old text under its key again
        b.save(&key, "重新写").unwrap();
        assert_eq!((b.done(&key), texts(&b, "work")), (Some(key), vec!["重新写".to_string()]));
    }
}
