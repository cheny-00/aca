//! Task tickets as the interface holds them: what the last refresh read from the registry (aca-core's store.rs and
//! task.rs; `aca task` reads and writes the same ones). Nothing is changed from here: what the interface does to a ticket
//! goes through a background job (ops.rs), and shows at the next refresh.

use std::rc::Rc;

use aca_core::store::{Store, Task, TaskState};

pub struct Tickets {
    /// The registry the notebooks are in too (see `Book::store`): the one on disk, or a copy in memory
    store: Rc<Store>,
    /// Every ticket, by number
    rows: Vec<Task>,
}

impl Default for Tickets {
    fn default() -> Self {
        Tickets { store: Rc::new(Store::memory()), rows: vec![] }
    }
}

impl Tickets {
    pub fn on(store: Rc<Store>) -> Tickets {
        let mut t = Tickets { store, rows: vec![] };
        t.reload();
        t
    }

    pub fn reload(&mut self) {
        // Not readable right now: keep showing what was read last
        if let Ok(v) = self.store.tasks() {
            self.rows = v;
        }
    }

    pub fn get(&self, n: i64) -> Option<&Task> {
        self.rows.iter().find(|t| t.n == n)
    }

    /// One group's tickets (not its subgroups') as its page lists them: the done ones first, in the order they were
    /// finished, then the rest by number
    pub fn of(&self, group: &str) -> Vec<&Task> {
        let mut v: Vec<&Task> = self.rows.iter().filter(|t| t.group == group).collect();
        v.sort_by_key(|t| match t.state() {
            TaskState::Done => (0, t.done, t.n),
            _ => (1, 0, t.n),
        });
        v
    }

    /// How many of a group's tickets are done, and how many it has
    pub fn tally(&self, group: &str) -> (usize, usize) {
        let all = self.rows.iter().filter(|t| t.group == group);
        all.fold((0, 0), |(done, n), t| (done + usize::from(t.state() == TaskState::Done), n + 1))
    }

    /// How many of a group's tickets aren't done
    pub fn open(&self, group: &str) -> usize {
        let (done, all) = self.tally(group);
        all - done
    }

    /// The ticket a session is doing (the lowest numbered, with several)
    pub fn doing(&self, session: &str) -> Option<&Task> {
        self.rows.iter().find(|t| !session.is_empty() && t.session == session && t.state() == TaskState::Doing)
    }

    /// The ticket a session finished last
    pub fn finished(&self, session: &str) -> Option<&Task> {
        let mine = self.rows.iter().filter(|t| !session.is_empty() && t.session == session && t.state() == TaskState::Done);
        mine.max_by_key(|t| (t.done, t.n))
    }

    /// Write a ticket into the registry held, as given (a test's fixture, on a registry in memory)
    #[cfg(test)]
    pub fn put(&mut self, t: Task) {
        self.store.task_put(&t).unwrap();
        self.reload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: i64, group: &str, session: &str, done: u64) -> Task {
        Task { n, group: group.into(), title: format!("单{n}"), session: session.into(), done, created: 1, ..Task::default() }
    }

    #[test]
    fn a_groups_tickets_list_the_done_ones_first_then_the_rest_by_number() {
        let mut k = Tickets::default();
        for x in [t(1, "work", "a", 0), t(2, "work", "b", 50), t(3, "work", "", 0), t(4, "work", "a", 20), t(5, "other", "a", 0)] {
            k.put(x);
        }
        // Done in the order they were finished, then the others by number; another group's aren't among them
        assert_eq!(k.of("work").iter().map(|t| t.n).collect::<Vec<_>>(), [4, 2, 1, 3]);
        assert_eq!((k.tally("work"), k.open("work"), k.tally("nope")), ((2, 4), 2, (0, 0)));
        // What a session is doing (the lowest numbered of them) and what it finished last
        assert_eq!((k.doing("a").map(|t| t.n), k.finished("a").map(|t| t.n)), (Some(1), Some(4)));
        assert_eq!((k.doing("b").map(|t| t.n), k.finished("b").map(|t| t.n)), (None, Some(2)));
        assert!(k.doing("").is_none() && k.finished("").is_none() && k.get(9).is_none());
        assert_eq!(k.get(3).map(|t| t.state()), Some(TaskState::Todo));
    }
}
