//! The traits for popups and the settings page: a feature's interface is written in its own file, not in ui.rs.
//!
//! - `Overlay`: one popup: what it keeps, how keys go, which lines it draws. ui.rs only pushes it onto the stack, hands keys
//!   to the top layer, and draws the lines it gives into the popup frame (centered on desktop, rising from the bottom on a
//!   phone, like the other popups).
//! - `Setting`: a row on the `^S` settings page.
//! - `Fx`: what a popup / settings row wants the interface to do. They can't touch App; they write what to do here and hand it
//!   back, so they can be tested without the interface.
//! - `kit`: parts for drawing lines.
//!
//! Adding a feature with its own interface:
//! 1. implement `Overlay` in its own file (for a row on the settings page, also implement `Setting`); add the `mod` in main.rs
//! 2. add one entry point in ui.rs: a key on the list: one `self.open(…)` line in `on_key`; a settings-page row: one
//!    `with_setting(…)` line in `run`
//! 3. work that runs in the background: add a variant to `Job` in ops.rs, carrying the feature's own type (`Job::Mcp`,
//!    `Job::Group`, `Job::Plugins`)
//! 4. document the keys in help.rs / docs/keys.md

use ratatui::crossterm::event::KeyEvent;
use ratatui::text::{Line, Span};

use crate::ops::Job;
use crate::ui::{External, TermReq};

/// A popup
pub trait Overlay {
    /// How wide the popup is on desktop (full width on a phone)
    fn width(&self) -> u16;
    /// Its lines; w = how wide a line can be
    fn lines(&self, w: usize) -> Vec<Line<'static>>;
    /// A fixed-size popup (the plugins page): on desktop it takes the screen's full height (minus a margin), and the frame doesn't
    /// change as the highlight moves; lines come from `lines_in`
    fn fill(&self) -> bool {
        false
    }
    /// At most how many rows a `fill` popup needs inside (not counting the frame and the message under it); None = as many as
    /// the screen fits. A page with little content (the accounts page) sizes its height to the content instead of leaving a
    /// big blank; it must make sure this number doesn't change as the highlight moves
    fn fill_rows(&self) -> Option<u16> {
        None
    }
    /// A `fill` popup: exactly h lines (extra ones cut, missing ones padded with blank lines)
    fn lines_in(&self, w: usize, _h: usize) -> Vec<Line<'static>> {
        self.lines(w)
    }
    /// One key. What the interface should do goes into fx; returns whether this layer stays
    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow;
    /// A background job finished, or back from the editor: reread the values shown
    fn reload(&mut self) {}
    /// Back from a program the terminal was handed to (editor…): what to do next goes into fx (plugins page: sync if plugins.toml
    /// changed). Only the top layer is asked
    fn returned(&mut self, _fx: &mut Fx) {}
    /// Typing (in the popup's own input box): several keys arriving at once are then a paste, not dropped as auto-typed input
    fn typing(&self) -> bool {
        false
    }
    /// A plugin's list view: whether this request (rereading itself, rereading after one of its keys finished) belongs to it
    fn takes(&self, _req: &crate::caps::Req) -> bool {
        false
    }
    /// Its own command ran again: swap in the new output
    fn feed(&mut self, _ok: bool, _out: &str) {}
    /// One of its keys finished: what it output (shown under the list)
    fn said(&mut self, _ok: bool, _out: &str) {}
}

/// What this popup layer does after a key
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Stay,
    Close,
}

/// What the interface should do; every field is optional
#[derive(Default)]
pub struct Fx {
    /// A message: shown in the bottom bar; if the popup stays open, also at the popup's bottom (on a phone the popup covers the bottom bar)
    pub flash: Option<String>,
    /// Run in the background (not while another job is unfinished; the bottom bar says "still busy"); when done the bottom bar
    /// says the result and the open popup gets `reload`
    pub job: Option<Job>,
    /// Hand the terminal to another program (editor…), then come back and keep showing
    pub external: Option<External>,
    /// Open a terminal in the bottom half of the screen
    pub term: Option<TermReq>,
    /// Open another layer; closing it returns to this one
    pub open: Option<Box<dyn Overlay>>,
    /// Copy this text (on a phone, via OSC 52 into the phone's clipboard); the bottom bar says what was copied
    pub copy: Option<String>,
    /// Run a plugin's command / view / pane (see caps.rs): runs in the background, doesn't take up `job`
    pub plugin: Option<crate::caps::Req>,
    /// After this layer closes, act as if this key was pressed on the main list (an aca action picked in the `:` panel)
    pub key: Option<KeyEvent>,
}

impl Fx {
    pub fn say(&mut self, m: impl Into<String>) {
        self.flash = Some(m.into());
    }
}

/// A feature's own row on the `^S` settings page: "key  name  current value  ›"
pub trait Setting {
    /// The key at the start of the row (mustn't clash with the settings page's own: t i a n g e; features' own: d Discord bridge,
    /// h remote tasks, m Claude mod, f auto-trust folders)
    fn key(&self) -> char;
    fn name(&self) -> &'static str;
    /// The current value: asked once when the settings page opens and once per reread, not every frame
    fn value(&self) -> Vec<Span<'static>>;
    /// The mark at the end of the row: › = go in and pick, ↗ = open in the editor, empty = toggles in place
    fn mark(&self) -> &'static str {
        "›"
    }
    /// ⏎ / its key was pressed
    fn enter(&self, fx: &mut Fx);
}

/// Parts for drawing lines (looking the same as the other popups)
pub mod kit {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    pub use crate::ui::{button, clip, dim, esc_hint, key_style, keys_line, modal_title, spread};

    /// The highlighted row: pad it to the full width with the selected background
    pub fn picked(line: Line<'static>, w: usize, on: bool) -> Line<'static> {
        shaded(line, w, on.then(|| crate::theme::pal().sel))
    }

    /// Pad to the full width with this background (None = no background)
    pub fn shaded(mut line: Line<'static>, w: usize, bg: Option<ratatui::style::Color>) -> Line<'static> {
        if let Some(bg) = bg {
            let pad = w.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(pad)));
            line = line.patch_style(Style::new().bg(bg));
        }
        line
    }
}
