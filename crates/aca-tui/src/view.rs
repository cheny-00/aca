//! Types the interface shares with the modules around it: which sessions the list shows ([`Scope`]), where the preview
//! goes ([`Preview`]), and what the run loop does after giving up the terminal ([`External`], [`Split`], [`TermReq`]).

use aca_core::t;
use aca_records::workdir::Where;

/// Output within this many seconds after coming back from viewing doesn't count as "new activity" (detach may redraw once)
pub(crate) const SEEN_SLACK: u64 = 3;

/// "Last 7 days": stopped sessions idle for less than this are listed too
pub const WEEK: u64 = 7 * 86400;

/// Which sessions the list shows. The second lists everything the first does and more; `w` switches between them. Older
/// stopped ones are reached through `F` and search, so the list never grows to every session ever started. Picking one state
/// with `F` sets this aside until it's cleared (see `App::status`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// The default: everything whose pane is still there (● ◆ ◐ ○ ✕), plus pinned ones
    Open,
    /// The above, plus stopped ones idle for less than [`WEEK`]
    Week,
}

impl Scope {
    pub const ALL: [Scope; 2] = [Scope::Open, Scope::Week];

    pub fn next(self) -> Scope {
        match self {
            Scope::Open => Scope::Week,
            Scope::Week => Scope::Open,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scope::Open => t!("未停止", "Open"),
            Scope::Week => t!("近 7 天", "7 days"),
        }
    }

    /// What the view file calls it
    pub fn key(self) -> &'static str {
        match self {
            Scope::Open => "open",
            Scope::Week => "week",
        }
    }

    /// `all` was a third step once; a view file saved on it lands on the wider one left
    pub fn from_key(k: &str) -> Option<Scope> {
        if k == "all" {
            return Some(Scope::Week);
        }
        Scope::ALL.into_iter().find(|s| s.key() == k)
    }
}

/// Layout (where the preview goes). Wide screens (desktop) and narrow ones (phone) remember theirs separately; `L` cycles only through
/// the ones that fit the current kind of screen:
/// wide: preview right → preview below → list only; narrow: split → mostly preview → list only
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preview {
    /// Default: on the right on wide screens, split top/bottom on narrow ones
    Auto,
    /// Below, on wide screens
    Below,
    /// Narrow screens' "mostly preview": the list keeps only a few rows around the highlight, the rest goes to the preview
    Focus,
    /// List only (a line at the bottom says what the highlighted one is doing)
    Off,
}

/// External programs run after giving up the terminal (`l` / `e` in the diff view, `y` in the list)
#[derive(Debug, PartialEq)]
pub enum External {
    /// lazygit, opened in this repo
    Lazygit(String),
    /// $EDITOR opens the file at this line
    Edit { path: String, line: usize },
    /// e in a session record: view read-only in the editor at this line; temp = a temporary file deleted afterwards
    View { path: String, line: usize, temp: bool },
    /// The config file (config.toml) the settings page opens in the editor: written from the template first if missing.
    /// section: open at that `[section]` (added at the end when it isn't there); None = at the top
    Config { path: String, template: String, section: Option<&'static str> },
    /// yazi, opened in the directory the session really works in (looked up when giving up the terminal, as with T).
    /// In a tmux popup it's instead run in a zoomed pane split off in the session (see the run loop); empty `target` = the session isn't
    /// running, so it can only open in place
    Yazi(Split),
    /// A plugin's pane while the interface isn't in tmux: run in the foreground (see caps.rs)
    Plugin(crate::caps::Req),
}

/// T: open a new window running a shell in its session (like prefix c); y in a popup: split a pane next to its pane to run yazi.
/// The directory is the one it really works in (the run loop finds the directory, opens the window / split, then goes in landing on the
/// new pane)
#[derive(Debug, PartialEq)]
pub struct Split {
    /// Split a pane to run yazi (zoomed to fill, closed on exit); otherwise open a new window running a shell
    pub yazi: bool,
    /// Which tmux session to go into
    pub name: String,
    /// Which pane to split (yazi): pane id, or that session's current pane if none
    pub target: String,
    pub w: Where,
    /// Split side by side (desktop); top and bottom on a phone (yazi)
    pub side: bool,
    /// The account the new pane carries (`data::shell_env`)
    pub env: Vec<(String, String)>,
}

/// t: open a temporary terminal for this row (the run loop finds the directory and opens the shell)
#[derive(Debug, PartialEq)]
pub struct TermReq {
    /// Keyed by row: t again goes back to the same one
    pub key: String,
    /// Its tmux session (to take the account's environment)
    pub family: String,
    pub title: String,
    pub w: Where,
    /// Extra environment to bring (config directory when logging in a new account); if given, not taken from family
    pub env: Vec<(String, String)>,
    /// Run this command instead of a shell (logging in a new account); the terminal closes when it exits
    pub run: Option<String>,
    /// Login terminal: when the login link shows up, pop up a browser picker (see browser.rs)
    pub link: Option<crate::term::Catch>,
}
