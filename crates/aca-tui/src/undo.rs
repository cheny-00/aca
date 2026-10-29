//! `^Z` undo (anything that can be taken back is): one step at a time backwards, at most `KEEP` steps, only while this aca is
//! open, no redo. Deleting a session or group can be undone because what `d` deletes goes to the trash and is only really
//! deleted after 1 day (see trash.rs); steps pushed out of these 10 are still in the trash.
//! Restart, sending a note, fork can't be taken back once done, so they aren't recorded; a closed pane / plain tmux session
//! whose processes are already killed is still recorded as a step (`Gone`), and `^Z` there only says it can't be undone,
//! so it doesn't skip past it to undo an earlier step.

use crate::data::State;
use crate::ops::Target;
use crate::reads::Mark;
use crate::spaces::Group;

/// At most this many steps are kept
pub const KEEP: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub enum Undo {
    /// A registered session / group deleted with d: its entry (id) in the trash, label is its name; start: sessions that were
    /// running before the delete, started again after the undo
    Bin { id: String, label: String, start: Vec<String> },
    /// A pane closed / plain tmux session deleted with d: can't be undone
    Gone { title: String },
    /// Stopped with D: undo = start again
    Stop { t: Target },
    /// The pane stopped with D (an extra window inside a session / a plain tmux session): undo = start again in place (see park.rs)
    Park { t: Target, p: crate::park::Pane },
    /// m: move back to from
    Move { t: Target, from: String, groups: Vec<String> },
    /// r: rename back to from; unlock: the title wasn't locked before (renaming locks it)
    Rename { t: Target, from: String, unlock: bool },
    /// M: attach / detach the other way round
    Mcp { t: Target, dir: String, on: Vec<String>, off: Vec<String> },
    /// p: the position it was pinned at (None = it wasn't pinned). On undo the current row is found by key (`notes::key`)
    Pin { key: String, name: String, title: String, rank: Option<usize> },
    /// u: what the seen file recorded before, and the state before; what: what it was marked as then (seen / unread); seen: when
    /// a registered session was marked seen, the "seen" time the core's copy recorded before
    Read { key: String, name: String, title: String, what: String, mark: Option<Mark>, state: State, seen: Option<u64> },
    /// Q: what the workspace file recorded before for these sessions / groups
    Space { what: String, sessions: Vec<(String, Option<char>)>, groups: Vec<(Group, Option<char>)> },
    /// e / ^G: the note before (empty = there was none)
    Note { key: String, title: String, text: String },
    /// r on a group: change it back (name, default folder), i.e. change it again the other way
    Group(crate::groups::Act),
    /// Done together after selecting with v (stop, move group, pin, mark seen / unread): all undone in one step. Deleted
    /// together is one entry in the trash, recorded as `Bin`
    Many(Vec<Undo>),
}

impl Undo {
    /// The kinds that are undone in the background: which session the undo is for
    pub fn target(&self) -> Option<&Target> {
        match self {
            Undo::Stop { t, .. } | Undo::Move { t, .. } | Undo::Rename { t, .. } | Undo::Mcp { t, .. } => Some(t),
            _ => None,
        }
    }
}

/// The most recent is last; past KEEP steps the oldest is pushed out
pub fn push(stack: &mut Vec<Undo>, u: Undo) {
    stack.push(u);
    if stack.len() > KEEP {
        stack.remove(0);
    }
}
