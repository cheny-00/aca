//! The interface: session list + live screen preview of the highlighted session. Refreshes in the background every 2 s; gives up the
//! terminal on attach and resumes drawing after detach.
//!
//! This file keeps `App` (its fields, how it is built, rebuilding the list, the highlight and moving it) and the tests that drive
//! several pages at once. Each page or part lives in its own file next to it: its state, its share of `impl App`, its drawing and
//! its tests (bill, confirm, draft, jobs, keys, list, order, overlays, pick, reader, run, screen, settings, space, term, tools,
//! trace).

use std::collections::{HashMap, HashSet, VecDeque};
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

use aca_core::answer;
use aca_core::i18n::{self, Lang, Tr, noun};
use aca_core::runtime::mark;
use aca_core::{t, tr};
use aca_records::agent::Agent;
use aca_records::subagent::{Phase, Subagent};
use aca_records::words;
use aca_records::workdir::Where;

use crate::ansi::{self, Styled};
use crate::book::Book;
use crate::tickets::Tickets;
use crate::create::{self, DirPick, DirRow, Draft, NewReq, Picked, Removal, Src};
use crate::data::{DEEP, Fork, Session, State, Sub, Up, collect, hangs, sort_sessions};
use crate::diff::{self, Glance, Mode};
use crate::follow::{Follow, Place};
use crate::groups;
use crate::handoff::Source;
use crate::kit::{
    CAP_GAP, SCRATCH, agent_style, button, cap_width, clip, clip_left, dim, esc_hint, group_label, group_shown, icon, key_cap, key_style,
    keys_line, keys_line_help, modal_title, scratch_len, spread, wrap_width,
};
use crate::mcp;
use crate::notes::{self, Notes, group_key, is_group_key};
use crate::ops::{self, Job, Outcome, Target, TaskAct};
use crate::overlay::{Flow, Fx, Overlay, Setting};
use crate::pins::Pins;
use crate::reads::Reads;
use crate::spaces::{self, Spaces};
use crate::state::View;
use crate::theme;
use crate::tools::Tool;
use crate::trash::Trash;
use crate::undo::{self, Undo};
use crate::view::{External, Preview, SEEN_SLACK, Scope, Split, TermReq, WEEK};

mod bill;
mod confirm;
mod draft;
mod jobs;
mod keys;
mod list;
mod notes_page;
mod order;
mod overlays;
mod pick;
mod rail;
mod reader;
mod router;
mod run;
mod screen;
mod settings;
mod space;
mod tasks_page;
mod term;
mod tools;
mod trace;

use bill::{BillView, draw_bill, read_bill};
use confirm::{
    Ask, Batch, Confirm, GroupPick, GroupRemoval, NoteEdit, Pending, Prompt, adopt_plan, draw_answer, draw_confirm, draw_prompt, reply_keys,
};
use draft::draw_draft;
use jobs::{Vanish, target_of};
use list::{
    AGENT_SEP, Boxed, HintMatch, Jump, Pen, TALLY_MARKS, agent_icon, agent_row, agents_line, group_letters, group_pane, hint_badge,
    jump_hints, jump_targets, list_rows_keyed, local_when, match_hint, menu_bar, menu_bar_in, page_bar, pinned_lines, preview_head,
    preview_lines, preview_name, row_keys, session_icon, shows_turn, space_of, split_bar, starts, state_word, strip_prompt, turn_lines,
};
use notes_page::{draw_notes, note_body, note_head, notes_bar, page_cells};
use tasks_page::{draw_give, draw_tasks, task_block, task_head, tasks_bar};
use rail::{RAIL_W, RailAt, RailKind, draw_rail, rail_shown};
use reader::{Clip, Reader, draw_reader};
use router::{RouterPane, pen_lines, router_pane};
use run::{cant_fill, closes_popup, key_is, write_view};
use screen::{draw, input_row, input_rule, modal, modal_beside, side_panel, side_width, title_rule};
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
/// Show only one state, picked in `F` (its rows' keys are 0–6 top to bottom): every session in that state within the group,
/// whatever the scope; ignored while searching. Not saved; gone once turned off
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    /// Every session, whatever its state and however long ago it stopped: what `w` no longer reaches, kept off it so the list
    /// only grows this long when asked for
    Every,
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
            Pick::Every => true,
            Pick::Run => st == State::Running,
            Pick::Wait => matches!(st, State::Ask | State::Unread),
            Pick::Idle => st == State::Idle,
            Pick::Err => st == State::Error,
            Pick::Off => matches!(st, State::Exited | State::Dead) || st.gone(),
            Pick::Reaped => false,
        }
    }

    /// Its key in `F`, and the line shown when the list is empty ("nothing running")
    fn label(self) -> (char, &'static str) {
        match self {
            Pick::Every => ('0', t!("没有会话", "no sessions")),
            Pick::Run => ('1', t!("暂无运行中的会话", "nothing running")),
            Pick::Wait => ('2', t!("暂无待处理会话", "nothing to look at")),
            Pick::Idle => ('3', t!("暂无空闲的会话", "nothing idle")),
            Pick::Err => ('4', t!("暂无出错的会话", "no errors")),
            Pick::Off => ('5', t!("暂无已停止的会话", "nothing stopped")),
            Pick::Reaped => ('6', t!("暂无自动停止的会话", "nothing auto-stopped")),
        }
    }
}

/// Order in `F`'s small list: all first, then the states as the counts on the right of the top bar go
const PICKS: [Pick; 7] = [Pick::Every, Pick::Run, Pick::Wait, Pick::Idle, Pick::Err, Pick::Off, Pick::Reaped];

/// One item of the list as drawn: which index in the view it is (a header = None), which group's header it is (see
/// `Row::head`), how many lines it takes
type Drawn = (Option<usize>, Option<usize>, u16);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Below,
    Right,
    Focus,
}

/// A group's pages, in the order Tab goes round them: its task tickets, its sessions, its notes. Outside a group there is
/// only the list of sessions
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Page {
    Tasks,
    Sessions,
    Notes,
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

/// Open ones: everything except not running (■ in `F`)
fn open(s: &Session) -> bool {
    !Pick::Off.covers(s.state)
}

/// Whether the scope lists this session on its own (its family and the sessions above it come along, see `rebuild`). Empty groups
/// are only a heading, listed in every scope. One someone is attached to is listed too, even with its program gone: the popup
/// opened from inside it highlights it
fn in_scope(scope: Scope, s: &Session) -> bool {
    match scope {
        Scope::Open => s.blank || s.pinned || s.attached || open(s),
        Scope::Week => in_scope(Scope::Open, s) || s.idle_for < WEEK,
    }
}

/// Search: the lowercased word is in its title, group or tmux name (its family and the sessions above it come along, see `rebuild`)
fn matches_word(s: &Session, kw: &str) -> bool {
    format!("{} {} {}", s.title, s.group, s.name).to_lowercase().contains(kw)
}

/// What the scope counts and `F` counts are counts of: rows of sessions and their windows, not subagents or empty groups
fn counted(s: &Session) -> bool {
    s.subagent.is_none() && !s.blank
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

/// `` ` ``: the cursor is on the rail with the groups' letters showing, and goes to the row of the group whose letter is
/// pressed (see pick.rs `on_letters_key`). Where it was before, for `` ` `` or Esc to go back to: on the rail or not, what a
/// rail laid over a narrow screen goes back to, where `a` goes back to
#[derive(Clone, Debug, PartialEq)]
struct Letters {
    rail: Option<RailAt>,
    from: Option<Option<String>>,
    hop: Option<(Option<String>, Option<RailAt>)>,
}

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
    /// Sessions the scope leaves out (none while searching or with a state picked), for `aca ui -l`'s last line
    hidden: usize,
    /// Which one in the visible list (view) is selected; at draw time converted to a row number including section headers, kept in disp
    list: ListState,
    /// Scroll position of the list as actually drawn (with section headers)
    disp: ListState,
    /// Each drawn item (see `Drawn`). Used for clicks, and for which rows are on screen
    drawn: Vec<Drawn>,
    selected: Option<String>,
    /// Which sessions are listed (`w`)
    scope: Scope,
    /// The one state picked in `F`: while set, every session in that state is listed, whatever the scope; cleared, the scope is
    /// back as it was
    status: Option<Pick>,
    /// Only this group (None = all groups)
    group: Option<String>,
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
    /// The groups' notebooks (see book.rs)
    book: Book,
    /// Which workspace each session is in, and the workspaces' names (see spaces.rs)
    spaces: Spaces,
    /// The workspace being viewed
    space: char,
    /// q / Q pressed, waiting for the next key (q again / a workspace's key)
    pending: Option<Pending>,
    /// The workspace list opened by S: index of the highlighted one
    space_menu: Option<usize>,
    /// `F`'s small one-state list: highlighted row (0 = all)
    only_menu: Option<usize>,
    /// `m` on the tasks page: whom the ticket under the cursor can go to (see tasks_page.rs)
    give_menu: Option<tasks_page::GiveMenu>,
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
    /// Plugin capabilities / views / panes to run in the background (the list's `?`, ^R in the reading page): the run loop takes them, one
    /// thread each
    pub plugin_q: Vec<crate::caps::Req>,
    /// The last highlighted session in each workspace: switching back stops there
    space_sel: HashMap<char, String>,
    /// E: the note to paste into its input box before entering the session (key, content)
    pub fill: Option<(String, String)>,
    /// When the highlight last moved, and how long that was after the move before it (see SETTLE)
    moved: Option<(Instant, Duration)>,
    /// Which session the preview is drawing now (None: a group's)
    shown: Option<String>,
    /// The preview switches at every step, even mid-slide: the screen is a local terminal (`data::local_term`), where a switch costs
    /// nothing; SETTLE's hold is for a phone over mosh. Off by default (tests and `--snapshot` hold as the phone does)
    pub preview_follows: bool,
    /// ^G: the note to hand to the editor for writing
    pub edit_note: Option<NoteEdit>,
    /// Read / unread (see reads.rs): records when a session was looked at from aca; no new output since then = not ◐, and `i` skips it
    reads: Reads,
    /// Hint letters already typed in jump mode (entered with Space or a); None = not in jump mode
    jump: Option<String>,
    /// The groups' letters show for `` ` `` (see `Letters`); None = they don't
    letters: Option<Letters>,
    /// The stops with a row on screen in the last frame, top to bottom, and the list they are positions in (`view_mark`):
    /// the ones jump mode gives a letter. Once the list is another one, until it's drawn every stop gets one (see `jump_stops`)
    on_screen: Option<(u64, Vec<Jump>)>,
    /// Where a was pressed from (the group viewed, None = all groups; and whether the cursor was on the rail): a pressed on
    /// all groups goes back there. Gone once used, and when all groups is left another way
    hop_back: Option<(Option<String>, Option<RailAt>)>,
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
    /// The key being handled arrived together with two or more others (the run loop sets it): a paste, not typing. In the reply
    /// box an Enter in it is a line break, not a send
    pub burst: bool,
    /// The reference key (`[keys] ref`) as the list sees it: `prefix b` is two presses, tmux's prefix key then b (no tmux in
    /// between here); empty = none. How many of them were pressed so far
    pub ref_keys: Vec<(KeyCode, KeyModifiers)>,
    ref_got: usize,
    /// The cursor is on the rail, not in the list (None = in the list). A screen too narrow to keep a rail shows one over its
    /// left for as long as this is set
    rail: Option<RailAt>,
    /// What the list showed (None = all groups) when the rail was called out over a narrow screen: where h / Esc go back to
    rail_from: Option<Option<String>>,
    /// Where the rail's rows were last drawn, and which row is at the top (zero-sized: the screen has no rail); for the mouse
    rail_area: Rect,
    rail_top: usize,
    /// The router whose thread shows beside the list (the cursor is on its group's row), and up to when you had seen that
    /// thread on getting there (see `follow_thread`)
    thread_on: Option<(String, u64)>,
    /// The router gone into from its group's row (`o`): back from it, the cursor is still on that row (see `came_back`)
    stay: Option<String>,
    /// The groups whose middle column isn't on their sessions page, each with the page it shows (Tab; see notes_page.rs,
    /// tasks_page.rs). Kept while aca runs: a group comes back on the page it was left on, but for one with tickets not
    /// done, which opens on its tasks (see `set_group`)
    pages: HashMap<String, Page>,
    /// What the last refresh read of the task tickets (see tickets.rs)
    tickets: Tickets,
    /// The ticket the tasks page's cursor is on (its number); none, or one that isn't listed = the first not done
    task_at: Option<i64>,
    /// The first ticket the tasks page draws
    task_top: usize,
    /// Where the last frame drew each ticket: (top row, height, its number), for clicks
    task_hits: Vec<(u16, u16, i64)>,
    /// The note the notes page's cursor is on (its id); none, or one that isn't listed = the first
    note_at: Option<String>,
    /// The first note the notes page draws
    note_top: usize,
    /// Where the notes page drew each note last: (first line, lines, id), for clicks
    note_hits: Vec<(u16, u16, String)>,
    /// The row the highlight was on in each place (None = all groups) when it was left: going back there lands on it
    left_at: HashMap<Option<String>, String>,
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
    /// The tag at the left of the top bar (a screen with no rail of its own): click it to call the rail out
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
    accounts_page: Option<fn(crate::acctpage::Opening) -> Box<dyn Overlay>>,
    /// The tool menu opened by `n` (or `^E`), and which entry is highlighted
    tool_menu: Option<(Vec<Tool>, usize)>,
    router_picker: Option<(Vec<Tool>, usize)>,
    /// The special environments' rows while `n`'s are shown, for `^E` to switch to; empty = none
    tool_more: Vec<Tool>,
    /// Showing the special environments' rows (`^E`)
    tool_special: bool,
    /// The menu's accounts' login state (`claude:team` → it), asked in the background; kept a while (see `LOGINS_KEPT`)
    tool_logins: Arc<std::sync::Mutex<HashMap<String, crate::accounts::Login>>>,
    /// When those answers were started; None = ask them all again next time
    tool_logins_at: Option<Instant>,
    /// `i` on a logged-in account in the menu: its row, waiting for y (the tool clears the login as it starts logging in)
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
    /// Waiting for y to confirm: delete (`d`) / restart (`R`) / stop / adopt. Once confirmed it goes into `job`,
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
    /// Where the hints for picking a group from a path come from when creating (`with_group_hints`); none by default,
    /// unit tests don't touch the real database
    group_hints: fn() -> crate::data::GroupHints,
    /// How `A` identifies the tmux session before adopting (`with_adopt`): given `$session` / `%pane`; can't by default,
    /// unit tests don't touch real tmux or processes
    adopt_planner: fn(&str) -> Result<aca_core::adopt::Plan, String>,
    /// Where `^N`'s default directory comes from (`with_new_group_dir`); ~/workspace by default, unit tests don't touch the real config
    new_group_dir: fn() -> String,
    /// The scratch directory (`with_scratch`): where sessions that belong to no project start; empty = none set (the default),
    /// and how the settings page makes sure a group works in the one just set (returns that group)
    scratch_dir: fn() -> String,
    scratch_ensure: fn(&str) -> Result<String, String>,
    /// The scratch group: the one that works in that directory (found again on every refresh, see `apply_pins`). It sits right
    /// under all groups on the rail, its sessions come first in the list, and it can't be deleted
    scratch: Option<String>,
    /// What the settings page reads and writes (`with_settings`): whether the input method switch is on, config errors, changing an item,
    /// accounts; reads and writes nothing by default, unit tests don't touch the real config
    ime_auto: fn() -> bool,
    config_bad: fn() -> Vec<String>,
    config_set: fn(&str, &str, toml::Value) -> Result<(), String>,
    accounts_lister: fn() -> Vec<crate::accounts::Account>,
    /// Feature popups (see overlay.rs), keys go to the top one; `M`'s MCP is the first
    overlays: Vec<Box<dyn Overlay>>,
    /// `?`: the keys of the page it was pressed on, over everything; keys go to it first (see sheet.rs)
    sheet: Option<crate::sheet::Sheet>,
    /// Rows features add to the settings page (`with_setting`); none by default, unit tests don't touch the real system
    setting_rows: Vec<Box<dyn Setting>>,
    /// Where `M` reads MCP from (`with_mcp`); empty by default, unit tests don't touch real files
    mcp_loader: fn(&Session) -> mcp::Info,
    /// The plugins page opened by `P` (`with_plugins`, see plugins.rs); absent in unit tests, which don't touch the real registry
    plugins_page: Option<fn() -> Box<dyn Overlay>>,
    /// What enabled plugins give the `?` sheet (`aca_core::plugin::menu`); none by default, unit tests don't read this machine's
    plugin_menu: fn() -> Vec<aca_core::plugin::MenuItem>,
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
        #[cfg(test)]
        crate::test_home();
        let mut app = App {
            all: vec![],
            view: vec![],
            hidden: 0,
            list: ListState::default(),
            disp: ListState::default(),
            drawn: vec![],
            selected: None,
            scope: Scope::Open,
            status: None,
            group: None,
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
            book: Book::default(),
            spaces: Spaces::default(),
            space: spaces::HOME,
            pending: None,
            space_menu: None,
            only_menu: None,
            give_menu: None,
            theme_menu: None,
            settings: None,
            settings_back: None,
            sync_failed: vec![],
            ime_changed: false,
            plugin_q: vec![],
            space_sel: HashMap::new(),
            fill: None,
            edit_note: None,
            moved: None,
            shown: None,
            preview_follows: false,
            reads: Reads::default(),
            jump: None,
            letters: None,
            on_screen: None,
            hop_back: None,
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
            burst: false,
            ref_keys: vec![],
            ref_got: 0,
            rail: None,
            rail_from: None,
            rail_area: Rect::default(),
            rail_top: 0,
            thread_on: None,
            stay: None,
            pages: HashMap::new(),
            tickets: Tickets::default(),
            task_at: None,
            task_top: 0,
            task_hits: vec![],
            note_at: None,
            note_top: 0,
            note_hits: vec![],
            left_at: HashMap::new(),
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
            tools_loader: || Ok(vec![]),
            accounts_page: None,
            tool_menu: None,
            router_picker: None,
            tool_more: vec![],
            tool_special: false,
            tool_logins: Arc::default(),
            tool_logins_at: None,
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
            scratch_dir: String::new,
            scratch_ensure: |_| Err(t!("这里不建分组", "no group making here").into()),
            scratch: None,
            ime_auto: || true,
            config_bad: Vec::new,
            config_set: |_, _, _| Err(t!("这里不写配置", "no config writing here").into()),
            accounts_lister: Vec::new,
            overlays: vec![],
            sheet: None,
            setting_rows: vec![],
            mcp_loader: |_| mcp::Info::default(),
            plugins_page: None,
            plugin_menu: Vec::new,
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

    pub fn with_accounts(mut self, f: fn(crate::acctpage::Opening) -> Box<dyn Overlay>) -> Self {
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

    /// Hook up the scratch directory: where it's read from, and how its group is made sure of once it's set
    pub fn with_scratch(mut self, dir: fn() -> String, ensure: fn(&str) -> Result<String, String>) -> Self {
        (self.scratch_dir, self.scratch_ensure) = (dir, ensure);
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
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

    pub fn with_plugin_menu(mut self, f: fn() -> Vec<aca_core::plugin::MenuItem>) -> Self {
        self.plugin_menu = f;
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

    /// Hook up the groups' notebooks (not hooked up = an empty one in memory)
    pub fn with_book(mut self, book: Book) -> Self {
        // The tickets are read from the registry the notebooks are in
        self.tickets = Tickets::on(book.store());
        self.book = book;
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
        // A group's note an older interface wrote into the notes file goes into its notebook; `aca note` may have changed that too
        self.book.adopt(&mut self.notes);
        self.book.reload();
        self.tickets.reload();
        self.spaces.reload();
        self.reads.reload();
        for s in &mut self.all {
            s.state = read_state(s, &self.reads);
        }
        self.apply_pins();
        self.recheck_answer();
        // The group viewed has no sessions left (in this workspace) → back to all groups. With a rail an empty group stays: the
        // rail stops on those too, and only one that is gone altogether goes back
        let rail = self.rail_on_screen();
        if self.group.as_ref().is_some_and(|g| !self.all.iter().any(|s| &s.group == g && s.space == self.space && (rail || !s.blank))) {
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
            KeyCode::Tab => true,
            KeyCode::Char('z' | 'l' | 'r' | 's' | 'n') if ctrl => true,
            KeyCode::Char(_) if ctrl => false,
            KeyCode::Char(c) => c.is_ascii_digit() || " kjJKgGi/awFhlqSvVW?$P".contains(c),
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
        self.sheet.is_some()
            || self.bill.is_some()
            || self.reader.is_some()
            || self.trace.is_some()
            || self.new_draft.is_some()
            || self.prompt.is_some()
            || self.tool_menu.is_some()
            || !self.overlays.is_empty()
            || self.space_menu.is_some()
            || self.only_menu.is_some()
            || self.give_menu.is_some()
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
            preview: self.preview,
            preview_narrow: self.preview_narrow,
            selected: self.selected.clone(),
            space: self.space,
        }
    }

    /// Restore the last view: the scope, the layout, the workspace and the highlighted session (gone = the first one). It always
    /// opens on all groups, whichever group was being viewed
    pub fn restore(mut self, v: View) -> Self {
        self.scope = v.scope;
        self.preview = v.preview;
        self.preview_narrow = v.preview_narrow;
        self.selected = v.selected;
        self.space = v.space;
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
    /// gets the highlight when the background reread arrives. A group's router gone into from the group's row (`o`) leaves the
    /// cursor on that row, with its thread read up to now; so does the desk gone into from the rail's row for all groups. One
    /// gone into from one of its sessions' rows leaves the cursor on that session
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
        if self.stay.take().is_some_and(|r| r == name) && last == name {
            if self.on_route_row() {
                self.thread_read();
            }
            return;
        }
        // Whatever other way it was gone into (a new one made from a group's rail row, a clicked notification): the cursor is
        // on it now
        self.rail = None;
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
        // A router's thread has a mark of its own under its session's key (see `follow_thread`)
        move |k: &str| none || keys.contains(k.strip_prefix(router::THREAD_KEY).unwrap_or(k))
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

    /// W: only the highlighted session's group; if already showing a single group, back to all groups. Either way the highlight
    /// stays on that session
    fn toggle_own_group(&mut self) {
        let to = match (&self.group, self.current()) {
            (Some(_), _) => None,
            (None, Some(s)) if !s.blank => Some(s.group.clone()),
            (None, _) => return,
        };
        if let Some(n) = self.selected.clone() {
            self.left_at.insert(to.clone(), n);
        }
        // Into its group to see it among its own: on the sessions page, whichever page that group would show
        let g = to.clone();
        self.set_group(to);
        if let Some(g) = &g {
            self.show_page(g, Page::Sessions);
        }
    }

    /// w: the other scope (open ones ↔ last 7 days). With a state picked in `F`, that's cleared first and the other scope is
    /// from the one it set aside
    fn cycle_scope(&mut self) {
        self.status = None;
        self.scope = self.scope.next();
        self.rebuild();
    }

    /// The search word changed: the highlight goes to the match with the most recent activity (the shortest time since it last
    /// printed anything; not the time on its row, which for a running one is how long it has run), the open ones first (stopped
    /// ones only when nothing open matches). Rows stay in their order; only matches themselves count, not the relatives they
    /// bring along
    fn land_on_latest(&mut self) {
        let kw = self.filter.to_lowercase();
        let best = (0..self.view.len())
            .filter(|&p| {
                let s = &self.all[self.view[p]];
                counted(s) && matches_word(s, &kw)
            })
            .min_by_key(|&p| {
                let s = &self.all[self.view[p]];
                (!open(s), s.idle_for)
            });
        if best.is_some() {
            self.select(best);
        }
    }

    /// Picked in `F`: only this state, again (or Esc) goes back to the scope
    fn toggle_status(&mut self, p: Pick) {
        self.status = if self.status == Some(p) { None } else { Some(p) };
        self.rebuild();
    }

    /// Sessions of the current workspace and group (empty groups only when sectioned, they're just a heading)
    fn in_view(&self) -> impl Iterator<Item = &Session> + '_ {
        self.in_group(self.group.as_ref())
    }

    /// Sessions of the current workspace in group g (None = all groups)
    fn in_group<'a>(&'a self, g: Option<&'a String>) -> impl Iterator<Item = &'a Session> + 'a {
        self.all.iter().filter(move |s| s.space == self.space && g.is_none_or(|g| &s.group == g))
    }

    /// The number after each scope in the top bar: sessions the scope lists on its own, in the current group
    pub(super) fn scope_count(&self, scope: Scope) -> usize {
        self.in_view().filter(|s| counted(s) && in_scope(scope, s)).count()
    }

    /// The number after the picked state in the top bar: in the current group (`F`'s own counts are the whole workspace's)
    pub(super) fn status_count(&self, p: Pick) -> usize {
        self.in_view().filter(|s| counted(s) && p.hit(s)).count()
    }

    /// What the list lists in group g (None = all groups) with the current scope, state picked and search; and the sessions it was
    /// picked from
    fn listing(&self, group: Option<&String>) -> (Vec<usize>, Vec<usize>) {
        let kw = self.filter.to_lowercase();
        let base: Vec<usize> = (0..self.all.len())
            .filter(|&i| self.all[i].space == self.space && group.is_none_or(|g| &self.all[i].group == g))
            .filter(|&i| group.is_none() || !self.all[i].blank)
            .collect();
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
        // Search ignores the scope and the picked state; the picked state ignores the scope. While searching, an empty group is
        // listed only if its name matches; with a state picked, it isn't listed (with all picked it is, as in the scopes)
        let scope = self.scope;
        let view = match self.status {
            _ if !kw.is_empty() => families(&|s| matches_word(s, &kw)),
            Some(Pick::Every) => base.clone(),
            Some(p) => families(&|s| !s.blank && p.hit(s)),
            None => families(&|s| in_scope(scope, s)),
        };
        (base, view)
    }

    /// The rail's numbers for group g (None = all groups), with the current scope / state picked / search: a group's is its header's in
    /// the list (its sessions, pinned ones aren't in it), all groups' is the top bar's. Then how many of them are ◆ ● ◐ ✕, counted
    /// as the top bar counts them
    pub(super) fn group_tally(&self, g: Option<&String>) -> (usize, [usize; 4]) {
        let view = self.listing(None).1;
        let mine: Vec<&Session> = view.iter().map(|&i| &self.all[i]).filter(|s| g.is_none_or(|g| !s.pinned && &s.group == g)).collect();
        let n = match (g, self.status) {
            (Some(_), _) => mine.iter().filter(|s| s.sub.is_none() && !s.blank).count(),
            (None, _) if !self.filter.is_empty() => mine.iter().filter(|s| counted(s)).count(),
            (None, Some(p)) => self.in_group(None).filter(|s| counted(s) && p.hit(s)).count(),
            (None, None) => self.in_group(None).filter(|s| counted(s) && in_scope(self.scope, s)).count(),
        };
        let mut marks = [0; 4];
        for s in mine.iter().filter(|s| counted(s)) {
            if let Some(k) = TALLY_MARKS.iter().position(|&st| st == s.state) {
                marks[k] += 1;
            }
        }
        (n, marks)
    }

    fn rebuild(&mut self) {
        let kw = self.filter.to_lowercase();
        let (base, view) = self.listing(self.group.as_ref());
        self.view = view;
        // For `aca ui -l`'s count: sessions only (child panes and empty groups don't count)
        let heads = |v: &[usize]| v.iter().filter(|&&i| self.all[i].sub.is_none() && !self.all[i].blank).count();
        self.hidden = if kw.is_empty() && self.status.is_none() { heads(&base) - heads(&self.view) } else { 0 };
        // After a refresh the highlight follows the session, not the old row number
        let pos = self.selected.as_ref().and_then(|n| self.view.iter().position(|&i| &self.all[i].name == n));
        let idx = match pos {
            Some(p) => Some(p),
            None if self.view.is_empty() => None,
            None => Some(self.list.selected().unwrap_or(0).min(self.view.len() - 1)),
        };
        let head = self.head.take();
        self.select(idx);
        // On a group header: still there after a refresh; if it's gone, follow the session
        if let Some(p) = head.as_deref().and_then(|g| self.head_pos(g)) {
            self.select_head(p);
        }
    }

    /// Position in the visible list of the first session under group g's header; not sectioned or not listed = None
    fn head_pos(&self, g: &str) -> Option<usize> {
        if !self.sectioned() {
            return None;
        }
        self.view.iter().position(|&i| !self.all[i].pinned && self.all[i].group == g)
    }

    /// Put the highlight on the header of the group of the p-th session in the visible list (p is the group's first)
    fn select_head(&mut self, p: usize) {
        self.select(Some(p));
        self.head = Some(self.all[self.view[p]].group.clone());
    }

    /// Which group the cursor is on: its header in the list, or its row on the rail. None = on a session, or on a rail row that
    /// is no group (all groups, the Trash)
    fn group_row(&self) -> Option<&str> {
        match self.rail {
            Some(RailAt::Here) => self.group.as_deref(),
            Some(RailAt::Trash) => None,
            None => self.head.as_deref(),
        }
    }

    fn on_group_row(&self) -> bool {
        self.group_row().is_some()
    }

    /// The cursor on an empty group (its header in the list, its row on the rail): return that group
    fn blank_group(&self) -> Option<&str> {
        let g = self.group_row()?;
        let empty = match self.rail {
            Some(_) => !self.all.iter().any(|s| s.space == self.space && s.group == g && !s.blank),
            None => self.current().is_some_and(|s| s.blank),
        };
        empty.then_some(g)
    }

    /// New session on an empty group that has no directory yet: go straight to fzf (same as ^N)
    fn pick_first(&self) -> bool {
        self.blank_group().is_some_and(|g| self.group_dir(g).is_none())
    }

    /// With the highlight on a group, things that only work on sessions just say so and do nothing: returns true
    fn on_group(&mut self, what: &str) -> bool {
        if !self.on_group_row() {
            return false;
        }
        self.flash(&tr!("不能对分组{what}", "can't {what} on a group"));
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

    /// The group under the highlight and its path (shown in the preview); on the rail's row for all groups, the scratch group:
    /// the desk works there. Recomputed only when the group changes or on the timed refresh (see `glance_request`), not by
    /// reading the db every frame
    fn spot(&mut self) -> Option<Spot> {
        let g = self.route_group()?.to_string();
        if self.spot.as_ref().is_none_or(|s| s.group != g) {
            let sp = self.group_dir(&g).map(|(s, dir, set)| Spot { group: g.clone(), dir, from: (!set).then(|| s.title.clone()) });
            self.spot = sp;
        }
        self.spot.clone()
    }

    /// Sessions visible now + how many the scope leaves out (for `aca ui -l`)
    pub fn visible(&self) -> (Vec<&Session>, usize) {
        (self.view.iter().map(|&i| &self.all[i]).collect(), self.hidden)
    }

    fn select(&mut self, idx: Option<usize>) {
        self.list.select(idx);
        self.selected = idx.map(|i| self.all[self.view[i]].name.clone());
        // An empty group's entry draws only the header: being on it means being on the header
        let blank = idx.map(|p| &self.all[self.view[p]]).filter(|s| s.blank);
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
        let Some(s) = self.current().filter(|_| !self.on_rail()) else { return String::new() };
        self.glances.get(&s.name).map(|g| g.work.dir.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| s.dir.clone())
    }

    /// Which registered session is highlighted (kitty's floating window is tied to it, see `tell_var`):
    /// empty for groups, plain tmux sessions, extra windows
    fn here_sid(&self) -> String {
        if self.on_group_row() || self.on_rail() {
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
        // Step one stop at a time in screen order; a group header is a stop too
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
        jump_targets(&view, self.sectioned())
    }

    /// What the list lists, in order, as one number: positions in the list mean the same rows while it's the same
    fn view_mark(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.sectioned().hash(&mut h);
        for &i in &self.view {
            self.all[i].name.hash(&mut h);
        }
        h.finish()
    }

    /// The stops jump mode gives a letter, top to bottom: the sessions with a row on screen in the last frame (a group's
    /// header has the group's own letter, see `group_keys`). A list that frame didn't draw (it changed since, and a key came
    /// before the next frame) has every session
    fn jump_stops(&self) -> Vec<Jump> {
        match &self.on_screen {
            Some((mark, stops)) if *mark == self.view_mark() => stops.clone(),
            _ => self.stops().into_iter().filter(|j| matches!(j, Jump::Row(_))).collect(),
        }
    }

    /// Each group's letter in jump mode (letter, group), in the rail's order: every group of this workspace, the empty ones
    /// too. What the list lists (the scope, a state picked, a search, which group or page) doesn't change them; adding,
    /// deleting or reordering groups does
    pub(super) fn group_keys(&self) -> Vec<(char, String)> {
        let rows = self.rail_rows();
        let groups: Vec<&str> = rows
            .iter()
            .filter_map(|r| match &r.kind {
                RailKind::Group(g) => Some(g.as_str()),
                _ => None,
            })
            .collect();
        group_letters(&groups).into_iter().zip(&groups).filter_map(|(c, g)| Some((c?, g.to_string()))).collect()
    }

    /// ⇧ and a group's letter in jump mode: into that group, as Enter on its rail row goes in — on the page and the row it
    /// was left on — with the cursor in the list, and a rail laid over a narrow screen put away. Already in that group's
    /// list: nothing but the letters going.
    /// A group with no session at all has no row for the cursor, and in its empty list the group's keys (a new session in
    /// it, its note, rename, delete) wouldn't be aimed at it: the cursor is on its rail row instead. On a screen with no
    /// rail that lays the rail over (such a group can't stay the one viewed there, see `set_sessions`)
    pub(super) fn jump_into(&mut self, g: String) {
        if self.group.as_deref() == Some(g.as_str()) && self.rail.is_none() {
            return;
        }
        let bare = !self.all.iter().any(|s| s.group == g && s.space == self.space && !s.blank);
        self.rail_from = None;
        self.rail = bare.then_some(RailAt::Here);
        self.set_group(Some(g));
        self.hop_back = None;
    }

    /// The cursor is in the list of all groups: where `a` shows the letters, and where it comes back from
    fn at_home(&self) -> bool {
        self.group.is_none() && self.rail.is_none()
    }

    /// Whether group g is one the list can show: it has a session in this workspace, or with a rail it's there at all (empty)
    fn group_listed(&self, g: &str) -> bool {
        let rail = self.rail_on_screen();
        self.all.iter().any(|s| s.group == g && s.space == self.space && (rail || !s.blank))
    }

    /// a: from anywhere to the list of all groups, the highlight on the row it was left on, with the jump letters showing; where
    /// it was pressed is remembered. Pressed in all groups, it goes back there, to the row and the column the cursor was in; with
    /// nowhere to go back to it shows the letters, as Space does
    fn hop(&mut self) {
        if self.at_home() {
            if !self.hop_back.take().is_some_and(|back| self.hop_to(back)) {
                self.start_jump();
            }
            return;
        }
        let from = (self.group.clone(), self.rail);
        self.rail = None;
        self.set_group(None);
        // With no rail `set_group` says in the bottom bar where the list is now: the letters' keys are what it should show
        if !rail_shown(self.screen) {
            self.msg = None;
        }
        self.hop_back = Some(from);
        self.start_jump();
    }

    /// Back to where `a` was pressed. False = its group is gone since (deleted, sent to another workspace): nowhere to go
    fn hop_to(&mut self, (group, rail): (Option<String>, Option<RailAt>)) -> bool {
        if group.as_deref().is_some_and(|g| !self.group_listed(g)) {
            return false;
        }
        self.set_group(group);
        self.rail = rail.filter(|_| rail_shown(self.screen));
        true
    }

    /// Show the jump letters (Space, a): the sessions', and the groups' own. With no session listed the groups' still show.
    /// With no rail the bottom bar is where it says ⇧ and a group's letter goes into it (inside a group nothing else does):
    /// what it last said (which group the list went to) makes way
    fn start_jump(&mut self) {
        if !self.view.is_empty() || !self.group_keys().is_empty() {
            self.jump = Some(String::new());
            if !rail_shown(self.screen) {
                self.msg = None;
            }
        }
    }

    /// `: the cursor onto the rail, on the row of the place the list shows, with the groups' letters showing (one is laid
    /// over a narrow screen); the next letter puts it on that group's row. Pressed with the letters showing: back where it
    /// was (see `on_letters_key`)
    pub(super) fn pick_row(&mut self) {
        self.digits.clear();
        self.jump = None;
        let back = Letters { rail: self.rail, from: self.rail_from.clone(), hop: self.hop_back.clone() };
        if !self.on_rail() {
            self.out_to_rail();
        }
        if !self.group_keys().is_empty() {
            self.letters = Some(back);
        }
        if !rail_shown(self.screen) {
            self.msg = None;
        }
    }

    /// Where the highlight is now
    fn at(&self) -> Option<Jump> {
        let p = self.list.selected()?;
        Some(if self.head.is_some() { Jump::Head(p) } else { Jump::Row(p) })
    }

    /// Enter: go into the session; on a group's header, into that group
    fn enter(&mut self) {
        self.digits.clear();
        if let Some(g) = self.group_row().map(String::from) {
            return self.set_group(Some(g));
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
        // `starts` is also what the bottom bar's 「⏎ 启动」 ("⏎ start") goes by
        let own = s.sub.is_none() && !s.deck_id.is_empty();
        if starts(s) && !own {
            self.respawn = Some((s.family().to_string(), self.pane_of(s)));
        } else if starts(s) {
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
        // On the rail no session is under the cursor
        let Some(s) = self.current().filter(|s| s.subagent.is_none() && !self.on_rail()) else { return };
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
    /// c: wait for the key saying what to copy (the bottom bar lists them)
    pub(super) fn start_copy(&mut self) {
        if self.on_group(t!("复制", "copy")) || self.current().is_none() {
            return;
        }
        self.pending = Some(Pending::Copy);
    }

    /// c then a key: c the last reply, b a reference, i s p a one thing about the session (see copies.rs); any other key does nothing
    pub(super) fn copy_by(&mut self, key: char) {
        match key {
            'c' => return self.ask_copy(true),
            'b' => return self.ask_copy(false),
            _ => {}
        }
        let Some(s) = self.current() else { return };
        let id = if s.profile == crate::data::REGISTERED { s.deck_id.as_str() } else { "" };
        match crate::copies::pick(key, id, &crate::data::copy_text(s)) {
            Some(Ok(text)) => self.clip = Some(Clip::Text(text)),
            Some(Err(why)) => self.flash(&why),
            None => {}
        }
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
            asked: None,
            replied: String::new(),
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
            router: false,
            desk: false,
            scratch: 0,
            ticket: String::new(),
            thread: vec![],
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

    /// The fixture with 「未读」 and 「错」 stopped, so the open ones are [fun] 问 [work] 跑 闲
    pub(super) fn three() -> App {
        let mut v = sessions();
        v.iter_mut().filter(|s| s.title == "未读" || s.title == "错").for_each(|s| s.state = State::Stopped);
        App::new(String::new(), v)
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
    fn restore_brings_back_the_view_on_all_groups_and_drops_what_is_gone() {
        let v = View {
            group: Some("work".into()),
            scope: Scope::Week,
            preview: Preview::Off,
            preview_narrow: Preview::Focus,
            selected: Some("错".into()),
            space: spaces::HOME,
        };
        // It opens on all groups whichever group was being viewed, on the session that was highlighted
        let app = fixture().restore(v.clone());
        assert_eq!(app.view_state(), View { group: None, ..v.clone() });
        assert_eq!((app.current().unwrap().title.as_str(), layout(&app)[0].as_str()), ("错", "[fun]"));
        // Session gone → highlight the first
        let gone = View { selected: Some("没了".into()), ..v };
        let app = fixture().restore(gone);
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
        // d on a group's header isn't removing a session: it asks about removing the group
        app = app.with_groups(deck_groups);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Group(_))));
    }

    pub(super) fn select(app: &mut App, title: &str) {
        let p = app.view.iter().position(|&i| app.all[i].title == title).unwrap_or_else(|| panic!("「{title}」 isn't listed"));
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
        let rows = reader_rows(&mut app, 60, 16);
        shows(&rows, &["交接「跑」给", "⚠ 原会话仍在运行,两边会同时修改这个目录", "带上便签", "1 cw  ~/.cw", "2 cx  ~/.cx", "⏎ 交接"]);
        assert!(!rows.iter().any(|r| r.contains("shell")), "{rows:#?}");
        // In English on a phone: the warning wraps rather than being cut, the note stays whole
        let rows = i18n::with(Lang::En, || reader_rows(&mut app, 46, 18));
        let all = rows.iter().map(|r| r.trim_matches(|c: char| c == '│' || c == ' ')).collect::<Vec<_>>().join(" ");
        assert!(all.contains("⚠ still running: both would change") && all.contains("this directory at once"), "{rows:#?}");
        shows(&rows, &["with the draft"]);
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
        let one = |t: &str| aca_core::session::Idled { id: t.into(), title: t.into(), idle: 3 * 86_400 };
        assert_eq!(idle_note(&[one("甲"), one("乙")], Scope::Week), "☾ 自动停止了 2 个:「甲」「乙」 · ⏎ 启动");
        let many: Vec<_> = ["a", "b", "c", "d", "e"].map(one).into_iter().collect();
        assert_eq!(idle_note(&many, Scope::Week), "☾ 自动停止了 5 个:「a」「b」「c」等 2 个 · ⏎ 启动");
        // Where they've left the list, the key is the one that shows them
        assert_eq!(idle_note(&[one("甲")], Scope::Open), "☾ 自动停止了 1 个:「甲」 · w 近 7 天");
        let old = aca_core::session::Idled { idle: 8 * 86_400, ..one("老") }; // idle longer than the last 7 days reach
        assert_eq!(idle_note(&[one("甲"), old.clone()], Scope::Open), "☾ 自动停止了 2 个:「甲」「老」 · F 查看");
        assert_eq!(idle_note(&[old], Scope::Week), "☾ 自动停止了 1 个:「老」 · F 查看");
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
        assert!(!rows.iter().any(|r| r.contains("X 换账号") || r.contains(&caps("X 换账号"))), "{rows:#?}");
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
        shows(&reader_rows(&mut app, 80, 20), &["MCP · 闲", "[ ] alpha", "[✓] beta", "·  shared", "全局", "空格 切换", "⏎ 应用"]);
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
        shows(&reader_rows(&mut app, 80, 20), &["[✓] alpha", "[ ] beta"]);
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
        to_scope(&mut app, Scope::Week);
        select(&mut app, "闲");
        shift(&mut app, KeyCode::Up);
        shift(&mut app, KeyCode::Up); // both moves arrive together; only now does the run loop start writing the first
        assert!(matches!(app.take_job(), Some(Job::Order { .. })));
        assert!(app.msg.is_none());
    }

    #[test]
    fn a_failed_write_drops_the_queue_and_the_list_goes_back() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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

    /// 「闲」 spawned two child sessions: 「子跑」 running (with another window opened in it), 「子旧」 stopped three days ago;
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
        v.push(child("子旧", State::Stopped, 3 * 86400, 9));
        App::new(String::new(), v)
    }

    /// The list row containing want
    pub(super) fn row_of(rows: &[String], want: &str) -> String {
        rows.iter().find(|r| r.contains(want)).unwrap_or_else(|| panic!("缺「{want}」:{rows:#?}")).clone()
    }

    #[test]
    fn a_kept_child_keeps_its_parent_but_not_the_other_way_round() {
        // Parent stopped three days ago, child running: among the open ones the parent stays too, the child hangs below it
        let mut app = with_children(3 * 86400);
        let mut v = app.all.clone();
        v.iter_mut().find(|s| s.title == "闲").unwrap().state = State::Stopped;
        app.set_sessions(v);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "错"]);
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
        for want in ["├ ● 子跑", "│ └ ○ #2 zsh", "└ ■ 子旧"] {
            assert!(row_of(&rr, want).contains(want), "{rr:#?}");
        }
        // p on a child that went up along with its parent: unpins the parent
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已取消置顶「闲」");
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "错"]);
        // The child pinned itself: top level in the pinned block, the parent stays in its group
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已置顶「子跑」");
        assert_eq!(order(&app), ["子跑", "zsh", "问", "未读", "跑", "闲", "错"]);
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
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙", "错"]);
        // The middle one pinned itself: top level in the pinned block with the ones below it, the ones above stay in the group
        pin(&mut app, "孙");
        assert_eq!(order(&app), ["孙", "top", "曾孙", "问", "未读", "跑", "闲", "子跑", "zsh", "错"]);
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
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week); // 7 days: fun[问 未读] work[跑 闲 错]
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.group.as_deref(), Some("work"));
        assert_eq!((titles(&app), at(&app)), (vec!["跑".to_string(), "错".into(), "闲".into()], "闲".to_string()));
        press(&mut app, KeyCode::Char('j')); // switch to another in the group and back, the highlight follows
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), at(&app).as_str()), (None, "错"));
        app.set_group(Some("work".into())); // a group entered another way: W goes back to all from it too
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.group, None);
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
    fn n_shows_each_accounts_login_and_a_opens_the_accounts_page_on_it() {
        use crate::acctpage::Opening;
        let page = |o: Opening| -> Box<dyn Overlay> {
            Box::new(FakePage(match o {
                Opening::At(Agent::Claude, n) if n == "sub" => 1,
                Opening::At(Agent::Claude, n) if n == "ding" => 2,
                Opening::First => 3,
                _ => 0,
            }))
        };
        let mut app = fixture().with_tools(sandbox_tools).with_rehome(|_| vec![], env_logins, false).with_accounts(page);
        app.set_sessions(on_ding());
        // n: the same row as everywhere, logged in or not included
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Down);
        shows(&reader_rows(&mut app, 72, 16), &["2 sub    Claude  ✓", "⏎ 选择 • a 账号页 • + 添加账号 • ^E 特殊环境"]);
        // a: the accounts page on the highlighted account, the menu closed
        press(&mut app, KeyCode::Char('a'));
        assert!(app.tool_menu.is_none());
        shows(&reader_rows(&mut app, 72, 16), &["假页面 1"]);
        press(&mut app, KeyCode::Esc);
        // On the list a isn't the accounts page (it shows the jump letters): that's ^S then a, which opens it on the highlighted
        // session's account; on a group, on the first account
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('a'));
        assert!(app.overlays.is_empty() && app.jump.is_some());
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('a'));
        shows(&reader_rows(&mut app, 72, 16), &["假页面 2"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.settings.is_none());
        jump_to_group(&mut app, "work");
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('a'));
        shows(&reader_rows(&mut app, 72, 16), &["假页面 3"]);
    }

    #[test]
    fn an_account_row_drops_whole_windows_from_the_right_when_narrow() {
        use aca_records::usage::{Quota, Win};
        let now = 1_000_000;
        let win = |label: &str, used, resets| Win { label: label.into(), used, resets: Some(resets) };
        let wins = vec![win("5h", 100.0, now + 3600), win("7d", 73.0, now + 86_400), win("Fable", 17.0, now + 86_400)];
        let q = Quota { agent: Agent::Claude, who: "sub".into(), plan: String::new(), updated: now - 3 * 3600, wins, note: String::new() };
        let login = crate::accounts::Login::In(String::new());
        let full: String = crate::rehome::account_tail(Some(Agent::Claude), Some(Some(&login)), Some(&q), (now, 0), 200)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(full.contains("5h 100% · 7d 73% · Fable 17%") && full.contains("重置") && full.trim_end().ends_with("前"), "{full}");
        for room in 0..full.width() {
            let text: String = crate::rehome::account_tail(Some(Agent::Claude), Some(Some(&login)), Some(&q), (now, 0), room)
                .iter()
                .map(|s| s.content.to_string())
                .collect();
            // Tool and login always; then only whole windows, each with its number
            assert!(text.width() <= room.max(18) && text.starts_with("Claude  ✓"), "{room}: {text}");
            assert!(full.starts_with(text.trim_end()), "{room}: {text}");
            for (label, pct) in [("5h ", "5h 100%"), ("7d ", "7d 73%"), ("Fable ", "Fable 17%")] {
                assert!(!text.contains(label) || text.contains(pct), "{room}: {text}");
            }
            assert!(!text.trim_end().ends_with('·'), "{room}: {text}");
        }
    }

    #[test]
    fn i_starts_the_same_login_for_an_account_wherever_it_is_listed() {
        let dir = std::env::temp_dir().join(format!("aca-same-login-{}", std::process::id()));
        let mut app = fixture()
            .with_tools(sandbox_tools)
            .with_rehome(|_| vec![], env_logins, false)
            .with_login_links(dir.clone())
            .with_handoff(fake_source)
            .with_group_hints(hints);
        app.set_sessions(on_ding());
        let run = |app: &mut App| app.term_req.take().and_then(|r| r.run);
        let (sub, team) = (crate::accounts::relogin_cmd(Agent::Claude, "sub"), crate::accounts::relogin_cmd(Agent::Claude, "team"));
        // n: sub is logged in, so it asks first
        keys(&mut app, "n↓iy");
        assert_eq!(run(&mut app), Some(sub.clone()));
        // H: the same; the hand off is dropped with the menu (H again once logged in)
        select(&mut app, "跑");
        keys(&mut app, "Hiy");
        assert_eq!((run(&mut app), app.handoff.is_none(), app.tool_menu.is_none()), (Some(sub.clone()), true, true));
        // X: the other Claude accounts, sub first
        keys(&mut app, "Xiy");
        assert_eq!(run(&mut app), Some(sub));
        // team, in its special environment: ^E and X
        ctrl(&mut app, 'e');
        keys(&mut app, "↓↓↓iy");
        assert_eq!(run(&mut app), Some(team.clone()));
        keys(&mut app, "X↓iy");
        assert_eq!(run(&mut app), Some(team));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn i_in_ctrl_e_logs_an_account_in_within_its_environment_asking_first_when_logged_in() {
        let dir = std::env::temp_dir().join(format!("aca-env-login-{}", std::process::id()));
        let mut app = fixture().with_tools(sandbox_tools).with_rehome(|_| vec![], env_logins, false).with_login_links(dir.clone());
        ctrl(&mut app, 'e');
        // Each account as everywhere: name, tool, login
        let rows = reader_rows(&mut app, 72, 20);
        shows(&rows, &["2 team-unis   Codex   未登录", "4 team        Claude  ✓", "5 team        Codex   ✓", "⏎ 选择 • a 账号页"]);
        // A terminal has none; l doesn't log in any more
        press(&mut app, KeyCode::Char('i'));
        assert!(app.term_req.is_none() && app.msg.as_ref().is_some_and(|m| m.0 == "终端不用登录"));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('l'));
        assert!(app.term_req.is_none() && app.tool_relogin.is_none());
        // Logged out: the bar says i; straight to its login, run for its environment's account, the link caught for the popup
        shows(&reader_rows(&mut app, 72, 20), &["⏎ 选择 • i 登录 • a 账号页"]);
        press(&mut app, KeyCode::Char('i'));
        let r = app.term_req.take().unwrap();
        let cmd = crate::accounts::relogin_cmd(Agent::Codex, "team-unis");
        assert_eq!((r.title.as_str(), r.run.as_deref()), ("登录 Codex · team-unis", Some(cmd.as_str())));
        assert!(r.link.is_some_and(|c| c.screen && c.file.starts_with(&dir)));
        assert!(app.tool_menu.is_none());
        // Logged in, Codex or Claude: what it costs as on the accounts page, and only y goes ahead
        for (downs, title) in [(4, "登录 Codex · team"), (3, "登录 Claude Code · team")] {
            ctrl(&mut app, 'e');
            for _ in 0..downs {
                press(&mut app, KeyCode::Down);
            }
            press(&mut app, KeyCode::Char('i'));
            shows(&reader_rows(&mut app, 72, 24), &["重新登录", "账号 team?", "y 重新登录"]);
            press(&mut app, KeyCode::Char('n'));
            assert!(app.term_req.is_none() && app.tool_relogin.is_none() && app.tool_menu.is_some());
            press(&mut app, KeyCode::Char('i'));
            press(&mut app, KeyCode::Char('y'));
            assert_eq!(app.term_req.take().map(|r| r.title).as_deref(), Some(title));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pick one state: `F`, then that state's key in its menu
    pub(super) fn only(app: &mut App, key: char) {
        press(app, KeyCode::Char('F'));
        press(app, KeyCode::Char(key));
    }

    /// Press `w` until the list shows this scope
    pub(super) fn to_scope(app: &mut App, scope: Scope) {
        for _ in 0..=Scope::ALL.len() {
            if app.scope == scope && app.status.is_none() {
                return;
            }
            press(app, KeyCode::Char('w'));
        }
        panic!("w never got to {scope:?}");
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn w_switches_between_open_and_seven_days_and_never_lists_everything() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        v.push(mk("老停", "work", State::Stopped, 9 * 86_400)); // stopped nine days ago
        v.push(mk("老闲", "work", State::Idle, 9 * 86_400)); // idle nine days, still open
        let mut app = App::new(String::new(), v);
        // Open ones by default, however long they've been idle
        assert_eq!((app.scope, titles(&app)), (Scope::Open, names(&["未读", "老闲", "跑", "错", "问", "闲"])));
        assert_eq!(active_tab(&mut app), "未停止 6");
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.scope, titles(&app)), (Scope::Week, names(&["停", "未读", "老闲", "跑", "错", "问", "闲"])));
        assert_eq!(active_tab(&mut app), "近 7 天 7");
        assert!(app.msg.is_none()); // lit in the top bar, nothing extra in the bottom bar
        let top = reader_rows(&mut app, 120, 20)[0].clone();
        assert!(top.contains("未停止 6   近 7 天 7") && !top.contains("全部 "), "{top}");
        // Back to open ones: one stopped nine days ago is never listed by w, only by F's ■
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(app.scope, Scope::Open);
        only(&mut app, '5');
        assert_eq!(titles(&app), ["停", "老停"]);
        // Kept for next time
        press(&mut app, KeyCode::Char('w'));
        let again = App::new(String::new(), sessions()).restore(app.view_state());
        assert_eq!(again.scope, Scope::Week);
        // The keys that used to change the days and pick states on the list do nothing now
        let mut app = App::new(String::new(), sessions());
        for c in ['*', '!', '@', '#', '&', '^', '%'] {
            press(&mut app, KeyCode::Char(c));
            assert_eq!((app.scope, app.status, app.only_menu), (Scope::Open, None, None), "{c}");
        }
    }

    #[test]
    fn open_counts_what_f_counts_for_the_open_states_plus_pinned_stopped_ones() {
        // A running child under a stopped parent, the child's window, a stopped one and a pinned stopped one
        let mut app = with_children(3 * 86400);
        let mut v = app.all.clone();
        v.iter_mut().find(|s| s.title == "闲").unwrap().state = State::Stopped;
        v.push(mk("停", "work", State::Stopped, 9 * 86_400));
        v.push(mk("钉", "fun", State::Stopped, 9 * 86_400));
        app.set_sessions(v);
        only(&mut app, '5'); // 「钉」 stopped nine days ago: only ■ lists it
        pin(&mut app, "钉");
        to_scope(&mut app, Scope::Open);
        let f = |p| app.pick_count(p);
        assert_eq!(app.scope_count(Scope::Open), f(Pick::Run) + f(Pick::Wait) + f(Pick::Idle) + f(Pick::Err) + 1);
        assert_eq!(Scope::ALL.map(|s| app.scope_count(s)), [7, 9]); // 「闲」 and 「子旧」 stopped three days ago
        // The stopped parent comes along with its running child, but isn't counted
        assert!(titles(&app).contains(&"闲".to_string()));
        assert_eq!(active_tab(&mut app), "未停止 7");
        // In one group: that group's
        app.set_group(Some("work".into()));
        assert_eq!(Scope::ALL.map(|s| app.scope_count(s)), [4, 6]);
    }

    #[test]
    fn nothing_open_says_so_and_w_goes_on_to_seven_days() {
        // After a reboot nothing is open; 「问」 stopped longest ago, over a week
        let mut v = sessions();
        v.iter_mut().for_each(|s| s.state = State::Stopped);
        let mut app = App::new(String::new(), v);
        assert_eq!((titles(&app).len(), app.hidden), (0, 5));
        shows(&reader_rows(&mut app, 60, 20), &["暂无未停止的会话 · w 近 7 天"]);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["未读", "跑", "错", "闲"]);
        // Nothing in the last week either
        let mut v = sessions();
        v.retain(|s| s.title == "问");
        v[0].state = State::Stopped;
        let mut app = App::new(String::new(), v);
        shows(&reader_rows(&mut app, 60, 20), &["暂无未停止的会话 · F 查看已停止的"]); // w would list nothing either
        press(&mut app, KeyCode::Char('w'));
        shows(&reader_rows(&mut app, 60, 20), &["近 7 天暂无会话 · F 查看已停止的"]);
        only(&mut app, '5');
        assert_eq!(titles(&app), ["问"]);
        // No sessions at all: no key to point at
        let mut app = App::new(String::new(), vec![]);
        shows(&reader_rows(&mut app, 60, 20), &["没有会话"]);
        press(&mut app, KeyCode::Char('w'));
        shows(&reader_rows(&mut app, 60, 20), &["没有会话"]);
        // An empty group viewed alone: it's the group that has none
        let mut app = App::new(String::new(), sessions());
        app.set_group(Some("空".into()));
        shows(&reader_rows(&mut app, 60, 20), &["这一组没有会话"]);
    }

    #[test]
    fn a_state_picked_in_f_lists_all_of_it_whatever_the_scope_and_esc_goes_back() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        v.push(mk("老停", "work", State::Stopped, 9 * 86_400));
        let mut reaped = mk("睡", "work", State::Stopped, 4 * 3600);
        reaped.idle_stopped = 1;
        v.push(reaped);
        let mut app = App::new(String::new(), v);
        // ■ from open ones, which have none: every stopped one, however old; the top bar says so in place of the scopes
        only(&mut app, '5');
        assert_eq!((app.scope, titles(&app)), (Scope::Open, names(&["停", "睡", "老停"])));
        assert_eq!(lit_counts(&mut app, 120), "■ 3 已停止");
        let rows = reader_rows(&mut app, 120, 20);
        assert!(rows[0].contains("仅 ■ 已停止 3") && !rows[0].contains("未停止"), "{}", rows[0]);
        assert!(rows.iter().any(|r| r.trim_start().starts_with(&caps("Esc 清除"))), "{rows:#?}");
        // ☾: only the ones aca stopped for idling
        only(&mut app, '6');
        assert_eq!(titles(&app), ["睡"]);
        // The same one again, or Esc, goes back to the scope as it was
        only(&mut app, '6');
        assert_eq!((app.status, app.scope, titles(&app).len()), (None, Scope::Open, 5));
        to_scope(&mut app, Scope::Week);
        only(&mut app, '1');
        assert_eq!(titles(&app), ["跑"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.status, app.scope), (None, Scope::Week));
        // w with a state picked: the state goes, on to the other scope from the one it set aside
        only(&mut app, '3');
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, app.scope), (None, Scope::Open));
        // Esc at the bottom doesn't quit (only popups do), quitting is q q
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit);
        assert_eq!(app.msg.as_ref().unwrap().0, "q q 退出");
        keys(&mut app, "qq");
        assert!(app.quit);
    }

    #[test]
    fn f_then_0_lists_every_session_however_long_ago_it_stopped_until_cleared() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        v.push(mk("老停", "work", State::Stopped, 9 * 86_400)); // past what w reaches
        let mut app = App::new(String::new(), v);
        only(&mut app, '0');
        assert_eq!((app.status, titles(&app).len()), (Some(Pick::Every), 7));
        assert!(titles(&app).contains(&"老停".to_string()));
        // The top bar gets a third cell, 「所有 7」, the lit one (the two scopes are still there, neither lit), and no state cell is lit
        let top = reader_rows(&mut app, 120, 20)[0].clone();
        assert!(top.contains("未停止 5   近 7 天 6   所有 7") && !top.contains("仅") && !top.contains("全部 7"), "{top}");
        assert_eq!(active_tab(&mut app), "所有 7");
        assert_eq!(lit_counts(&mut app, 120), "");
        // With no room for all three, the lit one stays
        let top = reader_rows(&mut app, 30, 20)[0].clone();
        assert!(top.contains("所有 7") && !top.contains("未停止"), "{top}");
        // In F it's row 0, highlighted and numbered like the others
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(0));
        shows(&reader_rows(&mut app, 60, 16), &["所有", "7"]);
        // 0 again clears it, back to the scope as it was; so does Esc
        press(&mut app, KeyCode::Char('0'));
        assert_eq!((app.status, app.scope, titles(&app).len()), (None, Scope::Open, 5));
        only(&mut app, '0');
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.status, titles(&app).len()), (None, 5));
        // Not kept for next time: it opens on the scope
        only(&mut app, '0');
        let again = App::new(String::new(), sessions()).restore(app.view_state());
        assert_eq!((again.status, again.scope), (None, Scope::Open));
    }

    #[test]
    fn zero_on_the_list_lists_every_session_and_again_or_esc_goes_back() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        v.push(mk("老停", "work", State::Stopped, 9 * 86_400)); // past what w reaches
        let mut app = App::new(String::new(), v);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('0'));
        assert_eq!((app.status, titles(&app).len(), at(&app)), (Some(Pick::Every), 7, "跑".to_string()));
        // It's a key here, not a digit: no number is being typed, and the top bar shows none
        let top = reader_rows(&mut app, 120, 20)[0].clone();
        assert!(app.digits.is_empty() && top.contains("所有 7") && !top.contains('#'), "{top}");
        // Again: back to the scope as it was, so twice in a row is on and off
        press(&mut app, KeyCode::Char('0'));
        assert_eq!((app.status, app.scope, titles(&app).len()), (None, Scope::Open, 5));
        keys(&mut app, "00");
        assert_eq!(app.status, None);
        // Esc goes back too
        press(&mut app, KeyCode::Char('0'));
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.status, app.scope), (None, Scope::Open));
        // w from there: on to the other scope from the one it set aside, as with a state picked in F
        keys(&mut app, "0w");
        assert_eq!((app.status, app.scope, active_tab(&mut app).as_str()), (None, Scope::Week, "近 7 天 6"));
        // Over a state picked in F it takes that state's place
        only(&mut app, '1');
        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.status, Some(Pick::Every));
        // Not kept for next time
        let again = App::new(String::new(), sessions()).restore(app.view_state());
        assert_eq!(again.status, None);
    }

    #[test]
    fn a_zero_typed_right_after_a_digit_is_the_numbers_next_digit_not_the_key() {
        let v: Vec<Session> = (1..=12).map(|i| mk(&format!("s{i}"), "work", State::Idle, 10)).collect();
        let mut app = App::new(String::new(), v);
        let rows = order(&app);
        assert_eq!(rows.len(), 12);
        // 1 then 0 is row 10, and nothing else is listed for it
        keys(&mut app, "10");
        assert_eq!((app.status, at(&app)), (None, rows[9].clone()));
        // No row 40: the 0 is dropped. The list is as it was, the highlight on row 4, and the number still 4
        app.digits.clear();
        keys(&mut app, "40");
        assert_eq!((app.status, at(&app), app.digits.as_str()), (None, rows[3].clone(), "4"));
        // Another one right after is dropped the same way; a digit that makes no number starts a new one, as before
        press(&mut app, KeyCode::Char('0'));
        assert_eq!((app.status, app.digits.as_str()), (None, "4"));
        press(&mut app, KeyCode::Char('7'));
        assert_eq!((at(&app), app.digits.as_str()), (rows[6].clone(), "7"));
        // The number is over once the gap has passed: 0 is the key again, and leaves no number behind
        app.digits_at = Instant::now() - DIGIT_GAP - Duration::from_millis(1);
        press(&mut app, KeyCode::Char('0'));
        assert_eq!((app.status, app.digits.as_str()), (Some(Pick::Every), ""));
    }

    #[test]
    fn an_empty_state_says_so_and_esc_goes_back() {
        let mut v = sessions();
        v.retain(|s| s.title != "错");
        let mut app = App::new(String::new(), v);
        only(&mut app, '4');
        assert!(app.visible().0.is_empty());
        shows(&reader_rows(&mut app, 46, 20), &["暂无出错的会话 · Esc 清除"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(titles(&app).len(), 4);
    }

    #[test]
    fn a_crashed_one_counts_as_not_running_not_as_an_error() {
        let mut v = sessions();
        v[3].state = State::Crashed; // 「闲」: the last turn ended in an error, gone from tmux too
        let mut app = App::new(String::new(), v);
        only(&mut app, '4'); // only the ones still open, stuck on an error
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["错".to_string()], "✕ 1 出错".to_string()));
        only(&mut app, '5');
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["闲".to_string()], "■ 1 已停止".to_string()));
        press(&mut app, KeyCode::Esc); // nor as open
        assert!(!titles(&app).contains(&"闲".to_string()));
    }

    #[test]
    fn the_popup_opened_from_a_session_whose_program_exited_lands_on_it() {
        let mut v = sessions();
        (v[3].state, v[3].attached) = (State::Dead, true); // 「闲」: its program exited, you're in its pane
        let mut app = App::new(String::new(), v);
        app.highlight_here("$闲", "");
        assert_eq!((app.scope, app.current().unwrap().title.as_str()), (Scope::Open, "闲"));
    }

    #[test]
    fn pinned_ones_are_listed_in_every_scope_even_stopped() {
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」
        v[3].idle_for = 9 * 86_400;
        let mut app = App::new(String::new(), v);
        only(&mut app, '5'); // stopped nine days ago: only ■ lists it
        pin(&mut app, "闲");
        for scope in Scope::ALL {
            to_scope(&mut app, scope);
            assert_eq!(order(&app), ["闲", "问", "未读", "跑", "错"], "{scope:?}");
        }
    }

    #[test]
    fn search_ignores_the_status_and_it_is_not_remembered() {
        let mut app = fixture();
        only(&mut app, '1');
        press(&mut app, KeyCode::Char('/'));
        typ(&mut app, "闲");
        press(&mut app, KeyCode::Enter);
        assert_eq!(titles(&app), ["闲"]);
        assert_eq!(lit_counts(&mut app, 120), ""); // no fill in the top bar while searching either
        press(&mut app, KeyCode::Esc); // clears the search term first, the state stays
        assert_eq!(titles(&app), ["跑"]);
        let app = fixture().restore(app.view_state());
        assert_eq!((app.status, titles(&app).len()), (None, 5));
    }

    #[test]
    fn the_lists_keys_fit_on_a_big_screen_in_columns_and_stay_one_column_on_a_phone() {
        let mut app = fixture();
        keys(&mut app, "?");
        // How far the legend's row reaches: past one column's width = other columns beside it
        let reach = |rows: &[String]| rows.iter().find(|r| r.contains("图例")).map_or(0, |r| r.trim_end_matches(['│', '░', ' ']).width());
        // 200×50: three columns side by side, all of it in view, no count
        let big = reader_rows(&mut app, 200, 50);
        let heads = ["图例", "导航", "打开", "列出哪些", "会话", "页面", "tmux 中", "新建时"];
        assert!(heads.iter().all(|h| big.iter().any(|r| r.contains(&format!("  {h} ")))), "{big:#?}");
        assert!(!big.iter().any(|r| r.contains("还有") || r.contains(" / 7")) && reach(&big) > 130, "{big:#?}");
        // 120×36: two columns that still don't hold it: it says how many are left
        let mid = reader_rows(&mut app, 120, 36);
        assert!(mid.iter().any(|r| r.contains("↓ 还有")) && (70..120).contains(&reach(&mid)), "{mid:#?}");
        // A phone: one column, as before
        let phone = reader_rows(&mut app, 46, 30);
        assert!(phone.iter().any(|r| r.contains("↓ 还有")) && reach(&phone) < 46, "{phone:#?}");
    }

    #[test]
    fn search_lands_on_the_match_most_recently_active_without_reordering() {
        // Stopped weeks ago (mk counts activity back from a fixed moment, so the age is set after)
        let old = |t: &str, days: u64| Session { idle_for: days * 86_400, active_at: 0, ..mk(t, "a", State::Stopped, 0) };
        let mut v: Vec<Session> = (0..12).map(|i| old(&format!("aca 停{i}"), 20 - i)).collect();
        v.push(old("aca 老", 28));
        v.push(mk("aca 闲", "b", State::Idle, 2 * 3600));
        v.push(mk("aca 新", "c", State::Idle, 5 * 60));
        v.push(mk("别的", "c", State::Idle, 60));
        let mut app = App::new(String::new(), v);
        // Scrolled down to the bottom first (stopped weeks ago: only ■ lists them)
        only(&mut app, '5');
        keys(&mut app, "G");
        assert!(!reader_rows(&mut app, 46, 12).iter().any(|r| r.contains("aca 停0")));
        keys(&mut app, "/aca");
        // On a tall screen everything fits again: nothing is left scrolled away above
        let tall = reader_rows(&mut app, 100, 40);
        assert!(tall.iter().any(|r| r.contains("aca 停0")) && !tall.iter().any(|r| r.contains("还有")), "{tall:#?}");
        // Rows keep their order (by group); the highlight is on the one 5 minutes ago, scrolled into view on a short screen
        assert_eq!(titles(&app).first().map(String::as_str), Some("aca 停0"));
        assert_eq!(app.current().unwrap().title, "aca 新");
        let mut t = Terminal::new(TestBackend::new(46, 12)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let lit: Vec<String> = (0..12u16)
            .filter(|&y| buf[(0, y)].symbol() == "▌")
            .map(|y| (0..46u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect();
        assert!(lit.len() == 1 && lit[0].contains("aca 新"), "{lit:?}");
        // ↑ ↓ move as usual (past the next group's header to its row); ⏎ ends typing and the highlight stays put
        keys(&mut app, "↓↓⏎");
        assert_eq!((app.current().unwrap().title.as_str(), app.head.as_deref()), ("aca 老", None));
        // Another word lands again; only stopped ones match: the most recent of them
        keys(&mut app, "/");
        typ(&mut app, " 停1");
        assert_eq!(app.current().unwrap().title, "aca 停11");
        // Cleared: back to the top, as before
        for _ in 0..7 {
            press(&mut app, KeyCode::Backspace);
        }
        assert_eq!(app.list.selected(), Some(0));
        // A running row goes by when it last printed something, not by the time on its row (how long it has run): running for 2
        // hours and still printing beats idle for 5 minutes; running but quiet for 10 minutes doesn't
        let v = vec![Session { ran: 2 * 3600, ..mk("aca 跑", "a", State::Running, 1) }, mk("aca 闲", "b", State::Idle, 5 * 60)];
        let mut app = App::new(String::new(), v);
        keys(&mut app, "/aca");
        assert_eq!(app.current().unwrap().title, "aca 跑");
        let v = vec![Session { ran: 60, ..mk("aca 跑", "a", State::Running, 10 * 60) }, mk("aca 闲", "b", State::Idle, 5 * 60)];
        let mut app = App::new(String::new(), v);
        keys(&mut app, "/aca");
        assert_eq!(app.current().unwrap().title, "aca 闲");
    }

    #[test]
    fn status_keeps_families_and_parents_together() {
        // Session in error for over a day, another window running: the whole family stays
        let mut app = with_windows();
        only(&mut app, '1');
        assert_eq!(order(&app), ["跑", "错", "构建"]);
        // A child running: its parent idle five days is listed too; the parent alone doesn't bring out its children
        let mut app = with_children(5 * 86400);
        only(&mut app, '1');
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
        only(&mut app, '1');
        assert_eq!(lit_counts(&mut app, 120), "● 1 运行中");
        only(&mut app, '2');
        assert_eq!(lit_counts(&mut app, 120), "◆ 1 等待回应 ◐ 1 未读");
        // Idle and ■ normally aren't counted; listed only when picked, even with 0
        assert!(!reader_rows(&mut app, 120, 20)[0].contains("空闲"));
        only(&mut app, '3');
        assert_eq!(lit_counts(&mut app, 120), "○ 1 空闲");
        only(&mut app, '5');
        assert_eq!(lit_counts(&mut app, 120), "■ 0 已停止");
        // Narrower, the counts are shortened, still filled
        assert_eq!(lit_counts(&mut app, 36), "■0");
    }

    #[test]
    fn the_rail_keeps_to_this_workspace() {
        let mut app = fun_in_h();
        let groups = |app: &App| -> Vec<String> {
            app.rail_rows().into_iter().filter(|r| matches!(r.kind, RailKind::Group(_))).map(|r| r.name).collect()
        };
        assert_eq!(groups(&app), ["work"]); // the whole fun group is in h: not listed in 1
        keys(&mut app, "Sh");
        assert_eq!(groups(&app), ["fun"]);
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
    fn tapping_the_top_left_label_calls_the_rail_out() {
        let mut app = fixture();
        reader_rows(&mut app, 80, 24);
        let (x, y, w) = (app.chip_area.x, app.chip_area.y, app.chip_area.width);
        assert!(w > 2);
        let tap = |app: &mut App, column: u16, row: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE })
        };
        tap(&mut app, x + w + 3, y); // outside the tag: nothing
        assert!(!app.on_rail());
        tap(&mut app, x + 1, y);
        assert!(app.on_rail() && app.group.is_none());
        // Laid over the list, its rows take the clicks: on a group's row the cursor goes there, a second one goes in
        let rows = reader_rows(&mut app, 80, 24);
        let work = rows.iter().position(|r| r.trim_start().starts_with("work")).unwrap() as u16;
        assert_eq!(app.chip_area.width, 0);
        tap(&mut app, x + 6, work);
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("work"), true));
        tap(&mut app, x + 6, work);
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("work"), false));
        // A click on the list beside it puts it away, the list staying on what it shows
        keys(&mut app, "h");
        reader_rows(&mut app, 80, 24);
        let beside = (app.rail_area.x + app.rail_area.width + 6, app.list_area.y);
        tap(&mut app, beside.0, beside.1);
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("work"), false));
    }

    #[test]
    fn group_and_scope_combine() {
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」
        let mut app = App::new(String::new(), v);
        app.set_group(Some("work".into()));
        assert_eq!(titles(&app), ["跑", "错"]); // only work's open ones
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["跑", "错", "闲"]);
    }

    #[test]
    fn rows_stay_put_when_states_and_activity_change() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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
        keys(&mut app, "QwSw"); // recorded in w before the removal
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
        keys(&mut app, "S1");
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
        keys(&mut app, "Sw");
        assert_eq!(app.current().unwrap().title, "跑"); // back in its original workspace
        // Undo failed: the step is put back, still in the Trash, ^Z can be pressed again
        keys(&mut app, "S1");
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
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let titles: Vec<String> = app.all.iter().map(|s| s.title.clone()).collect();
        // The mark of a row selected with v is green. The cursor's bar stands in the same cell on a screen with no margin, in
        // the accent color
        let marked = |y: usize| buf[(0, y as u16)].symbol() == "▌" && buf[(0, y as u16)].fg == theme::pal().green;
        let ticked = rows.iter().enumerate().filter(|(y, _)| marked(*y));
        ticked.filter_map(|(_, r)| titles.iter().find(|t| r.contains(&format!(" {t} "))).cloned()).collect()
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
        only(&mut app, '5'); // stopped over a week ago: only ■ lists them
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
        assert!(["旧停", "旧退", "旧关"].iter().all(|t| order(&app).contains(&t.to_string())), "{:?}", order(&app));
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
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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
        let mv = |t: &str, from: &str, to: &str, home: &str| Job::Move {
            t: target(t),
            from: from.into(),
            to: to.into(),
            groups: all.clone(),
            home: home.into(),
        };
        assert_eq!(app.job.take(), Some(Job::Many(vec![mv("未读", "fun", "work", "")])));
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
        let job = Job::Many(vec![mv("未读", "fun", "Zeta", ""), mv("跑", "work", "Zeta", "")]);
        app.job_done(&job, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), moved(sessions()));
        assert_eq!(said(&app), "已将 2 个移到 Zeta");
        ctrl(&mut app, 'z');
        let back = app.take_job().unwrap();
        assert_eq!(back, Job::Many(vec![mv("未读", "Zeta", "fun", ""), mv("跑", "Zeta", "work", "")]));
        app.job_done(&back, Outcome::Many(vec![Outcome::Done(String::new()), Outcome::Done(String::new())]), sessions());
        assert_eq!(said(&app), "已撤销移动分组:2 个回到原分组");
    }

    #[test]
    fn one_brought_back_stopped_says_which_key_shows_it_when_only_open_ones_are_listed() {
        let mut app = fixture();
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, binned("闲", "work"), sessions());
        keys(&mut app, "b⏎");
        let job = app.take_job().unwrap();
        let mut fresh = sessions();
        fresh[3].state = State::Stopped; // as it really comes back: stopped, the bringing back counting as activity
        app.job_done(&job, Outcome::Done("已恢复「闲」".into()), fresh);
        assert_eq!(said(&app), "已恢复「闲」 · w 近 7 天");
        press(&mut app, KeyCode::Char('w'));
        assert!(titles(&app).contains(&"闲".to_string()));
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
        to_scope(&mut app, Scope::Week);
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
        // m: moves back to the original group (a group made on the way, and its directory, stay)
        let mv = |from: &str, to: &str, home: &str| Job::Move {
            t: target("闲"),
            from: from.into(),
            to: to.into(),
            groups: vec![],
            home: home.into(),
        };
        let j = undo(&mut app, mv("work", "fun", "/p/work/fun"), with(&|s| s.group = "fun".into()), sessions());
        assert_eq!(j, mv("fun", "work", ""));
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
        app.job_done(&mv("work", "fun", ""), Outcome::Failed("x".into()), sessions());
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
        to_scope(&mut app, Scope::Week);
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
                '⇤' => KeyCode::BackTab,
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
        keys(&mut app, "Sl");
        assert_eq!(titles(&app), ["zsh", "子", "孙", "生活"]);
        // The middle one was sent on its own: it stays where it is, the ones below follow it, not the one further up
        select(&mut app, "子");
        keys(&mut app, "Qh");
        assert_eq!(titles(&app), ["zsh", "生活"]);
        select(&mut app, "生活");
        keys(&mut app, "Q1");
        keys(&mut app, "S1");
        assert_eq!(titles(&app), ["zsh", "工作", "生活", "生活二", "私"]);
        keys(&mut app, "Sh");
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

    /// The whole fun group (on its header) sent to h
    pub(super) fn fun_in_h() -> App {
        let mut app = App::new(String::new(), life_and_work());
        jump_to_group(&mut app, "fun");
        keys(&mut app, "Qh");
        app
    }

    #[test]
    fn one_session_out_of_a_hidden_group_is_remembered_and_the_rest_stay() {
        let mut app = fun_in_h();
        keys(&mut app, "Sh");
        select(&mut app, "私");
        keys(&mut app, "Q1"); // Group in h: sending one back to 1 needs its own record, or it goes back with the group again
        assert_eq!(app.spaces.of("id-私"), Some('1'));
        app.set_sessions(life_and_work());
        assert_eq!(space(&app, "私"), '1');
        // In 1, send the whole fun group (only "私" here) to 2: the group is now recorded as 2, the ones left in h stay in h
        keys(&mut app, "S1");
        jump_to_group(&mut app, "fun");
        keys(&mut app, "Q2");
        assert_eq!(app.spaces.group_of("default", "fun"), Some('2'));
        assert_eq!((space(&app, "私"), space(&app, "生活"), space(&app, "生活二"), space(&app, "子")), ('2', 'h', 'h', 'h'));
        assert_eq!(app.spaces.of("id-私"), None); // Follows the group
        // Sent back to 1: the group is no longer recorded
        keys(&mut app, "S2");
        jump_to_group(&mut app, "fun");
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
            note: None,
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
        // Done: the group being viewed moves along; one undo step recorded
        app.group = Some("work".into());
        let moved = |g: &str| {
            let mut v = life_and_work();
            v[0].group = g.into();
            v
        };
        app.job_done(&job, Outcome::Done("分组 work 改名为 my-job".into()), moved("my-job"));
        assert_eq!(app.group.as_deref(), Some("my-job"));
        assert_eq!(titles(&app), ["工作"]);
        ctrl(&mut app, 'z');
        let back = app.take_job().unwrap();
        assert_eq!(back, edit_group("my-job", "work", None));
        app.job_done(&back, Outcome::Done(String::new()), moved("work"));
        assert_eq!(app.msg.as_ref().unwrap().0, "已撤销修改分组:my-job 改回 work");
        assert_eq!(app.group.as_deref(), Some("work"));
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
        jump_to_group(&mut app, "-");
        press(&mut app, KeyCode::Char('r'));
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
        // The plain tmux sessions section has no group to delete
        jump_to_group(&mut app, "-");
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "这些是普通 tmux 会话,没有可删除的分组");
        jump_to_group(&mut app, "fun");
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Group(g)) if g.group == "fun"));
    }

    #[test]
    fn a_deleted_group_kept_in_h_goes_back_to_h() {
        let mut app = fun_in_h().with_groups(deck_groups);
        keys(&mut app, "Sh");
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
        to_scope(&mut app, Scope::Week); // 7 days: fun[问 未读] work[跑 闲 错]
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
        press(&mut app, KeyCode::Char('W')); // Only fun: no sections, J/K = to the bottom / top
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "未读");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
    }

    #[test]
    fn h_from_a_window_or_sub_session_goes_to_its_session_then_out_of_the_list() {
        let mut v = sessions();
        let idle = v[3].clone(); // 闲 (idle)
        v.push(sub_of(&idle, 2, false, "zsh", State::Idle, 20));
        let mut k = Session { up: Up::Under("闲".into(), "闲".into()), ..mk("子", "work", State::Idle, 10) };
        k.slot.1 = 10;
        v.push(k);
        let mut app = App::new(String::new(), v);
        to_scope(&mut app, Scope::Week);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "zsh", "子", "错"]);
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Left); // ← same as h: window → the session it belongs to
        assert_eq!(at(&app), "闲");
        select(&mut app, "子");
        press(&mut app, KeyCode::Char('h')); // Child session → parent session
        assert_eq!(at(&app), "闲");
        // A top-level session is the list's outermost level. This screen has no rail of its own: one is laid over its left
        press(&mut app, KeyCode::Char('W'));
        press(&mut app, KeyCode::Char('h'));
        assert!(app.on_rail() && app.group.as_deref() == Some("work"));
        press(&mut app, KeyCode::Esc);
        assert!(!app.on_rail() && app.group.as_deref() == Some("work"));
        // Esc inside a group: back to all groups, on the row left there
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.group.as_deref(), at(&app), app.quit), (None, "闲".to_string(), false));
    }

    #[test]
    fn the_reference_key_in_the_list_puts_a_picked_reference_into_the_highlighted_session() {
        let mut app = fixture();
        app.ref_keys = list_keys("prefix b", "C-b");
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        select(&mut app, "跑");
        // After the prefix key the bottom bar says what's next; another key does nothing (not even b's trash once it's a
        // different one)
        app.on_key(ctrl_b);
        shows(&reader_rows(&mut app, 100, 20), &[&caps("b 插入引用 • Esc 取消")]);
        press(&mut app, KeyCode::Char('x'));
        assert!(!reader_rows(&mut app, 100, 20).iter().any(|r| r.contains("插入引用")));
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

    /// The highlight onto a group's header in the list (where j / k stop on the way past it): on the header, not in a session
    pub(super) fn jump_to_group(app: &mut App, group: &str) {
        let pos = app.visible().0.iter().position(|s| s.group == group).unwrap();
        app.select_head(pos);
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
        let mut app = with_blank(); // [fun] 问 未读 [work] 跑 闲 错 [lab]
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, None, 0, false);
        let text: Vec<String> = rows.iter().map(|r| r.lines.concat().iter().map(|(t, _)| t.as_str()).collect()).collect();
        let heads: Vec<&str> = text.iter().map(|t| t.trim_end()).filter(|t| t.contains('▾')).collect();
        // Looks like any other group, with no rows below
        assert_eq!(heads, [" ▾ fun  2", " ▾ work  3", " ▾ lab  0"]);
        assert_eq!(text.len(), 8);
        assert!(text[7].contains("lab"));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(spot(&app), "[lab]");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(spot(&app), "错");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(spot(&app), "[lab]");
        // Numbers go to sessions only
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(spot(&app), "错");
        press(&mut app, KeyCode::Char('j'));
        // Still on it after a refresh; it gets a jump hint letter too
        app.set_sessions(app.all.clone());
        assert_eq!(spot(&app), "[lab]");
        press(&mut app, KeyCode::Char('g'));
        jump_to_group(&mut app, "lab");
        assert_eq!(spot(&app), "[lab]");
        // Session-only keys leave it alone
        press(&mut app, KeyCode::Char('Z'));
        press(&mut app, KeyCode::Char('W'));
        assert!(app.trace.is_none() && app.group.is_none());
        // Not counted by the top bar's counts, the rail's rows or keyword lookup
        assert_eq!(app.groups(), ["fun", "work"]);
        assert!(app.matching("lab").is_empty());
        assert_eq!(app.numbered(6), None);
    }

    #[test]
    fn filters_show_an_empty_group_only_when_its_name_matches() {
        let mut app = with_blank();
        let has = |app: &App| app.visible().0.iter().any(|s| s.blank);
        assert!(has(&app));
        // Listed in every scope (a new group has nothing in it yet), even with nothing else open
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('w'));
            assert!(has(&app), "{:?}", app.scope);
        }
        app.all.retain(|s| s.blank || s.title == "未读");
        app.all.iter_mut().for_each(|s| s.state = if s.blank { s.state } else { State::Stopped });
        app.rebuild();
        assert!(has(&app));
        let mut app = with_blank();
        only(&mut app, '3'); // A state picked
        assert!(!has(&app));
        press(&mut app, KeyCode::Esc);
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
        let mut app = three(); // [fun] 问 [work] 跑 闲
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
        // J / K land on sessions
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(spot(&app), "问");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(spot(&app), "跑");
    }

    #[test]
    fn on_a_group_the_preview_reads_and_shows_the_groups_dir() {
        let mut app = fixture();
        jump_to_group(&mut app, "work");
        let (key, w, turn) = app.glance_request(false).unwrap();
        assert_eq!((key.as_str(), w.dir.as_str(), w.pid, turn), ("group:work", "/p/work", 0, None));
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
    }

    #[test]
    fn a_session_with_nothing_on_screen_shows_its_last_turn() {
        let mut v = sessions();
        v[2].screen = vec!["⏺ 还在跑".into()];
        (v[3].state, v[3].screen) = (State::Stopped, vec![]); // 「闲」: gone from tmux
        let mut app = App::new(String::new(), v);
        to_scope(&mut app, Scope::Week);
        select(&mut app, "闲");
        let (key, _, turn) = app.glance_request(false).unwrap();
        assert_eq!(turn.as_deref(), Some(app.current().unwrap().deck_id.as_str()));
        // Before it's read: nothing in the body yet
        assert!(!reader_rows(&mut app, 120, 20).iter().any(|r| r.contains('❯')));
        let said = diff::Turn {
            prompt: "把计费页改成一行一个账号".into(), reply: "改好了。\n\n- 宽时三个窗口并排\n- 窄时照旧".into()
        };
        app.set_glance(key.clone(), Glance { turn: Some(said), ..Default::default() });
        let rows = reader_rows(&mut app, 120, 20);
        shows(&rows, &["❯ 把计费页改成一行一个账号", "⏺ 改好了。", "宽时三个窗口并排"]);
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap();
        assert!(at("❯ 把计费页") < at("⏺ 改好了"));
        // Never talked to: says so
        app.set_glance(key, Glance { turn: Some(diff::Turn::default()), ..Default::default() });
        shows(&reader_rows(&mut app, 120, 20), &["没有对话"]);
        // Something on screen: the screen as before, no turn read
        select(&mut app, "跑");
        assert_eq!(app.glance_request(false).map(|r| r.2), Some(None));
        shows(&reader_rows(&mut app, 120, 20), &["⏺ 还在跑"]);
        // Running with nothing on screen yet (just starting): no old turn either
        let i = app.all.iter().position(|s| s.title == "跑").unwrap();
        app.all[i].screen.clear();
        app.glance_for = None;
        assert_eq!(app.glance_request(false).map(|r| r.2), Some(None));
    }

    #[test]
    fn a_list_that_does_not_fit_says_how_many_more_above_and_below() {
        let v: Vec<Session> = (0..12).map(|i| mk(&format!("会话{i:02}"), "work", State::Idle, 10 + i)).collect();
        let mut app = App::new(String::new(), v);
        let count = |rows: &[String], arrow: char| {
            rows.iter().find_map(|r| r.trim().strip_prefix(arrow)?.trim().strip_prefix("还有 ")?.strip_suffix(" 个")?.parse::<usize>().ok())
        };
        // At the top: only below, and it counts sessions not lines (the shown ones plus the rest make 12). The cursor's row
        // has its bar in the first cell
        let listed = |rows: &[String]| rows.iter().filter(|r| r.starts_with("  ○ 会话") || r.starts_with("▌ ○ 会话")).count();
        press(&mut app, KeyCode::Char('g'));
        let rows = reader_rows(&mut app, 46, 20);
        let shown = listed(&rows);
        assert_eq!((count(&rows, '↑'), count(&rows, '↓')), (None, Some(12 - shown)), "{rows:#?}");
        // In the middle: both
        for _ in 0..9 {
            press(&mut app, KeyCode::Char('j'));
        }
        let rows = reader_rows(&mut app, 46, 20);
        let (up, down) = (count(&rows, '↑').unwrap(), count(&rows, '↓').unwrap());
        assert_eq!(up + down + listed(&rows), 12, "{rows:#?}");
        // The highlight is never under a hint: it's on a row that shows
        assert!(rows.iter().any(|r| r.contains(&app.current().unwrap().title)), "{rows:#?}");
        // At the bottom: only above
        press(&mut app, KeyCode::Char('G'));
        let rows = reader_rows(&mut app, 46, 20);
        assert!(count(&rows, '↓').is_none() && count(&rows, '↑').is_some(), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("会话11")), "{rows:#?}");
        // All fits: no hints
        assert!(!reader_rows(&mut app, 120, 40).iter().any(|r| r.contains("还有")));
    }

    #[test]
    fn a_jump_that_shows_or_hides_the_line_of_more_above_draws_the_settled_frame_at_once() {
        // Several groups: the blank line before a group's header is where the first layout used to show through (the row above it
        // drawn twice), until the next frame put it right
        let v: Vec<Session> = (0..15).map(|i| mk(&format!("会话{i:02}"), ["a", "b", "c"][(i / 5) as usize], State::Idle, 10 + i)).collect();
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 46, 20);
        for key in ['G', 'g', 'G'] {
            press(&mut app, KeyCode::Char(key));
            let first = reader_rows(&mut app, 46, 20);
            // Drawn again with nothing changed: the same frame
            assert_eq!(first, reader_rows(&mut app, 46, 20), "after {key}");
        }
    }

    #[test]
    fn the_last_turn_keeps_three_lines_of_what_you_said_and_cuts_the_reply_at_the_bottom() {
        let t = diff::Turn {
            prompt: "一\n二\n三\n四".into(),
            reply: (1..=30).map(|i| format!("第 {i} 行")).collect::<Vec<_>>().join("\n\n"),
        };
        let text: Vec<String> = turn_lines(&t, 40, 10).iter().map(|l| l.to_string()).collect();
        assert_eq!(text[..4], ["❯ 一", "  二", "  三…", ""]);
        assert_eq!(text[4], "⏺ 第 1 行");
        assert_eq!(text.len(), 10);
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
        // ⏎ = into the group: only its sessions are listed, and no session was gone into
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.group.as_deref(), app.attach.is_none()), (Some("work"), true));
    }

    /// Whether a group's header saying it has n sessions is among these screen rows: " ▾ work  3", or on the desktop the name
    /// with its number further right
    pub(super) fn has_head(rows: &[String], group: &str, n: usize) -> bool {
        rows.iter().any(|r| {
            let Some(at) = r.find(group) else { return false };
            let lead = r[..at].trim().trim_start_matches('▾');
            lead.is_empty() && r[at + group.len()..].split_whitespace().next() == Some(n.to_string().as_str())
        })
    }

    pub(super) fn layout(app: &App) -> Vec<String> {
        let view = app.visible().0;
        list_rows(&view, app.sectioned(), 46, None, app.who_width(), false)
            .iter()
            .map(|r| match r.pos {
                Some(p) => view[p].title.clone(),
                None => format!("[{}]", r.lines[0][1].0), // Group name on a header row (the pinned block's is 「置顶」, "Pinned")
            })
            .collect()
    }

    #[test]
    fn desktop_has_margins_one_line_a_session_and_no_dim_or_terminal_yellow() {
        let mut v = three().all;
        let at = |v: &[Session], t: &str| v.iter().position(|s| s.title == t).unwrap();
        let (ask, run) = (at(&v, "问"), at(&v, "跑"));
        v[ask].question = "Allow edit?".into();
        v[run].who = "team".into();
        v[run].dir = "/p/somewhere".into();
        let mut app = App::new(String::new(), v);
        let (w, h) = (120u16, 36u16);
        let rows = reader_rows(&mut app, w, h);
        // Margins all around: one blank line top and bottom, 3 cells left and right; one more blank line below the top bar and
        // above the bottom bar
        let hh = h as usize;
        assert!(rows[0].is_empty() && rows[1].starts_with("    全部分组 ") && rows[2].is_empty(), "{rows:#?}");
        // A group's header: its name, and how many sessions ending under their times
        assert!(rows[3].starts_with("      fun ") && rows[3].split('◆').next().unwrap().trim_end().ends_with(" 1"), "{rows:#?}");
        let end = |y: usize, t: &str| rows[y][..rows[y].find(t).unwrap() + t.len()].width();
        assert_eq!(end(3, " 1"), end(4, "11天"), "{rows:#?}");
        assert!(rows[hh - 1].is_empty() && rows[hh - 2].contains(&caps("? 快捷键")) && rows[hh - 3].is_empty(), "{rows:#?}");
        // One session per row: the account right after the (longest) title, no directory in the list; one asking you gets a line
        // below with what it asks, aligned with the title
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        let col = |y: usize, t: &str| rows[y][..rows[y].find(t).unwrap()].width();
        let run = at("2 ● 跑");
        assert!(col(run, "team") - col(run, "跑") <= 8 + 3, "标题栏至少 8 格,后面是标记和空格:{:?}", rows[run]);
        assert!(!rows.iter().any(|r| r.contains("/p/somewhere")), "{rows:#?}");
        let (ask, q) = (at("1 ◆ 问"), at("Allow edit?"));
        assert_eq!((q, col(q, "Allow")), (ask + 1, col(ask, "问")));
        // A group's header: its name alone, no mark before it
        let work = rows.iter().position(|r| r.trim_start().starts_with("work")).unwrap_or_else(|| panic!("{rows:#?}"));
        assert_eq!((work, at("3 ○ 闲")), (q + 2, run + 1));
        assert_eq!(run, q + 3);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        // The column beside the list has no frame: it's off the ground (the list is on it), its title dark, a faint rule
        // under its head
        let pre = app.preview_area;
        assert!(!rows.iter().any(|r| r.contains('╭') || r.contains('│')), "{rows:#?}");
        let title = (pre.x..w).find(|&x| buf[(x, 3)].symbol() == "问").unwrap();
        assert_eq!((buf[(title, 3)].fg, buf[(title, 3)].bg, buf[(pre.x - 1, 3)].bg), (Color::Reset, Color::Reset, theme::pal().back));
        assert_eq!((buf[(pre.x + 2, 5)].symbol(), buf[(pre.x + 2, 5)].fg), ("─", theme::pal().rule));
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
        let mut app = three();
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
        click(&mut app, 3); // Click the header again = into the group
        assert_eq!((app.group.as_deref(), app.attach.is_none()), (Some("work"), true));
        app.set_group(None);
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
        let mut app = three();
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

    /// The bottom bar's text for keys written "⏎ 打开 • s 回话" ("⏎ open • s reply"): each item a key cap (`kit::key_cap`),
    /// `CAP_GAP` between them, from the first key to the last description
    pub(super) fn caps(keys: &str) -> String {
        caps_apart(keys, CAP_GAP)
    }

    /// `caps` with the caps `gap` apart
    fn caps_apart(keys: &str, gap: usize) -> String {
        let mut spans = vec![];
        for (i, item) in keys.split(" • ").enumerate() {
            let (k, d) = item.split_once(' ').unwrap_or((item, ""));
            if i > 0 {
                spans.push(Span::raw(" ".repeat(gap)));
            }
            spans.extend(key_cap(k, d, None));
        }
        Line::from(spans).to_string().trim_start().to_string()
    }

    /// The bar has these keys (written as for `caps`), its caps the usual room apart or closed up for want of it
    pub(super) fn has_caps(bar: &str, keys: &str) -> bool {
        bar.contains(&caps(keys)) || bar.contains(&caps_apart(keys, CAP_GAP - 1))
    }

    /// Every one is on screen (each appearing in some row, in order)
    pub(super) fn shows(rows: &[String], wants: &[&str]) {
        for w in wants {
            assert!(rows.iter().any(|r| r.contains(w)), "缺「{w}」:{rows:#?}");
        }
    }

    /// The current scope on the top bar: its name (the underlined cells, in the accent color) and the count after it (not
    /// underlined, minor)
    fn active_tab(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        let row: Vec<&ratatui::buffer::Cell> = (0..80u16).map(|x| &buf[(x, 0)]).collect();
        let lit: Vec<usize> = (0..80).filter(|&x| row[x].modifier.contains(Modifier::UNDERLINED)).collect();
        let Some(&end) = lit.last() else { return String::new() };
        assert!(lit.iter().all(|&x| row[x].underline_color == theme::pal().accent && row[x].modifier.contains(Modifier::BOLD)));
        // After the name (the second half of a wide last character, then a space): the count
        let at = (end + 1..80).find(|&x| !row[x].symbol().trim().is_empty()).unwrap();
        assert!(row[at].fg == theme::pal().minor && !row[at].modifier.intersects(Modifier::BOLD | Modifier::UNDERLINED), "{:?}", row[at]);
        let count: String = row[at..].iter().map(|c| c.symbol()).take_while(|s| *s != " ").collect();
        format!("{} {count}", lit.iter().map(|&x| row[x].symbol()).collect::<String>())
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
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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
        // h: back to its session; h again: folds them
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "跑");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
    }

    #[test]
    fn a_subagent_that_reported_is_previewed_from_the_top_a_running_one_from_the_bottom() {
        let long = |first: &str| std::iter::once(first.to_string()).chain((1..=80).map(|k| format!("  第 {k} 行"))).collect::<Vec<_>>();
        let mut v = with_agents();
        let run = v.iter_mut().find(|s| s.title == "跑").unwrap();
        run.subagents[0].screen = long("派给它的");
        (run.subagents[2].screen, run.subagents[2].reported) = (long("交回来的"), true);
        let mut app = App::new(String::new(), v);
        to_scope(&mut app, Scope::Week);
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
        to_scope(&mut app, Scope::Week);
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

    /// What's drawn on the selected background, row by row (rows with none left out): the highlight is only a background
    pub(super) fn lit_rows(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let (mut line, mut x) = (String::new(), 0);
                while x < w {
                    let c = &buf[(x, y)];
                    if c.bg == theme::pal().sel {
                        line.push_str(c.symbol());
                    }
                    x += c.cell_width().max(1);
                }
                line.trim().to_string()
            })
            .filter(|l| !l.is_empty())
            .collect()
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

    /// The column beside the list as drawn on a screen this size: its rows from the title's down, each without the blank
    /// cells around it
    pub(super) fn beside(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let p = app.preview_area;
        (p.y..p.y + p.height)
            .map(|y| {
                let (mut line, mut x) = (String::new(), p.x);
                while x < p.x + p.width {
                    let cell = &buf[(x, y)];
                    line.push_str(&ansi::strip_osc8(cell.symbol()));
                    x += cell.cell_width().max(1);
                }
                line.trim().to_string()
            })
            .collect()
    }

    #[test]
    fn preview_holds_still_while_sliding_and_catches_up_after() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
        select(&mut app, "跑"); // The next two down are both sessions, no group header in between
        let border = |app: &mut App| beside(app, 100, 24).remove(0);
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
    fn a_group_header_passed_mid_slide_keeps_the_held_preview() {
        let mut app = fixture(); // [fun] 问 未读 [work] 跑 闲 错
        to_scope(&mut app, Scope::Week);
        select(&mut app, "问");
        let border = |app: &mut App| beside(app, 100, 24).remove(0);
        border(&mut app);
        press(&mut app, KeyCode::Down); // 未读, a single move: switches at once
        assert!(border(&mut app).contains("未读"));
        press(&mut app, KeyCode::Down); // The work header, mid-slide: still 未读, no flash of the group's preview
        assert!(app.on_group_row());
        let b = border(&mut app);
        assert!(b.contains("未读") && !b.contains("▸ work"), "{b}");
        press(&mut app, KeyCode::Down); // 跑
        assert!(border(&mut app).contains("未读"));
        // Stopped on the header: its group's preview
        press(&mut app, KeyCode::Up);
        app.moved = app.moved.map(|(_, gap)| (Instant::now() - SETTLE * 2, gap));
        assert!(border(&mut app).contains("▸ work"));
    }

    #[test]
    fn on_a_local_terminal_the_preview_follows_every_step_of_a_slide() {
        let mut app = fixture();
        app.preview_follows = true;
        to_scope(&mut app, Scope::Week);
        select(&mut app, "问");
        let border = |app: &mut App| beside(app, 100, 24).remove(0);
        border(&mut app);
        for want in ["未读", "▸ work", "跑", "闲"] {
            press(&mut app, KeyCode::Down);
            assert!(border(&mut app).contains(want), "{want}: {}", border(&mut app));
        }
        // Still sliding (follow mode waits for it), only the preview doesn't
        assert!(app.settle_left().is_some());
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
        // Account names in the list are gray like the time; the ▪ in the cell before each is colored by vendor
        select(&mut app, "闲");
        let mut t = Terminal::new(TestBackend::new(46, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let fg_of = |word: &str, before: u16| {
            let y = rows.iter().position(|r| r.contains(word) && !r.contains('·')).unwrap() as u16;
            let x = (0..46u16).find(|&x| buf[(x, y)].symbol() == &word[..1]).unwrap();
            assert_eq!(buf[(x - 1, y)].symbol(), "▪", "{word}: {rows:#?}");
            buf[(x - before, y)].fg
        };
        assert_eq!((fg_of("team", 0), fg_of("team", 1)), (dim().fg.unwrap(), theme::pal().claude));
        assert_eq!((fg_of("alt", 0), fg_of("alt", 1)), (dim().fg.unwrap(), theme::pal().codex));
        // The highlighted row: off the ground across the whole row (the terminal's own background), a bar in the accent
        // color in its first cell, each span keeping its own color; the rows around it are on the ground
        let y = rows.iter().position(|r| r.contains(" 闲")).unwrap() as u16;
        let (mut x, mut bad) = (0u16, vec![]);
        while x < 46 {
            let c = &buf[(x, y)];
            if c.bg != Color::Reset {
                bad.push(x);
            }
            x += c.cell_width().max(1); // The cell after a wide character is a placeholder, covered by the one before in a terminal
        }
        assert_eq!(buf[(2, y)].fg, theme::pal().muted, "图标 ○ 是灰的,不是反色");
        assert!(bad.is_empty(), "{:?} {bad:?}", rows[y as usize]);
        assert_eq!((buf[(0, y)].symbol(), buf[(0, y)].fg), ("▌", theme::pal().accent));
        assert_eq!((buf[(4, y - 1)].bg, buf[(4, y + 1)].bg), (theme::pal().back, theme::pal().back));
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
        fn sheet(&self) -> Option<Vec<crate::sheet::Key>> {
            (self.0 != 1).then(|| vec![crate::sheet::key("f", "说一句"), crate::sheet::key("n", "再开一页")])
        }
    }

    #[test]
    fn every_page_answers_question_mark_with_its_keys_except_where_you_type_or_answer() {
        i18n::with(Lang::Zh, || {
            let sheet = |app: &mut App| {
                let open = app.sheet.is_some();
                let rows = reader_rows(app, 100, 40);
                press(app, KeyCode::Esc);
                (open, rows)
            };
            let mut app = fixture().with_tools(sandbox_tools);
            select(&mut app, "跑");
            // Pages and popups: billing, n, S, F, the trace page
            for (open, want) in [("$", "重新读取"), ("n", "添加账号"), ("S", "重命名"), ("F", "只看这一种"), ("Z", "切换轮次")] {
                keys(&mut app, open);
                press(&mut app, KeyCode::Char('?'));
                let (on, rows) = sheet(&mut app);
                assert!(on && rows.iter().any(|r| r.contains(want)), "{open}: {rows:#?}");
                // Esc closed only the sheet; the page is still there, its own Esc closes it
                assert!(app.modal_open(), "{open}");
                press(&mut app, KeyCode::Esc);
                assert!(!app.modal_open() && !app.quit, "{open}");
            }
            // The trace page's ? also has what its wide bar leaves out: paging, and O for all of them
            keys(&mut app, "Z?");
            shows(&reader_rows(&mut app, 100, 40), &["翻页", "全部全文"]);
            keys(&mut app, "⎋⎋");
            // A key picked in billing's sheet goes to billing: m shows more
            keys(&mut app, "$?m");
            assert!(app.sheet.is_none() && app.bill.is_some());
            shows(&reader_rows(&mut app, 80, 10), &["m 收起"]);
            press(&mut app, KeyCode::Esc);
            // A confirm box: ? is "any other key", it cancels and opens nothing
            press(&mut app, KeyCode::Char('d'));
            assert!(app.confirm.is_some());
            press(&mut app, KeyCode::Char('?'));
            assert!(app.confirm.is_none() && app.sheet.is_none());
            // After q (waiting for the next key): ? cancels, no sheet
            keys(&mut app, "q?");
            assert!(app.pending.is_none() && app.sheet.is_none() && !app.quit);
            // Typing a search: ? is a character
            keys(&mut app, "/?");
            assert!(app.sheet.is_none() && app.filter == "?");
            press(&mut app, KeyCode::Esc);
            // The letter jump: ? ends it and opens the list's keys
            keys(&mut app, "'?");
            assert!(app.jump.is_none() && app.sheet.is_some());
        });
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
