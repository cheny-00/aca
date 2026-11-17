//! The interface: session list + live screen preview of the highlighted session. Refreshes in the background every 2 s; gives up the
//! terminal on attach and resumes drawing after detach.
//!
//! This file keeps `App` (its fields, how it is built, rebuilding the list, the highlight and moving it) and the tests that drive
//! several pages at once. Each page or part lives in its own file next to it: its state, its share of `impl App`, its drawing and
//! its tests (bill, confirm, draft, jobs, keys, list, order, overlays, pick, reader, run, screen, settings, space, term, tools,
//! trace).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::stdout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, CellDiffOption, CellWidth};
use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame, Terminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use aca_core::i18n::{self, Lang, Tr, noun};
use aca_core::runtime::mark;
use aca_core::{t, tr};
use aca_records::agent::Agent;
use aca_records::subagent::{Phase, Subagent};
use aca_records::words;
use aca_records::workdir::Where;

use crate::ansi::{self, Styled};
use crate::create::{self, DirPick, DirRow, Draft, NewReq, Picked, Removal, Src};
use crate::data::{DEEP, Fork, Session, State, Sub, Up, collect, hangs, sort_sessions};
use crate::diff::{self, Glance, Mode};
use crate::follow::{Follow, Place};
use crate::groups;
use crate::handoff::Source;
use crate::kit::{
    SEP, agent_style, button, clip, clip_left, dim, esc_hint, group_label, icon, key_spans, key_style, keys_line, modal_title, spread,
    wrap_width,
};
use crate::mcp;
use crate::notes::{self, Notes, group_key, is_group_key};
use crate::ops::{self, Job, Outcome, Target};
use crate::overlay::{Flow, Fx, Overlay, Setting};
use crate::pins::Pins;
use crate::reads::Reads;
use crate::spaces::{self, Spaces};
use crate::state::View;
use crate::theme;
use crate::tools::Tool;
use crate::trash::Trash;
use crate::undo::{self, Undo};
use crate::view::{DAYS, External, Preview, SEEN_SLACK, Scope, Split, TermReq};

mod bill;
mod confirm;
mod draft;
mod jobs;
mod keys;
mod list;
mod order;
mod overlays;
mod pick;
mod reader;
mod run;
mod screen;
mod settings;
mod space;
mod term;
mod tools;
mod trace;

use bill::{BillView, draw_bill, read_bill};
use confirm::{Ask, Batch, Confirm, GroupPick, GroupRemoval, NoteEdit, Pending, Prompt, adopt_plan, draw_confirm, draw_prompt};
use draft::draw_draft;
use jobs::{Vanish, target_of};
use list::{
    AGENT_SEP, Boxed, Folds, HintMatch, Jump, Row, agent_icon, agent_row, agents_line, group_keys, group_note, group_pane, jump_hints,
    jump_targets, local_when, match_hint, menu_bar, menu_bar_in, preview_head, preview_lines, preview_name, session_icon, space_of,
    state_word, strip_prompt,
};
use reader::{Clip, Reader, draw_help, draw_reader};
use run::{cant_fill, closes_popup, key_is, write_view};
use screen::{draw, floating, input_row, input_rule, modal, modal_area, modal_chrome, modal_frame, side_panel, side_width, title_rule};
use settings::{Set, Settings, ThemeMenu, draw_settings, draw_themes};
use tools::draw_tools;
use trace::{TraceView, below_tag, draw_trace};

pub(crate) use list::list_rows;
pub(crate) use run::{run, snapshot};

const REFRESH: Duration = Duration::from_secs(2);
/// How long the bottom bar's message stays
const FLASH: Duration = Duration::from_secs(3);
/// How the last interface went away, background errors: right when the interface opens you may not be looking yet, so keep it longer
const NOTICE: Duration = Duration::from_secs(12);
/// How often to apply `[idle]`: stop_after stops idle ones, remove_after removes long-stopped ones; a freshly opened interface first waits
/// IDLE_FIRST (see `idle_watch`)
const IDLE_EVERY: Duration = Duration::from_secs(300);
const IDLE_FIRST: Duration = Duration::from_secs(30);
/// How often to check whether the event stream file changed (one stat); if it did, reload right away instead of waiting for REFRESH
const EVENT_TICK: Duration = Duration::from_millis(150);
/// Event-triggered refreshes are at least this far apart
const EVENT_GAP: Duration = Duration::from_millis(400);
/// Ones removed with d stay hidden this long after it's done: a background refresh started before the removal may arrive late; don't let
/// them flash back
const GONE_LINGER: Duration = Duration::from_secs(5);
/// Digits typed further apart than this start over (1 then 2 = number 12)
const DIGIT_GAP: Duration = Duration::from_millis(1200);
/// Keys within this time after startup are all dropped: that's when Moshi types commands on your behalf after connecting, and no human
/// acts this fast. Not when opened by your own keypress (see startup_quiet)
const STARTUP_QUIET: Duration = Duration::from_millis(800);
/// Keys within this long after coming back from a session are dropped: keys pressed in a burst when leaving, and the terminal's replies,
/// are still in flight. Any longer would swallow keys pressed right after
const BACK_QUIET: Duration = Duration::from_millis(100);
/// How often to reread while the billing panel is open (each time reads all the records, a second or two)
const BILL_EVERY: Duration = Duration::from_secs(300);
/// The diff stats atop the preview and the diff x shows (the git of the highlighted repo): when the highlight hasn't changed and it has no
/// new activity, recount at most this often (so changes made elsewhere show up too)
const GLANCE_EVERY: Duration = Duration::from_secs(30);
/// Stay quiet this long after dropping a burst of automatic input: a command and its trailing Enter may arrive separately, and an Enter
/// arriving on its own would be taken as "enter"
const BURST_QUIET: Duration = Duration::from_millis(500);
/// While the highlight moves fast (swiping on a phone, holding an arrow key: less than this between two moves), don't switch the preview
/// yet; switch once it has stopped this long. Switching the preview at every step changes three times as much screen as moving the
/// highlight alone (measured ~800 vs ~270 bytes); mosh can't carry that, and the picture lags behind the finger
const SETTLE: Duration = Duration::from_millis(180);
/// Within this long after a move, new data from background refreshes is held and the view isn't saved yet: no full-screen redraw mid-swipe
const HOLD: Duration = Duration::from_millis(600);
/// The one that yielded (see `App::yielded`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Yielded {
    /// The ^ / % that was picked: w was pressed, entering "open ones"
    Stopped(Pick),
    /// "Open ones": ^ / % was picked inside it
    Open,
}

/// Show only one state (`!` `@` `#` `&` `^`, same keys as agent-deck; `%` is aca's own): stacks with group and range, ignored while
/// searching. Not saved; gone once turned off
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    /// ● running
    Run,
    /// ◐ waiting for you: awaiting confirmation and unread both count
    Wait,
    /// ○ idle
    Idle,
    /// ✕ error: the session is still open, stuck on an error (needs you to act)
    Err,
    /// ■ not running: exited, stopped, closed, archived; errors that aren't running (✕, mostly closed ones) count too. ☾ is included
    Off,
    /// ☾ idle, auto-stopped by aca (`[idle] stop_after`)
    Reaped,
}

impl Pick {
    fn hit(self, s: &Session) -> bool {
        match self {
            Pick::Reaped => reaped(s),
            _ => self.covers(s.state),
        }
    }

    /// The ones recognizable from the state alone; ☾ also needs to know whether it was stopped for being idle, so not here
    fn covers(self, st: State) -> bool {
        match self {
            Pick::Run => st == State::Running,
            Pick::Wait => matches!(st, State::Ask | State::Unread),
            Pick::Idle => st == State::Idle,
            Pick::Err => st == State::Error,
            Pick::Off => matches!(st, State::Exited | State::Dead) || st.gone(),
            Pick::Reaped => false,
        }
    }

    /// The two stopped kinds: "open ones" has none
    fn stopped(self) -> bool {
        matches!(self, Pick::Off | Pick::Reaped)
    }

    /// The key, and the line shown when the list is empty ("nothing running · ! back")
    fn label(self) -> (char, &'static str) {
        match self {
            Pick::Run => ('!', t!("暂无运行中的会话", "nothing running")),
            Pick::Wait => ('@', t!("暂无待处理会话", "nothing to look at")),
            Pick::Idle => ('#', t!("暂无空闲的会话", "nothing idle")),
            Pick::Err => ('&', t!("暂无出错的会话", "no errors")),
            Pick::Off => ('^', t!("暂无已停止的会话", "nothing stopped")),
            Pick::Reaped => ('%', t!("暂无自动停止的会话", "nothing auto-stopped")),
        }
    }
}

/// Order in `F`'s small list (same as the counts on the right of the top bar)
const PICKS: [Pick; 6] = [Pick::Run, Pick::Wait, Pick::Idle, Pick::Err, Pick::Off, Pick::Reaped];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Below,
    Right,
    Focus,
}

/// This wide counts as a wide screen: preview on the right (list 40 columns + preview 60). Narrower is a phone (portrait / landscape)
const AUTO_RIGHT_W: u16 = 100;
/// Below needs at least this height
const BELOW_MIN_H: u16 = 16;
/// Rows the list keeps in "mostly preview"
const FOCUS_ROWS: u16 = 4;

fn narrow(w: u16) -> bool {
    w < AUTO_RIGHT_W
}

fn preview_side(mode: Preview, w: u16, h: u16) -> Option<Side> {
    match mode {
        Preview::Off => None,
        Preview::Auto if !narrow(w) && h >= 8 => Some(Side::Right),
        Preview::Focus if narrow(w) && h >= FOCUS_ROWS + 8 => Some(Side::Focus),
        _ if h >= BELOW_MIN_H => Some(Side::Below),
        _ => None,
    }
}

/// The layouts `L` cycles through on this kind of screen, in order
fn layouts(narrow: bool) -> [(Preview, &'static str); 3] {
    if narrow {
        [
            (Preview::Auto, t!("分屏", "split")),
            (Preview::Focus, t!("预览为主", "mostly preview")),
            (Preview::Off, t!("只看列表", "list only")),
        ]
    } else {
        [
            (Preview::Auto, t!("右侧预览", "preview right")),
            (Preview::Below, t!("下方预览", "preview below")),
            (Preview::Off, t!("只看列表", "list only")),
        ]
    }
}

/// Open ones (w): everything except not running (^)
fn open(s: &Session) -> bool {
    !Pick::Off.covers(s.state)
}

/// Stopped by aca for being idle (☾, `[idle] stop_after`); not if it came back up afterwards and you stopped it again
fn reaped(s: &Session) -> bool {
    s.idle_stopped > 0 && matches!(s.state, State::Dead | State::Stopped)
}

/// The state from collection, overlaid with the read / unread the interface keeps itself (see reads.rs):
/// not unread if it has had no output since you looked; marked unread with u counts even when idle
fn read_state(s: &Session, reads: &Reads) -> State {
    if s.sub.is_some() {
        return s.state;
    }
    let key = notes::key(s);
    match s.state {
        State::Unread if reads.read_at(&key).is_some_and(|t| s.active_at <= t + SEEN_SLACK) => State::Idle,
        State::Idle if reads.unread(&key) => State::Unread,
        st => st,
    }
}

/// Waiting for you: wants your approval / choice (`!`), or finished and waiting for your next message (`●`). Errors don't count
fn waiting(s: &Session) -> bool {
    matches!(s.state, State::Ask | State::Unread)
}

fn epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// One bottom bar item: key, description, drop order when it doesn't fit (higher drops first, 0 = never)
type MenuItem<'a> = (&'a str, &'a str, u8);

/// What the preview shows when the highlight is on a group: which group, its path; when the path isn't the group's default path, which
/// session in the group (title) it follows
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    pub group: String,
    pub dir: String,
    pub from: Option<String>,
}

/// `H` pressed, tool not picked yet: whom to hand off, what to build the new session from (its directory, group); dropped when the
/// menu closes
struct Pass {
    t: Target,
    from: Box<Source>,
    draft: Draft,
    /// Its note was brought along: the note's key (removed once the new session is created)
    note: Option<String>,
    /// It's still running / waiting for your choice: the menu mentions it, since both sides would touch the same place at once
    busy: bool,
}

pub struct App {
    all: Vec<Session>,
    view: Vec<usize>,
    hidden: usize,
    /// Which one in the visible list (view) is selected; at draw time converted to a row number including section headers, kept in disp
    list: ListState,
    /// Scroll position of the list as actually drawn (with section headers)
    disp: ListState,
    /// Each drawn item: which index in view it corresponds to (header = None), which group's header it is (see `Row::head`), how many
    /// rows it takes. Used for clicks
    drawn: Vec<(Option<usize>, Option<usize>, u16)>,
    selected: Option<String>,
    scope: Scope,
    /// w: only open ones, stacked with the day count and state
    open: bool,
    /// The range before w: w first switches to 「全部」 ("All"), and w again comes back here. None if * / a was pressed inside open ones;
    /// on leaving, stay with what was picked
    back: Option<Scope>,
    /// The state picked with ! @ # & ^ %
    status: Option<Pick>,
    /// w and ^ / % don't go together (open ones has no stopped ones): the one pressed later makes the earlier pick yield; when the later
    /// one goes away, the yielded one comes back
    yielded: Option<Yielded>,
    /// Only this group (None = all groups)
    group: Option<String>,
    /// Total in the current group (the top bar's denominator)
    base_len: usize,
    /// Nothing in "recent days", so it fell back to showing everything — the top bar must honestly say 「全部」 ("All")
    fell_back: bool,
    /// Plugins in error (a hook, a scheduled run that failed last time, a gateway service that stopped): a ⚠ cell on the right of the top
    /// bar, P shows which one. Reread in the background along with the list
    pub plugin_alerts: Vec<aca_core::plugin::health::Problem>,
    filter: String,
    filtering: bool,
    digits: String,
    digits_at: Instant,
    /// The layouts for wide / narrow screens (see Preview)
    preview: Preview,
    preview_narrow: Preview,
    /// L was just pressed: the bottom bar briefly lights up "layout split → mostly preview → list only"
    layout_at: Option<Instant>,
    msg: Option<(String, Instant)>,
    /// How long msg stays in the bottom bar (usually 3 seconds; the line about how the last interface went away stays longer)
    msg_ttl: Duration,
    /// Screen size of the last frame, for `V`'s hint to say which side "auto" falls on now
    screen: Rect,
    /// What the last frame drew: the next frame checks against it to rewrite the cells ratatui missed (see `mark_torn_tails`)
    last_frame: Buffer,
    pins: Pins,
    notes: Notes,
    /// Which workspace each session is in, and the workspaces' names (see spaces.rs)
    spaces: Spaces,
    /// The workspace being viewed
    space: char,
    /// The workspace before switching to this one: ` goes back there
    last_space: Option<char>,
    /// q / Q pressed, waiting for the next key (a workspace's key)
    pending: Option<Pending>,
    /// The workspace list opened by S: index of the highlighted one
    space_menu: Option<usize>,
    /// The group list opened by Tab: highlighted row (row 0 is all groups)
    group_menu: Option<usize>,
    /// `F`'s small one-state list: highlighted row (0 = all)
    only_menu: Option<usize>,
    /// The palette list (see `open_themes`)
    theme_menu: Option<ThemeMenu>,
    /// The `^S` settings page
    settings: Option<Settings>,
    /// Entered from the settings page (theme list, changing a directory, adding an account, editor): when done, come back here on the
    /// settings page
    settings_back: Option<Settings>,
    /// Why config.toml couldn't be read when aca opened (listed under the settings page's broken entries, joined into a sentence then)
    pub sync_failed: Vec<String>,
    /// The settings page changed the input method: the run loop decides afresh whether to manage it
    pub ime_changed: bool,
    /// Plugin capabilities / views / panes to run in the background (`:` menu, r in the reading page): the run loop takes them, one
    /// thread each
    pub plugin_q: Vec<crate::caps::Req>,
    /// What was viewed before switching to the current group (None = all groups): the group list's highlight starts on it when opened,
    /// so Tab ⏎ switches back
    prev_group: Option<Option<String>>,
    /// The last highlighted session in each workspace: switching back stops there
    space_sel: HashMap<char, String>,
    /// E: the note to paste into its input box before entering the session (key, content)
    pub fill: Option<(String, String)>,
    /// When the highlight last moved, and how long that was after the move before it (see SETTLE)
    moved: Option<(Instant, Duration)>,
    /// Which session the preview is drawing now
    shown: Option<String>,
    /// ^G: the note to hand to the editor for writing
    pub edit_note: Option<NoteEdit>,
    /// Read / unread (see reads.rs): records when a session was looked at from aca; no new output since then = not ◐, and `i` skips it
    reads: Reads,
    /// Hint letters already typed in jump mode (entered with Space); None = not in jump mode
    jump: Option<String>,
    /// Sessions ticked in v multi-select (registered ids), see `on_pick_key`. Both this and the range anchor empty = not multi-selecting
    picked: Vec<String>,
    /// v range select (like yazi's visual): from this session (registered id) to the highlighted row; Esc fixes it
    anchor: Option<String>,
    /// This range is a V range unticking (like yazi's V)
    unpick: bool,
    /// The highlight is on this group's heading (jumped there), not on its first session: t / N / n act on the group.
    /// `list` still selects the group's first session then (scrolling and refresh follow it); moving the highlight goes back to sessions
    head: Option<String>,
    reader: Option<Reader>,
    trace: Option<TraceView>,
    /// While the `$` billing panel is open: the selected period, the scroll position
    bill: Option<BillView>,
    /// Billing numbers (read in the background): the latest copy and when it came; shown first when the panel reopens
    bill_data: Option<aca_records::usage::Report>,
    bill_got: Option<Instant>,
    /// Asked the background to read, not back yet; pressing r rereads at once
    bill_loading: bool,
    bill_want: bool,
    /// Which accounts to show (empty = all); still the same after closing and reopening the panel
    bill_only: Vec<String>,
    /// Whether the diff shows uncommitted changes or includes commits (`m` switches, kept across sessions)
    diff_mode: Mode,
    /// l / e pressed in the diff: the run loop gives up the terminal to run it
    pub external: Option<External>,
    /// T pressed (y in the popup): the run loop opens a window (split)
    pub split: Option<Split>,
    /// Temporary terminals opened with t, per row; kept after Esc hides them, removed only once `exit` runs inside
    pub terms: HashMap<String, crate::term::Term>,
    /// The one shown below the list that gets all keys
    pub term_on: Option<String>,
    /// t pressed but not opened yet: the run loop opens it
    pub term_req: Option<TermReq>,
    /// Diff stats at the top of the preview, by session name (read in the background, see `glance_request`)
    glances: HashMap<String, Glance>,
    /// Which session the stats were last read for, its last activity time then, and when they were read (see `glance_request`)
    glance_for: Option<(String, u64, Instant)>,
    /// Paths of the group the highlight is on (see `spot`)
    spot: Option<Spot>,
    pub clip: Option<Clip>,
    /// Picking a session whose reference goes into a pane (⏎ puts it there instead of going in): opened by `aca ui --ref`
    /// (`--insert`, the key pressed inside a session), or the same key pressed here in the list, for the highlighted session
    pub insert_to: Option<String>,
    /// Picking started here in the list: the row it goes into (`name`, title), highlighted again once done. None = opened by
    /// `aca ui --ref`, which closes once done
    pub insert_back: Option<(String, String)>,
    /// ⏎ pressed while picking: the text to put into `insert_to`, and the title of the session it refers to (the run loop pastes it)
    pub put: Option<(String, String)>,
    /// The reference key (`[keys] ref`) as the list sees it: `prefix b` is two presses, tmux's prefix key then b (no tmux in
    /// between here); empty = none. How many of them were pressed so far
    pub ref_keys: Vec<(KeyCode, KeyModifiers)>,
    ref_got: usize,
    /// Folded groups (saved in the view)
    folded: BTreeSet<String>,
    /// What this rebuild actually folded: group → (how many, how many need you); nothing folds while filtering, so it's empty
    folds: Folds,
    /// Sessions (keys) whose subagents `l` unfolded: each one it sent out gets a row (see subagent.rs). All folded by default,
    /// the session row just shows +N
    agents_open: HashSet<String>,
    /// The full-screen reader switched session / just opened, so fetch history from tmux (the run loop fetches; unit tests have no tmux)
    pub reader_load: bool,
    /// N pressed, editing the new session's path
    new_draft: Option<Draft>,
    list_area: Rect,
    preview_area: Rect,
    /// Preview scrolled back: how many lines, for which session, the most possible (computed when drawing); a new session goes to latest
    preview_up: usize,
    preview_for: String,
    preview_max: usize,
    /// The tab at the left of the top bar: click it to open the group list
    chip_area: Rect,
    pub quit: bool,
    /// Following (`--follow`, see follow.rs): wherever the highlight goes, this client switches there
    pub following: Option<Follow>,
    /// ^L: after leaving the interface, replace ourselves with the (newly installed) one and carry on, see `relaunch`
    pub relaunch: bool,
    pub attach: Option<String>,
    /// Switch to this cell (pane id) before attaching: a session itself lands in its main pane, a child row in that window / pane
    pub focus: Option<String>,
    /// ⏎ on a stopped session: the run loop starts it first, attaches once it's up. bool = it's a shell session (no waiting for a program)
    pub start: Option<Target>,
    /// ⏎ on a cell stopped with D: the run loop first restarts it in place (see park.rs), then attaches to it
    pub respawn: Option<(String, crate::park::Pane)>,
    /// When each session last had activity (seen while it was alive). After a stop tmux's activity time is gone, so this keeps
    /// counting "idle for how long", so a session isn't hidden for "over 1 day" right after D
    last_live: HashMap<String, u64>,
    pub create: Option<NewReq>,
    /// ↓ / ^F pressed while creating: the run loop opens fzf to find a directory (gives up the terminal; not in unit tests)
    pub pick: bool,
    /// Where `n`'s tool menu reads from (`with_tools`); empty by default, unit tests don't touch real files
    tools_loader: fn() -> Result<Vec<Tool>, String>,
    /// The accounts page (`with_accounts`, see acctpage.rs; true = open straight into adding an account); absent in unit tests,
    /// which don't touch the real config
    accounts_page: Option<fn(bool) -> Box<dyn Overlay>>,
    /// The tool menu opened by `n` (or `^E`), and which entry is highlighted
    tool_menu: Option<(Vec<Tool>, usize)>,
    /// The special environments' rows while `n`'s are shown, for `^E` to switch to; empty = none
    tool_more: Vec<Tool>,
    /// Showing the special environments' rows (`^E`)
    tool_special: bool,
    /// Their accounts' login state (`claude:team` → it), asked in the background each time they're shown
    tool_logins: Arc<std::sync::Mutex<HashMap<String, crate::accounts::Login>>>,
    /// `l` on a logged-in Codex account among them: its row, waiting for y (Codex clears the login as it starts logging in)
    tool_relogin: Option<usize>,
    /// Where `l`'s login hands its link over (`with_login_links`: the state directory; tests a temp one)
    login_links: std::path::PathBuf,
    /// Switched to them from `n`'s menu: that menu, to go back to with Esc
    tool_back: Option<(Vec<Tool>, usize)>,
    /// Each account's quota, read when the tool menu opens (both providers), shown after each account in the menu
    tool_quotas: Vec<aca_records::usage::Quota>,
    /// The tool menu opened by `H` is for picking where to hand off
    handoff: Option<Pass>,
    /// Where `H` recognizes the conversation to hand off (`with_handoff`); recognizes nothing by default, unit tests don't touch real files
    source_loader: fn(&Session) -> Result<Source, String>,
    /// The `X` switch-account box: each account's quota, asking whether it's logged in (`with_rehome`); nothing and no asking by default,
    /// unit tests don't touch real files
    rehome_quotas: fn(Agent) -> Vec<aca_records::usage::Quota>,
    rehome_ask: fn(Agent, &str) -> crate::accounts::Login,
    rehome_background: bool,
    /// Waiting for y to confirm: delete (`d`) / restart (`R`) / stop / send note. Once confirmed it goes into `job`,
    /// and the run loop runs it in the background
    confirm: Option<Confirm>,
    /// Being typed in the bottom bar: `r` new title / `G` new group name
    prompt: Option<Prompt>,
    /// Work to do in the background (restart / rename / move group / delete…), the run loop puts it on a background thread
    pub job: Option<Job>,
    /// What the background is doing, shown in the bottom bar the whole time; another
    /// can start only when it's done (deleting doesn't count, see `vanish`)
    busy: Option<String>,
    /// Once d is confirmed it's taken off the list first (`drop_vanished`) and deleted in the background; you can go on deleting others.
    /// Put back if the delete fails
    vanish: Vec<Vanish>,
    /// The session being restarted in the background is the highlighted one (its tmux name changes; the highlight follows when done)
    follow: bool,
    /// Moved with ⇧↑ ⇧↓ but not yet written to the registry: `Job::Order` (sessions) / `Job::GroupOrder` (groups). The list is
    /// moved first (see `apply_reorders`); when idle, they go to the background one at a time in order, each removed once written
    reorders: VecDeque<Job>,
    /// The one at the front is being written
    reorder_writing: bool,
    /// m pressed: the run loop opens fzf to pick a group (gives up the terminal; not in unit tests)
    pub pick_group: Option<GroupPick>,
    /// The number of days for "recent days" (one of DAYS)
    days: u64,
    /// Where the hints for picking a group from a path come from when creating (`with_group_hints`); none by default,
    /// unit tests don't touch the real database
    group_hints: fn() -> crate::data::GroupHints,
    /// How `A` identifies the tmux session before adopting (`with_adopt`): given `$session` / `%pane`; can't by default,
    /// unit tests don't touch real tmux or processes
    adopt_planner: fn(&str) -> Result<aca_core::adopt::Plan, String>,
    /// Where `^N`'s default directory comes from (`with_new_group_dir`); ~/workspace by default, unit tests don't touch the real config
    new_group_dir: fn() -> String,
    /// What the settings page reads and writes (`with_settings`): whether the input method switch is on, config errors, changing an item,
    /// accounts; reads and writes nothing by default, unit tests don't touch the real config
    ime_auto: fn() -> bool,
    config_bad: fn() -> Vec<String>,
    config_set: fn(&str, &str, toml::Value) -> Result<(), String>,
    accounts_lister: fn() -> Vec<crate::accounts::Account>,
    /// Feature popups (see overlay.rs), keys go to the top one; `M`'s MCP is the first
    overlays: Vec<Box<dyn Overlay>>,
    /// Rows features add to the settings page (`with_setting`); none by default, unit tests don't touch the real system
    setting_rows: Vec<Box<dyn Setting>>,
    /// Where `M` reads MCP from (`with_mcp`); empty by default, unit tests don't touch real files
    mcp_loader: fn(&Session) -> mcp::Info,
    /// The plugins page opened by `P` (`with_plugins`, see plugins.rs); absent in unit tests, which don't touch the real registry
    plugins_page: Option<fn() -> Box<dyn Overlay>>,
    /// Where the confirm box of `d` on a group reads the registry's groups from (subgroups, archived sessions) (`with_groups`);
    /// empty by default, unit tests don't touch the real database
    groups_loader: fn() -> Vec<groups::Row>,
    pub refresh: bool,
    /// What ^Z can undo, most recent last (see undo.rs)
    undo: Vec<Undo>,
    /// The trash (see trash.rs): sessions in it aren't listed
    pub trash: Trash,
    /// The background job is a ^Z undo: (the job, the undone step, what to say on success; None = its own words). Step put back on failure
    reverting: Option<(Job, Undo, Option<String>)>,
    /// The background is deleting the auto-cleanup batch (see `sweep`) and remove_after: when done the bottom bar says it was automatic
    swept: Option<(Job, u64)>,
    /// Something in the trash has expired: the run loop deletes it for real
    pub purge: bool,
}

impl App {
    pub fn new(filter: String, sessions: Vec<Session>) -> Self {
        let mut app = App {
            all: vec![],
            view: vec![],
            hidden: 0,
            list: ListState::default(),
            disp: ListState::default(),
            drawn: vec![],
            selected: None,
            scope: Scope::Recent,
            open: false,
            back: None,
            status: None,
            yielded: None,
            group: None,
            base_len: 0,
            fell_back: false,
            plugin_alerts: vec![],
            filter,
            filtering: false,
            digits: String::new(),
            digits_at: Instant::now(),
            preview: Preview::Auto,
            preview_narrow: Preview::Auto,
            layout_at: None,
            msg: None,
            msg_ttl: FLASH,
            screen: Rect::default(),
            last_frame: Buffer::default(),
            pins: Pins::default(),
            notes: Notes::default(),
            spaces: Spaces::default(),
            space: spaces::HOME,
            last_space: None,
            pending: None,
            space_menu: None,
            group_menu: None,
            only_menu: None,
            theme_menu: None,
            settings: None,
            settings_back: None,
            sync_failed: vec![],
            ime_changed: false,
            plugin_q: vec![],
            prev_group: None,
            space_sel: HashMap::new(),
            fill: None,
            edit_note: None,
            moved: None,
            shown: None,
            reads: Reads::default(),
            jump: None,
            picked: vec![],
            anchor: None,
            unpick: false,
            head: None,
            reader: None,
            trace: None,
            bill: None,
            bill_data: None,
            bill_got: None,
            bill_loading: false,
            bill_only: vec![],
            bill_want: false,
            diff_mode: Mode::Uncommitted,
            external: None,
            split: None,
            terms: HashMap::new(),
            term_on: None,
            term_req: None,
            glances: HashMap::new(),
            glance_for: None,
            spot: None,
            clip: None,
            insert_to: None,
            insert_back: None,
            put: None,
            ref_keys: vec![],
            ref_got: 0,
            folded: BTreeSet::new(),
            folds: HashMap::new(),
            agents_open: HashSet::new(),
            reader_load: false,
            new_draft: None,
            list_area: Rect::default(),
            preview_area: Rect::default(),
            preview_up: 0,
            preview_for: String::new(),
            preview_max: 0,
            chip_area: Rect::default(),
            quit: false,
            following: None,
            relaunch: false,
            attach: None,
            start: None,
            respawn: None,
            focus: None,
            last_live: HashMap::new(),
            create: None,
            pick: false,
            confirm: None,
            prompt: None,
            job: None,
            busy: None,
            vanish: vec![],
            follow: false,
            reorders: VecDeque::new(),
            reorder_writing: false,
            pick_group: None,
            days: DAYS[0],
            tools_loader: || Ok(vec![]),
            accounts_page: None,
            tool_menu: None,
            tool_more: vec![],
            tool_special: false,
            tool_logins: Arc::default(),
            tool_relogin: None,
            login_links: std::env::temp_dir().join(format!("aca-login-{}", std::process::id())),
            tool_back: None,
            tool_quotas: vec![],
            handoff: None,
            source_loader: |_| Err(t!("找不到对话记录", "can't find its conversation record").into()),
            rehome_quotas: |_| vec![],
            rehome_ask: |_, _| crate::accounts::Login::Unknown(String::new()),
            rehome_background: false,
            group_hints: crate::data::GroupHints::default,
            adopt_planner: |_| Err(t!("这里认不了", "can't recognize it here").into()),
            new_group_dir: || "~/workspace".into(),
            ime_auto: || true,
            config_bad: Vec::new,
            config_set: |_, _, _| Err(t!("这里不写配置", "no config writing here").into()),
            accounts_lister: Vec::new,
            overlays: vec![],
            setting_rows: vec![],
            mcp_loader: |_| mcp::Info::default(),
            plugins_page: None,
            groups_loader: Vec::new,
            refresh: false,
            undo: vec![],
            trash: Trash::default(),
            reverting: None,
            swept: None,
            purge: false,
        };
        app.set_sessions(sessions);
        app
    }

    /// Hook up the pins file (not hooked up = pins live only in memory, for unit tests)
    pub fn with_tools(mut self, f: fn() -> Result<Vec<Tool>, String>) -> Self {
        self.tools_loader = f;
        self
    }

    pub fn with_accounts(mut self, f: fn(bool) -> Box<dyn Overlay>) -> Self {
        self.accounts_page = Some(f);
        self
    }

    pub fn with_login_links(mut self, dir: std::path::PathBuf) -> Self {
        self.login_links = dir;
        self
    }

    pub fn with_group_hints(mut self, f: fn() -> crate::data::GroupHints) -> Self {
        self.group_hints = f;
        self
    }

    pub fn with_adopt(mut self, f: fn(&str) -> Result<aca_core::adopt::Plan, String>) -> Self {
        self.adopt_planner = f;
        self
    }

    pub fn with_new_group_dir(mut self, f: fn() -> String) -> Self {
        self.new_group_dir = f;
        self
    }

    pub fn with_settings(
        mut self,
        ime_auto: fn() -> bool,
        bad: fn() -> Vec<String>,
        set: fn(&str, &str, toml::Value) -> Result<(), String>,
        accounts: fn() -> Vec<crate::accounts::Account>,
    ) -> Self {
        (self.ime_auto, self.config_bad, self.config_set, self.accounts_lister) = (ime_auto, bad, set, accounts);
        self
    }

    /// Add a row to the settings page (after the built-in ones)
    pub fn with_setting(mut self, row: Box<dyn Setting>) -> Self {
        self.setting_rows.push(row);
        self
    }

    pub fn with_mcp(mut self, f: fn(&Session) -> mcp::Info) -> Self {
        self.mcp_loader = f;
        self
    }

    pub fn with_plugins(mut self, f: fn() -> Box<dyn Overlay>) -> Self {
        self.plugins_page = Some(f);
        self
    }

    pub fn with_groups(mut self, f: fn() -> Vec<groups::Row>) -> Self {
        self.groups_loader = f;
        self
    }

    pub fn with_handoff(mut self, f: fn(&Session) -> Result<Source, String>) -> Self {
        self.source_loader = f;
        self
    }

    /// The `X` switch-account box: where quotas are read, how login is checked, whether to check in the background
    pub fn with_rehome(
        mut self,
        quotas: fn(Agent) -> Vec<aca_records::usage::Quota>,
        ask: fn(Agent, &str) -> crate::accounts::Login,
        background: bool,
    ) -> Self {
        (self.rehome_quotas, self.rehome_ask, self.rehome_background) = (quotas, ask, background);
        self
    }

    pub fn with_pins(mut self, pins: Pins) -> Self {
        self.pins = pins;
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    /// Hook up the notes file (not hooked up = memory only)
    pub fn with_notes(mut self, notes: Notes) -> Self {
        self.notes = notes;
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    /// Show only this workspace (`aca -l` uses the one it was last on)
    pub fn in_space(mut self, c: char) -> Self {
        self.space = c;
        self.rebuild();
        self
    }

    /// `aca keyword` / `-n`: ones with this word in the title, group or tmux name; like /, only searches this workspace
    pub fn matching(&self, kw: &str) -> Vec<&Session> {
        let k = kw.to_lowercase();
        self.all
            .iter()
            .filter(|s| !s.blank && s.space == self.space && format!("{} {} {}", s.title, s.group, s.name).to_lowercase().contains(&k))
            .collect()
    }

    /// Hook up the record of deleted sessions awaiting real deletion (not hooked up = memory only)
    pub fn with_trash(mut self, trash: Trash) -> Self {
        self.trash = trash;
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    /// Hook up the read / workspace files (not hooked up = memory only)
    pub fn with_marks(mut self, reads: Reads, spaces: Spaces) -> Self {
        (self.reads, self.spaces) = (reads, spaces);
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    pub fn set_sessions(&mut self, mut v: Vec<Session>) {
        // Ones in the trash (deleted by this or another aca) aren't listed; if any have expired, delete them for real while at it
        self.trash.reload();
        if self.trash.due(epoch()) {
            self.purge = true;
        }
        let gone: HashSet<String> = v
            .iter()
            .filter(|s| s.sub.is_none() && !s.deck_id.is_empty() && self.trash.has(&s.profile, &s.deck_id))
            .map(|s| s.name.clone())
            .collect();
        v.retain(|s| !gone.contains(s.family()));
        for s in &mut v {
            if !s.state.gone() {
                self.last_live.insert(s.name.clone(), s.active_at);
            } else if let Some(&at) = self.last_live.get(&s.name).filter(|&&at| at > s.active_at) {
                s.active_at = at;
                s.idle_for = epoch().saturating_sub(at);
            }
        }
        self.all = v;
        self.lay_agents();
        self.drop_vanished();
        // A ticked session is gone (deleted, archived): no longer counts as selected
        let live = |id: &String, all: &[Session]| all.iter().any(|s| s.sub.is_none() && s.deck_id == *id);
        self.picked.retain(|id| live(id, &self.all));
        self.anchor = self.anchor.take().filter(|id| live(id, &self.all));
        self.pins.reload(); // another aca (phone / desktop) may have just changed it
        self.notes.reload();
        self.spaces.reload();
        self.reads.reload();
        for s in &mut self.all {
            s.state = read_state(s, &self.reads);
        }
        self.apply_pins();
        // The selected group has no sessions left (in this workspace) → back to all groups
        if self.group.as_ref().is_some_and(|g| !self.all.iter().any(|s| &s.group == g && s.space == self.space && !s.blank)) {
            self.group = None;
        }
        self.rebuild();
    }

    /// Unfolded sessions (`agents_open`): each subagent it sent out gets a row hung under it (where it sorts is up to `sort_sessions`,
    /// then `apply_pins`). The last round's rows are removed before adding, so just call it again after folding / unfolding.
    /// A session with no subagents listed any more (done, new turn) is no longer remembered as unfolded: the next ones start folded
    fn lay_agents(&mut self) {
        self.all.retain(|s| s.subagent.is_none());
        let all = &self.all;
        self.agents_open.retain(|n| all.iter().any(|s| s.sub.is_none() && &s.name == n && !s.subagents.is_empty()));
        let rows: Vec<Session> = self
            .all
            .iter()
            .filter(|s| s.sub.is_none() && self.agents_open.contains(&s.name))
            .flat_map(|s| s.subagents.iter().enumerate().map(move |(k, a)| agent_row(s, k, a)))
            .collect();
        self.all.extend(rows);
        // The subagent the highlight was on is no longer listed: back to its session
        if let Some(n) = self.selected.as_ref().filter(|n| n.contains(AGENT_SEP))
            && !self.all.iter().any(|s| &s.name == n)
        {
            self.selected = n.split(AGENT_SEP).next().map(String::from);
        }
    }

    /// Where the row numbered n is in the visible list (subagent rows and empty groups aren't numbered)
    fn numbered(&self, n: usize) -> Option<usize> {
        self.view
            .iter()
            .enumerate()
            .filter(|&(_, &i)| self.all[i].subagent.is_none() && !self.all[i].blank)
            .nth(n.checked_sub(1)?)
            .map(|(p, _)| p)
    }

    /// The highlight is on a subagent row: it has no pane and isn't a session, so it's view-only. ⏎ / Z show its record; moving,
    /// changing scope and opening panels work as usual (returns false, falls through to list keys); keys aimed at a session
    /// (rename, delete, note, view output…) are ignored with a message
    fn on_agent_row_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let pass = match k.code {
            KeyCode::Enter | KeyCode::Char('Z') if !shift || k.code != KeyCode::Enter => {
                self.open_trace();
                return true;
            }
            KeyCode::Up | KeyCode::Down => !shift,
            KeyCode::Esc | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End | KeyCode::Left | KeyCode::Right => true,
            KeyCode::Tab | KeyCode::BackTab => true,
            KeyCode::Char('z' | 'l' | 'r' | 's' | 'n') if ctrl => true,
            KeyCode::Char(_) if ctrl => false,
            KeyCode::Char(c) => c.is_ascii_digit() || " kjJKgGi/aw!@#&^{}hloOq[]`SvV W*?$P".contains(c),
            _ => false,
        };
        if !pass {
            self.flash(t!("子代理只能按 ⏎ 查看记录", "a subagent: only ⏎ for its record"));
        }
        !pass
    }

    /// Groups in this workspace that have sessions, in section order (same as top to bottom in the list)
    fn groups(&self) -> Vec<String> {
        let mut v: Vec<(usize, &str)> =
            self.all.iter().filter(|s| s.space == self.space && !s.blank).map(|s| (s.slot.0, s.group.as_str())).collect();
        v.sort();
        v.dedup();
        v.into_iter().map(|(_, g)| g.to_string()).collect()
    }

    /// With all groups shown, sections by group (headings inserted); not when showing a single group
    pub fn sectioned(&self) -> bool {
        self.group.is_none()
    }

    /// A popup / input box / full-screen page is open (it gets all keys; the top bar tab isn't drawn)
    fn modal_open(&self) -> bool {
        self.bill.is_some()
            || self.reader.is_some()
            || self.trace.is_some()
            || self.new_draft.is_some()
            || self.prompt.is_some()
            || self.tool_menu.is_some()
            || !self.overlays.is_empty()
            || self.space_menu.is_some()
            || self.group_menu.is_some()
            || self.only_menu.is_some()
            || self.theme_menu.is_some()
            || self.settings.is_some()
            || self.confirm.is_some()
            || self.pick_group.is_some()
    }

    /// The layout in use for this screen size
    fn mode(&self) -> Preview {
        match &self.following {
            Some(f) => f.layout,
            None if narrow(self.screen.width) => self.preview_narrow,
            None => self.preview,
        }
    }

    /// L: cycle through the layouts for this kind of screen; the bottom bar shows which for a moment
    fn cycle_preview(&mut self) {
        let n = narrow(self.screen.width);
        let order = layouts(n);
        let cur = order.iter().position(|(p, _)| *p == self.mode()).unwrap_or(0);
        let next = order[(cur + 1) % order.len()].0;
        match self.following.as_mut() {
            Some(f) => f.layout = next,
            None if n => self.preview_narrow = next,
            None => self.preview = next,
        }
        self.msg = None;
        self.layout_at = Some(Instant::now());
        if next != Preview::Off && self.screen.height > 0 && preview_side(next, self.screen.width, self.screen.height).is_none() {
            self.flash(t!("窗口高度不足,无法显示预览", "the screen is too short for a preview"));
        }
    }

    /// ?: key reference (see `help`), paged like the full-screen reader, from the top. Columns follow screen width, so laid out when drawn
    /// $: the billing panel, last 7 days by default. Numbers read in the background (the first time reads every conversation record);
    /// shows last time's until ready
    /// ^L: restart aca (switching to the newly installed program). Not while background work is still running — after a restart
    /// nobody would collect its result
    fn ask_relaunch(&mut self) {
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if !aca_core::agent::self_path().is_some_and(|p| p.exists()) {
            return self.flash(t!("找不到 aca 程序,未重启", "can't find the aca program; not restarted"));
        }
        self.relaunch = true;
        self.quit = true;
    }

    /// On restart, note what was open and pass it to the new one (`--restarted`): with the billing panel open it's bill:period,
    /// otherwise list; the accounts being shown, if any, follow as Tab-separated names
    pub fn relaunch_state(&self) -> String {
        let Some(b) = &self.bill else { return "list".into() };
        std::iter::once(format!("bill:{}", b.sel)).chain(self.bill_only.iter().cloned()).collect::<Vec<_>>().join("\t")
    }

    /// Just restarted by ^L: say when this aca was installed (shows whether the new version took), reopen the billing panel if it was open
    pub fn resume(&mut self, state: &str, installed: Option<String>) {
        let mut parts = state.split('\t');
        if let Some(sel) = parts.next().and_then(|x| x.strip_prefix("bill:")).and_then(|x| x.parse::<usize>().ok()) {
            self.bill_only = parts.map(String::from).collect();
            self.open_bill();
            if let Some(b) = self.bill.as_mut() {
                b.sel = sel.min(aca_records::usage::RANGES.len() - 1);
            }
        }
        let m = installed
            .map_or(t!("已重启", "restarted").into(), |t| tr!("已重启 · 当前 aca 安装于 {t}", "restarted · this aca was installed {t}"));
        self.flash(&m);
    }

    /// The current view (the part to save)
    pub fn view_state(&self) -> View {
        View {
            group: self.group.clone(),
            scope: self.scope,
            open: self.open,
            back: self.back,
            days: self.days,
            preview: self.preview,
            preview_narrow: self.preview_narrow,
            selected: self.selected.clone(),
            folded: self.folded.iter().cloned().collect(),
            space: self.space,
            last_space: self.last_space,
        }
    }

    /// Restore the last view. If that group has no sessions any more, back to all groups; if that session is gone, highlight the first
    pub fn restore(mut self, v: View) -> Self {
        self.group = v.group;
        self.scope = v.scope;
        self.open = v.open;
        self.back = v.back;
        self.days = v.days;
        self.preview = v.preview;
        self.preview_narrow = v.preview_narrow;
        self.selected = v.selected;
        self.folded = v.folded.into_iter().collect();
        self.space = v.space;
        self.last_space = v.last_space;
        let all = std::mem::take(&mut self.all);
        self.set_sessions(all);
        self
    }

    /// Session key → tmux session id (`$3`; going in and splitting look it up by this: the tmux session name is the title, which changes).
    /// Already a session id (^` back to the previous session) = as is; just created / started ones not yet in the list are looked up now
    pub fn tmux_of(&self, key: &str) -> String {
        if key.starts_with('$') {
            return key.to_string();
        }
        let listed = self.all.iter().find(|s| s.sub.is_none() && s.name == key).map(|s| s.tmux.clone()).filter(|t| !t.is_empty());
        listed.or_else(|| aca_core::tmux::find(key).map(|p| p.sid)).unwrap_or_default()
    }

    /// Went into / just back from this session: counts as seen
    pub fn mark_seen(&mut self, name: &str) {
        let Some(s) = self.all.iter().find(|s| s.sub.is_none() && (s.name == name || s.tmux == name)) else { return };
        let key = notes::key(s);
        let _ = self.reads.mark_read(&key, epoch(), &self.keep_marks());
    }

    /// Back from a session (name is the one gone into, left is the session id last on, see `data::go_in`): all visited count as seen.
    /// The highlight goes to the last one visited: the one switched to inside (a clicked notification, ^`), otherwise the one gone into
    /// (newly created / handed-off ones were never selected yet); already on it or its window = left alone. One not in the list yet
    /// gets the highlight when the background reread arrives
    fn came_back(&mut self, name: &str, left: &str) {
        self.mark_seen(name);
        let last = self.all.iter().find(|s| !left.is_empty() && s.sub.is_none() && s.tmux == left).map(|s| s.name.clone());
        let last = match last {
            Some(l) if l != name => {
                self.mark_seen(&l);
                l
            }
            _ => name.to_string(),
        };
        if !last.starts_with('$') && self.current().is_none_or(|s| s.family() != last) {
            self.selected = Some(last);
        }
    }

    /// What the read file keeps: sessions still listed, and deleted ones that can
    /// still be undone. Nothing read at all (collection failed) = keep all
    fn keep_marks(&self) -> impl Fn(&str) -> bool + use<> {
        let mut keys: HashSet<String> = self.all.iter().filter(|s| s.sub.is_none() && !s.blank).map(notes::key).collect();
        let none = keys.is_empty();
        keys.extend(self.trash.ids().map(String::from));
        move |k: &str| none || keys.contains(k)
    }

    /// u: mark unread as read, idle as unread (◐ until you go in)
    fn toggle_read(&mut self) {
        if self.on_group(t!("标记", "mark")) {
            return;
        }
        let Some(i) = self.list.selected().and_then(|p| self.view.get(p).copied()) else { return };
        if let Some(x) = &self.all[i].sub {
            let m = tr!("{} 是窗口,只能按 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            return self.flash(&m);
        }
        match self.mark_one(i) {
            None => self.flash(t!("只能标记空闲或未读的会话", "only idle / unread ones can be marked")),
            Some(Ok(u)) => {
                if let Undo::Read { what, title, .. } = &u {
                    self.flash(&tr!("{what}「{title}」", "marked {what} “{title}”"));
                }
                self.remember(u);
                self.rebuild(); // with @ picked, ones marked read drop off the list
            }
            Some(Err(e)) => self.flash(&tr!("保存失败:{e}", "not saved: {e}")),
        }
    }

    /// Mark one session (index i in all): unread → read, idle → unread, the
    /// list's state follows; returns the undo step. Other states = None
    fn mark_one(&mut self, i: usize) -> Option<Result<Undo, String>> {
        let s = &self.all[i];
        let (key, title, name, state) = (notes::key(s), s.title.clone(), s.name.clone(), s.state);
        let registered = s.profile == crate::data::REGISTERED;
        let keep = self.keep_marks();
        let mark = self.reads.mark(&key);
        let r = match state {
            State::Unread => self.reads.mark_read(&key, epoch(), &keep).map(|_| (State::Idle, t!("已读", "read"))),
            State::Idle => self.reads.mark_unread(&key, &keep).map(|_| (State::Unread, t!("未读", "unread"))),
            _ => return None,
        };
        Some(match r {
            Ok((st, what)) => {
                self.all[i].state = st;
                // A registered session's "seen" goes into the core's copy (same as `aca seen`): the read file's time is compared with the
                // window's last output time, so the session turns unread again as soon as the screen redraws
                let seen = (st == State::Idle && registered && self.reads.live())
                    .then(|| aca_core::runtime::update(&name, |r| r.seen = epoch()).0.seen);
                Ok(Undo::Read { key, name, title, what: what.into(), mark, state, seen })
            }
            Err(e) => Err(e.to_string()),
        })
    }

    /// W: only the highlighted session's group; if already showing a single group, back to all groups
    fn toggle_own_group(&mut self) {
        let to = match (&self.group, self.current()) {
            (Some(_), _) => None,
            (None, Some(s)) if !s.blank => Some(s.group.clone()),
            (None, _) => return,
        };
        self.set_group(to);
    }

    /// w: only open ones, again to stop; days and the picked state are kept, layered on top. Starts from all (ones open days ago
    /// are there too), press * inside to narrow. With ^ / % picked, w makes them step aside; they come back on leaving
    fn toggle_open(&mut self) {
        self.set_open(!self.open);
        match self.yielded.take() {
            Some(Yielded::Stopped(p)) if !self.open => self.status = Some(p),
            y => self.yielded = y,
        }
        if self.open && self.status.is_some_and(Pick::stopped) {
            self.yielded = self.status.take().map(Yielded::Stopped);
        }
        self.rebuild();
    }

    /// Turn the "open ones" layer on / off: on switches the scope to "all" first, off goes back to what it was before (or to what
    /// was picked if * / a was pressed inside)
    fn set_open(&mut self, on: bool) {
        if on == self.open {
            return;
        }
        self.open = on;
        if on {
            self.back = Some(self.scope);
            self.scope = Scope::All;
        } else if let Some(b) = self.back.take() {
            self.scope = b;
        }
    }

    /// a: go into "all", again goes back to recent days (days are never lost anyway). Open ones and the picked state are kept
    fn toggle_all(&mut self) {
        self.back = None;
        self.scope = if self.scope == Scope::All { Scope::Recent } else { Scope::All };
        self.rebuild();
    }

    /// ! @ # & ^ %: only this state, again cancels; scope kept. Picking ^ / % among "open ones" makes "open ones" step aside;
    /// it comes back once they're cancelled / swapped
    fn toggle_status(&mut self, p: Pick) {
        self.status = if self.status == Some(p) { None } else { Some(p) };
        let stopped = self.status.is_some_and(Pick::stopped);
        match self.yielded.take() {
            Some(Yielded::Open) if stopped => self.yielded = Some(Yielded::Open),
            Some(Yielded::Open) => self.set_open(true),
            _ => {}
        }
        if self.open && stopped {
            self.set_open(false);
            self.yielded = Some(Yielded::Open);
        }
        self.rebuild();
    }

    /// *: 1 day → 3 days → 7 days → 1 day, days only (all is a, each key does one thing); in "all" it goes back to recent days.
    /// Open ones and the picked state are kept: among open ones it narrows to the open ones of recent days. Which one is on lights
    /// up in the top bar, the bottom bar doesn't say it again
    fn cycle_days(&mut self) {
        self.back = None;
        if self.scope == Scope::All {
            self.scope = Scope::Recent;
        } else {
            let next = DAYS.iter().position(|&d| d == self.days).map_or(0, |i| (i + 1) % DAYS.len());
            self.days = DAYS[next];
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let kw = self.filter.to_lowercase();
        let window = self.days * 86400;
        // Only list the current workspace's; empty groups exist only when sectioned (they're just a heading)
        let base: Vec<usize> = (0..self.all.len())
            .filter(|&i| self.all[i].space == self.space && self.group.as_ref().is_none_or(|g| &self.all[i].group == g))
            .filter(|&i| self.sectioned() || !self.all[i].blank)
            .collect();
        // The count only counts sessions (child panes and empty groups don't count)
        let heads = |v: &[usize]| v.iter().filter(|&&i| self.all[i].sub.is_none() && !self.all[i].blank).count();
        self.base_len = heads(&base);
        self.fell_back = false;
        // A session and its windows come and go together: if one matches, the whole family stays (a window running while the
        // session itself is idle must still show). A child session that stays keeps its parent too, up to the top level (otherwise
        // you couldn't tell who sent it); not the reverse, or one move of a parent would dig up all its long-finished child sessions
        let hangs = hangs(&self.all);
        let families = |pass: &dyn Fn(&Session) -> bool| -> Vec<usize> {
            let mut keep: HashSet<&str> = base.iter().map(|&i| &self.all[i]).filter(|s| pass(s)).map(|s| s.family()).collect();
            let ups: Vec<&str> =
                base.iter().map(|&i| &self.all[i]).filter(|s| keep.contains(s.family())).flat_map(|s| s.ancestors(&hangs)).collect();
            keep.extend(ups);
            base.iter().copied().filter(|&i| keep.contains(self.all[i].family())).collect()
        };
        // Days / all, open ones, and the status picked with ! @ # & ^ % stack in three layers; search ignores all of them. While searching,
        // an empty group is listed only if its name matches; with a status picked or only open ones shown, it isn't listed
        let pick = self.status;
        let only_open = self.open;
        let st = |s: &Session| pick.is_none_or(|p| !s.blank && p.hit(s)) && (!only_open || !s.blank && open(s));
        let view = if !kw.is_empty() {
            families(&|s| format!("{} {} {}", s.title, s.group, s.name).to_lowercase().contains(&kw))
        } else {
            match self.scope {
                Scope::All if pick.is_none() && !only_open => base.clone(),
                Scope::All => families(&st),
                Scope::Recent => {
                    let keep = families(&|s| (s.blank || s.idle_for < window || s.state == State::Ask || s.attached || s.pinned) && st(s));
                    // Picking a status or only open ones says you want to see just those: none means empty, don't fall back to all
                    if heads(&keep) == 0 && pick.is_none() && !only_open {
                        self.fell_back = true;
                        base.clone()
                    } else {
                        keep
                    }
                }
            }
        };
        self.view = view;
        // How many more widening the days would show;
        // with a status picked or only open ones shown, count only those (view is a subset of it, can't go negative)
        self.hidden = if kw.is_empty() && self.scope == Scope::Recent {
            let reach = if pick.is_some() || only_open { heads(&families(&st)) } else { self.base_len };
            reach - heads(&self.view)
        } else {
            0
        };
        // Folding: each folded section keeps only its first session as a placeholder (pinned ones aren't in sections, unaffected).
        // Everything is unfolded while filtering, so nothing is hidden from search
        self.folds.clear();
        if self.sectioned() && kw.is_empty() && !self.folded.is_empty() {
            let mut kept = vec![];
            for &i in &self.view {
                let s = &self.all[i];
                if !s.pinned && self.folded.contains(&s.group) {
                    let first = !self.folds.contains_key(&s.group);
                    let e = self.folds.entry(s.group.clone()).or_default();
                    e.0 += usize::from(s.sub.is_none() && !s.blank);
                    e.1 += usize::from(s.state == State::Ask);
                    if !first {
                        continue;
                    }
                }
                kept.push(i);
            }
            self.view = kept;
        }
        // After a refresh the highlight follows the session, not the old row number
        let pos = self.selected.as_ref().and_then(|n| self.view.iter().position(|&i| &self.all[i].name == n));
        let idx = match pos {
            Some(p) => Some(p),
            None if self.view.is_empty() => None,
            None => Some(self.list.selected().unwrap_or(0).min(self.view.len() - 1)),
        };
        let head = self.head.take();
        self.select(idx);
        // On a group header: still there after a refresh; if the group got folded, land on the fold row; if it's gone, follow the session
        if let Some(p) = head.as_deref().and_then(|g| self.head_pos(g)) {
            self.select_head(p);
        }
    }

    /// Position in the visible list of the first session under group g's unfolded header; not sectioned, folded, or not listed = None
    fn head_pos(&self, g: &str) -> Option<usize> {
        if !self.sectioned() || self.folds.contains_key(g) {
            return None;
        }
        self.view.iter().position(|&i| !self.all[i].pinned && self.all[i].group == g)
    }

    /// Put the highlight on the header of the group of the p-th session in the visible list (p is the group's first)
    fn select_head(&mut self, p: usize) {
        self.select(Some(p));
        self.head = Some(self.all[self.view[p]].group.clone());
    }

    /// Which group the highlight is on: an unfolded group header, or a fold row. None = on a session
    fn group_row(&self) -> Option<&str> {
        match &self.head {
            Some(g) => Some(g),
            None => self.on_fold_row().then(|| self.current().map(|s| s.group.as_str())).flatten(),
        }
    }

    fn on_group_row(&self) -> bool {
        self.group_row().is_some()
    }

    /// Highlight on an empty group (unfolded header or fold row both count): return that group
    fn blank_group(&self) -> Option<&str> {
        self.group_row().filter(|_| self.current().is_some_and(|s| s.blank))
    }

    /// New session on an empty group that has no directory yet: go straight to fzf (same as ^N)
    fn pick_first(&self) -> bool {
        self.blank_group().is_some_and(|g| self.group_dir(g).is_none())
    }

    /// With the highlight on a group, things that only work on sessions just say so and do nothing: returns true
    fn on_group(&mut self, what: &str) -> bool {
        let m = match () {
            _ if self.head.is_some() => tr!("不能对分组{what}", "can't {what} on a group"),
            _ if self.on_fold_row() => tr!("先展开再{what}", "unfold first, then {what}"),
            _ => return false,
        };
        self.flash(&m);
        true
    }

    /// The group's path (for t / N on a group): the group's default path (if the directory still exists), else the directory of the group's
    /// session with the latest activity (worktree: the original repo; a tie goes to the one higher in the list). Also returns that session
    /// (N reuses its tool / account) and whether it's the group's default path
    fn group_dir(&self, g: &str) -> Option<(&Session, String, bool)> {
        let mine = self.all.iter().filter(|s| s.space == self.space && s.group == g && s.sub.is_none());
        let s = mine.min_by_key(|s| std::cmp::Reverse(s.active_at))?;
        let set = match s.profile.as_str() {
            "" => None,
            _ => (self.group_hints)().groups.into_iter().find(|(x, d)| x == g && std::path::Path::new(d).is_dir()).map(|(_, d)| d),
        };
        let repo = s.info.lines().find_map(|l| l.strip_prefix("Repo: ")).filter(|_| s.worktree);
        let is_set = set.is_some();
        let dir = set.or_else(|| repo.map(String::from)).unwrap_or_else(|| s.dir.clone());
        (!dir.is_empty()).then_some((s, dir, is_set))
    }

    /// The group under the highlight and its path (shown in the preview). Recomputed only when the group changes or on the timed refresh
    /// (see `glance_request`), not by reading the db every frame
    fn spot(&mut self) -> Option<Spot> {
        let g = self.group_row()?.to_string();
        if self.spot.as_ref().is_none_or(|s| s.group != g) {
            let sp = self.group_dir(&g).map(|(s, dir, set)| Spot { group: g.clone(), dir, from: (!set).then(|| s.title.clone()) });
            self.spot = sp;
        }
        self.spot.clone()
    }

    /// Sessions visible now + how many "recent days" hides (for `aca -l`)
    pub fn visible(&self) -> (Vec<&Session>, usize) {
        (self.view.iter().map(|&i| &self.all[i]).collect(), self.hidden)
    }

    fn select(&mut self, idx: Option<usize>) {
        self.list.select(idx);
        self.selected = idx.map(|i| self.all[self.view[i]].name.clone());
        // An empty group's entry draws only the header: being on it means being on the header (folded, it's still the fold row)
        let blank = idx.filter(|&p| !self.is_fold_row(p)).map(|p| &self.all[self.view[p]]).filter(|s| s.blank);
        self.head = blank.map(|s| s.group.clone());
    }

    fn current(&self) -> Option<&Session> {
        self.list.selected().and_then(|i| self.view.get(i)).map(|&i| &self.all[i])
    }

    /// The highlighted item's directory (kitty's floating window opens there, see `tell_var`): for a group, the group's path;
    /// for a session, the working directory the preview found (same as t's terminal), the registered one until found. Empty = list is empty
    fn here_dir(&mut self) -> String {
        if self.on_group_row() {
            return self.spot().map(|sp| sp.dir).unwrap_or_default();
        }
        let Some(s) = self.current() else { return String::new() };
        self.glances.get(&s.name).map(|g| g.work.dir.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| s.dir.clone())
    }

    /// Which registered session is highlighted (kitty's floating window is tied to it, see `tell_var`):
    /// empty for groups, plain tmux sessions, extra windows
    fn here_sid(&self) -> String {
        if self.on_group_row() {
            return String::new();
        }
        self.current().map(|s| s.deck_id.clone()).unwrap_or_default()
    }

    /// Where each section of the visible list starts: one per group when sectioned (the pinned block is its own), the whole list when not
    fn section_starts(&self) -> Vec<usize> {
        let key = |p: usize| {
            let s = &self.all[self.view[p]];
            match () {
                _ if !self.sectioned() => (false, ""),
                _ if s.pinned => (true, ""),
                _ => (false, s.group.as_str()),
            }
        };
        (0..self.view.len()).filter(|&p| p == 0 || key(p) != key(p - 1)).collect()
    }

    /// J: first of the next section, or the last one if there's no next section.
    /// K: first of this section, or of the previous one if already there
    fn jump_section(&mut self, d: isize) {
        if self.view.is_empty() {
            return;
        }
        let starts = self.section_starts();
        let cur = self.list.selected().unwrap_or(0);
        let to = if d > 0 {
            starts.iter().copied().find(|&p| p > cur).unwrap_or(self.view.len() - 1)
        } else {
            starts.iter().copied().rev().find(|&p| p < cur).unwrap_or(0)
        };
        self.select(Some(to));
    }

    /// Moving quickly in a row (mid-slide): don't switch the preview yet.
    /// A single move (long enough after the last one) switches it right away
    fn sliding(&self) -> bool {
        self.moved.is_some_and(|(t, gap)| gap < SETTLE && t.elapsed() < SETTLE)
    }

    /// How long until the preview switches (the run loop sets its next wakeup from this, so it switches on time once you stop)
    pub fn settle_left(&self) -> Option<Duration> {
        self.sliding().then(|| self.moved.map_or(Duration::ZERO, |(t, _)| SETTLE.saturating_sub(t.elapsed())))
    }

    /// Just moved (see HOLD)
    pub fn just_moved(&self) -> bool {
        self.moved.is_some_and(|(t, _)| t.elapsed() < HOLD)
    }

    fn move_by(&mut self, d: isize) {
        if self.view.is_empty() {
            return;
        }
        let now = Instant::now();
        let gap = self.moved.map_or(Duration::MAX, |(t, _)| now - t);
        self.moved = Some((now, gap));
        // Step one stop at a time in screen order; an unfolded group header is a stop too
        let stops = self.stops();
        let cur = stops.iter().position(|&t| Some(t) == self.at()).unwrap_or(0) as isize;
        match stops[(cur + d).clamp(0, stops.len() as isize - 1) as usize] {
            Jump::Row(p) => self.select(Some(p)),
            Jump::Head(p) => self.select_head(p),
        }
    }

    /// Where the highlight can stop, top to bottom as on screen (see `jump_targets`)
    fn stops(&self) -> Vec<Jump> {
        let view: Vec<&Session> = self.view.iter().map(|&i| &self.all[i]).collect();
        jump_targets(&view, self.sectioned(), &self.folds)
    }

    /// Where the highlight is now
    fn at(&self) -> Option<Jump> {
        let p = self.list.selected()?;
        Some(if self.head.is_some() { Jump::Head(p) } else { Jump::Row(p) })
    }

    /// Enter: go into the session; on a group, fold / unfold
    fn enter(&mut self) {
        self.digits.clear();
        if self.on_group_row() {
            return self.toggle_fold();
        }
        // A subagent row has no pane to go into: show its record
        if self.current().is_some_and(|s| s.subagent.is_some()) {
            return self.open_trace();
        }
        // Picking (`aca ui --ref`, or the reference key here): what `B` would copy goes where you were typing
        if self.insert_to.is_some() {
            self.put = self.current().map(|s| (crate::data::reference(s).unwrap_or_else(|| crate::data::copy_text(s)), s.title.clone()));
            return;
        }
        if let Some(i) = self.list.selected().and_then(|p| self.view.get(p).copied()) {
            self.go(i);
        }
    }

    /// Go into item all[i] (Enter, clicked notification)
    fn go(&mut self, i: usize) {
        let s = &self.all[i];
        // A registered session's row: if the agent exited (the pane and its screen are still there) or it's gone from tmux entirely, start
        // it again continuing the old conversation (`session::start`)
        let own = s.sub.is_none() && !s.deck_id.is_empty();
        if s.state == State::Dead && !own {
            self.respawn = Some((s.family().to_string(), self.pane_of(s)));
        } else if own && (s.state.gone() || s.state == State::Dead) {
            self.start = Some(Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() });
        } else {
            let (focus, family) = ((!s.pane.is_empty()).then(|| s.pane.clone()), s.family().to_string());
            self.focus = focus;
            self.attach = Some(family);
        }
    }

    /// c: copy the last reply; B: copy path / branch / session id (like agent-deck). The actual copying is done by the run loop
    fn ask_copy(&mut self, last: bool) {
        if self.on_group(t!("复制", "copy")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let title = s.title.clone();
        self.clip = Some(if last {
            // When unavailable (plain tmux session, no finished turn yet) use the text on screen
            let screen = preview_lines(s).iter().map(ansi::plain).collect::<Vec<_>>().join("\n");
            Clip::Last { profile: s.profile.clone(), id: s.deck_id.clone(), title, screen }
        } else {
            match crate::data::reference(s) {
                Some(text) => Clip::Ref { title, text },
                None => Clip::Info { title, text: crate::data::copy_text(s) },
            }
        });
    }

    /// The reference key pressed in the list: pick a session, its reference goes into the highlighted one's input box
    fn start_insert(&mut self) {
        if self.on_group(t!("插入引用", "insert a reference")) {
            return;
        }
        let Some(s) = self.current().filter(|s| s.subagent.is_none()) else { return };
        if s.state.gone() || s.state == State::Dead {
            let m = tr!("「{}」未运行,无法插入", "“{}” isn't running; nothing to insert into", s.title);
            return self.flash(&m);
        }
        // Its agent's pane; not known: the session's current one
        let pane = if s.pane.is_empty() { format!("{}:", s.tmux) } else { s.pane.clone() };
        self.insert_back = Some((s.name.clone(), s.title.clone()));
        self.insert_to = Some(pane);
    }

    /// Stop picking started in the list, and highlight the row it was for again
    pub fn end_insert(&mut self) {
        self.insert_to = None;
        if let Some((name, _)) = self.insert_back.take() {
            self.selected = Some(name);
            let all = std::mem::take(&mut self.all);
            self.set_sessions(all);
        }
    }

    /// C: pick one thing to copy (aca id / session id / path, see copies.rs)
    fn open_copy_menu(&mut self) {
        if self.on_group(t!("复制", "copy")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let id = if s.profile == crate::data::REGISTERED { s.deck_id.as_str() } else { "" };
        let m = crate::copies::Menu::new(&s.title, id, &crate::data::copy_text(s));
        self.open(Box::new(m));
    }

    pub fn flash(&mut self, m: &str) {
        self.flash_for(m, FLASH);
    }

    pub fn flash_for(&mut self, m: &str, ttl: Duration) {
        self.msg = Some((m.to_string(), Instant::now()));
        self.msg_ttl = ttl;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::run::{drops, idle_note, list_keys, startup_quiet};
    use crate::ui::screen::{mark_torn_tails, top_bar};

    #[test]
    fn swipes_and_arrow_runs_are_never_taken_for_auto_input() {
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        assert!(!drops(&[k(KeyCode::Down)], true, false)); // swipe right after leaving a session
        assert!(!drops(&[k(KeyCode::Up); 5], false, false)); // a network stall, a run arriving together
        assert!(drops(&[k(KeyCode::Enter)], true, false)); // a lone Enter in the quiet period is still dropped
        let typed: Vec<_> = "cd ~/x".chars().map(|c| k(KeyCode::Char(c))).collect();
        assert!(drops(&typed, false, false));
        assert!(!drops(&typed, false, true)); // typing in an input box doesn't count
        assert!(drops(&[k(KeyCode::Char('j')), k(KeyCode::Down), k(KeyCode::Down)], false, false));
    }

    #[test]
    fn popup_starts_without_the_quiet_period() {
        assert_eq!(startup_quiet(false), STARTUP_QUIET); // opened directly (the Moshi connect path): unchanged
        assert_eq!(startup_quiet(true), Duration::ZERO); // popup: ⏎ right after C-\ must not be swallowed
    }

    pub(super) fn mk(title: &str, group: &str, state: State, idle_for: u64) -> Session {
        Session {
            name: title.into(),
            tmux: format!("${title}"),
            title: title.into(),
            locked: false,
            group: group.into(),
            state,
            question: String::new(),
            idle_for,
            active_at: 1_000_000 - idle_for,
            ran: 0,
            attached: false,
            screen: vec![],
            pinned: false,
            noted: false,
            group_noted: false,
            space: spaces::HOME,
            dir: format!("/p/{group}"),
            profile: "default".into(),
            tool: "claude".into(),
            account: String::new(),
            agent: Agent::Claude,
            who: String::new(),
            within: String::new(),
            pid: 0,
            deck_id: format!("id-{title}"),
            info: format!("Path: /p/{group}"),
            worktree: false,
            // Section order: fun before work
            slot: (["fun", "work"].iter().position(|g| *g == group).unwrap_or(usize::MAX), 0, 0),
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

    pub(super) fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    pub(super) fn titles(app: &App) -> Vec<String> {
        let mut v: Vec<String> = app.visible().0.iter().map(|s| s.title.clone()).collect();
        v.sort();
        v
    }

    /// Order within a group is the order here
    pub(super) fn sessions() -> Vec<Session> {
        let v = vec![
            mk("问", "fun", State::Ask, 999_999),
            mk("未读", "fun", State::Unread, 500_000),
            mk("跑", "work", State::Running, 5),
            mk("闲", "work", State::Idle, 10),
            mk("错", "work", State::Error, 90_000),
        ];
        v.into_iter()
            .enumerate()
            .map(|(i, mut s)| {
                s.slot.1 = i as i64;
                s
            })
            .collect()
    }

    pub(super) fn fixture() -> App {
        App::new(String::new(), sessions())
    }

    /// In list order, unsorted
    pub(super) fn order(app: &App) -> Vec<String> {
        app.visible().0.iter().map(|s| s.title.clone()).collect()
    }

    pub(super) fn pin(app: &mut App, title: &str) {
        let p = app.view.iter().position(|&i| app.all[i].title == title).unwrap();
        app.select(Some(p));
        press(app, KeyCode::Char('p'));
    }

    pub(super) fn ctrl(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    pub(super) fn typ(app: &mut App, text: &str) {
        text.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    #[test]
    fn restore_brings_back_the_view_but_drops_what_is_gone() {
        let v = View {
            group: Some("work".into()),
            scope: Scope::All,
            open: true,
            back: Some(Scope::Recent),
            days: 3,
            preview: Preview::Off,
            preview_narrow: Preview::Focus,
            selected: Some("错".into()),
            folded: vec![],
            space: spaces::HOME,
            last_space: Some('2'),
        };
        let app = fixture().restore(v.clone());
        assert_eq!(app.view_state(), v);
        assert_eq!(app.current().unwrap().title, "错");
        // Group gone → all groups; session gone → highlight the first
        let gone = View { group: Some("旧分组".into()), selected: Some("没了".into()), ..v };
        let app = fixture().restore(gone);
        assert_eq!(app.group, None);
        assert_eq!(app.current().unwrap().title, "问");
    }

    #[test]
    fn d_asks_first_and_only_y_deletes() {
        let mut app = fixture();
        pin(&mut app, "跑");
        pin(&mut app, "跑"); // select 「跑」
        for cancel in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('q')] {
            press(&mut app, KeyCode::Char('d'));
            let rows = reader_rows(&mut app, 46, 12);
            // On a phone, a sheet rising from the bottom: what's asked, which session, what happens, then the y button
            let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("缺「{t}」:{rows:#?}"));
            let (head, y) = (at("删除会话？"), at("y 删除"));
            assert!(head < y && rows[head..y].iter().any(|r| r.contains("跑")), "{rows:#?}");
            press(&mut app, cancel);
            assert!(app.job.is_none() && !app.quit, "{cancel:?}");
            assert_eq!(app.msg.as_ref().unwrap().0, "未删除");
        }
        press(&mut app, KeyCode::Char('d'));
        let Some(Confirm::Remove(r)) = &app.confirm else { panic!("没问") };
        let r = r.clone();
        press(&mut app, KeyCode::Char('y'));
        // Runs in the background (removing a session waits for its processes to exit fully), the UI doesn't wait: drop it from
        // the list first, highlight moves to the next
        assert_eq!(app.job, Some(Job::Remove { t: target("跑"), pane: None }));
        let has = |app: &App, t: &str| order(app).contains(&t.to_string());
        assert!(!has(&app, "跑") && app.current().is_some_and(|s| s.title != "跑"));
        let job = app.take_job().unwrap();
        assert!(app.busy.is_none());
        assert_eq!(app.doing().as_deref(), Some("正在删除「跑」…"));
        press(&mut app, KeyCode::Char('y')); // the confirmation covers one time only
        assert!(app.job.is_none());
        // A refresh from before the removal finished still sees it: stays hidden
        app.set_sessions(sessions());
        assert!(!has(&app, "跑"));
        // Can remove others while one is still being removed; other operations wait until it's done
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none() && app.confirm.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在删除「跑」…");
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        let job2 = app.take_job().unwrap();
        assert!(!has(&app, "跑") && !has(&app, "闲"));
        assert_eq!(app.doing().as_deref(), Some("正在删除 2 个…"));
        let fresh: Vec<Session> = sessions().into_iter().filter(|s| s.title != "跑").collect();
        app.job_done(&job, Outcome::Done("已删除「跑」".into()), fresh);
        assert!(!has(&app, "跑") && !has(&app, "闲"));
        assert_eq!(app.msg.as_ref().unwrap().0, "已删除「跑」");
        assert_eq!(app.doing().as_deref(), Some("正在删除「闲」…"));
        // A stale refresh arriving after the removal doesn't bring it back either; a failed removal is put back
        app.job_done(&job2, Outcome::Failed("删除失败:不让删".into()), sessions());
        assert!(!has(&app, "跑") && has(&app, "闲") && app.doing().is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "删除失败:不让删");
        // Once hidden long enough, leave it alone (if it's really still there, it's really there)
        app.vanish[0].until = Some(Instant::now());
        app.set_sessions(sessions());
        assert!(has(&app, "跑") && app.vanish.is_empty());
        // Mention a worktree / being attached elsewhere; when narrow, truncate the title first, keep the key hints
        let r = Removal {
            title: "很长很长很长很长很长很长很长很长很长很长的标题".into(), worktree: true, attached: true, ..r
        };
        app.confirm = Some(Confirm::Remove(r));
        let rows = reader_rows(&mut app, 46, 14);
        for want in ["很长很长", "同时删除 worktree 目录", "⇄ 在别处打开，将被断开", "y 删除"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        app.confirm = None;
        // d on a fold row isn't removing a session: it asks about removing the group
        app = app.with_groups(deck_groups);
        press(&mut app, KeyCode::Char('O'));
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Group(_))));
    }

    pub(super) fn select(app: &mut App, title: &str) {
        let p = app.view.iter().position(|&i| app.all[i].title == title).unwrap();
        app.select(Some(p));
    }

    pub(super) fn target(title: &str) -> Target {
        Target { name: title.into(), title: title.into(), profile: "default".into(), id: format!("id-{title}") }
    }

    pub(super) fn hints() -> crate::data::GroupHints {
        let g = |n: &str| (n.to_string(), String::new());
        crate::data::GroupHints { groups: vec![g("work"), g("fun"), g("Zeta"), g("fun")] }
    }

    #[test]
    fn a_title_changed_before_forking_is_passed_on() {
        let mut app = fixture();
        select(&mut app, "闲");
        // F (formerly name-then-fork) is the same as f
        press(&mut app, KeyCode::Char('f'));
        shows(&reader_rows(&mut app, 46, 12), &["分叉 · 闲", "闲 (分叉)", "⏎ 分叉", "^U 清空重写"]);
        assert!(app.typing());
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none());
        shows(&reader_rows(&mut app, 46, 12), &["标题不能为空"]);
        typ(&mut app, "试试 看");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(Job::Fork { t: target("闲"), title: Some("试试 看".into()) }));
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Esc);
        assert!(app.job.is_none() && app.prompt.is_none());
        assert!(app.msg.is_none());
    }

    pub(super) fn fake_source(s: &Session) -> Result<Source, String> {
        Ok(Source {
            title: s.title.clone(),
            label: s.agent_label(),
            agent: s.agent,
            dir: s.dir.clone(),
            info: s.info.clone(),
            sid: format!("sid-{}", s.title),
            log: std::path::PathBuf::from(format!("/c/{}.jsonl", s.title)),
            pane: String::new(),
            note: None,
            w: Where::default(),
            conf: String::new(),
        })
    }

    /// A menu item: the name is the account name, the description is its config directory
    pub(super) fn tool(name: &str, tool: &str, account: &str, agent: Agent) -> Tool {
        Tool {
            name: name.into(),
            tool: tool.into(),
            account: account.into(),
            agent,
            note: format!("~/.{name}"),
            problem: None,
            within: String::new(),
            under: String::new(),
        }
    }

    pub(super) fn handoff_tools() -> Result<Vec<Tool>, String> {
        Ok(vec![
            tool("shell", "shell", "", Agent::None),
            tool("cw", "claude", "cw", Agent::Claude),
            tool("cx", "codex", "cx", Agent::Codex),
        ])
    }

    #[test]
    fn shift_h_hands_off_to_a_new_session_picked_from_the_tool_menu() {
        let mut app = fixture().with_tools(handoff_tools).with_handoff(fake_source).with_group_hints(hints);
        app.notes.set("id-跑", "顺便补测试").unwrap();
        app.set_sessions(sessions());
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        // Same menu as n, but no shell; the title says who's handed off, mentioning it if it's running or has a note
        let rows = reader_rows(&mut app, 60, 14);
        shows(&rows, &["交接「跑」给", "仍在运行 · 带上便签", "1 cw  ~/.cw", "2 cx  ~/.cx", "⏎ 交接"]);
        assert!(!rows.iter().any(|r| r.contains("shell")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        assert!(app.tool_menu.is_none() && app.handoff.is_none() && app.job.is_none());
        assert!(app.msg.is_none());
        // n again: the usual new, no handoff
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 60, 14), &["选择工具", "⏎ 选择"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.msg.is_none());

        press(&mut app, KeyCode::Char('H'));
        press(&mut app, KeyCode::Char('2'));
        assert!(app.tool_menu.is_none() && app.new_draft.is_none() && app.create.is_none());
        let Some(Job::Handoff { t, from, req, to, note }) = app.job.clone() else { panic!("{:?}", app.job) };
        assert_eq!(t, target("跑"));
        assert_eq!((from.sid.as_str(), from.note.as_deref()), ("sid-跑", Some("顺便补测试")));
        // The new session opens in its directory and group, with the tool / account picked in the menu
        assert_eq!((req.dir.as_str(), req.group.as_str(), req.spec().as_str()), ("/p/work", "work", "codex:cx"));
        assert_eq!((to, note.as_deref()), (Agent::Codex, Some("id-跑")));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在交接「跑」…"));

        // Created: highlight the new session; the note moved over, so it's deleted
        let mut fresh = sessions();
        let mut new = mk("接手", "work", State::Idle, 0);
        new.slot.1 = 99;
        fresh.push(new);
        app.job_done(&job, Outcome::Made { msg: "已交接".into(), id: "id-接手".into() }, fresh.clone());
        assert_eq!(app.current().unwrap().title, "接手");
        assert_eq!(app.msg.as_ref().unwrap().0, "已交接");
        assert_eq!(app.notes.get("id-跑"), None);

        // Not sent: the explanation stays in the new session's note. H only hands off: picking another account of the same tool
        // also opens a new one (switching accounts in place is X)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('H'));
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        let Job::Handoff { note, to, .. } = &job else { panic!() };
        assert_eq!((note, *to), (&None, Agent::Claude));
        app.job_done(&job, Outcome::Handed { msg: "没发出去(超时)".into(), id: "id-接手".into(), note: "接手「闲」…".into() }, fresh);
        assert_eq!(app.current().unwrap().title, "接手");
        assert_eq!(app.notes.get("id-接手"), Some("接手「闲」…"));
        assert!(app.current().unwrap().noted);
    }

    pub(super) fn rehome_tools() -> Result<Vec<Tool>, String> {
        Ok(vec![
            tool("shell", "shell", "", Agent::None),
            tool("ding", "claude", "ding", Agent::Claude),
            tool("cw", "claude", "cw", Agent::Claude),
            tool("cw2", "claude", "cw2", Agent::Claude),
            tool("cx", "codex", "cx", Agent::Codex),
        ])
    }

    /// All on ding
    pub(super) fn on_ding() -> Vec<Session> {
        sessions()
            .into_iter()
            .map(|mut s| {
                (s.account, s.who) = ("ding".into(), "ding".into());
                s
            })
            .collect()
    }

    #[test]
    fn idle_stopping_says_how_many_which_and_how_to_resume() {
        let one = |t: &str| aca_core::session::Idled { id: t.into(), title: t.into(), idle: 0 };
        assert_eq!(idle_note(&[one("甲"), one("乙")]), "☾ 自动停止了 2 个:「甲」「乙」 · ⏎ 重新启动");
        let many: Vec<_> = ["a", "b", "c", "d", "e"].map(one).into_iter().collect();
        assert_eq!(idle_note(&many), "☾ 自动停止了 5 个:「a」「b」「c」等 2 个 · ⏎ 重新启动");
    }

    #[test]
    fn a_session_stopped_by_its_account_says_so_and_points_to_x() {
        let fail = |error: &str, kind: &str| {
            let mut v = on_ding();
            let s = v.iter_mut().find(|s| s.title == "闲").unwrap();
            (s.state, s.error, s.error_kind) = (State::Error, error.into(), kind.into());
            v
        };
        let mut app = fixture().with_tools(rehome_tools);
        app.set_sessions(fail("You've hit your session limit · resets 2:40pm", "account"));
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 100, 30);
        // Under the list row: the original message (truncated if it doesn't fit), what X does; preview: state, original
        // message, button
        shows(&rows, &["You've hit your session li…  X 换账号", "出错 · 额度 / 登录", "✕ You've hit your session limit", "X 换账号"]);
        // Other errors (not account ones): the original message only in the preview, X not mentioned
        app.set_sessions(fail("prompt is too long", "fatal"));
        let rows = reader_rows(&mut app, 100, 30);
        shows(&rows, &["✕ prompt is too long"]);
        assert!(!rows.iter().any(|r| r.contains("X 换账号")), "{rows:#?}");
    }

    #[test]
    fn shift_h_only_hands_off_claude_or_codex_with_a_conversation() {
        let mut v = sessions();
        v[3].agent = Agent::None; // 「闲」: a shell session
        let mut app = App::new(String::new(), v).with_tools(handoff_tools);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "只能交接 Claude / Codex 会话");
        // Transcript not found (the default loader is like this)
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "找不到对话记录");
        // Only shell in the menu
        let mut app = fixture().with_handoff(fake_source).with_tools(|| Ok(vec![tool("shell", "shell", "", Agent::None)]));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none() && app.handoff.is_none());
        assert!(app.msg.as_ref().unwrap().0.starts_with("工具菜单中没有可接手的工具"), "{:?}", app.msg);
        // Busy: wait until it's done
        let mut app = fixture().with_handoff(fake_source).with_tools(handoff_tools);
        app.busy = Some("正在重启「闲」…".into());
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在重启「闲」…");
    }

    fn fake_mcp(s: &Session) -> mcp::Info {
        let row = |name: &str, kind, on| mcp::Row { name: name.into(), desc: format!("{name} 说明"), kind, on, want: on };
        let rows = if s.agent == Agent::Codex {
            vec![row("lint", mcp::Kind::Global, true)]
        } else {
            vec![row("alpha", mcp::Kind::Project, false), row("beta", mcp::Kind::Project, true), row("shared", mcp::Kind::Global, true)]
        };
        mcp::Info { home: String::new(), rows }
    }

    #[test]
    fn shift_m_toggles_project_mcps_and_applies_only_the_changes() {
        let mut app = fixture().with_mcp(fake_mcp);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('M'));
        shows(&reader_rows(&mut app, 80, 20), &["MCP · 闲", "[ ] alpha", "[x] beta", "·  shared", "全局", "空格 切换", "⏎ 应用"]);
        // Enter with nothing changed: does nothing
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none() && app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "MCP 未修改");
        press(&mut app, KeyCode::Char('M'));
        press(&mut app, KeyCode::Char(' ')); // attach alpha
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' ')); // detach beta
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' ')); // global ones can't be changed
        assert_eq!(app.msg.as_ref().unwrap().0, "此项只读");
        shows(&reader_rows(&mut app, 80, 20), &["[x] alpha", "[ ] beta"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job, Some(Job::Mcp { t: target("闲"), dir: "/p/work".into(), on: vec!["alpha".into()], off: vec!["beta".into()] }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在修改「闲」的 MCP…"));
        app.job_done(&job, Outcome::Done("已挂载 alpha · 已卸载 beta · 重启后生效".into()), sessions());
        assert!(app.busy.is_none());
        // Esc changes nothing
        press(&mut app, KeyCode::Char('M'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Esc);
        assert!(app.job.is_none() && app.overlays.is_empty());
    }

    #[test]
    fn codex_mcps_are_read_only() {
        let mut v = sessions();
        v[3].agent = Agent::Codex;
        let mut app = App::new(String::new(), v).with_mcp(fake_mcp);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('M'));
        let rows = reader_rows(&mut app, 80, 20);
        shows(&rows, &["MCP · 闲", "·  lint", "全局", "Esc 关闭"]);
        assert!(!rows.iter().any(|r| r.contains("应用")), "{rows:#?}");
        // On a phone it rises from the bottom and still fits
        shows(&reader_rows(&mut app, 44, 30), &["MCP · 闲", "·  lint"]);
    }

    #[test]
    fn restart_moves_pin_and_highlight_to_the_new_tmux_name() {
        let mut app = fixture();
        pin(&mut app, "闲");
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('R'));
        let job = app.take_job().unwrap();
        // After a rename the tmux session name changes, the registered id doesn't
        let fresh: Vec<Session> = sessions()
            .into_iter()
            .map(|mut s| {
                if s.title == "闲" {
                    s.name = "agentdeck_闲_new".into();
                }
                s
            })
            .collect();
        app.job_done(&job, Outcome::Done("已重启「闲」".into()), fresh);
        let s = app.current().unwrap();
        assert_eq!((s.name.as_str(), s.pinned), ("agentdeck_闲_new", true));
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启「闲」");
    }

    /// Another window opened in the parent session (split = a split-off pane)
    fn sub_of(parent: &Session, win: u32, split: bool, title: &str, state: State, idle_for: u64) -> Session {
        Session {
            name: format!("{}\u{1}%{win}", parent.name),
            title: title.into(),
            state,
            idle_for,
            active_at: 1_000_000 - idle_for,
            deck_id: String::new(),
            pane: format!("%{win}"),
            sub: Some(crate::data::Sub {
                parent: parent.name.clone(),
                parent_title: parent.title.clone(),
                win,
                pane: u32::from(split),
                split,
            }),
            ..parent.clone()
        }
    }

    pub(super) fn shift(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::SHIFT));
    }

    #[test]
    fn pressing_on_at_the_edge_still_says_so_when_the_write_starts_after() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "闲");
        shift(&mut app, KeyCode::Up);
        shift(&mut app, KeyCode::Up); // both moves arrive together; only now does the run loop start writing the first
        assert!(matches!(app.take_job(), Some(Job::Order { .. })));
        assert!(app.msg.is_none());
    }

    #[test]
    fn a_failed_write_drops_the_queue_and_the_list_goes_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "错");
        shift(&mut app, KeyCode::Up);
        let job = app.take_job().unwrap();
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["问", "未读", "错", "跑", "闲"]);
        app.job_done(&job, Outcome::Failed("移动失败:x".into()), sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        assert_eq!(app.msg.as_ref().unwrap().0, "移动失败:x");
        assert!(app.take_job().is_none());
    }

    #[test]
    fn a_session_takes_its_windows_and_sub_sessions_along_and_sub_sessions_stay_under_the_parent() {
        let mut v = sessions();
        let idle = v[3].clone(); // 「闲」
        v.push(sub_of(&idle, 2, false, "zsh", State::Idle, 20));
        for (i, t) in ["子一", "子二"].into_iter().enumerate() {
            let mut k = Session { up: Up::Under("闲".into(), "闲".into()), ..mk(t, "work", State::Idle, 10) };
            k.slot.1 = 10 + i as i64;
            v.push(k);
        }
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "zsh", "子一", "子二", "错"]);
        select(&mut app, "子二");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "zsh", "子二", "子一", "错"]);
        assert_eq!(app.take_job().unwrap(), Job::Order { t: target("子二"), by: "id-子一".into(), after: false });
        shift(&mut app, KeyCode::Up); // can't leave the parent session
        assert!(app.msg.is_none());
        select(&mut app, "跑"); // jumps over 「闲」's whole family
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "闲", "zsh", "子二", "子一", "跑", "错"]);
        select(&mut app, "zsh");
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "#2 是窗口,只能按 ⏎ / d / D");
    }

    #[test]
    fn pinned_and_plain_tmux_sessions_stay_put() {
        let mut v = sessions();
        v[4].deck_id.clear(); // 「错」: a plain tmux session
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "错");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能移动");
        select(&mut app, "闲"); // only plain tmux sessions below
        app.msg = None;
        shift(&mut app, KeyCode::Down);
        assert!(app.msg.is_none());
        pin(&mut app, "跑");
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "置顶会话不按分组排序");
        assert!(app.take_job().is_none());
    }

    pub(super) fn with_windows() -> App {
        let mut v = sessions();
        let idle = v.iter().find(|s| s.title == "闲").unwrap().clone();
        let err = v.iter().find(|s| s.title == "错").unwrap().clone();
        v.push(sub_of(&idle, 2, false, "zsh", State::Idle, 20));
        v.push(sub_of(&idle, 1, true, "htop", State::Idle, 30));
        // 「错」 hasn't moved for over a day, but a window it opened is running
        let mut build = sub_of(&err, 3, false, "构建", State::Running, 5);
        (build.agent, build.who) = (Agent::Codex, "cx".into()); // codex started by hand in the window
        v.push(build);
        App::new(String::new(), v)
    }

    #[test]
    fn back_from_a_session_the_highlight_is_on_the_one_you_were_last_in() {
        let mut app = with_windows();
        // Enter from 「跑」, click a notification inside to switch to 「闲」, Ctrl+Q out: lands on 「闲」
        select(&mut app, "跑");
        app.came_back("跑", "$闲");
        app.rebuild();
        assert_eq!(app.current().map(|s| s.title.as_str()), Some("闲"));
        // Didn't switch, tmux didn't say, or the one switched to isn't in the list: lands on the one entered
        for left in ["$跑", "", "$99"] {
            select(&mut app, "问");
            app.came_back("跑", left);
            app.rebuild();
            assert_eq!(app.current().map(|s| s.title.as_str()), Some("跑"), "{left:?}");
        }
        // Already on its window: no change
        select(&mut app, "zsh");
        app.came_back("跑", "$闲");
        app.rebuild();
        assert_eq!(app.current().map(|s| s.title.as_str()), Some("zsh"));
    }

    /// 「闲」 spawned two child sessions: 「子跑」 running (with another window opened in it), 「子旧」 idle for three days;
    /// parent_idle = how long 「闲」 has been idle
    pub(super) fn with_children(parent_idle: u64) -> App {
        let mut v = sessions();
        let idle = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (idle.idle_for, idle.active_at) = (parent_idle, 1_000_000 - parent_idle);
        let child = |title: &str, state: State, idle_for: u64, order: i64| Session {
            up: Up::Under("闲".into(), "闲".into()),
            slot: (1, order, 0),
            ..mk(title, "work", state, idle_for)
        };
        let run = child("子跑", State::Running, 5, -1); // ahead of everyone in group order, still follows its parent
        v.push(sub_of(&run, 2, false, "zsh", State::Idle, 20));
        v.push(run);
        v.push(child("子旧", State::Idle, 3 * 86400, 9));
        App::new(String::new(), v)
    }

    /// The list row containing want
    pub(super) fn row_of(rows: &[String], want: &str) -> String {
        rows.iter().find(|r| r.contains(want)).unwrap_or_else(|| panic!("缺「{want}」:{rows:#?}")).clone()
    }

    #[test]
    fn a_kept_child_keeps_its_parent_but_not_the_other_way_round() {
        // Parent idle for three days, child running: the parent stays too, the child hangs below it
        let mut app = with_children(3 * 86400);
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        assert!(row_of(&reader_rows(&mut app, 46, 30), "子跑").contains("└ ● 子跑"));
        // Searching a child: the parent comes along; searching the parent: the children don't
        let v: Vec<Session> = app.all.clone();
        let mut app = App::new("子跑".into(), v.clone());
        assert_eq!(order(&app), ["闲", "子跑", "zsh"]);
        assert!(row_of(&reader_rows(&mut app, 46, 30), "● 子跑").contains("└ ● 子跑")); // the search term in the top bar has 「子跑」 too
        assert_eq!(order(&App::new("闲".into(), v)), ["闲"]);
    }

    #[test]
    fn tree_branches_connect_through_the_question_line() {
        let mut v = with_children(10).all;
        let p = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (p.state, p.question) = (State::Ask, "Allow?".into());
        let mut app = App::new(String::new(), v);
        let rows = reader_rows(&mut app, 46, 30);
        // Its question sits between parent and child: the vertical line passes beside it, the question lines up with the title
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        let (p, q, c, w) = (at("◆ 闲"), at("Allow?"), at("● 子跑"), at("#2 zsh"));
        assert_eq!((q, c, w), (p + 1, p + 2, p + 3), "{rows:#?}");
        let col = |y: usize, t: &str| rows[y][..rows[y].find(t).unwrap()].width();
        assert_eq!(col(q, "│"), col(c, "└"), "{rows:#?}");
        assert_eq!(col(q, "Allow?"), col(p, "闲"), "{rows:#?}");
        assert!(rows[w].contains("  └ ○ #2 zsh"), "子跑是最后一个,它的窗口前面不再有竖线:{rows:#?}");
    }

    #[test]
    fn children_follow_a_pinned_parent_and_stand_alone_when_pinned_themselves() {
        let mut app = with_children(10);
        pin(&mut app, "闲");
        assert_eq!(order(&app)[..3], ["闲", "子跑", "zsh"]);
        // Branches connect: 子跑 has 子旧 after it, so ├; 子跑's window is prefixed with its │
        let rr = reader_rows(&mut app, 46, 30);
        for want in ["├ ● 子跑", "│ └ ○ #2 zsh", "└ ○ 子旧"] {
            assert!(row_of(&rr, want).contains(want), "{rr:#?}");
        }
        // p on a child that went up along with its parent: unpins the parent
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已取消置顶「闲」");
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        // The child pinned itself: top level in the pinned block, the parent stays in its group
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已置顶「子跑」");
        assert_eq!(order(&app), ["子跑", "zsh", "问", "跑", "闲"]);
        let r = row_of(&reader_rows(&mut app, 46, 30), "子跑");
        assert!(r.contains("● 子跑") && !r.contains('└'), "{r}");
    }

    /// [`with_children`] two levels deeper: 「子跑」 spawned 「孙」 (with another window opened in it), 「孙」 spawned 「曾孙」, all
    /// running
    pub(super) fn with_grandchildren() -> App {
        let mut v = with_children(10).all;
        let under = |title: &str, up: &str, order: i64| Session {
            up: Up::Under(up.into(), up.into()),
            slot: (1, order, 0),
            ..mk(title, "work", State::Running, 5)
        };
        let kid = under("孙", "子跑", 7);
        v.push(sub_of(&kid, 3, false, "top", State::Idle, 20));
        v.push(kid);
        v.push(under("曾孙", "孙", 0));
        App::new(String::new(), v)
    }

    #[test]
    fn a_pin_carries_every_level_below_it_and_p_down_there_undoes_the_nearest_one() {
        let mut app = with_grandchildren();
        pin(&mut app, "闲");
        assert_eq!(order(&app)[..6], ["闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        // p on 曾孙, which went up along: unpins the topmost pinned one
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已取消置顶「闲」");
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        // The middle one pinned itself: top level in the pinned block with the ones below it, the ones above stay in the group
        pin(&mut app, "孙");
        assert_eq!(order(&app), ["孙", "top", "曾孙", "问", "跑", "闲", "子跑", "zsh"]);
        let rows = reader_rows(&mut app, 46, 30);
        assert!(!row_of(&rows, "● 孙").contains('└'), "{rows:#?}");
        for want in ["├ ○ #3 top", "└ ● 曾孙", "└ ● 子跑", "  └ ○ #2 zsh"] {
            assert!(row_of(&rows, want).contains(want), "{rows:#?}");
        }
        // The one above pinned too: each follows its own, 曾孙 follows the nearest
        pin(&mut app, "闲");
        assert_eq!(order(&app)[..6], ["孙", "top", "曾孙", "闲", "子跑", "zsh"]);
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已取消置顶「孙」");
        assert_eq!(order(&app)[..6], ["闲", "子跑", "zsh", "孙", "top", "曾孙"]);
    }

    #[test]
    fn shift_arrows_move_a_sub_session_with_everything_under_it() {
        let mut app = with_grandchildren();
        press(&mut app, KeyCode::Char('a'));
        // 子跑 down: skips its own levels below, swaps with its sibling (子旧), the whole family moves together
        select(&mut app, "子跑");
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子旧", "子跑", "zsh", "孙", "top", "曾孙", "错"]);
        assert_eq!(app.take_job().unwrap(), Job::Order { t: target("子跑"), by: "id-子旧".into(), after: true });
        // No siblings: can't leave its parent's family
        select(&mut app, "孙");
        shift(&mut app, KeyCode::Down);
        assert!(app.msg.is_none());
        select(&mut app, "曾孙");
        shift(&mut app, KeyCode::Up);
        assert!(app.msg.is_none());
        // A top-level one jumps over another's whole family (all levels)
        select(&mut app, "跑");
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "闲", "子旧", "子跑", "zsh", "孙", "top", "曾孙", "跑", "错"]);
    }

    /// Plugins with errors: a ⚠ cell at the right of the top bar, counting plugins; when wide, 「插件出错(P)」 ("plugin errors (P)")
    #[test]
    fn a_coloured_wide_char_shifted_left_gets_its_right_half_rewritten() {
        let row = |spans: Vec<Span<'static>>| {
            let mut b = Buffer::empty(Rect::new(0, 0, 14, 1));
            b.set_line(0, 0, &Line::from(spans), 14);
            b
        };
        // Closing the 「 未停止 」 (" Open ") chip: the three characters move one cell left; 「止」's old right half (cell 9),
        // not written, keeps its purple background
        let on = row(vec![Span::raw("|  "), Span::styled(" 未停止 ", theme::chip())]);
        let mut off = row(vec![Span::raw("|  "), Span::raw("未停止")]);
        assert!(!on.diff(&off).iter().any(|&(x, _, _)| x == 9));
        mark_torn_tails(&on, &mut off);
        assert!(on.diff(&off).iter().any(|&(x, _, c)| x == 9 && c.bg == Color::Reset));
        // Text without a background moves one cell: the cell the terminal clears is blank anyway, nothing extra written
        let plain = row(vec![Span::raw("|   未停止 ")]);
        let mut off = row(vec![Span::raw("|  "), Span::raw("未停止")]);
        mark_torn_tails(&plain, &mut off);
        assert!(!plain.diff(&off).iter().any(|&(x, _, _)| x == 9));
    }

    #[test]
    fn broken_plugins_get_a_warning_cell_in_the_top_bar() {
        let mut app = App::new(String::new(), sessions());
        assert!(!top_bar(&app, 120).to_string().contains('⚠'));
        let p = |plugin: &str| aca_core::plugin::health::Problem {
            plugin: plugin.into(),
            what: "钩子 turn.done".into(),
            detail: "退出码 1".into(),
            at: 0,
        };
        app.plugin_alerts = vec![p("aca.bark"), p("aca.bark"), p("me.todo")];
        assert!(top_bar(&app, 160).to_string().contains("⚠ 2 插件出错(P)"), "{}", top_bar(&app, 160));
        assert!(top_bar(&app, 60).to_string().contains("⚠2"), "{}", top_bar(&app, 60));
    }

    #[test]
    fn ctrl_n_names_a_group_then_asks_for_the_path() {
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑");
        ctrl(&mut app, 'n');
        shows(&reader_rows(&mut app, 46, 12), &["新建分组", "> ▏", "⏎ 下一步"]);
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 46, 12), &["分组名不能为空"]);
        typ(&mut app, "side proj");
        press(&mut app, KeyCode::Enter);
        // Straight into fzf, no search term; after Esc the input box holds the default directory
        assert!(std::mem::take(&mut app.pick));
        let p = app.dir_pick_from(vec![("~/workspace".into(), Src::Near), ("/p/fun".into(), Src::Known)], 40, 16).unwrap();
        assert_eq!((p.query.as_str(), p.rows[0].group.clone()), ("", ("side-proj".into(), true)));
        app.picked(Ok(None));
        assert_eq!(app.new_draft.as_ref().unwrap().text, "~/workspace/");
        shows(&reader_rows(&mut app, 60, 14), &["新建会话", "side-proj  新分组 · 绑定目录", "~/workspace/"]);
        ctrl(&mut app, 'w'); // path changed, still the same group
        typ(&mut app, "elsewhere");
        let p = app.dir_pick_from(vec![], 40, 16).unwrap();
        assert_eq!(p.query, "elsewhere");
        press(&mut app, KeyCode::Enter);
        let req = app.create.take().unwrap();
        // The new group records the entered directory as its default path
        assert_eq!((req.group.as_str(), req.bind, req.by_dir), ("side-proj", true, false));
        // Enter in fzf opens it there
        let mut app = app.with_new_group_dir(|| "/p".into());
        ctrl(&mut app, 'n');
        typ(&mut app, "lab");
        press(&mut app, KeyCode::Enter);
        assert!(std::mem::take(&mut app.pick));
        assert_eq!(app.new_draft.as_ref().unwrap().base.as_deref(), Some("/p"));
        app.picked(Ok(Some(Picked::Open("/p/fun".into()))));
        let req = app.create.take().unwrap();
        assert_eq!((req.dir.as_str(), req.group.as_str(), req.bind), ("/p/fun", "lab", true));
        // Actually exists already (different case): use the existing one, its default path unchanged
        ctrl(&mut app, 'n');
        typ(&mut app, "zeta");
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 60, 14);
        shows(&rows, &["分组   Zeta"]);
        assert!(!rows.iter().any(|r| r.contains("绑定目录")), "{rows:#?}");
        press(&mut app, KeyCode::Enter);
        let req = app.create.take().unwrap();
        assert_eq!((req.group.as_str(), req.bind), ("Zeta", false));
        ctrl(&mut app, 'n');
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.new_draft.is_none());
    }

    #[test]
    fn shift_w_jumps_to_the_sessions_group_and_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // all: fun[问 未读] work[跑 闲 错]
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.group.as_deref(), Some("work"));
        assert_eq!((titles(&app), at(&app)), (vec!["跑".to_string(), "错".into(), "闲".into()], "闲".to_string()));
        press(&mut app, KeyCode::Char('j')); // switch to another in the group and back, the highlight follows
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), at(&app).as_str()), (None, "错"));
        press(&mut app, KeyCode::Char('}')); // group entered with }, W also goes back to all
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.group, None);
    }

    #[test]
    fn star_cycles_one_three_seven_days_and_a_toggles_all() {
        let mut app = fixture();
        assert_eq!(titles(&app), ["跑", "问", "闲"]);
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(titles(&app), ["跑", "错", "问", "闲"]); // 「错」 over a day
        assert_eq!(active_tab(&mut app), "3 天");
        assert!(app.msg.is_none()); // lit in the top bar, nothing extra in the bottom bar
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(titles(&app), ["未读", "跑", "错", "问", "闲"]); // 「未读」 almost six days
        assert_eq!(active_tab(&mut app), "7 天");
        // * only changes the days, wrapping back to 1 day; all is a
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((app.scope, app.days), (Scope::Recent, 1));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.scope, Scope::All);
        assert_eq!(active_tab(&mut app), "全部");
        // * in all goes back to recent days, the days kept
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((app.scope, app.days), (Scope::Recent, 1));
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.scope, app.days), (Scope::Recent, 3));
        assert_eq!(app.view_state().days, 3);
        // * among open ones shows the open ones of recent days (days unchanged)
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.scope, Scope::All);
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((app.scope, app.days, app.open), (Scope::Recent, 3, true));
    }

    /// As tools::load gives them: this Mac's, then the special environments' (unis on this Mac, work a cage), each a terminal
    /// then its accounts
    pub(super) fn sandbox_tools() -> Result<Vec<Tool>, String> {
        let la = "本机 · Los_Angeles · 代理 127.0.0.1:28888";
        let bound = |name: &str, kind: &str, agent: Agent, within: &str, under: &str| Tool {
            note: String::new(),
            within: within.into(),
            under: under.into(),
            ..tool(name, kind, name, agent)
        };
        let shell = |within: &str, under: &str| Tool {
            note: "终端".into(),
            within: within.into(),
            under: under.into(),
            ..tool(&format!("shell:{within}"), "shell", within, Agent::None)
        };
        Ok(vec![
            tool("shell", "shell", "", Agent::None),
            bound("sub", "claude", Agent::Claude, "", ""),
            shell("unis", la),
            bound("team-unis", "codex", Agent::Codex, "unis", la),
            shell("work", "ssh work"),
            bound("team", "claude", Agent::Claude, "work", "ssh work"),
            bound("team", "codex", Agent::Codex, "work", "ssh work"),
        ])
    }

    /// unis's Codex logged out, the rest logged in
    fn env_logins(a: Agent, name: &str) -> crate::accounts::Login {
        match (a, name) {
            (Agent::Codex, "team-unis") => crate::accounts::Login::Out,
            _ => crate::accounts::Login::In("me@corp".into()),
        }
    }

    #[test]
    fn l_in_ctrl_e_logs_an_account_in_within_its_environment_asking_first_for_a_logged_in_codex() {
        let dir = std::env::temp_dir().join(format!("aca-env-login-{}", std::process::id()));
        let mut app = fixture().with_tools(sandbox_tools).with_rehome(|_| vec![], env_logins, false).with_login_links(dir.clone());
        ctrl(&mut app, 'e');
        // Each account says whether it's logged in
        let rows = reader_rows(&mut app, 72, 20);
        shows(
            &rows,
            &["2 team-unis   Codex   ○ 未登录", "4 team        Claude  ● 已登录", "5 team        Codex   ● 已登录", "⏎ 选择 • l 登录"],
        );
        // A terminal has none
        press(&mut app, KeyCode::Char('l'));
        assert!(app.term_req.is_none() && app.msg.as_ref().is_some_and(|m| m.0 == "终端不用登录"));
        // Logged out: straight to its login, run for its environment's account, the link caught for the popup
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('l'));
        let r = app.term_req.take().unwrap();
        let cmd = crate::accounts::relogin_cmd(Agent::Codex, "team-unis");
        assert_eq!((r.title.as_str(), r.run.as_deref()), ("登录 Codex · team-unis", Some(cmd.as_str())));
        assert!(r.link.is_some_and(|c| c.screen && c.file.starts_with(&dir)));
        assert!(app.tool_menu.is_none());
        // A logged-in Codex: what it costs, and only y goes ahead
        ctrl(&mut app, 'e');
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('l'));
        shows(&reader_rows(&mut app, 72, 24), &["team 现在已登录", "y 重新登录"]);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.term_req.is_none() && app.tool_relogin.is_none() && app.tool_menu.is_some());
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.term_req.take().map(|r| r.title), Some("登录 Codex · team".to_string()));
        // Claude's login replaces it only once done: no asking
        ctrl(&mut app, 'e');
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.term_req.take().map(|r| r.title), Some("登录 Claude Code · team".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pinned_shows_in_recent_even_if_old_but_working_stays_a_filter() {
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」: stopped
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        pin(&mut app, "错"); // idle for 25 hours
        pin(&mut app, "闲");
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["错", "闲", "问", "跑"]);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(order(&app), ["错", "问", "未读", "跑"]); // stopped ones aren't open, pinned ones included
    }

    #[test]
    fn w_shows_every_open_one_regardless_of_age() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        v.push(mk("退", "work", State::Exited, 10));
        v.push(mk("关", "work", State::Crashed, 10));
        let mut app = App::new(String::new(), v);
        assert_eq!(titles(&app), ["停", "关", "跑", "退", "问", "闲"]); // 1 day: 未读 and 错 are over a day old, hidden; 问 always shows
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["未读", "跑", "错", "问", "闲"]); // idle ones too; not running ones (■ and closed ✕) aren't
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["停", "关", "跑", "退", "问", "闲"]);
    }

    #[test]
    fn w_starts_from_all_and_star_narrows_it_to_recent_days() {
        let mut v = sessions();
        v.push(mk("老闲", "work", State::Idle, 9 * 86_400)); // idle nine days, not in the last week either
        let mut app = App::new(String::new(), v);
        let now = |app: &App| (app.scope, app.days, app.open);
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // w within 1 day: all open ones first, ones from days ago too
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((now(&app), titles(&app)), ((Scope::All, 1, true), names(&["未读", "老闲", "跑", "错", "问", "闲"])));
        // * inside: narrows to the open ones of the last day; the older ones are mentioned in the empty-list hint
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((now(&app), titles(&app), app.hidden), ((Scope::Recent, 1, true), names(&["跑", "问", "闲"]), 3));
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((now(&app), titles(&app)), ((Scope::Recent, 3, true), names(&["跑", "错", "问", "闲"])));
        // Leaving after *: stays on the picked 3 days
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(now(&app), (Scope::Recent, 3, false));
        // No * / a pressed: leaving goes back to what it was before w
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(now(&app), (Scope::Recent, 3, false));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(now(&app), (Scope::All, 3, false));
        // a among open ones: all ↔ recent days, open stays on
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::Recent, 3, true));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::All, 3, true));
        // Off and on again still remembers where it was before w
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('w'));
        let mut app = App::new(String::new(), sessions()).restore(app.view_state());
        assert_eq!(now(&app), (Scope::All, 3, true));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(now(&app), (Scope::Recent, 3, false));
    }

    #[test]
    fn nothing_open_in_recent_days_says_how_many_are_older() {
        let mut v = sessions();
        v.retain(|s| s.title != "跑" && s.title != "闲" && s.title != "问");
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('*'));
        assert!(app.visible().0.is_empty() && !app.fell_back);
        shows(&reader_rows(&mut app, 60, 20), &["暂无未停止的会话 · 还有 2 个更早的 · * 放宽"]);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(titles(&app), ["未读", "错"]);
    }

    #[test]
    fn percent_picks_the_ones_aca_stopped_for_idling() {
        let mut v = sessions();
        let mut reaped = mk("睡", "work", State::Stopped, 4 * 3600);
        reaped.idle_stopped = 1;
        v.push(reaped);
        v.push(mk("停", "work", State::Stopped, 10));
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('%'));
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["睡".to_string()], "☾ 1 自动停止".to_string()));
        // ^ is all stopped ones, ☾ included
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["停".to_string(), "睡".to_string()], "■ 2 已停止".to_string()));
        // Like ^, none of it is open: the later key counts, the earlier pick steps aside
        press(&mut app, KeyCode::Char('%'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, app.open), (None, true));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, app.open, titles(&app)), (Some(Pick::Reaped), false, vec!["睡".to_string()]));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('%'));
        assert_eq!((app.status, app.open), (Some(Pick::Reaped), false));
        // Open, once stepped aside, keeps stepping aside while switching between ^ / %; it comes back on switching to something else
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((app.status, app.open), (Some(Pick::Off), false));
        press(&mut app, KeyCode::Char('#'));
        assert_eq!((app.status, app.open, titles(&app)), (Some(Pick::Idle), true, vec!["闲".to_string()]));
    }

    #[test]
    fn status_keys_narrow_the_range_and_toggle_off() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(titles(&app), ["跑"]);
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(titles(&app), ["跑", "问", "闲"]);
        // Needs you within the last day: awaiting confirmation always shows, unread over a day is hidden, with a last line
        // saying there's one older; the state stays when * widens
        press(&mut app, KeyCode::Char('@'));
        assert_eq!((titles(&app), app.hidden), (vec!["问".to_string()], 1));
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((titles(&app), app.hidden), (vec!["未读".to_string(), "问".to_string()], 0));
        // One at a time: another key switches over
        press(&mut app, KeyCode::Char('&'));
        assert_eq!(titles(&app), ["错"]);
        press(&mut app, KeyCode::Char('#'));
        assert_eq!(titles(&app), ["闲"]);
        // Esc clears the state first; pressing it again at the bottom doesn't exit (only popups do), quitting is q q
        press(&mut app, KeyCode::Esc);
        assert_eq!(titles(&app).len(), 5);
        assert!(!app.quit);
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit);
        assert_eq!(app.msg.as_ref().unwrap().0, "q q 退出");
        keys(&mut app, "qq");
        assert!(app.quit);
    }

    #[test]
    fn an_empty_status_stays_empty_and_counts_only_its_own_older_ones() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('&')); // 「错」 hasn't moved for over a day
        assert!(app.visible().0.is_empty() && !app.fell_back);
        assert_eq!(app.hidden, 1); // counts only the error one, not the separately hidden 「未读」
        shows(&reader_rows(&mut app, 46, 20), &["暂无出错的会话 · 还有 1 个更早的 · * 放宽"]);
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(titles(&app), ["错"]);
        press(&mut app, KeyCode::Char('^'));
        shows(&reader_rows(&mut app, 46, 20), &["暂无已停止的会话 · ^ 返回"]);
    }

    #[test]
    fn a_crashed_one_counts_as_not_running_not_as_an_error() {
        let mut v = sessions();
        v[3].state = State::Crashed; // 「闲」: the last turn ended in an error, gone from tmux too
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('&')); // only the ones still open, stuck on an error
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["错".to_string()], "✕ 1 出错".to_string()));
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["闲".to_string()], "■ 1 已停止".to_string()));
        // w with ^ picked: the state steps aside, none of it is open
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, titles(&app)), (None, ["未读", "跑", "错", "问"].map(String::from).to_vec()));
    }

    #[test]
    fn status_inside_open_narrows_it_or_leaves_it() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('!'));
        assert_eq!((titles(&app), app.open), (vec!["跑".to_string()], true));
        press(&mut app, KeyCode::Char('#')); // idle is open too
        assert_eq!((titles(&app), app.open), (vec!["闲".to_string()], true));
        // None of ^'s are open: open steps aside, back to the "all" from before; ^ canceled, open comes back
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((titles(&app), app.scope, app.open), (vec!["停".to_string()], Scope::All, false));
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((app.status, app.open), (None, true));
        // The other way round, w with ^ picked: ^ steps aside; w again leaves open, ^ comes back
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('^'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, app.open, titles(&app).len()), (None, true, 5));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, app.scope, app.open, titles(&app)), (Some(Pick::Off), Scope::All, false, vec!["停".to_string()]));
    }

    #[test]
    fn range_open_and_state_stack_and_each_key_takes_off_only_its_own() {
        let mut v = sessions();
        v.push(mk("老闲", "work", State::Idle, 9 * 86_400)); // idle nine days, not in the last day
        let mut app = App::new(String::new(), v);
        let now = |app: &App| (app.scope, app.open, app.status, titles(app));
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // # among open ones stacks idle on top (of any age), # again goes back to open
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('#'));
        assert_eq!(now(&app), (Scope::All, true, Some(Pick::Idle), names(&["老闲", "闲"])));
        press(&mut app, KeyCode::Char('#'));
        assert_eq!((app.open, app.status), (true, None));
        // ! first, then w: stacks open on top; w again takes open off, ! stays
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.open, app.status), (true, Some(Pick::Run)));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(now(&app), (Scope::Recent, false, Some(Pick::Run), names(&["跑"])));
        // Same for a
        press(&mut app, KeyCode::Char('#'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::All, false, Some(Pick::Idle), names(&["老闲", "闲"])));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::Recent, false, Some(Pick::Idle), names(&["闲"])));
        // a among open ones with @ picked: switches to recent days, again back to all; open and @ stay on throughout
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('@'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.scope, app.open, app.status), (Scope::Recent, true, Some(Pick::Wait)));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.scope, app.open, app.status), (Scope::All, true, Some(Pick::Wait)));
        // Esc cancels only the state
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.scope, app.open, app.status), (Scope::All, true, None));
    }

    #[test]
    fn search_ignores_the_status_and_it_is_not_remembered() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Char('/'));
        typ(&mut app, "闲");
        press(&mut app, KeyCode::Enter);
        assert_eq!(titles(&app), ["闲"]);
        assert_eq!(lit_counts(&mut app, 120), ""); // no fill in the top bar while searching either
        press(&mut app, KeyCode::Esc); // clears the search term first, the state stays
        assert_eq!(titles(&app), ["跑"]);
        let app = fixture().restore(app.view_state());
        assert_eq!((app.status, titles(&app)), (None, vec!["跑".to_string(), "问".to_string(), "闲".to_string()]));
    }

    #[test]
    fn status_keeps_families_and_parents_together() {
        // Session in error for over a day, another window running: the whole family stays
        let mut app = with_windows();
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(order(&app), ["跑", "错", "构建"]);
        // A child running: its parent idle five days is listed too; the parent alone doesn't bring out its children
        let mut app = with_children(5 * 86400);
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(order(&app), ["跑", "闲", "子跑", "zsh"]);
    }

    /// Text in the top bar on the selection background (the cells of the picked state)
    fn lit_counts(app: &mut App, w: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, 20)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        let (mut out, mut x) = (String::new(), 0);
        while x < w {
            let c = &buf[(x, 0)];
            if c.bg == theme::pal().sel {
                out.push_str(c.symbol());
            }
            x += c.cell_width().max(1);
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn the_picked_status_is_lit_in_the_top_bar_counts() {
        let mut app = fixture();
        assert_eq!(lit_counts(&mut app, 120), "");
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(lit_counts(&mut app, 120), "● 1 运行中");
        press(&mut app, KeyCode::Char('@'));
        assert_eq!(lit_counts(&mut app, 120), "◆ 1 等待回应 ◐ 0 未读");
        // Idle and ■ normally aren't counted; listed only when picked, even with 0
        assert!(!reader_rows(&mut app, 120, 20)[0].contains("空闲"));
        press(&mut app, KeyCode::Char('#'));
        assert_eq!(lit_counts(&mut app, 120), "○ 1 空闲");
        press(&mut app, KeyCode::Char('^'));
        assert_eq!(lit_counts(&mut app, 120), "■ 0 已停止");
        // On a phone the counts are shortened, still filled
        assert_eq!(lit_counts(&mut app, 46), "■0");
    }

    #[test]
    fn empty_working_stays_empty_instead_of_falling_back() {
        let mut app = App::new(String::new(), vec![mk("停", "work", State::Stopped, 5), mk("退", "work", State::Exited, 5)]);
        press(&mut app, KeyCode::Char('w'));
        assert!(app.visible().0.is_empty());
    }

    #[test]
    fn the_group_list_keeps_to_this_workspace_and_letters_come_from_names() {
        let mut app = fun_in_h();
        assert_eq!(app.group_rows(), [(' ', None), ('w', Some("work".into()))]); // the whole fun group is in h: not listed in 1
        keys(&mut app, "qh");
        assert_eq!(app.group_rows(), [(' ', None), ('f', Some("fun".into()))]);
        let gs = ["work", "web", "-", "ops/qa", "中文", "Wq"].map(String::from);
        assert_eq!(group_keys(&gs), ['w', 'e', 't', 'a', 's', 'd']); // clash: next; "Other tmux sessions" = t; never q; else jump letters
    }

    #[test]
    fn wheel_over_the_preview_scrolls_back_and_another_session_starts_at_the_latest() {
        let wheel = |app: &mut App, kind| {
            let p = app.preview_area;
            app.on_mouse(MouseEvent { kind, column: p.x + 2, row: p.y + 2, modifiers: KeyModifiers::NONE });
        };
        let mut app = fixture();
        let i = app.all.iter().position(|s| s.title == "跑").unwrap();
        app.all[i].screen = (1..=80).map(|n| format!("第 {n} 行")).collect();
        select(&mut app, "跑");
        let has = |rows: &[String], t: &str| rows.iter().any(|r| r.contains(t));
        let rows = reader_rows(&mut app, 120, 36);
        assert!(has(&rows, "第 80 行") && !has(&rows, "↓"), "{rows:#?}");
        // Scroll up: three rows at a time; the bottom row gives way to how many rows are below, itself included, rather than
        // covering the end of one
        wheel(&mut app, MouseEventKind::ScrollUp);
        let rows = reader_rows(&mut app, 120, 36);
        assert!(has(&rows, "第 76 行") && !has(&rows, "第 77 行") && has(&rows, "↓ 4 行"), "{rows:#?}");
        assert!(app.msg.is_none() && app.current().unwrap().title == "跑");
        // At the top of this screen: further up points to full-screen reading
        for _ in 0..40 {
            wheel(&mut app, MouseEventKind::ScrollUp);
            reader_rows(&mut app, 120, 36);
        }
        assert!(has(&reader_rows(&mut app, 120, 36), "第 1 行"));
        assert_eq!(said(&app), "z 全屏阅读");
        // Scroll back down
        let up = app.preview_up;
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!(app.preview_up, up - 3);
        // Switch session and back: starts from the latest
        select(&mut app, "闲");
        reader_rows(&mut app, 120, 36);
        select(&mut app, "跑");
        let rows = reader_rows(&mut app, 120, 36);
        assert!(has(&rows, "第 80 行") && !has(&rows, "↓"), "{rows:#?}");
    }

    #[test]
    fn wheel_moves_only_the_list_and_preview_points_to_the_reader() {
        let wheel = |app: &mut App, kind, column, row| {
            app.on_mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
        };
        for (w, h) in [(120, 36), (46, 24)] {
            let mut app = fixture();
            select(&mut app, "跑");
            reader_rows(&mut app, w, h);
            let before = app.at();
            let preview = app.preview_area;
            assert!(preview.width > 0 && preview.height > 0);
            // The preview fits the whole screen: nothing to scroll up, points to full-screen reading; down does nothing
            wheel(&mut app, MouseEventKind::ScrollUp, preview.x + 1, preview.y + 1);
            assert_eq!(app.at(), before);
            assert_eq!(said(&app), "z 全屏阅读");
            app.msg = None;
            wheel(&mut app, MouseEventKind::ScrollDown, preview.x + 1, preview.y + 1);
            assert_eq!(app.at(), before);
            assert!(app.msg.is_none());
            wheel(&mut app, MouseEventKind::ScrollDown, 0, 0);
            assert_eq!(app.at(), before);
            assert!(app.msg.is_none());
            let list = app.list_area;
            wheel(&mut app, MouseEventKind::ScrollDown, list.x, list.y);
            assert_eq!(app.current().unwrap().title, "闲");
            wheel(&mut app, MouseEventKind::ScrollUp, list.x, list.y);
            assert_eq!(app.at(), before);
            press(&mut app, KeyCode::Char('N'));
            wheel(&mut app, MouseEventKind::ScrollDown, list.x, list.y);
            assert_eq!(app.at(), before);
            assert!(app.msg.is_none());
        }
    }

    #[test]
    fn tapping_the_top_left_label_opens_the_group_list() {
        let mut app = fixture();
        reader_rows(&mut app, 80, 24);
        let (x, y, w) = (app.chip_area.x, app.chip_area.y, app.chip_area.width);
        assert!(w > 2);
        let tap = |app: &mut App, column: u16, row: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
        };
        tap(&mut app, x + 1, y);
        assert!(app.group_menu.is_some());
        tap(&mut app, x + 1, y + 5); // while open, a click anywhere closes it
        assert!(app.group_menu.is_none());
        tap(&mut app, x + w + 3, y); // outside the tab doesn't open it
        assert!(app.group_menu.is_none());
    }

    #[test]
    fn group_and_working_combine() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('}'));
        press(&mut app, KeyCode::Char('}'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["跑", "错", "闲"]); // only work's; idle is open too
    }

    #[test]
    fn rows_stay_put_when_states_and_activity_change() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        let mut v = sessions();
        v[0].state = State::Idle; // 问 answered
        v[3].state = State::Ask; // 闲 starts asking
        v[3].idle_for = 0;
        v[2].state = State::Exited; // 跑 exited
        v[4].title = "错(改了标题)".into();
        app.set_sessions(v);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错(改了标题)"]);
    }

    #[test]
    fn u_marks_read_or_unread_and_it_holds_until_new_output() {
        let v = vec![mk("未读", "fun", State::Unread, 10), mk("闲", "fun", State::Idle, 20), mk("跑", "fun", State::Running, 5)];
        let mut app = App::new(String::new(), v.clone());
        let st = |app: &App, t: &str| app.all.iter().find(|s| s.title == t).unwrap().state;
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(st(&app, "未读"), State::Idle);
        app.set_sessions(v.clone()); // refresh: collection still says it finished unseen, still counts as read
        assert_eq!(st(&app, "未读"), State::Idle);
        press(&mut app, KeyCode::Char('i')); // i doesn't enter either
        assert!(app.attach.is_none());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('u'));
        app.set_sessions(v.clone());
        assert_eq!(st(&app, "闲"), State::Unread);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        app.mark_seen("闲"); // looked at inside, so no longer unread
        app.set_sessions(v.clone());
        assert_eq!(st(&app, "闲"), State::Idle);
        let mut v2 = v.clone();
        v2[0].active_at = epoch() + 10; // ran another turn and finished
        app.set_sessions(v2);
        assert_eq!(st(&app, "未读"), State::Unread);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(st(&app, "跑"), State::Running);
    }

    pub(super) fn said(app: &App) -> &str {
        app.msg.as_ref().map_or("", |m| m.0.as_str())
    }

    #[test]
    fn ctrl_z_takes_back_pin_read_workspace_and_note_one_step_at_a_time() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有可撤销的操作");
        let st = |app: &App, t: &str| app.all.iter().find(|s| s.title == t).unwrap().state;
        let pinned = |app: &App| app.all.iter().filter(|s| s.pinned).map(|s| s.title.clone()).collect::<Vec<_>>();
        for t in ["闲", "跑", "错"] {
            pin(&mut app, t);
        }
        pin(&mut app, "跑"); // unpin the middle one
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(st(&app, "未读"), State::Idle);
        select(&mut app, "问");
        keys(&mut app, "Qw");
        assert_eq!(space(&app, "问"), 'w');
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "hi");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.notes.get("id-闲"), Some("hi"));
        // Undo backwards, one step at a time
        ctrl(&mut app, 'z');
        assert_eq!((app.notes.get("id-闲"), said(&app)), (None, "已撤销修改便签「闲」"));
        ctrl(&mut app, 'z');
        assert_eq!((space(&app, "问"), said(&app)), ('1', "已撤销 「问」 → w"));
        ctrl(&mut app, 'z');
        assert_eq!((st(&app, "未读"), said(&app)), (State::Unread, "已撤销标为已读「未读」"));
        assert_eq!(app.current().unwrap().title, "未读"); // the highlight is on the undone one
        ctrl(&mut app, 'z');
        assert_eq!(pinned(&app), ["闲", "跑", "错"]); // back in its original position
        assert_eq!(said(&app), "已撤销取消置顶「跑」");
        for _ in 0..3 {
            ctrl(&mut app, 'z');
        }
        assert!(pinned(&app).is_empty());
        assert_eq!(said(&app), "已撤销置顶「闲」");
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有可撤销的操作");
        // An unchanged note isn't recorded
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有可撤销的操作");
    }

    #[test]
    fn d_hides_the_session_until_ctrl_z_brings_it_back() {
        let mut app = fixture();
        select(&mut app, "跑");
        keys(&mut app, "Qwqw"); // recorded in w before the removal
        select(&mut app, "跑");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Remove { t: target("跑"), pane: None });
        // Still archiving (this step isn't recorded yet): don't skip past it to undo Q
        ctrl(&mut app, 'z');
        assert!(said(&app).starts_with("请稍候"), "{}", said(&app));
        app.job_done(&job, binned("跑", "work"), sessions());
        assert_eq!(said(&app), "已删除「跑」 · ^Z 撤销");
        app.set_sessions(sessions()); // still registered (archived), not listed
        assert!(!app.all.iter().any(|s| s.title == "跑"));
        // While waiting for the real deletion, its workspace record isn't cleared along the way
        keys(&mut app, "q1");
        select(&mut app, "闲");
        keys(&mut app, "Qw");
        assert_eq!(app.spaces.of("id-跑"), Some('w'));
        ctrl(&mut app, 'z'); // undo Q first
        assert_eq!(said(&app), "已撤销 「闲」 → w");
        ctrl(&mut app, 'z');
        let job = app.take_job().unwrap();
        // The entry in the Trash is restored; it was running before the removal, so it starts again
        let Job::Restore { item, start } = &job else { panic!("{job:?}") };
        assert_eq!((item.label(), start.as_slice()), ("跑", &["id-跑".to_string()][..]));
        ctrl(&mut app, 'z');
        assert!(said(&app).starts_with("请稍候"), "{}", said(&app));
        app.job_done(&job, Outcome::Done("已恢复「跑」".into()), sessions());
        assert!(!app.trash.has("default", "id-跑"));
        assert_eq!(said(&app), "已恢复「跑」");
        keys(&mut app, "qw");
        assert_eq!(app.current().unwrap().title, "跑"); // back in its original workspace
        // Undo failed: the step is put back, still in the Trash, ^Z can be pressed again
        keys(&mut app, "q1");
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, binned("闲", "work"), sessions());
        ctrl(&mut app, 'z');
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Restore { start, .. } if start == &["id-闲".to_string()]));
        app.job_done(&job, Outcome::Failed("恢复失败:x".into()), sessions());
        assert_eq!(said(&app), "恢复失败:x");
        assert!(app.trash.has("default", "id-闲") && !app.all.iter().any(|s| s.title == "闲"));
        ctrl(&mut app, 'z');
        assert_eq!(app.take_job(), Some(job.clone()));
        app.job_done(&job, Outcome::Failed("恢复失败:x".into()), sessions());
        // Restored meanwhile from the Trash page (or deleted for real once expired): ^Z says so, doesn't skip past it
        let id = app.trash.items(epoch())[0].id.clone();
        app.trash.forget(&id).unwrap();
        ctrl(&mut app, 'z');
        assert!(app.job.is_none());
        assert_eq!(said(&app), "回收站中已没有「闲」");
    }

    /// What the background removal hands back: this session went to the Trash
    fn binned(title: &str, group: &str) -> Outcome {
        let g = crate::trash::Gone { id: format!("id-{title}"), title: title.into(), group: group.into(), archived: false };
        Outcome::Binned { msg: format!("已删除「{title}」 · ^Z 撤销"), item: Box::new(crate::trash::Item::session("default", g)) }
    }

    /// Titles of the list rows ticked ✓ (80 columns, not the desktop margins)
    pub(super) fn ticked(app: &mut App) -> Vec<String> {
        let rows = reader_rows(app, 80, 20);
        let titles: Vec<String> = app.all.iter().map(|s| s.title.clone()).collect();
        rows.iter().filter(|r| r.starts_with('▌')).filter_map(|r| titles.iter().find(|t| r.contains(&format!(" {t} "))).cloned()).collect()
    }

    pub(super) fn many(act: fn(Target) -> Job, titles: &[&str]) -> Job {
        Job::Many(titles.iter().map(|t| act(target(t))).collect())
    }

    #[test]
    fn sessions_stopped_too_long_are_swept_into_the_trash_unless_something_keeps_them() {
        let (now, after, old) = (1_000_000, 10 * 86_400, 11 * 86_400);
        let mut v = sessions(); // 问: needs you, never removed however old
        for (t, st, idle) in [
            ("旧停", State::Stopped, old),
            ("旧退", State::Exited, old),
            ("旧关", State::Crashed, old),
            ("新停", State::Stopped, 86_400),
            ("旧顶", State::Stopped, old),
            ("旧签", State::Stopped, old),
            ("旧父", State::Stopped, old),
            ("旧窗", State::Exited, old),
        ] {
            v.push(mk(t, "old", st, idle));
        }
        // A spawned child still running, another window still open: their sessions are kept
        v.push(Session { up: Up::Under("旧父".into(), "旧父".into()), ..mk("子", "old", State::Running, 5) });
        let win = sub_of(v.iter().find(|s| s.title == "旧窗").unwrap(), 2, false, "zsh", State::Idle, 20);
        v.push(win);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        pin(&mut app, "旧顶");
        app.notes.set("id-旧签", "还要看").unwrap();
        app.notes.set("id-归档签", "还要看").unwrap();
        app.pins.toggle("归档顶").unwrap();
        app.apply_pins();
        // Archived ones (not listed): likewise checked for idle time, a note, pinned
        let archived = || {
            vec![(target("归档旧"), now - old), (target("归档新"), now - 100), (target("归档签"), now - old), (target("归档顶"), now - old)]
        };
        let mut got: Vec<String> = app.stale(after, now, archived()).into_iter().map(|t| t.title).collect();
        got.sort();
        assert_eq!(got, ["归档旧", "旧停", "旧关", "旧退"]);
        // Multi-selecting, a job in hand: this round does nothing
        keys(&mut app, "v");
        app.sweep(after, now, archived());
        assert!(app.job.is_none());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        // Same as removing together: hidden from the list first, removed in the background, one entry in the Trash
        app.sweep(after, now, archived());
        let job = app.take_job().unwrap();
        assert_eq!(job, many(|t| Job::Remove { t, pane: None }, &["旧停", "旧关", "旧退", "归档旧"]));
        assert!(!order(&app).iter().any(|t| ["旧停", "旧退", "旧关"].contains(&t.as_str())));
        assert_eq!(app.doing().as_deref(), Some("正在删除 4 个…"));
        let gone = |t: &str| crate::trash::Gone { id: format!("id-{t}"), title: t.into(), group: "old".into(), archived: t == "归档旧" };
        let item = crate::trash::Item::sessions("default", ["旧停", "旧关", "旧退", "归档旧"].map(gone).to_vec());
        let out = Outcome::Binned { msg: format!("已删除{} · ^Z 撤销", item.name()), item: Box::new(item) };
        app.job_done(&job, out, sessions());
        assert_eq!(said(&app), "已自动删除停止超过 10天 的「旧停」等 4 个 · ^Z 撤销");
        assert_eq!(app.trash.items(epoch()).len(), 1);
        ctrl(&mut app, 'z');
        let Some(Job::Restore { item, .. }) = app.take_job() else { panic!("{:?}", app.job) };
        assert_eq!(item.sessions.len(), 4);
        // All removed: the next round, with nothing to remove, does nothing
        let mut app = fixture();
        app.sweep(after, now, vec![]);
        assert!(app.job.is_none());
    }

    #[test]
    fn only_registered_sessions_can_be_selected_and_one_selected_acts_like_the_key_on_it() {
        let mut v = sessions();
        let mut plain = mk("裸", "-", State::Idle, 5);
        plain.deck_id = String::new();
        v.push(plain);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "裸");
        press(&mut app, KeyCode::Char('v'));
        assert!(!app.picking());
        assert_eq!(said(&app), "普通 tmux 会话 · A 接管后才能多选");
        // The range passes over it: it doesn't count
        select(&mut app, "错");
        keys(&mut app, "vj");
        press(&mut app, KeyCode::Esc);
        assert_eq!(ticked(&mut app), ["错"]);
        // Only one selected: same as pressing on it (the highlight moves there, the usual confirmation)
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Remove(r)) if r.title == "错"));
        assert!(!app.picking() && app.current().unwrap().title == "错");
    }

    #[test]
    fn shift_d_and_r_on_the_selection_ask_once_and_ctrl_z_starts_the_stopped_again() {
        let mut v = sessions();
        v.iter_mut().filter(|s| s.title == "错").for_each(|s| s.state = State::Stopped);
        let mut app = App::new(String::new(), v.clone());
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        keys(&mut app, "vjj"); // 跑 闲 错 (错 already stopped)
        press(&mut app, KeyCode::Char('D'));
        shows(&reader_rows(&mut app, 80, 24), &["停止 2 个会话？", "跑", "闲", "另有 1 个已停止", "y 停止"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert_eq!(job, many(|t| Job::Stop { t }, &["跑", "闲"]));
        assert_eq!(app.doing().as_deref(), Some("正在停止 2 个…"));
        let stopped: Vec<Session> = v
            .iter()
            .cloned()
            .map(|mut s| {
                if ["跑", "闲"].contains(&s.title.as_str()) {
                    s.state = State::Stopped;
                }
                s
            })
            .collect();
        app.job_done(&job, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), stopped);
        assert_eq!(said(&app), "已停止 2 个");
        // ^Z one step: both start again
        ctrl(&mut app, 'z');
        let undo = app.take_job().unwrap();
        assert_eq!(undo, many(|t| Job::Revive { t }, &["跑", "闲"]));
        app.job_done(&undo, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Failed("启动失败:x".into())]), v.clone());
        assert_eq!(said(&app), "已撤销停止:1 个已重新启动 · 1 个失败:启动失败:x");
        // R: none running, no question, restart them together
        select(&mut app, "闲");
        keys(&mut app, "vjR");
        assert_eq!(app.job.take(), Some(many(|t| Job::Restart { t, force: false }, &["闲", "错"])));
        // Some running: ask first
        select(&mut app, "跑");
        keys(&mut app, "vjR");
        shows(&reader_rows(&mut app, 80, 24), &["重启 2 个会话？", "1 个正在运行或等待回应，将被中断", "y 重启"]);
    }

    #[test]
    fn m_on_the_selection_moves_them_together_and_ctrl_z_puts_each_back() {
        let mut app = fixture().with_group_hints(hints);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('v'));
        select(&mut app, "跑"); // 未读 (fun) to 跑 (work)
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        assert_eq!(p.what(), "「未读」等 2 个");
        // Not in the same group: both groups listed; different directories, no directory shown
        let groups: Vec<&str> = p.rows.iter().map(|r| r.group.as_str()).collect();
        assert!(groups.contains(&"fun") && groups.contains(&"work") && p.here.is_empty(), "{groups:?}");
        app.group_picked(p, Ok(Some("work".into())));
        let all = ["Zeta", "fun", "work"].map(String::from).to_vec();
        // Ones already in work don't move
        let job = Job::Many(vec![Job::Move { t: target("未读"), from: "fun".into(), to: "work".into(), groups: all.clone() }]);
        assert_eq!(app.job.take(), Some(job));
        assert!(!app.picking());
        // Two moved: ^Z moves them back together, each to its own original group
        let moved = |v: Vec<Session>| {
            v.into_iter()
                .map(|mut s| {
                    s.group = "Zeta".into();
                    s
                })
                .collect::<Vec<_>>()
        };
        let job = Job::Many(vec![
            Job::Move { t: target("未读"), from: "fun".into(), to: "Zeta".into(), groups: all.clone() },
            Job::Move { t: target("跑"), from: "work".into(), to: "Zeta".into(), groups: all.clone() },
        ]);
        app.job_done(&job, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), moved(sessions()));
        assert_eq!(said(&app), "已将 2 个移到 Zeta");
        ctrl(&mut app, 'z');
        let back = app.take_job().unwrap();
        let back_to = |t: &str, to: &str| Job::Move { t: target(t), from: "Zeta".into(), to: to.into(), groups: all.clone() };
        assert_eq!(back, Job::Many(vec![back_to("未读", "fun"), back_to("跑", "work")]));
        app.job_done(&back, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), sessions());
        assert_eq!(said(&app), "已撤销移动分组:2 个回到原分组");
    }

    #[test]
    fn b_lists_the_trash_and_enter_brings_one_back_stopped() {
        let mut app = fixture();
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, binned("闲", "work"), sessions());
        press(&mut app, KeyCode::Char('b'));
        assert!(app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["回收站", " 闲  work", "刚刚", "⏎ 恢复"]);
        shows(&reader_rows(&mut app, 46, 30), &["回收站", " 闲  work"]);
        // ⏎: restored as stopped (not started again), the page closes, the highlight lands on it
        press(&mut app, KeyCode::Enter);
        assert!(!app.modal_open());
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Restore { item, start } if item.label() == "闲" && start.is_empty()));
        app.job_done(&job, Outcome::Done("已恢复「闲」".into()), sessions());
        assert!(!app.trash.has("default", "id-闲"));
        assert_eq!((said(&app), app.current().unwrap().title.as_str()), ("已恢复「闲」", "闲"));
        // Expired ones: deleted for real on refresh
        let old = crate::trash::Item {
            at: epoch() - crate::trash::KEEP,
            ..crate::trash::Item::session(
                "default",
                crate::trash::Gone { id: "id-旧".into(), title: "旧".into(), group: String::new(), archived: false },
            )
        };
        app.trash.add(old).unwrap();
        assert!(!app.purge);
        app.set_sessions(sessions());
        assert!(std::mem::take(&mut app.purge));
    }

    #[test]
    fn b_brings_back_or_deletes_several_together() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        for t in ["闲", "错"] {
            select(&mut app, t);
            keys(&mut app, "dy");
            let job = app.take_job().unwrap();
            app.job_done(&job, binned(t, "work"), sessions());
        }
        // In b, v ranges over two entries, ⏎ restores them together: both leave the Trash, the highlight lands on the first
        press(&mut app, KeyCode::Char('b'));
        keys(&mut app, "vj");
        press(&mut app, KeyCode::Enter);
        assert!(!app.modal_open());
        let job = app.take_job().unwrap();
        let Job::Many(jobs) = &job else { panic!("{job:?}") };
        let Some(Job::Restore { item: first, .. }) = jobs.first() else { panic!("{jobs:?}") };
        let first = first.label().to_string();
        assert_eq!(jobs.len(), 2);
        assert_eq!(app.doing().as_deref(), Some("正在恢复 2 条…"));
        app.job_done(&job, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), sessions());
        assert!(app.trash.items(epoch()).is_empty());
        assert_eq!((said(&app), app.current().unwrap().title.clone()), ("已恢复 2 条", first));
        // Deleted for real together: both leave the Trash
        for t in ["闲", "错"] {
            select(&mut app, t);
            keys(&mut app, "dy");
            let job = app.take_job().unwrap();
            app.job_done(&job, binned(t, "work"), sessions());
        }
        press(&mut app, KeyCode::Char('b'));
        keys(&mut app, "vjdy");
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), sessions());
        assert!(app.trash.items(epoch()).is_empty());
        assert_eq!(said(&app), "已永久删除 2 条");
    }

    #[test]
    fn closed_panes_cannot_come_back_and_ctrl_z_says_so_first() {
        let mut app = fixture();
        pin(&mut app, "闲");
        let job = Job::Remove { t: Target { id: String::new(), ..target("闲") }, pane: Some("%3".into()) };
        app.job_done(&job, Outcome::Done("已关闭「闲」".into()), sessions());
        assert_eq!(said(&app), "已关闭「闲」");
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "「闲」无法撤销");
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "已撤销置顶「闲」");
    }

    #[test]
    fn agent_deck_jobs_that_went_through_undo_in_reverse() {
        let mut app = fixture();
        let with = |f: &dyn Fn(&mut Session)| {
            let mut v = sessions();
            v.iter_mut().filter(|s| s.title == "闲").for_each(f);
            v
        };
        let undo = |app: &mut App, job: Job, after: Vec<Session>, back: Vec<Session>| {
            app.job_done(&job, Outcome::Done(String::new()), after);
            ctrl(app, 'z');
            let j = app.take_job().unwrap();
            app.job_done(&j, Outcome::Done("做完了".into()), back);
            j
        };
        // m: moves back to the original group
        let j = undo(
            &mut app,
            Job::Move { t: target("闲"), from: "work".into(), to: "fun".into(), groups: vec![] },
            with(&|s| s.group = "fun".into()),
            sessions(),
        );
        assert_eq!(j, Job::Move { t: target("闲"), from: "fun".into(), to: "work".into(), groups: vec![] });
        assert_eq!(said(&app), "已撤销移动分组:「闲」回到 work");
        // r: renamed back; unlocked if it wasn't locked before
        let renamed = || with(&|s| (s.title, s.locked) = ("新名".into(), true));
        let j = undo(&mut app, Job::Rename { t: target("闲"), to: "新名".into(), unlock: false }, renamed(), sessions());
        assert_eq!(j, Job::Rename { t: Target { title: "新名".into(), ..target("闲") }, to: "闲".into(), unlock: true });
        assert_eq!(said(&app), "已撤销重命名:恢复为「闲」");
        app.set_sessions(with(&|s| s.locked = true));
        let j = undo(&mut app, Job::Rename { t: target("闲"), to: "新名".into(), unlock: false }, renamed(), sessions());
        assert!(matches!(j, Job::Rename { unlock: false, .. }));
        // D: start again; not needed if it's already running again
        let stopped = || with(&|s| s.state = State::Stopped);
        let j = undo(&mut app, Job::Stop { t: target("闲") }, stopped(), sessions());
        assert_eq!(j, Job::Revive { t: target("闲") });
        assert_eq!(said(&app), "做完了");
        app.job_done(&Job::Stop { t: target("闲") }, Outcome::Done(String::new()), sessions());
        ctrl(&mut app, 'z');
        assert!(app.job.is_none());
        assert_eq!(said(&app), "「闲」已在运行");
        // M: attach / detach reversed
        let mcp = Job::Mcp { t: target("闲"), dir: "/p".into(), on: vec!["a".into()], off: vec!["b".into()] };
        let j = undo(&mut app, mcp, sessions(), sessions());
        assert_eq!(j, Job::Mcp { t: target("闲"), dir: "/p".into(), on: vec!["b".into()], off: vec!["a".into()] });
        // Failed ones and restarts aren't recorded
        app.job_done(
            &Job::Move { t: target("闲"), from: "work".into(), to: "fun".into(), groups: vec![] },
            Outcome::Failed("x".into()),
            sessions(),
        );
        app.job_done(&Job::Restart { t: target("闲"), force: false }, Outcome::Done(String::new()), sessions());
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有可撤销的操作");
        // The session is gone
        app.job_done(&Job::Stop { t: target("闲") }, Outcome::Done(String::new()), stopped());
        app.set_sessions(sessions().into_iter().filter(|s| s.title != "闲").collect());
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "「闲」已不存在");
    }

    #[test]
    fn children_of_a_deleted_parent_stay_listed() {
        let mut app = with_children(10);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        let v = app.all.clone();
        app.job_done(&job, binned("闲", "work"), v);
        let t = titles(&app);
        assert!(!t.contains(&"闲".to_string()) && t.contains(&"子跑".to_string()) && t.contains(&"子旧".to_string()), "{t:?}");
        let rows = reader_rows(&mut app, 110, 20);
        assert!(rows.iter().any(|r| r.contains("子跑")), "{rows:#?}");
    }

    #[test]
    fn a_delete_pushed_out_of_the_ten_steps_stays_in_the_trash() {
        let mut app = fixture();
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, binned("闲", "work"), sessions());
        for _ in 0..undo::KEEP {
            pin(&mut app, "跑");
        }
        assert_eq!(app.undo.len(), undo::KEEP);
        assert!(!app.undo.iter().any(|u| matches!(u, Undo::Bin { .. })));
        assert!(!app.purge && app.trash.has("default", "id-闲"));
    }

    pub(super) fn keys(app: &mut App, ks: &str) {
        for c in ks.chars() {
            let code = match c {
                '⎋' => KeyCode::Esc,
                '⏎' => KeyCode::Enter,
                '↓' => KeyCode::Down,
                '↑' => KeyCode::Up,
                '⇥' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            press(app, code);
        }
    }

    pub(super) fn life_and_work() -> Vec<Session> {
        let mut v = vec![
            mk("工作", "work", State::Idle, 10),
            mk("生活", "fun", State::Unread, 10),
            mk("生活二", "fun", State::Idle, 10),
            mk("私", "fun", State::Idle, 10),
        ];
        let life = v[1].clone();
        v.push(sub_of(&life, 2, false, "zsh", State::Idle, 10));
        // Child session: never sent anywhere on its own, so it follows its parent
        v.push(Session { up: Up::Under("生活".into(), "生活".into()), ..mk("子", "fun", State::Idle, 10) });
        v
    }

    #[test]
    fn d_hides_the_session_with_its_windows_and_leaves_its_sub_sessions_orphaned() {
        let mut app = App::new(String::new(), life_and_work());
        select(&mut app, "生活");
        keys(&mut app, "dy");
        assert!(matches!(app.take_job(), Some(Job::Remove { pane: None, .. })));
        // Windows hide along with the session; the child session stays, flush left as it is once deleted
        assert_eq!(titles(&app), ["子", "工作", "生活二", "私"]);
        assert_eq!(app.all.iter().find(|s| s.title == "子").map(|s| &s.up), Some(&Up::Gone));
        app.set_sessions(life_and_work());
        assert_eq!(titles(&app), ["子", "工作", "生活二", "私"]);
    }

    #[test]
    fn sub_sessions_follow_the_one_above_to_its_workspace_at_every_level() {
        let mut v = life_and_work();
        v.push(Session { up: Up::Under("子".into(), "子".into()), ..mk("孙", "fun", State::Idle, 10) });
        let mut app = App::new(String::new(), v.clone());
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        assert_eq!(titles(&app), ["工作", "生活二", "私"]); // The child session and its own child went along
        keys(&mut app, "ql");
        assert_eq!(titles(&app), ["zsh", "子", "孙", "生活"]);
        // The middle one was sent on its own: it stays where it is, the ones below follow it, not the one further up
        select(&mut app, "子");
        keys(&mut app, "Qh");
        assert_eq!(titles(&app), ["zsh", "生活"]);
        select(&mut app, "生活");
        keys(&mut app, "Q1");
        keys(&mut app, "q1");
        assert_eq!(titles(&app), ["zsh", "工作", "生活", "生活二", "私"]);
        keys(&mut app, "qh");
        assert_eq!(titles(&app), ["子", "孙"]);
        app.set_sessions(v); // Same after a refresh
        assert_eq!(titles(&app), ["子", "孙"]);
    }

    #[test]
    fn a_sandboxs_sessions_go_to_its_workspace_ahead_of_their_groups() {
        let boxed = Boxed(aca_core::config::parse(
            "[sandbox.work]\nssh = \"work\"\nspace = \"3\"\n[accounts.claude]\nteam = { sandbox = \"work\" }\nsub = \"~/.c\"\n",
        ));
        let firm = Session { tool: "claude".into(), account: "team".into(), ..mk("公司", "app", State::Idle, 10) };
        let mine = Session { tool: "claude".into(), account: "sub".into(), ..mk("自己", "app", State::Idle, 10) };
        let all = vec![firm.clone(), mine.clone()];
        let mut sp = Spaces::default();
        sp.set_group(&(firm.profile.clone(), "app".into()), Some('2'));
        assert_eq!((space_of(&all, &sp, &boxed, &firm, true), space_of(&all, &sp, &boxed, &mine, true)), ('3', '2'));
        // Sent elsewhere by hand: that wins
        sp.set(&notes::key(&firm), Some('5'));
        assert_eq!(space_of(&all, &sp, &boxed, &firm, true), '5');
    }

    pub(super) fn space(app: &App, title: &str) -> char {
        app.all.iter().find(|s| s.title == title && s.sub.is_none()).unwrap().space
    }

    /// The whole fun group (on its fold row) sent to h
    pub(super) fn fun_in_h() -> App {
        let mut app = App::new(String::new(), life_and_work());
        select(&mut app, "生活二");
        keys(&mut app, "oQh");
        app.folded.clear();
        app.rebuild();
        app
    }

    #[test]
    fn one_session_out_of_a_hidden_group_is_remembered_and_the_rest_stay() {
        let mut app = fun_in_h();
        keys(&mut app, "qh");
        select(&mut app, "私");
        keys(&mut app, "Q1"); // Group in h: sending one back to 1 needs its own record, or it goes back with the group again
        assert_eq!(app.spaces.of("id-私"), Some('1'));
        app.set_sessions(life_and_work());
        assert_eq!(space(&app, "私"), '1');
        // In 1, send the whole fun group (only "私" here) to 2: the group is now recorded as 2, the ones left in h stay in h
        keys(&mut app, "q1");
        select(&mut app, "私");
        keys(&mut app, "oQ2");
        assert_eq!(app.spaces.group_of("default", "fun"), Some('2'));
        assert_eq!((space(&app, "私"), space(&app, "生活"), space(&app, "生活二"), space(&app, "子")), ('2', 'h', 'h', 'h'));
        assert_eq!(app.spaces.of("id-私"), None); // Follows the group
        // Sent back to 1: the group is no longer recorded
        keys(&mut app, "q2");
        select(&mut app, "私");
        keys(&mut app, "Q1");
        assert_eq!((space(&app, "私"), space(&app, "生活"), app.spaces.group_of("default", "fun")), ('1', 'h', None));
    }

    /// Groups in the registry (fake): the two that have sessions in the list, a child group under fun holding only archived sessions
    /// (one of them deleted, in the Trash), plus two with no sessions
    pub(super) fn deck_groups() -> Vec<groups::Row> {
        let row = |path: &str, live, archived: &[&str]| groups::Row {
            profile: "default".into(),
            path: path.into(),
            live,
            archived: archived.iter().map(|s| s.to_string()).collect(),
        };
        vec![row("fun", 4, &[]), row("fun/old", 0, &["id-旧", "id-删过"]), row("work", 1, &[]), row("empty", 0, &[]), row("misc", 0, &[])]
    }

    fn delete_group(group: &str) -> Job {
        Job::Group(groups::Act::Delete { profile: "default".into(), group: group.into() })
    }

    /// The one handed back once the background has deleted fun: the group, its child groups and the sessions in them (旧 was
    /// already archived before the delete)
    pub(super) fn fun_binned() -> Outcome {
        let rec =
            |path: &str, order| crate::trash::GroupRec { path: path.into(), order, dir: String::new(), space: None, note: String::new() };
        let gone =
            |t: &str, group: &str, archived| crate::trash::Gone { id: format!("id-{t}"), title: t.into(), group: group.into(), archived };
        let item = crate::trash::Item {
            id: "x1".into(),
            at: epoch(),
            profile: "default".into(),
            group: "fun".into(),
            groups: vec![rec("fun", 1), rec("fun/old", 1)],
            sessions: vec![
                gone("生活", "fun", false),
                gone("生活二", "fun", false),
                gone("私", "fun", false),
                gone("子", "fun", false),
                gone("旧", "fun/old", true),
            ],
        };
        Outcome::Binned { msg: "已删除分组 fun 和里面 4 个会话 · ^Z 撤销".into(), item: Box::new(item) }
    }

    pub(super) fn without_fun() -> Vec<Session> {
        life_and_work().into_iter().filter(|s| s.group != "fun").collect()
    }

    fn edit_hints() -> crate::data::GroupHints {
        let g = |n: &str, d: &str| (n.to_string(), d.to_string());
        crate::data::GroupHints { groups: vec![g("work", "/w/work"), g("fun", ""), g("Other", "/")] }
    }

    fn edit_group(group: &str, to: &str, dir: Option<(&str, &str)>) -> Job {
        let dir = dir.map(|(a, b)| (a.to_string(), b.to_string()));
        Job::Group(groups::Act::Edit { profile: "default".into(), group: group.into(), to: to.into(), dir })
    }

    fn prompt_of(app: &App) -> (String, String) {
        app.prompt.as_ref().map(|p| (p.text.clone(), p.hint.clone())).unwrap()
    }

    #[test]
    fn r_on_a_group_renames_it_then_edits_its_dir_and_ctrl_z_puts_both_back() {
        let mut app = App::new(String::new(), life_and_work()).with_group_hints(edit_hints);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::GroupName(g)) if g == "work"));
        assert_eq!(prompt_of(&app).0, "work");
        // Clashes with an existing one (case-insensitively too), or moved under itself: doesn't go on
        ctrl(&mut app, 'u');
        keys(&mut app, "other⏎");
        assert_eq!(prompt_of(&app), ("other".into(), "分组 Other 已存在".into()));
        ctrl(&mut app, 'u');
        keys(&mut app, "work/x⏎");
        assert_eq!(prompt_of(&app).1, "work 不能移到自身下面");
        // Spaces become -; then on to the default directory, prefilled with the old one
        ctrl(&mut app, 'u');
        keys(&mut app, "my job⏎");
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::GroupHome { to, .. }) if to == "my-job"));
        assert_eq!(prompt_of(&app).0, "/w/work");
        // Not a directory, or another group's default directory: rejected
        ctrl(&mut app, 'u');
        keys(&mut app, "/no/such/dir⏎");
        assert_eq!(prompt_of(&app).1, "/no/such/dir 不是目录");
        ctrl(&mut app, 'u');
        keys(&mut app, "/⏎");
        assert_eq!(prompt_of(&app).1, "该目录已是分组 Other 的默认目录");
        // ⏎ straight on the old directory (fine even if it's gone): rename only
        ctrl(&mut app, 'u');
        keys(&mut app, "/w/work/⏎");
        let job = app.take_job().unwrap();
        assert_eq!(job, edit_group("work", "my-job", None));
        // Done: the group being viewed and the folded ones (child groups included) move along; one undo step recorded
        app.group = Some("work".into());
        app.folded.insert("work/sub".into());
        let moved = |g: &str| {
            let mut v = life_and_work();
            v[0].group = g.into();
            v
        };
        app.job_done(&job, Outcome::Done("分组 work 改名为 my-job".into()), moved("my-job"));
        assert_eq!((app.group.as_deref(), app.folded.contains("my-job/sub")), (Some("my-job"), true));
        assert_eq!(titles(&app), ["工作"]);
        ctrl(&mut app, 'z');
        let back = app.take_job().unwrap();
        assert_eq!(back, edit_group("my-job", "work", None));
        app.job_done(&back, Outcome::Done(String::new()), moved("work"));
        assert_eq!(app.msg.as_ref().unwrap().0, "已撤销修改分组:my-job 改回 work");
        assert_eq!((app.group.as_deref(), app.folded.contains("work/sub")), (Some("work"), true));
        // Directory only: cleared = not recorded; undo records the old one again
        app.group = None;
        jump_to_group(&mut app, "work");
        keys(&mut app, "r⏎");
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        assert_eq!(job, edit_group("work", "work", Some(("/w/work", ""))));
        app.job_done(&job, Outcome::Done(String::new()), moved("work"));
        ctrl(&mut app, 'z');
        assert_eq!(app.take_job().unwrap(), edit_group("work", "work", Some(("", "/w/work"))));
        // Nothing changed
        app.job_done(&back, Outcome::Done(String::new()), moved("work"));
        jump_to_group(&mut app, "work");
        keys(&mut app, "r⏎⏎");
        assert!(app.job.is_none());
        assert!(app.msg.is_none());
    }

    #[test]
    fn r_on_rows_that_are_not_registered_groups_says_so() {
        let mut v = life_and_work();
        v[0].group = "未分组".into();
        v.push(Session { profile: String::new(), deck_id: String::new(), ..mk("裸", "-", State::Idle, 10) });
        let mut app = App::new(String::new(), v).with_group_hints(edit_hints);
        jump_to_group(&mut app, "未分组");
        press(&mut app, KeyCode::Char('r'));
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "「未分组」不是已登记的分组:用 m 移到分组");
        select(&mut app, "裸");
        keys(&mut app, "or");
        assert_eq!(app.msg.as_ref().unwrap().0, "这些是普通 tmux 会话,没有可修改的分组");
        // r on a session still edits the session's title
        select(&mut app, "生活");
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::Rename(_))));
    }

    #[test]
    fn d_on_a_group_asks_and_y_deletes_it_with_everything_inside() {
        let mut v = life_and_work();
        v.iter_mut().filter(|s| s.title == "私").for_each(|s| s.state = State::Running);
        let mut app = App::new(String::new(), v).with_groups(deck_groups);
        let gone = crate::trash::Gone { id: "id-删过".into(), title: "删过".into(), group: "fun/old".into(), archived: false };
        app.trash.add(crate::trash::Item::session("default", gone)).unwrap();
        jump_to_group(&mut app, "fun");
        press(&mut app, KeyCode::Char('d'));
        assert!(app.job.is_none());
        // Says what goes with it: what's in the Trash doesn't count; running sessions are noted and started again after a ^Z undo
        let Some(Confirm::Group(g)) = &app.confirm else { panic!("没问") };
        assert_eq!((g.sessions, g.archived, g.busy, g.kids), (5, 1, 1, 1));
        assert_eq!(g.running, ["id-生活", "id-子", "id-生活二", "id-私"]);
        shows(
            &reader_rows(&mut app, 100, 30),
            &["删除分组？", "▾ fun", "同时删除其中 5 个会话（1 个运行中，1 个已归档）", "同时删除 1 个子分组", "y 删除"],
        );
        shows(&reader_rows(&mut app, 46, 30), &["删除分组？", "▾ fun"]);
        // Any other key: not deleted, and says so; Esc says nothing
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((app.job.is_none(), said(&app)), (true, "未删除"));
        app.msg = None;
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Esc);
        assert!(app.confirm.is_none() && app.job.is_none() && app.msg.is_none() && !app.quit);
        // y: the group's sessions leave the list at once, without waiting for the background delete to finish
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        assert_eq!(job, delete_group("fun"));
        assert_eq!(titles(&app), ["工作"]);
        shows(&reader_rows(&mut app, 46, 30), &["正在删除分组 fun…"]);
        app.job_done(&job, fun_binned(), without_fun());
        assert_eq!(said(&app), "已删除分组 fun 和里面 4 个会话 · ^Z 撤销");
        assert!(app.trash.has("default", "id-生活") && app.trash.has("default", "id-旧"));
        // ^Z: the whole group comes back, the ones running before the delete start again; highlight on the group's header
        ctrl(&mut app, 'z');
        let job = app.take_job().unwrap();
        let Job::Restore { item, start } = &job else { panic!("{job:?}") };
        assert_eq!((item.id.as_str(), start.len()), ("x1", 4));
        app.job_done(&job, Outcome::Done("已恢复分组 fun · 4 个会话".into()), life_and_work());
        assert_eq!(said(&app), "已恢复分组 fun · 4 个会话");
        assert!(!app.trash.has("default", "id-生活"));
        assert!(titles(&app).contains(&"生活".to_string()));
        assert_eq!(app.head.as_deref(), Some("fun"));
    }

    #[test]
    fn d_on_rows_that_are_not_registered_groups_says_so() {
        let mut v = life_and_work();
        v[0].group = "未分组".into();
        v.push(Session { profile: String::new(), deck_id: String::new(), ..mk("裸", "-", State::Idle, 10) });
        let mut app = App::new(String::new(), v).with_groups(deck_groups);
        jump_to_group(&mut app, "未分组");
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "「未分组」不是已登记的分组");
        // Same on a fold row; the plain tmux sessions section has no group to delete
        select(&mut app, "裸");
        keys(&mut app, "od");
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "这些是普通 tmux 会话,没有可删除的分组");
        select(&mut app, "生活");
        keys(&mut app, "od");
        assert!(matches!(&app.confirm, Some(Confirm::Group(g)) if g.group == "fun"));
    }

    #[test]
    fn a_deleted_group_kept_in_h_goes_back_to_h() {
        let mut app = fun_in_h().with_groups(deck_groups);
        keys(&mut app, "qh");
        jump_to_group(&mut app, "fun");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        assert_eq!(job, delete_group("fun"));
        // Not deleted: the group's record stays
        app.job_done(&job, Outcome::Failed("删除分组失败:database is locked".into()), life_and_work());
        assert_eq!(app.spaces.group_of("default", "fun"), Some('h'));
        jump_to_group(&mut app, "fun");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, fun_binned(), without_fun());
        // The group's record is cleared (a new fun made later won't land in h again), and kept in the Trash entry
        assert_eq!(app.spaces.group_of("default", "fun"), None);
        assert_eq!(app.trash.get("x1").unwrap().groups.iter().map(|g| g.space).collect::<Vec<_>>(), [Some('h'), None]);
        // A while after the delete (the group's sessions are no longer hidden)
        app.vanish.iter_mut().for_each(|v| v.until = Some(Instant::now()));
        let mut v = without_fun();
        v.push(mk("新的", "fun", State::Idle, 1));
        app.set_sessions(v);
        assert_eq!(space(&app, "新的"), '1');
        // Restored: the group goes back to h, its sessions with it
        ctrl(&mut app, 'z');
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Done("已恢复分组 fun · 4 个会话".into()), life_and_work());
        assert_eq!(app.spaces.group_of("default", "fun"), Some('h'));
        assert_eq!(["生活", "生活二", "私", "子"].map(|t| space(&app, t)), ['h'; 4]);
    }

    #[test]
    fn made_here_stays_here_even_if_its_group_is_kept_elsewhere() {
        let mut app = fun_in_h();
        // Already in the list
        let mut v = life_and_work();
        v.push(mk("新", "fun", State::Idle, 1));
        app.set_sessions(v.clone());
        app.adopt("id-新");
        app.apply_pins();
        assert_eq!((space(&app, "新"), app.spaces.of("id-新")), ('1', Some('1')));
        // Not refreshed in yet: record it first
        app.adopt("id-新二");
        v.push(mk("新二", "fun", State::Idle, 1));
        app.set_sessions(v);
        assert_eq!(space(&app, "新二"), '1');
    }

    pub(super) fn at(app: &App) -> String {
        app.current().unwrap().title.clone()
    }

    #[test]
    fn shift_j_k_jump_between_sections() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // All: fun[问 未读] work[跑 闲 错]
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "跑");
        press(&mut app, KeyCode::Char('J')); // No next section → the last one
        assert_eq!(at(&app), "错");
        press(&mut app, KeyCode::Char('K')); // Back to the start of this section first
        assert_eq!(at(&app), "跑");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
        pin(&mut app, "闲"); // The pinned block is a section of its own: [闲] fun[问 未读] work[跑 错]
        press(&mut app, KeyCode::Char('G')); // To the bottom / top, like vim
        assert_eq!(at(&app), "错");
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(at(&app), "闲");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "问");
        press(&mut app, KeyCode::Char('}')); // Only fun: no sections, J/K = to the bottom / top
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "未读");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
    }

    #[test]
    fn h_from_a_window_or_sub_session_goes_to_its_session_and_out_of_a_single_group() {
        let mut v = sessions();
        let idle = v[3].clone(); // 闲 (idle)
        v.push(sub_of(&idle, 2, false, "zsh", State::Idle, 20));
        let mut k = Session { up: Up::Under("闲".into(), "闲".into()), ..mk("子", "work", State::Idle, 10) };
        k.slot.1 = 10;
        v.push(k);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "zsh", "子", "错"]);
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Left); // ← same as h: window → the session it belongs to
        assert_eq!(at(&app), "闲");
        select(&mut app, "子");
        press(&mut app, KeyCode::Char('h')); // Child session → parent session
        assert_eq!(at(&app), "闲");
        press(&mut app, KeyCode::Char('W')); // Only work: no header, h goes back to all groups, highlight stays
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.group.as_deref(), at(&app)), (None, "闲".to_string()));
    }

    #[test]
    fn the_reference_key_in_the_list_puts_a_picked_reference_into_the_highlighted_session() {
        let mut app = fixture();
        app.ref_keys = list_keys("prefix b", "C-b");
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        select(&mut app, "跑");
        // After the prefix key another key does nothing (not even b's trash once it's a different one)
        app.on_key(ctrl_b);
        press(&mut app, KeyCode::Char('x'));
        assert!(app.insert_to.is_none() && !app.modal_open());
        app.on_key(ctrl_b);
        press(&mut app, KeyCode::Char('b'));
        assert_eq!((app.insert_to.as_deref(), app.insert_back.as_ref().map(|b| b.1.as_str())), (Some("$跑:"), Some("跑")));
        // b held, repeating: nothing; then pick another one
        press(&mut app, KeyCode::Char('b'));
        assert!(!app.modal_open());
        press(&mut app, KeyCode::Char('j'));
        let picked = at(&app);
        assert_ne!(picked, "跑");
        press(&mut app, KeyCode::Enter);
        let (text, of) = app.put.take().unwrap();
        assert!(text.starts_with(&format!("aca 会话 id-{picked}「{picked}」")) && of == picked, "{text}");
        // Pasted (the run loop): back on the row it went into, picking over
        app.end_insert();
        assert_eq!((at(&app), app.insert_to.is_none()), ("跑".to_string(), true));
        // Esc stops picking, back where it was; Enter goes in as usual again
        app.on_key(ctrl_b);
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Esc);
        assert_eq!((at(&app), app.insert_to.is_none(), app.quit), ("跑".to_string(), true, false));
    }

    #[test]
    fn tmux_key_names_are_read_as_the_list_receives_them() {
        let k = |c: char, m| (KeyCode::Char(c), m);
        assert_eq!(list_keys("prefix b", "C-b"), [k('b', KeyModifiers::CONTROL), k('b', KeyModifiers::NONE)]);
        assert_eq!(list_keys("C-M-y", ""), [k('y', KeyModifiers::CONTROL | KeyModifiers::ALT)]);
        assert_eq!(list_keys("F6", ""), [(KeyCode::F(6), KeyModifiers::NONE)]);
        assert_eq!(list_keys("prefix B", "C-a"), [k('a', KeyModifiers::CONTROL), k('B', KeyModifiers::NONE)]);
        assert!(list_keys("prefix b", "").is_empty() && list_keys("Space", "").is_empty());
    }

    /// After Space, press a group header's letter: the highlight stops on the header, doesn't enter a session
    pub(super) fn jump_to_group(app: &mut App, group: &str) {
        press(app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), app.fold_info(), 0, false);
        let pos = view.iter().position(|s| s.group == group).unwrap();
        let row = rows.iter().find(|r| r.head == Some(pos)).unwrap();
        let hint: String = row.lines[0][0].0.trim().into();
        hint.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    /// One more, empty group lab, sorted after work
    fn with_blank() -> App {
        let mut v = sessions();
        v.push(crate::data::blank_row("lab", 2));
        App::new(String::new(), v)
    }

    fn spot(app: &App) -> String {
        app.head.clone().map_or_else(|| at(app), |g| format!("[{g}]"))
    }

    #[test]
    fn an_empty_group_is_just_its_header_and_the_highlight_stops_on_it() {
        let mut app = with_blank(); // 1 day: [fun] 问 [work] 跑 闲 [lab]
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, None, &Folds::new(), 0, false);
        let text: Vec<String> = rows.iter().map(|r| r.lines.concat().iter().map(|(t, _)| t.as_str()).collect()).collect();
        let heads: Vec<&str> = text.iter().map(|t| t.trim_end()).filter(|t| t.contains('▾')).collect();
        // Looks like any other group, with no rows below
        assert_eq!(heads, [" ▾ fun  1", " ▾ work  2", " ▾ lab  0"]);
        assert_eq!(text.len(), 6);
        assert!(text[5].contains("lab"));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(spot(&app), "[lab]");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(spot(&app), "闲");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(spot(&app), "[lab]");
        // Numbers go to sessions only
        press(&mut app, KeyCode::Char('3'));
        assert_eq!(spot(&app), "闲");
        press(&mut app, KeyCode::Char('j'));
        // Still on it after a refresh; it gets a jump hint letter too
        app.set_sessions(app.all.clone());
        assert_eq!(spot(&app), "[lab]");
        press(&mut app, KeyCode::Char('g'));
        jump_to_group(&mut app, "lab");
        assert_eq!(spot(&app), "[lab]");
        // Folded it's a fold row of 0; unfolding goes back to the header
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row() && app.head.is_none());
        assert_eq!(app.fold_info().get("lab"), Some(&(0, 0)));
        press(&mut app, KeyCode::Enter);
        assert_eq!(spot(&app), "[lab]");
        // Session-only keys leave it alone
        press(&mut app, KeyCode::Char('Z'));
        press(&mut app, KeyCode::Char('W'));
        assert!(app.trace.is_none() && app.group.is_none());
        // Not counted by the top bar's counts, Tab's group list or keyword lookup
        assert_eq!(app.groups(), ["fun", "work"]);
        assert!(app.matching("lab").is_empty());
        assert_eq!(app.numbered(4), None);
    }

    #[test]
    fn filters_show_an_empty_group_only_when_its_name_matches() {
        let mut app = with_blank();
        let has = |app: &App| app.visible().0.iter().any(|s| s.blank);
        assert!(has(&app));
        // Nothing active in recent days: still falls back to all as before; it doesn't stop the fallback
        app.all.retain(|s| s.blank || s.title == "未读");
        app.rebuild();
        assert!(app.fell_back && has(&app));
        let mut app = with_blank();
        press(&mut app, KeyCode::Char('w')); // Not stopped
        assert!(!has(&app));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('#')); // A state picked
        assert!(!has(&app));
        press(&mut app, KeyCode::Char('#'));
        press(&mut app, KeyCode::Char('/'));
        typ(&mut app, "la");
        assert!(has(&app));
        assert_eq!(spot(&app), "[lab]");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        typ(&mut app, "跑");
        assert!(!has(&app));
        assert_eq!(app.hidden, 0);
    }

    #[test]
    fn n_on_an_empty_group_starts_the_new_session_inside_it() {
        // No directory recorded: like ^N, starts from the new group directory, straight into fzf
        fn hints_bare() -> crate::data::GroupHints {
            crate::data::GroupHints { groups: vec![("lab".into(), String::new())] }
        }
        let mut app = with_blank().with_group_hints(hints_bare).with_new_group_dir(|| "/p".into());
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('N'));
        assert!(std::mem::take(&mut app.pick));
        let d = app.new_draft.take().unwrap();
        assert_eq!((d.text.as_str(), d.group(), d.label.as_str()), ("/p/", ("lab".into(), false), ""));
        // Still this group after the path changes
        let mut d = d;
        d.set_dir("/q/x");
        assert_eq!(d.group().0, "lab");
        // Directory recorded: starts there, no fzf
        fn hints_lab() -> crate::data::GroupHints {
            crate::data::GroupHints { groups: vec![("lab".into(), "/tmp".into())] }
        }
        let mut app = with_blank().with_group_hints(hints_lab);
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('N'));
        assert!(!app.pick);
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.text.as_str(), d.group().0.as_str()), ("/tmp/", "lab"));
    }

    #[test]
    fn q_sends_an_empty_group_by_recording_the_group_only() {
        let mut app = with_blank();
        press(&mut app, KeyCode::Char('G'));
        press(&mut app, KeyCode::Char('Q'));
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.spaces.group_of(crate::data::REGISTERED, "lab"), Some('2'));
        assert!(app.all.iter().all(|s| app.spaces.of(&notes::key(s)).is_none()));
        assert!(!app.visible().0.iter().any(|s| s.blank));
    }

    #[test]
    fn ctrl_n_creates_the_group_right_away_and_lands_on_it() {
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑");
        ctrl(&mut app, 'n');
        typ(&mut app, "lab");
        press(&mut app, KeyCode::Enter);
        let job = app.job.take().unwrap();
        assert_eq!(job, Job::Group(groups::Act::Create { group: "lab".into() }));
        // Created: highlight on its header, Esc out lands here
        let mut fresh = sessions();
        fresh.push(crate::data::blank_row("lab", 2));
        app.job_done(&job, Outcome::Done(String::new()), fresh);
        assert!(app.msg.is_none());
        app.pick = false;
        press(&mut app, KeyCode::Esc);
        assert!(app.new_draft.is_none());
        assert_eq!(spot(&app), "[lab]");
        // An existing one (differing in case) isn't created again
        ctrl(&mut app, 'n');
        typ(&mut app, "zeta");
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none());
        // Created in another workspace: the group is recorded there
        let mut app = fixture().with_group_hints(hints).in_space('2');
        ctrl(&mut app, 'n');
        typ(&mut app, "lab");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.spaces.group_of(crate::data::REGISTERED, "lab"), Some('2'));
    }

    #[test]
    fn j_k_stop_on_group_headers_too() {
        let mut app = fixture(); // 1 day: [fun] 问 [work] 跑 闲
        let spot = |app: &App| app.head.clone().map_or_else(|| at(app), |g| format!("[{g}]"));
        let walk = |app: &mut App, k: char, n: usize| -> Vec<String> {
            (0..n)
                .map(|_| {
                    press(app, KeyCode::Char(k));
                    spot(app)
                })
                .collect()
        };
        assert_eq!(spot(&app), "问");
        assert_eq!(walk(&mut app, 'j', 4), ["[work]", "跑", "闲", "闲"]);
        assert_eq!(walk(&mut app, 'k', 5), ["跑", "[work]", "问", "[fun]", "[fun]"]);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(spot(&app), "闲");
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(spot(&app), "[fun]");
        press(&mut app, KeyCode::Char('2')); // Numbers are sessions as usual
        assert_eq!(spot(&app), "跑");
        // A folded group is just its one fold row; unfolded, the highlight is still on the group
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        assert_eq!(walk(&mut app, 'k', 2), ["问", "[fun]"]);
        assert_eq!(walk(&mut app, 'j', 2), ["问", "跑"]);
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Enter);
        assert_eq!(spot(&app), "[work]");
        // J / K still land on sessions
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(spot(&app), "问");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(spot(&app), "跑");
    }

    #[test]
    fn on_a_group_the_preview_reads_and_shows_the_groups_dir() {
        let mut app = fixture();
        jump_to_group(&mut app, "work");
        let (key, w) = app.glance_request(false).unwrap();
        assert_eq!((key.as_str(), w.dir.as_str(), w.pid), ("group:work", "/p/work", 0));
        assert!(app.glance_request(false).is_none()); // The same group isn't read twice
        let files = diff::Files { dirs: vec!["src".into()], files: vec!["Cargo.toml".into()], ..Default::default() };
        app.set_glance(key, Glance { files: Some(files), ..Default::default() });
        // Desktop: in the box on the right; phone: below
        for (w, h) in [(100, 30), (46, 24)] {
            let rows = reader_rows(&mut app, w, h);
            assert!(rows.iter().any(|r| r.contains("▸ work")), "{rows:#?}");
            assert!(rows.iter().any(|r| r.contains("「跑」的目录")), "{rows:#?}");
            assert!(rows.iter().any(|r| r.contains("src/") && r.contains("Cargo.toml")), "{rows:#?}");
        }
        // Back on a session: reads the session's
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.glance_request(false).map(|r| r.0).as_deref(), Some("跑"));
        // A fold row is a group too
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.glance_request(false).map(|r| r.0).as_deref(), Some("group:work"));
    }

    #[test]
    fn session_only_keys_do_nothing_on_a_group_header() {
        let mut app = fixture();
        jump_to_group(&mut app, "work");
        // r on a group edits the group (see r_on_a_group_…), e writes the group's note (see e_on_a_group_…)
        for c in ['p', 'u', 'z', 'T'] {
            app.msg = None;
            press(&mut app, KeyCode::Char(c));
            assert!(app.msg.as_ref().is_some_and(|m| m.0.starts_with("不能对分组")), "{c}: {:?}", app.msg);
            assert!(app.confirm.is_none() && app.prompt.is_none() && app.split.is_none() && app.reader.is_none(), "{c}");
            assert_eq!(app.head.as_deref(), Some("work"), "{c}");
        }
        assert!(app.pins.rank("跑").is_none());
        let before = order(&app);
        app.msg = None;
        shift(&mut app, KeyCode::Down); // ⇧↑ ⇧↓ on a group move the group; work is already at the bottom
        assert!(app.msg.is_none() && before == order(&app));
        assert!(app.reorders.is_empty());
        // ⏎ = fold, highlight lands on the fold row
        press(&mut app, KeyCode::Enter);
        assert!(app.on_fold_row() && app.head.is_none() && app.attach.is_none());
        press(&mut app, KeyCode::Enter);
        assert!(!app.on_fold_row() && app.attach.is_none());
    }

    pub(super) fn layout(app: &App) -> Vec<String> {
        let view = app.visible().0;
        list_rows(&view, app.sectioned(), 46, None, app.fold_info(), app.who_width(), false)
            .iter()
            .map(|r| match r.pos {
                Some(p) if app.is_fold_row(p) => format!("[▸ {}{}{}]", r.lines[0][2].0, r.lines[0][3].0, r.lines[0][4].0),
                Some(p) => view[p].title.clone(),
                None => format!("[{}]", r.lines[0][1].0), // Group name on a header row (the pinned block's is 「置顶」, "Pinned")
            })
            .collect()
    }

    #[test]
    fn o_folds_the_current_section_and_enter_unfolds() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // All
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        pin(&mut app, "闲");
        pin(&mut app, "闲"); // Select 闲 (pin then unpin, position unchanged)
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[▸ work  3]"]);
        assert!(app.on_fold_row()); // Highlight lands on the fold row
        press(&mut app, KeyCode::Char('z')); // A fold row can't be read
        assert!(app.reader.is_none());
        press(&mut app, KeyCode::Enter); // ⏎ = unfold, not enter a session
        assert!(app.attach.is_none());
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('o')); // fun has a !, visible even folded
        assert_eq!(layout(&app), ["[▸ fun  2 ◆1]", "[work]", "跑", "闲", "错"]);
        press(&mut app, KeyCode::Char('i')); // Everything that needs you is folded: doesn't go in
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Char('c')); // No copying on a fold row
        assert!(app.clip.is_none());
        press(&mut app, KeyCode::Char('o')); // On a fold row, o = unfold
        assert_eq!(layout(&app)[0], "[fun]");
    }

    #[test]
    fn desktop_has_margins_one_line_a_session_and_no_dim_or_terminal_yellow() {
        let mut v = sessions();
        v[0].question = "Allow edit?".into();
        v[2].who = "team".into();
        v[2].dir = "/p/somewhere".into();
        let mut app = App::new(String::new(), v);
        let (w, h) = (120u16, 36u16);
        let rows = reader_rows(&mut app, w, h);
        // Margins all around: one blank line top and bottom, 3 cells left and right; one more blank line below the top bar and
        // above the bottom bar
        let hh = h as usize;
        assert!(rows[0].is_empty() && rows[1].starts_with("    全部分组 ") && rows[2].is_empty(), "{rows:#?}");
        assert!(rows[3].starts_with("    ▾ fun"), "{rows:#?}");
        assert!(rows[hh - 1].is_empty() && rows[hh - 2].contains("? 快捷键") && rows[hh - 3].is_empty(), "{rows:#?}");
        // One session per row: the account right after the (longest) title, no directory in the list; one asking you gets a line
        // below with what it asks, aligned with the title
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        let col = |y: usize, t: &str| rows[y][..rows[y].find(t).unwrap()].width();
        let run = at("2 ● 跑");
        assert!(col(run, "team") - col(run, "跑") <= 8 + 3, "标题栏至少 8 格,后面是标记和空格:{:?}", rows[run]);
        assert!(!rows.iter().any(|r| r.contains("/p/somewhere")), "{rows:#?}");
        let (ask, q) = (at("1 ◆ 问"), at("Allow edit?"));
        assert_eq!((q, col(q, "Allow")), (ask + 1, col(ask, "问")));
        assert_eq!((at("▾ work"), at("3 ○ 闲")), (q + 2, run + 1));
        assert_eq!(run, q + 3);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        // The preview's box is faint, the title on it dark
        let corner = (0..w).find(|&x| buf[(x, 3)].symbol() == "╭").unwrap();
        assert_eq!(buf[(corner, 3)].fg, theme::pal().faint);
        let title = (corner..w).find(|&x| buf[(x, 3)].symbol() == "问").unwrap();
        assert_eq!(buf[(title, 3)].fg, Color::Reset);
        for y in 0..h {
            for x in 0..w {
                let c = &buf[(x, y)];
                if c.symbol().trim().is_empty() {
                    continue;
                }
                assert!(!c.modifier.contains(Modifier::DIM), "({x},{y}) {:?} 用了 DIM:{rows:#?}", c.symbol());
                assert!(
                    !matches!(c.fg, Color::Yellow | Color::Gray | Color::White | Color::DarkGray),
                    "({x},{y}) {:?} {:?}",
                    c.symbol(),
                    c.fg
                );
            }
        }
    }

    #[test]
    fn click_on_a_section_header_stops_on_the_group() {
        let mut app = fixture();
        let mut t = Terminal::new(TestBackend::new(46, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        // Row 0 top bar, 1 [fun], 2 问, 3 [work], 4 跑
        let click = |app: &mut App, row: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 5, row, modifiers: KeyModifiers::NONE })
        };
        click(&mut app, 4);
        assert_eq!(app.current().unwrap().title, "跑");
        click(&mut app, 3);
        assert_eq!((app.current().unwrap().title.as_str(), app.head.as_deref()), ("跑", Some("work")));
        click(&mut app, 4); // Highlight on a header, click a session below it: selects, doesn't enter
        assert!(app.head.is_none() && app.attach.is_none());
        click(&mut app, 3);
        click(&mut app, 3); // Click the header again = fold
        assert!(app.on_fold_row() && app.attach.is_none());
        press(&mut app, KeyCode::Enter);
        t.draw(|f| draw(f, &mut app)).unwrap();
        click(&mut app, 2);
        assert_eq!(app.current().unwrap().title, "问");
        assert!(app.attach.is_none());
        click(&mut app, 2); // Click again to enter
        assert_eq!(app.attach.as_deref(), Some("问"));
    }

    #[test]
    fn preview_goes_right_only_when_wide_enough() {
        use Preview::*;
        assert_eq!(preview_side(Auto, 46, 30), Some(Side::Below)); // Phone portrait: split
        assert_eq!(preview_side(Auto, 110, 20), Some(Side::Right)); // Phone landscape / desktop
        assert_eq!(preview_side(Auto, 90, 30), Some(Side::Below));
        assert_eq!(preview_side(Focus, 46, 30), Some(Side::Focus)); // Mostly preview
        assert_eq!(preview_side(Focus, 150, 40), Some(Side::Below)); // Wide screens don't have this one (only a phone turning hits it)
        assert_eq!(preview_side(Below, 200, 50), Some(Side::Below));
        assert_eq!(preview_side(Below, 46, 12), None); // Too short
        assert_eq!(preview_side(Off, 200, 50), None);
    }

    #[test]
    fn shift_l_cycles_what_this_screen_can_show_and_phone_and_desk_keep_their_own() {
        use Preview::*;
        let mut app = fixture();
        app.screen = Rect::new(0, 0, 46, 30); // Phone
        let mut seen = vec![];
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('L'));
            seen.push(app.mode());
        }
        assert_eq!(seen, [Focus, Off, Auto]); // split → mostly preview → list only → split, each press different
        press(&mut app, KeyCode::Char('L'));
        let rows = reader_rows(&mut app, 46, 30);
        // The bottom bar lights up for a while: which of these it's on now
        assert_eq!(rows[29].trim(), "布局  分屏 →  预览为主  → 只看列表", "{rows:#?}"); // The current one is on the accent background
        // Mostly preview: the list is down to a few rows; the rule holding the title says which one is highlighted
        let rule = rows.iter().position(|r| r.starts_with("─ ")).unwrap();
        assert!(rule <= 1 + FOCUS_ROWS as usize && rows[rule].contains("1/3"), "{rows:#?}");
        // Desktop keeps its own: a change on a phone doesn't affect the desktop
        app.screen = Rect::new(0, 0, 150, 40);
        assert_eq!(app.mode(), Auto);
        press(&mut app, KeyCode::Char('L'));
        assert_eq!(app.mode(), Below);
        press(&mut app, KeyCode::Char('L'));
        assert_eq!(app.mode(), Off);
        // List only: a line at the bottom says what the highlighted one is doing
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("─ ◆ 等待回应 · ") && r.contains("1/3")), "{rows:#?}");
        assert_eq!((app.view_state().preview, app.view_state().preview_narrow), (Off, Focus));
    }

    #[test]
    fn wrap_breaks_by_display_width_and_prefers_spaces() {
        let wrap_lines = |xs: &[&str], w: usize| ansi::wrap(&ansi::parse_lines(xs), w).iter().map(ansi::plain).collect::<Vec<_>>();
        let v = |xs: &'static [&'static str]| xs;
        assert_eq!(wrap_lines(v(&["hello world foo"]), 11), ["hello world", "foo"]);
        assert_eq!(wrap_lines(v(&["abcdefghij"]), 4), ["abcd", "efgh", "ij"]); // No space: hard break
        assert_eq!(wrap_lines(v(&["中文一二三"]), 4), ["中文", "一二", "三"]); // Two cells per character
        assert_eq!(wrap_lines(v(&["a", "", "b"]), 10), ["a", "", "b"]); // Blank lines kept
        assert_eq!(wrap_lines(v(&["x yyyyyyyyy"]), 6), ["x yyyy", "yyyyy"]); // A space too near the start isn't used as the break
    }

    /// Every one is on screen (each appearing in some row, in order)
    pub(super) fn shows(rows: &[String], wants: &[&str]) {
        for w in wants {
            assert!(rows.iter().any(|r| r.contains(w)), "缺「{w}」:{rows:#?}");
        }
    }

    /// The current scope on the top bar (the accented, underlined one)
    fn active_tab(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        (0..80u16).map(|x| &buf[(x, 0)]).filter(|c| c.modifier.contains(Modifier::UNDERLINED)).map(|c| c.symbol().to_string()).collect()
    }

    /// Subagents in the list (matching the three in the `with_subs` record): Explore running, the one it started running too, the
    /// fork finished
    fn agents() -> Vec<Subagent> {
        let agent = |id: &str, kind: &str, desc: &str, phase, dur, depth| Subagent {
            id: id.into(),
            kind: kind.into(),
            desc: desc.into(),
            phase,
            t0: 1_000_000_000,
            dur,
            tools: 3,
            depth,
            screen: vec!["派给它的".into(), format!("  {desc}"), String::new(), "  09:01 Bash  rg -n width".into()],
            reported: false,
        };
        vec![
            agent("a1", "Explore", "找 width", Phase::Running, 62_000, 0),
            agent("a2", "general-purpose", "细看 ui.rs", Phase::Running, 5_000, 1),
            agent("a3", "fork", "接着想", Phase::Done, 40_000, 0),
        ]
    }

    /// 「跑」 opened another window and dispatched three subagents (the window is put together first at collection time, without
    /// the subagents)
    pub(super) fn with_agents() -> Vec<Session> {
        let mut v = sessions();
        let win = sub_of(v.iter().find(|s| s.title == "跑").unwrap(), 2, false, "zsh", State::Idle, 20);
        v.iter_mut().find(|s| s.title == "跑").unwrap().subagents = agents();
        v.push(win);
        v
    }

    /// See what the list looks like with a real record on this machine (not run in CI):
    /// `ACA_SUBAGENT_PROBE=<session record> cargo test -p aca-tui agents_look -- --ignored --nocapture`.
    /// Every subagent that conversation started hangs off 「跑」 (one in three counted as still running), unfolded, highlight on
    /// the middle one
    #[test]
    #[ignore]
    fn agents_look() {
        let Ok(p) = std::env::var("ACA_SUBAGENT_PROBE") else { return };
        let p = p.split(',').next().unwrap_or_default().to_string();
        let agent = if p.contains("rollout-") { Agent::Codex } else { Agent::Claude };
        let (mut agents, _) = crate::subagent::every(&crate::trace::Trace::open(p.into(), agent).unwrap());
        for a in agents.iter_mut().step_by(3) {
            a.phase = Phase::Running;
        }
        let n = agents.len();
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "跑").unwrap().subagents = agents;
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        for _ in 0..n / 2 {
            press(&mut app, KeyCode::Char('j'));
        }
        for (w, h) in [(150, 44), (60, 36)] {
            println!("[{n} 个子代理 · {w}x{h}]");
            println!("{}", reader_rows(&mut app, w, h).join("\n"));
            println!("{}", "=".repeat(w as usize));
        }
    }

    #[test]
    fn l_unfolds_a_sessions_subagents_into_rows_under_it_and_h_folds_them_back() {
        let mut app = App::new(String::new(), with_agents());
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        // Folded: that row says how many are running, plus a line atop the preview; they take no rows
        let rows = reader_rows(&mut app, 120, 30);
        shows(&rows, &[" 3 ● 跑 +2", "子代理 2 个运行中 · 1 个已完成 · l 展开", "● 1 运行中"]);
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
        // l: unfolds into rows right under the session, before its windows; highlight stays
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(order(&app), ["问", "未读", "跑", "Explore · 找 width", "细看 ui.rs", "fork · 接着想", "zsh", "闲", "错"]);
        assert_eq!(app.current().unwrap().title, "跑");
        let rows = reader_rows(&mut app, 120, 30);
        // Not numbered (other rows' numbers don't shift), tree branches before the text; the nested one has ↳; finished ones are
        // ticked; the last column is how long it ran. The top still counts sessions;
        // the line atop the preview changes to the keys usable now
        shows(
            &rows,
            &[
                "子代理 2 个运行中 · 1 个已完成 · h 收起",
                " 3 ● 跑 +2",
                "    ├ ● Explore · 找 width",
                "    ├ ● ↳ 细看 ui.rs",
                "    ├ ✓ fork · 接着想",
                " 4 └ ○ #2 zsh",
                " 5 ○ 闲",
                "● 1 运行中",
            ],
        );
        let row = |rows: &[String], what: &str| rows.iter().find(|r| r.contains(what)).cloned().unwrap_or_default();
        assert!(row(&rows, "├ ● Explore").contains(" 1分"), "{rows:#?}");
        assert!(row(&rows, "├ ✓ fork").contains("40秒"), "{rows:#?}");
        // Digit keys follow the numbers: 4 is the window, not a subagent
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.current().unwrap().title, "zsh");
        // l again: onto the first subagent; the preview draws its lines, with its state and who dispatched it on top
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.current().unwrap().title, "Explore · 找 width");
        let rows = reader_rows(&mut app, 120, 30);
        shows(
            &rows,
            &[
                "● Explore · 找 width",
                "子代理",
                "运行中 · 1 分钟   3 步",
                "「跑」的子代理 · ⏎ 查看记录",
                "派给它的",
                "09:01 Bash  rg -n width",
            ],
        );
        // Keys that act on a session are ignored; moving works as usual
        for c in ['d', 'r', 'D', 'e', 'z', 'x', 'p', 'N'] {
            app.msg = None;
            press(&mut app, KeyCode::Char(c));
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("子代理只能按 ⏎ 查看记录"), "{c}");
            assert!(app.confirm.is_none() && app.prompt.is_none() && app.reader.is_none() && app.new_draft.is_none(), "{c}");
        }
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "细看 ui.rs");
        // h: back to its session; h again: folds them (the group isn't folded); only another h folds the group
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "跑");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
        assert!(app.folded.is_empty());
        press(&mut app, KeyCode::Char('h'));
        assert!(app.folded.contains("work"));
    }

    #[test]
    fn a_subagent_that_reported_is_previewed_from_the_top_a_running_one_from_the_bottom() {
        let long = |first: &str| std::iter::once(first.to_string()).chain((1..=80).map(|k| format!("  第 {k} 行"))).collect::<Vec<_>>();
        let mut v = with_agents();
        let run = v.iter_mut().find(|s| s.title == "跑").unwrap();
        run.subagents[0].screen = long("派给它的");
        (run.subagents[2].screen, run.subagents[2].reported) = (long("交回来的"), true);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        // Still running: shows the bottom part, like the screen (latest step at the bottom)
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows.iter().any(|r| r.contains("第 80 行")) && !rows.iter().any(|r| r.contains("第 1 行")), "{rows:#?}");
        // Reported back: shown from the top, so the start of what it handed back comes first
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "fork · 接着想");
        // Two presses in a row: the preview waits for the highlight to settle before switching (see SETTLE); treat it as settled here
        app.moved = None;
        let rows = reader_rows(&mut app, 120, 30);
        shows(&rows, &["交回来的", "第 1 行"]);
        assert!(!rows.iter().any(|r| r.contains("第 80 行")), "{rows:#?}");
    }

    #[test]
    fn subagent_rows_follow_the_refresh_and_go_back_under_cover_when_none_are_left() {
        let mut app = App::new(String::new(), with_agents());
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "细看 ui.rs");
        // Refresh: Explore finished and the one it started is no longer listed → highlight back on the session; other rows drawn anew
        let mut v = with_agents();
        let run = v.iter_mut().find(|s| s.title == "跑").unwrap();
        run.subagents.remove(1);
        run.subagents[0].phase = Phase::Done;
        app.set_sessions(v);
        assert_eq!(order(&app), ["问", "未读", "跑", "Explore · 找 width", "fork · 接着想", "zsh", "闲", "错"]);
        assert_eq!(app.current().unwrap().title, "跑");
        let rows = reader_rows(&mut app, 120, 30);
        shows(&rows, &[" 3 ● 跑", "    ├ ✓ Explore · 找 width", "子代理 2 个已完成 · h 收起"]);
        assert!(!rows.iter().any(|r| r.contains("跑 +")), "{rows:#?}");
        // None listed anymore: the rows are gone and unfolded is no longer remembered — the next dispatch starts folded again
        app.set_sessions(sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        app.set_sessions(with_agents());
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
        // On a session with no subagents, l still does nothing
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
    }

    pub(super) fn reader_rows(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                // The cell after a wide character is a placeholder, skip it
                let (mut line, mut x) = (String::new(), 0);
                while x < w {
                    let cell = &buf[(x, y)];
                    line.push_str(&ansi::strip_osc8(cell.symbol()));
                    x += cell.cell_width().max(1);
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn preview_holds_still_while_sliding_and_catches_up_after() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑"); // The next two down are both sessions, no group header in between
        let border = |app: &mut App| reader_rows(app, 100, 24).into_iter().find(|r| r.contains('╭')).unwrap();
        let cur = |app: &App| app.current().unwrap().title.clone();
        border(&mut app);
        press(&mut app, KeyCode::Down); // A single move: switches at once
        let one = cur(&app);
        assert!(border(&mut app).contains(&one));
        press(&mut app, KeyCode::Down); // Another move right after: sliding, the preview stays on the previous one for now
        assert_ne!(cur(&app), one);
        assert!(border(&mut app).contains(&one), "滑动途中不换预览");
        assert!(app.settle_left().is_some());
        // Stopped long enough: switch
        app.moved = app.moved.map(|(_, gap)| (Instant::now() - SETTLE * 2, gap));
        assert!(app.settle_left().is_none());
        let now = cur(&app);
        assert!(border(&mut app).contains(&now));
    }

    #[test]
    fn preview_title_and_list_show_vendor_and_account() {
        let mut v = sessions();
        v[0].who = "team".into(); // 问, the first one highlighted
        v[2].who = "alt".into();
        v[2].agent = Agent::Codex;
        let mut app = App::new(String::new(), v);
        let rows = reader_rows(&mut app, 46, 20);
        // On a phone the preview is below: the title sits in the divider, followed by vendor·account
        assert!(rows.iter().any(|r| r.starts_with("─ ◆ 问 ─") && r.ends_with(" Claude·team ─")), "{rows:#?}");
        // Account names in the list are colored by vendor
        select(&mut app, "闲");
        let mut t = Terminal::new(TestBackend::new(46, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let fg_of = |word: &str| {
            let y = rows.iter().position(|r| r.contains(word) && !r.contains('·')).unwrap() as u16;
            let x = (0..46u16).find(|&x| buf[(x, y)].symbol() == &word[..1]).unwrap();
            buf[(x, y)].fg
        };
        assert_eq!(fg_of("team"), theme::pal().claude);
        assert_eq!(fg_of("alt"), theme::pal().codex);
        // The highlighted row: a pale purple background across the whole row, each span keeping its own color
        let y = rows.iter().position(|r| r.contains(" 闲")).unwrap() as u16;
        let (mut x, mut bad) = (0u16, vec![]);
        while x < 46 {
            let c = &buf[(x, y)];
            if c.bg != theme::pal().sel {
                bad.push(x);
            }
            x += c.cell_width().max(1); // The cell after a wide character is a placeholder, covered by the one before in a terminal
        }
        assert_eq!(buf[(2, y)].fg, theme::pal().muted, "图标 ○ 是灰的,不是反色");
        assert!(bad.is_empty(), "{:?} {bad:?}", rows[y as usize]);
    }

    /// A row a feature adds has its name and value asked when the settings page opens: a language change (the run loop calls
    /// lang_changed when it sees one) asks again on the spot
    pub(super) struct SaysRow;

    impl Setting for SaysRow {
        fn key(&self) -> char {
            'd'
        }
        fn name(&self) -> &'static str {
            t!("假功能", "fake feature")
        }
        fn value(&self) -> Vec<Span<'static>> {
            vec![Span::raw(t!("关着", "off"))]
        }
        fn enter(&self, _: &mut Fx) {}
    }

    /// A settings row and popup a feature adds itself (see overlay.rs): the popup remembers which layer it is, +10 per reload
    pub(super) struct FakeRow;
    pub(super) struct FakePage(pub(super) u8);

    impl Setting for FakeRow {
        fn key(&self) -> char {
            'd'
        }
        fn name(&self) -> &'static str {
            "假功能"
        }
        fn value(&self) -> Vec<Span<'static>> {
            vec![Span::raw("现在关着")]
        }
        fn enter(&self, fx: &mut Fx) {
            fx.open = Some(Box::new(FakePage(0)));
        }
    }

    impl Overlay for FakePage {
        fn width(&self) -> u16 {
            40
        }
        fn lines(&self, _: usize) -> Vec<Line<'static>> {
            vec![Line::raw(format!("假页面 {}", self.0))]
        }
        fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
            match k.code {
                KeyCode::Char('f') => fx.say("说一句"),
                KeyCode::Char('j') => fx.job = Some(Job::Stop { t: target("闲") }),
                KeyCode::Char('n') => fx.open = Some(Box::new(FakePage(self.0 + 1))),
                KeyCode::Char('e') => fx.external = Some(External::Lazygit("/p".into())),
                KeyCode::Char('c') => fx.copy = Some("https://a.ts.net/".into()),
                KeyCode::Esc => return Flow::Close,
                _ => {}
            }
            Flow::Stay
        }
        fn reload(&mut self) {
            self.0 += 10;
        }
        fn typing(&self) -> bool {
            self.0 == 1
        }
    }

    #[test]
    fn what_a_plugin_hangs_on_a_row_follows_its_title() {
        let mut v = sessions();
        v[3].tokens = vec![("pr".into(), "#12".into()), ("ci".into(), "✓".into()), ("_float".into(), "▣2".into())];
        let mut app = App::new(String::new(), v);
        let rows = reader_rows(&mut app, 100, 30);
        // Names starting with _ show only the value
        assert!(rows.iter().any(|r| r.contains("闲 [pr #12] [ci ✓] ▣2")), "{rows:#?}");
    }

    /// A fork: gray text after the title says what it was forked from, before the plugin text; not repeated when the title
    /// already has the original session's name (the default one)
    #[test]
    fn a_fork_says_who_it_came_from_after_its_title_unless_the_title_already_does() {
        let mut v = sessions();
        v[3].fork = Fork::Of("跑".into());
        v[3].tokens = vec![("pr".into(), "#12".into())];
        let mut app = App::new(String::new(), v.clone());
        let rows = reader_rows(&mut app, 100, 30);
        assert!(rows.iter().any(|r| r.contains("闲 分叉自 跑 [pr #12]")), "{rows:#?}");
        // The default title 「跑 (分叉)」 ("跑 (fork)"): the name is already in the title
        v[3].title = "跑 (分叉)".into();
        let mut app = App::new(String::new(), v.clone());
        let rows = reader_rows(&mut app, 100, 30);
        assert!(rows.iter().any(|r| r.contains("跑 (分叉) [pr #12]")) && !rows.iter().any(|r| r.contains("分叉自")), "{rows:#?}");
        // Original session deleted: nothing on the row (the preview says so)
        v[3].fork = Fork::Gone;
        let mut app = App::new(String::new(), v);
        assert!(!reader_rows(&mut app, 100, 30).iter().any(|r| r.contains("分叉自")));
    }
}
