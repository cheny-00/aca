//! Collection: sessions aca registered (registry + state the hook writes) + the panes actually in tmux + each pane's screen,
//! merged into the list's rows.
//!
//! A registered session's state is decided by `aca_core::status::merge` (the screen wins; the hook adds what the screen can't
//! show: finished but not looked at, errored);
//! plain tmux sessions and extra windows opened inside a session go by the screen only:
//!   a select box / confirm box prompt at the bottom → Ask; "· verb… (9m 1s · …)" above the input box → Running.
//!
//! A session isn't identified by its tmux session name (that's the title, and it changes): `name` is the stable key
//! (the registered id / a plain session's name); tmux commands use `tmux` (session id `$3`) and `pane` (pane id `%12`).

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, OnceLock};
use std::thread;
use std::time::Duration;

use aca_core::config::Config;
use aca_core::proc::{self, Kids};
use aca_core::runtime::Run;
use aca_core::store::{self, Store};
use aca_core::tmux::Pane;
use aca_core::i18n::{self, Lang};
use aca_core::{paths, status};
use aca_core::{t, tr};
use regex::Regex;

use crate::subagent::Subagent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ask,
    Running,
    Unread,
    Idle,
    Error,
    /// No longer produced (formerly: the Claude inside quit, leaving only a shell); kept because the UI still draws it
    Exited,
    /// Registered but not in tmux (computer rebooted, tmux server gone, brought back from the archive). ⏎ restarts and carries on
    Stopped,
    /// The program stopped, the pane stays with its last screen: stopped with `D`, or the agent exited on its own.
    /// ⏎ restarts it in place (a registered session carries on the original conversation)
    Dead,
    /// Same as Stopped, but the last turn ended in an error: drawn as ✕
    Crashed,
}

impl State {
    /// Not in tmux, still registered: ⏎ = `session::start` (recreates the tmux session)
    pub fn gone(self) -> bool {
        matches!(self, State::Stopped | State::Crashed)
    }
}

/// Which vendor's agent runs in the session. The account name in the list is colored by it
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Agent {
    /// A plain tmux session, or one opened as a shell in the first place
    #[default]
    None,
    Claude,
    Codex,
    Pi,
    Opencode,
    /// A declared tool (a `[tools.<name>]` in config.toml without a kind)
    Other,
}

#[derive(Clone, Debug)]
pub struct Session {
    /// The stable key: a registered session's id, a plain tmux session's name, a sub-pane's "session key \u{1} pane id".
    /// Pins, notes and read marks are all keyed by it
    pub name: String,
    /// tmux session id (`$3`), for attach and splits; empty for registered sessions not in tmux
    pub tmux: String,
    pub title: String,
    /// The title is locked (set by hand, Claude can't change it; renaming with `r` locks it)
    pub locked: bool,
    pub group: String,
    pub state: State,
    /// In the Ask state, what it's asking (empty if not found)
    pub question: String,
    pub idle_for: u64,
    /// When there was last activity (epoch seconds): for a registered agent that isn't running it's what the hook reported
    /// (see `Run::hook_at`), otherwise when the window last printed something
    pub active_at: u64,
    /// In the Running state, how many seconds it has been running (read off the screen, 0 = unknown)
    pub ran: u64,
    /// A terminal elsewhere is attached (e.g. the desktop); control-mode clients don't count
    pub attached: bool,
    /// Screen content (trailing blank lines removed, with color / link escape sequences), for the preview
    pub screen: Vec<String>,
    /// Pinned. Always false when collected; the UI marks it from the pins file
    pub pinned: bool,
    /// Has a note (written with `e`, filled into the input box on Enter). Always false when collected; the UI marks it from the notes file
    pub noted: bool,
    /// Its group has a note (✎ on the group title and folded row). Always false when collected; the UI marks it from the notes file
    pub group_noted: bool,
    /// Which workspace it's in (see spaces.rs). Always 1 when collected; the UI marks it from the workspaces file
    pub space: char,
    /// Project directory (the registered one, else the pane's current directory); new sessions open here
    pub dir: String,
    /// `REGISTERED` for a registered session, empty = plain tmux session
    pub profile: String,
    /// Registered tool name (claude / codex / a custom name) and account, reused when creating a new one.
    /// Empty = use the default; a shell session's tool is also recorded as empty (a new one from it starts the default tool,
    /// not another shell)
    pub tool: String,
    pub account: String,
    /// Which vendor actually runs and with which account (see `identify`), for display; empty = unknown
    pub agent: Agent,
    pub who: String,
    /// The running claude / codex process, 0 = not running / unknown (used to find the directory it actually works in, see `workdir`)
    pub pid: u32,
    /// Registered session id (every operation on it looks it up by this), empty = plain tmux session or extra window inside a session
    pub deck_id: String,
    /// What `B` copies: Path / Session
    pub info: String,
    /// The session's directory is a git worktree made just for it: `d` deletes the directory too, and the confirmation must say so
    /// (no worktrees are made now, always false)
    pub worktree: bool,
    /// Fixed position in the list: (group's rank, sort_order within the group, creation time), all from the registry.
    /// Plain tmux sessions' group ranks usize::MAX, sinking to the bottom
    pub slot: (usize, i64, i64),
    /// tmux pane id (%12): preview, full-screen reading, pasting notes, which pane you land in on entering. Stopped sessions have none
    pub pane: String,
    /// Extra window / split pane inside a session (listed under the session); None = the session itself
    pub sub: Option<Sub>,
    /// Whether it's a child session spawned by another session (its window follows its own session)
    pub up: Up,
    /// Whether it was forked from another session (marked only on the session's row)
    pub fork: Fork,
    /// Subagents it spawned itself (Claude's Agent tool, Codex's spawn_agent): running ones, plus those started in the last turn
    /// (see subagent.rs). Filled in on the session's row when collected; in the UI each gets its own row only when unfolded with `l`
    pub subagents: Vec<Subagent>,
    /// This row is an unfolded subagent (the UI builds it from the above; `sub` records the session it hangs under, pane is its index):
    /// no pane, ⏎ shows its transcript
    pub subagent: Option<Subagent>,
    /// Text plugins attach to the row (`aca meta set`, unexpired): name → value
    pub tokens: Vec<(String, String)>,
    /// Stopped on an error (registered sessions): the hook's message and category (transient / account / fatal,
    /// see `lifecycle::error_kind`); empty when no error
    pub error: String,
    pub error_kind: String,
    /// When aca stopped it for idling (`[idle] stop_after`, see `session::stop_idle`): drawn as ☾ and labelled auto-stopped,
    /// distinct from D's ■. 0 otherwise
    pub idle_stopped: u64,
    /// Things it scheduled for itself that aren't due yet (Claude's CronCreate, the next `/loop` run, see `aca_core::schedule`), and
    /// scheduled messages in its inbox (`aca msg --in`): ◷ and the next time after the title. One that has any of these isn't
    /// auto-stopped while idle. Claude's two live only in its process, so they exist only while the agent is still open; only
    /// registered sessions have them, and the UI's first screen doesn't read them
    pub due: Vec<aca_core::schedule::Due>,
    /// This item isn't a session but an empty group in the registry (not even subgroups have sessions, see `blank_row`):
    /// the list draws only its title, and the highlight rests on the title
    pub blank: bool,
}

/// Parent/child sessions (`aca new` run inside some session, or set with `--parent`).
/// Nested at any depth (one spawned by a child hangs under the child): hangs under the parent only if the parent is listed
/// and in the same group, otherwise flush left
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Up {
    /// Not a child session
    #[default]
    Top,
    /// Hangs under the parent: the parent's key and title
    Under(String, String),
    /// Parent still exists, but in another group / not listed: flush left
    Apart(String, String),
    /// Parent deleted (deleting a parent leaves its children alone)
    Gone,
}

impl Up {
    /// The parent's key (hanging under it or elsewhere both count)
    pub fn parent(&self) -> Option<&str> {
        match self {
            Up::Under(p, _) | Up::Apart(p, _) => Some(p),
            Up::Top | Up::Gone => None,
        }
    }
}

/// A forked session (`f` / `aca fork`, `fork_of` in the registry). It wasn't spawned by the original: it's a peer of the
/// original, not hung under it; only the row and preview say what it was forked from
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Fork {
    /// Not forked
    #[default]
    No,
    /// Forked from this session: its title
    Of(String),
    /// The original session was deleted
    Gone,
}

/// Extra window inside a session (prefix c) / split pane (prefix % / ")
#[derive(Clone, Debug, PartialEq)]
pub struct Sub {
    /// The containing session's key and title
    pub parent: String,
    pub parent_title: String,
    /// tmux window index and pane index within the window; listed as #2 / #2.1 (the .N only when split)
    pub win: u32,
    pub pane: u32,
    pub split: bool,
}

impl Sub {
    pub fn loc(&self) -> String {
        if self.split { format!("#{}.{}", self.win, self.pane) } else { format!("#{}", self.win) }
    }
}

impl Session {
    /// Key of the session it belongs to (a sub-pane's is the session it hangs under, a session's is itself):
    /// attach, pins and ordering go by it
    pub fn family(&self) -> &str {
        self.sub.as_ref().map_or(&self.name, |s| &s.parent)
    }

    /// Which session it hangs under (the parent's key): child sessions and their windows both count
    pub fn under(&self) -> Option<&str> {
        match &self.up {
            Up::Under(p, _) => Some(p),
            _ => None,
        }
    }

    /// The parents it hangs under at each level, nearest first (parent, grandparent…). `hangs` is the map from [`hangs`]
    pub fn ancestors<'a>(&'a self, hangs: &'a HashMap<String, String>) -> impl Iterator<Item = &'a str> {
        std::iter::successors(self.under(), move |p| hangs.get(*p).map(String::as_str)).take(DEEP)
    }

    /// "Claude·team" / "Codex·alt"; when the account is just the tool name (the fallback shown), only the vendor
    pub fn agent_label(&self) -> String {
        let name = match self.agent {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Pi => "Pi",
            Agent::Opencode => "OpenCode",
            Agent::None | Agent::Other => "",
        };
        match (name, self.who.as_str()) {
            (n, "") => n.to_string(),
            ("", w) => w.to_string(),
            (n, w) if n.eq_ignore_ascii_case(w) => n.to_string(),
            (n, w) => format!("{n}·{w}"),
        }
    }

    /// Last column of the list: for running ones, how long it has run (latest activity is always 「刚刚」 ("just now"), which
    /// says nothing), otherwise how long it has been still
    pub fn when(&self) -> String {
        if self.state == State::Running && self.ran > 0 { age(self.ran) } else { age(self.idle_for) }
    }
}

static ASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Enter to select|esc to cancel|Do you want to|Would you like to").unwrap());
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\S\s+\S+…\s+\((?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?").unwrap());
const SPINNER: &str = "⠁⠂⠄⡀⢀⠠⠐⠈⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

static HOST: LazyLock<String> = LazyLock::new(|| sh(&["hostname", "-s"]).trim().to_string());

/// Every tmux call goes through here. With ACA_TMUX_SOCKET set it connects to `tmux -L <it>` instead — tests point it at a
/// separate server holding only fake sessions, so they can't possibly read or enter real ones.
pub fn tmux_cmd() -> Command {
    let mut c = Command::new("tmux");
    if let Some(sock) = std::env::var_os("ACA_TMUX_SOCKET").filter(|s| !s.is_empty()) {
        c.arg("-L").arg(sock);
    }
    c
}

fn tmux(args: &[&str]) -> String {
    tmux_cmd().args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn sh(args: &[&str]) -> String {
    Command::new(args[0]).args(&args[1..]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Attach to the session; returns on detach (success = tmux exited normally).
/// Every attach is a new tmux client, and tmux doesn't know where this terminal just was, so ^` / prefix L can't go back:
/// so first connect to the one just left and switch straight to the target (the first frame is drawn only after both commands
/// finish, so the first one never shows), and tmux records the "previous session".
/// stdout isn't printed to the terminal: on detach the tmux client prints "[detached (from session …)]" to stdout, landing on
/// the terminal's main screen, and after a few round trips a string of them shows once the UI exits; here it's read to learn
/// which session it ended in (it may have switched inside).
/// The tmux server writes the picture to the terminal fd on stdin, unaffected; errors go to stderr, kept as is.
/// The UI does this in three steps (see [`prepare`], [`go_in`], [`remember_left`]), giving up and taking back the terminal in between
pub fn attach(name: &str) -> bool {
    let e = prepare(name);
    let Some(left) = go_in(&e) else { return false };
    remember_left(&e, &left);
    true
}

/// What's prepared before entering a session: the target, the one just left (if still there), the sessions this terminal has been in
pub struct Entry {
    name: String,
    back: Option<String>,
    been: Vec<String>,
}

/// Nothing that has to happen before entering a session needs the terminal: add the Ctrl+Q and hotkey bindings, find the one
/// just left. The UI calls it before giving up the terminal — the list is still on screen then, tmux draws right after,
/// and the shell underneath never shows. The three are each a tmux command, done at the same time
pub fn prepare(name: &str) -> Entry {
    let (been, live) = thread::scope(|s| {
        s.spawn(aca_core::tmux::bind_detach);
        let live = s.spawn(live_sessions);
        (been(), live.join().unwrap_or_default())
    });
    let back = been.iter().find(|s| *s != name && live.contains(*s)).filter(|_| live.contains(name)).cloned();
    Entry { name: name.to_string(), back, been }
}

/// Attach; on detach returns which session it ended in (session id; it may have switched inside, e.g. a clicked notification;
/// empty if tmux didn't say or it's gone).
/// None = didn't get in.
/// If there's one just left, pass through it first (so ^` can go back once inside); passing through doesn't count as seen
/// (see `tmux::attach_via`)
pub fn go_in(e: &Entry) -> Option<String> {
    let run = |back: Option<&String>| {
        let mut c = tmux_cmd();
        match back {
            Some(b) => c.args(aca_core::tmux::attach_via(b, &e.name)),
            None => c.args(["attach-session", "-t", &e.name]),
        };
        c.stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::inherit()).output().ok()
    };
    let mut out = run(e.back.as_ref());
    // The one just left vanished at this very moment: didn't connect (nothing printed), try again without it
    if e.back.is_some() && out.as_ref().is_none_or(|o| !o.status.success() && o.stdout.is_empty()) {
        out = run(None);
    }
    let o = out.filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&o.stdout);
    // tmux gives the session name (= title, changes); turn it into the session id
    let left = text.lines().find_map(|l| l.strip_prefix("[detached (from session ")?.strip_suffix(")]")).unwrap_or_default();
    Some((!left.is_empty()).then(|| session_id(left)).flatten().unwrap_or_default())
}

/// Once back, record the sessions this terminal has been in, in order (one tmux command): called on the first frame the UI draws
/// after coming back. left is what [`go_in`] gave
pub fn remember_left(e: &Entry, left: &str) {
    remember(if left.is_empty() { &e.name } else { left }, &e.name, &e.been);
}

/// Session name → session id (`$3`)
fn session_id(name: &str) -> Option<String> {
    tmux(&["list-sessions", "-F", "#{session_id}\t#{session_name}"])
        .lines()
        .find_map(|l| l.split_once('\t').filter(|x| x.1 == name).map(|x| x.0.to_string()))
}

/// Which tmux server option records the sessions (session ids) this terminal has been in (@aca_been_ttys000):
/// survives ^L restarts and quitting and reopening the UI; phone and desktop are different terminals, each with its own.
/// Not recorded if there's no tty
fn been_opt() -> Option<&'static str> {
    static OPT: OnceLock<Option<String>> = OnceLock::new();
    OPT.get_or_init(|| {
        let o = Command::new("tty").stdin(Stdio::inherit()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
        let tty = String::from_utf8_lossy(&o.stdout).trim().trim_start_matches("/dev/").replace(|c: char| !c.is_ascii_alphanumeric(), "_");
        (!tty.is_empty()).then(|| format!("@aca_been_{tty}"))
    })
    .as_deref()
}

/// Sessions this terminal has been in, newest first
fn been() -> Vec<String> {
    let Some(opt) = been_opt() else { return vec![] };
    tmux(&["show-options", "-gqv", opt]).trim_end_matches('\n').split('\t').filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Entered name, finally came out of left: both move to the front (left first), keep the latest 8
fn remember(left: &str, name: &str, been: &[String]) {
    let Some(opt) = been_opt() else { return };
    let v = been_after(left, name, been);
    let _ = tmux_cmd().args(["set-option", "-gq", opt, &v.join("\t")]).status();
}

fn been_after<'a>(left: &'a str, name: &'a str, been: &'a [String]) -> Vec<&'a str> {
    let mut v: Vec<&str> = vec![];
    for s in [left, name].into_iter().chain(been.iter().map(String::as_str)) {
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v.truncate(8);
    v
}

/// Sessions now in tmux: both session ids and session names are in it (one command, no has-session one by one)
fn live_sessions() -> HashSet<String> {
    tmux(&["list-sessions", "-F", "#{session_id}\t#{session_name}"]).lines().flat_map(|l| l.split('\t')).map(String::from).collect()
}

/// Paste text into the session's input box: bracketed paste (if the program enabled that mode; Claude / Codex both do), no Enter,
/// so multiple lines aren't taken as sending.
/// target: pane id (%12), or `session id:` (its current pane)
pub fn paste_into(target: &str, text: &str) -> Result<(), &'static str> {
    let ok = |c: &mut Command| c.status().is_ok_and(|s| s.success());
    leave_mode(target)?;
    let pasted = ok(tmux_cmd().args(["set-buffer", "-b", "aca-note", "--", text]))
        && ok(tmux_cmd().args(["paste-buffer", "-p", "-d", "-b", "aca-note", "-t", target]));
    if pasted { Ok(()) } else { Err(t!("会话可能刚结束", "the session may have just ended")) }
}

/// When the pane sits in a mode like copy-mode (scrolled up with the wheel and never left), a paste goes in without brackets,
/// typed straight into the program char by char (newlines become Enter), and the following Enter is eaten by copy-mode —
/// so leave it first (`copy-mode -q` leaves any mode). If it still won't leave, don't paste
fn leave_mode(target: &str) -> Result<(), &'static str> {
    let in_mode = || tmux(&["display-message", "-p", "-t", target, "#{pane_in_mode}"]).trim() == "1";
    if in_mode() {
        let _ = tmux_cmd().args(["copy-mode", "-q", "-t", target]).status();
        if in_mode() {
            return Err(t!("窗格处于 tmux 模式中", "the pane is in a tmux mode"));
        }
    }
    Ok(())
}

/// s: paste into the input box, then Enter to send it. First pin down which pane (`session id:` turned into a pane id, so
/// switching panes midway can't send to the wrong one); the Enter has to wait a moment: an Enter right after a bracketed paste
/// is swallowed by Claude (Ink) as part of the paste
pub fn send_into(target: &str, text: &str) -> Result<(), &'static str> {
    let pane = tmux(&["display-message", "-p", "-t", target, "#{pane_id}"]).trim().to_string();
    if !pane.starts_with('%') {
        return Err(t!("会话可能刚结束", "the session may have just ended"));
    }
    paste_into(&pane, text)?;
    thread::sleep(Duration::from_millis(150));
    let sent = tmux_cmd().args(["send-keys", "-t", &pane, "Enter"]).status().is_ok_and(|s| s.success());
    if sent { Ok(()) } else { Err(t!("会话可能刚结束", "the session may have just ended")) }
}

/// Before entering the session, switch to that pane (that window, that pane), so you land on it
pub fn focus_pane(pane: &str) {
    let _ = tmux_cmd().args(["select-window", "-t", pane, ";", "select-pane", "-t", pane]).stderr(Stdio::null()).status();
}

/// `--client`: when the UI runs in a tmux popup, which client pressed the key. A bare switch-client in a popup switches the client
/// with the latest keypress, so with phone and desktop attached to the same session it would switch the other one away
static CLIENT: OnceLock<String> = OnceLock::new();

/// The UI runs in a tmux popup: TMUX set, TMUX_PANE not
pub fn in_popup() -> bool {
    std::env::var_os("TMUX").is_some() && std::env::var_os("TMUX_PANE").is_none()
}

pub fn set_client(c: String) {
    let _ = CLIENT.set(c);
}

/// The --client given at startup (passed on unchanged on a ^L restart)
pub fn client() -> Option<&'static str> {
    CLIENT.get().map(String::as_str)
}

/// This client's terminal type (kitty's is xterm-kitty): in a popup it's the --client one, in a pane the one that last used this session
pub fn client_term() -> String {
    let mut args = vec!["display-message", "-p"];
    if let Some(cl) = CLIENT.get() {
        args.extend(["-c", cl.as_str()]);
    }
    args.push("#{client_termname}");
    tmux(&args).trim().to_string()
}

/// Already inside tmux: switch this client to that session (no nested attach). Callers add the Ctrl+Q binding first
/// (`tmux::bind_detach`)
pub fn switch_to(name: &str) -> std::io::Result<std::process::ExitStatus> {
    let mut c = tmux_cmd();
    c.arg("switch-client");
    if let Some(cl) = CLIENT.get() {
        c.args(["-c", cl]);
    }
    c.args(["-t", name]).status()
}

/// Follow (see follow.rs): take this client to that session's pane without quitting the UI — the popup belongs to the client and
/// stays when the session switches. Done in one tmux command (switch to the pane first, then the session); without `--client`
/// it does nothing, since a bare switch-client might switch another terminal
pub fn show(session: &str, pane: &str) {
    let Some(cl) = CLIENT.get() else { return };
    let mut c = tmux_cmd();
    if !pane.is_empty() {
        c.args(["select-window", "-t", pane, ";", "select-pane", "-t", pane, ";"]);
    }
    let _ = c.args(["switch-client", "-c", cl, "-t", session]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Which session and pane this client is in now (highlighted as soon as the popup opens)
pub fn client_at() -> Option<(String, String)> {
    let cl = CLIENT.get()?;
    tmux(&["list-clients", "-F", "#{client_name}\t#{session_id}\t#{pane_id}"]).lines().find_map(|l| {
        let mut f = l.split('\t');
        (f.next() == Some(cl.as_str())).then(|| (f.next().unwrap_or("").to_string(), f.next().unwrap_or("").to_string()))
    })
}

/// This client's previous session (the one tmux's `switch-client -l` goes back to); none right after connecting, before any switch
pub fn last_session() -> Option<String> {
    let mut args = vec!["display-message", "-p"];
    if let Some(cl) = CLIENT.get() {
        args.extend(["-c", cl.as_str()]);
    }
    args.push("#{client_last_session}");
    Some(tmux(&args).trim().to_string()).filter(|s| !s.is_empty()).and_then(|n| session_id(&n))
}

/// Open a shell in a split beside this pane (yazi = zoomed to fill, running yazi), in dir (side = left/right split, else
/// top/bottom), returns the new pane's id.
/// Environment as in `open_in`
pub fn split(session: &str, target: &str, dir: &str, side: bool, yazi: bool, env: &[(String, String)]) -> Option<String> {
    let mut c = tmux_cmd();
    c.args(["split-window", if side { "-h" } else { "-v" }, "-t", target, "-P", "-F", "#{pane_id}"]);
    if yazi {
        c.arg("-Z"); // zoomed to fill; when yazi exits the pane closes and the zoom goes with it
    }
    open_in(c, session, dir, yazi, env)
}

/// Open a new window in this session running a shell (like prefix c, placed last), in dir, returns the new pane's id.
/// Environment as in `open_in`
pub fn new_window(session: &str, dir: &str, env: &[(String, String)]) -> Option<String> {
    let mut c = tmux_cmd();
    c.args(["new-window", "-t", &format!("{session}:"), "-P", "-F", "#{pane_id}"]);
    open_in(c, session, dir, false, env)
}

/// The account a shell opened beside the session carries: the environment of the registered "tool:account" layer (the same
/// `aca run` carries), without the session's identity.
/// Sessions aca starts have an empty tmux environment (the account is carried only on the agent's pane), so it's worked out
/// from the config here; not for unregistered sessions or shell sessions
pub fn shell_env(s: &Session) -> Vec<(String, String)> {
    if s.deck_id.is_empty() || s.tool.is_empty() {
        return vec![];
    }
    aca_core::agent::account_env(&aca_core::config::load(), &crate::tools::spec(&s.tool, &s.account)).unwrap_or_default()
}

/// Second half of split / new window: open in dir, run a shell (or yazi), get the new pane's id. session is the tmux session id.
/// Account: env (see `shell_env`) goes with `-e` to the new pane only, not into the session's environment.
/// Sessions made earlier still have their session id in the tmux environment (ACA_SESSION_ID, CLAUDE_SESSION_ID…), which the new
/// pane would inherit; the new pane is your own shell, not this session's agent (an `aca new` in it shouldn't hang as its child,
/// remote task results shouldn't be sent to it) → those are removed
fn open_in(mut c: Command, session: &str, dir: &str, yazi: bool, env: &[(String, String)]) -> Option<String> {
    for (k, v) in env {
        c.args(["-e", &format!("{k}={v}")]);
    }
    let env = tmux(&["show-environment", "-t", session]);
    if std::path::Path::new(dir).is_dir() {
        c.args(["-c", dir]);
    }
    let strip = strip_env(&env);
    if yazi {
        c.arg(format!("exec env{strip} yazi"));
    } else if !strip.is_empty() {
        c.arg(format!("exec env{strip} \"${{SHELL:-/bin/sh}}\" -l"));
    }
    let out = c.output().ok().filter(|o| o.status.success())?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    id.starts_with('%').then_some(id)
}

/// Each vendor's session id (ACA_SESSION_ID, CLAUDE_SESSION_ID, the old AGENTDECK_*): not carried into a newly opened shell
pub fn session_id_var(k: &str) -> bool {
    k.starts_with("AGENTDECK_") || k.ends_with("_SESSION_ID")
}

/// The account a temporary terminal carries: each vendor's config dir (CLAUDE_CONFIG_DIR / CODEX_HOME / PI_CODING_AGENT_DIR)
/// from the registered account layer (`shell_env`); if the agent is running, what's in its process wins. s is the registered
/// session (an extra window inside a session uses its session), pid / agent are this row's own
pub fn account_env(s: Option<&Session>, pid: u32, agent: Agent) -> Vec<(String, String)> {
    let keep: Vec<&str> = aca_core::agent::kinds().filter_map(|k| aca_core::agent::builtin(k)?.home_var()).collect();
    let mut out: Vec<(String, String)> =
        s.map(shell_env).unwrap_or_default().into_iter().filter(|(k, _)| keep.contains(&k.as_str())).collect();
    if pid > 0
        && let Some(k) = home_var(agent)
        && let Some(d) = config_home(pid, agent)
    {
        out.retain(|(x, _)| x != k);
        out.push((k.into(), d));
    }
    out
}

/// The built-in tool this vendor is; None for a shell and declared tools
fn builtin(agent: Agent) -> Option<&'static dyn aca_core::agent::Agent> {
    let kind = match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Pi => "pi",
        Agent::Opencode => "opencode",
        Agent::None | Agent::Other => return None,
    };
    aca_core::agent::builtin(kind)
}

/// Which environment variable takes this vendor's config dir; None where there's no such thing (shell, declared tools, opencode)
fn home_var(agent: Agent) -> Option<&'static str> {
    builtin(agent)?.home_var()
}

/// The config dir this row's agent uses, where its conversations are kept (`B` copies it): the running process's, else the
/// registered account's, else the vendor's default (`~/.claude`); None where there's no such thing
fn agent_home(s: &Session) -> Option<String> {
    let var = home_var(s.agent)?;
    let set = account_env(Some(s), s.pid, s.agent).into_iter().find(|(k, _)| k == var).map(|(_, d)| d);
    let d = set.or_else(|| builtin(s.agent)?.default_home().map(String::from))?;
    Some(expand(&d).trim_end_matches('/').to_string())
}

/// What to drop from show-environment: each vendor's session id. Joined as ` -u A -u B`
fn strip_env(env: &str) -> String {
    env.lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .filter(|k| session_id_var(k))
        .filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(|k| format!(" -u {k}"))
        .collect()
}

/// Duration (long form, for the preview and hints): just now / 40 sec / 12 min / 2 hours / 3 days
pub fn dur(sec: u64) -> String {
    match sec {
        0 => t!("刚刚", "just now").into(),
        1..=59 => tr!("{sec} 秒", "{sec}s"),
        60..=3599 => tr!("{} 分钟", "{}m", sec / 60),
        3600..=86399 => tr!("{} 小时", "{}h", sec / 3600),
        _ => tr!("{} 天", "{}d", sec / 86400),
    }
}

/// Duration (short form, the list's last column, at most [`age_w`] cells wide): now / 40s / 12m / 2h / 3d, in Chinese
/// 刚刚 / 40秒 / 12分 / 2时 / 3天 (the units `aca ls` uses)
pub fn age(sec: u64) -> String {
    match sec {
        0 => t!("刚刚", "now").into(),
        1..=59 => tr!("{sec}秒", "{sec}s"),
        60..=3599 => tr!("{}分", "{}m", sec / 60),
        3600..=86399 => tr!("{}时", "{}h", sec / 3600),
        _ => tr!("{}天", "{}d", sec / 86400),
    }
}

/// How wide the list's time column is: a Chinese unit takes two cells
pub fn age_w() -> usize {
    match i18n::lang() {
        Lang::Zh => 4,
        Lang::En => 3,
    }
}

/// Config dir the agent process uses (CLAUDE_CONFIG_DIR / CODEX_HOME / PI_CODING_AGENT_DIR in its environment);
/// unset or no such thing = None (use the default)
pub(crate) fn config_home(pid: u32, agent: Agent) -> Option<String> {
    let d = aca_core::proc::env_var(pid, home_var(agent)?)?;
    Some(expand(&d).trim_end_matches('/').to_string())
}

/// At most this many levels up when counting parents (same as `aca answer` counting parents in aca-core): finishes even if
/// the registry's parents form a cycle
pub const DEEP: usize = 32;

/// What each session hangs under: session key → parent key (only ones hanging, see [`Up::Under`]).
/// For walking up level by level (see [`Session::ancestors`])
pub fn hangs(v: &[Session]) -> HashMap<String, String> {
    v.iter().filter(|s| s.sub.is_none()).filter_map(|s| Some((s.name.clone(), s.under()?.to_string()))).collect()
}

/// Position depends only on group and order within the group; state, activity time and title (Claude changes it) don't
/// affect it — rows don't jump on the 2-second refresh.
/// What needs handling is found by icons, the top bar counts, `i` and `@`.
/// Unfolded subagents follow right under their session (nearest first: those in the same process come first), in the
/// order they started;
/// sub-panes come after them, by window and pane index; child sessions hanging under it (with their windows and whatever
/// they spawned) come after the parent's windows, in their own order within the group.
/// Same at every depth: after a session comes its whole family, then its next sibling
pub fn sort_sessions(v: &mut [Session]) {
    let slots: HashMap<String, (usize, i64, i64)> = v.iter().filter(|s| s.sub.is_none()).map(|s| (s.name.clone(), s.slot)).collect();
    let hangs = hangs(v);
    v.sort_by_cached_key(|s| {
        // Walk from the flush-left one down to this family level by level, each step (position in group, key); if a parent
        // isn't in the list, count flush left from there
        let mut path: Vec<_> = s.ancestors(&hangs).map_while(|p| Some((1, *slots.get(p)?, p.to_string(), (0, 0, 0)))).collect();
        path.reverse();
        path.push((1, s.slot, s.family().to_string(), (0, 0, 0)));
        // Windows and subagents come before this family's child sessions (first element smaller than the session step's)
        if let Some(x) = &s.sub {
            path.push((0, (0, 0, 0), String::new(), (if s.subagent.is_some() { 1 } else { 2 }, x.win, x.pane)));
        }
        path
    });
}

/// Order of groups, as a tree: a parent group is followed right by its subgroups; siblings by the registry's sort_order,
/// then by name
pub(crate) fn group_ranks<'a>(groups: impl Iterator<Item = &'a str>, order: &HashMap<String, i64>) -> HashMap<String, usize> {
    let key = |g: &str| -> Vec<(i64, String)> {
        let parts: Vec<&str> = g.split('/').collect();
        (1..=parts.len()).map(|i| (order.get(&parts[..i].join("/")).copied().unwrap_or(0), parts[i - 1].to_string())).collect()
    };
    let mut gs: Vec<&str> = groups.collect();
    gs.sort_by_cached_key(|g| key(g));
    gs.dedup();
    gs.into_iter().enumerate().map(|(i, g)| (g.to_string(), i)).collect()
}

/// Read the bottom of the screen: whether it's waiting for you (with the question), whether it's running (with seconds run).
pub fn screen_state(lines: &[String]) -> (Option<State>, String, u64) {
    let nonempty: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let tail = &nonempty[nonempty.len().saturating_sub(25)..];
    if tail[tail.len().saturating_sub(12)..].iter().any(|l| ASK.is_match(l)) {
        let q = tail.iter().map(|l| l.trim()).find(|l| l.ends_with('?') || l.ends_with('？')).unwrap_or("").to_string();
        return (Some(State::Ask), q, 0);
    }
    for l in tail.iter().rev() {
        if let Some(c) = RUNNING.captures(l) {
            let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
            return (Some(State::Running), String::new(), n(1) * 3600 + n(2) * 60 + n(3));
        }
    }
    (None, String::new(), 0)
}

/// Drop trailing blank lines ("blank" lines carrying only escape sequences count too)
fn trim_tail(out: String) -> Vec<String> {
    let mut lines: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| crate::ansi::strip(l).trim().is_empty()) {
        lines.pop();
    }
    lines
}

/// For full-screen reading: the session's current window screen + HISTORY lines of history above. -J rejoins lines tmux
/// wrapped itself; the UI rewraps by width.
/// target as in paste_into
pub fn capture_history(target: &str) -> Vec<String> {
    const HISTORY: &str = "-3000";
    trim_tail(tmux(&["capture-pane", "-p", "-e", "-J", "-S", HISTORY, "-t", target]))
}

/// The title the pane wrote itself: Claude's "✳ project title" minus the status symbol; never written (tmux defaults to
/// the hostname "myhost.local") counts as empty
fn own_title(title: &str) -> String {
    let mut cs = title.chars();
    let t: String = match (cs.next(), cs.next()) {
        (Some(c0), Some(' ')) if !c0.is_alphanumeric() => cs.collect(),
        _ => title.to_string(),
    };
    if t.split('.').next() == Some(HOST.as_str()) { String::new() } else { t }
}

/// All registered sessions sit under this one "profile" (aca has no profiles; the field only tells "registered" from
/// "plain tmux session")
pub const REGISTERED: &str = "default";

/// Sessions registered without a group are listed under this name.
/// Ungrouped sessions go into this section in the UI. An internal name (UI state is keyed by it), not translated;
/// ui.rs's group_label translates it for display
pub const UNGROUPED: &str = "未分组";

/// What "B copy" copies: the directory, plus Claude's / Codex's own conversation id
fn copy_info(path: &str, conv: &str) -> String {
    let mut v = vec![];
    if !path.is_empty() {
        v.push(format!("Path: {path}"));
    }
    if !conv.is_empty() {
        v.push(format!("Session: {conv}"));
    }
    v.join("\n")
}

/// What `B` copies (and `C` picks from), worked out when pressed: `info` with `Path:` turned into the directory it's really
/// working in (see `workdir`), the one it started in following as `Started in:` when that's elsewhere (Claude files the
/// conversation record under it), and `Home:` the agent's config dir
pub fn copy_text(s: &Session) -> String {
    let work = crate::workdir::find(&crate::workdir::Where::of(s)).dir;
    with_work_home(&s.info, &work, agent_home(s).as_deref())
}

fn with_work_home(info: &str, work: &str, home: Option<&str>) -> String {
    let mut v = vec![];
    for l in info.lines() {
        match l.strip_prefix("Path: ") {
            Some(p) if !work.is_empty() && work != p => {
                v.push(format!("Path: {work}"));
                v.push(format!("Started in: {p}"));
            }
            _ => v.push(l.to_string()),
        }
    }
    v.extend(home.map(|h| format!("Home: {h}")));
    v.join("\n")
}

fn kind_of(name: &str) -> Agent {
    match name {
        "" | "shell" => Agent::None,
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        "pi" => Agent::Pi,
        "opencode" => Agent::Opencode,
        _ => Agent::Other,
    }
}

/// Which vendor a tool name is: claude / codex themselves, a custom-named tool by its `kind`, declared tools count as others
pub fn agent_of_tool(cfg: &Config, tool: &str) -> Agent {
    match cfg.tool(tool) {
        Some(t) if !t.kind.is_empty() => kind_of(&t.kind),
        Some(_) => Agent::Other,
        None => kind_of(tool),
    }
}

/// How the account is written in the list: the registered name; one given as a directory is the directory name
/// (leading dot removed)
fn account_name(account: &str) -> String {
    aca_core::agent::account_label(account).trim_start_matches('.').to_string()
}

/// A registered session: which vendor, which account (the tool name if no account)
fn registered(cfg: &Config, s: &store::Session) -> (Agent, String) {
    let agent = agent_of_tool(cfg, &s.tool);
    if agent == Agent::None {
        return (Agent::None, String::new());
    }
    let who = if s.account.is_empty() { s.tool.clone() } else { account_name(&s.account) };
    (agent, who)
}

/// The directory a registered session is really working in (see `workdir`) without building the whole list: `aca show --json`'s
/// `work`. kitty's floating window (kfloat, cmd+enter) opened inside a session reads it: tmux only knows where the agent started.
/// Same fields as `build` takes for the row
pub fn work_dir(cfg: &Config, s: &store::Session) -> String {
    let Some(p) = aca_core::tmux::find(&s.id).filter(|p| !p.dead) else { return s.dir.clone() };
    let running = agent_in(&proc::fg_kids(), &p);
    let shell = agent_of_tool(cfg, &s.tool) == Agent::None;
    let r = status::load(&s.id);
    let w = crate::workdir::Where {
        dir: if shell && !p.path.is_empty() { p.path.clone() } else { s.dir.clone() },
        pid: running.map_or(0, |r| r.1),
        agent: match running.filter(|_| shell) {
            Some((tool, _)) => kind_of(tool),
            None => registered(cfg, s).0,
        },
        id: if r.conv.is_empty() { s.conv.clone() } else { r.conv },
    };
    crate::workdir::find(&w).dir
}

/// Accounts with a registered config dir: (vendor, name, dir). Claude first, then Codex, each by name
pub fn accounts(cfg: &Config) -> Vec<(Agent, String, String)> {
    let mut v: Vec<(Agent, String, String)> = cfg
        .accounts
        .iter()
        .flat_map(|(kind, m)| m.iter().filter(|(_, a)| !a.home.is_empty()).map(move |(n, a)| (kind_of(kind), n.clone(), a.home.clone())))
        .filter(|(a, _, _)| matches!(a, Agent::Claude | Agent::Codex))
        .collect();
    v.sort_by_key(|s| s.0 != Agent::Claude);
    v
}

/// An agent started by hand (in a shell session or a plain tmux session): `home` is the config dir in its process environment,
/// matched back to a registered account name, else the directory name
fn identify(tool: &str, home: Option<String>, cfg: &Config) -> (Agent, String) {
    let agent = kind_of(tool);
    let who = match home {
        Some(dir) => match cfg.accounts.get(tool).and_then(|m| m.iter().find(|(_, a)| a.home == dir)) {
            Some((name, _)) => name.clone(),
            None => dir.rsplit('/').next().unwrap_or(&dir).trim_start_matches('.').to_string(),
        },
        None => tool.to_string(),
    };
    (agent, who)
}

fn state_of(st: status::State) -> State {
    match st {
        status::State::Running => State::Running,
        status::State::Ask => State::Ask,
        status::State::Unread => State::Unread,
        status::State::Idle => State::Idle,
        status::State::Error => State::Error,
        status::State::Exited => State::Dead,
        status::State::Stopped => State::Stopped,
    }
}

/// Match child sessions to their parent (`parent` in the registry). Nested at any depth: hangs under the parent if the parent is
/// listed and in the same group (whether or not the parent is itself a child), otherwise flush left.
/// One that hasn't reached the top after [`DEEP`] levels — parents forming a cycle (a's parent is b, b's parent is a), a parent
/// on a cycle, absurdly deep — is treated as not a child: so the ones hanging can't form a cycle, and walking up finishes
fn link_parents(v: &mut [Session], sessions: &[store::Session]) {
    let by_id: HashMap<&str, &store::Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let listed: HashMap<String, (String, String)> =
        v.iter().filter(|s| s.sub.is_none()).map(|s| (s.name.clone(), (s.group.clone(), s.title.clone()))).collect();
    // A session that is its own parent is the same as having no parent (counted that way below too)
    let parent_of = |m: &store::Session| by_id.get(m.parent.as_str()).filter(|_| m.parent != m.id).copied();
    let endless = |m: &store::Session| std::iter::successors(Some(m), |m| parent_of(m)).nth(DEEP).is_some();
    for s in v.iter_mut() {
        let Some(m) = by_id.get(s.family()).filter(|m| !m.parent.is_empty() && m.parent != m.id && !endless(m)) else { continue };
        s.up = match by_id.get(m.parent.as_str()) {
            None => Up::Gone,
            Some(pm) => match listed.get(&pm.id) {
                Some((g, t)) if *g == s.group => Up::Under(pm.id.clone(), t.clone()),
                Some((_, t)) => Up::Apart(pm.id.clone(), t.clone()),
                None => Up::Apart(pm.id.clone(), pm.title.clone()),
            },
        };
    }
}

/// Match forks to their original (`fork_of` in the registry): the title as shown in the list; also noted when the original
/// isn't listed (deleted)
fn link_forks(v: &mut [Session], sessions: &[store::Session]) {
    let from: HashMap<&str, &str> =
        sessions.iter().filter(|s| !s.fork_of.is_empty()).map(|s| (s.id.as_str(), s.fork_of.as_str())).collect();
    if from.is_empty() {
        return;
    }
    let titles: HashMap<String, String> = v.iter().filter(|s| s.sub.is_none()).map(|s| (s.name.clone(), s.title.clone())).collect();
    for s in v.iter_mut().filter(|s| s.sub.is_none()) {
        if let Some(of) = from.get(s.name.as_str()) {
            s.fork = titles.get(*of).map_or(Fork::Gone, |t| Fork::Of(t.clone()));
        }
    }
}

/// Group hints: for `N` / `n` to pick a group by path (see group_for in create.rs)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupHints {
    /// (group path, default path)
    pub groups: Vec<(String, String)>,
}

pub fn group_hints() -> GroupHints {
    let groups = Store::open().and_then(|s| s.groups()).unwrap_or_default();
    // The default dir is normalized first, like `aca new -g .` (that's what store::group_by_dir compares)
    GroupHints { groups: groups.into_iter().map(|g| (g.path, paths::real(&g.default_path))).collect() }
}

/// group's siblings (those under the same parent, itself included), in list order; None = no such group.
/// ⇧↑ ⇧↓ moving a group counts positions by this
pub fn group_siblings(group: &str) -> Option<Vec<String>> {
    let groups = Store::open().and_then(|s| s.groups()).ok()?;
    let rows: Vec<(String, i64)> = groups.into_iter().map(|g| (g.path, g.order)).collect();
    siblings(&rows, group)
}

/// group's siblings in rows (those under the same parent group, itself included), ordered among themselves as in the list:
/// by sort_order, then name; None = no group
fn siblings(rows: &[(String, i64)], group: &str) -> Option<Vec<String>> {
    let parent = |p: &str| p.rsplit_once('/').map(|x| x.0.to_string());
    rows.iter().any(|r| r.0 == group).then_some(())?;
    let mut v: Vec<(i64, &str, &str)> =
        rows.iter().filter(|r| parent(&r.0) == parent(group)).map(|(p, o)| (*o, p.rsplit('/').next().unwrap_or(p), p.as_str())).collect();
    v.sort();
    Some(v.into_iter().map(|x| x.2.to_string()).collect())
}

/// Paths recorded in the config / database may contain ~
fn expand(d: &str) -> String {
    paths::expand(d)
}

/// Registered project dirs (archived ones too), newest registered first (candidates when picking a dir in `N`)
pub fn project_dirs() -> Vec<String> {
    let Ok(store) = Store::open() else { return vec![] };
    let mut rows: Vec<(u64, String)> =
        store.sessions(true).unwrap_or_default().into_iter().filter(|s| !s.dir.is_empty()).map(|s| (s.created, s.dir)).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    rows.into_iter().map(|r| r.1).collect()
}

/// Sessions someone is attached to in a terminal (by tmux session id). Not `#{session_attached}`: it counts control-mode clients too
fn attached_sessions() -> HashSet<String> {
    humans(&tmux(&["list-clients", "-F", "#{client_control_mode}\t#{session_id}"]))
}

fn humans(out: &str) -> HashSet<String> {
    out.lines().filter_map(|l| l.split_once('\t')).filter(|(ctl, _)| *ctl == "0").map(|(_, name)| name.to_string()).collect()
}

/// When there was last output (used both to compare with read marks and to compute idle time): the window's activity time;
/// the session's is when someone last touched it, which doesn't follow output
fn output_at(p: &Pane, now: u64) -> u64 {
    match (p.win_activity, p.activity) {
        (0, 0) => now,
        (0, a) => a,
        (w, _) => w,
    }
}

/// Each tmux session's main pane (by session id): for registered ones the agent's pane (`@aca_agent`), otherwise the
/// lowest-numbered pane.
/// The other windows / panes hang under it
fn main_panes<'a>(panes: &'a [Pane], known: &dyn Fn(&str) -> bool) -> HashMap<&'a str, &'a Pane> {
    let mut m: HashMap<&str, &Pane> = HashMap::new();
    for p in panes {
        let e = m.entry(p.sid.as_str()).or_insert(p);
        let rank = |x: &Pane| (!(known(&x.id) && x.agent), x.window, x.index);
        if rank(p) < rank(e) {
            *e = p;
        }
    }
    m
}

/// The agent running in the pane (started by hand in a shell, or the pane opened straight into it): (claude / codex, pid)
fn agent_in(kids: &Kids, p: &Pane) -> Option<(&'static str, u32)> {
    if p.pid == 0 {
        return None;
    }
    proc::agent_under(kids, p.pid).or_else(|| aca_core::agent::by_comm(&p.cmd).map(|a| (a, p.pid)))
}

/// What collection needs, read from outside
pub struct Inputs {
    pub now: u64,
    pub cfg: Config,
    pub sessions: Vec<store::Session>,
    pub groups: Vec<store::Group>,
    pub panes: Vec<Pane>,
    /// One per pane in panes: each pane's screen (with colors)
    pub screens: Vec<Vec<String>>,
    /// tmux session ids someone is attached to
    pub attached: HashSet<String>,
    pub kids: Kids,
}

pub fn collect() -> Vec<Session> {
    gather(true)
}

/// The first screen when the UI opens: schedules aren't read (the first time every session's transcript is read, hundreds of MB
/// taking a few tenths of a second); the refresh two seconds later fills them in
pub fn collect_first() -> Vec<Session> {
    gather(false)
}

fn gather(schedules: bool) -> Vec<Session> {
    let now = paths::now();
    let cfg = aca_core::config::load();
    let (sessions, groups) = match Store::open() {
        Ok(st) => (st.sessions(false).unwrap_or_default(), st.groups().unwrap_or_default()),
        Err(_) => (vec![], vec![]),
    };
    let panes = aca_core::tmux::panes();
    // Record pane titles whose title changed first (so the state files read below are current); no need to relist when the
    // session name follows, since registered sessions aren't identified by session name
    status::follow_titles(&cfg, &sessions, &panes);
    // Capture every pane's screen in one tmux command (one per pane would be dozens of processes per refresh); the process table and
    // attached clients are queried at the same time.
    // With color / link escape sequences (-e): the preview is colored as is, they're stripped when judging state
    let (screens, (attached, kids)) = thread::scope(|s| {
        let other = s.spawn(|| (attached_sessions(), proc::fg_kids()));
        let ids: Vec<&str> = panes.iter().map(|p| p.pane.as_str()).collect();
        let screens: Vec<Vec<String>> = aca_core::tmux::capture_many(&ids, true).into_iter().map(trim_tail).collect();
        (screens, other.join().unwrap_or_default())
    });
    // Fill in turn ends it didn't report (Codex errors, an untrusted Stop hook) first: so the state files read below are current
    status::settle(&cfg, &sessions, &panes, &|p| {
        let at = panes.iter().position(|q| q.pane == p.pane);
        at.map(|i| screens[i].iter().map(|l| crate::ansi::strip(l)).collect()).unwrap_or_default()
    });
    let homes = homes_of(&panes, &kids);
    let home_of = |pid: u32, a: Agent| homes.get(&(pid, a)).cloned().flatten();
    let mut out = build(Inputs { now, cfg, sessions, groups, panes, screens, attached, kids }, &status::load, &home_of);
    // Subagents each vendor spawned: read their transcripts (see subagent.rs); a session with background ones still running
    // counts as running
    crate::subagent::attach(&mut out, &status::load);
    // Things scheduled and not due yet: only while the agent is still open (gone once the process exits). Transcripts already read
    // are read only for what's newly written. Scheduled messages in the inbox are there even while it's stopped (if it isn't up
    // when one is due, it's delivered once it is)
    if schedules {
        let open = |s: &Session| !matches!(s.state, State::Dead | State::Stopped | State::Crashed | State::Exited);
        for s in out.iter_mut().filter(|s| s.sub.is_none() && !s.deck_id.is_empty()) {
            let mut due = if open(s) { aca_core::schedule::due(&status::load(&s.deck_id), now) } else { vec![] };
            due.extend(aca_core::schedule::inbox(&s.deck_id));
            due.sort_by_key(|d| d.next.unwrap_or(u64::MAX));
            s.due = due;
        }
    }
    out
}

type Homes = HashMap<(u32, Agent), Option<String>>;

/// Config dirs used by the agents running in panes: (pid, vendor) → dir.
/// Needs the process environment (one `ps` per process, 2–3 ms); a process's environment doesn't change, so answers are cached;
/// those not yet asked are asked in parallel
fn homes_of(panes: &[Pane], kids: &Kids) -> Homes {
    static HOMES: LazyLock<std::sync::Mutex<Homes>> = LazyLock::new(Default::default);
    let want: HashSet<(u32, Agent)> =
        panes.iter().filter(|p| !p.dead).filter_map(|p| agent_in(kids, p)).map(|(tool, pid)| (pid, kind_of(tool))).collect();
    let mut known = HOMES.lock().unwrap_or_else(|e| e.into_inner());
    // Drop processes that are gone (the pid may be reused by another process later)
    known.retain(|k, _| want.contains(k));
    let missing: Vec<(u32, Agent)> = want.into_iter().filter(|k| !known.contains_key(k)).collect();
    let got: Vec<Option<String>> = thread::scope(|s| {
        let hs: Vec<_> = missing.iter().map(|&(pid, agent)| s.spawn(move || config_home(pid, agent))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or(None)).collect()
    });
    known.extend(missing.into_iter().zip(got));
    known.clone()
}

/// What's seen on screen: if it can tell which vendor runs inside, use that vendor's own reader (the one in aca-core, shared
/// with registered sessions); otherwise read it the Claude way
fn seen_state(tool: Option<&str>, plain: &[String]) -> (Option<State>, String, u64) {
    use aca_core::screen::Seen;
    match tool.and_then(aca_core::agent::builtin) {
        Some(agent) => match agent.screen(plain) {
            Seen::Ask(q) => (Some(State::Ask), q, 0),
            Seen::Running(n) => (Some(State::Running), String::new(), n),
            Seen::Quiet | Seen::Moving | Seen::Unsure => (None, String::new(), 0),
        },
        None => screen_state(plain),
    }
}

/// Merge the registry, the panes actually in tmux, the state the hook writes and what's seen on screen into the list.
/// `run`: a session's runtime state (written by the hook); `home_of`: config dir in a process environment (to identify the
/// account of an agent started by hand)
pub fn build(inp: Inputs, run: &dyn Fn(&str) -> Run, home_of: &dyn Fn(u32, Agent) -> Option<String>) -> Vec<Session> {
    let Inputs { now, cfg, sessions, groups, panes, screens, attached, kids } = inp;
    let by_id: HashMap<&str, &store::Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let group_name = |g: &str| if g.is_empty() { UNGROUPED.to_string() } else { g.to_string() };
    let mut order: HashMap<String, i64> = groups.iter().map(|g| (g.path.clone(), g.order)).collect();
    // Ungrouped ones go after all groups
    order.insert(UNGROUPED.into(), i64::MAX);
    let names: Vec<String> = sessions.iter().map(|s| group_name(&s.group)).collect();
    // Empty groups get a position too (the list still shows their title)
    let ranks = group_ranks(names.iter().map(String::as_str).chain(groups.iter().map(|g| g.path.as_str())), &order);
    let mains = main_panes(&panes, &|id| by_id.contains_key(id));
    // The same registered id recorded on two tmux sessions (shouldn't happen): only the first one met counts, others are plain sessions
    let mut claimed: HashSet<&str> = HashSet::new();
    let mut out: Vec<Session> = vec![];
    let mut subs: Vec<(&Pane, Vec<String>)> = vec![];
    for (p, screen) in panes.iter().zip(screens) {
        if mains.get(p.sid.as_str()).is_none_or(|m| m.pane != p.pane) {
            subs.push((p, screen));
            continue;
        }
        let plain: Vec<String> = screen.iter().map(|l| crate::ansi::strip(l)).collect();
        let running = agent_in(&kids, p).filter(|_| !p.dead);
        let (scr, scr_q, scr_ran) = seen_state(running.map(|r| r.0), &plain);
        let spinning = p.title.chars().next().is_some_and(|c| SPINNER.contains(c));
        let act = output_at(p, now);
        let reg = by_id.get(p.id.as_str()).copied().filter(|s| claimed.insert(s.id.as_str()));
        let Some(s) = reg else {
            // Plain tmux session: whatever the screen says; if claude / codex was started inside by hand, the vendor and account come
            // from that process
            let state = match scr {
                _ if p.dead => State::Dead,
                Some(st) => st,
                None if spinning => State::Running,
                None => State::Idle,
            };
            let (agent, who) = match running {
                Some((tool, pid)) => identify(tool, home_of(pid, kind_of(tool)), &cfg),
                None => (Agent::None, String::new()),
            };
            let own = own_title(&p.title);
            out.push(Session {
                name: p.session.clone(),
                tmux: p.sid.clone(),
                title: if own.is_empty() { p.session.clone() } else { own },
                locked: false,
                group: "-".into(),
                state,
                question: scr_q,
                idle_for: now.saturating_sub(act),
                active_at: act,
                ran: scr_ran,
                attached: attached.contains(&p.sid),
                screen,
                pinned: false,
                noted: false,
                group_noted: false,
                space: crate::spaces::HOME,
                dir: p.path.clone(),
                profile: String::new(),
                tool: String::new(),
                account: String::new(),
                agent,
                who,
                pid: running.map_or(0, |r| r.1),
                deck_id: String::new(),
                info: format!("Path: {}", p.path),
                worktree: false,
                slot: (usize::MAX, 0, 0),
                pane: p.pane.clone(),
                sub: None,
                up: Up::Top,
                fork: Fork::No,
                subagents: vec![],
                subagent: None,
                tokens: vec![],
                error: String::new(),
                error_kind: String::new(),
                idle_stopped: 0,
                due: vec![],
                blank: false,
            });
            continue;
        };
        let r = run(&s.id);
        let handle = aca_core::agent::of(&cfg, s).ok().map(|x| x.0);
        let seen = handle.as_ref().filter(|_| !p.dead).map(|h| status::look(&s.id, &**h, &plain));
        let (st, question, ran) = status::merge(Some(p), &r, seen.as_ref(), now);
        let (mut state, mut question, mut ran) = (state_of(st), question, ran);
        let shell = agent_of_tool(&cfg, &s.tool) == Agent::None;
        // A session opened as a shell has no hook and no vendor reader: an agent started inside by hand that's asking / running goes
        // by the screen
        if shell && state == State::Idle {
            match scr {
                Some(x) => (state, question, ran) = (x, scr_q, scr_ran),
                None if spinning => state = State::Running,
                None => {}
            }
        }
        // An agent that isn't running but is still open counts idle time from what the hook reported (see `Run::hook_at`): otherwise
        // one unused for days would turn into minutes ago on a redraw
        let act = match r.hook_at() {
            at if at > 0 && !shell && matches!(state, State::Idle | State::Unread | State::Ask | State::Error) => at,
            _ => act,
        };
        let (agent, who) = match running.filter(|_| shell) {
            Some((tool, pid)) => identify(tool, home_of(pid, kind_of(tool)), &cfg),
            None => registered(&cfg, s),
        };
        let conv = if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() };
        let group = group_name(&s.group);
        // A shell session's dir is wherever it cd'd to (the registered one is only where it opened)
        let dir = if shell && !p.path.is_empty() { p.path.clone() } else { s.dir.clone() };
        out.push(Session {
            name: s.id.clone(),
            tmux: p.sid.clone(),
            title: status::title(s, &r),
            locked: s.title_locked,
            slot: (ranks[&group], s.order, s.created as i64),
            group,
            state,
            question,
            idle_for: now.saturating_sub(act),
            active_at: act,
            ran,
            attached: attached.contains(&p.sid),
            screen,
            pinned: false,
            noted: false,
            group_noted: false,
            space: crate::spaces::HOME,
            info: copy_info(&dir, &conv),
            dir,
            profile: REGISTERED.into(),
            tool: if shell { String::new() } else { s.tool.clone() },
            account: s.account.clone(),
            agent,
            who,
            pid: running.map_or(0, |r| r.1),
            deck_id: s.id.clone(),
            worktree: false,
            pane: p.pane.clone(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: r.live_tokens(now).into_iter().collect(),
            error: if state == State::Error { r.error.clone() } else { String::new() },
            error_kind: if state == State::Error { r.error_kind.clone() } else { String::new() },
            idle_stopped: if p.dead { r.idle_stopped } else { 0 },
            due: vec![],
            blank: false,
        });
    }
    // Extra windows inside a session (prefix c) and split panes (prefix % / "): listed under the session, with their own state and
    // their own agent detection.
    // Can't be restarted / renamed; d closes and D stops this pane
    let by_sid: HashMap<String, Session> = out.iter().map(|s| (s.tmux.clone(), s.clone())).collect();
    for (p, screen) in subs {
        let Some(parent) = by_sid.get(&p.sid) else { continue };
        let plain: Vec<String> = screen.iter().map(|l| crate::ansi::strip(l)).collect();
        let running = agent_in(&kids, p).filter(|_| !p.dead);
        let (scr, question, ran) = seen_state(running.map(|r| r.0), &plain);
        let spinning = p.title.chars().next().is_some_and(|c| SPINNER.contains(c));
        let state = match scr {
            _ if p.dead => State::Dead,
            Some(s @ (State::Ask | State::Running)) => s,
            _ if spinning => State::Running,
            _ => State::Idle,
        };
        let (agent, who) = match running {
            Some((tool, pid)) => identify(tool, home_of(pid, kind_of(tool)), &cfg),
            None => (Agent::None, String::new()),
        };
        let own = own_title(&p.title);
        // Claude's process name is its version number (and so is the window name tmux sets automatically); write it as claude
        let pretty = |n: &str| aca_core::agent::by_comm(n).map_or_else(|| n.to_string(), String::from);
        let split = p.win_panes > 1;
        let title = match () {
            _ if !own.is_empty() => own,
            // Unsplit window: the window name (the new name if renamed; otherwise tmux writes the foreground command); an unnamed new
            // window is called tmux, which doesn't count
            _ if !split && !p.win_name.is_empty() && p.win_name != "tmux" => pretty(&p.win_name),
            _ => pretty(&p.cmd),
        };
        let act = output_at(p, now);
        out.push(Session {
            name: format!("{}\u{1}{}", parent.name, p.pane),
            title,
            state,
            question,
            idle_for: now.saturating_sub(act),
            active_at: act,
            ran,
            attached: false,
            screen,
            pinned: false,
            noted: false,
            group_noted: false,
            space: crate::spaces::HOME,
            dir: p.path.clone(),
            deck_id: String::new(),
            info: format!("Path: {}", p.path),
            worktree: false,
            agent,
            who,
            pid: running.map_or(0, |r| r.1),
            pane: p.pane.clone(),
            sub: Some(Sub { parent: parent.name.clone(), parent_title: parent.title.clone(), win: p.window, pane: p.index, split }),
            idle_stopped: 0,
            due: vec![],
            ..parent.clone()
        });
    }
    // Registered but not in tmux (computer rebooted after stopping, tmux server gone): listed anyway, ⏎ restarts and carries on
    let live: HashSet<String> = out.iter().map(|s| s.name.clone()).collect();
    for s in sessions.iter().filter(|s| !live.contains(&s.id)) {
        let r = run(&s.id);
        let (agent, who) = registered(&cfg, s);
        // Bringing it back (unarchiving) counts as activity: otherwise one restored from the trash would be auto-removed again
        // next round (`[idle] remove_after`)
        let at = r.ts.max(r.restored).max(s.created);
        let conv = if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() };
        let group = group_name(&s.group);
        out.push(Session {
            name: s.id.clone(),
            tmux: String::new(),
            title: status::title(s, &r),
            locked: s.title_locked,
            slot: (ranks[&group], s.order, s.created as i64),
            group,
            state: if r.status == "error" { State::Crashed } else { State::Stopped },
            question: String::new(),
            idle_for: now.saturating_sub(at),
            active_at: at,
            ran: 0,
            attached: false,
            screen: vec![],
            pinned: false,
            noted: false,
            group_noted: false,
            space: crate::spaces::HOME,
            dir: s.dir.clone(),
            profile: REGISTERED.into(),
            tool: if agent == Agent::None { String::new() } else { s.tool.clone() },
            account: s.account.clone(),
            agent,
            who,
            pid: 0,
            deck_id: s.id.clone(),
            info: copy_info(&s.dir, &conv),
            worktree: false,
            pane: String::new(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: vec![],
            error: String::new(),
            error_kind: String::new(),
            idle_stopped: r.idle_stopped,
            due: vec![],
            blank: false,
        });
    }
    // Groups with no session at all (subgroups included): one item each, the list still shows their title, and N with the
    // highlight on it opens in that group. All sink to the very bottom of the list
    let within = |g: &str, top: &str| g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'));
    for g in groups.iter().filter(|g| !sessions.iter().any(|s| within(&s.group, &g.path))) {
        out.push(blank_row(&g.path, ranks[&g.path]));
    }
    link_parents(&mut out, &sessions);
    link_forks(&mut out, &sessions);
    sort_sessions(&mut out);
    out
}

/// Key of an empty group's item: this char, then the group (never in a session key)
const BLANK: char = '\u{3}';

/// The item an empty group takes in the list (`Session::blank`): not a session, no pane, dir or registered id.
/// After all sessions (plain tmux sessions included); empty groups among themselves by group order (rank)
pub fn blank_row(group: &str, rank: usize) -> Session {
    Session {
        name: format!("{BLANK}{group}"),
        tmux: String::new(),
        title: String::new(),
        locked: false,
        group: group.to_string(),
        state: State::Idle,
        question: String::new(),
        idle_for: 0,
        active_at: 0,
        ran: 0,
        attached: false,
        screen: vec![],
        pinned: false,
        noted: false,
        group_noted: false,
        space: crate::spaces::HOME,
        dir: String::new(),
        profile: REGISTERED.into(),
        tool: String::new(),
        account: String::new(),
        agent: Agent::None,
        who: String::new(),
        pid: 0,
        deck_id: String::new(),
        info: String::new(),
        worktree: false,
        slot: (usize::MAX, 1, rank as i64),
        pane: String::new(),
        sub: None,
        up: Up::Top,
        fork: Fork::No,
        subagents: vec![],
        subagent: None,
        tokens: vec![],
        error: String::new(),
        error_kind: String::new(),
        idle_stopped: 0,
        due: vec![],
        blank: true,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_session_left_last_comes_first_then_the_one_entered() {
        let been: Vec<String> = ["乙", "甲", "丙"].map(String::from).into();
        // Entered 甲, switched to 丁 inside, came out of 丁: 丁 newest, 甲 next, older ones move back, no duplicates
        assert_eq!(been_after("丁", "甲", &been), ["丁", "甲", "乙", "丙"]);
        // Came out of the same one it entered
        assert_eq!(been_after("丙", "丙", &been), ["丙", "乙", "甲"]);
        let many: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        assert_eq!(been_after("x", "y", &many).len(), 8);
    }

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_terminal_clients_count_as_open_elsewhere() {
        // $1 has someone attached (plus a control-mode client); $2 has only a control-mode client, the person left long ago
        let got = humans("0\t$1\n1\t$1\n1\t$2\n0\t$3\n");
        let mut got: Vec<_> = got.into_iter().collect();
        got.sort();
        assert_eq!(got, ["$1", "$3"]);
        assert!(humans("").is_empty());
    }

    #[test]
    fn detects_ask_with_question() {
        let s =
            v(&["● 我需要你拍板", "", "要用哪个方案?", "", "❯ 1. B", "  2. A", "", "Enter to select · ↑/↓ to navigate · Esc to cancel"]);
        assert_eq!(screen_state(&s), (Some(State::Ask), "要用哪个方案?".into(), 0));
    }

    #[test]
    fn detects_running_duration() {
        let bar = "─".repeat(40);
        let s = v(&[
            "⏺ Bash(ls)",
            "· Smooshing… (1h 9m 1s · ↓ 9.1k tokens · esc to interrupt)",
            &bar,
            "❯ ",
            &bar,
            "  ⏵⏵ bypass permissions on",
        ]);
        assert_eq!(screen_state(&s), (Some(State::Running), String::new(), 4141));
        let s = v(&["· Thinking… (45s · ↑ 1k tokens)", &bar, "❯ ", &bar]);
        assert_eq!(screen_state(&s).2, 45);
    }

    #[test]
    fn finished_is_not_running() {
        let bar = "─".repeat(40);
        let s = v(&["⏺ 完成了。", "✻ Sautéed for 1m 49s", &bar, "❯ ", &bar, "  ⏵⏵ bypass permissions on (shift+tab to cycle)"]);
        assert_eq!(screen_state(&s).0, None);
    }

    #[test]
    fn output_time_is_the_window_activity_not_the_last_attach() {
        // Entered and looked at 100, came back; Claude kept running and finished at 500. The session's activity stays at 100
        let p = Pane { activity: 100, win_activity: 500, ..Default::default() };
        assert_eq!(output_at(&p, 7), 500);
        assert_eq!(output_at(&Pane { activity: 100, ..Default::default() }, 7), 100);
        assert_eq!(output_at(&Pane::default(), 7), 7); // unreadable counts as now
    }

    fn pane(sid: &str, id: &str, agent: bool, pane: &str, win: u32, idx: u32) -> Pane {
        Pane {
            sid: sid.into(),
            session: format!("名字{sid}"),
            id: id.into(),
            agent,
            pane: pane.into(),
            window: win,
            index: idx,
            win_panes: 1,
            ..Default::default()
        }
    }

    #[test]
    fn the_agent_pane_is_the_session_and_the_rest_hang_under_it() {
        let panes = [
            // Registered session: the agent is in window 2 (window 1 was opened later and moved to the front)
            pane("$1", "ab12", false, "%5", 1, 0),
            pane("$1", "ab12", true, "%3", 2, 0),
            // Plain tmux session: the lowest-numbered pane
            pane("$2", "", false, "%8", 1, 1),
            pane("$2", "", false, "%7", 1, 0),
            // Marked but missing from the registry (registration deleted): a plain session
            pane("$3", "gone", true, "%10", 2, 0),
            pane("$3", "gone", false, "%9", 1, 0),
        ];
        let mains = main_panes(&panes, &|id| id == "ab12");
        assert_eq!((mains["$1"].pane.as_str(), mains["$2"].pane.as_str(), mains["$3"].pane.as_str()), ("%3", "%7", "%9"));
        assert_eq!(own_title("✳ 修登录"), "修登录");
        assert_eq!(own_title(&format!("{}.local", *HOST)), ""); // tmux's default hostname isn't a title
        let sub = |win, pane, split| Sub { parent: "a".into(), parent_title: String::new(), win, pane, split };
        assert_eq!((sub(2, 0, false).loc(), sub(1, 1, true).loc()), ("#2".to_string(), "#1.1".to_string()));
        // Ordering: sub-panes follow right after their session, by window and pane index
        let mk = |name: &str, s: Option<Sub>| Session { name: name.into(), sub: s, ..mk_session() };
        let mut v = vec![mk("b", None), mk("a\u{1}%5", Some(sub(2, 0, false))), mk("a\u{1}%3", Some(sub(1, 1, true))), mk("a", None)];
        sort_sessions(&mut v);
        assert_eq!(v.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["a", "a\u{1}%3", "a\u{1}%5", "b"]);
    }

    pub(crate) fn mk_session() -> Session {
        Session {
            name: String::new(),
            tmux: String::new(),
            title: String::new(),
            locked: false,
            group: "-".into(),
            state: State::Idle,
            question: String::new(),
            idle_for: 0,
            active_at: 0,
            ran: 0,
            attached: false,
            screen: vec![],
            pinned: false,
            noted: false,
            group_noted: false,
            space: crate::spaces::HOME,
            dir: String::new(),
            profile: String::new(),
            tool: String::new(),
            account: String::new(),
            agent: Agent::None,
            who: String::new(),
            pid: 0,
            deck_id: String::new(),
            info: String::new(),
            worktree: false,
            slot: (0, 0, 0),
            pane: String::new(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: vec![],
            error: String::new(),
            error_kind: String::new(),
            idle_stopped: 0,
            due: vec![],
            blank: false,
        }
    }

    #[test]
    fn order_ignores_state_activity_and_title() {
        let mk = |name: &str, title: &str, st: State, idle: u64, slot: (usize, i64, i64)| Session {
            name: name.into(),
            title: title.into(),
            state: st,
            idle_for: idle,
            slot,
            ..mk_session()
        };
        let mut v = vec![
            mk("d", "甲", State::Ask, 99999, (usize::MAX, 0, 0)), // plain tmux session, last even when waiting for you
            mk("c", "乙", State::Dead, 0, (1, 0, 0)),
            mk("b", "丙", State::Running, 5, (0, 2, 0)),
            mk("a", "丁", State::Idle, 9000, (0, 1, 7)),
            mk("e", "戊", State::Idle, 1, (0, 1, 3)), // same sort_order goes by creation time
        ];
        sort_sessions(&mut v);
        let order: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["e", "a", "b", "c", "d"]);
    }

    fn reg(id: &str, tool: &str, group: &str, order: i64, created: u64) -> store::Session {
        store::Session {
            id: id.into(),
            title: id.into(),
            tool: tool.into(),
            group: group.into(),
            order,
            dir: "/p".into(),
            created,
            ..Default::default()
        }
    }

    #[test]
    fn children_hang_under_their_parent_at_every_level() {
        let kid = |id: &str, group: &str, parent: &str| store::Session { parent: parent.into(), ..reg(id, "claude", group, 0, 1) };
        let sessions = vec![
            store::Session { title: "父".into(), ..reg("p", "claude", "g", 0, 1) },
            reg("q", "claude", "g", 0, 1),
            kid("c1", "g", "p"),
            kid("c2", "h", "p"),    // in another group
            kid("c3", "g", "GONE"), // parent deleted
            kid("gc", "g", "c1"),   // parent is itself a child: hangs under it
            kid("gg", "g", "gc"),   // one level further down
            kid("c2k", "h", "c2"),  // parent is flush left (its own parent is in another group); this one still hangs under it
            store::Session { title: "没列出来的".into(), ..reg("a", "claude", "g", 0, 1) },
            kid("ac", "g", "a"), // parent registered but not listed
        ];
        let mk = |name: &str, group: &str, slot: (usize, i64, i64)| Session {
            name: name.into(),
            title: name.into(),
            group: group.into(),
            slot,
            ..mk_session()
        };
        let win = |of: &Session| Session {
            name: format!("{}\u{1}%9", of.name),
            sub: Some(Sub { parent: of.name.clone(), parent_title: of.title.clone(), win: 2, pane: 0, split: false }),
            ..of.clone()
        };
        let p = Session { title: "父(屏幕)".into(), ..mk("p", "g", (0, 5, 100)) };
        let c1 = mk("c1", "g", (0, 0, 10)); // first in the group, still follows its parent
        let gc = mk("gc", "g", (0, 8, 0));
        let mut v = vec![
            mk("c3", "g", (0, 9, 0)),
            mk("gg", "g", (0, -3, 0)),
            win(&c1),
            c1,
            win(&gc),
            gc,
            mk("ac", "g", (0, 7, 0)),
            mk("c2k", "h", (1, -1, 0)),
            mk("c2", "h", (1, 0, 0)),
            win(&p),
            p,
            mk("q", "g", (0, 1, 50)),
        ];
        link_parents(&mut v, &sessions);
        sort_sessions(&mut v);
        let order: Vec<String> = v.iter().map(|s| s.name.replace('\u{1}', "#")).collect();
        // After a session comes its whole family: its own windows, child sessions (their windows, what they spawned)…
        // only then other sessions
        assert_eq!(order, ["q", "p", "p#%9", "c1", "c1#%9", "gc", "gc#%9", "gg", "ac", "c3", "c2", "c2k"]);
        let up = |n: &str| v.iter().find(|s| s.name == n).unwrap().up.clone();
        let under = Up::Under("p".into(), "父(屏幕)".into()); // the title as shown in the list
        assert_eq!(up("c1"), under);
        assert_eq!(up("c1\u{1}%9"), under); // the window follows its own session
        assert_eq!(up("c2"), Up::Apart("p".into(), "父(屏幕)".into()));
        assert_eq!(up("c3"), Up::Gone);
        assert_eq!(up("gc"), Up::Under("c1".into(), "c1".into()));
        assert_eq!(up("gc\u{1}%9"), Up::Under("c1".into(), "c1".into()));
        assert_eq!(up("gg"), Up::Under("gc".into(), "gc".into()));
        assert_eq!(up("c2k"), Up::Under("c2".into(), "c2".into()));
        assert_eq!(up("ac"), Up::Apart("a".into(), "没列出来的".into()));
        assert_eq!((up("p"), up("q")), (Up::Top, Up::Top));
        // Walking up: nearest first, up to the flush-left one
        let hangs = hangs(&v);
        let chain = |n: &str| v.iter().find(|s| s.name == n).unwrap().ancestors(&hangs).collect::<Vec<_>>().join(" ");
        assert_eq!(
            (chain("gg"), chain("gc\u{1}%9"), chain("c2k"), chain("p")),
            ("gc c1 p".into(), "c1 p".into(), "c2".into(), String::new())
        );
    }

    #[test]
    fn siblings_keep_their_own_order_at_every_level() {
        let mk = |name: &str, up: &str, order: i64| Session {
            name: name.into(),
            title: name.into(),
            group: "g".into(),
            slot: (0, order, 0),
            up: if up.is_empty() { Up::Top } else { Up::Under(up.into(), up.into()) },
            ..mk_session()
        };
        let win = |of: &Session, win: u32| Session {
            name: format!("{}#{win}", of.name),
            sub: Some(Sub { parent: of.name.clone(), parent_title: of.title.clone(), win, pane: 0, split: false }),
            ..of.clone()
        };
        let (a1, a1y) = (mk("a1", "a", 2), mk("a1y", "a1", 1));
        // Order within a group compares only with its own siblings at each level: a2 comes before a1, and a1's whole family
        // (grandchild's windows included) stays together
        let mut v = vec![
            mk("b", "", 2),
            win(&a1y, 2),
            mk("a1x", "a1", 9),
            a1y,
            win(&a1, 3),
            win(&a1, 2),
            a1,
            mk("a2", "a", 1),
            mk("b1", "b", -5),
            mk("a", "", 1),
        ];
        sort_sessions(&mut v);
        let order: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["a", "a2", "a1", "a1#2", "a1#3", "a1y", "a1y#2", "a1x", "b", "b1"]);
    }

    /// Forks don't hang under the original: flush left, they only remember what they forked from (the title as shown in the
    /// list); original not listed = deleted
    #[test]
    fn forks_stay_top_level_and_remember_who_they_came_from() {
        let fork = |id: &str, of: &str| store::Session { fork_of: of.into(), ..reg(id, "claude", "g", 0, 1) };
        let sessions = vec![reg("o", "claude", "g", 0, 1), fork("f", "o"), fork("lost", "gone")];
        let mut v: Vec<Session> = sessions.iter().map(|m| Session { name: m.id.clone(), group: m.group.clone(), ..mk_session() }).collect();
        v[0].title = "原来的(屏幕)".into();
        // An extra window inside f: not marked
        v.push(Session {
            name: "f\u{1}%9".into(),
            sub: Some(Sub { parent: "f".into(), parent_title: String::new(), win: 2, pane: 0, split: false }),
            ..v[1].clone()
        });
        link_parents(&mut v, &sessions);
        link_forks(&mut v, &sessions);
        let got: Vec<(&str, &Up, &Fork)> = v.iter().map(|s| (s.name.as_str(), &s.up, &s.fork)).collect();
        assert_eq!(
            got,
            [
                ("o", &Up::Top, &Fork::No),
                ("f", &Up::Top, &Fork::Of("原来的(屏幕)".into())),
                ("lost", &Up::Top, &Fork::Gone),
                ("f\u{1}%9", &Up::Top, &Fork::No),
            ]
        );
    }

    #[test]
    fn a_loop_of_parents_falls_back_to_top_level() {
        let kid = |id: &str, parent: &str| store::Session { parent: parent.into(), ..reg(id, "claude", "g", 0, 1) };
        let mut sessions = vec![
            kid("a", "b"), // a and b are each other's parent
            kid("b", "a"),
            kid("ak", "a"),  // parent on a cycle: walking up never ends either
            kid("me", "me"), // parent is itself: same as none
            kid("mek", "me"),
            kid("x", "y"), // three in a loop
            kid("y", "z"),
            kid("z", "x"),
        ];
        // A long chain: d0 flush left, d1 under d0 … those whose walk up ends hang as usual, those not at the top after DEEP levels
        // are all flush left
        sessions.push(reg("d0", "claude", "g", 0, 1));
        sessions.extend((1..=DEEP + 1).map(|i| kid(&format!("d{i}"), &format!("d{}", i - 1))));
        let mut v: Vec<Session> =
            sessions.iter().map(|m| Session { name: m.id.clone(), title: m.id.clone(), group: "g".into(), ..mk_session() }).collect();
        link_parents(&mut v, &sessions);
        let up = |v: &[Session], n: &str| v.iter().find(|s| s.name == n).unwrap().up.clone();
        let deep = |i: usize| format!("d{i}");
        for n in ["a", "b", "ak", "me", "x", "y", "z", &deep(DEEP), &deep(DEEP + 1)] {
            assert_eq!(up(&v, n), Up::Top, "{n}");
        }
        assert_eq!(up(&v, "mek"), Up::Under("me".into(), "me".into()));
        assert_eq!(up(&v, &deep(DEEP - 1)), Up::Under(deep(DEEP - 2), deep(DEEP - 2)));
        // Ordering and walking up both finish, nothing lost
        let n = v.len();
        sort_sessions(&mut v);
        assert_eq!(v.len(), n);
        let hangs = hangs(&v);
        assert!(v.iter().all(|s| s.ancestors(&hangs).count() < DEEP));
        // A hand-built cycle (never in the registry, link_parents breaks it) also finishes
        let mut ring = vec![
            Session { name: "r1".into(), up: Up::Under("r2".into(), "r2".into()), ..mk_session() },
            Session { name: "r2".into(), up: Up::Under("r1".into(), "r1".into()), ..mk_session() },
        ];
        sort_sessions(&mut ring);
        assert_eq!(ring[0].ancestors(&super::hangs(&ring)).count(), DEEP);
    }

    const CONFIG: &str = "\
[accounts.claude]
main = \"/h/.claude-main\"
team = { home = \"/h/.claude-team\", args = [\"--model\", \"opus\"] }
bed = { env = { CLAUDE_CODE_USE_BEDROCK = \"1\" } }
[accounts.codex]
alt = \"/h/.cx-alt\"
[tools.claude]
account = \"main\"
[tools.mycx]
kind = \"codex\"
account = \"alt\"
[tools.odd]
";

    #[test]
    fn a_registered_session_shows_the_account_it_was_registered_with() {
        let cfg = aca_core::config::parse(CONFIG);
        let who = |tool: &str, account: &str| {
            registered(&cfg, &store::Session { tool: tool.into(), account: account.into(), ..Default::default() })
        };
        assert_eq!(who("claude", "team"), (Agent::Claude, "team".into()));
        // No account → the tool name; given as a directory → the directory name
        assert_eq!(who("claude", ""), (Agent::Claude, "claude".into()));
        assert_eq!(who("codex", "/h/.newcodex"), (Agent::Codex, "newcodex".into()));
        // A custom-named tool goes by kind; declared tools count as others; one opened as a shell has none
        assert_eq!(who("mycx", "alt"), (Agent::Codex, "alt".into()));
        assert_eq!(who("odd", ""), (Agent::Other, "odd".into()));
        assert_eq!(who("shell", ""), (Agent::None, String::new()));
        // Only accounts with a registered config dir, Claude first, then Codex
        let names: Vec<(Agent, String)> = accounts(&cfg).into_iter().map(|a| (a.0, a.1)).collect();
        assert_eq!(names, [(Agent::Claude, "main".into()), (Agent::Claude, "team".into()), (Agent::Codex, "alt".into())]);
    }

    #[test]
    fn a_hand_started_agent_is_matched_back_to_its_account_by_its_config_dir() {
        let cfg = aca_core::config::parse(CONFIG);
        let id = |tool: &str, home: Option<&str>| identify(tool, home.map(String::from), &cfg);
        assert_eq!(id("claude", Some("/h/.claude-main")), (Agent::Claude, "main".into()));
        assert_eq!(id("claude", Some("/h/.claude-team")), (Agent::Claude, "team".into()));
        assert_eq!(id("codex", Some("/h/.cx-alt")), (Agent::Codex, "alt".into()));
        // Unregistered dir: the directory name; no config dir set: which vendor it is
        assert_eq!(id("claude", Some("/h/.claude-other")), (Agent::Claude, "claude-other".into()));
        assert_eq!(id("codex", None), (Agent::Codex, "codex".into()));
        // A dir registered under another vendor doesn't count
        assert_eq!(id("codex", Some("/h/.claude-main")), (Agent::Codex, "claude-main".into()));
    }

    #[test]
    fn agent_label_names_the_vendor_and_the_account() {
        let mut s = mk_session();
        assert_eq!(s.agent_label(), "");
        (s.agent, s.who) = (Agent::Claude, "team".into());
        assert_eq!(s.agent_label(), "Claude·team");
        (s.agent, s.who) = (Agent::Codex, "codex".into());
        assert_eq!(s.agent_label(), "Codex");
        (s.agent, s.who) = (Agent::Other, "odd".into());
        assert_eq!(s.agent_label(), "odd");
    }

    #[test]
    fn groups_follow_the_tree_parents_before_children() {
        let order: HashMap<String, i64> =
            [("b", 0), ("b/y", 2), ("b/x", 5), ("a", 1), ("c", -1)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let r = group_ranks(["a", "b/x", "b", "c", "b/y", "a"].into_iter(), &order);
        let mut gs: Vec<(&usize, &str)> = r.iter().map(|(g, i)| (i, g.as_str())).collect();
        gs.sort();
        let gs: Vec<&str> = gs.into_iter().map(|(_, g)| g).collect();
        assert_eq!(gs, ["c", "b", "b/y", "b/x", "a"]);
    }

    #[test]
    fn siblings_share_the_parent_and_sort_like_the_list() {
        let rows: Vec<(String, i64)> = [("b", 0), ("b/y", 2), ("b/x", 5), ("a", 1), ("c", -1), ("Z", 1), ("b/y/1", 0)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        assert_eq!(siblings(&rows, "a").unwrap(), ["c", "b", "Z", "a"]); // ties at rank 1 go by name
        assert_eq!(siblings(&rows, "b/x").unwrap(), ["b/y", "b/x"]);
        assert_eq!(siblings(&rows, "b/y/1").unwrap(), ["b/y/1"]);
        assert_eq!(siblings(&rows, "d"), None);
    }

    #[test]
    fn copy_info_is_the_dir_and_the_conversation() {
        assert_eq!(copy_info("/p", "abc"), "Path: /p\nSession: abc");
        assert_eq!(copy_info("/p", ""), "Path: /p");
        assert_eq!(copy_info("", ""), "");
    }

    #[test]
    fn b_copies_where_it_really_works_and_its_config_dir() {
        let info = "Path: /w\nSession: abc";
        assert_eq!(with_work_home(info, "/w", Some("/h/.claude-x")), "Path: /w\nSession: abc\nHome: /h/.claude-x");
        // cd'd into /w/app: that's the path; where it started stays (its record is filed under it)
        assert_eq!(with_work_home(info, "/w/app", None), "Path: /w/app\nStarted in: /w\nSession: abc");
        assert_eq!(with_work_home("", "", None), "");
    }

    #[test]
    fn a_hand_started_agent_is_read_the_way_its_own_kind_draws_the_screen() {
        // Codex draws "• Working (1m 05s • esc to interrupt)" while running: reading it the Claude way doesn't recognize it
        let codex = v(&["› 写一篇长文", "", "• Working (1m 05s • esc to interrupt)", "", "› Ask Codex to do anything"]);
        assert_eq!(screen_state(&codex).0, None);
        assert_eq!(seen_state(Some("codex"), &codex), (Some(State::Running), String::new(), 65));
        let gate = v(&["  Hooks need review", "› 1. Review hooks", "  2. Trust all and continue", "  enter confirm · esc skip"]);
        assert_eq!(seen_state(Some("codex"), &gate), (Some(State::Ask), "Hooks need review".into(), 0));
        // Claude's, and ones whose vendor can't be told: the original reader
        let claude = v(&["· Thinking… (45s · ↑ 1k tokens)", "❯ "]);
        assert_eq!((seen_state(Some("claude"), &claude).0, seen_state(None, &claude).0), (Some(State::Running), Some(State::Running)));
        assert_eq!(seen_state(Some("codex"), &[]), (None, String::new(), 0));
    }

    #[test]
    fn empty_screen() {
        assert_eq!(screen_state(&[]).0, None);
    }

    #[test]
    fn a_split_drops_the_session_ids_but_keeps_the_account() {
        let env = "ACA_SESSION_ID=ab12\nACA_TOOL=claude\nAGENTDECK_INSTANCE_ID=abc\nCLAUDE_CONFIG_DIR=/h/.ct\nCLAUDE_SESSION_ID=1-2\n-SSH_AGENT_PID\nDISPLAY=:0\n";
        assert_eq!(strip_env(env), " -u ACA_SESSION_ID -u AGENTDECK_INSTANCE_ID -u CLAUDE_SESSION_ID");
        assert_eq!(strip_env("DISPLAY=:0\n"), "");
        assert_eq!(strip_env("AGENTDECK_X;rm=1\n"), ""); // odd names aren't spliced into the command
    }

    /// A scene with everything: a registered Claude session (running, with an extra window), one opened as a shell (codex started
    /// by hand inside), one registered but not in tmux (exited on an error last time), a plain tmux session
    fn scene() -> Vec<Session> {
        let cfg = aca_core::config::parse(CONFIG);
        let sessions = vec![
            store::Session {
                title: "修登录".into(),
                account: "team".into(),
                conv: "c-0".into(),
                dir: "/p/api".into(),
                ..reg("aa11", "claude", "work", 1, 10)
            },
            store::Session { dir: "/q".into(), ..reg("bb22", "shell", "", 1, 20) },
            store::Session { title: "停着的".into(), ..reg("cc33", "codex", "work", 2, 30) },
        ];
        let groups = vec![store::Group { path: "work".into(), order: 1, default_path: String::new() }];
        let panes = vec![
            Pane {
                pid: 100,
                cmd: "2.1.290".into(),
                path: "/p/api/src".into(),
                title: "⠐ 修登录".into(),
                win_activity: 990,
                ..pane("$1", "aa11", true, "%1", 1, 0)
            },
            Pane {
                pid: 110,
                cmd: "zsh".into(),
                path: "/p/api".into(),
                win_name: "zsh".into(),
                win_activity: 800,
                ..pane("$1", "aa11", false, "%2", 2, 0)
            },
            Pane { pid: 200, cmd: "zsh".into(), path: "/q/web".into(), win_activity: 700, ..pane("$2", "bb22", true, "%3", 1, 0) },
            Pane {
                pid: 300,
                cmd: "zsh".into(),
                path: "/tmp".into(),
                session: "scratch".into(),
                activity: 600,
                ..pane("$3", "", false, "%4", 0, 0)
            },
        ];
        let screens = vec![v(&["一屏字"]), vec![], vec![], vec![]];
        let kids: Kids = [(200, vec![(201, "codex".to_string())])].into_iter().collect();
        let run = |id: &str| match id {
            "aa11" => Run { status: "running".into(), turn: 935, conv: "c-1".into(), ..Default::default() },
            "cc33" => Run { status: "error".into(), ts: 500, ..Default::default() },
            _ => Run::default(),
        };
        let home_of = |pid: u32, a: Agent| (pid == 201 && a == Agent::Codex).then(|| "/h/.dd".to_string());
        let inp = Inputs { now: 1000, cfg, sessions, groups, panes, screens, attached: ["$1".to_string()].into(), kids };
        build(inp, &run, &home_of)
    }

    #[test]
    fn groups_without_sessions_get_a_blank_in_their_place() {
        let cfg = aca_core::config::parse(CONFIG);
        let g = |path: &str, order: i64| store::Group { path: path.into(), order, default_path: String::new() };
        // The empty group empty comes before work; fun itself has no sessions but subgroup fun/app does: not empty;
        // fun/lab with no session at all is
        let groups = vec![g("empty", 1), g("work", 2), g("fun", 3), g("fun/app", 1), g("fun/lab", 2)];
        let sessions = vec![reg("aa11", "claude", "work", 1, 10), reg("bb22", "claude", "fun/app", 1, 20)];
        let panes =
            vec![Pane { pid: 300, cmd: "zsh".into(), path: "/tmp".into(), session: "scratch".into(), ..pane("$3", "", false, "%4", 0, 0) }];
        let inp = Inputs { now: 1000, cfg, sessions, groups, panes, screens: vec![vec![]], attached: HashSet::new(), kids: Kids::new() };
        let out = build(inp, &|_| Run::default(), &|_, _| None);
        let rows: Vec<(&str, bool)> = out.iter().map(|s| (s.group.as_str(), s.blank)).collect();
        // Empty groups sink to the very bottom (even plain tmux sessions are above them), among themselves by group order
        assert_eq!(rows, [("work", false), ("fun/app", false), ("-", false), ("empty", true), ("fun/lab", true)]);
        let b = out.iter().find(|s| s.blank).unwrap();
        assert_eq!((b.profile.as_str(), b.deck_id.as_str(), b.dir.as_str(), b.sub.is_none()), (REGISTERED, "", "", true));
        // Keys don't collide with sessions', one per group
        assert_ne!(out[3].name, out[4].name);
        assert!(!out.iter().filter(|s| s.blank).any(|s| s.name == s.group));
    }

    #[test]
    fn the_list_merges_the_registry_tmux_and_what_the_hooks_said() {
        let out = scene();
        let names: Vec<String> = out.iter().map(|s| s.name.replace('\u{1}', "#")).collect();
        // Groups in registry order, ungrouped after all groups, plain tmux sessions sink to the end; extra windows follow their session
        assert_eq!(names, ["aa11", "aa11#%2", "cc33", "bb22", "scratch"]);
        let s = |n: &str| out.iter().find(|s| s.name.replace('\u{1}', "#") == n).unwrap();

        // Registered and running: the key is the registered id, found in tmux by session id; the account is the registered one;
        // the conversation id the hook reports wins
        let a = s("aa11");
        assert_eq!((a.tmux.as_str(), a.pane.as_str(), a.deck_id.as_str(), a.profile.as_str()), ("$1", "%1", "aa11", REGISTERED));
        assert_eq!((a.title.as_str(), a.group.as_str(), a.state, a.ran), ("修登录", "work", State::Running, 65));
        assert_eq!((a.agent, a.who.as_str(), a.tool.as_str(), a.account.as_str(), a.pid), (Agent::Claude, "team", "claude", "team", 100));
        assert_eq!((a.dir.as_str(), a.info.as_str(), a.attached, a.idle_for), ("/p/api", "Path: /p/api\nSession: c-1", true, 10));
        assert_eq!(a.screen, ["一屏字"]);

        // The extra window inside it: its own row, not the registered pane (can't be restarted / renamed)
        let w = s("aa11#%2");
        assert_eq!(
            (w.title.as_str(), w.state, w.deck_id.as_str(), w.pane.as_str(), w.dir.as_str()),
            ("zsh", State::Idle, "", "%2", "/p/api")
        );
        assert_eq!(w.sub.as_ref().map(|x| (x.parent.as_str(), x.loc())), Some(("aa11", "#2".to_string())));
        assert_eq!(w.family(), "aa11");

        // Registered, not in tmux: listed, drawn as ✕ since it exited on an error last time
        let c = s("cc33");
        assert_eq!((c.state, c.tmux.as_str(), c.pane.as_str(), c.title.as_str()), (State::Crashed, "", "", "停着的"));
        assert_eq!((c.agent, c.who.as_str(), c.idle_for), (Agent::Codex, "codex", 500));

        // Opened as a shell: the dir follows cd; the codex started inside by hand is identified by its process's config dir
        // (unregistered → the directory name)
        let b = s("bb22");
        assert_eq!((b.group.as_str(), b.dir.as_str(), b.tool.as_str(), b.state), (UNGROUPED, "/q/web", "", State::Idle));
        assert_eq!((b.agent, b.who.as_str(), b.pid), (Agent::Codex, "dd", 201));

        // Plain tmux session: the key is the session name, no profile
        let p = s("scratch");
        assert_eq!((p.tmux.as_str(), p.group.as_str(), p.profile.as_str(), p.deck_id.as_str(), p.idle_for), ("$3", "-", "", "", 400));
        assert_eq!((p.title.as_str(), p.slot.0), ("scratch", usize::MAX));
    }

    #[test]
    fn a_stopped_one_counts_from_when_it_was_brought_back() {
        // Both stopped, not in tmux; the second was just restored from the trash
        let cfg = aca_core::config::parse(CONFIG);
        let sessions = vec![reg("aa11", "claude", "", 1, 10), reg("bb22", "claude", "", 2, 20)];
        let run = |id: &str| match id {
            "aa11" => Run { status: "ended".into(), ts: 300, ..Default::default() },
            _ => Run { status: "ended".into(), ts: 300, restored: 990, ..Default::default() },
        };
        let inp = Inputs { now: 1000, cfg, sessions, groups: vec![], panes: vec![], screens: vec![], attached: HashSet::new(), kids: Kids::new() };
        let out = build(inp, &run, &|_, _| None);
        let got: Vec<(&str, State, u64)> = out.iter().map(|s| (s.name.as_str(), s.state, s.active_at)).collect();
        assert_eq!(got, [("aa11", State::Stopped, 300), ("bb22", State::Stopped, 990)]);
    }

    #[test]
    fn an_idle_agent_counts_from_its_last_turn_not_from_a_redraw() {
        // All four were redrawn at 995 (settings changed, window resized); the hook reported long before
        let cfg = aca_core::config::parse(CONFIG);
        let sessions = vec![
            reg("aa11", "claude", "", 1, 10),
            reg("bb22", "claude", "", 2, 20),
            reg("cc33", "shell", "", 3, 30),
            reg("dd44", "claude", "", 4, 40),
        ];
        let at = |sid: &str, id: &str| Pane { pid: 0, cmd: "2.1.300".into(), win_activity: 995, ..pane(sid, id, true, sid, 1, 0) };
        let panes = vec![at("$1", "aa11"), at("$2", "bb22"), Pane { cmd: "zsh".into(), ..at("$3", "cc33") }, at("$4", "dd44")];
        let run = |id: &str| match id {
            // Started at 800, finished at 900, the one at 960 is the idle-for-a-minute reminder, not activity
            "aa11" => {
                Run { status: "done".into(), event: "Notification".into(), ts: 960, turn: 800, done: 900, seen: 950, ..Default::default() }
            }
            // Started asking you at 940
            "bb22" => Run { status: "waiting".into(), ts: 940, turn: 930, ..Default::default() },
            // A shell session has no hook, only the window to go by
            "cc33" => Run { started: 100, ..Default::default() },
            // Running: the window keeps printing, go by the window
            _ => Run { status: "running".into(), ts: 700, turn: 700, ..Default::default() },
        };
        let ask = v(&[" Do you want to proceed?", " ❯ 1. Yes", "   2. No", "", " Esc to cancel · Tab to amend"]);
        let screens = vec![vec![], ask, vec![], v(&["✻ Pondering… (5m 0s · esc to interrupt)"])];
        let inp = Inputs { now: 1000, cfg, sessions, groups: vec![], panes, screens, attached: HashSet::new(), kids: Kids::new() };
        let out = build(inp, &run, &|_, _| None);
        let got: Vec<(&str, State, u64, u64)> = out.iter().map(|s| (s.name.as_str(), s.state, s.idle_for, s.active_at)).collect();
        assert_eq!(
            got,
            [
                ("aa11", State::Idle, 100, 900),
                ("bb22", State::Ask, 60, 940),
                ("cc33", State::Idle, 5, 995),
                ("dd44", State::Running, 5, 995)
            ]
        );
    }
}
