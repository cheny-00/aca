//! Follow (`aca ui --client … --follow`): when the interface is open in a tmux popup taking only one side of the screen,
//! wherever the highlight moves, this client switches to that session. The popup is attached to the client, so it stays when
//! the session switches, and what shows beside it is the highlighted one.
//!
//! - ⏎ stays in that session (as without follow: switch there, the popup closes, counted as seen)
//! - Esc goes back to the pane it was on when opened
//! - Ctrl+\ (the key that opens the popup) / qq closes it; you stay in the one now showing beside it, which counts as seen
//!
//! Switches only once the highlight settles: not while swiping or typing a filter; on a group or a session that isn't running,
//! the side stays as it is.
//! Passing by doesn't count as seen: switching there makes tmux record it as "attached", which clears core's "finished, not
//! looked at", so the ones passed by are marked unread (see reads.rs).
//! The preview isn't drawn by default (the real thing is right beside it); `v` changes it for this time only, leaving the
//! layouts the phone / desktop each remember alone.
//!
//! This only decides "whether to switch, and where"; `data::show` sends the tmux commands, and the places it's wired into the
//! interface are in ui.rs (`follow_step` / `follow_done`).

use std::sync::atomic::{AtomicBool, Ordering};

use crate::ui::Preview;

/// Started with `--follow` (passed on as is when ^L restarts)
static ASKED: AtomicBool = AtomicBool::new(false);

pub fn ask() {
    ASKED.store(true, Ordering::Relaxed);
}

pub fn asked() -> bool {
    ASKED.load(Ordering::Relaxed)
}

/// Where the client is / is going
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// The key of that row in the list (`Session::name`); empty if the highlight wasn't on any row when opened
    pub row: String,
    /// tmux's session id (`$3`)
    pub session: String,
    /// Which pane it lands in (`%12`); empty = that session's current pane
    pub pane: String,
}

impl Place {
    fn same_place(&self, o: &Place) -> bool {
        (&self.session, &self.pane) == (&o.session, &o.pane)
    }
}

pub struct Follow {
    /// When opened: the highlighted row, plus the session and pane the client was really on (that row's own pane may be a
    /// different one; coming back lands on this original pane)
    home: Place,
    /// Where the client has been taken now
    at: Place,
    /// Esc was pressed: the next step goes back home
    back: bool,
    /// The layout used this time
    pub layout: Preview,
}

impl Follow {
    pub fn new(home: Place) -> Self {
        Follow { at: home.clone(), home, back: false, layout: Preview::Off }
    }

    /// The highlight settled on want (None = not settled yet, or on a row that can't be switched to). Returns where to switch to if
    /// needed; nothing if already there
    pub fn step(&mut self, want: Option<Place>) -> Option<Place> {
        let to = match want {
            _ if self.back => self.home.clone(),
            // Back to the row it opened on: land on the original pane, not that row's own pane
            Some(w) if w.row == self.home.row => self.home.clone(),
            Some(w) => w,
            None => return None,
        };
        if to.same_place(&self.at) {
            return None;
        }
        self.at = to.clone();
        Some(to)
    }

    /// Esc: go back to where it was when opened
    pub fn back(&mut self) {
        self.back = true;
    }

    /// Whether this is the session it was on when opened
    pub fn is_home(&self, session: &str) -> bool {
        self.home.session == session
    }

    /// You stayed in another session on close: its session id
    pub fn stayed(&self) -> Option<&str> {
        (!self.is_home(&self.at.session)).then_some(self.at.session.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(row: &str, session: &str, pane: &str) -> Place {
        Place { row: row.into(), session: session.into(), pane: pane.into() }
    }

    #[test]
    fn opening_moves_nothing_and_each_row_is_switched_to_once() {
        // Opened on a shell pane (%7) of session a, with the highlight on a's row (its own pane is the agent's %1)
        let mut f = Follow::new(spot("a", "$1", "%7"));
        assert_eq!(f.step(Some(spot("a", "$1", "%1"))), None, "刚打开:不动");
        assert_eq!(f.step(None), None, "滑动途中 / 停在分组上:不动");
        assert_eq!(f.step(Some(spot("b", "$2", "%3"))), Some(spot("b", "$2", "%3")));
        assert_eq!(f.step(Some(spot("b", "$2", "%3"))), None, "已经在那:不再发命令");
        // Another pane in the same session switches too
        assert_eq!(f.step(Some(spot("b\u{1}%4", "$2", "%4"))), Some(spot("b\u{1}%4", "$2", "%4")));
        // Back to the row it opened on: lands on the original pane (%7), not the agent's %1
        assert_eq!(f.step(Some(spot("a", "$1", "%1"))), Some(spot("a", "$1", "%7")));
        assert_eq!(f.stayed(), None);
    }

    #[test]
    fn esc_goes_back_to_where_it_opened_whatever_is_highlighted() {
        let mut f = Follow::new(spot("a", "$1", "%7"));
        f.step(Some(spot("b", "$2", "%3")));
        assert_eq!(f.stayed(), Some("$2"));
        f.back();
        // The highlight still on b, even still swiping: goes back anyway
        assert_eq!(f.step(Some(spot("b", "$2", "%3"))), Some(spot("a", "$1", "%7")));
        assert_eq!((f.step(None), f.stayed()), (None, None));
        // Esc without having moved: already there, no command sent
        let mut f = Follow::new(spot("a", "$1", "%7"));
        f.back();
        assert_eq!(f.step(None), None);
    }

    #[test]
    fn the_preview_is_off_until_asked_for() {
        assert_eq!(Follow::new(Place::default()).layout, Preview::Off);
    }
}
