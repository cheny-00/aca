//! The interface: session list + live screen preview of the highlighted session. Refreshes in the background every 2 s; gives up the
//! terminal on attach and resumes drawing after detach.

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

use crate::ansi::{self, Styled};
use crate::create::{self, DirPick, DirRow, Draft, NewReq, Picked, Removal, Src};
use crate::data::{Agent, DEEP, Fork, Session, State, Sub, Up, collect, hangs, sort_sessions};
use crate::diff::{self, Glance, Mode};
use crate::follow::{Follow, Place};
use crate::groups;
use crate::handoff::Source;
use crate::mcp;
use crate::notes::{self, Notes, group_key, is_group_key};
use crate::ops::{self, Job, Outcome, Target};
use crate::overlay::{Flow, Fx, Overlay, Setting};
use crate::pins::Pins;
use crate::reads::Reads;
use crate::spaces::{self, Spaces};
use crate::state::View;
use crate::subagent::{Phase, Subagent};
use crate::theme;
use crate::tools::Tool;
use crate::trash::Trash;
use crate::undo::{self, Undo};
use crate::workdir::Where;

/// Day counts offered for "recent days"; `*` cycles through them in this order (wraps around at the end; all is `a`). One day by default
pub const DAYS: [u64; 3] = [1, 3, 7];

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
/// Output within this many seconds after coming back from viewing doesn't count as "new activity" (detach may redraw once)
pub(crate) const SEEN_SLACK: u64 = 3;

/// How far back sessions are listed. "Open ones" (`w`) is another layer on top (see `App::open`)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Active within recent days (one by default, `*` changes the number of days)
    Recent,
    All,
}

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

/// Layout (where the preview goes). Wide screens (desktop) and narrow ones (phone) remember theirs separately; `v` cycles only through
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

/// Secondary text. Not the terminal's DIM: it fades to unreadable on a light background (see theme.rs)
pub fn dim() -> Style {
    theme::minor()
}

/// Style of the "key" in key hints: bold, the description dim, so the keys catch the eye
pub fn key_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// Separator between key hints
const SEP: &str = " • ";

/// A run of "key description" items separated by •, laid out front to back until room is used up (later ones are less common; what
/// doesn't fit is left out); returns the spans and the width taken
fn key_spans(items: &[(&str, &str)], room: usize) -> (Vec<Span<'static>>, usize) {
    let mut spans = vec![];
    let mut used = 0;
    for (k, d) in items {
        let add = k.width() + 1 + d.width() + if spans.is_empty() { 0 } else { SEP.width() };
        if used + add > room {
            break;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(SEP, theme::faint()));
        }
        spans.push(Span::styled(k.to_string(), key_style()));
        spans.push(Span::styled(format!(" {d}"), dim()));
        used += add;
    }
    (spans, used)
}

/// One bottom bar item: key, description, drop order when it doesn't fit (higher drops first, 0 = never)
type MenuItem<'a> = (&'a str, &'a str, u8);

/// Bottom bar: several groups of keys, • within a group, │ between groups; group number action is the main action, in the accent color.
/// The whole line is centered. When it doesn't fit, drop by priority from low to high until it does
fn menu_bar(groups: &[Vec<MenuItem>], action: Option<usize>, w: usize) -> Line<'static> {
    menu_bar_in(groups, action, w, theme::pal().accent)
}

/// Same as `menu_bar`, with the main action's group in color main (green in multi-select)
fn menu_bar_in(groups: &[Vec<MenuItem>], action: Option<usize>, w: usize, main: Color) -> Line<'static> {
    let gap = if w >= 60 { "  │  " } else { " │ " };
    let mut keep: Vec<Vec<MenuItem>> = groups.to_vec();
    let width = |gs: &[Vec<MenuItem>]| -> usize {
        let gs: Vec<&Vec<MenuItem>> = gs.iter().filter(|g| !g.is_empty()).collect();
        let items: usize = gs.iter().flat_map(|g| g.iter()).map(|(k, d, _)| k.width() + 1 + d.width()).sum();
        let seps: usize = gs.iter().map(|g| g.len().saturating_sub(1) * SEP.width()).sum();
        items + seps + gs.len().saturating_sub(1) * gap.width()
    };
    while width(&keep) > w {
        let worst = keep.iter().flatten().map(|it| it.2).max().unwrap_or(0);
        if worst == 0 {
            break;
        }
        for g in &mut keep {
            if let Some(i) = g.iter().rposition(|it| it.2 == worst) {
                g.remove(i);
                break;
            }
        }
    }
    let mut spans = vec![];
    for (gi, g) in keep.iter().enumerate().filter(|(_, g)| !g.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled(gap, theme::faint()));
        }
        let on = action == Some(gi);
        for (i, (k, d, _)) in g.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(SEP, theme::faint()));
            }
            let st = Style::new().fg(main);
            let (ks, ds) = if on { (st.add_modifier(Modifier::BOLD), st) } else { (key_style(), dim()) };
            spans.push(Span::styled(k.to_string(), ks));
            spans.push(Span::styled(format!(" {d}"), ds));
        }
    }
    Line::from(spans).centered()
}

/// State icons: ● running, ◆ needs you (the screen is asking you), ◐ unread (finished, waiting for your next message), ○ idle,
/// ✕ error, ■ stopped.
/// Needs you and unread get two glyphs (agent-deck uses ◐ for both, told apart only by red vs yellow), so monochrome terminals
/// and color-blind users can tell them apart.
/// Colors are in theme.rs; colored ones are bold, gray ones aren't (the glyph alone already differs from the rest)
pub fn icon(s: State) -> (&'static str, Style) {
    let p = theme::pal();
    let bold = |c| Style::new().fg(c).add_modifier(Modifier::BOLD);
    match s {
        State::Ask => ("◆", bold(p.red)),
        State::Running => ("●", bold(p.green)),
        State::Unread => ("◐", bold(p.amber)),
        State::Idle => ("○", Style::new().fg(p.muted)),
        State::Error => ("✕", bold(p.red)),
        // An error that isn't running: red, not bold — it doesn't count as open; don't steal the eye from errors that are running
        State::Crashed => ("✕", Style::new().fg(p.red)),
        State::Exited | State::Stopped | State::Dead => ("■", Style::new().fg(p.muted)),
    }
}

/// One session's icon: ☾ if aca stopped it for being idle (gray, unobtrusive like ■; `[idle] stop_after`), otherwise by state
pub fn session_icon(s: &Session) -> (&'static str, Style) {
    if reaped(s) {
        return (aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted));
    }
    icon(s.state)
}

/// Icon of a subagent row: running looks the same as a session; finished gets a check mark (sessions have none), errored and stopped
/// ones are drawn like a session's
fn agent_icon(p: Phase) -> (&'static str, Style) {
    let pal = theme::pal();
    match p {
        Phase::Running => icon(State::Running),
        Phase::Done => ("✓", Style::new().fg(pal.muted)),
        Phase::Failed => ("✕", Style::new().fg(pal.red)),
        Phase::Stopped => ("■", Style::new().fg(pal.muted)),
    }
}

/// Key of a subagent row: "session key \u{2} subagent id" (pane rows use \u{1})
const AGENT_SEP: char = '\u{2}';

/// The k-th subagent sent out by session parent, made into a row (only there after `l` unfolds, see `App::lay_agents`). It has no pane
/// and isn't a session: `sub` records whom it hangs under (so it doesn't count as a session, comes and goes with that family, and `h`
/// gets back); the rest is copied from the parent session (group, workspace, pinned follow it);
/// both numbers in the last column hold how long it has run — up to now if running, the total if finished
fn agent_row(parent: &Session, k: usize, a: &Subagent) -> Session {
    let secs = (a.dur.max(0) / 1000) as u64;
    Session {
        name: format!("{}{AGENT_SEP}{}", parent.name, a.id),
        title: a.label(),
        locked: true,
        state: if a.running() { State::Running } else { State::Idle },
        question: String::new(),
        idle_for: secs,
        ran: secs,
        active_at: (a.t0.max(0) / 1000) as u64 + secs,
        attached: false,
        screen: a.screen.clone(),
        noted: false,
        group_noted: false,
        pid: 0,
        deck_id: String::new(),
        info: String::new(),
        worktree: false,
        pane: String::new(),
        sub: Some(Sub { parent: parent.name.clone(), parent_title: parent.title.clone(), win: 0, pane: k as u32, split: false }),
        subagents: vec![],
        subagent: Some(a.clone()),
        due: vec![],
        ..parent.clone()
    }
}

/// Color of the account name: Claude orange, Codex blue (see theme.rs)
pub fn agent_style(a: Agent) -> Style {
    match a {
        Agent::Claude => Style::new().fg(theme::pal().claude),
        Agent::Codex => Style::new().fg(theme::pal().codex),
        Agent::Pi => Style::new().fg(theme::pal().pi),
        Agent::Opencode => Style::new().fg(theme::pal().opencode),
        Agent::Other => dim(),
        Agent::None => Style::new(),
    }
}

/// The account column in the list is at most this wide; longer gets cut
const WHO_MAX: usize = 8;

/// Cut and pad to a display width (Chinese takes two cells)
pub fn clip(s: &str, w: usize) -> String {
    if s.width() <= w {
        return format!("{s}{}", " ".repeat(w - s.width()));
    }
    let (mut out, mut n) = (String::new(), 0);
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if n + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        n += cw;
    }
    format!("{out}…{}", " ".repeat(w.saturating_sub(1 + n)))
}

/// Cut from the left (a path keeps its last few levels: …/fun/app), no padding
fn clip_left(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let (mut tail, mut n) = (vec![], 0);
    for ch in s.chars().rev() {
        let cw = ch.width().unwrap_or(0);
        if n + cw > w.saturating_sub(1) {
            break;
        }
        tail.push(ch);
        n += cw;
    }
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

/// One list item: a session (its index in the visible list) or a section header (pos = None)
pub struct Row {
    pub pos: Option<usize>,
    /// Unfolded group header: index in the visible list of the group's first session (the highlight can stop on headers, see `App::head`)
    pub head: Option<usize>,
    pub lines: Vec<Vec<(String, Style)>>,
}

/// Hint letters for jump mode, same as agent-deck (Vimium's default, home row)
const HINT_CHARS: &str = "sadfjklewcmpgh";

/// Hints for n rows, copying agent-deck's (Vimium's) algorithm: one letter where possible, all single letters up to 13 rows;
/// two-letter ones are inserted among the single letters. The same row count gives exactly agent-deck's hints
fn jump_hints(n: usize) -> Vec<String> {
    if n == 0 {
        return vec![];
    }
    let mut hints = vec![String::new()];
    let mut offset = 0;
    while hints.len() - offset < n || hints.len() == 1 {
        let h = hints[offset].clone();
        offset += 1;
        hints.extend(HINT_CHARS.chars().map(|c| format!("{c}{h}")));
    }
    let mut r = hints[offset..offset + n].to_vec();
    r.sort();
    r.into_iter().map(|s| s.chars().rev().collect()).collect()
}

/// Where the highlight can stop (jumps and j / k all go by this): a session (index in the visible list), or an unfolded group header
/// (recorded as the group's first session)
#[derive(Clone, Copy, Debug, PartialEq)]
enum Jump {
    Row(usize),
    Head(usize),
}

/// Jump targets, ordered top to bottom as on screen: in sections, an unfolded group header comes before its sessions.
/// The pinned block's header doesn't count; a fold row is an item anyway (jumping to it unfolds)
fn jump_targets(view: &[&Session], sections: bool, folds: &Folds) -> Vec<Jump> {
    let mut v = vec![];
    let mut cur = None;
    for (pos, s) in view.iter().enumerate() {
        if sections && cur != Some(section_key(s)) {
            cur = Some(section_key(s));
            if !s.pinned && !folds.contains_key(&s.group) {
                v.push(Jump::Head(pos));
            }
        }
        // An empty group has only its header (a folded one has the fold row)
        if !s.blank || folds.contains_key(&s.group) {
            v.push(Jump::Row(pos));
        }
    }
    v
}

enum HintMatch {
    /// Matches exactly one row
    Hit(usize),
    /// Still the start of some hint; wait for the next letter
    Prefix,
    Miss,
}

fn match_hint(hints: &[String], typed: &str) -> HintMatch {
    let exact = hints.iter().position(|h| h == typed);
    match (exact, hints.iter().filter(|h| h.starts_with(typed)).count()) {
        (Some(p), 1) => HintMatch::Hit(p),
        (_, 0) => HintMatch::Miss,
        _ => HintMatch::Prefix,
    }
}

/// A folded section is a single row in the list (held by the section's first session):
/// group → (how many folded, how many of them asking you)
pub type Folds = HashMap<String, (usize, usize)>;

/// Numbers are written only if the list is this wide (on a phone those 3 cells go to the title; digits still work, and jumps have hint
/// letters)
const NUM_MIN_W: usize = 56;

/// The whole list. In sections, each group gets a header row first (pinned ones at the very top, header 「★ 置顶」 ("★ Pinned"));
/// one row per session, and one asking you gets a row below with what it asks.
/// desk = desktop: always write numbers, a blank line between groups (directory and branch are in the preview header, not in the list).
/// In jump mode (jump = letters already typed), matching sessions show the hint in place of the number, and so does the ▾ of group headers.
/// Shared by the TUI and `aca -l`, so both look the same.
pub fn list_rows(view: &[&Session], sections: bool, width: usize, jump: Option<&str>, folds: &Folds, who_w: usize, desk: bool) -> Vec<Row> {
    let mut rows = vec![];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in view.iter().filter(|s| !s.pinned && s.sub.is_none() && !s.blank) {
        *counts.entry(s.group.as_str()).or_default() += 1;
    }
    let nums = desk || width >= NUM_MIN_W;
    let trees = tree_marks(view, sections);
    // On the desktop the title column is as wide as the longest title, with account and time right after it, not pushed to the far right
    let now = epoch();
    let fit = if desk {
        view.iter()
            .zip(&trees)
            .map(|(s, (t, _))| {
                t.width() + row_loc(s).width() + s.title.width() + agents_badge(s).width() + due_badge(s, now).width() + marks(s).width()
            })
            .max()
            .unwrap_or(0)
    } else {
        usize::MAX
    };
    // Subagent rows get no number: they come and go, and numbering them would make other sessions' numbers jump
    let mut n = 0;
    let numbers: Vec<Option<usize>> = view
        .iter()
        .map(|s| {
            (s.subagent.is_none() && !s.blank).then(|| {
                n += 1;
                n
            })
        })
        .collect();
    let targets = if jump.is_some() { jump_targets(view, sections, folds) } else { vec![] };
    let hints = jump_hints(targets.len());
    let hint_of = |t: Jump| {
        let typed = jump?;
        let h = &hints[targets.iter().position(|&x| x == t)?];
        h.starts_with(typed).then(|| hint_badge(h, typed))
    };
    let mut cur: Option<(bool, &str)> = None;
    for (pos, s) in view.iter().enumerate() {
        let hint = hint_of(Jump::Row(pos));
        let key = section_key(s);
        let new_sec = sections && cur != Some(key);
        if new_sec {
            if desk && cur.is_some() {
                rows.push(Row { pos: None, head: None, lines: vec![vec![]] });
            }
            cur = Some(key);
        }
        if let (false, Some(&(n, asks))) = (s.pinned, folds.get(&s.group)) {
            rows.push(Row { pos: Some(pos), head: None, lines: vec![folded_line(&s.group, n, asks, s.group_noted, width, hint)] });
            continue;
        }
        if new_sec {
            let (line, head) = match s.pinned {
                true => (pinned_line(), None),
                false => {
                    let n = counts.get(s.group.as_str()).copied().unwrap_or(0);
                    (section_line(&s.group, n, s.group_noted, width, hint_of(Jump::Head(pos))), Some(pos))
                }
            };
            rows.push(Row { pos: None, head, lines: vec![line] });
        }
        if s.blank {
            continue;
        }
        let lead = match (hint, nums) {
            (Some(h), true) => [vec![(" ".into(), Style::new())], h, vec![(" ".into(), Style::new())]].concat(),
            (Some(h), false) => h,
            (None, true) => vec![(numbers[pos].map_or_else(|| "    ".to_string(), |n| format!(" {n:>2} ")), dim())],
            (None, false) => vec![("  ".into(), Style::new())],
        };
        let (tree, cont) = &trees[pos];
        rows.push(Row { pos: Some(pos), head: None, lines: row_segments(lead, s, width, who_w, tree, cont, fit) });
    }
    rows
}

/// Section key: the pinned block is one section, the rest go by group
fn section_key(s: &Session) -> (bool, &str) {
    (s.pinned, if s.pinned { "" } else { s.group.as_str() })
}

/// Tree branches are drawn at most this many levels deep (two cells each); deeper ones are drawn level with this one: titles on narrow
/// screens don't get squeezed out
const TREE_DEEP: usize = 4;

/// The branch before each row's icon and the vertical bar on its continuation line (what it asks), joined like the tree command:
/// level 0 = a top-level session; a session's windows / subagents / child sessions are one level deeper ("├ " "└ "), preceded by "│ " for
/// each level above. Any depth works the same, drawn up to TREE_DEEP levels.
/// Hung under the parent session only if the parent is in that chain above (top level if the parent is filtered out / in the pinned block)
fn tree_marks(view: &[&Session], sections: bool) -> Vec<(String, String)> {
    let mut levels = Vec::with_capacity(view.len());
    let mut secs = Vec::with_capacity(view.len());
    // The chain the previous row is in: from the top-level session down, level by level, to its own session
    let mut chain: Vec<&str> = vec![];
    let (mut cur, mut sec): (Option<(bool, &str)>, usize) = (None, 0);
    for s in view {
        if sections && cur != Some(section_key(s)) {
            cur = Some(section_key(s));
            chain.clear();
            sec += 1;
        }
        let at = |n: &str| chain.iter().rposition(|c| *c == n);
        let lvl = match &s.sub {
            // A session: hangs under its parent if the parent is in this chain, otherwise top level
            None => {
                let lvl = s.under().and_then(at).map_or(0, |p| p + 1);
                chain.truncate(lvl);
                chain.push(&s.name);
                lvl
            }
            // A window / subagent: one level deeper than its session
            Some(x) => {
                let lvl = at(&x.parent).map_or(1, |p| p + 1);
                chain.truncate(lvl);
                lvl
            }
        };
        levels.push(lvl.min(TREE_DEEP));
        secs.push(sec);
    }
    // After pos, before going back to a shallower level, is there another one at level lvl (if so draw ├, otherwise └)
    let more =
        |pos: usize, lvl: usize| (pos + 1..view.len()).take_while(|&j| secs[j] == secs[pos] && levels[j] >= lvl).any(|j| levels[j] == lvl);
    // The next row hangs under it
    let kids = |pos: usize| pos + 1 < view.len() && secs[pos + 1] == secs[pos] && levels[pos + 1] > levels[pos];
    let bar = |on: bool| if on { "│ " } else { "  " };
    let fork = |on: bool| if on { "├ " } else { "└ " };
    (0..view.len())
        .map(|pos| match levels[pos] {
            0 => (String::new(), bar(kids(pos)).to_string()),
            lvl => {
                // Vertical bars for the levels above: only where that level has more siblings after it
                let ups: String = (1..lvl).map(|l| bar(more(pos, l))).collect();
                let on = more(pos, lvl);
                (format!("{ups}{}", fork(on)), format!("{ups}{}{}", bar(on), bar(kids(pos))))
            }
        })
        .collect()
}

/// Style of the group name in section headers: bold, uncolored (color is kept for states)
fn group_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A folded section: " ▸ work  3 ◆1 ✎", same as an unfolded header except ▾ becomes ▸; selectable, Enter / o unfolds
fn folded_line(group: &str, n: usize, asks: usize, noted: bool, width: usize, hint: Option<Vec<(String, Style)>>) -> Vec<(String, Style)> {
    let mut v = hint.unwrap_or_else(|| vec![(" ".into(), Style::new())]);
    let count = format!("  {n}");
    let ask = if asks > 0 { format!(" ◆{asks}") } else { String::new() };
    let mark = note_mark(noted);
    let name = clip(group_label(group), width.saturating_sub(4 + count.width() + ask.width() + mark.0.width())).trim_end().to_string();
    v.extend([("▸ ".into(), theme::faint()), (name, group_style()), (count, dim()), (ask, icon(State::Ask).1), mark]);
    v
}

/// The ✎ at the end of group headers and fold rows: the group has a note
fn note_mark(noted: bool) -> (String, Style) {
    (if noted { " ✎" } else { "" }.into(), theme::warn())
}

/// Hint letters: already typed on gray, still to type on yellow (as in agent-deck), padded to two cells
fn hint_badge(hint: &str, typed: &str) -> Vec<(String, Style)> {
    let p = theme::pal();
    let badge = |bg| Style::new().fg(p.on_hint).bg(bg).add_modifier(Modifier::BOLD);
    let n = typed.chars().count();
    let mut v: Vec<(String, Style)> =
        hint.chars().enumerate().map(|(i, c)| (c.to_string(), badge(if i < n { p.hint_typed } else { p.hint }))).collect();
    let len = hint.chars().count();
    if len < 2 {
        v.push((" ".repeat(2 - len), Style::new()));
    }
    v
}

/// Name in the section header: a plain tmux session's group is recorded as "-"
/// Which workspace a session is in: its own assignment → a child session follows its parent (if the parent has none either, keep going
/// up, any depth) → the topmost one's group → 1. Windows follow their own session.
/// mine = false ignores its own assignment, i.e. "where it would be if not recorded"
fn space_of(all: &[Session], sp: &Spaces, s: &Session, mine: bool) -> char {
    let head = |n: &str| all.iter().find(|x| x.sub.is_none() && x.name == n);
    let mut s = head(s.family()).unwrap_or(s);
    if let Some(c) = sp.of(&notes::key(s)).filter(|_| mine) {
        return c;
    }
    for _ in 0..DEEP {
        let Some(p) = s.up.parent().and_then(head) else { break };
        if let Some(c) = sp.of(&notes::key(p)) {
            return c;
        }
        s = p;
    }
    sp.group_of(&s.profile, &s.group).unwrap_or(spaces::HOME)
}

/// Which letter each group gets in the group list: the first not-yet-taken letter of the group name (last level first), else the next one
/// in the jump alphabet;
/// q is kept for closing the list, space is all groups. As long as the group order doesn't change, neither do the letters; once learned,
/// no need to look
fn group_keys(groups: &[String]) -> Vec<char> {
    let mut used = vec!['q'];
    groups
        .iter()
        .map(|g| {
            let name = group_label(g);
            let last = name.rsplit('/').next().unwrap_or(name);
            let free = |c: &char| c.is_ascii_lowercase() && !used.contains(c);
            let c = last
                .chars()
                .chain(name.chars())
                .map(|c| c.to_ascii_lowercase())
                .find(free)
                .or_else(|| HINT_CHARS.chars().chain('a'..='z').find(free));
            let c = c.unwrap_or(' ');
            used.push(c);
            c
        })
        .collect()
}

pub(crate) fn group_label(group: &str) -> &str {
    match group {
        "-" => t!("其他 tmux 会话", "Other tmux sessions"),
        crate::data::UNGROUPED => t!("未分组", "No group"),
        _ => group,
    }
}

/// " ▾ group name  4": the section's count follows the name, then a ✎ if the group has a note. In jump mode the hint letters go before ▾
/// (as on fold rows)
fn section_line(group: &str, n: usize, noted: bool, width: usize, hint: Option<Vec<(String, Style)>>) -> Vec<(String, Style)> {
    let mut v = match hint {
        Some(h) => [h, vec![("▾ ".into(), theme::faint())]].concat(),
        None => vec![(" ▾ ".into(), theme::faint())],
    };
    let count = format!("  {n}");
    let mark = note_mark(noted);
    let name = clip(group_label(group), width.saturating_sub(4 + count.width() + mark.0.width())).trim_end().to_string();
    v.extend([(name, group_style()), (count, dim()), mark]);
    v
}

/// Header of the pinned block
fn pinned_line() -> Vec<(String, Style)> {
    vec![(" ★ ".into(), theme::warn()), (t!("置顶", "Pinned").into(), group_style())]
}

/// One session: (text, style) spans, one row; one asking you gets another row below with what it asks, aligned with the title.
/// lead = the number / indent at the start of the row (or the jump hint)
/// `who_w`: account column width, 0 = not shown (when there isn't a single registered session)
/// `tree` / `cont`: the branch before the icon, the vertical bar on the continuation line (see tree_marks)
/// `fit`: the title column is at most this wide (branch and window number included), usize::MAX = take it all
fn row_segments(
    lead: Vec<(String, Style)>,
    s: &Session,
    width: usize,
    who_w: usize,
    tree: &str,
    cont: &str,
    fit: usize,
) -> Vec<Vec<(String, Style)>> {
    let lead_w: usize = lead.iter().map(|(t, _)| t.width()).sum();
    let (ic, ic_style) = s.subagent.as_ref().map_or_else(|| session_icon(s), |a| agent_icon(a.phase));
    // A subagent row: not a session, no note or account, text in gray
    let own = s.subagent.is_none();
    let flags: String = [(s.noted, '✎'), (s.attached, '⇄')].iter().filter(|f| f.0 && own).map(|f| f.1).collect();
    let flag_style = if s.noted { theme::warn() } else { dim() };
    let when = s.when();
    let age_w = crate::data::age_w();
    let when = format!("{}{when}", " ".repeat(age_w.saturating_sub(when.width())));
    let who_cols = if who_w > 0 { who_w + 1 } else { 0 };
    let tail_w = 2 + 1 + who_cols + age_w + 1;
    let pre = tree;
    let loc = row_loc(s);
    let badge = agents_badge(s);
    let due = due_badge(s, epoch());
    // The minimum of 8 cells only matters on very narrow screens; the width set by the longest title isn't stretched per row any more,
    // or else the row with a long window number would have its time out of line
    let title_w =
        width.saturating_sub(lead_w + pre.width() + 2 + tail_w + loc.width()).max(8).min(fit.saturating_sub(pre.width() + loc.width()));
    let mut first = lead;
    if !pre.is_empty() {
        first.push((pre.into(), theme::faint()));
    }
    first.extend([(ic.to_string(), ic_style), (" ".into(), Style::new()), (loc, dim())]);
    // Three things follow the title: a subagent +N (green), scheduled ◷ (accent color), plugin text (gray); the title comes first,
    // keeping at least two thirds
    let marks = marks(s);
    let tail = badge.width() + due.width() + marks.width();
    if tail == 0 {
        first.push((clip(&s.title, title_w), if own { Style::new() } else { dim() }));
    } else {
        let tw = s.title.width().min(title_w.saturating_sub(tail).max(title_w * 2 / 3));
        let shown = clip(&s.title, tw).trim_end().to_string();
        let room = title_w.saturating_sub(shown.width());
        let badge = clip(&badge, room).trim_end().to_string();
        // If the whole "◷ 08:57" doesn't fit, keep only ◷: a half-cut time would be misread
        let left = room.saturating_sub(badge.width());
        let due = match due.width() {
            w if w <= left => due,
            _ if left >= 2 => format!(" {DUE}"),
            _ => String::new(),
        };
        // Plugin text fills this column (spaces if none), so the times after it line up
        let marks = clip(&marks, room.saturating_sub(badge.width() + due.width()));
        first.extend([
            (shown, if own { Style::new() } else { dim() }),
            (badge, Style::new().fg(theme::pal().green)),
            (due, Style::new().fg(theme::pal().accent)),
            (marks, theme::faint()),
        ]);
    }
    first.extend([
        (format!("{flags:<2}"), flag_style),
        (" ".into(), Style::new()),
        (if who_cols > 0 { format!("{} ", clip(if own { &s.who } else { "" }, who_w)) } else { String::new() }, agent_style(s.agent)),
        (when, dim()),
        (" ".into(), Style::new()),
    ]);
    let mut lines = vec![first];
    if !s.question.is_empty() {
        let q = clip(&s.question, width.saturating_sub(lead_w + cont.width() + 1)).trim_end().to_string();
        lines.push(vec![(" ".repeat(lead_w), Style::new()), (cont.into(), theme::faint()), (q, Style::new().fg(theme::pal().red))]);
    }
    // Stopped for running out of quota / not being logged in: the original message, followed by what you can do
    if can_rehome(s) {
        let hint = t!("  X 换账号", "  X switch account");
        let room = width.saturating_sub(lead_w + cont.width() + 1 + hint.width());
        let e = clip(&s.error, room).trim_end().to_string();
        lines.push(vec![
            (" ".repeat(lead_w), Style::new()),
            (cont.into(), theme::faint()),
            (e, Style::new().fg(theme::pal().red)),
            (hint.into(), theme::accent().add_modifier(Modifier::BOLD)),
        ]);
    }
    lines
}

/// A registered Claude / Codex session stopped because of its account (out of quota, not logged in, unpaid): switching accounts lets it
/// carry on (`X`)
fn can_rehome(s: &Session) -> bool {
    s.state == State::Error
        && s.error_kind == "account"
        && s.sub.is_none()
        && !s.deck_id.is_empty()
        && matches!(s.agent, Agent::Claude | Agent::Codex)
}

/// The short bit before the title: windows / panes get their tmux number (#2 / #2.1); subagents have no number, one started by another
/// subagent gets a ↳
fn row_loc(s: &Session) -> String {
    match (&s.subagent, &s.sub) {
        (Some(a), _) if a.depth > 0 => format!("{}↳ ", "  ".repeat(a.depth as usize - 1)),
        (Some(_), _) | (None, None) => String::new(),
        (None, Some(x)) => format!("{} ", x.loc()),
    }
}

/// The " +2" after a session's title: how many of the subagents it sent out are still running (nothing if none are; unfold with `l` to
/// see which)
fn agents_badge(s: &Session) -> String {
    match s.subagents.iter().filter(|a| a.running()).count() {
        0 => String::new(),
        n => format!(" +{n}"),
    }
}

/// The ◷ after the title: things it scheduled for itself that aren't due yet (see `Session::due`), with the time of the earliest one;
/// +N if there are several
fn due_badge(s: &Session, now: u64) -> String {
    let Some(first) = s.due.first() else { return String::new() };
    let when = first.next.map_or_else(|| t!("定时", "scheduled").to_string(), |t| local_when(t, now));
    let more = if s.due.len() > 1 { format!(" +{}", s.due.len() - 1) } else { String::new() };
    format!(" {DUE} {when}{more}")
}

/// Scheduled for itself and not due yet (after the title in the list, in the preview, in the legend)
const DUE: &str = "◷";

/// Local time: today's as "08:57", other days as "10/16 08:57"
fn local_when(t: u64, now: u64) -> String {
    aca_core::schedule::clock(t, now)
}

/// One line each in the preview: kind, cron, next time, the message handed to it when it's due
fn due_line(d: &aca_core::schedule::Due, now: u64) -> String {
    let when = d.next.map_or_else(|| t!("无法识别时间", "time unknown").to_string(), |t| local_when(t, now));
    let what = match (d.cron.is_empty(), d.recurring) {
        _ if d.inbox => tr!("定时消息 {when}", "scheduled message at {when}"),
        (true, _) => tr!("/loop 下一次 {when}", "/loop next at {when}"),
        (false, true) => tr!("循环定时 {} · 下次 {when}", "recurring {} · next {when}", d.cron),
        (false, false) => tr!("一次性定时 {} · {when}", "one-shot {} · at {when}", d.cron),
    };
    let prompt = d.prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if prompt.is_empty() { format!("{DUE} {what}") } else { format!("{DUE} {what} · {prompt}") }
}

/// Gray text after the title: a fork first gets " fork of X", then what plugins attach (`aca meta set`): " [pr #12] [ci ✓]".
/// Not repeated if the title already contains the original session's name (the default 「X (分叉)」 ("X (fork)"))
fn marks(s: &Session) -> String {
    let fork = match &s.fork {
        Fork::Of(t) if s.sub.is_none() && !s.title.contains(t.as_str()) => tr!(" 分叉自 {t}", " fork of {t}"),
        _ => String::new(),
    };
    s.tokens.iter().fold(fork, |out, (k, v)| out + " " + &mark(k, v))
}

/// A worktree session's branch (the Branch line in info)
fn branch_of(s: &Session) -> Option<&str> {
    s.info.lines().find_map(|l| l.strip_prefix("Branch: ")).filter(|b| !b.is_empty())
}

/// Which session the preview draws: the highlighted one; while swiping, stay on the last one drawn (see SETTLE)
fn preview_name(app: &mut App) -> Option<String> {
    let cur = app.current().map(|s| s.name.clone());
    let keep = app.sliding() && app.shown.as_ref().is_some_and(|n| app.all.iter().any(|s| &s.name == n));
    if !keep {
        app.shown = cur;
    }
    app.shown.clone()
}

/// How long ago: just now / 5 min ago
fn ago(sec: u64) -> String {
    if sec == 0 { t!("刚刚", "just now").into() } else { tr!("{}前", "{} ago", crate::data::dur(sec)) }
}

/// State in words: needs you / running · 12 min / unread · 5 min ago …
fn state_word(s: &Session) -> String {
    if let Some(a) = &s.subagent {
        let took = crate::data::dur((a.dur.max(0) / 1000) as u64);
        return match a.phase {
            Phase::Running => tr!("运行中 · {took}", "running · {took}"),
            Phase::Done => tr!("已完成 · 用时 {took}", "finished · took {took}"),
            Phase::Failed => tr!("出错 · 用时 {took}", "failed · took {took}"),
            Phase::Stopped => tr!("已中断 · 用时 {took}", "interrupted · ran {took}"),
        };
    }
    match s.state {
        State::Ask => t!("等待回应", "needs you").to_string(),
        State::Running if s.ran > 0 => tr!("运行中 · {}", "running · {}", crate::data::dur(s.ran)),
        State::Running => t!("运行中", "running").into(),
        State::Unread => tr!("未读 · {}", "unread · {}", ago(s.idle_for)),
        State::Idle => tr!("空闲 · {}", "idle · {}", ago(s.idle_for)),
        State::Error if s.error_kind == "account" => t!("出错 · 额度 / 登录", "error · quota / login").into(),
        State::Error => t!("出错", "error").into(),
        State::Dead | State::Stopped if s.idle_stopped > 0 => {
            tr!("自动停止 · {}", "auto-stopped · {}", ago(epoch().saturating_sub(s.idle_stopped)))
        }
        State::Stopped => t!("已停止", "stopped").into(),
        State::Exited | State::Dead => tr!("已停止 · {}", "stopped · {}", ago(s.idle_for)),
        State::Crashed => t!("出错 · 已停止", "error · stopped").into(),
    }
}

/// Uncommitted changes: 「  3 个文件 +12 −4」 ("  3 files +12 −4"); nothing when clean
fn stat_spans(r: &diff::Stat) -> Vec<(String, Style)> {
    if r.files == 0 {
        return vec![];
    }
    let (k, w) = (r.files, noun(r.files as u64, "file", "files"));
    let mut v = vec![(tr!("  {k} 个文件", "  {k} {w}"), dim())];
    if r.add > 0 {
        v.push((format!(" +{}", r.add), Style::new().fg(theme::pal().green)));
    }
    if r.del > 0 {
        v.push((format!(" −{}", r.del), Style::new().fg(theme::pal().red)));
    }
    v
}

/// Spans laid one after another into a line, cut beyond w
fn fit_line(pieces: Vec<(String, Style)>, w: usize) -> Line<'static> {
    let (mut v, mut used) = (vec![], 0);
    for (t, st) in pieces {
        if used >= w {
            break;
        }
        let t = if used + t.width() > w { clip(&t, w - used).trim_end().to_string() } else { t };
        used += t.width();
        v.push(Span::styled(t, st));
    }
    Line::from(v)
}

/// Directory entries get columns at most this many cells wide (longer ones are cut)
const LS_COL: usize = 28;

/// Preview when the highlight is on a group: the group's path, branch, changes; which session the path is borrowed from (when the group
/// has no default path); the group's note; then a blank line and what's in the directory
/// (subdirectories first, with /, laid out in columns; what doesn't fit is only counted, dotfiles aren't listed). Before g is read back,
/// only the path and note.
/// The note matters more than the directory: it can take up to half the preview even when not editing; while editing, same as a session's
fn group_pane(sp: &Spot, g: Option<&Glance>, note: Option<&str>, editing: bool, w: usize, h: usize) -> Vec<Line<'static>> {
    let repo = g.and_then(|g| g.repo.as_ref());
    let mut tail: Vec<(String, Style)> = vec![];
    if let Some(r) = repo {
        if !r.branch.is_empty() {
            tail.push((format!("  {}", r.branch), dim()));
        }
        tail.extend(stat_spans(r));
    }
    // Once the branch and changes fit, the path gets the remaining room, cut from the left
    let room = w.saturating_sub(tail.iter().map(|(t, _)| t.width()).sum::<usize>());
    let dir = create::tilde(&sp.dir);
    let dir = if room >= 8 { clip_left(&dir, room) } else { dir };
    let mut out = vec![fit_line([vec![(dir, Style::new().add_modifier(Modifier::BOLD))], tail].concat(), w)];
    if let Some(t) = &sp.from {
        out.push(Line::styled(clip(&tr!("「{t}」的目录", "“{t}”'s directory"), w).trim_end().to_string(), dim()));
    }
    if let Some(text) = note {
        out.extend(group_note(text, editing, w, h, out.len()));
    }
    let Some(f) = g.and_then(|g| g.files.as_ref()) else { return out };
    out.push(Line::default());
    if let Some(e) = &f.err {
        out.push(Line::styled(e.clone(), theme::warn()));
        return out;
    }
    let dir_style = Style::new().fg(theme::pal().accent);
    let items: Vec<(String, Style)> =
        f.dirs.iter().map(|d| (format!("{d}/"), dir_style)).chain(f.files.iter().map(|x| (x.clone(), Style::new()))).collect();
    if items.is_empty() {
        if f.hidden == 0 {
            out.push(Line::styled(t!("空目录", "empty directory"), dim()));
        }
        return out;
    }
    let cw = items.iter().map(|(t, _)| t.width()).max().unwrap_or(1).min(LS_COL) + 2;
    let cols = (w / cw).max(1);
    // The last line is kept for "how many more"
    let room = h.saturating_sub(out.len());
    let need = items.len().div_ceil(cols);
    let rows = if need <= room { need } else { room.saturating_sub(1) };
    for r in 0..rows {
        let row: Vec<(String, Style)> = items.iter().skip(r * cols).take(cols).map(|(t, st)| (clip(t, cw - 2) + "  ", *st)).collect();
        out.push(fit_line(row, w));
    }
    let left = items.len().saturating_sub(rows * cols);
    if left > 0 && out.len() < h {
        out.push(Line::styled(tr!("还有 {left} 个", "{left} more"), dim()));
    }
    out
}

/// The group's note (the preview already has used rows above it): up to half the preview, both while editing and otherwise
fn group_note(text: &str, editing: bool, w: usize, h: usize, used: usize) -> Vec<Line<'static>> {
    let room = (h / 2).saturating_sub(used);
    note_lines(text, editing, w, room, room.saturating_sub(1).max(3))
}

/// The preview's second line from the top: how many of the subagents it sent out are running and how many finished, followed by the keys
/// that work right now (folded: l unfolds; unfolded: h folds). None if it sent none
fn agents_line(s: &Session, open: bool, w: usize) -> Option<Line<'static>> {
    if s.subagent.is_some() || s.subagents.is_empty() {
        return None;
    }
    let run = s.subagents.iter().filter(|a| a.running()).count();
    let rest = s.subagents.len() - run;
    let many = noun(s.subagents.len() as u64, "subagent", "subagents");
    let key = if open { t!("h 收起", "h folds") } else { t!("l 展开", "l unfolds") };
    let text = match (run, rest) {
        (r, 0) => tr!("子代理 {r} 个运行中 · {key}", "{r} {many} running · {key}"),
        (0, d) => tr!("子代理 {d} 个已完成 · {key}", "{d} {many} finished · {key}"),
        (r, d) => tr!("子代理 {r} 个运行中 · {d} 个已完成 · {key}", "subagents: {r} running · {d} finished · {key}"),
    };
    Some(Line::styled(clip(&text, w).trim_end().to_string(), dim()))
}

/// The preview's top lines: state (in the icon's color), changes, directory, branch; the note; then a blank line before the screen content.
/// The note only takes room when written (at most three lines, the rest summed up as how many more lines); while editing it's shown whole,
/// with a cursor, taking at most half the preview (the screen content must always show)
/// g: the directory it really works in, branch, uncommitted changes, read in the background (see `diff::glance`); until read, use the
/// registered directory and branch
fn preview_head(s: &Session, g: Option<&Glance>, note: Option<&str>, editing: bool, w: usize, h: usize) -> Vec<Line<'static>> {
    // A subagent row: state and how many tools it used; the second line says who sent it and how to look closer. It has no directory,
    // changes or note
    if let (Some(a), Some(x)) = (&s.subagent, &s.sub) {
        let (n, steps) = (a.tools, noun(a.tools as u64, "step", "steps"));
        let steps = if n == 0 { String::new() } else { tr!("   {n} 步", "   {n} {steps}") };
        return vec![
            fit_line(vec![(state_word(s), agent_icon(a.phase).1), (steps, dim())], w),
            Line::styled(
                clip(&tr!("「{}」的子代理 · ⏎ 查看记录", "subagent of “{}” · ⏎ opens its record", x.parent_title), w)
                    .trim_end()
                    .to_string(),
                dim(),
            ),
        ];
    }
    let (_, sty) = session_icon(s);
    let word = state_word(s);
    let used = word.width();
    let dir = create::tilde(g.map(|g| g.work.dir.as_str()).filter(|d| !d.is_empty()).unwrap_or(&s.dir));
    let repo = g.and_then(|g| g.repo.as_ref());
    let branch = repo.map(|r| r.branch.clone()).filter(|b| !b.is_empty()).or_else(|| branch_of(s).map(String::from));
    // The diff stats follow the state right away (so they survive when narrow), the directory after them
    let stats = repo.map(stat_spans).unwrap_or_default();
    let mut tail: Vec<(String, Style)> = vec![];
    if let Some(b) = branch {
        tail.push((format!("  {b}"), dim()));
    }
    let width = |v: &[(String, Style)]| v.iter().map(|(t, _)| t.width()).sum::<usize>();
    // Once the rest fits, the directory gets the remaining room, cut from the left (the last level is the most useful: …/app)
    let room = w.saturating_sub(used + width(&stats) + 3 + width(&tail));
    let mut pieces = stats;
    if room >= 5 {
        pieces.push((format!("   {}", clip_left(&dir, room)), Style::new()));
    } else {
        pieces.push((format!("   {dir}"), Style::new()));
    }
    pieces.extend(tail);
    let mut out = vec![fit_line([vec![(word, sty)], pieces].concat(), w)];
    if let Some(x) = &s.sub {
        let kind = if x.split { t!("拆分的窗格", "split pane") } else { t!("窗口", "window") };
        out.push(Line::styled(
            clip(&tr!("「{}」里的{kind} {}", "{kind} {1} in “{0}”", x.parent_title, x.loc()), w).trim_end().to_string(),
            dim(),
        ));
    } else {
        match &s.up {
            Up::Under(_, t) | Up::Apart(_, t) => {
                out.push(Line::styled(clip(&tr!("「{t}」的子会话", "child of “{t}”"), w).trim_end().to_string(), dim()))
            }
            Up::Gone => out.push(Line::styled(t!("子会话 · 父会话已删除", "child session · parent deleted"), dim())),
            Up::Top => {}
        }
        match &s.fork {
            Fork::Of(t) => out.push(Line::styled(clip(&tr!("分叉自「{t}」", "forked from “{t}”"), w).trim_end().to_string(), dim())),
            Fork::Gone => out.push(Line::styled(t!("分叉 · 原会话已删除", "a fork · original deleted"), dim())),
            Fork::No => {}
        }
    }
    // Scheduled for itself and not due yet: one line each (at most three), then a line on what stopping it would do — visible before D, R
    if !s.due.is_empty() {
        let now = epoch();
        for d in s.due.iter().take(3) {
            let l = clip(&due_line(d, now), w).trim_end().to_string();
            out.push(match l.strip_prefix(DUE) {
                Some(rest) => Line::from(vec![Span::styled(DUE, Style::new().fg(theme::pal().accent)), Span::raw(rest.to_string())]),
                None => Line::raw(l),
            });
        }
        if s.due.len() > 3 {
            out.push(Line::styled(tr!("  还有 {} 项", "  and {} more", s.due.len() - 3), dim()));
        }
    }
    // Stopped on an error: the original message (at most two lines); stopped because of the account: offer X
    if s.state == State::Error && !s.error.is_empty() {
        let red = Style::new().fg(theme::pal().red);
        for (i, l) in wrap_width(&s.error, w.saturating_sub(2)).into_iter().take(2).enumerate() {
            out.push(Line::from(vec![Span::styled(if i == 0 { "✕ " } else { "  " }, red), Span::styled(l, red)]));
        }
        if can_rehome(s) {
            out.push(Line::from(vec![button(t!("X 换账号", "X switch account"), theme::pal().accent)]));
        }
    }
    if let Some(text) = note {
        let room = (h / 2).saturating_sub(out.len() + 1).max(1);
        out.extend(note_lines(text, editing, w, room, 3));
    }
    if s.state.gone() {
        out.push(Line::raw(""));
        out.push(Line::styled(t!("⏎ 启动 · d 删除", "⏎ start · d delete"), dim()));
    }
    out.push(Line::raw(""));
    out
}

/// The note in the preview: while editing it's shown whole, with a cursor, taking room lines including the 「✎ 便签」 ("✎ Note") line
/// (if longer, the last lines are kept, where the cursor is);
/// otherwise at most cap lines, the rest summed up as how many more lines
fn note_lines(text: &str, editing: bool, w: usize, room: usize, cap: usize) -> Vec<Line<'static>> {
    let warn = theme::warn();
    let mut body: Vec<String> = text.split('\n').flat_map(|l| wrap_width(l, w.saturating_sub(2))).collect();
    let mut out = vec![];
    if editing {
        out.push(Line::styled(t!("✎ 便签", "✎ Note"), warn.add_modifier(Modifier::BOLD)));
        let room = room.saturating_sub(1).max(1);
        // The cursor is at the end
        match body.last_mut() {
            Some(l) if l.width() < w.saturating_sub(2) => l.push('▏'),
            _ => body.push("▏".into()),
        }
        if body.len() > room {
            let more = body.len() - room + 1;
            body.drain(..more);
            body.insert(0, tr!("… 上面还有 {more} 行", "… {more} more lines above"));
        } else {
            body.resize(room, String::new());
        }
        out.extend(body.into_iter().map(|l| Line::raw(format!("  {l}"))));
    } else {
        let more = body.len().saturating_sub(cap);
        body.truncate(cap);
        for (i, l) in body.into_iter().enumerate() {
            let mark = if i == 0 { Span::styled("✎ ", warn) } else { Span::raw("  ") };
            out.push(Line::from(vec![mark, Span::raw(l)]));
        }
        if more > 0 {
            let w = noun(more as u64, "line", "lines");
            out.push(Line::styled(tr!("  … 还有 {more} 行", "  … {more} more {w}"), dim()));
        }
    }
    out
}

/// Wrap by display width (Chinese takes two cells); an empty line stays an empty line
pub fn wrap_width(s: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        let cur = out.last_mut().unwrap();
        if cur.width() + cw > w.max(1) {
            // English words aren't broken in the middle: when halfway through a word, carry that half to the next line (a word wider than a
            // line is still broken hard)
            let word = |x: char| x.is_ascii_alphanumeric() || matches!(x, '-' | '_' | '.' | ',' | ';' | ':' | ')' | '\'' | '/');
            let half = match cur.rfind(' ') {
                Some(i) if word(c) && i > 0 && cur[i + 1..].chars().all(word) => cur.split_off(i + 1),
                _ => String::new(),
            };
            let kept = cur.trim_end().len();
            cur.truncate(kept);
            if c == ' ' && half.is_empty() {
                out.push(String::new());
                continue;
            }
            out.push(half);
        }
        out.last_mut().unwrap().push(c);
    }
    out
}

/// The preview drops Claude's input box and bottom status bar, keeping only the conversation. Not cut while it waits for you: that box is
/// exactly what to look at.
fn preview_lines(s: &Session) -> Vec<Styled> {
    // A subagent's "screen" is a few lines written from its record (see subagent.rs); there's no input box to cut
    if s.subagent.is_some() {
        return ansi::parse_lines(&s.screen);
    }
    let lines = strip_prompt(ansi::parse_lines(&s.screen), s.state == State::Ask);
    if s.agent == Agent::Claude { quiet_claude(lines) } else { lines }
}

/// Lines on Claude's screen meant for its own terminal, useless in the preview, yet often taking the most visible last lines:
/// hints like "(ctrl+o to expand)" that can't be pressed in the preview; the compacted-while-idle line ("Compacted while idle …",
/// folded into two lines when narrow, the second holding only settings.json); files and skills listed as brought back after compaction
/// ("⎿  Referenced file …"
/// "⎿  Read <path> (12 lines)" "⎿  Skills restored (…)"; the file-reading tool's own result is "⎿  Read 12 lines", without path and
/// parentheses, left alone);
/// a hook's success line ("PostCompact […aca hook claude] completed successfully", mostly aca's own hook)
fn quiet_claude(lines: Vec<Styled>) -> Vec<Styled> {
    const HINTS: [&str; 3] = ["(ctrl+o to expand)", "(ctrl+r to expand)", "(ctrl+o to see full summary)"];
    let mut out: Vec<Styled> = Vec::with_capacity(lines.len());
    let mut idle_note = false;
    for mut l in lines {
        let text = ansi::plain(&l);
        let t = text.trim();
        let attached = t.strip_prefix("⎿").map(str::trim_start).is_some_and(|r| {
            r.starts_with("Referenced file ")
                || r.starts_with("Skills restored (")
                || (r.starts_with("Read ") && r.ends_with(" lines)") && r.contains(" ("))
        });
        let hook_ok = t.ends_with("] completed successfully") && t.contains(" [");
        // "⎿  Tip: Run /install-github-app …": a tip for its own terminal, can't be acted on in the preview
        let tip = t.strip_prefix("⎿").map(str::trim_start).is_some_and(|r| r.starts_with("Tip: "));
        let tail_of_note = std::mem::take(&mut idle_note) && t.ends_with("settings.json") && !t.contains(' ');
        if t.contains("Compacted while idle") {
            idle_note = true;
            continue;
        }
        if attached || hook_ok || tail_of_note || tip {
            continue;
        }
        for h in HINTS {
            // The hint is often a span of its own (gray), with the space before it in the previous span: after removing the hint, also
            // drop that space
            if let Some(i) = l.iter().rposition(|g| g.text.contains(h)) {
                l[i].text = l[i].text.replacen(h, "", 1);
                for j in (0..=i).rev() {
                    let t = l[j].text.trim_end().to_string();
                    let empty = t.is_empty();
                    l[j].text = t;
                    if !empty {
                        break;
                    }
                }
            }
        }
        out.push(l);
    }
    while out.last().is_some_and(|l| ansi::plain(l).trim().is_empty()) {
        out.pop();
    }
    out
}

/// Drop the input box at the bottom and the status bar below it; not while it waits for you (that box is exactly what to look at)
fn strip_prompt(mut styled: Vec<Styled>, asking: bool) -> Vec<Styled> {
    let lines: Vec<String> = styled.iter().map(ansi::plain).collect();
    if !asking {
        // Input box = separator line + "❯ …"; below it there may also be a status bar or a background task / agent list, so search up
        // 20 lines
        // The top border may have the session name embedded: "────── project-main-session ─"
        let is_bar = |l: &str| {
            let t = l.trim();
            t.starts_with("───") && t.chars().filter(|&c| c == '─').count() * 2 > t.chars().count()
        };
        let from = lines.len().saturating_sub(20).max(1);
        if let Some(p) = (from..lines.len()).rev().find(|&i| lines[i].trim_start().starts_with('❯') && is_bar(&lines[i - 1])) {
            styled.truncate(p - 1);
        }
    }
    while styled.last().is_some_and(|l| ansi::plain(l).trim().is_empty()) {
        styled.pop();
    }
    styled
}

/// What to copy (the run loop fetches the content and writes the clipboard)
#[derive(Debug, PartialEq)]
pub enum Clip {
    /// c: the last reply; for a registered session the one the hook recorded, falling back to the text on screen
    Last { profile: String, id: String, title: String, screen: String },
    /// B: path / branch / session id
    Info { title: String, text: String },
    /// A piece of text handed back by a popup (`Fx::copy`): an address or such; the bottom bar says it as is
    Text(String),
}

/// Full-screen reading (`z`): doesn't go into the session, just shows its output, can scroll back through history
struct Reader {
    /// tmux session name
    name: String,
    /// History + screen, input box already removed
    lines: Vec<Styled>,
    /// Rows from the bottom (after wrapping), 0 = follow the latest
    scroll: usize,
    /// Cached wrap result, wrapped at wrap_w; dirty when the content changes
    wrapped: Vec<Styled>,
    wrap_w: usize,
    dirty: bool,
    /// The key help opened by `?`, not a session
    help: bool,
    /// Rows per screen when last drawn (the keys popup pages by this)
    view_h: usize,
    /// `x`: showing changes (git diff), not output
    diff: Option<DiffView>,
    /// The key just pressed in the keys popup: lights up its description row
    hit: Option<crate::help::Hit>,
    /// Showing a plugin's output (a view, or a finished capability run): `r` reruns it
    plugin: Option<crate::caps::Req>,
}

impl Reader {
    /// Which wrapped row is at the very top when a screen holds h rows (correct even if a key changed scroll before the redraw)
    fn top(&self, h: usize) -> usize {
        let total = self.wrapped.len();
        (total - self.scroll.min(total.saturating_sub(h))).saturating_sub(h)
    }
}

/// `$` billing panel (content in `bill`, data in `usage`)
#[derive(Default)]
struct BillView {
    /// Selected period (index into usage::RANGES)
    sel: usize,
    /// Rows from the top
    scroll: usize,
    /// Rows per screen when last drawn (pages by this)
    view_h: usize,
    /// The a account-picker float is open
    pick: Option<BillPick>,
    /// m: the folded-away blocks (usage, weekday × hour, by directory) are laid out
    more: bool,
}

/// The account-picker float: row 0 is 「全部」 ("all"), then each account (see `usage::Report::accounts`)
#[derive(Default)]
struct BillPick {
    sel: usize,
    /// Checked ones (multi-select); none checked means only the highlighted one
    marked: Vec<String>,
    /// v / V pressed, range-selecting: from this row to the highlighted one (as in the main list: Esc settles it into marked; v / V again
    /// starts over from the highlighted row)
    anchor: Option<usize>,
    /// This range is V unchecking
    unpick: bool,
}

impl BillPick {
    /// The chosen accounts: the checked ones plus the range being selected (minus it for an unchecking range), in the float's order
    /// (bigger spenders first)
    fn chosen(&self, accts: &[crate::usage::Row]) -> Vec<String> {
        let range = self.anchor.map(|a| a.min(self.sel)..=a.max(self.sel));
        let ranged = |i: usize| range.as_ref().is_some_and(|r| r.contains(&(i + 1)));
        accts
            .iter()
            .enumerate()
            .filter(|&(i, a)| match self.unpick {
                true => self.marked.contains(&a.name) && !ranged(i),
                false => self.marked.contains(&a.name) || ranged(i),
            })
            .map(|(_, a)| a.name.clone())
            .collect()
    }
}

/// Full-screen reading switched to showing changes (see `diff`). git runs in the background in the run loop and `set_diff` sends the
/// result back when done
struct DiffView {
    mode: Mode,
    /// Where to find the directory it really works in (see `workdir`)
    w: Where,
    /// Index in lines of each file's header, and its position after wrapping (n / p jump)
    files: Vec<usize>,
    rows: Vec<usize>,
    /// Which wrapped row each line of lines starts at (e finds which line is at the top of the screen)
    starts: Vec<usize>,
    /// (file number, line number) for each line, each file's path, repo root: see `diff::Diff`
    at: Vec<Option<(usize, usize)>>,
    paths: Vec<Option<String>>,
    top: String,
    /// No result yet
    loading: bool,
    /// Needs a reread: just opened / mode changed (timed refresh is separate)
    want: bool,
    /// When the session last had activity and when it was read, as of the last read: timed refresh only rereads on new activity /
    /// after GLANCE_EVERY (as `glance_request`)
    seen: Option<(u64, Instant)>,
}

/// A row confirmed with d and hidden ahead of time (removing a group: everything in it is hidden)
#[derive(Debug)]
struct Vanish {
    job: Job,
    /// Removal finished, hidden until this time; None = still removing
    until: Option<Instant>,
    /// Sessions running before the removal (registered ids; restarted after ^Z undoes it, since once hidden they can't be found in the
    /// list)
    running: Vec<String>,
}

impl Vanish {
    /// A session is hidden along with its windows; closing a single pane hides just that pane (by pane id: the session name is the whole
    /// session's). Not hidden when it's the session's first pane being closed: windows can't lose their head; after a refresh, the pane
    /// that becomes the head moves up onto the session's row
    fn hides(&self, s: &Session) -> bool {
        hides(&self.job, s)
    }

    /// This one was undone (the item in the Trash): no need to keep hiding it
    fn undone_by(&self, item: &crate::trash::Item) -> bool {
        undone_by(&self.job, item)
    }
}

fn hides(job: &Job, s: &Session) -> bool {
    match job {
        Job::Remove { pane: Some(p), .. } => s.sub.is_some() && s.pane == *p,
        Job::Remove { t, pane: None } => s.family() == t.name,
        Job::Group(a) => a.covers(&s.profile, &s.group),
        Job::Many(jobs) => jobs.iter().any(|j| hides(j, s)),
        _ => false,
    }
}

fn undone_by(job: &Job, item: &crate::trash::Item) -> bool {
    match job {
        Job::Remove { t, pane: None } => item.group.is_empty() && item.sessions.iter().any(|g| g.id == t.id),
        Job::Group(groups::Act::Delete { group, .. }) => *group == item.group,
        Job::Many(jobs) => jobs.iter().any(|j| undone_by(j, item)),
        _ => false,
    }
}

/// What a job on this session needs (a registered session's key is its id)
fn target_of(s: &Session) -> Target {
    Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() }
}

/// The step of a done job that can be undone (removing sessions into the Trash is recorded separately, see `job_done`). locked: whether
/// the title lock was on before the rename
fn undo_of(job: &Job, locked: bool) -> Option<Undo> {
    match job {
        // A closed pane / plain tmux session: can't be undone, but still recorded as a step (see undo.rs)
        Job::Remove { t, .. } => Some(Undo::Gone { title: t.title.clone() }),
        Job::Stop { t } => Some(Undo::Stop { t: t.clone() }),
        Job::Park { t, p } => Some(Undo::Park { t: t.clone(), p: p.clone() }),
        Job::Move { t, from, groups, .. } => Some(Undo::Move { t: t.clone(), from: from.clone(), groups: groups.clone() }),
        Job::Rename { t, .. } => Some(Undo::Rename { t: t.clone(), from: t.title.clone(), unlock: !locked }),
        Job::Mcp { t, dir, on, off } => Some(Undo::Mcp { t: t.clone(), dir: dir.clone(), on: off.clone(), off: on.clone() }),
        Job::Group(a) => a.undo().map(Undo::Group),
        _ => None,
    }
}

/// Jobs that remove things: hide from the list first, without waiting for the background removal; several can be removed together (see
/// `Vanish`)
fn deleting(job: &Job) -> bool {
    match job {
        Job::Remove { .. } | Job::Group(groups::Act::Delete { .. }) => true,
        Job::Many(jobs) => !jobs.is_empty() && jobs.iter().all(deleting),
        _ => false,
    }
}

/// `Z` session record (see `trace`). Desktop, two panes: the conversation on the left (one block per turn: what you said, what Claude
/// said last, a line of numbers),
/// the selected turn's steps on the right (one block per call, per tool); `→` goes into the right side step by step, `←` comes back.
/// Phones don't fit two panes: the whole screen is the conversation, `⏎` shows this turn's steps full screen. The record is read in the
/// background by the run loop; each later refresh reads only the newly added part.
/// `⏎` on a step that started a subagent goes in to view its own record in the same two panes; `Esc` comes back to carry on
struct TraceView {
    /// tmux session name
    name: String,
    t: Option<crate::trace::Trace>,
    /// Why it couldn't be read
    err: Option<String>,
    /// Needs a background read: just opened / session changed
    want: bool,
    loading: bool,
    /// Unfolded on the left: turns shown in full, the block being copied
    open: HashSet<usize>,
    /// Unfolded on the right: calls (full thinking and text), tools (diffs with context, file content, first 200 lines of output)
    unfold: HashSet<usize>,
    /// How many times unfold changed (the steps laid out on the right are relaid with it)
    unfold_gen: u64,
    /// The conversation on the left
    chat: Blocks,
    /// The steps on the right
    steps: Blocks,
    /// Keys go to the right side (on a phone = the whole screen is steps)
    in_steps: bool,
    /// Whose steps the right side shows (start from the top when it changes)
    steps_of: Option<usize>,
    detail: Option<TraceDetail>,
    /// Whether the last draw was two panes or phone
    wide: bool,
    /// Which column the left pane ends at (which side the mouse scrolls)
    split: u16,
    /// Laid-out conversation: turn → laid-out rows
    chat_cache: HashMap<usize, ChatLaid>,
    /// Laid-out steps (of the item on the right)
    steps_cache: Option<StepsLaid>,
    /// Which palette was used for layout (address of `theme::pal()`): after a theme change, the colors in laid-out rows are stale
    pal: usize,
    /// Which subagent we're in: the tools along the way that started it (empty = the session's own record)
    path: Vec<usize>,
    /// How far each level had been viewed before entering a subagent (to carry on when back)
    saved: Vec<Level>,
    /// Opened from a subagent row in the list: once read, go straight into this subagent (its id)
    goto: Option<String>,
    /// Which level we landed on when entering that way (0 = didn't enter that way): Esc at this level goes straight back to the list,
    /// without backing out level by level
    home: usize,
}

/// What the level looked like before entering a subagent
struct Level {
    open: HashSet<usize>,
    unfold: HashSet<usize>,
    unfold_gen: u64,
    chat: Blocks,
    steps: Blocks,
    in_steps: bool,
    steps_of: Option<usize>,
    chat_cache: HashMap<usize, ChatLaid>,
    steps_cache: Option<StepsLaid>,
}

/// One laid-out turn of the conversation. Laid out uncolored first (to measure height), colored once it's visible (see `hl::plain`)
struct ChatLaid {
    w: usize,
    ver: u64,
    /// Full text (o)
    full: bool,
    lines: Vec<Styled>,
    /// Whether it was cut while folded (if not, o makes no difference)
    clipped: bool,
    /// How many rows are colored (from the top; coloring is fed from the start of a code block, so only the later part can be skipped)
    colored: usize,
}

/// Laid-out steps on the right: which item, width, version, how many times unfold changed, block by block
struct StepsLaid {
    item: usize,
    w: usize,
    ver: u64,
    folds: u64,
    blocks: Vec<StepLaid>,
}

struct StepLaid {
    node: usize,
    lines: Vec<Styled>,
    /// Whether o works
    fold: bool,
    /// How many rows are colored (same as ChatLaid)
    colored: usize,
    /// What the layout depends on (trace::step_sig): when relaying a whole turn, identical ones are reused
    sig: u64,
}

/// One item full screen (⏎)
struct TraceDetail {
    node: usize,
    lines: Vec<crate::trace::Para>,
    wrapped: Vec<Styled>,
    wrap_w: usize,
    /// Rows from the top
    scroll: usize,
    view_h: usize,
}

impl TraceDetail {
    fn new(node: usize) -> Self {
        TraceDetail { node, lines: vec![], wrapped: vec![], wrap_w: 0, scroll: 0, view_h: 0 }
    }
}

/// A column of selectable blocks (the conversation on the left, the steps on the right). Keys go by the last draw: each block is
/// (node, first row, row it ends before)
#[derive(Default)]
struct Blocks {
    items: Vec<(usize, usize, usize)>,
    /// Selected node
    sel: usize,
    /// Row at the very top
    top: usize,
    view_h: usize,
    /// Sticking to the latest: when new ones arrive, select the last block, stuck to the bottom
    follow: bool,
    /// The selected block was just unfolded: after the next layout, show the whole block if possible (head not off screen)
    reveal: bool,
}

impl Blocks {
    fn at(&self) -> Option<usize> {
        self.items.iter().position(|x| x.0 == self.sel)
    }

    fn total(&self) -> usize {
        self.items.last().map_or(0, |x| x.2)
    }

    /// ↑↓: a block taller than the screen scrolls inside first, and moves to the next block only at its end; after the last block,
    /// go on following the latest
    fn go(&mut self, down: bool) {
        let Some(k) = self.at() else { return };
        let h = self.view_h.max(1);
        let (_, s, e) = self.items[k];
        self.follow = false;
        if down {
            if e > self.top + h {
                self.top += 1;
            } else if let Some(&(n, s2, e2)) = self.items.get(k + 1) {
                self.sel = n;
                if e2 > self.top + h {
                    self.top = if e2 - s2 > h { s2 } else { e2 - h };
                }
            } else {
                self.follow = true;
            }
        } else if s < self.top {
            self.top -= 1;
        } else if k > 0 {
            let (n, s2, e2) = self.items[k - 1];
            self.sel = n;
            if s2 < self.top {
                self.top = if e2 - s2 > h { e2 - h } else { s2 };
            }
        }
    }

    /// Scroll the whole column d rows; if the selected one scrolls out, select the first block on screen
    fn scroll(&mut self, d: isize) {
        let h = self.view_h.max(1);
        self.top = self.top.saturating_add_signed(d).min(self.total().saturating_sub(h));
        self.follow = false;
        let top = self.top;
        let gone = self.at().is_none_or(|k| self.items[k].2 <= top || self.items[k].1 >= top + h);
        if gone && let Some(&(n, ..)) = self.items.iter().find(|x| x.2 > top) {
            self.sel = n;
        }
    }

    fn home(&mut self) {
        if let Some(&(n, ..)) = self.items.first() {
            self.sel = n;
            self.top = 0;
            self.follow = false;
        }
    }

    /// Select a block and put its head at the very top
    fn jump(&mut self, n: usize) {
        self.sel = n;
        if let Some(k) = self.at() {
            self.top = self.items[k].1;
        }
        self.follow = false;
    }

    /// Laid out: record where each block is; when following the latest, select the last block, stuck to the bottom; if the selected one
    /// is gone (folded away), select the level above it; if the whole block is off screen, bring its head into view
    fn lay(&mut self, t: &crate::trace::Trace, sizes: &[(usize, usize)], h: usize) {
        let mut y = 0;
        self.items = sizes
            .iter()
            .map(|&(n, len)| {
                y += len;
                (n, y - len, y)
            })
            .collect();
        self.view_h = h;
        if self.follow {
            if let Some(&(n, ..)) = self.items.last() {
                self.sel = n;
            }
            self.top = y.saturating_sub(h);
        }
        let mut x = self.sel;
        while self.items.iter().all(|i| i.0 != x) {
            match t.nodes.get(x).and_then(|n| n.parent) {
                Some(p) => x = p,
                None => {
                    x = self.items.first().map_or(0, |i| i.0);
                    break;
                }
            }
        }
        self.sel = x;
        if let Some(k) = self.at() {
            let (_, s, e) = self.items[k];
            if e <= self.top || s >= self.top + h {
                self.top = s;
            }
            if std::mem::take(&mut self.reveal) && e > self.top + h {
                self.top = s.min(e - h);
            }
        }
        self.top = self.top.min(y.saturating_sub(h));
    }
}

impl TraceView {
    fn new(name: String) -> Self {
        TraceView {
            name,
            t: None,
            err: None,
            want: true,
            loading: true,
            open: HashSet::new(),
            unfold: HashSet::new(),
            unfold_gen: 0,
            chat: Blocks { follow: true, ..Blocks::default() },
            steps: Blocks::default(),
            in_steps: false,
            steps_of: None,
            detail: None,
            wide: true,
            split: 0,
            chat_cache: HashMap::new(),
            steps_cache: None,
            pal: 0,
            path: vec![],
            saved: vec![],
            goto: None,
            home: 0,
        }
    }

    /// The record of the level being viewed (a subagent's, once inside one)
    fn tr(&self) -> Option<&crate::trace::Trace> {
        self.t.as_ref().map(|t| t.at(&self.path))
    }

    /// Whether this level is running (subagent: the session is running and it hasn't finished)
    fn live(&self, live: bool, now: i64) -> bool {
        self.t.as_ref().is_some_and(|t| t.live_at(&self.path, live, now))
    }

    /// ⏎ on a step that started a subagent: go in and view its record (same two panes, at the latest), noting how far this level was
    /// viewed
    fn enter_sub(&mut self, tool: usize) -> bool {
        if self.tr().and_then(|t| t.sub_of(tool)).is_none() {
            return false;
        }
        use std::mem::take;
        let lv = Level {
            open: take(&mut self.open),
            unfold: take(&mut self.unfold),
            unfold_gen: self.unfold_gen,
            chat: take(&mut self.chat),
            steps: take(&mut self.steps),
            in_steps: self.in_steps,
            steps_of: self.steps_of.take(),
            chat_cache: take(&mut self.chat_cache),
            steps_cache: self.steps_cache.take(),
        };
        self.saved.push(lv);
        self.path.push(tool);
        self.chat = Blocks { follow: true, ..Blocks::default() };
        self.in_steps = true;
        self.detail = None;
        true
    }

    /// Back to the level above, carrying on where we were before going in
    fn leave_sub(&mut self) -> bool {
        let Some(lv) = self.saved.pop() else { return false };
        self.path.pop();
        self.open = lv.open;
        self.unfold = lv.unfold;
        self.unfold_gen = lv.unfold_gen;
        self.chat = lv.chat;
        self.steps = lv.steps;
        self.in_steps = lv.in_steps;
        self.steps_of = lv.steps_of;
        self.chat_cache = lv.chat_cache;
        self.steps_cache = lv.steps_cache;
        self.detail = None;
        true
    }

    /// Read: stick to the latest
    fn set(&mut self, t: crate::trace::Trace) {
        let name = std::mem::take(&mut self.name);
        let goto = self.goto.take();
        *self = TraceView::new(name);
        self.loading = false;
        self.want = false;
        self.t = Some(t);
        if let Some(id) = goto {
            self.enter_to(&id);
        }
    }

    /// Go into subagent id: level by level through the tools along the way that started it (nested ones go through the levels above),
    /// each level stopping at the turn that started it, so Esc comes back there
    fn enter_to(&mut self, id: &str) {
        fn path(t: &crate::trace::Trace, id: &str) -> Option<Vec<usize>> {
            (0..t.nodes.len()).find_map(|i| {
                let s = t.sub_of(i)?;
                if s.id == id {
                    return Some(vec![i]);
                }
                let mut p = path(&s.t, id)?;
                p.insert(0, i);
                Some(p)
            })
        }
        let Some(p) = self.t.as_ref().and_then(|t| path(t, id)) else { return };
        for tool in p {
            if let Some(turn) = self.tr().map(|t| crate::trace::turn_of(t, tool)) {
                self.chat.sel = turn;
                self.chat.follow = false;
            }
            if !self.enter_sub(tool) {
                break;
            }
        }
        // Stop on the left (what was sent to it, what it handed back); the right side shows its steps as usual; one Esc goes back to the
        // list
        self.in_steps = false;
        self.home = self.path.len();
    }

    /// Read the newly added part (when following the latest, the draw selects the end by itself)
    fn tail(&mut self) {
        let Some(t) = self.t.as_mut() else { return };
        if !matches!(t.tail(), Ok(true)) {
            return;
        }
        if let Some(d) = self.detail.as_mut() {
            // Full screen is open: the content may have grown (a tool got its result); reread on the next draw
            d.wrap_w = 0;
            d.lines.clear();
        }
    }

    /// The column keys go to
    fn cur(&mut self) -> &mut Blocks {
        if self.in_steps { &mut self.steps } else { &mut self.chat }
    }

    /// Move the left side to the next / previous turn (not stopping at the compaction row)
    fn turn_step(&mut self, next: bool) -> bool {
        let Some(t) = self.t.as_ref().map(|t| t.at(&self.path)) else { return false };
        let Some(k) = self.chat.at() else { return false };
        let turnish = |x: &&(usize, usize, usize)| matches!(t.nodes[x.0].kind, crate::trace::Kind::Turn | crate::trace::Kind::Copied);
        let items = &self.chat.items;
        let to = if next { items.iter().skip(k + 1).find(turnish) } else { items[..k].iter().rev().find(turnish) };
        let Some(&(n, ..)) = to else { return false };
        self.chat.jump(n);
        // The right side switches to this turn's steps, from the top
        self.steps = Blocks { sel: n, ..Blocks::default() };
        self.steps_of = Some(n);
        true
    }

    /// Whether the content can change: in the last turn (still growing) it changes with how far the record is read and whether the
    /// session is running; earlier turns usually don't change,
    /// unless a background subagent they started is still growing, or they were rolled back later
    fn version(&self, i: usize, live: bool) -> u64 {
        let Some(t) = self.tr() else { return 0 };
        let turn = crate::trace::turn_of(t, i);
        let mut top = turn;
        if let Some(p) = t.nodes[top].parent {
            top = p;
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        if Some(top) == t.last_turn() || t.roots.last() == Some(&top) {
            (t.off, live).hash(&mut h);
        }
        let subs = t.subs_in(turn);
        if !subs.is_empty() {
            (live, subs.iter().filter_map(|&k| t.sub_of(k)).map(|s| s.t.grown()).sum::<u64>()).hash(&mut h);
        }
        t.nodes[turn].rolled.hash(&mut h);
        h.finish()
    }
}

/// Things waiting for y to confirm
enum Confirm {
    Remove(Removal),
    /// d on a group: remove the group, with its subgroups and the sessions in it
    Group(GroupRemoval),
    /// why: why ask (running / waiting for your choice); force: not used now
    Restart {
        t: Target,
        why: String,
        force: bool,
    },
    /// D: attached = connected from elsewhere, will be disconnected
    Stop {
        t: Target,
        attached: bool,
    },
    /// D / R when a session has more than one pane: only stop (move to the end of the queue) / restart this pane in place, see park.rs.
    /// restart: it's R; why: why ask (running…); next: when the head pane is stopped, the pane that becomes the head
    Pane {
        t: Target,
        p: crate::park::Pane,
        row: String,
        restart: bool,
        why: String,
        next: Option<String>,
    },
    /// s: send the note to it. pane: which pane to send to; key: the note's key
    Send {
        t: Target,
        pane: String,
        key: String,
        text: String,
    },
    /// A: adopt this plain tmux session. plan: what was recognized and how it will be registered (that's what's shown, and what y
    /// carries out)
    Adopt {
        t: Target,
        plan: Box<aca_core::adopt::Plan>,
    },
    /// d / D / R after v multi-select: on these sessions (in list order). notes: what to watch out for, written in the box (how many
    /// running ones will be interrupted…)
    Many {
        act: Batch,
        ts: Vec<Target>,
        notes: Vec<String>,
    },
}

/// The kinds after v multi-select that ask first
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Batch {
    Remove,
    Stop,
    Restart,
}

impl Batch {
    fn job(self, t: Target) -> Job {
        match self {
            Batch::Remove => Job::Remove { t, pane: None },
            Batch::Stop => Job::Stop { t },
            Batch::Restart => Job::Restart { t, force: false },
        }
    }
}

/// What the confirm box for d on a group says: what gets removed along with it
struct GroupRemoval {
    profile: String,
    group: String,
    /// Number of subgroups
    kids: usize,
    /// Number of sessions (including those in subgroups and archived ones; ones in the Trash don't count)
    sessions: usize,
    /// How many of them are archived
    archived: usize,
    /// How many of them are still running this turn (removing interrupts them)
    busy: usize,
    /// The open ones among them (registered ids; restarted after ^Z undoes it)
    running: Vec<String>,
}

/// Single-line input in the bottom bar
struct Prompt {
    ask: Ask,
    text: String,
    /// Hint on the line below the input box (on error); empty = show the description
    hint: String,
}

enum Ask {
    /// `r`: new title
    Rename(Target),
    /// `G`: new group name; after Enter, go on to fill in the path
    NewGroup,
    /// `e`: the note (key in `notes::key`); orig: the note when opened, if it changed when done, record an undo step
    Note { key: String, title: String, orig: String },
    /// `f`: what to call the forked session (what follows is what a Claude session must check before forking, see `Job::Fork`)
    Fork(Target),
    /// ^R in S: the workspace's new name
    Space(char),
    /// Settings page: the default directory of `^N`
    GroupDir,
    /// `r` on a group: the new group name (may include parents); after Enter, go on to change the default directory
    GroupName(String),
    /// Then fill in its default directory: group is renamed to to, was is the old default directory (empty = none recorded)
    GroupHome { group: String, to: String, was: String },
}

/// q / Q pressed, waiting for the next key
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// q: which workspace to switch to
    Switch,
    /// Q: which workspace to send the highlighted session to
    Send,
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
    /// The config file (config.toml) the settings page opens in the editor: written from the template first if missing
    Config { path: String, template: String },
    /// yazi, opened in the directory the session really works in (looked up when giving up the terminal, as with T).
    /// In a tmux popup it's instead run in a zoomed pane split off in the session (see the run loop); empty `target` = the session isn't
    /// running, so it can only open in place
    Yazi(Split),
    /// A plugin's pane while the interface isn't in tmux: run in the foreground (see caps.rs)
    Plugin(crate::caps::Req),
}

/// A note handed to the editor with ^G
pub struct NoteEdit {
    key: String,
    title: String,
    /// The note before editing started (see `Ask::Note`)
    orig: String,
    pub text: String,
    /// Pressed from the note editing in the bottom bar: go back there to keep writing when done
    from_prompt: bool,
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

/// What `m` needs to pick a group
pub struct GroupPick {
    pub t: Target,
    /// Its current group
    from: String,
    /// Its directory (unchanged by moving, shown at the top of fzf)
    here: String,
    /// fzf candidates: groups in the same directory, then the list's section order, then groups not in the list; its current one excluded
    pub rows: Vec<ops::GroupRow>,
    /// All groups this profile already has (moving into a missing one creates it first)
    groups: Vec<String>,
    /// The others moved along after v multi-select, with their current groups; empty = only move t
    more: Vec<(Target, String)>,
}

impl GroupPick {
    /// The top of fzf says what's being moved
    pub fn what(&self) -> String {
        ops::named(&self.t.title, 1 + self.more.len())
    }
}

/// One row of the `^S` settings page
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SetRow {
    Theme,
    Lang,
    Ime,
    Accounts,
    Tools,
    GroupDir,
    Config,
    /// Added by a feature itself (`App::with_setting`): index in `Settings::extra`
    Extra(usize),
    /// The broken ones (not listed if none)
    Problems,
}

impl SetRow {
    /// The key at the start of the row; the broken row has none, reach it with ↑↓. For rows added by a feature, ask `Settings::key`
    fn key(self) -> Option<char> {
        match self {
            SetRow::Theme => Some('t'),
            SetRow::Lang => Some('l'),
            SetRow::Ime => Some('i'),
            SetRow::Accounts => Some('a'),
            SetRow::Tools => Some('n'),
            SetRow::GroupDir => Some('g'),
            SetRow::Config => Some('e'),
            SetRow::Extra(_) | SetRow::Problems => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            SetRow::Theme => t!("主题", "theme"),
            SetRow::Lang => t!("语言", "language"),
            SetRow::Ime => t!("自动切英文", "auto English input"),
            SetRow::Accounts => t!("账号", "accounts"),
            SetRow::Tools => t!("工具菜单", "tool menu"),
            SetRow::GroupDir => t!("新分组目录", "new group directory"),
            SetRow::Config => "config.toml",
            SetRow::Extra(_) | SetRow::Problems => "",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SetPage {
    Main,
    Problems,
}

/// The `^S` settings page. Values shown are read fresh on open (and reread after a change, or when config.toml was changed), not read
/// from file every frame
#[derive(Clone, Debug)]
struct Settings {
    page: SetPage,
    /// Highlighted row on the main page
    sel: usize,
    theme: String,
    lang: Lang,
    ime: bool,
    group_dir: String,
    bad: Vec<String>,
    accts: Vec<crate::accounts::Account>,
    /// Rows added by features, asked for on open
    extra: Vec<ExtraRow>,
}

/// What a row added by a feature (`overlay::Setting`) looks like when the settings page opens
#[derive(Clone, Debug)]
struct ExtraRow {
    key: char,
    name: &'static str,
    value: Vec<Span<'static>>,
    mark: &'static str,
}

impl Settings {
    fn rows(&self) -> Vec<SetRow> {
        let mut v = vec![SetRow::Theme, SetRow::Lang];
        // Switching input method is only possible on macOS (ime.rs): not listed elsewhere, so there's no 「开」 ("on") that does nothing
        // when pressed
        if cfg!(target_os = "macos") {
            v.push(SetRow::Ime);
        }
        v.extend([SetRow::Accounts, SetRow::Tools, SetRow::GroupDir, SetRow::Config]);
        v.extend((0..self.extra.len()).map(SetRow::Extra));
        if !self.bad.is_empty() {
            v.push(SetRow::Problems);
        }
        v
    }

    fn key(&self, r: SetRow) -> Option<char> {
        match r {
            SetRow::Extra(i) => self.extra.get(i).map(|x| x.key),
            r => r.key(),
        }
    }

    fn name(&self, r: SetRow) -> &'static str {
        match r {
            SetRow::Extra(i) => self.extra.get(i).map_or("", |x| x.name),
            r => r.name(),
        }
    }
}

/// The palette list (「主题」 ("theme") in `^S`, see `open_themes`): all themes (each one's colors prepared up front; moving onto one
/// switches the whole interface to it), which one is highlighted,
/// the one in use before opening (Esc switches back), the one saved in config (marked ●)
struct ThemeMenu {
    list: Vec<(theme::Theme, &'static theme::Pal)>,
    sel: usize,
    was: &'static theme::Pal,
    now: String,
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
    bill_data: Option<crate::usage::Report>,
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
    /// The tool menu opened by `n`, and which entry is highlighted
    tool_menu: Option<(Vec<Tool>, usize)>,
    /// Each account's quota, read when the tool menu opens (both providers), shown after each account in the menu
    tool_quotas: Vec<crate::usage::Quota>,
    /// The tool menu opened by `H` is for picking where to hand off
    handoff: Option<Pass>,
    /// Where `H` recognizes the conversation to hand off (`with_handoff`); recognizes nothing by default, unit tests don't touch real files
    source_loader: fn(&Session) -> Result<Source, String>,
    /// The `X` switch-account box: each account's quota, asking whether it's logged in (`with_rehome`); nothing and no asking by default,
    /// unit tests don't touch real files
    rehome_quotas: fn(Agent) -> Vec<crate::usage::Quota>,
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
        quotas: fn(Agent) -> Vec<crate::usage::Quota>,
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

    /// Cycle: all groups → each group (in section order) → all groups
    fn cycle_group(&mut self, d: isize) {
        let gs = self.groups();
        let cur = self.group.as_ref().and_then(|g| gs.iter().position(|n| n == g)).map_or(0, |p| p as isize + 1);
        let next = (cur + d).rem_euclid(gs.len() as isize + 1);
        self.set_group(if next == 0 { None } else { Some(gs[next as usize - 1].clone()) });
    }

    /// ⇧Tab: switch back to the group just looked at (same as Tab ⏎); again switches back
    fn back_group(&mut self) {
        match self.prev_group.clone() {
            Some(Some(g)) if !self.groups().contains(&g) => {
                self.flash(&tr!("「{}」中已没有会话", "no sessions left in {}", group_label(&g)))
            }
            Some(g) => self.set_group(g),
            None => self.flash(t!("还没有切换过分组", "no group switched yet")),
        }
    }

    /// Switch to showing only this group (None = all groups), remembering the one shown before
    fn set_group(&mut self, to: Option<String>) {
        if to != self.group {
            self.prev_group = Some(std::mem::replace(&mut self.group, to));
        }
        let m = match &self.group {
            Some(g) => tr!("分组:{}", "group: {}", group_label(g)),
            None => t!("全部分组", "all groups").into(),
        };
        self.flash(&m);
        self.rebuild();
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

    /// Rows of the group list: (letter, group); row 0 is all groups (Space)
    fn group_rows(&self) -> Vec<(char, Option<String>)> {
        let gs = self.groups();
        let keys = group_keys(&gs);
        std::iter::once((' ', None)).chain(keys.into_iter().zip(gs.into_iter().map(Some))).collect()
    }

    /// Tab: this workspace's group list, the highlight on the group last looked at (on the current one if never switched)
    /// `F`: the small only-one-state list. The highlight is on the state picked now ("All" if none)
    fn open_only_menu(&mut self) {
        self.digits.clear();
        self.only_menu = Some(self.status.and_then(|p| PICKS.iter().position(|&x| x == p)).map_or(0, |i| i + 1));
    }

    /// In the only-state list: ↑↓ j k pick, ⏎ shows only it (on the one already picked = cancel), the key at the start of a row picks
    /// directly, Space = all; Esc / F / q close
    fn on_only_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.only_menu else { return };
        let n = PICKS.len() + 1;
        let choose = |me: &mut Self, p: Option<Pick>| {
            me.only_menu = None;
            match (p, me.status) {
                (Some(p), _) => me.toggle_status(p),
                (None, Some(cur)) => me.toggle_status(cur),
                (None, None) => {}
            }
        };
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.only_menu = Some((sel + 1) % n),
            KeyCode::Up | KeyCode::Char('k') => self.only_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => choose(self, if sel == 0 { None } else { Some(PICKS[sel - 1]) }),
            KeyCode::Char(' ') => choose(self, None),
            KeyCode::Esc | KeyCode::Char('F' | 'q') => self.only_menu = None,
            KeyCode::Char(c) => {
                if let Some(&p) = PICKS.iter().find(|p| p.label().0 == c) {
                    choose(self, Some(p));
                }
            }
            _ => {}
        }
    }

    /// How many in this workspace are in this state (not counting subagents or
    /// placeholder empty rows; same as the count at the right of the top bar)
    fn pick_count(&self, p: Option<Pick>) -> usize {
        self.all.iter().filter(|s| s.subagent.is_none() && !s.blank && s.space == self.space && p.is_none_or(|p| p.hit(s))).count()
    }

    fn open_group_menu(&mut self) {
        let rows = self.group_rows();
        let at = |g: &Option<String>| rows.iter().position(|(_, x)| x == g);
        self.group_menu = Some(self.prev_group.as_ref().and_then(at).or_else(|| at(&self.group)).unwrap_or(0));
    }

    /// In the group list: ↑↓ pick, ⏎ switches there, typing a letter switches too, Space = all groups; Esc / Tab / q close
    fn on_group_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.group_menu else { return };
        let rows = self.group_rows();
        let n = rows.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let pick = |me: &mut Self, g: Option<String>| {
            me.group_menu = None;
            me.set_group(g);
        };
        match k.code {
            KeyCode::Down => self.group_menu = Some((sel + 1) % n),
            KeyCode::Up => self.group_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => pick(self, rows[sel.min(n - 1)].1.clone()),
            KeyCode::Char(' ') => pick(self, None),
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') => self.group_menu = None,
            KeyCode::Char(c) if !ctrl => {
                if let Some((_, g)) = rows.iter().skip(1).find(|(x, _)| *x == c) {
                    pick(self, g.clone());
                }
            }
            _ => {}
        }
    }

    /// Pinned ones go on top in pin order, the rest sort as usual. Everything is
    /// re-sorted first, so a just-unpinned one goes back to its place
    fn apply_pins(&mut self) {
        self.apply_group_reorders();
        sort_sessions(&mut self.all);
        self.apply_reorders();
        // Child panes go with their own session (pinned together); hanging child sessions that aren't pinned themselves go with the
        // nearest pinned one above (the parent, the one above that…)
        let hangs = hangs(&self.all);
        let rank = |pins: &Pins, s: &Session| pins.rank(s.family()).or_else(|| s.ancestors(&hangs).find_map(|p| pins.rank(p)));
        let space: Vec<char> = self.all.iter().map(|s| space_of(&self.all, &self.spaces, s, true)).collect();
        for (s, c) in self.all.iter_mut().zip(space) {
            s.pinned = rank(&self.pins, s).is_some();
            s.noted = self.notes.get(&notes::key(s)).is_some();
            s.group_noted = self.notes.get(&group_key(&s.group)).is_some();
            s.space = c;
        }
        let pins = &self.pins;
        self.all.sort_by_key(|s| rank(pins, s).unwrap_or(usize::MAX));
    }

    /// ⇧↑ ⇧↓ moves not yet written to the registry are applied to the list first: the whole family (windows, all levels of child
    /// sessions hanging below) moves to right before / right after that session's family.
    /// Until written, every refresh reads the old order, so move it again; moving one already written again changes nothing
    fn apply_reorders(&mut self) {
        let hangs = hangs(&self.all);
        let row = |all: &[Session], t: &Target| -> Option<String> {
            all.iter().find(|s| s.sub.is_none() && s.profile == t.profile && s.deck_id == t.id).map(|s| s.name.clone())
        };
        for job in &self.reorders {
            let Job::Order { t, by, after } = job else { continue };
            let anchor = Target { id: by.clone(), ..t.clone() };
            let (Some(me), Some(it)) = (row(&self.all, t), row(&self.all, &anchor)) else { continue };
            let home = |s: &Session, n: &str| s.family() == n || s.ancestors(&hangs).any(|p| p == n);
            let (mine, mut rest): (Vec<Session>, Vec<Session>) = std::mem::take(&mut self.all).into_iter().partition(|s| home(s, &me));
            let at = if *after { rest.iter().rposition(|s| home(s, &it)).map(|p| p + 1) } else { rest.iter().position(|s| home(s, &it)) };
            let at = at.unwrap_or(rest.len());
            rest.splice(at..at, mine);
            self.all = rest;
        }
    }

    /// ⇧↑ ⇧↓ on a group not yet written to the registry: changes group order (the first field of slot), followed when sorting.
    /// A group moves with its subgroups to right before / right after the `by` family; "Other tmux sessions" always sinks to the bottom.
    /// Moving again changes nothing
    fn apply_group_reorders(&mut self) {
        let within = |g: &str, top: &str| g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'));
        for job in &self.reorders {
            let Job::GroupOrder { group, by, after } = job else { continue };
            let mut gs: Vec<(usize, &str)> =
                self.all.iter().filter(|s| s.slot.0 != usize::MAX).map(|s| (s.slot.0, s.group.as_str())).collect();
            gs.sort();
            gs.dedup();
            let (mine, mut rest): (Vec<&str>, Vec<&str>) = gs.into_iter().map(|g| g.1).partition(|g| within(g, group));
            let at = if *after { rest.iter().rposition(|g| within(g, by)).map(|p| p + 1) } else { rest.iter().position(|g| within(g, by)) };
            let Some(at) = at.filter(|_| !mine.is_empty()) else { continue };
            rest.splice(at..at, mine);
            let rank: HashMap<String, usize> = rest.into_iter().enumerate().map(|(i, g)| (g.to_string(), i)).collect();
            for s in &mut self.all {
                if let Some(&r) = rank.get(&s.group) {
                    s.slot.0 = r;
                }
            }
        }
    }

    /// ⇧↑ ⇧↓: swap with the previous / next session in the same group (child sessions swap under the same parent), written to the
    /// registry's in-group order. Only what's visible in the list counts: ones in between (archived, other workspaces', filtered
    /// out) stay on the other side
    fn reorder(&mut self, d: isize) {
        if let Some(g) = self.group_row().map(String::from) {
            return self.reorder_group(&g, d);
        }
        let Some(p) = self.list.selected().filter(|&p| p < self.view.len()) else { return };
        let s = &self.all[self.view[p]];
        let m = match () {
            _ if s.sub.is_some() => Some(tr!(
                "{} 是窗口,只能按 ⏎ / d / D",
                "{} is a window: only ⏎ / d / D",
                s.sub.as_ref().map(|x| x.loc()).unwrap_or_default()
            )),
            _ if s.deck_id.is_empty() => {
                Some(t!("普通 tmux 会话 · A 接管后才能移动", "an ordinary tmux session · A adopts it, then it can move").into())
            }
            _ if self.pins.rank(&s.name).is_some() => Some(t!("置顶会话不按分组排序", "pinned ones aren't ordered by group").into()),
            _ => None,
        };
        if let Some(m) = m {
            return self.flash(&m);
        }
        // Swappable: same profile, same group, same level (top level with top
        // level, child sessions under the same parent), not pinned itself
        let peer = |o: &Session| {
            o.sub.is_none()
                && !o.deck_id.is_empty()
                && o.profile == s.profile
                && o.group == s.group
                && o.under() == s.under()
                && self.pins.rank(&o.name).is_none()
        };
        // Stop searching once out of this group (child sessions: out of the parent's family); child sessions at any level hanging
        // under a sibling are still in that family, skip over them and keep looking
        let hangs = hangs(&self.all);
        let inside = |o: &Session| match s.under() {
            Some(up) => o.family() == up || o.ancestors(&hangs).any(|p| p == up),
            None => o.group == s.group,
        };
        let mut q = p as isize + d;
        let mut hit = None;
        while let Some(&i) = usize::try_from(q).ok().and_then(|q| self.view.get(q)) {
            let o = &self.all[i];
            if !inside(o) {
                break;
            }
            if peer(o) {
                hit = Some(o.deck_id.clone());
                break;
            }
            q += d;
        }
        let Some(by) = hit else { return };
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        self.queue_reorder(Job::Order { t, by, after: d > 0 });
    }

    /// ⇧↑ ⇧↓ on a group: swap with the previous / next visible sibling group (each with its subgroups), written to the registry's
    /// group order. A subgroup can't leave its parent group; invisible ones in between (other workspaces', filtered out) stay put
    fn reorder_group(&mut self, g: &str, d: isize) {
        if g == "-" {
            return self.flash(t!("普通 tmux 会话区域不能移动", "the ordinary tmux sessions section can't be moved"));
        }
        let secs: Vec<&str> =
            self.section_starts().into_iter().map(|p| &self.all[self.view[p]]).filter(|s| !s.pinned).map(|s| s.group.as_str()).collect();
        let Some(cur) = secs.iter().position(|h| *h == g) else { return };
        let depth = g.split('/').count();
        let parent = |p: &str| p.rsplit_once('/').map(|x| x.0.to_string());
        // Which group section h is at g's level (g itself = h is its subgroup); outside g's parent group = None
        let peer = |h: &str| -> Option<String> {
            let parts: Vec<&str> = h.split('/').collect();
            let at = parts.get(..depth)?.join("/");
            (h != "-" && parent(&at) == parent(g)).then_some(at)
        };
        let mut q = cur as isize + d;
        let mut hit = None;
        while let Some(h) = usize::try_from(q).ok().and_then(|q| secs.get(q)) {
            match peer(h) {
                None => break,
                Some(at) if at == g => {}
                Some(at) => {
                    hit = Some(at);
                    break;
                }
            }
            q += d;
        }
        let Some(by) = hit else { return };
        self.queue_reorder(Job::GroupOrder { group: g.to_string(), by, after: d > 0 });
    }

    /// Record a ⇧↑ ⇧↓; the list moves at once. Consecutive moves of the same one (session / group) keep only the last (only it
    /// moves; the order among the others stays); the one being written can't be changed
    fn queue_reorder(&mut self, job: Job) {
        let same = |a: &Job, b: &Job| match (a, b) {
            (Job::Order { t: x, .. }, Job::Order { t: y, .. }) => x.profile == y.profile && x.id == y.id,
            (Job::GroupOrder { group: x, .. }, Job::GroupOrder { group: y, .. }) => x == y,
            _ => false,
        };
        let busy_front = self.reorder_writing && self.reorders.len() == 1;
        match self.reorders.back_mut() {
            Some(last) if same(last, &job) && !busy_front => *last = job,
            _ => self.reorders.push_back(job),
        }
        self.apply_pins();
        self.rebuild();
    }

    /// p: on a child pane = pin the session it's in; on a child session pinned along with a session above = unpin that session
    /// (the parent, or the pinned level further up)
    fn toggle_pin(&mut self) {
        let Some(s) = self.current() else { return };
        let hangs = hangs(&self.all);
        let lead = s.ancestors(&hangs).find(|p| self.pins.rank(p).is_some()).filter(|_| self.pins.rank(s.family()).is_none());
        let (name, title) = match (&s.sub, lead.and_then(|p| self.all.iter().find(|x| x.sub.is_none() && x.name == p))) {
            (_, Some(p)) => (p.name.clone(), p.title.clone()),
            (Some(x), _) => (x.parent.clone(), x.parent_title.clone()),
            (None, _) => (s.name.clone(), s.title.clone()),
        };
        let key = self.all.iter().find(|x| x.sub.is_none() && x.name == name).map_or_else(|| format!("tmux:{name}"), notes::key);
        let rank = self.pins.rank(&name);
        let m = match self.pins.toggle(&name) {
            Ok(on) => {
                self.remember(Undo::Pin { key, name, title: title.clone(), rank });
                if on {
                    tr!("已置顶「{title}」", "pinned “{title}”")
                } else {
                    tr!("已取消置顶「{title}」", "unpinned “{title}”")
                }
            }
            Err(e) => tr!("置顶保存失败:{e}", "pin not saved: {e}"),
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild(); // the highlight follows the session up
    }

    /// Go into the nearest one in the visible list that needs you (red / yellow `◐`). Ones looked at with no new output since are
    /// skipped — come back with prefix+d and press `i` again for the next one, going through them one by one
    fn jump_waiting(&mut self) {
        let pick = (0..self.view.len())
            .filter(|&p| !self.is_fold_row(p))
            .filter(|&p| {
                let s = &self.all[self.view[p]];
                waiting(s) && self.reads.read_at(&notes::key(s)).is_none_or(|t| s.active_at > t + SEEN_SLACK)
            })
            .min_by_key(|&p| self.all[self.view[p]].idle_for);
        match pick {
            Some(p) => {
                self.select(Some(p));
                self.enter();
            }
            None => self.flash(t!("暂无待处理会话", "nothing needs you")),
        }
    }

    /// Keys in jump mode, like agent-deck: lowercase letters spell a hint, a match goes into that session (a group heading: the
    /// highlight stays on the heading); Esc leaves. Other keys leave jump mode and are handled as usual (returns false)
    fn on_jump_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            KeyCode::Esc => {
                self.jump = None;
                true
            }
            KeyCode::Char(c) if c.is_ascii_lowercase() && !k.modifiers.contains(KeyModifiers::CONTROL) => {
                let targets = self.stops();
                let hints = jump_hints(targets.len());
                let typed = self.jump.get_or_insert_default();
                typed.push(c);
                match match_hint(&hints, typed) {
                    HintMatch::Hit(i) => {
                        self.jump = None;
                        match targets[i] {
                            Jump::Row(p) => {
                                self.select(Some(p));
                                self.enter();
                            }
                            Jump::Head(p) => self.select_head(p),
                        }
                    }
                    HintMatch::Prefix => {}
                    HintMatch::Miss => self.jump = None,
                }
                true
            }
            _ => {
                self.jump = None;
                false
            }
        }
    }

    /// Multi-selecting: some are ticked, or a range is in progress
    fn picking(&self) -> bool {
        !self.picked.is_empty() || self.anchor.is_some()
    }

    /// Whether row p of the visible list can be multi-selected: only registered sessions (not windows, subagents, plain tmux
    /// sessions, empty groups or fold rows)
    fn pickable(&self, p: usize) -> bool {
        let s = &self.all[self.view[p]];
        s.sub.is_none() && !s.blank && !s.deck_id.is_empty() && !self.is_fold_row(p)
    }

    /// The range being selected (positions in the visible list): from the start row to the highlighted row. Start not listed = None.
    /// With the highlight on a group heading `list` selects the session below it: the range stops at the heading, not including it
    fn range(&self) -> Option<std::ops::RangeInclusive<usize>> {
        let a = self.anchor.as_deref()?;
        let at = self.view.iter().position(|&i| self.all[i].sub.is_none() && self.all[i].deck_id == a)?;
        let cur = self.list.selected()?;
        Some(if self.head.is_some() && at < cur { at..=cur - 1 } else { at.min(cur)..=at.max(cur) })
    }

    /// Selected sessions (registered ids): ticked plus the range in progress (an unticking range: minus), in list order;
    /// ticked ones not listed now (filtered out, folded) come last
    fn chosen(&self) -> Vec<String> {
        let id = |p: usize| &self.all[self.view[p]].deck_id;
        let ranged: Vec<&String> = self.range().map_or(vec![], |r| r.filter(|&p| self.pickable(p)).map(id).collect());
        let on = |id: &String| match self.unpick {
            true => self.picked.contains(id) && !ranged.contains(&id),
            false => self.picked.contains(id) || ranged.contains(&id),
        };
        let mut v: Vec<String> = (0..self.view.len()).filter(|&p| self.pickable(p)).map(id).filter(|id| on(id)).cloned().collect();
        for id in &self.picked {
            if on(id) && !v.contains(id) {
                v.push(id.clone());
            }
        }
        v
    }

    /// The selected sessions themselves, in `chosen` order
    fn chosen_rows(&self) -> Vec<&Session> {
        self.chosen().iter().filter_map(|id| self.all.iter().find(|s| s.sub.is_none() && s.deck_id == *id)).collect()
    }

    fn drop_picks(&mut self) {
        self.picked.clear();
        self.anchor = None;
        self.unpick = false;
    }

    /// Only one selected and it's listed: the highlight moves to it, multi-select ends, and the key acts as if pressed on it
    /// (confirm boxes and messages as usual)
    fn only_one(&mut self) -> bool {
        let ids = self.chosen();
        let [id] = &ids[..] else { return false };
        let Some(p) = self.view.iter().position(|&i| self.all[i].sub.is_none() && self.all[i].deck_id == *id) else { return false };
        self.select(Some(p));
        self.drop_picks();
        true
    }

    /// v: range select from the highlighted row (↑↓ extends it); V (unpick) unticks a range. v again while ranging: the unfixed
    /// range is dropped and starts again from this row (like yazi). On a group heading it starts from the group's first session
    fn start_pick(&mut self, unpick: bool) {
        if self.on_fold_row() {
            return self.flash(t!("先展开再多选", "unfold first, then select"));
        }
        let Some(p) = self.list.selected() else { return };
        if !self.pickable(p) {
            let s = &self.all[self.view[p]];
            let m = match () {
                _ if s.blank => t!("空分组里没有会话", "the group has no sessions"),
                _ if s.sub.is_some() => t!("只能多选会话", "only sessions can be selected"),
                _ => t!("普通 tmux 会话 · A 接管后才能多选", "can't select an ordinary tmux session · A adopts it"),
            };
            return self.flash(m);
        }
        self.unpick = unpick;
        self.anchor = Some(self.all[self.view[p]].deck_id.clone());
    }

    /// Keys while multi-selecting (shown in the bottom bar), like yazi: v / V start a new range from this row; Esc while ranging
    /// fixes the range, otherwise unselects all; Space ticks / unticks one and moves down (also while ranging, extending the range).
    /// Returns false = not handled here, as usual (move highlight, view, search, ⏎…)
    fn on_pick_key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let ranging = self.anchor.is_some();
        match k.code {
            KeyCode::Esc if ranging => {
                self.picked = self.chosen();
                self.anchor = None;
            }
            KeyCode::Esc => self.picked.clear(),
            KeyCode::Char(c @ ('v' | 'V')) => self.start_pick(c == 'V'),
            KeyCode::Char(' ') => self.tick(),
            KeyCode::Char('d') => self.ask_many(Batch::Remove),
            KeyCode::Char('D') => self.ask_many(Batch::Stop),
            KeyCode::Char('R') => self.ask_many(Batch::Restart),
            KeyCode::Char('m') => self.ask_move_many(),
            KeyCode::Char('p') => self.pin_many(),
            KeyCode::Char('u') => self.read_many(),
            KeyCode::Char('Q') => self.pending = Some(Pending::Send),
            _ => return false,
        }
        true
    }

    /// Space: tick / untick the highlighted session and move down a row. On a group heading: tick the group's listed sessions
    /// together (all ticked = untick all)
    fn tick(&mut self) {
        let Some(p) = self.list.selected() else { return };
        let ids: Vec<String> = match &self.head {
            Some(g) => (0..self.view.len())
                .filter(|&q| self.pickable(q) && !self.all[self.view[q]].pinned && self.all[self.view[q]].group == *g)
                .map(|q| self.all[self.view[q]].deck_id.clone())
                .collect(),
            None if self.pickable(p) => vec![self.all[self.view[p]].deck_id.clone()],
            None => vec![],
        };
        if ids.iter().all(|id| self.picked.contains(id)) {
            self.picked.retain(|id| !ids.contains(id));
        } else {
            for id in ids {
                if !self.picked.contains(&id) {
                    self.picked.push(id);
                }
            }
        }
        self.move_by(1);
    }

    /// d / D / R after multi-select: ask first, then do them together (restart doesn't ask when none is running, same as R).
    /// With only one selected it's like pressing on it
    fn ask_many(&mut self, act: Batch) {
        // Other deletes still running don't matter, delete on (same as d); other work must wait
        let busy = match act {
            Batch::Remove => self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)),
            _ => self.doing(),
        };
        if let Some(b) = busy {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        if self.only_one() {
            return match act {
                Batch::Remove => self.ask_remove(),
                Batch::Stop => self.ask_stop(),
                Batch::Restart => self.ask_restart(),
            };
        }
        let ids = self.chosen();
        let rows = self.chosen_rows();
        let count = |f: &dyn Fn(&Session) -> bool| rows.iter().filter(|s| f(s)).count();
        let busy = count(&|s| matches!(s.state, State::Running | State::Ask));
        let attached = count(&|s| s.attached);
        let mut notes = vec![];
        let mut add = |n: usize, m: String| {
            if n > 0 {
                notes.push(m);
            }
        };
        let (ts, ask): (Vec<Target>, bool) = match act {
            Batch::Remove => {
                let names: Vec<&str> = rows.iter().map(|s| s.name.as_str()).collect();
                let windows = self.all.iter().filter(|x| x.sub.is_some() && x.subagent.is_none() && names.contains(&x.family())).count();
                let kids = self
                    .all
                    .iter()
                    .filter(|x| x.sub.is_none() && !ids.contains(&x.deck_id) && x.up.parent().is_some_and(|p| names.contains(&p)))
                    .count();
                let worktrees = count(&|s| s.worktree);
                add(busy, tr!("{busy} 个正在运行或等待回应，将被中断", "{busy} running or waiting for you; this interrupts them"));
                add(attached, tr!("⇄ {attached} 个在别处打开，将被断开", "⇄ {attached} open elsewhere; they will be disconnected"));
                add(worktrees, tr!("同时删除 {worktrees} 个 worktree 目录", "{worktrees} worktree directories are deleted too"));
                add(windows, tr!("同时关闭其中的 {windows} 个窗口 / 窗格", "{windows} windows / panes opened inside close too"));
                add(kids, tr!("{kids} 个子会话未选中，不会删除", "{kids} child sessions aren't selected and won't be deleted"));
                (rows.iter().map(|s| target_of(s)).collect(), true)
            }
            Batch::Stop => {
                let live: Vec<&&Session> = rows.iter().filter(|s| !s.state.gone() && s.state != State::Dead).collect();
                if live.is_empty() {
                    return self.flash(t!("均已停止", "they're all stopped already"));
                }
                let attached = live.iter().filter(|s| s.attached).count();
                let off = rows.len() - live.len();
                add(attached, tr!("⇄ {attached} 个在别处打开，将被断开", "⇄ {attached} open elsewhere; they will be disconnected"));
                add(off, tr!("另有 {off} 个已停止", "{off} more are stopped already"));
                (live.iter().map(|s| target_of(s)).collect(), true)
            }
            Batch::Restart => {
                add(busy, tr!("{busy} 个正在运行或等待回应，将被中断", "{busy} running or waiting for you; this interrupts them"));
                (rows.iter().map(|s| target_of(s)).collect(), busy > 0)
            }
        };
        if ts.is_empty() {
            return self.flash(t!("尚未选中会话", "nothing selected"));
        }
        if ask {
            self.confirm = Some(Confirm::Many { act, ts, notes });
        } else {
            self.run_many(act, ts);
        }
    }

    /// Confirmed d / D / R: run together in the background, multi-select cleared. Deleted ones are hidden from the list first
    /// (same as d); ones running before deletion are noted, to start again after ^Z undo
    fn run_many(&mut self, act: Batch, ts: Vec<Target>) {
        self.drop_picks();
        let live = |t: &Target| self.all.iter().any(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile && !s.state.gone());
        let running: Vec<String> = ts.iter().filter(|t| live(t)).map(|t| t.id.clone()).collect();
        let job = Job::Many(ts.into_iter().map(|t| act.job(t)).collect());
        if act == Batch::Remove {
            self.vanish.push(Vanish { job: job.clone(), until: None, running });
            self.job = Some(job);
            self.drop_vanished();
            self.apply_pins();
            self.rebuild();
        } else {
            self.job = Some(job);
        }
    }

    /// `[idle] remove_after`: ones stopped too long (see `stale`) go to the trash like a v multi-select delete, as one entry,
    /// ^Z brings them back, the bottom bar says it was automatic. `archived` is archived sessions (not listed) and their last
    /// activity time. Skips this round while busy or multi-selecting
    fn sweep(&mut self, after: u64, now: u64, archived: Vec<(Target, u64)>) {
        if self.job.is_some() || self.doing().is_some() || !self.picked.is_empty() || self.anchor.is_some() {
            return;
        }
        let ts = self.stale(after, now, archived);
        if ts.is_empty() {
            return;
        }
        let job = Job::Many(ts.into_iter().map(|t| Job::Remove { t, pane: None }).collect());
        self.swept = Some((job.clone(), after));
        self.vanish.push(Vanish { job: job.clone(), until: None, running: vec![] });
        self.job = Some(job);
        self.drop_vanished();
        self.apply_pins();
        self.rebuild();
    }

    /// Auto-deletable: registered sessions stopped (■ ☾, closed ✕) with no activity for over `after` seconds, not pinned, no note,
    /// nobody attached, with no child session hanging under them and no extra window inside still running; plus archived ones
    /// equally long idle, with no note, not pinned (archiving doesn't clear pins; restored they're back in place). Not ones in the trash
    fn stale(&self, after: u64, now: u64, archived: Vec<(Target, u64)>) -> Vec<Target> {
        let hangs = hangs(&self.all);
        let needed: HashSet<&str> =
            self.all.iter().filter(|s| open(s)).flat_map(|s| s.ancestors(&hangs).chain(s.sub.as_ref().map(|x| x.parent.as_str()))).collect();
        let old = |at: u64| now.saturating_sub(at) >= after;
        let listed = self.all.iter().filter(|s| {
            s.sub.is_none()
                && s.subagent.is_none()
                && !s.blank
                && !s.deck_id.is_empty()
                && s.profile == crate::data::REGISTERED
                && !open(s)
                && !s.pinned
                && !s.noted
                && !s.attached
                && old(s.active_at)
                && !needed.contains(s.name.as_str())
        });
        let hidden = archived.into_iter().filter(|(t, at)| {
            old(*at) && self.notes.get(&t.id).is_none() && self.pins.rank(&t.name).is_none() && !self.trash.has(&t.profile, &t.id)
        });
        listed.map(target_of).chain(hidden.map(|(t, _)| t)).collect()
    }

    /// m after multi-select: pick a group in fzf, the selected ones move into it together (ones already in it stay)
    fn ask_move_many(&mut self) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        if self.only_one() {
            return self.ask_move();
        }
        let rows = self.chosen_rows();
        let dirs: HashSet<&str> = rows.iter().map(|s| s.dir.as_str()).collect();
        let here = if dirs.len() == 1 { rows[0].dir.clone() } else { String::new() };
        let mut all: Vec<(Target, String)> = rows.iter().map(|s| (target_of(s), s.group.clone())).collect();
        if all.is_empty() {
            return self.flash(t!("尚未选中会话", "nothing selected"));
        }
        let (t, from) = all.remove(0);
        self.pick_group = Some(self.group_pick(t, from, here, all));
    }

    /// p after multi-select: pin all if any is unpinned, unpin all if all are pinned
    fn pin_many(&mut self) {
        if self.only_one() {
            return self.toggle_pin();
        }
        let rows: Vec<(String, String, String)> =
            self.chosen_rows().iter().map(|s| (s.name.clone(), s.title.clone(), notes::key(s))).collect();
        let on = rows.iter().any(|(name, ..)| self.pins.rank(name).is_none());
        let (mut us, mut err) = (vec![], None);
        for (name, title, key) in rows {
            let rank = self.pins.rank(&name);
            if on && rank.is_some() {
                continue;
            }
            match self.pins.toggle(&name) {
                Ok(_) => us.push(Undo::Pin { key, name, title, rank }),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let n = us.len();
        let m = match err {
            Some(e) => tr!("置顶保存失败:{e}", "pin not saved: {e}"),
            None if on => tr!("已置顶 {n} 个", "pinned {n}"),
            None => tr!("已取消置顶 {n} 个", "unpinned {n}"),
        };
        if n > 0 {
            self.remember(Undo::Many(us));
            self.drop_picks();
        }
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// u after multi-select: if any is unread (◐) mark all read, otherwise mark the idle ones unread; running ones and ones
    /// needing your answer are left alone
    fn read_many(&mut self) {
        if self.only_one() {
            return self.toggle_read();
        }
        let idx: Vec<usize> =
            self.chosen().iter().filter_map(|id| self.all.iter().position(|s| s.sub.is_none() && s.deck_id == *id)).collect();
        let read = idx.iter().any(|&i| self.all[i].state == State::Unread);
        let want = if read { State::Unread } else { State::Idle };
        let idx: Vec<usize> = idx.into_iter().filter(|&i| self.all[i].state == want).collect();
        let (mut us, mut err) = (vec![], None);
        for i in idx {
            match self.mark_one(i) {
                Some(Ok(u)) => us.push(u),
                Some(Err(e)) => {
                    err = Some(e);
                    break;
                }
                None => {}
            }
        }
        let n = us.len();
        let what = if read { t!("已读", "read") } else { t!("未读", "unread") };
        let m = match err {
            Some(e) => tr!("保存失败:{e}", "not saved: {e}"),
            None if n == 0 => t!("只能标记空闲或未读的会话", "only idle / unread ones can be marked").to_string(),
            None => tr!("已将 {n} 个标为{what}", "marked {n} {what}"),
        };
        if n > 0 {
            self.remember(Undo::Many(us));
            self.drop_picks();
        }
        self.flash(&m);
        self.rebuild(); // with @ picked, ones marked read drop off the list
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

    /// Full-screen reader on the highlighted session: show the screen content we already have, the run loop fetches history next
    fn open_reader(&mut self) {
        if self.on_group(t!("查看全文", "read in full")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let (name, lines) = (s.name.clone(), preview_lines(s));
        self.trace = None;
        self.reader = Some(Reader {
            name,
            lines,
            scroll: 0,
            wrapped: vec![],
            wrap_w: 0,
            dirty: true,
            help: false,
            view_h: 0,
            diff: None,
            hit: None,
            plugin: None,
        });
        self.reader_load = true;
    }

    /// `x`: full-screen diff of the highlighted session's directory, from the top; the mode carries over from last time
    fn open_diff(&mut self) {
        if self.on_group(t!("查看改动", "view the diff")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let (name, w) = (s.name.clone(), Where::of(s));
        self.trace = None;
        let lines = vec![vec![ansi::Seg { text: t!(" 正在读取 git…", " reading git…").into(), style: dim(), link: None }]];
        let diff = DiffView {
            mode: self.diff_mode,
            w,
            files: vec![],
            rows: vec![],
            starts: vec![],
            at: vec![],
            paths: vec![],
            top: String::new(),
            loading: true,
            want: true,
            seen: None,
        };
        self.reader = Some(Reader {
            name,
            lines,
            scroll: usize::MAX,
            wrapped: vec![],
            wrap_w: 0,
            dirty: true,
            help: false,
            view_h: 0,
            diff: Some(diff),
            hit: None,
            plugin: None,
        });
    }

    /// Whether the diff needs a git run: just opened, mode switched, or the periodic refresh is due. Gives the run loop
    /// (session name, directory, mode)
    pub fn diff_request(&mut self, refreshed: bool) -> Option<(String, Where, Mode)> {
        let act = self.reader.as_ref().and_then(|r| self.all.iter().find(|s| s.name == r.name)).map_or(0, |s| s.active_at);
        let r = self.reader.as_mut()?;
        let d = r.diff.as_mut()?;
        let fresh = refreshed && d.seen.is_none_or(|(a, t)| a != act || t.elapsed() >= GLANCE_EVERY);
        if !(std::mem::take(&mut d.want) || fresh) {
            return None;
        }
        d.seen = Some((act, Instant::now()));
        Some((r.name.clone(), d.w.clone(), d.mode))
    }

    /// A finished diff. Dropped if the session / mode has changed since; left alone if same as last time (no re-wrapping)
    pub fn set_diff(&mut self, name: &str, mode: Mode, d: diff::Diff) {
        let Some(r) = self.reader.as_mut().filter(|r| r.name == name) else { return };
        let Some(v) = r.diff.as_mut().filter(|v| v.mode == mode) else { return };
        if !v.loading && r.lines == d.lines {
            return;
        }
        v.loading = false;
        v.files = d.files;
        v.at = d.at;
        v.paths = d.paths;
        v.top = d.top;
        r.lines = d.lines;
        r.dirty = true;
    }

    /// `Z`: the highlighted session's record (see `trace`)
    fn open_trace(&mut self) {
        if self.on_fold_row() {
            return self.flash(t!("先按回车展开", "press Enter to unfold first"));
        }
        let Some(s) = self.current().filter(|s| !s.blank) else { return };
        // Subagent row: open its session's record, and once read go straight into its level (Esc goes back to the turn that started it)
        let (name, goto) = match &s.subagent {
            Some(a) => (s.family().to_string(), Some(a.id.clone())),
            None => (s.name.clone(), None),
        };
        self.reader = None;
        self.trace = Some(TraceView { goto, ..TraceView::new(name) });
    }

    /// Whether to read the record in the background: just opened / session switched. Gives the run loop that session
    pub fn trace_request(&mut self) -> Option<Session> {
        let v = self.trace.as_mut()?;
        if !std::mem::take(&mut v.want) {
            return None;
        }
        let s = self.all.iter().find(|s| s.name == v.name).cloned();
        if s.is_none() {
            v.loading = false;
            v.err = Some(t!("会话已不存在", "the session is gone").into());
        }
        s
    }

    /// The background read is done. Dropped if the session has changed since
    pub fn set_trace(&mut self, name: &str, r: Result<crate::trace::Trace, String>) {
        let Some(v) = self.trace.as_mut().filter(|v| v.name == name) else { return };
        v.loading = false;
        match r {
            Ok(t) => v.set(t),
            Err(e) => v.err = Some(e),
        }
    }

    /// Periodic refresh: if the record grew, read the new tail (usually a few KB, a few ms)
    pub fn trace_tail(&mut self) {
        if let Some(v) = self.trace.as_mut() {
            v.tail();
        }
    }

    /// Whether the record's session is running (a running turn and tools with no result yet count up to now)
    fn trace_live(&self) -> bool {
        self.trace.as_ref().is_some_and(|v| self.all.iter().any(|s| s.name == v.name && matches!(s.state, State::Running | State::Ask)))
    }

    /// e: view the selected item read-only in the editor — tools that read / change a file show the file itself, others are
    /// written to a temporary Markdown file
    fn trace_open_view(&mut self) {
        let live = self.trace_live();
        let Some(v) = self.trace.as_ref() else { return };
        let live = v.live(live, now_ms());
        let Some(t) = v.tr() else { return };
        let i = match &v.detail {
            Some(d) => d.node,
            None if v.in_steps => v.steps.sel,
            None => v.chat.sel,
        };
        if t.nodes.get(i).is_none() {
            return;
        }
        match crate::trace::view(t, i, live, now_ms()) {
            None => self.flash(t!("此行没有可查看的内容", "nothing to see on this row")),
            Some(crate::trace::View::File { path, line }) => self.external = Some(External::View { path, line, temp: false }),
            Some(crate::trace::View::Text { text, ext, line }) => match write_view(&crate::trace::where_of(t, i), &text, &ext) {
                Ok(path) => self.external = Some(External::View { path, line, temp: true }),
                Err(e) => self.flash(&tr!("无法写入临时文件:{e}", "can't write the temp file: {e}")),
            },
        }
    }

    fn on_trace_key(&mut self, k: KeyEvent) {
        // Ctrl combos aren't letters (^Z isn't z for output): ^L restarts in place (same as the list and billing panel), others are ignored
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            if k.code == KeyCode::Char('l') {
                self.ask_relaunch();
            }
            return;
        }
        let Some(v) = self.trace.as_mut() else { return };
        if let Some(d) = v.detail.as_mut() {
            let page = d.view_h.saturating_sub(1).max(1);
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => v.detail = None,
                KeyCode::Char('e') => return self.trace_open_view(),
                KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => d.scroll = d.scroll.saturating_add(1),
                KeyCode::PageUp | KeyCode::Char('b') => d.scroll = d.scroll.saturating_sub(page),
                KeyCode::PageDown | KeyCode::Char(' ') => d.scroll = d.scroll.saturating_add(page),
                KeyCode::Home | KeyCode::Char('g') => d.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => d.scroll = usize::MAX, // clamped to the bottom when drawn
                KeyCode::Char(c @ ('n' | 'p')) => {
                    // Next / previous block in the same column
                    let b = v.cur();
                    let at = b.at().unwrap_or(0);
                    let to = if c == 'n' { b.items.get(at + 1) } else { at.checked_sub(1).and_then(|a| b.items.get(a)) };
                    if let Some(x) = to.map(|x| x.0) {
                        b.jump(x);
                        v.detail = Some(TraceDetail::new(x));
                    }
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q' | 'Z') => return self.trace = None,
            // In a subagent: back up a level
            // Entered straight from a subagent row in the list: Esc goes back to
            // the list (← / h still go up a level, to whoever started it)
            KeyCode::Esc if !v.in_steps && v.home > 0 && v.path.len() == v.home => return self.trace = None,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') if !v.in_steps && v.leave_sub() => return,
            KeyCode::Esc if !v.in_steps => return self.trace = None,
            KeyCode::Char('z') | KeyCode::Tab => return self.open_reader(),
            KeyCode::Char('x') => return self.open_diff(),
            KeyCode::Char('e') => return self.trace_open_view(),
            KeyCode::Char('J') => return self.trace_step(1),
            KeyCode::Char('K') => return self.trace_step(-1),
            _ => {}
        }
        if v.t.is_none() {
            return;
        }
        let page = v.cur().view_h.saturating_sub(1).max(1) as isize;
        let sel = v.cur().sel;
        let kind = v.tr().and_then(|t| t.nodes.get(sel)).map(|n| n.kind);
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => v.cur().go(false),
            KeyCode::Down | KeyCode::Char('j') => v.cur().go(true),
            KeyCode::PageUp | KeyCode::Char('b') => v.cur().scroll(-page),
            KeyCode::PageDown | KeyCode::Char(' ') => v.cur().scroll(page),
            KeyCode::Home | KeyCode::Char('g') => v.cur().home(),
            KeyCode::End | KeyCode::Char('G') => {
                v.cur().follow = true;
                if !v.in_steps {
                    v.steps_of = None;
                }
            }
            // Right side: ⏎ views this step full screen (one that started a subagent: go into its record); ← / Esc back to the conversation
            KeyCode::Enter if v.in_steps => {
                if !v.enter_sub(sel) {
                    v.detail = Some(TraceDetail::new(sel));
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc if v.in_steps => v.in_steps = false,
            // Left side: ⏎ / → unfolds a copied block; ⏎ / → on a turn shows its steps
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if kind == Some(crate::trace::Kind::Copied) => {
                if !v.open.remove(&sel) {
                    v.open.insert(sel);
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if !v.in_steps && kind == Some(crate::trace::Kind::Turn) => {
                v.in_steps = true
            }
            KeyCode::Char('o') if !v.in_steps => {
                let clipped = v.chat_cache.get(&sel).is_some_and(|c| c.clipped);
                let can = v.open.contains(&sel)
                    || kind == Some(crate::trace::Kind::Copied)
                    || (kind == Some(crate::trace::Kind::Turn) && clipped);
                if !can {
                    return;
                }
                if !v.open.remove(&sel) {
                    v.open.insert(sel);
                    v.chat.reveal = true;
                }
            }
            // Right side: o folds / unfolds this step, O the whole turn
            KeyCode::Char('o') => {
                let fold = v.steps_cache.as_ref().and_then(|c| c.blocks.iter().find(|x| x.node == sel)).is_some_and(|x| x.fold);
                if !fold {
                    return;
                }
                if !v.unfold.remove(&sel) {
                    v.unfold.insert(sel);
                    v.steps.reveal = true;
                }
                v.unfold_gen += 1;
            }
            KeyCode::Char('O') if v.in_steps => {
                let folds: Vec<usize> =
                    v.steps_cache.as_ref().map_or(vec![], |c| c.blocks.iter().filter(|x| x.fold).map(|x| x.node).collect());
                if folds.iter().any(|x| v.unfold.contains(x)) {
                    folds.iter().for_each(|x| {
                        v.unfold.remove(x);
                    });
                } else {
                    v.unfold.extend(folds);
                }
                v.unfold_gen += 1;
            }
            KeyCode::Char('O') => {
                if v.open.is_empty() {
                    let all: Vec<usize> = v.tr().map_or(vec![], |t| {
                        (0..t.nodes.len())
                            .filter(|&x| matches!(t.nodes[x].kind, crate::trace::Kind::Turn | crate::trace::Kind::Copied))
                            .collect()
                    });
                    v.open.extend(all);
                } else {
                    v.open.clear();
                }
            }
            KeyCode::Char(c @ ('n' | 'p')) => {
                v.turn_step(c == 'n');
            }
            _ => {}
        }
    }

    /// J / K: switch to the next / previous session in the list and keep viewing records
    fn trace_step(&mut self, d: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        let mut next = cur + d;
        while next >= 0 && next < self.view.len() as isize && self.is_fold_row(next as usize) {
            next += d;
        }
        if next < 0 || next >= self.view.len() as isize {
            return;
        }
        self.select(Some(next as usize));
        self.open_trace();
    }

    /// Whether to read the diff stats at the top of the preview: the highlighted session changed; on a periodic refresh it has new
    /// activity (running: the pane has output; idle: a hook reported something, see `Session::active_at`), or GLANCE_EVERY passed.
    /// Counting on every refresh would mean five git processes every two seconds while sitting on a big repository.
    /// Not read while the preview is off or a full-screen view is open
    pub fn glance_request(&mut self, refreshed: bool) -> Option<(String, Where)> {
        if self.reader.is_some() || self.trace.is_some() || self.mode() == Preview::Off {
            return None;
        }
        if refreshed {
            self.spot = None;
        }
        // On a group: read the group's paths (key from group_key; by it the run loop also reads what's in the directories);
        // activity is the group's most recently active session
        let (key, w, act) = if self.on_group_row() {
            let sp = self.spot()?;
            let act = self.all.iter().filter(|s| s.group == sp.group && s.sub.is_none()).map(|s| s.active_at).max().unwrap_or(0);
            (group_key(&sp.group), Where { dir: sp.dir, ..Where::default() }, act)
        } else {
            let s = self.current().filter(|s| s.subagent.is_none())?;
            (s.name.clone(), Where::of(s), s.active_at)
        };
        let due = match &self.glance_for {
            Some((k, a, t)) if *k == key => refreshed && (*a != act || t.elapsed() >= GLANCE_EVERY),
            _ => true,
        };
        if !due {
            return None;
        }
        self.glance_for = Some((key.clone(), act, Instant::now()));
        Some((key, w))
    }

    pub fn set_glance(&mut self, name: String, g: Glance) {
        self.glances.insert(name, g);
    }

    /// Back from lazygit / the editor: the diff may have changed (committed, files changed), reread; say so if it didn't start
    pub fn external_done(&mut self, err: Option<String>) {
        if let Some(d) = self.reader.as_mut().and_then(|r| r.diff.as_mut()) {
            d.want = true;
        }
        self.glance_for = None;
        if let Some(e) = err {
            self.flash(&e);
        }
        self.reload_overlays();
        if let Some(top) = self.overlays.last_mut() {
            let mut fx = Fx::default();
            top.returned(&mut fx);
            self.apply(fx);
        }
        // An editor opened from a popup: back to the popup; once that closes, back to the settings page
        if self.overlays.is_empty() {
            self.back_to_settings();
        }
    }

    /// l: open lazygit in this repository
    fn diff_lazygit(&mut self) {
        let Some(v) = self.reader.as_ref().and_then(|r| r.diff.as_ref()) else { return };
        match () {
            _ if v.loading => self.flash(t!("正在读取 git,请稍候", "still reading git, wait a moment")),
            _ if v.top.is_empty() => self.flash(t!("不是 git 仓库", "not a git repository")),
            _ => self.external = Some(External::Lazygit(v.top.clone())),
        }
    }

    /// e: open the spot at the very top of the screen in the editor (top is the
    /// file list / a blank line: look down for the first one with a position)
    fn diff_edit(&mut self) {
        let Some(r) = self.reader.as_ref() else { return };
        let Some(v) = r.diff.as_ref() else { return };
        let top = r.top(r.view_h.max(1));
        let line = v.starts.partition_point(|&x| x <= top).saturating_sub(1);
        let Some((file, n)) = v.at.iter().skip(line).find_map(|a| *a) else {
            return self.flash(if v.loading {
                t!("正在读取 git,请稍候", "still reading git, wait a moment")
            } else {
                t!("没有可打开的文件", "no file to open")
            });
        };
        match v.paths.get(file).cloned().flatten() {
            None => self.flash(t!("该文件已被删除", "this file has been deleted")),
            Some(p) => {
                let path = std::path::Path::new(&v.top).join(p);
                if path.is_file() {
                    self.external = Some(External::Edit { path: path.to_string_lossy().into_owned(), line: n });
                } else {
                    self.flash(t!("文件已不存在", "the file is gone"));
                }
            }
        }
    }

    /// n / p: put the next / previous file's heading at the very top (p past the first goes back to the file list)
    fn diff_jump(&mut self, d: isize) {
        let Some(r) = self.reader.as_mut() else { return };
        let Some(v) = r.diff.as_ref() else { return };
        let (h, total) = (r.view_h.max(1), r.wrapped.len());
        let top = r.top(h);
        let to = if d > 0 {
            v.rows.iter().copied().find(|&x| x > top)
        } else {
            Some(v.rows.iter().copied().rev().find(|&x| x < top).unwrap_or(0))
        };
        let scroll = to.map(|t| total.saturating_sub(t + h));
        if let Some(s) = scroll.filter(|&s| s != r.scroll.min(total.saturating_sub(h))) {
            r.scroll = s;
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
                b.sel = sel.min(crate::usage::RANGES.len() - 1);
            }
        }
        let m = installed
            .map_or(t!("已重启", "restarted").into(), |t| tr!("已重启 · 当前 aca 安装于 {t}", "restarted · this aca was installed {t}"));
        self.flash(&m);
    }

    fn open_bill(&mut self) {
        self.bill = Some(BillView { sel: 1, ..Default::default() });
        self.bill_want = true;
    }

    /// Asked by the run loop: whether the background should read a fresh copy (if so, which accounts to show). Once a minute
    /// while the panel is open, at once on r or an account switch; only one read at a time
    pub fn bill_request(&mut self) -> Option<Vec<String>> {
        if self.bill.is_none() || self.bill_loading {
            return None;
        }
        let stale = self.bill_got.is_none_or(|t| t.elapsed() >= BILL_EVERY);
        if std::mem::take(&mut self.bill_want) || stale {
            self.bill_loading = true;
            return Some(self.bill_only.clone());
        }
        None
    }

    /// The background read is done; on failure keep the last numbers and say so in the bottom bar
    pub fn set_bill(&mut self, r: Result<crate::usage::Report, String>) {
        self.bill_got = Some(Instant::now());
        self.bill_loading = false;
        match r {
            Ok(r) => self.bill_data = Some(r),
            Err(e) => self.flash(&tr!("读取计费失败:{e}", "couldn't read billing: {e}")),
        }
    }

    /// Accounts in the account picker (from row 1): every account in the latest numbers, biggest spenders first
    fn bill_accounts(&self) -> Vec<crate::usage::Row> {
        self.bill_data.as_ref().map(|r| r.accounts.clone()).unwrap_or_default()
    }

    /// a: open the account picker, the highlight on the first account shown now ("All" if all)
    fn open_bill_pick(&mut self) {
        let accts = self.bill_accounts();
        if accts.is_empty() {
            return self.flash(t!("尚未读取到账号", "no accounts read yet"));
        }
        let sel = self.bill_only.first().and_then(|o| accts.iter().position(|a| &a.name == o)).map_or(0, |i| i + 1);
        if let Some(b) = self.bill.as_mut() {
            b.pick = Some(BillPick { sel, ..Default::default() });
        }
    }

    /// Switch to showing only these accounts (empty = all): close the picker, back to the top, have the background reread for it
    fn set_bill_only(&mut self, only: Vec<String>) {
        if let Some(b) = self.bill.as_mut() {
            b.pick = None;
            if only != self.bill_only {
                b.scroll = 0;
            }
        }
        if only != self.bill_only {
            self.bill_only = only;
            self.bill_want = true;
        }
    }

    /// Keys in the account picker: ↑↓ pick, ⏎ / a digit shows only it (with some ticked, shows the ticked ones), Space ticks / unticks,
    /// v range select, V range untick (↑↓ extends, Esc fixes, same as the main list), 0 all, Esc / a / q close
    fn on_bill_pick_key(&mut self, k: KeyEvent) {
        let accts = self.bill_accounts();
        let Some(p) = self.bill.as_mut().and_then(|b| b.pick.as_mut()) else { return };
        let n = accts.len() + 1;
        let ranging = p.anchor.is_some();
        match k.code {
            // While ranging: only move among accounts (don't wrap to "All")
            KeyCode::Up | KeyCode::Char('k') if ranging => p.sel = (p.sel - 1).max(1),
            KeyCode::Down | KeyCode::Char('j') if ranging => p.sel = (p.sel + 1).min(n - 1),
            KeyCode::Esc if ranging => {
                p.marked = p.chosen(&accts);
                p.anchor = None;
            }
            KeyCode::Esc | KeyCode::Char('a' | 'q') => {
                if let Some(b) = self.bill.as_mut() {
                    b.pick = None;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => p.sel = (p.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => p.sel = (p.sel + 1) % n,
            KeyCode::Char(c @ ('v' | 'V')) if n > 1 => {
                p.sel = p.sel.max(1);
                p.unpick = c == 'V';
                p.anchor = Some(p.sel);
            }
            KeyCode::Char(' ') => match accts.get(p.sel.wrapping_sub(1)) {
                Some(a) => {
                    match p.marked.iter().position(|m| *m == a.name) {
                        Some(i) => drop(p.marked.remove(i)),
                        None => p.marked.push(a.name.clone()),
                    }
                    p.sel = if ranging { (p.sel + 1).min(n - 1) } else { (p.sel + 1) % n };
                }
                None => p.marked.clear(),
            },
            KeyCode::Enter => {
                let chosen = p.chosen(&accts);
                let only = if !chosen.is_empty() {
                    chosen
                } else {
                    accts.get(p.sel.wrapping_sub(1)).map(|a| vec![a.name.clone()]).unwrap_or_default()
                };
                self.set_bill_only(only);
            }
            KeyCode::Char('0') => self.set_bill_only(vec![]),
            KeyCode::Char(c @ '1'..='9') => {
                if let Some(a) = accts.get(c as usize - '1' as usize) {
                    self.set_bill_only(vec![a.name.clone()]);
                }
            }
            _ => {}
        }
    }

    fn on_bill_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Char('l') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return self.ask_relaunch();
        }
        if self.bill.as_ref().is_some_and(|b| b.pick.is_some()) {
            return self.on_bill_pick_key(k);
        }
        if k.code == KeyCode::Char('r') {
            self.bill_want = true;
            return;
        }
        if k.code == KeyCode::Char('a') {
            return self.open_bill_pick();
        }
        let Some(b) = self.bill.as_mut() else { return };
        let page = b.view_h.max(1);
        let n = crate::usage::RANGES.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q' | '$') => self.bill = None,
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => b.sel = (b.sel + 1) % n,
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => b.sel = (b.sel + n - 1) % n,
            KeyCode::Char(c @ '1'..='4') => b.sel = c as usize - '1' as usize,
            KeyCode::Char('m') => b.more = !b.more,
            KeyCode::Up | KeyCode::Char('k') => b.scroll = b.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => b.scroll = b.scroll.saturating_add(1),
            KeyCode::PageUp | KeyCode::Char('b') => b.scroll = b.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => b.scroll = b.scroll.saturating_add(page),
            KeyCode::Home | KeyCode::Char('g') => b.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => b.scroll = usize::MAX, // clamped to the bottom when drawn
            _ => {}
        }
    }

    fn open_help(&mut self) {
        self.reader = Some(Reader {
            name: t!("aca 按键", "aca keys").into(),
            lines: vec![],
            scroll: usize::MAX,
            wrapped: vec![],
            wrap_w: 0,
            dirty: true,
            help: true,
            view_h: 0,
            diff: None,
            hit: None,
            plugin: None,
        });
    }

    /// The cell the full-screen reader is showing (the run loop fetches history from
    /// it): the pane id, or the session name for a stopped session without one
    pub fn reader_target(&self) -> Option<String> {
        let r = self.reader.as_ref().filter(|r| !r.help && r.diff.is_none() && r.plugin.is_none())?;
        let pane = self.all.iter().find(|s| s.name == r.name).map(|s| s.pane.clone()).filter(|p| !p.is_empty());
        Some(pane.unwrap_or_else(|| format!("={}:", r.name)))
    }

    /// History fetched back. While scrolled up, new content growing at the bottom doesn't push the view away
    pub fn set_reader_lines(&mut self, lines: Vec<String>) {
        let Some(r) = self.reader.as_mut().filter(|r| r.diff.is_none() && r.plugin.is_none()) else { return };
        let asking = self.all.iter().any(|s| s.name == r.name && s.state == State::Ask);
        r.lines = strip_prompt(ansi::parse_lines(&lines), asking);
        r.dirty = true;
    }

    fn on_reader_key(&mut self, k: KeyEvent) {
        let page = (self.screen.height.saturating_sub(3) as usize).max(1);
        let Some(r) = self.reader.as_mut() else { return };
        let help = r.help;
        let diff = r.diff.is_some();
        let page = if help && r.view_h > 0 { r.view_h } else { page };
        if help {
            // Keys popup: Esc / ? / q close, arrows and paging keys scroll; any other key
            // "looks it up" — lights up the row explaining it and scrolls there
            match k.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q') if !k.modifiers.contains(KeyModifiers::CONTROL) => self.reader = None,
                KeyCode::Up => r.scroll = r.scroll.saturating_add(1),
                KeyCode::Down => r.scroll = r.scroll.saturating_sub(1),
                KeyCode::PageUp => r.scroll = r.scroll.saturating_add(page),
                KeyCode::PageDown => r.scroll = r.scroll.saturating_sub(page),
                KeyCode::Home => r.scroll = usize::MAX,
                KeyCode::End => r.scroll = 0,
                _ => r.hit = Some(crate::help::hit(k)),
            }
            return;
        }
        if let Some(req) = r.plugin.clone() {
            // Plugin output: can page, copy, r reruns; it's not a session, so you can't go in or switch to another session
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?') => self.reader = None,
                KeyCode::Char('r') => {
                    self.flash(&tr!("正在重新运行 {}…", "re-running {}…", req.title()));
                    self.plugin_q.push(req);
                }
                KeyCode::Char('c') => {
                    let text: Vec<String> = r.lines.iter().map(ansi::plain).collect();
                    self.clip = Some(Clip::Text(text.join("\n")));
                }
                KeyCode::Up | KeyCode::Char('k') => r.scroll = r.scroll.saturating_add(1),
                KeyCode::Down | KeyCode::Char('j') => r.scroll = r.scroll.saturating_sub(1),
                KeyCode::PageUp | KeyCode::Char('b') => r.scroll = r.scroll.saturating_add(page),
                KeyCode::PageDown | KeyCode::Char(' ') => r.scroll = r.scroll.saturating_sub(page),
                KeyCode::Char('u') => r.scroll = r.scroll.saturating_add(page / 2),
                KeyCode::Char('d') => r.scroll = r.scroll.saturating_sub(page / 2),
                KeyCode::Home | KeyCode::Char('g') => r.scroll = usize::MAX,
                KeyCode::End | KeyCode::Char('G') => r.scroll = 0,
                _ => {}
            }
            return;
        }
        match k.code {
            // z for output, x for the diff: pressing the one already shown goes back
            KeyCode::Char('z') if diff => self.open_reader(),
            KeyCode::Char('x') if !diff => self.open_diff(),
            KeyCode::Tab if diff => self.open_trace(),
            KeyCode::Tab => self.open_diff(),
            KeyCode::Char('Z') => self.open_trace(),
            KeyCode::Esc | KeyCode::Char('q' | 'z' | 'x' | '?') => self.reader = None,
            KeyCode::Char('n') if diff => self.diff_jump(1),
            KeyCode::Char('l') if diff => self.diff_lazygit(),
            KeyCode::Char('e') if diff => self.diff_edit(),
            KeyCode::Char('p') if diff => self.diff_jump(-1),
            KeyCode::Char('m') if diff => {
                self.diff_mode = if self.diff_mode == Mode::Uncommitted { Mode::Branch } else { Mode::Uncommitted };
                self.open_diff();
            }
            KeyCode::Enter => {
                self.reader = None;
                self.enter();
            }
            KeyCode::Up | KeyCode::Char('k') => r.scroll = r.scroll.saturating_add(1),
            KeyCode::Down | KeyCode::Char('j') => r.scroll = r.scroll.saturating_sub(1),
            KeyCode::PageUp | KeyCode::Char('b') => r.scroll = r.scroll.saturating_add(page),
            KeyCode::PageDown | KeyCode::Char(' ') => r.scroll = r.scroll.saturating_sub(page),
            KeyCode::Char('u') => r.scroll = r.scroll.saturating_add(page / 2),
            KeyCode::Char('d') => r.scroll = r.scroll.saturating_sub(page / 2),
            KeyCode::Home | KeyCode::Char('g') => r.scroll = usize::MAX, // clamped to the top when drawn
            KeyCode::End | KeyCode::Char('G') => r.scroll = 0,
            KeyCode::Char('J') => self.reader_step(1),
            KeyCode::Char('K') => self.reader_step(-1),
            KeyCode::Char('c') => self.ask_copy(true),
            KeyCode::Char('B') => self.ask_copy(false),
            _ => {}
        }
    }

    /// J / K: switch to the next / previous session in the list and keep viewing
    fn reader_step(&mut self, d: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        let mut next = cur + d;
        // Fold rows aren't sessions, skip them
        while next >= 0 && next < self.view.len() as isize && self.is_fold_row(next as usize) {
            next += d;
        }
        if next < 0 || next >= self.view.len() as isize {
            return;
        }
        self.select(Some(next as usize));
        if self.reader.as_ref().is_some_and(|r| r.diff.is_some()) {
            self.open_diff();
        } else {
            self.open_reader();
        }
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

    /// Workspaces that can be listed (ones with sessions, plus the current one), not h; sorted by key (digits before letters)
    fn space_list(&self) -> Vec<char> {
        let mut v: Vec<char> = self.all.iter().map(|s| s.space).chain([self.space]).filter(|&c| c != spaces::HIDDEN).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// What to call the current workspace in the top bar / list: with only 1 in use and never renamed it's "sessions", as before
    fn space_label(&self) -> String {
        if self.space == spaces::HOME && !self.spaces.renamed(spaces::HOME) && self.space_list().len() <= 1 {
            t!("全部分组", "All groups").into()
        } else {
            self.spaces.name(self.space)
        }
    }

    /// Picked with q+key / [ ] / ` / S: switch to this workspace, highlight the session last highlighted there
    fn switch_space(&mut self, c: char) {
        self.space_menu = None;
        if c != self.space {
            if let Some(n) = self.selected.take() {
                self.space_sel.insert(self.space, n);
            }
            self.last_space = Some(self.space);
            self.space = c;
            self.group = None;
            self.prev_group = None;
            self.selected = self.space_sel.get(&c).cloned();
            self.head = None;
            self.list.select(Some(0));
            self.rebuild();
        }
        let m = tr!("工作区:{}", "workspace: {}", self.spaces.name(c));
        self.flash(&m);
    }

    /// [ ]: previous / next workspace (h skipped), wrapping around at the ends
    fn cycle_space(&mut self, d: isize) {
        let v = self.space_list();
        let next = if d > 0 {
            v.iter().copied().find(|&c| c > self.space).or(v.first().copied())
        } else {
            v.iter().copied().rev().find(|&c| c < self.space).or(v.last().copied())
        };
        match next {
            Some(c) if c != self.space => self.switch_space(c),
            _ => self.flash(t!("只有这一个工作区", "there's only this workspace")),
        }
    }

    /// `: back to the workspace just visited (h counts), again comes back
    fn back_space(&mut self) {
        match self.last_space.filter(|&c| c != self.space) {
            Some(c) => self.switch_space(c),
            None => self.flash(t!("还没有切换过工作区", "no workspace switched yet")),
        }
    }

    /// ^` (in the popup): this client switches back to the previous session, landing in the cell it left from; switching away quits aca
    fn back_session(&mut self) {
        match crate::data::last_session() {
            Some(name) => {
                self.focus = None;
                self.attach = Some(name);
            }
            None => self.flash(t!("没有上一个会话", "no previous session")),
        }
    }

    /// Q+key: send the highlighted session to that workspace (child row = the session it's in); child sessions never placed on their
    /// own go along (and theirs too). On a fold row = this workspace's whole group, recording the group too, so new ones opened in it
    /// later (including `aca new` in a terminal) land there directly. You stay here
    fn send_to_space(&mut self, c: char) {
        if c == self.space {
            return self.flash(t!("已在该工作区", "already in this workspace"));
        }
        // v multi-selecting: send the selected ones
        let many = self.picking().then(|| self.chosen());
        let fold = many.is_none() && self.on_group_row();
        let cur = self.current().filter(|_| many.is_none());
        let (fam, group) = cur.map(|s| (s.family().to_string(), s.group.clone())).unwrap_or_default();
        // Empty group: no sessions to send, just record the group
        let blank = cur.filter(|s| s.blank);
        let heads = || self.all.iter().filter(|x| x.sub.is_none() && !x.blank);
        let go: Vec<&Session> = match &many {
            Some(ids) => heads().filter(|x| !x.deck_id.is_empty() && ids.contains(&x.deck_id)).collect(),
            None if fold => heads().filter(|x| x.space == self.space && x.group == group).collect(),
            None => heads().filter(|x| x.name == fam).collect(),
        };
        let Some(first) = go.first().or(blank.as_ref()) else { return };
        let what = match () {
            _ if many.is_some() => ops::named(&first.title, go.len()),
            _ if fold => tr!("「{}」整组", "group “{}”", group_label(&group)),
            _ => tr!("「{}」", "“{}”", first.title),
        };
        let mut names: HashSet<&str> = go.iter().map(|x| x.name.as_str()).collect();
        // Add child sessions never placed on their own, level by level, until no more are added
        loop {
            let more: Vec<&str> = heads()
                .filter(|x| !names.contains(x.name.as_str()) && self.spaces.of(&notes::key(x)).is_none())
                .filter(|x| x.up.parent().is_some_and(|p| names.contains(p)))
                .map(|x| x.name.as_str())
                .collect();
            if more.is_empty() {
                break;
            }
            names.extend(more);
        }
        let want: HashMap<String, char> = heads().filter(|x| names.contains(x.name.as_str())).map(|x| (notes::key(x), c)).collect();
        // A group of the same name may be in several profiles, one record each; sending back to 1 = group not recorded
        let mut groups: Vec<(spaces::Group, Option<char>)> = vec![];
        if fold {
            for x in go.iter().chain(blank.as_ref()) {
                let g = (x.profile.clone(), x.group.clone());
                if !groups.iter().any(|(y, _)| *y == g) {
                    groups.push((g, (c != spaces::HOME).then_some(c)));
                }
            }
        }
        let sessions = self.settle(&self.all, &want, &groups);
        let keep = self.keep_marks();
        // Put back on undo: what these sessions and groups had recorded before
        let before = (
            sessions.iter().map(|(k, _)| (k.clone(), self.spaces.of(k))).collect(),
            groups.iter().map(|(g, _)| (g.clone(), self.spaces.group_of(&g.0, &g.1))).collect(),
        );
        let m = match self.spaces.assign(&sessions, &groups, &keep) {
            Ok(()) => {
                let what = format!("{what} → {}", self.spaces.name(c));
                self.remember(Undo::Space { what: what.clone(), sessions: before.0, groups: before.1 });
                if many.is_some() {
                    self.drop_picks();
                }
                what
            }
            Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// Which sessions' records must change so each session lands in the workspace `want` says (ones not given stay where they are);
    /// groups are group records to change at the same time. after = the sessions after the change (when moving groups, the group has
    /// already changed). Records kept minimal: not recorded if it would be there anyway without one
    fn settle(
        &self,
        after: &[Session],
        want: &HashMap<String, char>,
        groups: &[(spaces::Group, Option<char>)],
    ) -> Vec<(String, Option<char>)> {
        let mut next = self.spaces.clone().detached();
        for (g, c) in groups {
            next.set_group(g, *c);
        }
        let heads: Vec<&Session> = after.iter().filter(|s| s.sub.is_none() && !s.blank).collect();
        let goal = |s: &Session| want.get(&notes::key(s)).copied().unwrap_or(s.space);
        // Record it if it's not where it should be. Child sessions follow their parent, and a parent's record change changes them, so
        // repeat until nothing changes (any depth takes two passes: once a record above changes, the ones below that moved with it
        // record themselves in the second pass, affecting no one else)
        for _ in 0..3 {
            let mut changed = false;
            for s in &heads {
                if space_of(after, &next, s, true) != goal(s) {
                    next.set(&notes::key(s), Some(goal(s)));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for s in &heads {
            let k = notes::key(s);
            if next.of(&k).is_some_and(|c| c == space_of(after, &next, s, false)) {
                next.set(&k, None);
            }
        }
        heads.iter().map(|s| notes::key(s)).filter(|k| next.of(k) != self.spaces.of(k)).map(|k| (k.clone(), next.of(&k))).collect()
    }

    /// A newly created / forked session lands in the current workspace (even if its group is recorded elsewhere)
    pub fn adopt(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        let keep = self.keep_marks();
        let sessions = if self.all.iter().any(|s| s.sub.is_none() && notes::key(s) == id) {
            self.settle(&self.all, &HashMap::from([(id.to_string(), self.space)]), &[])
        } else {
            // Not in the list yet: record it first; the extra record is cleared next time another session is sent
            vec![(id.to_string(), Some(self.space))]
        };
        let _ = self.spaces.assign(&sessions, &[], &keep);
    }

    /// The key after q / Q: a workspace key switches / sends; q after q quits; other keys (Esc etc.) cancel
    fn on_pending_key(&mut self, p: Pending, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match (p, k.code) {
            (Pending::Switch, KeyCode::Char('q')) if !ctrl => self.quit = true,
            (Pending::Switch, KeyCode::Char(c)) if !ctrl && spaces::is_key(c) => self.switch_space(c),
            (Pending::Send, KeyCode::Char(c)) if !ctrl && spaces::is_key(c) => self.send_to_space(c),
            _ => {}
        }
    }

    /// S's workspace list: ↑↓ pick, ⏎ switches, typing a key switches too (even unlisted ones like h); ^R renames
    fn on_space_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.space_menu else { return };
        let v = self.space_list();
        let n = v.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Down => self.space_menu = Some((sel + 1) % n),
            KeyCode::Up => self.space_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => self.switch_space(v[sel.min(n - 1)]),
            KeyCode::Char('r') if ctrl => {
                let c = v[sel.min(n - 1)];
                self.space_menu = None;
                self.prompt = Some(Prompt { text: self.spaces.name(c), ask: Ask::Space(c), hint: String::new() });
            }
            KeyCode::Char(c) if !ctrl && spaces::is_key(c) => self.switch_space(c),
            KeyCode::Esc | KeyCode::Char('S' | 'q') => self.space_menu = None,
            _ => {}
        }
    }

    fn open_space_menu(&mut self) {
        let v = self.space_list();
        self.space_menu = Some(v.iter().position(|&c| c == self.space).unwrap_or(0));
    }

    /// The color list (the settings page's "theme"): lists themes, the highlight on the one in use
    fn open_themes(&mut self) {
        let now = theme::current();
        let list: Vec<(theme::Theme, &'static theme::Pal)> = theme::list()
            .into_iter()
            .map(|t| {
                let p: &'static theme::Pal = Box::leak(Box::new(t.pal));
                (t, p)
            })
            .collect();
        let sel = list.iter().position(|x| x.0.name == now).unwrap_or(0);
        self.theme_menu = Some(ThemeMenu { list, sel, was: theme::pal(), now });
    }

    /// Color list: moving ↑↓ switches the whole interface to that theme, ⏎ uses it
    /// (saved to config.toml), Esc switches back to the one before opening
    fn on_theme_key(&mut self, k: KeyEvent) {
        let Some(m) = self.theme_menu.as_mut() else { return };
        let n = m.list.len();
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => m.sel = (m.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') => m.sel = (m.sel + n - 1) % n,
            KeyCode::Enter => {
                let (t, p) = &m.list[m.sel];
                let name = t.name.clone();
                theme::set(p);
                self.theme_menu = None;
                match theme::choose(&name) {
                    Ok(()) => self.flash(&tr!("配色:{name}", "colors: {name}")),
                    Err(e) => self.flash(&e),
                }
                return self.back_to_settings();
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                theme::set(m.was);
                self.theme_menu = None;
                return self.back_to_settings();
            }
            _ => return,
        }
        theme::set(m.list[m.sel].1);
    }

    /// `^S`: the settings page. Everything rarely used lives here, taking no single key
    fn open_settings(&mut self) {
        self.settings = Some(self.load_settings(SetPage::Main, 0));
    }

    /// Values shown by the settings page, read fresh
    fn load_settings(&self, page: SetPage, sel: usize) -> Settings {
        let mut bad = (self.config_bad)();
        bad.extend(
            self.sync_failed.iter().map(|e| {
                tr!("config.toml 有误,已忽略账号和工具设置:{e}", "config.toml has a problem; accounts and tools are ignored: {e}")
            }),
        );
        let mut s = Settings {
            page,
            sel,
            theme: theme::current(),
            lang: i18n::lang(),
            ime: (self.ime_auto)(),
            group_dir: (self.new_group_dir)(),
            bad,
            accts: (self.accounts_lister)(),
            extra: self.setting_rows.iter().map(|r| ExtraRow { key: r.key(), name: r.name(), value: r.value(), mark: r.mark() }).collect(),
        };
        s.sel = s.sel.min(s.rows().len() - 1);
        if s.page == SetPage::Problems && s.bad.is_empty() {
            s.page = SetPage::Main;
        }
        s
    }

    /// Language changed (l on the settings page, another aca changed config.toml): text written and stored in the old language is
    /// void, what needs rereading is reread at once. List and popup text is built every frame, nothing to do; this handles what was
    /// read in the background or computed on opening
    pub fn lang_changed(&mut self) {
        // Diff stats at the top of the preview (the branch's "detached", why a directory can't be read)
        self.glances.clear();
        self.glance_for = None;
        // Billing: quota descriptions are written by a subprocess in the language of the time; reread at once if open, next open if closed
        self.bill_data = None;
        self.bill_got = None;
        self.bill_want = self.bill.is_some();
        // Records: node labels are set when read, reread all of it
        if let Some(v) = self.trace.take() {
            self.trace = Some(TraceView::new(v.name));
        }
        if let Some(d) = self.reader.as_mut().and_then(|r| r.diff.as_mut()) {
            d.want = true;
        }
        for o in &mut self.overlays {
            o.reload();
        }
        self.reload_settings();
    }

    /// config.toml changed (in an editor, another aca): an open settings page rereads
    pub fn reload_settings(&mut self) {
        if let Some(s) = self.settings.take() {
            self.settings = Some(self.load_settings(s.page, s.sel));
        }
    }

    /// What was done from the settings page is finished (or cancelled): back to where it was on the settings page, values reread
    fn back_to_settings(&mut self) {
        if let Some(s) = self.settings_back.take() {
            self.settings = Some(self.load_settings(s.page, s.sel));
        }
    }

    /// Leave the settings page to do something, back when done
    fn leave_settings(&mut self) {
        self.settings_back = self.settings.take();
    }

    fn on_settings_key(&mut self, k: KeyEvent) {
        let Some(s) = self.settings.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('s') {
            self.settings = None;
            return;
        }
        match s.page {
            SetPage::Main => {
                let rows = s.rows();
                let n = rows.len();
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => s.sel = (s.sel + 1) % n,
                    KeyCode::Up | KeyCode::Char('k') => s.sel = (s.sel + n - 1) % n,
                    KeyCode::Enter => {
                        let r = rows[s.sel];
                        self.setting(r);
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.settings = None,
                    KeyCode::Char(c) if !ctrl => {
                        if let Some(i) = rows.iter().position(|&r| s.key(r) == Some(c)) {
                            s.sel = i;
                            self.setting(rows[i]);
                        }
                    }
                    _ => {}
                }
            }
            SetPage::Problems => match k.code {
                KeyCode::Char('e') => self.setting(SetRow::Config),
                KeyCode::Esc | KeyCode::Char('q') => s.page = SetPage::Main,
                _ => {}
            },
        }
    }

    /// A settings page row: switches flip on the spot, others go in
    fn setting(&mut self, r: SetRow) {
        let Some(s) = self.settings.as_mut() else { return };
        match r {
            SetRow::Theme => {
                self.leave_settings();
                self.open_themes();
            }
            // Switch on the spot (the whole interface is in that language next frame), also saved to config.toml for next time
            SetRow::Lang => {
                let to = if s.lang == Lang::Zh { Lang::En } else { Lang::Zh };
                match (self.config_set)("ui", "lang", toml::Value::String(to.code().into())) {
                    Ok(()) => {
                        i18n::set(to);
                        s.lang = to;
                        if i18n::forced() {
                            self.flash(t!(
                                "已切换;当前终端设置了 ACA_LANG,下次打开仍以它为准",
                                "switched; this terminal sets ACA_LANG, which wins next time"
                            ));
                        }
                    }
                    Err(e) => self.flash(&e),
                }
            }
            SetRow::Ime => {
                let on = !s.ime;
                match (self.config_set)("ime", "auto", toml::Value::Boolean(on)) {
                    Ok(()) => {
                        s.ime = on;
                        self.ime_changed = true;
                        if crate::ime::forced() {
                            self.flash(t!("当前终端设置了 ACA_IME,以它为准", "this terminal sets ACA_IME, which wins"));
                        }
                    }
                    Err(e) => self.flash(&e),
                }
            }
            // The accounts page is its own popup (acctpage.rs): closing it returns to this settings row, accounts recounted
            SetRow::Accounts => {
                if let Some(page) = self.accounts_page {
                    self.leave_settings();
                    self.open(page(false));
                }
            }
            SetRow::Tools | SetRow::Config => {
                self.leave_settings();
                let path = crate::config::path().to_string_lossy().into_owned();
                self.external = Some(External::Config { path, template: CONFIG_TEMPLATE.to_string() });
            }
            SetRow::GroupDir => {
                let text = s.group_dir.clone();
                self.leave_settings();
                self.prompt = Some(Prompt { ask: Ask::GroupDir, text, hint: String::new() });
            }
            SetRow::Extra(i) => {
                let mut fx = Fx::default();
                if let Some(row) = self.setting_rows.get(i) {
                    row.enter(&mut fx);
                }
                self.leave_settings();
                self.apply(fx);
                // Didn't go anywhere (switched on the spot): back to the settings page, values reread
                if self.overlays.is_empty() && self.external.is_none() {
                    self.back_to_settings();
                }
            }
            SetRow::Problems => s.page = SetPage::Problems,
        }
    }

    /// The path defaults to the selected session's directory — the list itself is the directory picker; mostly just Enter, no typing
    /// paths on a phone
    /// Start of the fzf candidates: the selected session's directory, then the other sessions' in the list
    pub fn dir_hints(&self) -> Vec<String> {
        let first = self.current().map(|s| s.dir.clone());
        first.clone().into_iter().chain(self.view.iter().map(|&i| self.all[i].dir.clone()).filter(|d| Some(d) != first.as_ref())).collect()
    }

    /// The fzf opened by ↓ / ^F: candidate directories, each marked with an icon, which group it goes to, and sessions already in it.
    /// Scans directories and asks zoxide (~0.1s), so prepared before giving up the terminal, to avoid showing the shell underneath
    pub fn dir_pick(&self, width: u16, height: u16) -> Option<DirPick> {
        let base = self.new_draft.as_ref()?.base.clone();
        self.dir_pick_from(create::dir_candidates(base.as_deref(), &self.dir_hints()), width, height)
    }

    fn dir_pick_from(&self, cands: Vec<(String, Src)>, width: u16, height: u16) -> Option<DirPick> {
        let d = self.new_draft.as_ref()?;
        let rows = cands
            .into_iter()
            .map(|(dir, src)| {
                // Count only this workspace's: other workspaces' titles (h especially) shouldn't show here
                let here: Vec<&Session> =
                    self.all.iter().filter(|s| s.space == self.space && !s.dir.is_empty() && create::tilde(&s.dir) == dir).collect();
                let mark = match src {
                    _ if !here.is_empty() => '●',
                    Src::Known => '◇',
                    Src::Zoxide => '↗',
                    Src::Near => ' ',
                };
                DirRow {
                    group: d.group_at(&dir),
                    sessions: here.iter().map(|s| (session_icon(s).0.to_string(), s.title.clone())).collect(),
                    dir,
                    mark,
                }
            })
            .collect();
        Some(DirPick { rows, what: d.label.clone(), query: d.pick_query(), width, height })
    }

    /// fzf is back: Enter / double click opens there; → fills the path box to
    /// keep typing; Esc changes nothing; errors go below the input box
    pub fn picked(&mut self, got: Result<Option<Picked>, String>) {
        let Some(d) = self.new_draft.as_mut() else { return };
        match got {
            Ok(Some(Picked::Edit(dir))) => d.set_dir(&dir),
            Ok(Some(Picked::Open(dir))) => {
                d.set_dir(&dir);
                match d.finish() {
                    Ok(r) => {
                        self.create = Some(r);
                        self.new_draft = None;
                    }
                    Err(e) => d.hint = e,
                }
            }
            Ok(None) => {}
            Err(e) => d.hint = e,
        }
    }

    /// `n`: pick a tool first (accounts and tools named in config.toml), then fill the path like N
    fn open_tools(&mut self) {
        match (self.tools_loader)() {
            Ok(v) if v.is_empty() => self.flash(t!("工具菜单为空", "the tool menu is empty")),
            Ok(v) => {
                self.load_tool_quotas();
                self.tool_menu = Some((v, 0));
            }
            Err(e) => self.flash(&e),
        }
    }

    /// The quota after each account in the tool menu: the same copy X's box reads
    fn load_tool_quotas(&mut self) {
        self.tool_quotas = [Agent::Claude, Agent::Codex].into_iter().flat_map(self.rehome_quotas).collect();
    }

    fn on_tool_key(&mut self, k: KeyEvent) {
        let Some((menu, sel)) = self.tool_menu.as_mut() else { return };
        let n = menu.len();
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') => *sel = (*sel + n - 1) % n,
            KeyCode::Enter => {
                let i = *sel;
                self.pick_tool(i);
            }
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => self.pick_tool(c as usize - '1' as usize),
            // Adding accounts is on the accounts page (pick Claude Code or Codex first);
            // after adding it stays on that page, showing whether it's logged in
            KeyCode::Char('+') if self.handoff.is_none() => {
                if let Some(page) = self.accounts_page {
                    self.tool_menu = None;
                    self.open(page(true));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.tool_menu = None;
                self.handoff = None;
            }
            _ => {}
        }
    }

    fn pick_tool(&mut self, i: usize) {
        // A row that can't start (account not registered, broken tool) can't be picked: the menu stays, the bottom bar says why
        if let Some(p) = self.tool_menu.as_ref().and_then(|(m, _)| m[i].problem.clone()) {
            return self.flash(&p);
        }
        let Some((menu, _)) = self.tool_menu.take() else { return };
        let t = &menu[i];
        if let Some(p) = self.handoff.take() {
            let mut d = p.draft;
            (d.tool, d.account) = (t.tool.clone(), t.account.clone());
            d.label = t.name.clone();
            match d.finish() {
                Ok(req) => self.job = Some(Job::Handoff { t: p.t, from: p.from, req, to: t.agent, note: p.note }),
                Err(e) => self.flash(&e),
            }
            return;
        }
        match self.draft() {
            Ok(mut d) => {
                (d.tool, d.account) = (t.tool.clone(), t.account.clone());
                d.label = t.name.clone();
                self.new_draft = Some(d);
                self.pick = self.pick_first();
            }
            Err(e) => self.flash(&e),
        }
    }

    fn ask_remove(&mut self) {
        // Other deletes still running don't matter, delete on; other work (restart, rename…) must finish first
        if let Some(b) = self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)) {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        let Some(s) = self.current() else { return };
        // A plain tmux session row with other windows / panes inside: close only this cell, the lowest-numbered one becomes the head
        // (tmux's renumber-windows turns #2 into #1). A registered session deletes the whole session: extra windows inside close with
        // it (the confirm box says how many)
        let others = self.all.iter().filter(|x| x.sub.is_some() && x.subagent.is_none() && x.family() == s.name).count();
        let next = match &s.sub {
            None if !s.pane.is_empty() && s.deck_id.is_empty() => self
                .all
                .iter()
                .filter(|x| x.family() == s.name)
                .filter_map(|x| Some((x.sub.as_ref()?, x)))
                .min_by_key(|(y, _)| (y.win, y.pane)),
            _ => None,
        };
        self.confirm = Some(Confirm::Remove(Removal {
            name: s.name.clone(),
            title: s.title.clone(),
            profile: s.profile.clone(),
            id: s.deck_id.clone(),
            worktree: s.worktree,
            attached: s.attached,
            pane: match (&s.sub, next) {
                (Some(x), _) => Some((s.pane.clone(), x.split)),
                (None, Some(_)) => Some((s.pane.clone(), false)),
                (None, None) => None,
            },
            next: next.map(|(y, x)| tr!("{}「{}」", "{} “{}”", y.loc(), clip(&x.title, 16).trim_end())),
            others: if s.sub.is_none() && !s.deck_id.is_empty() { others } else { 0 },
            kids: if s.sub.is_some() || next.is_some() {
                0
            } else {
                self.all.iter().filter(|x| x.sub.is_none() && x.up.parent() == Some(&s.name)).count()
            },
        }));
    }

    /// What R / r / m need: the highlighted registered session. Background
    /// busy, on a fold row, or a plain tmux session: say so, return None
    fn deck_target(&mut self, what: &str) -> Option<Target> {
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            self.flash(&m);
            return None;
        }
        if self.on_group(what) {
            return None;
        }
        let s = self.current()?;
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能按 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            self.flash(&m);
            return None;
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        if t.id.is_empty() {
            let m = tr!("普通 tmux 会话 · A 接管后才能{what}", "can't {what} an ordinary tmux session · A adopts it");
            self.flash(&m);
            return None;
        }
        Some(t)
    }

    /// s's cell, handed to park.rs
    fn pane_of(&self, s: &Session) -> crate::park::Pane {
        let title = clip(&s.title, 16).trim_end().to_string();
        crate::park::Pane {
            id: s.pane.clone(),
            label: s.sub.as_ref().map_or_else(|| title.clone(), |x| tr!("{}「{title}」", "{} “{title}”", x.loc())),
        }
    }

    /// D on a cell that isn't a registered agent (extra windows / panes in a session, a plain tmux session): stops only this cell,
    /// the pane is kept with its last screen (see park.rs). Asks first. Returns false = not handled here (a registered session's
    /// row: it's the agent that stops, via `session::stop`)
    fn ask_pane(&mut self) -> bool {
        if self.on_group_row() {
            return false;
        }
        let Some(s) = self.current() else { return false };
        if s.pane.is_empty() || (s.sub.is_none() && !s.deck_id.is_empty()) {
            return false;
        }
        if let Some(b) = self.doing() {
            self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
            return true;
        }
        if s.state == State::Dead {
            self.flash(t!("已停止 · ⏎ 重新启动", "already stopped · ⏎ starts it"));
            return true;
        }
        let p = self.pane_of(s);
        let why = match s.state {
            State::Running => t!("正在运行,将被中断", "it's running; this interrupts it"),
            State::Ask => t!("正在等待回应,将被中断", "it's waiting for your choice; this interrupts it"),
            _ => "",
        };
        let t = Target { name: s.family().to_string(), title: p.label.clone(), profile: s.profile.clone(), id: String::new() };
        let row = s.name.clone();
        self.confirm = Some(Confirm::Pane { t, p, row, restart: false, why: why.into(), next: None });
        true
    }

    /// R: running / waiting for your choice asks first (a restart interrupts it), others restart directly
    fn ask_restart(&mut self) {
        let why = match self.current().map(|s| s.state) {
            Some(State::Running) => t!("正在运行,将被中断", "it's running; this interrupts it"),
            Some(State::Ask) => t!("正在等待回应,将被中断", "it's waiting for your choice; this interrupts it"),
            _ => "",
        };
        let Some(t) = self.deck_target(t!("重启", "restart")) else { return };
        if why.is_empty() {
            self.job = Some(Job::Restart { t, force: false });
        } else {
            self.confirm = Some(Confirm::Restart { t, why: why.into(), force: false });
        }
    }

    /// D: stop — the agent exits, the pane is kept with its last screen, the registration too; later ⏎ restarts it in place and
    /// the conversation continues. Asks first
    fn ask_stop(&mut self) {
        if self.ask_pane() {
            return;
        }
        let Some(t) = self.deck_target(t!("停止", "stop")) else { return };
        let (state, attached) = self.current().map_or((State::Idle, false), |s| (s.state, s.attached));
        if state.gone() || state == State::Dead {
            self.flash(t!("已停止 · ⏎ 重新启动", "already stopped · ⏎ starts it"));
        } else {
            self.confirm = Some(Confirm::Stop { t, attached });
        }
    }

    /// A: adopt the highlighted plain tmux session (not started by aca) — register it with aca, processes untouched. Identify it
    /// first and show what was found in the confirm box. On an extra window in a plain session: adopt the session it's in, the
    /// agent counted as this cell's
    fn ask_adopt(&mut self) {
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("接管", "adopt")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let registered = |name: &str| self.all.iter().any(|x| x.sub.is_none() && x.name == name && !x.deck_id.is_empty());
        if !s.deck_id.is_empty() || registered(s.family()) {
            return self.flash(t!("它已是 aca 会话", "it's already an aca session"));
        }
        if s.state == State::Dead {
            return self.flash(t!("该窗格的程序已退出,无法接管", "the program in that pane has exited; nothing to adopt"));
        }
        let target = if s.sub.is_some() { s.pane.clone() } else { s.tmux.clone() };
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: String::new() };
        match (self.adopt_planner)(&target) {
            Ok(plan) => self.confirm = Some(Confirm::Adopt { t, plan: Box::new(plan) }),
            Err(e) => self.flash(&tr!("无法接管:{e}", "can't adopt: {e}")),
        }
    }

    /// r: change the title in a popup, the old title pre-filled. On a group: edit the group (name, default directory)
    fn ask_rename(&mut self) {
        if let Some(g) = self.group_row().map(String::from) {
            return self.ask_group_edit(g);
        }
        let Some(t) = self.deck_target(t!("重命名", "rename")) else { return };
        self.prompt = Some(Prompt { text: t.title.clone(), ask: Ask::Rename(t), hint: String::new() });
    }

    /// r on a group: rename first (old name pre-filled, may include parents), then after Enter the default directory. Only groups
    /// in the registry ("No group" and the plain tmux sessions section aren't)
    fn ask_group_edit(&mut self, g: String) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        if g == "-" {
            return self.flash(t!("这些是普通 tmux 会话,没有可修改的分组", "these are ordinary tmux sessions; no group to edit"));
        }
        if !(self.group_hints)().groups.iter().any(|(x, _)| *x == g) {
            return self.flash(&tr!("「{g}」不是已登记的分组:用 m 移到分组", "“{g}” isn't a registered group: use m to put it in a group"));
        }
        self.prompt = Some(Prompt { text: g.clone(), ask: Ask::GroupName(g), hint: String::new() });
    }

    /// The group's new name is in: if it doesn't clash with another group, go on to the default directory (old one pre-filled)
    fn group_name_given(&mut self, group: String, text: String) {
        let hints = (self.group_hints)().groups;
        let to = match ops::clean_group(&text) {
            None => Err(t!("分组名不能为空", "the group name is empty").to_string()),
            Some(to) if to.strip_prefix(&group).is_some_and(|r| r.starts_with('/')) => {
                Err(tr!("{group} 不能移到自身下面", "{group} can't move under itself"))
            }
            Some(to) => match hints.iter().find(|(x, _)| x.eq_ignore_ascii_case(&to) && *x != group) {
                Some((x, _)) => Err(tr!("分组 {x} 已存在", "group {x} already exists")),
                None => Ok(to),
            },
        };
        match to {
            Err(hint) => self.prompt = Some(Prompt { ask: Ask::GroupName(group), text, hint }),
            Ok(to) => {
                let was = hints.iter().find(|(x, _)| *x == group).map(|(_, d)| d.clone()).unwrap_or_default();
                let text = create::tilde(&was);
                self.prompt = Some(Prompt { ask: Ask::GroupHome { group, to, was }, text, hint: String::new() });
            }
        }
    }

    /// The default directory is in too: empty = not recorded; otherwise it must be an existing directory and not another group's
    /// default. Neither name nor directory changed = nothing done
    fn group_home_given(&mut self, group: String, to: String, was: String, text: String) {
        let real = aca_core::paths::real;
        let dir = match text.trim() {
            "" => Ok(String::new()),
            // Unchanged: no more checks (lets you just rename even if the old directory is gone)
            t if !was.is_empty() && real(&create::expand(t)) == real(&was) => Ok(was.clone()),
            t => {
                let abs = create::expand(t);
                let taken = (self.group_hints)().groups.into_iter().find(|(x, d)| *x != group && !d.is_empty() && real(d) == real(&abs));
                match taken {
                    _ if !std::path::Path::new(&abs).is_dir() => Err(tr!("{} 不是目录", "{} is not a directory", create::tilde(&abs))),
                    Some((x, _)) => Err(tr!("该目录已是分组 {x} 的默认目录", "this directory is already the default of group {x}")),
                    None => Ok(real(&abs)),
                }
            }
        };
        let dir = match dir {
            Ok(d) => d,
            Err(hint) => return self.prompt = Some(Prompt { ask: Ask::GroupHome { group, to, was }, text, hint }),
        };
        let changed = dir != was;
        if to == group && !changed {
            return;
        }
        let dir = changed.then_some((was, dir));
        self.job = Some(Job::Group(groups::Act::Edit { profile: crate::data::REGISTERED.into(), group, to, dir }));
    }

    /// A group was renamed (an undo renaming it back counts): whatever the interface remembers of it — the group shown, folded ones,
    /// the group heading the highlight is on, workspace records — follows
    fn group_renamed(&mut self, from: &str, to: &str) {
        let r = |g: &str| groups::renamed(g, from, to);
        if let Some(g) = self.group.as_deref().and_then(r) {
            self.group = Some(g);
        }
        if let Some(g) = self.head.as_deref().and_then(r) {
            self.head = Some(g);
        }
        self.folded = std::mem::take(&mut self.folded).into_iter().map(|g| r(&g).unwrap_or(g)).collect();
        self.spot = None;
        // The workspace file's records were already changed in the background (`groups::rename_all`)
        self.spaces.reload();
    }

    /// m: pick a group in fzf to move into. Each row shows that group's
    /// directory, groups with the same directory first (m ⏎ puts it back home)
    fn ask_move(&mut self) {
        let Some(t) = self.deck_target(t!("移到分组", "move")) else { return };
        let (from, here) = self.current().map(|s| (s.group.clone(), s.dir.clone())).unwrap_or_default();
        self.pick_group = Some(self.group_pick(t, from, here, vec![]));
    }

    /// Groups m's fzf lists: if all moved ones are in one group, not that one; `here` is their shared directory (different = empty),
    /// groups with that directory first
    fn group_pick(&self, t: Target, from: String, here: String, more: Vec<(Target, String)>) -> GroupPick {
        let hints = (self.group_hints)().groups;
        let mut groups: Vec<String> = hints.iter().map(|(g, _)| g.clone()).collect();
        groups.sort();
        groups.dedup();
        // First in the list's section order, then those not in the list (empty)
        let mut cands: Vec<String> = self.groups().into_iter().filter(|g| groups.contains(g)).collect();
        let rest: Vec<String> = groups.iter().filter(|g| !cands.contains(g)).cloned().collect();
        cands.extend(rest);
        if more.iter().all(|(_, g)| *g == from) {
            cands.retain(|g| *g != from);
        }
        let real = aca_core::paths::real;
        let mine = real(&here);
        let mut rows: Vec<ops::GroupRow> = cands
            .into_iter()
            .map(|group| {
                let home = hints.iter().find(|(g, d)| *g == group && !d.is_empty()).map(|(_, d)| d.clone());
                let dir = home.unwrap_or_else(|| self.usual_dir(&group));
                let same = !dir.is_empty() && !here.is_empty() && real(&dir) == mine;
                ops::GroupRow { group, dir, same }
            })
            .collect();
        // Stable sort: the rest keep their order
        rows.sort_by_key(|r| !r.same);
        GroupPick { t, from, here, rows, groups, more }
    }

    /// The directory with the most sessions in the group (on a tie, the first in the list); no sessions in the group = empty.
    /// `m` shows this when a group has no default directory recorded
    fn usual_dir(&self, group: &str) -> String {
        let mut seen: Vec<(&str, usize)> = vec![];
        for s in self.all.iter().filter(|s| s.group == group && !s.dir.is_empty()) {
            match seen.iter_mut().find(|(d, _)| *d == s.dir) {
                Some((_, n)) => *n += 1,
                None => seen.push((&s.dir, 1)),
            }
        }
        // max_by_key takes the later one on a tie, so search reversed
        seen.iter().rev().max_by_key(|(_, n)| *n).map(|(d, _)| d.to_string()).unwrap_or_default()
    }

    /// f: fork a new session carrying the conversation, naming it first (the fork is identical to the original, without a name
    /// they can't be told apart): pre-filled with the default "<title> (fork)", ⏎ just uses it
    fn ask_fork(&mut self) {
        let Some(t) = self.deck_target(t!("分叉", "fork")) else { return };
        if self.current().is_none_or(|s| !matches!(s.agent, Agent::Claude | Agent::Codex | Agent::Pi | Agent::Opencode)) {
            return self.flash(t!("该工具不支持分叉", "this agent can't fork"));
        }
        self.prompt = Some(Prompt { text: fork_title(&t.title), ask: Ask::Fork(t), hint: String::new() });
    }

    /// X: switch account and carry on — the box shows its current account and quota and the accounts it can switch to
    /// (logged in or not, quota); ⏎ switches directly (see `rehome`)
    fn ask_rehome(&mut self) {
        self.digits.clear();
        let Some(t) = self.deck_target(t!("换账号", "switch accounts of")) else { return };
        let Some(s) = self.current().cloned() else { return };
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只有 Claude / Codex 会话可以切换账号", "only Claude / Codex sessions can switch accounts"));
        }
        let tools = match (self.tools_loader)() {
            Ok(v) => v,
            Err(e) => return self.flash(&e),
        };
        let env = crate::rehome::Env { ask: self.rehome_ask, background: self.rehome_background, accounts: self.accounts_page };
        let now = epoch() as i64;
        let menu = crate::rehome::Menu::new(t, &s, tools, &(self.rehome_quotas)(s.agent), (now, crate::usage::local_offset()), env);
        self.open(Box::new(menu));
    }

    /// H: hand off to a new session — first pick the new session's tool / account (`n`'s menu, without shell), create it in the
    /// same directory and group, send the handoff note, and have the new session read its conversation and carry on
    /// (see `handoff`). Its note goes along too
    fn ask_handoff(&mut self) {
        self.digits.clear();
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("交接", "hand off")) {
            return;
        }
        let Some(s) = self.current().cloned() else { return };
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能按 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            return self.flash(&m);
        }
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只能交接 Claude / Codex 会话", "only Claude / Codex sessions can be handed off"));
        }
        let mut from = match (self.source_loader)(&s) {
            Ok(f) => f,
            Err(e) => return self.flash(&e),
        };
        let key = notes::key(&s);
        from.note = self.notes.get(&key).map(String::from);
        let draft = Draft::from(Some(&s), self.group_hints);
        let menu: Vec<Tool> = match (self.tools_loader)() {
            Ok(v) => v.into_iter().filter(|t| t.agent != Agent::None).collect(),
            Err(e) => return self.flash(&e),
        };
        if menu.is_empty() {
            return self.flash(t!("工具菜单中没有可接手的工具(只有 shell)", "nothing in the tool menu can take over (only shell)"));
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        let note = from.note.is_some().then_some(key);
        let busy = matches!(s.state, State::Running | State::Ask);
        self.handoff = Some(Pass { t, from: Box::new(from), draft, note, busy });
        self.load_tool_quotas();
        self.tool_menu = Some((menu, 0));
    }

    /// M: this session's MCP
    fn open_mcp(&mut self) {
        let Some(t) = self.deck_target(t!("查看 MCP", "view the MCPs of")) else { return };
        let Some(s) = self.current() else { return };
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只有 Claude / Codex 会话有 MCP", "only Claude / Codex sessions have MCPs"));
        }
        let info = (self.mcp_loader)(s);
        let v = mcp::View::new(t, s.dir.clone(), info.rows);
        self.open(Box::new(v));
    }

    /// P: the mounts / remote tasks menu (not aimed at a session, opens on a group too)
    fn open_plugins(&mut self) {
        if let Some(page) = self.plugins_page {
            self.open(page());
        }
    }

    /// d on a group: the confirm box says subgroups and sessions inside are deleted with it, y deletes (see groups.rs)
    fn ask_remove_group(&mut self) {
        if let Some(b) = self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)) {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        let Some(g) = self.group_row().map(String::from) else { return };
        if g == "-" {
            return self.flash(t!("这些是普通 tmux 会话,没有可删除的分组", "these are ordinary tmux sessions; no group to delete"));
        }
        let rows: Vec<groups::Row> = (self.groups_loader)().into_iter().filter(|r| groups::within(&r.path, &g)).collect();
        if !rows.iter().any(|r| r.path == g) {
            return self.flash(&tr!("「{}」不是已登记的分组", "“{}” isn't a registered group", group_label(&g)));
        }
        let archived = rows.iter().flat_map(|r| &r.archived).filter(|id| !self.trash.ids().any(|x| x == id.as_str())).count();
        let mine = |s: &&Session| s.sub.is_none() && !s.blank && !s.deck_id.is_empty() && groups::within(&s.group, &g);
        let busy = self.all.iter().filter(mine).filter(|s| s.state == State::Running).count();
        let running = self.all.iter().filter(mine).filter(|s| !s.state.gone()).map(|s| s.deck_id.clone()).collect();
        self.confirm = Some(Confirm::Group(GroupRemoval {
            profile: crate::data::REGISTERED.into(),
            group: g,
            kids: rows.len() - 1,
            sessions: rows.iter().map(|r| r.live).sum::<usize>() + archived,
            archived,
            busy,
            running,
        }));
    }

    /// b: the trash (see trash.rs)
    fn open_trash(&mut self) {
        self.open(Box::new(crate::trash::Page::new(self.trash.clone(), epoch)));
    }

    /// Open a feature's own popup (see overlay.rs)
    fn open(&mut self, o: Box<dyn Overlay>) {
        self.overlays.push(o);
    }

    /// `:`: the command palette (aca's actions + plugins'), aimed at the highlighted row (session / group; plain tmux sessions and
    /// extra windows count as elsewhere)
    fn open_caps(&mut self) {
        let target = match (&self.head, self.current()) {
            (Some(g), _) => crate::caps::Target { on: "group", session: String::new(), label: g.clone() },
            (None, Some(s)) if !self.on_fold_row() && s.sub.is_none() && !s.deck_id.is_empty() => {
                crate::caps::Target { on: "session", session: s.deck_id.clone(), label: s.title.clone() }
            }
            _ => crate::caps::Target { on: "global", ..Default::default() },
        };
        self.open(Box::new(crate::caps::Menu::new(aca_core::plugin::menu(), target)));
    }

    /// A plugin finished: views and long output go into the full-screen reader page; short output goes in the bottom bar
    pub fn plugin_done(&mut self, req: crate::caps::Req, ok: bool, out: String) {
        use aca_core::plugin::ItemKind;
        // List view: replace the open page with the new one; if none is open, open one (not for rereads or one already closed)
        if req.item.kind == ItemKind::View && req.item.rows {
            // The "running / rereading…" message is stale by now: the list is there on its own
            self.msg = None;
            if let Some(o) = self.overlays.iter_mut().find(|o| o.takes(&req)) {
                o.feed(ok, &out);
            } else if !req.refresh {
                self.open(Box::new(crate::caps::List::new(req, ok, &out)));
            }
            return;
        }
        // A list key finished: the result goes below that page and the list rereads; if the page is already closed, handle it as usual
        if let Some(view) = req.then.as_deref()
            && let Some(o) = self.overlays.iter_mut().find(|o| o.takes(view))
        {
            o.said(ok, &out);
            self.plugin_q.push(view.clone());
            return;
        }
        let title = req.title();
        let text = out.trim_end();
        let one_line = !text.contains('\n') && text.width() <= 100;
        let mark = if ok { "✓" } else { "✕" };
        match req.item.kind {
            ItemKind::Pane if ok => {}
            ItemKind::Command if text.is_empty() => self.flash(&if ok {
                tr!("✓ {title} 已完成", "✓ {title} done")
            } else {
                tr!("✕ {title} 出错", "✕ {title} failed")
            }),
            ItemKind::Command | ItemKind::Pane if one_line => self.flash(&format!("{mark} {title}:{text}")),
            _ => {
                let lines = ansi::parse_lines(&text.lines().map(str::to_string).collect::<Vec<_>>());
                // The rerun one is open: replace it, keeping the scroll position
                let scroll = self.reader.as_ref().filter(|r| r.plugin.as_ref() == Some(&req)).map_or(usize::MAX, |r| r.scroll);
                self.trace = None;
                self.reader = Some(Reader {
                    name: format!("{mark} {title}"),
                    lines,
                    scroll,
                    wrapped: vec![],
                    wrap_w: 0,
                    dirty: true,
                    help: false,
                    view_h: 0,
                    diff: None,
                    hit: None,
                    plugin: Some(req),
                });
            }
        }
    }

    /// Keys go to the top popup; do what it asks, and when it closes the one below shows (none left and it came from the
    /// settings page: back to the settings page)
    fn on_overlay_key(&mut self, k: KeyEvent) {
        let Some(top) = self.overlays.last_mut() else { return };
        let mut fx = Fx::default();
        if top.on_key(k, &mut fx) == Flow::Close {
            self.overlays.pop();
        }
        self.apply(fx);
        if self.overlays.is_empty() && self.external.is_none() {
            self.back_to_settings();
        }
    }

    /// What a popup / settings row asks the interface to do
    fn apply(&mut self, fx: Fx) {
        if let Some(m) = fx.flash {
            self.flash(&m);
        }
        if let Some(j) = fx.job {
            match self.doing() {
                Some(b) => self.flash(&tr!("请稍候:{b}", "still busy: {b}")),
                None => self.job = Some(j),
            }
        }
        if let Some(x) = fx.external {
            self.external = Some(x);
        }
        if let Some(t) = fx.term {
            // The terminal takes the lower half of the screen: all popups are put
            // away, and closing the terminal doesn't return to the settings page
            self.overlays.clear();
            self.settings_back = None;
            self.term_req = Some(t);
        }
        if let Some(o) = fx.open {
            self.open(o);
        }
        if let Some(t) = fx.copy {
            self.clip = Some(Clip::Text(t));
        }
        if let Some(r) = fx.plugin {
            self.plugin_q.push(r);
        }
        if let Some(k) = fx.key {
            self.on_key(k);
        }
    }

    /// Background work finished, back from the editor: open popups reread
    fn reload_overlays(&mut self) {
        for o in &mut self.overlays {
            o.reload();
        }
    }

    /// Before m moves a group: a session that follows its group would follow to the new group's recorded workspace, so record where
    /// it is now and leave it there after the move
    fn stay_on_move(&mut self, name: &str, to: &str) {
        let mut after = self.all.clone();
        for s in after.iter_mut().filter(|s| s.family() == name) {
            s.group = to.to_string();
        }
        let sessions = self.settle(&after, &HashMap::new(), &[]);
        if !sessions.is_empty() {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&sessions, &[], &keep);
        }
    }

    /// Back from picking a group in fzf: the selected one or a newly typed name
    pub fn group_picked(&mut self, p: GroupPick, got: Result<Option<String>, String>) {
        match got {
            Ok(Some(g)) => match ops::clean_group(&g) {
                Some(to) if p.more.is_empty() && to.eq_ignore_ascii_case(&p.from) => self.flash(&tr!("已在 {to} 中", "already in {to}")),
                Some(to) if p.more.is_empty() => {
                    self.stay_on_move(&p.t.name, &to);
                    self.job = Some(Job::Move { t: p.t, from: p.from, to, groups: p.groups });
                }
                // Multi-selected ones move together: ones already in that group stay
                Some(to) => {
                    let moves: Vec<(Target, String)> =
                        std::iter::once((p.t, p.from)).chain(p.more).filter(|(_, from)| !to.eq_ignore_ascii_case(from)).collect();
                    if moves.is_empty() {
                        return self.flash(&tr!("均已在 {to} 中", "all already in {to}"));
                    }
                    for (t, _) in &moves {
                        self.stay_on_move(&t.name, &to);
                    }
                    let jobs = moves.into_iter().map(|(t, from)| Job::Move { t, from, to: to.clone(), groups: p.groups.clone() }).collect();
                    self.drop_picks();
                    self.job = Some(Job::Many(jobs));
                }
                None => {}
            },
            Ok(None) => {}
            Err(e) => self.flash(&e),
        }
    }

    /// e: write a note, the existing one pre-filled
    fn ask_note(&mut self) {
        let Some((key, title)) = self.note_target() else { return };
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        self.prompt = Some(Prompt { ask: Ask::Note { key, title, orig: text.clone() }, text, hint: String::new() });
    }

    /// Whose note e / ^G writes: (key, what it's called). On a group (heading /
    /// fold row) it's the group's, otherwise the highlighted session's
    fn note_target(&self) -> Option<(String, String)> {
        match self.group_row() {
            Some(g) => Some((group_key(g), group_label(g).to_string())),
            None => self.current().map(|s| (notes::key(s), s.title.clone())),
        }
    }

    /// E / Shift+Enter: go in with the note — pasted into its input box before going in (not sent), deleted once filled. No note = Enter.
    /// Only fills Claude / Codex (other programs, like vim, would take the pasted text as commands); not while waiting for your choice (!)
    /// — the text would be taken as an option. If it can't fill, say why and don't go in (so you don't think it filled)
    fn enter_with_note(&mut self) {
        if self.on_group_row() {
            return self.toggle_fold();
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.enter() };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("未附带便签:{w}", "didn't go in with the note: {w}"));
        }
        self.fill = Some((key, text));
        self.enter();
    }

    /// s: send the note to it directly (pasted into the input box, then Enter), you stay in the list. No note sends nothing;
    /// with one it still asks first, against slips
    fn ask_send(&mut self) {
        self.digits.clear();
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("发便签", "send the note")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.flash(t!("没有便签", "no note")) };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("未发送:{w}", "not sent: {w}"));
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        let pane = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        self.confirm = Some(Confirm::Send { t, pane, key, text });
    }

    /// ^G (in the list): write the highlighted session's / group's note in the editor
    fn note_in_editor(&mut self) {
        let Some((key, title)) = self.note_target() else { return };
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        self.edit_note = Some(NoteEdit { key, title, orig: text.clone(), text, from_prompt: false });
    }

    /// Back from the editor: save it; if it came from bottom bar editing, go back there to keep writing
    pub fn note_editor_done(&mut self, ed: NoteEdit, got: Result<String, String>) {
        let (text, err) = match got {
            Ok(t) => (t, None),
            Err(e) => (ed.text.clone(), Some(e)),
        };
        if err.is_none() {
            self.save_note(&ed.key, &text);
        }
        if ed.from_prompt {
            let hint = err.unwrap_or_default();
            self.prompt = Some(Prompt { ask: Ask::Note { key: ed.key, title: ed.title, orig: ed.orig }, text, hint });
        } else if let Some(e) = err {
            self.flash(&e);
        } else {
            self.note_done(ed.key, ed.title, ed.orig);
            self.note_saved_flash(&text);
        }
    }

    /// Save the note whenever it changes (every keystroke: when a phone disconnects aca is killed outright, it never gets to exit)
    fn save_note(&mut self, key: &str, text: &str) {
        if let Err(e) = self.notes.set(key, text) {
            self.flash(&tr!("便签保存失败:{e}", "note not saved: {e}"));
        }
        self.apply_pins();
    }

    /// The note is done: if it differs from before writing started, record an undo step
    fn note_done(&mut self, key: String, title: String, orig: String) {
        if self.notes.get(&key).unwrap_or_default() != orig {
            self.remember(Undo::Note { key, title, text: orig });
        }
    }

    fn note_saved_flash(&mut self, text: &str) {
        self.flash(if text.trim().is_empty() { t!("便签已清空", "note cleared") } else { t!("便签已保存", "note saved") });
    }

    /// The note was filled in: delete it
    pub fn note_filled(&mut self, key: &str) {
        let _ = self.notes.take(key);
        self.apply_pins();
    }

    /// ^N: name the new group first, after Enter go straight into fzf to pick a directory (first row is the default directory);
    /// the new session opens in this group
    fn ask_new_group(&mut self) {
        self.prompt = Some(Prompt { ask: Ask::NewGroup, text: String::new(), hint: String::new() });
    }

    /// The new group named with ^N: created in the background (when done the highlight goes to its heading, see `job_done`).
    /// If not created in 1, the group is recorded in this workspace (it shows here in the list, and new ones in it later land here)
    fn add_group(&mut self, g: &str) {
        if self.space != spaces::HOME {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&[], &[((crate::data::REGISTERED.into(), g.to_string()), Some(self.space))], &keep);
        }
        self.job = Some(Job::Group(groups::Act::Create { group: g.to_string() }));
    }

    fn on_prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = self.prompt.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Ask::Note { key, title, orig } = &p.ask {
            // Note: Enter is a newline, Esc leaves (already saved long ago), ^G hands it to the editor
            match k.code {
                KeyCode::Esc => {
                    let (key, title, orig) = (key.clone(), title.clone(), orig.clone());
                    let text = p.text.trim_end().to_string();
                    self.prompt = None;
                    self.save_note(&key, &text);
                    self.note_done(key, title, orig);
                    return self.note_saved_flash(&text);
                }
                KeyCode::Char('g') if ctrl => {
                    let ed =
                        NoteEdit { key: key.clone(), title: title.clone(), orig: orig.clone(), text: p.text.clone(), from_prompt: true };
                    self.prompt = None;
                    self.edit_note = Some(ed);
                    return;
                }
                KeyCode::Enter => p.text.push('\n'),
                KeyCode::Char('j') if ctrl => p.text.push('\n'), // a pasted newline
                KeyCode::Backspace => {
                    p.text.pop();
                }
                KeyCode::Char('u') if ctrl => p.text.clear(),
                KeyCode::Char('w') if ctrl => {
                    let t = p.text.trim_end_matches([' ', '\n']);
                    let cut = t.rfind([' ', '\n']).map_or(0, |i| i + 1);
                    p.text.truncate(cut);
                }
                KeyCode::Char(c) if !ctrl => p.text.push(c),
                _ => return,
            }
            p.hint.clear();
            let (key, text) = (key.clone(), p.text.clone());
            return self.save_note(&key, &text);
        }
        match k.code {
            KeyCode::Enter => return self.finish_prompt(),
            KeyCode::Tab if matches!(p.ask, Ask::GroupDir | Ask::GroupHome { .. }) => match create::complete_path(&p.text) {
                Some(t) => p.text = t,
                None => return,
            },
            KeyCode::Esc => {
                self.prompt = None;
                if self.settings_back.is_some() {
                    return self.back_to_settings();
                }
                return;
            }
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Char('u') if ctrl => p.text.clear(),
            // Delete the last word (along with the space / the group's `/` next to it)
            KeyCode::Char('w') if ctrl => {
                let t = p.text.trim_end_matches([' ', '/']);
                let cut = t.rfind([' ', '/']).map_or(0, |i| i + 1);
                p.text.truncate(cut);
            }
            KeyCode::Char(c) if !ctrl => p.text.push(c),
            _ => return,
        }
        p.hint.clear();
    }

    fn finish_prompt(&mut self) {
        let Some(Prompt { ask, text, .. }) = self.prompt.take() else { return };
        match ask {
            Ask::Rename(t) => {
                let to = text.trim().to_string();
                if to.is_empty() {
                    self.prompt = Some(Prompt { ask: Ask::Rename(t), text, hint: t!("标题不能为空", "the title is empty").into() });
                } else if to != t.title {
                    self.job = Some(Job::Rename { t, to, unlock: false });
                }
            }
            Ask::Fork(t) => {
                let to = text.trim().to_string();
                if to.is_empty() {
                    self.prompt = Some(Prompt { ask: Ask::Fork(t), text, hint: t!("标题不能为空", "the title is empty").into() });
                } else {
                    // Unchanged: use the default (not locked, Claude can still change it later); changed: lock it
                    let title = (to != fork_title(&t.title)).then_some(to);
                    self.job = Some(Job::Fork { t, title });
                }
            }
            // Enter in a note is a newline, never gets here
            Ask::Note { .. } => {}
            Ask::GroupDir => {
                let dir = text.trim().to_string();
                let r = if dir.is_empty() {
                    Err(t!("目录不能为空", "the directory is empty").to_string())
                } else {
                    (self.config_set)("new_group", "default_path", toml::Value::String(dir))
                };
                match r {
                    Ok(()) => self.back_to_settings(),
                    Err(hint) => self.prompt = Some(Prompt { ask: Ask::GroupDir, text, hint }),
                }
            }
            Ask::GroupName(group) => self.group_name_given(group, text),
            Ask::GroupHome { group, to, was } => self.group_home_given(group, to, was, text),
            Ask::Space(c) => {
                let m = match self.spaces.rename(c, &text) {
                    Ok(()) => tr!("工作区 {c}:{}", "workspace {c}: {}", self.spaces.name(c)),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.flash(&m);
            }
            Ask::NewGroup => {
                let Some(mut name) = ops::clean_group(&text) else {
                    self.prompt =
                        Some(Prompt { ask: Ask::NewGroup, text, hint: t!("分组名不能为空", "the group name is empty").into() });
                    return;
                };
                match self.draft() {
                    Ok(mut d) => {
                        match d.hints.groups.iter().find(|(g, _)| g.eq_ignore_ascii_case(&name)) {
                            // Actually exists already (different case): use the existing one
                            Some((g, _)) => name = g.clone(),
                            // New: create it now, the group stays even if Esc is pressed midway
                            None => self.add_group(&name),
                        }
                        d.start_group(name, &(self.new_group_dir)());
                        self.new_draft = Some(d);
                        self.pick = true; // Esc out of it lands in the input box with the default directory filled
                    }
                    Err(e) => self.flash(&e),
                }
            }
        }
    }

    /// Work the run loop should put in the background; also record "busy" and whether the highlight should follow the restart.
    /// Deletes don't record "busy" (`vanish` tracks them)
    pub fn take_job(&mut self) -> Option<Job> {
        if self.job.is_none()
            && self.doing().is_none()
            && !self.reorder_writing
            && let Some(job) = self.reorders.front().cloned()
        {
            self.reorder_writing = true;
            self.job = Some(job);
        }
        let job = self.job.take()?;
        // Earlier messages give way to the "…ing…" status; ⇧↑ ⇧↓ writes don't count
        // (each write while pressing repeatedly would wipe other messages)
        if !matches!(job, Job::Order { .. } | Job::GroupOrder { .. }) {
            self.msg = None;
        }
        if !deleting(&job) {
            self.busy = Some(job.doing());
            self.follow = job.target().is_some_and(|t| self.selected.as_deref() == Some(t.name.as_str()));
        }
        Some(job)
    }

    /// The "…ing…" status in the bottom bar, also what other operations wait for: other work, or deletes still running (can be several)
    fn doing(&self) -> Option<String> {
        if let Some(b) = &self.busy {
            return Some(b.clone());
        }
        let live: Vec<&Job> = self.vanish.iter().filter(|v| v.until.is_none()).map(|v| &v.job).collect();
        // A batch deleted together counts by sessions
        let n: usize = live.iter().map(|j| if let Job::Many(v) = j { v.len() } else { 1 }).sum();
        match live[..] {
            [] => None,
            [j] => Some(j.doing()),
            _ if live.iter().all(|j| matches!(j, Job::Remove { pane: Some(_), .. })) => Some(tr!("正在关闭 {n} 个…", "closing {n}…")),
            _ => Some(tr!("正在删除 {n} 个…", "deleting {n}…")),
        }
    }

    /// Remove from all what d deleted (still deleting, just deleted); child sessions hanging under it are counted as after the
    /// delete (deleting a parent doesn't take the child sessions with it)
    fn drop_vanished(&mut self) {
        let now = Instant::now();
        self.vanish.retain(|v| v.until.is_none_or(|u| now < u));
        if self.vanish.is_empty() {
            return;
        }
        let vanish = &self.vanish;
        self.all.retain(|s| !vanish.iter().any(|v| v.hides(s)));
        let heads: Vec<&str> = vanish
            .iter()
            .flat_map(|v| match &v.job {
                Job::Many(jobs) => jobs.iter().collect(),
                j => vec![j],
            })
            .filter_map(|j| match j {
                Job::Remove { t, pane: None } => Some(t.name.as_str()),
                _ => None,
            })
            .collect();
        for s in &mut self.all {
            if s.up.parent().is_some_and(|p| heads.contains(&p)) {
                s.up = Up::Gone;
            }
        }
    }

    /// Background work done. fresh is the sessions reread afterwards: a restart
    /// changes the tmux session name, so pins, highlight and seen records follow
    pub fn job_done(&mut self, job: &Job, out: Outcome, fresh: Vec<Session>) {
        let mut was_running = None;
        if deleting(job) {
            // Deleted (into the trash): hide it a while longer; not deleted: put it back
            let ok = matches!(out, Outcome::Done(_) | Outcome::Binned { .. });
            let at = self.vanish.iter().position(|v| v.until.is_none() && v.job == *job);
            was_running = at.map(|i| self.vanish[i].running.clone());
            match at {
                Some(i) if ok => self.vanish[i].until = Some(Instant::now() + GONE_LINGER),
                Some(i) => drop(self.vanish.remove(i)),
                None => {}
            }
        } else {
            self.busy = None;
        }
        if let Job::Order { .. } | Job::GroupOrder { .. } = job {
            self.reorder_writing = false;
            self.reorders.pop_front();
            // Write failed, the queued ones after it don't count either, the list goes back to the registry's order
            if matches!(out, Outcome::Failed(_)) {
                self.reorders.clear();
            }
        }
        let follow = std::mem::take(&mut self.follow);
        if let (Job::Group(groups::Act::Edit { group, to, .. }), Outcome::Done(_)) = (job, &out)
            && group != to
        {
            self.group_renamed(group, to);
        }
        if let Some((_, u, msg)) = self.reverting.take_if(|(j, ..)| j == job) {
            return self.reverted(job, u, msg, out, fresh);
        }
        // Done and undoable: record a step
        // What it was like before (running or not, title locked or not)
        let (running, locked) = self
            .all
            .iter()
            .find(|s| s.sub.is_none() && job.target().is_some_and(|t| s.deck_id == t.id && s.profile == t.profile))
            .map_or((false, true), |s| (!s.state.gone(), s.locked));
        let running = match (was_running, job.target()) {
            (Some(v), _) => v,
            (None, Some(t)) if running => vec![t.id.clone()],
            _ => vec![],
        };
        let mut out = out;
        // Deleted by auto-cleanup: say it was automatic and stopped for how long
        if let Some((_, after)) = self.swept.take_if(|(j, _)| j == job)
            && let Outcome::Binned { msg, item } = &mut out
        {
            let (age, who) = (crate::print::age(after), item.name());
            *msg = tr!("已自动删除停止超过 {age} 的{who} · ^Z 撤销", "auto-deleted {who}, stopped over {age} · ^Z undoes");
        }
        // A session / group deleted with d went to the trash: record the entry (no longer listed), ^Z brings it back
        if let Outcome::Binned { item, .. } = &mut out {
            let mut item = *std::mem::take(item);
            // The group's recorded workspace and its note: restored as before when brought back
            self.notes.reload();
            for g in &mut item.groups {
                g.space = self.spaces.group_of(&item.profile, &g.path);
                g.note = self.notes.get(&group_key(&g.path)).unwrap_or_default().to_string();
            }
            let u = Undo::Bin { id: item.id.clone(), label: item.name(), start: running };
            let gone = item.groups.clone();
            match self.trash.add(item) {
                Ok(()) => {
                    self.remember(u);
                    // The group's records and note are cleared: a later group of the same
                    // name shouldn't land in the old place or carry the old note
                    if !gone.is_empty() {
                        let profile = crate::data::REGISTERED;
                        let _ = self.spaces.forget_groups(&|p, g| p == profile && gone.iter().any(|x| x.path == g));
                    }
                    for g in gone.iter().filter(|g| !g.note.is_empty()) {
                        let _ = self.notes.set(&group_key(&g.path), "");
                    }
                }
                Err(e) => out = Outcome::Failed(tr!("已删除,但未能记入回收站:{e}", "deleted, but the trash didn't record it: {e}")),
            }
        }
        // An entry in the trash was brought back / deleted for real
        match (job, &out) {
            (Job::Restore { item, .. }, Outcome::Done(m)) => return self.restored(item, fresh, m),
            (Job::Purge { item }, Outcome::Done(_)) => drop(self.trash.forget(&item.id)),
            _ => {}
        }
        if let Outcome::Done(_) = &out
            && let Some(u) = undo_of(job, locked)
        {
            self.remember(u);
        }
        // Done together: the ones that succeeded become one step, undone together by ^Z. Brought back from the trash together:
        // the highlight goes to the first one
        let mut landed = None;
        if let (Job::Many(jobs), Outcome::Many(outs)) = (job, &out) {
            let done = || jobs.iter().zip(outs).filter(|(_, o)| matches!(o, Outcome::Done(_))).map(|(j, _)| j);
            let us: Vec<Undo> = done().filter_map(|j| undo_of(j, true)).collect();
            if !us.is_empty() {
                self.remember(Undo::Many(us));
            }
            for j in done() {
                match j {
                    Job::Restart { t, .. } => {
                        let follow = self.selected.as_deref() == Some(t.name.as_str());
                        self.restarted(t, &fresh, follow);
                    }
                    Job::Restore { item, .. } => {
                        self.unbin(item);
                        if landed.is_none() {
                            self.land(item, &fresh);
                            landed = Some(item);
                        }
                    }
                    Job::Purge { item } => drop(self.trash.forget(&item.id)),
                    _ => {}
                }
            }
        }
        if let (Job::Restart { t, .. } | Job::Rehome { t, .. }, Outcome::Done(_)) = (job, &out) {
            self.restarted(t, &fresh, follow);
        }
        // Adopted: the row's key changed from the tmux session name to the registered id, pins follow
        if let (Job::Adopt { t, .. }, Outcome::Made { id, .. }) = (job, &out) {
            let _ = self.pins.rename(&t.name, id);
        }
        if let (Job::Send { key, .. }, Outcome::Done(_)) = (job, &out) {
            self.note_filled(key);
        }
        // Hand off: the note went with the handoff note to the new session
        if let (Job::Handoff { note: Some(key), .. }, Outcome::Made { .. } | Outcome::Handed { .. }) = (job, &out) {
            self.note_filled(key);
        }
        self.set_sessions(fresh);
        if let Some(item) = landed {
            self.land_on_group(item);
        }
        // A group made with ^N is still empty: the highlight goes to its heading
        // (even while the new session box is open, so Esc lands here)
        if let (Job::Group(groups::Act::Create { group }), Outcome::Done(_)) = (job, &out)
            && let Some(p) = self.head_pos(group).filter(|&p| self.all[self.view[p]].blank)
        {
            self.select_head(p);
        }
        self.reload_overlays();
        match out {
            Outcome::Done(m) | Outcome::Failed(m) | Outcome::Binned { msg: m, .. } if !m.is_empty() => self.flash(&m),
            Outcome::Done(_) | Outcome::Failed(_) | Outcome::Binned { .. } => {}
            Outcome::Many(outs) => {
                if let Job::Many(jobs) = job {
                    self.flash(&ops::many_msg(jobs, &outs));
                }
            }
            Outcome::Made { msg, id } => self.made(&id, &msg),
            Outcome::Handed { msg, id, note } => {
                self.save_note(&id, &note);
                self.made(&id, &msg);
            }
        }
    }

    /// After a restart / account switch the tmux session name changed: pins follow, and the highlight too with follow
    fn restarted(&mut self, t: &Target, fresh: &[Session], follow: bool) {
        let new = fresh.iter().find(|s| s.deck_id == t.id && s.profile == t.profile).map(|s| s.name.clone());
        if let Some(new) = new.filter(|n| *n != t.name) {
            let _ = self.pins.rename(&t.name, &new);
            if follow {
                self.selected = Some(new);
            }
        }
    }

    /// A new session forked / handed off: lands in the current workspace and gets the highlight (if filtered out by the current
    /// scope / filter, the highlight stays put)
    fn made(&mut self, id: &str, msg: &str) {
        self.adopt(id);
        self.apply_pins();
        self.rebuild();
        let name = self.all.iter().find(|s| !id.is_empty() && s.deck_id == id && s.sub.is_none()).map(|s| s.name.clone());
        if let Some(p) = name.and_then(|n| self.view.iter().position(|&i| self.all[i].name == n)) {
            self.select(Some(p));
        }
        self.flash(msg);
    }

    /// Record a step ^Z can undo
    fn remember(&mut self, u: Undo) {
        undo::push(&mut self.undo, u);
    }

    /// An entry in the trash was brought back (⏎ on b's page, ^Z): the group's recorded workspace as before, no longer hidden,
    /// the highlight on it (a group: on its heading)
    fn restored(&mut self, item: &crate::trash::Item, fresh: Vec<Session>, msg: &str) {
        self.unbin(item);
        self.land(item, &fresh);
        self.set_sessions(fresh);
        self.land_on_group(item);
        self.reload_overlays();
        self.flash(msg);
    }

    /// An entry brought back: removed from the trash, the group's recorded workspace and note as before, no longer hidden
    fn unbin(&mut self, item: &crate::trash::Item) {
        let _ = self.trash.forget(&item.id);
        let spaces: Vec<(spaces::Group, Option<char>)> =
            item.groups.iter().filter(|g| g.space.is_some()).map(|g| ((item.profile.clone(), g.path.clone()), g.space)).collect();
        if !spaces.is_empty() {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&[], &spaces, &keep);
        }
        // A group of the same name was created and a note written in the meantime: the one brought back is appended, both kept
        self.notes.reload();
        for g in item.groups.iter().filter(|g| !g.note.is_empty()) {
            let key = group_key(&g.path);
            let text = match self.notes.get(&key) {
                Some(now) if now != g.note => format!("{now}\n\n{}", g.note),
                _ => g.note.clone(),
            };
            let _ = self.notes.set(&key, &text);
        }
        self.vanish.retain(|v| !v.undone_by(item));
    }

    /// A session was brought back (several deleted together: the first): after the list is replaced, the highlight goes to it
    fn land(&mut self, item: &crate::trash::Item, fresh: &[Session]) {
        let one = item.sessions.first().filter(|_| item.group.is_empty());
        if let Some(s) = one.and_then(|g| fresh.iter().find(|s| s.sub.is_none() && s.deck_id == g.id && s.profile == item.profile)) {
            self.selected = Some(s.name.clone());
        }
    }

    /// A group was brought back: the highlight goes to its heading (after the list is replaced)
    fn land_on_group(&mut self, item: &crate::trash::Item) {
        if !item.group.is_empty()
            && let Some(p) = self.head_pos(&item.group)
        {
            self.select_head(p);
        }
    }

    /// The session undo looks for: the one still listed (its tmux name may have changed with a restart). If gone, say so, return None
    fn still_here(&mut self, t: &Target) -> Option<(Target, &Session)> {
        if self.here(t).is_none() {
            self.flash(&tr!("「{}」已不存在", "“{}” is gone", clip(&t.title, 16).trim_end()));
            return None;
        }
        self.here(t)
    }

    /// Like `still_here`, but says nothing if gone (one missing among several undone together is no big deal)
    fn here(&self, t: &Target) -> Option<(Target, &Session)> {
        let s = self.all.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile)?;
        Some((Target { name: s.name.clone(), title: s.title.clone(), ..t.clone() }, s))
    }

    /// Find a session's current tmux name by key (`notes::key`) (a restart changes it); if not found, use the recorded one
    fn name_now(&self, key: &str, name: String) -> String {
        self.all.iter().find(|s| s.sub.is_none() && notes::key(s) == key).map_or(name, |s| s.name.clone())
    }

    /// ^Z: undo the last step (see undo.rs)
    fn undo_last(&mut self) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        let Some(u) = self.undo.pop() else { return self.flash(t!("没有可撤销的操作", "nothing to undo")) };
        let keep = self.keep_marks();
        let short = |t: &str| clip(t, 16).trim_end().to_string();
        match u.clone() {
            Undo::Gone { title } => self.flash(&tr!("「{}」无法撤销", "“{}” can't be undone", short(&title))),
            Undo::Pin { key, name, title, rank } => {
                let name = self.name_now(&key, name);
                let m = match self.pins.put(&name, rank) {
                    Ok(()) => tr!(
                        "已撤销{}「{}」",
                        "undid {} “{}”",
                        if rank.is_some() { t!("取消置顶", "unpin") } else { t!("置顶", "pin") },
                        short(&title)
                    ),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.selected = Some(name);
                self.flash(&m);
                self.apply_pins();
                self.rebuild();
            }
            Undo::Read { key, name, title, what, mark, state, seen } => {
                let name = self.name_now(&key, name);
                if let Some(t) = seen {
                    aca_core::runtime::update(&name, |r| r.seen = t);
                }
                let m = match self.reads.put(&key, mark, &keep) {
                    Ok(()) => tr!("已撤销标为{what}「{}」", "undid marking “{}” {what}", short(&title)),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                // The list's state has the read records laid over it, recomputing would lose the original: put it back directly,
                // the next refresh computes from the new records
                if let Some(s) = self.all.iter_mut().find(|s| s.name == name) {
                    s.state = state;
                }
                self.selected = Some(name);
                self.flash(&m);
                self.rebuild();
            }
            Undo::Space { what, sessions, groups } => {
                let m = match self.spaces.assign(&sessions, &groups, &keep) {
                    Ok(()) => tr!("已撤销 {what}", "undid {what}"),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.flash(&m);
                self.apply_pins();
                self.rebuild();
            }
            Undo::Note { key, title, text } => {
                let m = match self.notes.set(&key, &text) {
                    Ok(()) => tr!("已撤销修改便签「{}」", "undid the note change on “{}”", short(&title)),
                    Err(e) => tr!("便签保存失败:{e}", "note not saved: {e}"),
                };
                self.flash(&m);
                self.apply_pins();
            }
            Undo::Bin { id, label, start } => {
                self.trash.reload();
                let Some(item) = self.trash.get(&id).cloned() else {
                    return self.flash(&tr!("回收站中已没有{label}", "{label} is no longer in the trash"));
                };
                self.revert(Job::Restore { item: Box::new(item), start }, u, None);
            }
            Undo::Park { t, p } => {
                if !self.all.iter().any(|s| s.pane == p.id && s.state == State::Dead) {
                    return self.flash(&tr!("「{}」已不是停止状态", "“{}” isn't stopped any more", short(&p.label)));
                }
                let m = tr!("已撤销停止:「{}」已在原位置重新启动", "undid the stop: “{}” started again in place", short(&p.label));
                self.revert(Job::Respawn { t, p }, u, Some(m));
            }
            Undo::Stop { t } => {
                let Some((t, s)) = self.still_here(&t) else { return };
                if !s.state.gone() && s.state != State::Dead {
                    return self.flash(&tr!("「{}」已在运行", "“{}” is already running", short(&t.title)));
                }
                self.revert(Job::Revive { t }, u, None);
            }
            Undo::Move { t, from, groups } => {
                let Some((t, s)) = self.still_here(&t) else { return };
                let now = s.group.clone();
                if now.eq_ignore_ascii_case(&from) {
                    return self.flash(&tr!("已在 {from} 中", "already in {from}"));
                }
                let m = tr!("已撤销移动分组:「{}」回到 {from}", "undid the move: “{}” is back in {from}", short(&t.title));
                self.stay_on_move(&t.name, &from);
                self.revert(Job::Move { t, from: now, to: from, groups }, u, Some(m));
            }
            Undo::Rename { t, from, unlock } => {
                let Some((t, _)) = self.still_here(&t) else { return };
                let m = tr!("已撤销重命名:恢复为「{}」", "undid the rename: back to “{}”", short(&from));
                self.revert(Job::Rename { t, to: from, unlock }, u, Some(m));
            }
            Undo::Mcp { t, dir, on, off } => {
                let Some((t, _)) = self.still_here(&t) else { return };
                let m = tr!(
                    "已撤销修改 MCP「{}」 · 重启后生效",
                    "undid the MCP change on “{}” · takes effect after a restart",
                    short(&t.title)
                );
                self.revert(Job::Mcp { t, dir, on, off }, u, Some(m));
            }
            Undo::Group(a) => {
                let m = match &a {
                    groups::Act::Edit { group, to, .. } if group != to => {
                        tr!("已撤销修改分组:{group} 改回 {to}", "undid the group change: {group} back to {to}")
                    }
                    _ => t!("已撤销修改分组默认目录", "undid the change to the group's default directory").to_string(),
                };
                self.revert(Job::Group(a), u, Some(m));
            }
            Undo::Many(us) => self.undo_many(&us, u),
        }
    }

    /// ^Z undoing a step done together via v multi-select: pins and read marks restored on the spot; stops and group moves undone
    /// together in the background (ones gone or already back are skipped)
    fn undo_many(&mut self, us: &[Undo], u: Undo) {
        let keep = self.keep_marks();
        let (mut put, mut jobs, mut err) = (0, vec![], None);
        // Put back in reverse: pin order as before
        for x in us.iter().rev().cloned() {
            match x {
                Undo::Pin { key, name, rank, .. } => {
                    let name = self.name_now(&key, name);
                    match self.pins.put(&name, rank) {
                        Ok(()) => put += 1,
                        Err(e) => err = Some(e.to_string()),
                    }
                }
                Undo::Read { key, name, mark, state, seen, .. } => {
                    let name = self.name_now(&key, name);
                    if let Some(t) = seen {
                        aca_core::runtime::update(&name, |r| r.seen = t);
                    }
                    match self.reads.put(&key, mark, &keep) {
                        Ok(()) => put += 1,
                        Err(e) => err = Some(e.to_string()),
                    }
                    if let Some(s) = self.all.iter_mut().find(|s| s.name == name) {
                        s.state = state;
                    }
                }
                Undo::Stop { t } => {
                    if let Some((t, s)) = self.here(&t)
                        && (s.state.gone() || s.state == State::Dead)
                    {
                        jobs.push(Job::Revive { t });
                    }
                }
                Undo::Move { t, from, groups } => {
                    if let Some((t, s)) = self.here(&t)
                        && !s.group.eq_ignore_ascii_case(&from)
                    {
                        let now = s.group.clone();
                        self.stay_on_move(&t.name, &from);
                        jobs.push(Job::Move { t, from: now, to: from, groups });
                    }
                }
                _ => {}
            }
        }
        jobs.reverse();
        let n = jobs.len();
        let m = match us.first() {
            Some(Undo::Pin { rank: Some(_), .. }) => tr!("已撤销取消置顶 {put} 个", "undid unpinning {put}"),
            Some(Undo::Pin { .. }) => tr!("已撤销置顶 {put} 个", "undid pinning {put}"),
            Some(Undo::Read { what, .. }) => tr!("已撤销标为{what} {put} 个", "undid marking {put} {what}"),
            Some(Undo::Stop { .. }) => tr!("已撤销停止:{n} 个已重新启动", "undid the stop: {n} started again"),
            Some(Undo::Move { .. }) => tr!("已撤销移动分组:{n} 个回到原分组", "undid the move: {n} back in their groups"),
            _ => String::new(),
        };
        if !jobs.is_empty() {
            return self.revert(Job::Many(jobs), u, Some(m));
        }
        let m = match err {
            Some(e) => tr!("保存失败:{e}", "not saved: {e}"),
            None if put == 0 => t!("没有可撤销的操作", "nothing left to undo").to_string(),
            None => m,
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// Undo work done in the background: queue it, `reverted` finishes up when done
    fn revert(&mut self, job: Job, u: Undo, msg: Option<String>) {
        self.reverting = Some((job.clone(), u, msg));
        self.job = Some(job);
    }

    fn reverted(&mut self, job: &Job, u: Undo, msg: Option<String>, out: Outcome, fresh: Vec<Session>) {
        let m = match out {
            Outcome::Done(m) => {
                if let Job::Restore { item, .. } = job {
                    return self.restored(item, fresh, &msg.unwrap_or(m));
                }
                // The highlight goes to the one brought back
                if let Some(t) = u.target()
                    && let Some(s) = fresh.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile)
                {
                    self.selected = Some(s.name.clone());
                }
                msg.unwrap_or(m)
            }
            // Undo failed: put this step back, ^Z can be pressed again
            Outcome::Failed(m) => {
                self.undo.push(u);
                m
            }
            // Undone together: put back only if none succeeded; a partial undo is reported as it is
            Outcome::Many(outs) => {
                let jobs = if let Job::Many(v) = job { v.as_slice() } else { &[] };
                let ok = outs.iter().filter(|o| matches!(o, Outcome::Done(_))).count();
                if ok == 0 {
                    self.undo.push(u);
                }
                match msg {
                    Some(m) if ok == outs.len() => m,
                    _ => ops::many_msg(jobs, &outs),
                }
            }
            Outcome::Made { msg, .. } | Outcome::Handed { msg, .. } | Outcome::Binned { msg, .. } => msg,
        };
        self.set_sessions(fresh);
        self.flash(&m);
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

    fn ask_new(&mut self) {
        match self.draft() {
            Ok(d) => {
                self.new_draft = Some(d);
                self.pick = self.pick_first();
            }
            Err(e) => self.flash(&e),
        }
    }

    /// Where N / n start filling from: on a session, another one like it; on a group, the group's path (see `group_dir`), into
    /// this group. Empty group: into this group (it stays even if the path changes), starting from the group's default directory,
    /// or if none recorded, from the new group directory like ^N
    fn draft(&self) -> Result<Draft, String> {
        if let Some(g) = self.blank_group() {
            let mut d = Draft::from(None, self.group_hints);
            let base = self.group_dir(g).map_or_else(|| (self.new_group_dir)(), |(_, dir, _)| dir);
            d.start_group(g.to_string(), &base);
            return Ok(d);
        }
        match self.group_row().and_then(|g| Some((g, self.group_dir(g)?))) {
            Some((g, (s, dir, _))) => Ok(Draft::in_group(s, g, &dir, self.group_hints)),
            None => Ok(Draft::from(self.current(), self.group_hints)),
        }
    }

    /// Typing (filter words / a new session's path): several keys arriving at once
    /// then is IME suggestions or a paste, not Moshi typing on your behalf
    pub fn typing(&self) -> bool {
        self.filtering
            || self.new_draft.is_some()
            || self.prompt.is_some()
            || self.term_on.is_some()
            || self.overlays.last().is_some_and(|o| o.typing())
    }

    fn on_draft_key(&mut self, k: KeyEvent) {
        let Some(d) = self.new_draft.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => match d.finish() {
                Ok(r) => {
                    self.create = Some(r);
                    self.new_draft = None;
                }
                Err(e) => d.hint = e,
            },
            KeyCode::Esc => self.new_draft = None,
            KeyCode::Tab => d.complete(),
            KeyCode::BackTab => d.complete_back(),
            KeyCode::Right => d.accept(),
            KeyCode::Down => self.pick = true,
            KeyCode::Char('f') if ctrl => self.pick = true,
            KeyCode::Backspace => d.backspace(),
            KeyCode::Char('u') if ctrl => d.clear(),
            KeyCode::Char('w') if ctrl => d.kill_segment(),
            KeyCode::Char('t') if ctrl => d.shell = !d.shell,
            KeyCode::Char(c) if !ctrl => d.push(c),
            _ => {}
        }
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

    /// Clicked a desktop notification (see clicked.rs): go into that session, move the highlight onto it (if it's in the list)
    pub fn open_clicked(&mut self, id: &str) {
        let Some(i) = self.all.iter().position(|s| s.deck_id == id && s.sub.is_none() && s.subagent.is_none() && !s.blank) else {
            return self.flash(t!("通知中的会话已不存在", "the notification's session is gone"));
        };
        if let Some(p) = self.view.iter().position(|&v| v == i) {
            self.select(Some(p));
        }
        self.go(i);
    }

    /// t: open a temporary terminal in the directory it's really working in, drawn below the list; if this row already has one, back to it.
    /// Works for stopped sessions too. On a group: opens at the group's path (see `group_dir`), one per group
    fn open_term(&mut self) {
        self.digits.clear();
        if let Some(g) = self.group_row().map(String::from) {
            let key = group_key(&g);
            if self.terms.contains_key(&key) {
                self.term_on = Some(key);
                return;
            }
            let Some((_, dir, _)) = self.group_dir(&g) else {
                return self.flash(t!("无法确定该分组的目录", "no idea which directory this group is in"));
            };
            let w = Where { dir, ..Where::default() };
            self.term_req =
                Some(TermReq { key, family: String::new(), title: group_label(&g).to_string(), w, env: vec![], run: None, link: None });
            return;
        }
        let Some(s) = self.current() else { return };
        if self.terms.contains_key(&s.name) {
            self.term_on = Some(s.name.clone());
            return;
        }
        self.term_req = Some(TermReq {
            key: s.name.clone(),
            family: s.family().to_string(),
            title: s.title.clone(),
            w: Where::of(s),
            env: vec![],
            run: None,
            link: None,
        });
    }

    /// A login link showed up in a login terminal (a hidden one counts too): pop up a choice of browser to open it with
    fn catch_links(&mut self) {
        let mut got: Vec<(String, crate::term::Link)> =
            self.terms.values_mut().filter_map(|t| t.new_link().map(|l| (t.title.clone(), l))).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        for (title, link) in got {
            self.open(Box::new(crate::browser::Pick::new(&title, link)));
        }
    }

    /// Terminal open: every key goes to it (^C included), only Esc hides it — the shell keeps running, t again comes back
    fn on_term_key(&mut self, key: &str, k: KeyEvent) {
        if k.code == KeyCode::Esc && k.modifiers.is_empty() {
            self.term_on = None;
        } else if let Some(t) = self.terms.get_mut(key) {
            t.key(k);
        }
    }

    /// T: open a new window running a shell in its session (same as prefix c), in the directory it's really working in
    fn window_here(&mut self) {
        self.digits.clear();
        if self.on_group(t!("打开窗口", "open a window")) {
            return;
        }
        let Some(s) = self.current() else { return };
        if s.state.gone() {
            return self.flash(t!("未运行,先按回车启动", "it isn't running; press Enter to start it first"));
        }
        self.split = Some(self.split_of(s, false));
    }

    /// Split next to s's pane: a window row splits that pane; a session row with no known pane id splits its current pane
    fn split_of(&self, s: &Session, yazi: bool) -> Split {
        let target = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        // A row for an extra window in a session: account comes from the session it's in
        let head = if s.sub.is_none() { Some(s) } else { self.all.iter().find(|x| x.sub.is_none() && x.name == s.family()) };
        let env = head.map(crate::data::shell_env).unwrap_or_default();
        Split { yazi, name: s.family().to_string(), target, w: Where::of(s), side: self.screen.width >= 100, env }
    }

    /// y: open yazi in the directory it's really working in, q quits back to aca.
    /// Works for stopped sessions too (uses the directory it saved)
    fn open_yazi(&mut self) {
        self.digits.clear();
        if self.on_group(t!("打开 yazi", "open yazi")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let mut sp = self.split_of(s, true);
        if s.state.gone() {
            sp.target.clear();
        }
        self.external = Some(External::Yazi(sp));
    }

    /// Interface in a popup: where this client is now (tmux session, pane) → highlight it
    /// (that pane's own row if it has one, else the session's row)
    pub fn highlight_here(&mut self, session: &str, pane: &str) {
        let row = self.all.iter().find(|s| !pane.is_empty() && s.pane == pane && s.tmux == session);
        let Some(name) = row.or_else(|| self.all.iter().find(|s| s.sub.is_none() && s.tmux == session)).map(|s| s.name.clone()) else {
            return;
        };
        self.selected = Some(name);
        let all = std::mem::take(&mut self.all);
        self.set_sessions(all);
    }

    /// Start following (see follow.rs): session / pane is where this client is now. `highlight_here` first; whichever row the highlight
    /// lands on is taken as "the original one"
    pub fn follow_from(&mut self, session: &str, pane: &str) {
        let row = self.current().filter(|_| !self.on_group_row()).map(|s| s.name.clone()).unwrap_or_default();
        self.following = Some(Follow::new(Place { row, session: session.into(), pane: pane.into() }));
    }

    /// Where the highlighted row should take the client: group rows and sessions not running = nowhere
    fn follow_want(&self) -> Option<Place> {
        let s = self.current().filter(|_| !self.on_group_row())?;
        // A subagent row has no pane: take it to its session's pane
        let pane = match &s.subagent {
            Some(_) => self.all.iter().find(|x| x.sub.is_none() && x.name == s.family()).map_or_else(String::new, |x| x.pane.clone()),
            None => s.pane.clone(),
        };
        (!s.tmux.is_empty() && !s.state.gone()).then(|| Place { row: s.name.clone(), session: s.tmux.clone(), pane })
    }

    /// One step of following (the run loop asks each round): once the highlight settles and it's time to move, say where to switch to. Not
    /// while sliding or typing a filter
    pub fn follow_step(&mut self) -> Option<Place> {
        let want = if self.sliding() || self.filtering { None } else { self.follow_want() };
        let f = self.following.as_mut()?;
        let to = f.step(want)?;
        // Passing by doesn't count as seen: switching there makes tmux record it as "attached", and core's "finished, not looked at" goes
        // away with it → mark it unread; only ⏎ stay or closing on it clears that
        let passing = !f.is_home(&to.session);
        if let Some(key) =
            self.all.iter().find(|s| passing && s.sub.is_none() && s.tmux == to.session && s.state == State::Unread).map(notes::key)
        {
            let _ = self.reads.mark_unread(&key, &self.keep_marks());
        }
        Some(to)
    }

    /// Closed while following (^\ / qq; ⏎ and Esc each go elsewhere): you stay on the session showing beside; it counts as seen
    pub fn follow_done(&mut self) {
        if let Some(session) = self.following.as_ref().and_then(|f| f.stayed()).map(String::from) {
            self.mark_seen(&session);
        }
    }

    /// Whether the pos-th item of the visible list is a fold row (placeholder of a folded section)
    fn is_fold_row(&self, pos: usize) -> bool {
        self.view.get(pos).is_some_and(|&i| {
            let s = &self.all[i];
            !s.pinned && self.folds.contains_key(&s.group)
        })
    }

    fn on_fold_row(&self) -> bool {
        self.list.selected().is_some_and(|p| self.is_fold_row(p))
    }

    /// Account column width: set by the longest among all sessions (not just the ones shown),
    /// so headers don't resize when switching scope / group
    pub fn who_width(&self) -> usize {
        self.all.iter().map(|s| s.who.width()).max().unwrap_or(0).min(WHO_MAX)
    }

    pub fn fold_info(&self) -> &Folds {
        &self.folds
    }

    /// o: fold / unfold the current section (on a fold row: unfold). After folding the highlight lands on the fold row
    fn toggle_fold(&mut self) {
        let Some(s) = self.current() else { return };
        if !self.sectioned() {
            return self.flash(t!("单个分组中不能折叠", "can't fold inside a single group"));
        }
        if s.pinned {
            return self.flash(t!("置顶项不能折叠", "pinned ones can't fold"));
        }
        let g = s.group.clone();
        match self.folded.contains(&g) {
            false => {
                // The placeholder is this section's first session; the highlight goes with it
                let first = self.view.iter().map(|&i| &self.all[i]).find(|x| !x.pinned && x.group == g).map(|x| x.name.clone());
                self.folded.insert(g.clone());
                self.selected = first;
            }
            true => {
                self.folded.remove(&g);
            }
        }
        self.rebuild();
        // Unfolded: the highlight stays on this group (on its header)
        if let Some(p) = self.head_pos(&g).filter(|_| !self.folded.contains(&g)) {
            self.select_head(p);
        }
    }

    /// h / ←: one level out. Window / subagent / child session → the session it hangs off; on a session with subagents unfolded = fold
    /// them; on a session or an unfolded header = fold the group (only h folds, only l unfolds); viewing a single group: back to all groups
    /// (highlight stays put). Fold rows and pinned-block items have no outer level; do nothing
    fn step_out(&mut self) {
        if self.head.is_some() {
            return self.toggle_fold();
        }
        let Some(s) = self.current() else { return };
        // A session with subagents unfolded: fold those first; only the next press gets to the group
        if s.sub.is_none() && self.agents_open.contains(&s.name) {
            let name = s.name.clone();
            self.agents_open.remove(&name);
            self.lay_agents();
            self.apply_pins();
            return self.rebuild();
        }
        let up = s.sub.as_ref().map(|x| x.parent.clone()).or_else(|| s.under().map(String::from));
        let (group, pinned) = (s.group.clone(), s.pinned);
        if let Some(p) = up.and_then(|n| self.view.iter().position(|&i| self.all[i].name == n)) {
            return self.select(Some(p));
        }
        if !self.sectioned() {
            return self.set_group(None);
        }
        if self.head_pos(&group).is_some() && !pinned {
            self.toggle_fold();
        }
    }

    /// l / →: one level in. On a fold row = unfold (highlight on the header); on a group header = go to the group's first session; on a
    /// session: if it spawned subagents, unfold them into rows (highlight stays), if already unfolded go to the first; none, do nothing
    fn step_in(&mut self) {
        if self.on_fold_row() {
            self.toggle_fold();
        } else if self.head.is_some() {
            let p = self.list.selected();
            self.select(p);
        } else if let Some(name) = self.current().filter(|s| s.sub.is_none() && !s.subagents.is_empty()).map(|s| s.name.clone()) {
            if self.agents_open.insert(name.clone()) {
                self.lay_agents();
                self.apply_pins();
                self.rebuild();
            } else if let Some(p) = self.view.iter().position(|&i| self.all[i].subagent.is_some() && self.all[i].family() == name) {
                self.select(Some(p));
            }
        }
    }

    /// O: unfold everything if anything is folded, else fold every section
    fn toggle_fold_all(&mut self) {
        if !self.sectioned() {
            return self.flash(t!("单个分组中不能折叠", "can't fold inside a single group"));
        }
        if self.folded.is_empty() {
            self.folded.extend(self.view.iter().map(|&i| &self.all[i]).filter(|s| !s.pinned).map(|s| s.group.clone()));
        } else {
            self.folded.clear();
        }
        self.rebuild();
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
            Clip::Info { title, text: crate::data::copy_text(s) }
        });
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

    pub fn on_key(&mut self, k: KeyEvent) {
        // ^`: back to the previous session, like ^` in tmux. With a popup open tmux doesn't apply key bindings;
        // it turns it into NUL for aca, which arrives as ^Space (a real ^Space is taken by macOS to switch input method,
        // never gets here); in the keys popup it's only looked up
        if crate::data::in_popup()
            && k.code == KeyCode::Char(' ')
            && k.modifiers.contains(KeyModifiers::CONTROL)
            && !self.reader.as_ref().is_some_and(|r| r.help)
        {
            return self.back_session();
        }
        // A popup covers the terminal (popped when a login link appears, see browser.rs):
        // keys go to the popup first, back to the terminal once it's closed
        if self.term_on.is_some() && !self.overlays.is_empty() {
            return self.on_overlay_key(k);
        }
        if let Some(key) = self.term_on.clone() {
            return self.on_term_key(&key, k);
        }
        // ^\: the key that opened the popup, press it again to close (with a popup open tmux doesn't apply key bindings, so the key arrives
        // here). You stay on the session now showing beside; in the keys popup it's only looked up
        if (self.following.is_some() || crate::data::in_popup()) && closes_popup(k) && !self.reader.as_ref().is_some_and(|r| r.help) {
            self.quit = true;
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.new_draft.is_some() {
            return self.on_draft_key(k);
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(k);
        }
        if self.tool_menu.is_some() {
            return self.on_tool_key(k);
        }
        if !self.overlays.is_empty() {
            return self.on_overlay_key(k);
        }
        if self.space_menu.is_some() {
            return self.on_space_menu_key(k);
        }
        if self.group_menu.is_some() {
            return self.on_group_menu_key(k);
        }
        if self.only_menu.is_some() {
            return self.on_only_menu_key(k);
        }
        if self.theme_menu.is_some() {
            return self.on_theme_key(k);
        }
        if self.settings.is_some() {
            return self.on_settings_key(k);
        }
        // Confirm: only y counts, any other key (Enter included) cancels — delete can't be undone, restart interrupts it
        if let Some(c) = self.confirm.take() {
            let yes = k.code == KeyCode::Char('y');
            // Any key but y means no: Esc is an explicit no, nothing to say; other keys get a line, in case it was pressed by accident
            let no = |what: &'static str| if k.code == KeyCode::Esc { "" } else { what };
            let mut said = "";
            match c {
                Confirm::Remove(r) if yes => {
                    let t = Target { name: r.name, title: r.title, profile: r.profile, id: r.id };
                    // Remove from the list first, without waiting for the background delete (it waits for the process to exit cleanly)
                    let running = !t.id.is_empty()
                        && self.all.iter().any(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile && !s.state.gone());
                    let job = Job::Remove { t: t.clone(), pane: r.pane.map(|(p, _)| p) };
                    let running = if running { vec![t.id] } else { vec![] };
                    self.vanish.push(Vanish { job: job.clone(), until: None, running });
                    self.job = Some(job);
                    self.drop_vanished();
                    self.apply_pins();
                    self.rebuild();
                }
                Confirm::Remove(r) if r.pane.is_some() => said = no(t!("未关闭", "not closed")),
                Confirm::Remove(_) => said = no(t!("未删除", "not deleted")),
                Confirm::Group(g) if yes => {
                    // Remove the group's sessions from the list first, without waiting for the background stop / delete
                    let job = Job::Group(groups::Act::Delete { profile: g.profile, group: g.group });
                    self.vanish.push(Vanish { job: job.clone(), until: None, running: g.running });
                    self.job = Some(job);
                    self.head = None;
                    self.drop_vanished();
                    self.apply_pins();
                    self.rebuild();
                }
                Confirm::Group(_) => said = no(t!("未删除", "not deleted")),
                Confirm::Restart { t, force, .. } if yes => self.job = Some(Job::Restart { t, force }),
                Confirm::Restart { .. } => said = no(t!("未重启", "not restarted")),
                Confirm::Stop { t, .. } if yes => self.job = Some(Job::Stop { t }),
                Confirm::Stop { .. } => said = no(t!("未停止", "not stopped")),
                Confirm::Pane { t, p, restart, .. } if yes => {
                    self.job = Some(if restart { Job::Respawn { t, p } } else { Job::Park { t, p } })
                }
                Confirm::Pane { restart, .. } => {
                    said = no(if restart { t!("未重启", "not restarted") } else { t!("未停止", "not stopped") })
                }
                Confirm::Send { t, pane, key, text } if yes => self.job = Some(Job::Send { t, pane, text, key }),
                Confirm::Send { .. } => said = no(t!("未发送", "not sent")),
                Confirm::Adopt { t, plan } if yes => self.job = Some(Job::Adopt { t, plan }),
                Confirm::Adopt { .. } => said = no(t!("未接管", "not adopted")),
                Confirm::Many { act, ts, .. } if yes => self.run_many(act, ts),
                Confirm::Many { act, .. } => {
                    said = no(match act {
                        Batch::Remove => t!("未删除", "not deleted"),
                        Batch::Stop => t!("未停止", "not stopped"),
                        Batch::Restart => t!("未重启", "not restarted"),
                    })
                }
            }
            if !said.is_empty() {
                self.flash(said);
            }
            return;
        }
        if self.bill.is_some() {
            return self.on_bill_key(k);
        }
        if self.trace.is_some() {
            return self.on_trace_key(k);
        }
        if self.reader.is_some() {
            return self.on_reader_key(k);
        }
        if self.filtering {
            match k.code {
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => self.filter.push(c),
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                }
                KeyCode::Up => return self.move_by(-1),
                KeyCode::Down => return self.move_by(1),
                _ => return,
            }
            self.selected = None;
            self.head = None;
            self.list.select(Some(0));
            return self.rebuild();
        }
        if self.jump.is_some() && self.on_jump_key(k) {
            return;
        }
        if let Some(p) = self.pending.take() {
            return self.on_pending_key(p, k);
        }
        if self.picking() && self.on_pick_key(k) {
            return;
        }
        if self.head.is_none() && self.current().is_some_and(|s| s.subagent.is_some()) && self.on_agent_row_key(k) {
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char(' ') if !self.view.is_empty() => self.jump = Some(String::new()),
            KeyCode::Char('z') if ctrl => self.undo_last(),
            KeyCode::Char('z') => self.open_reader(),
            KeyCode::Char('x') => self.open_diff(),
            KeyCode::Char('Z') => self.open_trace(),
            KeyCode::Char('t') => self.open_term(),
            KeyCode::Char('s') if ctrl => self.open_settings(),
            KeyCode::Char('s') => self.ask_send(),
            KeyCode::Char('T') => self.window_here(),
            KeyCode::Char('y') => self.open_yazi(),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.rebuild();
            }
            KeyCode::Esc if let Some(p) = self.status => self.toggle_status(p),
            // In a tmux popup or while following, Esc closes (following first goes back to where it was when opened; `follow_step` in the
            // run loop switches). In a standalone terminal Esc at the bottom doesn't quit: every popup and page closes with Esc, one press
            // too many shouldn't close the whole interface; quitting is q q
            KeyCode::Esc if self.following.is_some() || crate::data::in_popup() => {
                if let Some(f) = self.following.as_mut() {
                    f.back();
                }
                self.quit = true
            }
            KeyCode::Esc => self.flash(t!("q q 退出", "q q quits")),
            KeyCode::Up if k.modifiers.contains(KeyModifiers::SHIFT) => self.reorder(-1),
            KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => self.reorder(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('J') => self.jump_section(1),
            KeyCode::Char('K') => self.jump_section(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-5),
            KeyCode::PageDown => self.move_by(5),
            KeyCode::Char('g') if ctrl => self.note_in_editor(),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.move_by(10_000),
            KeyCode::Enter if k.modifiers.contains(KeyModifiers::SHIFT) => self.enter_with_note(),
            KeyCode::Enter => self.enter(),
            KeyCode::Char('i') => self.jump_waiting(),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('a') => self.toggle_all(),
            KeyCode::Char('w') => self.toggle_open(),
            KeyCode::Char('!') => self.toggle_status(Pick::Run),
            KeyCode::Char('@') => self.toggle_status(Pick::Wait),
            KeyCode::Char('#') => self.toggle_status(Pick::Idle),
            KeyCode::Char('%') => self.toggle_status(Pick::Reaped),
            KeyCode::Char('&') => self.toggle_status(Pick::Err),
            KeyCode::Char('^') => self.toggle_status(Pick::Off),
            KeyCode::Tab => self.open_group_menu(),
            KeyCode::BackTab => self.back_group(),
            KeyCode::Char('}') => self.cycle_group(1),
            KeyCode::Char('{') => self.cycle_group(-1),
            KeyCode::Char('l') if ctrl => self.ask_relaunch(),
            KeyCode::Left | KeyCode::Char('h') => self.step_out(),
            KeyCode::Right | KeyCode::Char('l') => self.step_in(),
            KeyCode::Char('o') => self.toggle_fold(),
            KeyCode::Char('O') => self.toggle_fold_all(),
            KeyCode::Char('N') => self.ask_new(),
            KeyCode::Char('n') if ctrl => self.ask_new_group(),
            KeyCode::Char('n') => self.open_tools(),
            KeyCode::Char('d') if self.on_group_row() => self.ask_remove_group(),
            KeyCode::Char('d') => self.ask_remove(),
            KeyCode::Char('b') => self.open_trash(),
            KeyCode::Char('p') if self.on_group_row() => {
                self.on_group(t!("置顶", "pin"));
            }
            KeyCode::Char('p') => self.toggle_pin(),
            KeyCode::Char('u') => self.toggle_read(),
            KeyCode::Char('q') => self.pending = Some(Pending::Switch),
            KeyCode::Char('Q') if self.current().is_some() => self.pending = Some(Pending::Send),
            KeyCode::Char('[') => self.cycle_space(-1),
            KeyCode::Char(']') => self.cycle_space(1),
            KeyCode::Char('`') => self.back_space(),
            KeyCode::Char('S') => self.open_space_menu(),
            KeyCode::Char('c') => self.ask_copy(true),
            KeyCode::Char('B') => self.ask_copy(false),
            KeyCode::Char('C') => self.open_copy_menu(),
            KeyCode::Char(c @ ('v' | 'V')) => self.start_pick(c == 'V'),
            KeyCode::Char('L') => self.cycle_preview(),
            KeyCode::Char('r') if ctrl => self.refresh = true,
            KeyCode::Char('r') => self.ask_rename(),
            KeyCode::Char('R') => self.ask_restart(),
            KeyCode::Char('D') => self.ask_stop(),
            KeyCode::Char('m') => self.ask_move(),
            // F used to be "name it, then fork"; f does that now
            KeyCode::Char('f') => self.ask_fork(),
            KeyCode::Char('F') => self.open_only_menu(),
            KeyCode::Char('H') => self.ask_handoff(),
            KeyCode::Char('X') => self.ask_rehome(),
            KeyCode::Char('A') => self.ask_adopt(),
            KeyCode::Char('M') => self.open_mcp(),
            KeyCode::Char('P') => self.open_plugins(),
            KeyCode::Char(':') => self.open_caps(),
            KeyCode::Char('W') => self.toggle_own_group(),
            KeyCode::Char('*') => self.cycle_days(),
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Char('$') => self.open_bill(),
            KeyCode::Char('e') => self.ask_note(),
            KeyCode::Char('E') => self.enter_with_note(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if self.digits_at.elapsed() > DIGIT_GAP {
                    self.digits.clear();
                }
                self.digits_at = Instant::now();
                self.digits.push(c);
                let n: usize = self.digits.parse().unwrap_or(0);
                if self.numbered(n).is_none() {
                    // No number 12 → treat it as pressing 2 afresh
                    self.digits = c.to_string();
                }
                let n: usize = self.digits.parse().unwrap_or(0);
                if let Some(p) = self.numbered(n) {
                    self.select(Some(p));
                }
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.term_on.is_some() || self.theme_menu.is_some() || self.settings.is_some() {
            return;
        }
        if let Some(b) = self.bill.as_mut() {
            match m.kind {
                MouseEventKind::ScrollUp => b.scroll = b.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => b.scroll = b.scroll.saturating_add(3),
                _ => {}
            }
            return;
        }
        if let Some(r) = self.reader.as_mut() {
            match m.kind {
                MouseEventKind::ScrollUp => r.scroll = r.scroll.saturating_add(3),
                MouseEventKind::ScrollDown => r.scroll = r.scroll.saturating_sub(3),
                _ => {}
            }
            return;
        }
        if let Some(v) = self.trace.as_mut() {
            let d = match m.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => return,
            };
            match v.detail.as_mut() {
                Some(x) => x.scroll = x.scroll.saturating_add_signed(d),
                // Two panes: scroll whichever side the mouse is on
                None if v.wide && m.column >= v.split => v.steps.scroll(d),
                None if v.wide => v.chat.scroll(d),
                None => v.cur().scroll(d),
            }
            return;
        }
        if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            if self.modal_open() {
                return;
            }
            let pos = ratatui::layout::Position::new(m.column, m.row);
            if self.list_area.contains(pos) {
                self.move_by(if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 });
            } else if self.preview_area.contains(pos) && !self.on_group_row() && self.current().is_some_and(|s| s.subagent.is_none()) {
                // The preview scrolls back through what doesn't fit on this screen;
                // past the top, earlier stuff is in the full-screen reader
                match m.kind {
                    MouseEventKind::ScrollUp if self.preview_up >= self.preview_max => self.flash(t!("z 全屏阅读", "z read output")),
                    MouseEventKind::ScrollUp => self.preview_up = (self.preview_up + 3).min(self.preview_max),
                    _ => self.preview_up = self.preview_up.saturating_sub(3),
                }
            }
            return;
        }
        match m.kind {
            // Group list open: a click anywhere closes it
            MouseEventKind::Down(MouseButton::Left) if self.group_menu.is_some() => self.group_menu = None,
            MouseEventKind::Down(MouseButton::Left)
                if self.chip_area.contains(ratatui::layout::Position::new(m.column, m.row)) && !self.modal_open() =>
            {
                self.open_group_menu()
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let a = self.list_area;
                if m.column < a.x || m.column >= a.x + a.width || m.row < a.y || m.row >= a.y + a.height {
                    return;
                }
                // List items are one or two lines (two with a question), with section headers in between; add up heights from the current
                // scroll position to find the clicked one
                let mut y = a.y;
                for &(pos, head, h) in self.drawn.iter().skip(self.disp.offset()) {
                    if m.row < y + h {
                        let on = self.list.selected();
                        // Clicking the already highlighted row = Enter (go in / fold); group headers are clickable too, blank rows and the
                        // pinned block's header aren't
                        match (pos, head) {
                            (Some(p), _) if on == Some(p) && self.head.is_none() => self.enter(),
                            (Some(p), _) => self.select(Some(p)),
                            (None, Some(p)) if on == Some(p) && self.head.is_some() => self.enter(),
                            (None, Some(p)) => self.select_head(p),
                            (None, None) => {}
                        }
                        return;
                    }
                    y += h;
                }
            }
            _ => {}
        }
    }
}

/// Desktop (wide and tall enough): margins all round (3 columns left and right, 1 line top and bottom, a blank line below the top bar and
/// above the bottom bar); the list shows numbers and a blank line between groups
fn roomy(area: Rect) -> bool {
    area.width >= 100 && area.height >= 30
}

/// The tag at the left of the top bar: the workspace's name ("sessions" when only 1 is in use); viewing a single group, that group instead
fn chip_text(app: &App) -> String {
    let label = app.group.as_ref().map_or_else(|| app.space_label(), |g| group_label(g).to_string());
    format!(" {} ", clip(&label, 14).trim_end())
}

/// Top bar: left is what you're looking at (accent-colored tag + days / all / open, or the search term), right is the count per state. When
/// narrow, shorten the counts first (◆1), then keep only the current scope
fn top_bar(app: &App, w: usize) -> Line<'static> {
    let on = theme::accent().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let chip = vec![Span::styled(chip_text(app), theme::chip()), Span::raw("  ")];
    let tabs = |all: bool| -> Vec<Span<'static>> {
        if !app.filter.is_empty() || app.filtering {
            let cursor = if app.filtering { "▏" } else { "" };
            return vec![
                Span::styled("/ ", theme::accent()),
                Span::styled(format!("{}{cursor}", app.filter), Style::new().add_modifier(Modifier::BOLD)),
            ];
        }
        let mut v = vec![];
        let mut tab = |t: String, active: bool| {
            if all || active {
                if !v.is_empty() {
                    v.push(Span::raw("  "));
                }
                v.push(Span::styled(t, if active { on } else { dim() }));
            }
        };
        for d in DAYS {
            tab(tr!("{d} 天", "{d}d"), app.scope == Scope::Recent && app.days == d);
        }
        tab(t!("全部", "All").into(), app.scope == Scope::All);
        // "Open" is a toggle stacked on the scope, not one of its steps:
        // set apart, a tag when on, not mixed with the scope's underline
        if all || app.open {
            if !v.is_empty() {
                v.push(Span::styled("  │  ", theme::faint()));
            }
            v.push(if app.open {
                Span::styled(t!(" 未停止 ", " Open "), theme::chip())
            } else {
                Span::styled(t!("未停止", "Open"), dim())
            });
        }
        if app.fell_back {
            v.push(Span::styled(t!("  近期无活动，显示全部", "  nothing active, showing all"), dim()));
        }
        v
    };
    let mut extra = vec![];
    if !app.digits.is_empty() && app.digits_at.elapsed() <= DIGIT_GAP {
        extra.push(Span::styled(format!("  #{}", app.digits), theme::accent().add_modifier(Modifier::BOLD)));
    }
    // Status picked with ! @ # & ^ %: those cells get the selection background (search ignores status, so no background then). Idle, ■, ☾
    // aren't counted normally; listed when picked, even at 0
    let pick = app.status.filter(|_| app.filter.is_empty());
    // Like i, count only what's visible in the list (folded groups' placeholder rows don't count); the ■ cell counts everything ^ picks
    // (exited, errored and not running, ☾ too), the ☾ cell only auto-stopped ones
    let count = |hit: &dyn Fn(&Session) -> bool| {
        app.view
            .iter()
            .enumerate()
            .filter(|&(p, &i)| !app.is_fold_row(p) && app.all[i].subagent.is_none() && !app.all[i].blank && hit(&app.all[i]))
            .count()
    };
    let counts = |long: bool| -> Vec<Span<'static>> {
        let mut v = vec![];
        for (st, name) in [
            (Some(State::Ask), t!("等待回应", "needs you")),
            (Some(State::Running), t!("运行中", "running")),
            (Some(State::Unread), t!("未读", "unread")),
            (Some(State::Error), t!("出错", "error")),
            (Some(State::Idle), t!("空闲", "idle")),
            (Some(State::Stopped), t!("已停止", "stopped")),
            (None, t!("自动停止", "auto-stopped")),
        ] {
            let (n, on, (ic, sty)) = match st {
                Some(State::Stopped) => (count(&|s| Pick::Off.covers(s.state)), pick == Some(Pick::Off), icon(State::Stopped)),
                Some(st) => (count(&|s| s.state == st), pick.is_some_and(|p| p.covers(st)), icon(st)),
                None => (count(&reaped), pick == Some(Pick::Reaped), (aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted))),
            };
            if !on && (n == 0 || !matches!(st, Some(State::Ask | State::Running | State::Unread | State::Error))) {
                continue;
            }
            if !v.is_empty() {
                v.push(Span::raw(if long { "    " } else { "  " }));
            }
            let mut cell = vec![
                Span::styled(ic, sty),
                Span::styled(if long { format!(" {n}") } else { n.to_string() }, Style::new().add_modifier(Modifier::BOLD)),
            ];
            if long {
                cell.push(Span::styled(format!(" {name}"), if on { Style::new() } else { dim() }));
            }
            if on {
                let bg = Style::new().bg(theme::pal().sel);
                cell.insert(0, Span::styled(" ", bg));
                cell.push(Span::styled(" ", bg));
                cell.iter_mut().for_each(|s| s.style = s.style.patch(bg));
            }
            v.extend(cell);
        }
        // Broken plugins: how many plugins, not how many places
        let mut broken: Vec<&str> = app.plugin_alerts.iter().map(|p| p.plugin.as_str()).collect();
        broken.dedup();
        if !broken.is_empty() {
            if !v.is_empty() {
                v.push(Span::raw(if long { "    " } else { "  " }));
            }
            v.push(Span::styled("⚠", theme::warn()));
            v.push(Span::styled(
                if long { format!(" {}", broken.len()) } else { broken.len().to_string() },
                Style::new().add_modifier(Modifier::BOLD),
            ));
            if long {
                v.push(Span::styled(t!(" 插件出错(P)", " plugin errors (P)"), dim()));
            }
        }
        if v.is_empty() {
            v.push(Span::styled(t!("暂无待处理会话", "nothing to look at"), dim()));
        }
        v
    };
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    let mut best = None;
    for (all, long) in [(true, true), (true, false), (false, false)] {
        let left: Vec<Span> = [chip.clone(), tabs(all), extra.clone()].concat();
        let right = counts(long);
        if wid(&left) + 2 + wid(&right) <= w || (!all && !long) {
            best = Some((left, right));
            break;
        }
    }
    let (mut left, right) = best.unwrap_or_default();
    let pad = w.saturating_sub(wid(&left) + wid(&right)).max(1);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    Line::from(left)
}

/// Divider when the preview is below: a faint line with the title set in it — "─ ● title ───── Claude·team ─"
fn title_rule(ic: (&'static str, Style), title: &str, label: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let line = theme::faint();
    let label_w: usize = label.iter().map(|s| s.content.width()).sum();
    let title = clip(title, w.saturating_sub(4 + 2 + label_w + 5).max(4)).trim_end().to_string();
    let used = 2 + 2 + title.width() + 1 + if label_w > 0 { 1 + label_w } else { 0 } + 2;
    let mut v = vec![
        Span::styled("─ ", line),
        Span::styled(ic.0, ic.1),
        Span::raw(" "),
        Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ];
    v.push(Span::styled("─".repeat(w.saturating_sub(used)), line));
    if label_w > 0 {
        v.push(Span::raw(" "));
        v.extend(label);
    }
    v.push(Span::styled(" ─", line));
    Line::from(v)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    draw_screen(f, app);
    mark_torn_tails(&app.last_frame, f.buffer_mut());
    app.last_frame = f.buffer_mut().clone();
}

/// When a line of Chinese shifts left one cell as a whole (say closing the top bar's 「 未停止 」 (" Open ") tag), new chars cover the old
/// chars' left halves; ratatui leaves the right halves for the terminal to clear and doesn't write them, but tmux keeps the old char's
/// background in that cell, leaving a small colored block. When the old char has a background (or an underline, visible on spaces too),
/// mark that cell as must-write
fn mark_torn_tails(prev: &Buffer, next: &mut Buffer) {
    if prev.area != next.area {
        return;
    }
    let seen_on_blank = Modifier::REVERSED | Modifier::UNDERLINED | Modifier::CROSSED_OUT;
    let w = next.area.width as usize;
    for row in (0..next.content.len()).step_by(w.max(1)) {
        let mut x = 0;
        while x < w {
            let end = x + next.content[row + x].cell_width().max(1) as usize;
            if end - x > 1 && end < w {
                let old = &prev.content[row + end - 1];
                let ow = old.cell_width() as usize;
                if ow > 1 && (old.bg != Color::Reset || old.modifier.intersects(seen_on_blank)) {
                    for c in &mut next.content[row + end..row + (end + ow - 1).min(w)] {
                        c.set_diff_option(CellDiffOption::AlwaysUpdate);
                    }
                }
            }
            x = end;
        }
    }
}

fn draw_screen(f: &mut Frame, app: &mut App) {
    let full = f.area();
    app.screen = full;
    if app.bill.is_some() {
        return draw_bill(f, app);
    }
    if app.trace.is_some() {
        return draw_trace(f, app);
    }
    if app.reader.as_ref().is_some_and(|r| !r.help) {
        return draw_reader(f, app);
    }
    let roomy = roomy(full);
    let area = if roomy { Rect { x: full.x + 3, y: full.y + 1, width: full.width - 6, height: full.height - 2 } } else { full };
    let side = preview_side(app.mode(), full.width, full.height).filter(|_| !app.view.is_empty());
    // List only: keep one line at the bottom saying what the highlighted one is doing
    let summary = side.is_none() && app.mode() == Preview::Off && !app.view.is_empty();
    // Note being written in the preview (with cursor): the bottom bar doesn't repeat its content, only the keys
    let note_in_preview = side.is_some() && matches!(&app.prompt, Some(Prompt { ask: Ask::Note { .. }, .. }));
    let foot_h = 1;
    let gap = u16::from(roomy);
    let [head, _, body, _, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Fill(1),
        Constraint::Length(gap),
        Constraint::Length(foot_h),
    ])
    .areas(area);
    // t's terminal: takes a bit over the lower half; the list / preview stays above as usual
    let term = app.term_on.clone().filter(|k| app.terms.contains_key(k));
    let (body, term_area) = match term {
        Some(_) => {
            let h = (body.height * 11 / 20).max(5).min(body.height.saturating_sub(3));
            let [b, t] = Layout::vertical([Constraint::Fill(1), Constraint::Length(h)]).areas(body);
            (b, Some(t))
        }
        None => (body, None),
    };
    // Split ratio is fixed, not resized by the number of sessions — so the preview's edge doesn't jump up and down as sessions come and go
    let (list_area, prev_area) = match side {
        Some(Side::Below) => {
            let [l, p] = Layout::vertical([Constraint::Percentage(50), Constraint::Fill(1)]).areas(body);
            (l, Some(p))
        }
        Some(Side::Right) => {
            let list_w = (body.width * 2 / 5).clamp(44, 72);
            let [l, _, p] = Layout::horizontal([Constraint::Length(list_w), Constraint::Length(3), Constraint::Fill(1)]).areas(body);
            (l, Some(p))
        }
        Some(Side::Focus) => {
            let [l, p] = Layout::vertical([Constraint::Length(FOCUS_ROWS), Constraint::Fill(1)]).areas(body);
            (l, Some(p))
        }
        None if summary => {
            let [l, s] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(body);
            (l, Some(s))
        }
        None => (body, None),
    };
    app.list_area = list_area;
    app.preview_area = prev_area.filter(|_| !summary).unwrap_or_default();
    let mut rows = {
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        list_rows(&view, app.sectioned(), list_area.width as usize, app.jump.as_deref(), &app.folds, app.who_width(), roomy)
    };
    // Rows selected with v (like yazi): a green mark at the start, the whole row on a light green background
    let mut ticked = vec![false; rows.len()];
    if app.picking() {
        let chosen = app.chosen();
        for (r, on) in rows.iter_mut().zip(&mut ticked) {
            if let (Some(p), None) = (r.pos, r.head)
                && app.pickable(p)
                && chosen.contains(&app.all[app.view[p]].deck_id)
            {
                r.lines.iter_mut().for_each(tick_row);
                *on = true;
            }
        }
    }
    if app.hidden > 0 && !app.view.is_empty() {
        if roomy {
            rows.push(Row { pos: None, head: None, lines: vec![vec![]] });
        }
        rows.push(Row {
            pos: None,
            head: None,
            lines: vec![vec![
                (tr!(" 还有 {} 个更早的 · ", " {} more, older · ", app.hidden), dim()),
                ("*".into(), key_style()),
                (t!(" 放宽", " widen").into(), dim()),
            ]],
        });
    }
    app.drawn = rows.iter().map(|r| (r.pos, r.head, r.lines.len() as u16)).collect();

    f.render_widget(Paragraph::new(top_bar(app, head.width as usize)), head);
    app.chip_area = Rect { width: (chip_text(app).width() as u16).min(head.width), height: 1, ..head };

    // List
    if app.view.is_empty() {
        let msg = if !app.filter.is_empty() {
            tr!(" 没有匹配「{}」的会话", " nothing matches “{}”", app.filter)
        } else if let Some(p) = app.status {
            let (key, none) = p.label();
            match app.hidden {
                0 => tr!(" {none} · {key} 返回", " {none} · {key} back"),
                n => tr!(" {none} · 还有 {n} 个更早的 · * 放宽", " {none} · {n} more, older · * widens"),
            }
        } else if app.open {
            match app.hidden {
                0 => t!(" 暂无未停止的会话 · w 返回", " nothing open · w back").into(),
                n => tr!(" 暂无未停止的会话 · 还有 {n} 个更早的 · * 放宽", " nothing open · {n} more, older · * widens"),
            }
        } else {
            t!(" tmux 里没有会话", " no sessions in tmux").into()
        };
        f.render_widget(Paragraph::new(msg).style(dim()), list_area);
    } else {
        let sel = app.list.selected();
        let sel_row = match app.head {
            Some(_) => rows.iter().position(|r| r.head.is_some() && r.head == sel),
            None => rows.iter().position(|r| r.pos.is_some() && r.pos == sel),
        };
        app.disp.select(sel_row);
        let items: Vec<ListItem> = rows
            .into_iter()
            .zip(ticked)
            .map(|(r, on)| {
                let lines: Vec<Line> = r
                    .lines
                    .into_iter()
                    .map(|segs| Line::from(segs.into_iter().map(|(t, s)| Span::styled(t, s)).collect::<Vec<_>>()))
                    .collect();
                let item = ListItem::new(Text::from(lines));
                if on { item.style(Style::new().bg(theme::pal().pick)) } else { item }
            })
            .collect();
        // The selected item gets a light purple background, keeping each part's color (state and account still readable at a glance). Keep
        // one item of padding: scrolling to a group's first session also shows the section header above it
        let list = List::new(items).highlight_style(Style::new().bg(theme::pal().sel)).scroll_padding(1);
        f.render_stateful_widget(list, list_area, &mut app.disp);
    }

    // Preview: the bottom of the highlighted session's screen. On the right it's the only box on the screen (accent-colored, title set in
    // the top border); below, it's an accent-colored line with the title in it, which also separates list and preview
    let spot = if app.on_group_row() { app.spot() } else { None };
    let shown = if app.on_group_row() { None } else { preview_name(app).and_then(|n| app.all.iter().position(|s| s.name == n)) };
    if let Some(pa) = prev_area {
        let (ic, title, label) = match shown.map(|i| &app.all[i]) {
            Some(s) => {
                // Clip the account name when too long; the title comes first
                let label = clip(&s.agent_label(), (pa.width as usize / 3).max(12)).trim_end().to_string();
                let label = if label.is_empty() { vec![] } else { vec![Span::styled(label, agent_style(s.agent))] };
                match &s.subagent {
                    Some(a) => (agent_icon(a.phase), s.title.clone(), vec![Span::styled(t!("子代理", "subagent"), dim())]),
                    None => (session_icon(s), s.title.clone(), label),
                }
            }
            None => (("▸", dim()), app.current().map(|s| group_label(&s.group).to_string()).unwrap_or_default(), vec![]),
        };
        // When little of the list shows (mostly preview / list only), the line says which number the highlighted one is
        let at = Span::styled(format!("{}/{}", app.list.selected().map_or(0, |p| p + 1), app.view.len()), dim());
        let label = if side == Some(Side::Focus) { [label, vec![Span::raw("  "), at.clone()]].concat() } else { label };
        let inner = if summary {
            // List only: the preview shrinks to one line, showing state and directory
            let what = match (shown.map(|i| &app.all[i]), &spot) {
                (Some(s), _) => format!("{} · {}", state_word(s), create::tilde(&s.dir)),
                (None, Some(sp)) => format!("{title} · {}", create::tilde(&sp.dir)),
                (None, None) => title.clone(),
            };
            f.render_widget(Paragraph::new(title_rule(ic, &what, vec![at], pa.width as usize)), pa);
            Rect { height: 0, ..pa }
        } else if side == Some(Side::Right) {
            let t = clip(&title, (pa.width as usize).saturating_sub(12 + label.iter().map(|s| s.content.width()).sum::<usize>()).max(6));
            // A faint box: no big block of strong color standing next to the list; the title stays dark bold (without a foreground color it
            // would take the border's color)
            let bold = Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD);
            let mut block =
                Block::bordered().border_type(BorderType::Rounded).border_style(theme::faint()).padding(Padding::horizontal(1)).title_top(
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(ic.0, ic.1),
                        Span::raw(" "),
                        Span::styled(t.trim_end().to_string(), bold),
                        Span::raw(" "),
                    ]),
                );
            if !label.is_empty() {
                block = block.title_top(Line::from([vec![Span::raw(" ")], label, vec![Span::raw(" ")]].concat()).right_aligned());
            }
            let inner = block.inner(pa);
            f.render_widget(block, pa);
            inner
        } else {
            f.render_widget(Paragraph::new(title_rule(ic, &title, label, pa.width as usize)), Rect { height: 1, ..pa });
            Rect { x: pa.x + 1, y: pa.y + 1, width: pa.width.saturating_sub(2), height: pa.height.saturating_sub(1) }
        };
        match shown {
            _ if inner.height == 0 => {}
            None => {
                // Group: if its note is being written, show what's being written.
                // A group without a directory yet (empty group) has only the note
                let key = app.group_row().map(group_key);
                let editing = app.prompt.as_ref().and_then(|p| match &p.ask {
                    Ask::Note { key: k, .. } if Some(k) == key.as_ref() => Some(p.text.as_str()),
                    _ => None,
                });
                let note = editing.or_else(|| key.as_ref().and_then(|k| app.notes.get(k)));
                let (w, h) = (inner.width as usize, inner.height as usize);
                let lines = match &spot {
                    Some(sp) => group_pane(sp, app.glances.get(&group_key(&sp.group)), note, editing.is_some(), w, h),
                    None => note.map(|t| group_note(t, editing.is_some(), w, h, 0)).unwrap_or_default(),
                };
                f.render_widget(Paragraph::new(lines), inner);
            }
            Some(i) => {
                let s = &app.all[i];
                // This session's note is being written: show what's being written
                let editing = app.prompt.as_ref().and_then(|p| match &p.ask {
                    Ask::Note { key, .. } if *key == notes::key(s) => Some(p.text.as_str()),
                    _ => None,
                });
                let note = editing.or_else(|| app.notes.get(&notes::key(s)));
                let mut head =
                    preview_head(s, app.glances.get(&s.name), note, editing.is_some(), inner.width as usize, inner.height as usize);
                if let Some(l) = agents_line(s, app.agents_open.contains(&s.name), inner.width as usize) {
                    head.insert(1.min(head.len()), l);
                }
                let hh = (head.len() as u16).min(inner.height);
                f.render_widget(Paragraph::new(head), Rect { height: hh, ..inner });
                let body = Rect { y: inner.y + hh, height: inner.height - hh, ..inner };
                let lines = ansi::wrap(&preview_lines(s), body.width as usize);
                let h = body.height as usize;
                // Show the bottom of the screen (latest at the bottom), moved up by what the wheel scrolled back; a subagent that has
                // reported back shows from the top (its report comes first)
                let top_first = s.subagent.as_ref().is_some_and(|a| a.reported);
                let max = if top_first { 0 } else { lines.len().saturating_sub(h) };
                let up = if app.preview_for == s.name { app.preview_up.min(max) } else { 0 };
                let tail = match top_first {
                    true => &lines[..lines.len().min(h)],
                    false => &lines[lines.len().saturating_sub(h + up)..lines.len() - up],
                };
                f.render_widget(Paragraph::new(tail.iter().map(ansi::to_line).collect::<Vec<_>>()), body);
                ansi::paint_links(f.buffer_mut(), body, tail);
                // Scrolled up: the bottom row says how many lines are below (like the reader)
                if up > 0 {
                    below_tag(f, body, up);
                }
                let name = s.name.clone();
                (app.preview_for, app.preview_up, app.preview_max) = (name, up, max);
            }
        }
    }

    // Bottom bar: key hints, or a message shown briefly.
    // New, rename, confirm, tool pick and keys are all popups, on top of this whole layer
    let w = foot.width as usize;
    let layout_strip = app.layout_at.filter(|t| t.elapsed() < Duration::from_secs(3)).map(|_| {
        let mut v = vec![Span::styled(t!("布局  ", "layout  "), dim())];
        for (i, (p, name)) in layouts(narrow(full.width)).into_iter().enumerate() {
            if i > 0 {
                v.push(Span::styled(" → ", dim()));
            }
            v.push(if p == app.mode() { Span::styled(format!(" {name} "), theme::chip()) } else { Span::styled(name, dim()) });
        }
        Line::from(v).centered()
    });
    let doing = app.doing();
    let foot_line = match &app.msg {
        _ if note_in_preview => menu_bar(
            &[vec![("Esc", t!("保存", "done"), 0), ("⏎", t!("换行", "newline"), 0), ("^G", t!("编辑器", "editor"), 1)]],
            Some(0),
            w,
        ),
        // q / Q pressed: more urgent than the hint before
        _ if app.pending.is_some() => pending_bar(app, w),
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ if term_area.is_some() => menu_bar(&[vec![("Esc", t!("收起", "hide"), 0), ("exit", t!("关闭", "close"), 0)]], Some(0), w),
        _ if layout_strip.is_some() => layout_strip.unwrap_or_default(),
        // Multi-selecting: more urgent than background work (the keys pressed are all here)
        _ if app.picking() && !app.filtering => pick_bar(app, w),
        _ if doing.is_some() => Line::styled(doing.clone().unwrap_or_default(), theme::warn()).centered(),
        _ if app.filtering => {
            menu_bar(&[vec![("⏎", t!("确定", "ok"), 0), ("↑↓", t!("选择", "select"), 0), ("Esc", t!("清除", "clear"), 0)]], Some(0), w)
        }
        _ if app.jump.is_some() => {
            Line::styled(t!("按字母打开或定位到分组 · Esc 取消", "press a letter to open / land on a group · Esc cancels"), theme::warn())
                .centered()
        }
        _ => {
            // Three groups: manage sessions │ act on the highlighted one (main actions) │ view. Only the most used, the rest are in ?; when
            // it doesn't fit, drop the later ones first, ? and ⏎ always stay. On a subagent row: only view its record or go back to its
            // session; the session-management keys aren't listed
            let on_agent = app.head.is_none() && app.current().is_some_and(|s| s.subagent.is_some());
            let enter = match () {
                _ if app.head.is_some() => t!("折叠", "fold"),
                _ if app.on_fold_row() => t!("展开", "unfold"),
                _ if on_agent => t!("查看记录", "record"),
                // Following: it's already beside
                _ if app.following.is_some() => t!("留下", "stay"),
                _ => t!("打开", "open"),
            };
            let mut act: Vec<MenuItem> = vec![];
            if !app.filter.is_empty() {
                act.push(("Esc", t!("清除", "clear"), 0));
            } else if app.following.is_some() && app.status.is_none() {
                act.push(("Esc", t!("返回", "go back"), 0));
            }
            act.push(("⏎", enter, 0));
            if app.view.iter().any(|&i| waiting(&app.all[i])) {
                act.push(("i", t!("下一个", "next"), 1));
            }
            if on_agent {
                act.push(("h", t!("返回会话", "its session"), 6));
            } else {
                act.push(("e", t!("便签", "note"), 6));
            }
            // s only when there's a note
            if !app.on_group_row() && !on_agent && app.current().is_some_and(|s| s.noted) {
                act.push(("s", t!("发送", "send"), 6));
            }
            menu_bar(
                &[
                    if on_agent { vec![] } else { vec![("n", t!("新建", "new"), 2)] },
                    act,
                    vec![("/", t!("搜索", "search"), 3), ("v", t!("多选", "select"), 4), ("?", t!("快捷键", "keys"), 0)],
                ],
                Some(1),
                w,
            )
        }
    };
    f.render_widget(Paragraph::new(foot_line), foot);
    if let (Some(key), Some(ta)) = (app.term_on.clone(), term_area) {
        draw_term(f, app, &key, ta);
    }

    // Popups
    if app.reader.as_ref().is_some_and(|r| r.help) {
        draw_help(f, app);
    } else if let Some(d) = &app.new_draft {
        draw_draft(f, d);
    } else if let Some(p) = app.prompt.as_ref().filter(|_| !note_in_preview) {
        draw_prompt(f, p);
    } else if let Some((menu, sel)) = &app.tool_menu {
        draw_tools(f, menu, *sel, app.handoff.as_ref(), &app.tool_quotas);
    } else if let Some(o) = app.overlays.last() {
        // The bottom bar's line (just said, or else the background work) also goes at the bottom of the popup: on a phone the popup covers
        // the bottom bar, so results of things done with a popup open would be invisible
        let note = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.clone()).or_else(|| app.doing());
        if o.fill() {
            // Box size stays: the last line is kept for that line (two if long), blank when nothing to say; page lines don't jump
            modal_fill(f, o.width(), o.fill_rows(), theme::pal().accent, |w, h| {
                let mut said: Vec<Line> = note
                    .as_ref()
                    .map_or(vec![], |n| wrap_width(n, w).into_iter().take(2).map(|l| Line::styled(l, theme::warn())).collect());
                if said.is_empty() {
                    said.push(Line::raw(""));
                }
                let mut lines = o.lines_in(w, h.saturating_sub(said.len()));
                lines.extend(said);
                lines
            });
        } else {
            modal(f, o.width(), theme::pal().accent, |w| {
                let mut lines = o.lines(w);
                if let Some(n) = &note {
                    lines.push(Line::raw(""));
                    lines.extend(wrap_width(n, w).into_iter().map(|l| Line::styled(l, theme::warn())));
                }
                lines
            });
        }
    } else if let Some(sel) = app.space_menu {
        draw_spaces(f, app, sel);
    } else if let Some(sel) = app.group_menu {
        draw_groups(f, app, sel);
    } else if let Some(sel) = app.only_menu {
        draw_only(f, app, sel);
    } else if let Some(m) = &app.theme_menu {
        draw_themes(f, m);
    } else if let Some(st) = &app.settings {
        draw_settings(f, st);
    } else if let Some(c) = &app.confirm {
        draw_confirm(f, c, &app.all);
    }
}

/// A row selected with v (drawn for every row): the blank cell at the start becomes a green "▌" (like yazi's mark),
/// the number turns green too
fn tick_row(segs: &mut Vec<(String, Style)>) {
    let Some((text, style)) = segs.first_mut() else { return };
    let Some(rest) = text.strip_prefix(' ') else { return };
    let green = Style::new().fg(theme::pal().green);
    *text = rest.to_string();
    if !text.trim().is_empty() {
        *style = green.add_modifier(Modifier::BOLD);
    }
    segs.insert(0, ("▌".into(), green));
}

/// Bottom bar while multi-selecting (like yazi): first a green block saying how many are selected (during a range select, first whether the
/// range selects or unselects), then the keys acting on all of them together (main actions; p / u say what pressing does), last range /
/// select / set / none
fn pick_bar(app: &App, w: usize) -> Line<'static> {
    let rows = app.chosen_rows();
    let n = rows.len();
    let count = tr!("已选 {n}", "{n} selected");
    let badge = match app.anchor {
        Some(_) if app.unpick => button(&format!("{} · {count}", t!("取消选中", "unselect")), theme::pal().red),
        Some(_) => button(&format!("{} · {count}", t!("连选", "range")), theme::pal().green),
        None => button(&count, theme::pal().green),
    };
    let head = vec![badge, Span::raw(" ")];
    let pin = if rows.iter().any(|s| app.pins.rank(&s.name).is_none()) { t!("置顶", "pin") } else { t!("取消置顶", "unpin") };
    let read = if rows.iter().any(|s| s.state == State::Unread) || !rows.iter().any(|s| s.state == State::Idle) {
        t!("已读", "read")
    } else {
        t!("未读", "unread")
    };
    // Nothing selected (V pressed with nothing ticked): nothing to act on together
    let acts: Vec<MenuItem> = if n == 0 {
        vec![]
    } else {
        vec![
            ("d", t!("删除", "delete"), 0),
            ("m", t!("移到分组", "move"), 2),
            ("D", t!("停止", "stop"), 3),
            ("u", read, 4),
            ("R", t!("重启", "restart"), 5),
            ("p", pin, 6),
            ("Q", t!("移动到工作区", "to space"), 7),
        ]
    };
    // Whether the highlighted row is ticked: whether Space ticks or unticks.
    // When it doesn't fit drop Space first, then V (a phone keeps only v, Esc)
    let on = app.list.selected().is_some_and(|p| app.head.is_none() && app.picked.contains(&app.all[app.view[p]].deck_id));
    let tail: Vec<MenuItem> = match app.anchor {
        Some(_) => vec![("Esc", t!("确定", "keep"), 0)],
        None => vec![
            ("v", t!("连选", "range"), 0),
            ("V", t!("连续取消", "range unselect"), 1),
            (t!("空格", "Space"), if on { t!("取消选中", "unselect") } else { t!("选中", "select") }, 1),
            ("Esc", t!("清空选择", "clear"), 0),
        ],
    };
    let used: usize = head.iter().map(|s| s.content.width()).sum();
    let mut l = menu_bar_in(&[acts, tail], Some(0), w.saturating_sub(used), theme::pal().green);
    l.spans.splice(0..0, head);
    l
}

/// After q / Q, waiting for the next key: the bottom bar lists the workspaces you can go to (not h, nor the current one)
fn pending_bar(app: &App, w: usize) -> Line<'static> {
    let names: Vec<(String, String)> =
        app.space_list().into_iter().filter(|&c| c != app.space).map(|c| (c.to_string(), app.spaces.name(c))).collect();
    let mut items: Vec<MenuItem> = names.iter().enumerate().map(|(i, (k, n))| (k.as_str(), n.as_str(), (i + 1).min(200) as u8)).collect();
    let (what, tail): (&str, Vec<MenuItem>) = match app.pending {
        Some(Pending::Send) => (t!("移到", "Send to"), vec![("Esc", t!("取消", "cancel"), 0)]),
        _ => (t!("工作区", "Workspace"), vec![("q", t!("退出", "quit"), 0), ("Esc", t!("取消", "cancel"), 0)]),
    };
    if items.is_empty() {
        items = tail;
        let mut l = menu_bar(&[items], None, w.saturating_sub(what.width() + 2));
        l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
        return l;
    }
    let mut l = menu_bar(&[items, tail], Some(0), w.saturating_sub(what.width() + 2));
    l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
    l
}

/// S: workspace list. One per row: key, name, how many sessions, count per state; the current one's key in accent color, the highlighted
/// row gets a background
fn draw_spaces(f: &mut Frame, app: &App, sel: usize) {
    let list = app.space_list();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("工作区", "Workspace"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        // Unnamed ones show only the key, without repeating the key as the name
        let shown = |c: char| if app.spaces.renamed(c) { app.spaces.name(c) } else { String::new() };
        let name_w = list.iter().map(|&c| shown(c).width()).max().unwrap_or(0).min(16);
        for (i, &c) in list.iter().enumerate() {
            let heads: Vec<&Session> = app.all.iter().filter(|s| s.space == c && s.sub.is_none() && !s.blank).collect();
            let key_st = if c == app.space { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut line = menu_row(&c.to_string(), key_st, &shown(c), name_w, &heads);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("切换", "switch")), ("^R", t!("重命名", "rename"))], vec![], w));
        out
    });
}

/// A row of the workspace / group list: key, name, how many sessions, count per state
fn menu_row(key: &str, key_st: Style, name: &str, name_w: usize, heads: &[&Session]) -> Line<'static> {
    let mut spans = vec![
        Span::styled(key.to_string(), key_st),
        Span::raw("  "),
        Span::styled(clip(name, name_w), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {:>3}", heads.len()), dim()),
    ];
    for st in [State::Ask, State::Running, State::Unread, State::Error] {
        let n = heads.iter().filter(|s| s.state == st).count();
        if n > 0 {
            let (ic, sty) = icon(st);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(ic, sty));
            spans.push(Span::raw(n.to_string()));
        }
    }
    Line::from(spans)
}

/// Tab: this workspace's group list. One per row: letter (␣ for all groups), group name, how many sessions, count per state; the viewed
/// group's letter in accent color, the highlighted row gets a background
fn draw_groups(f: &mut Frame, app: &App, sel: usize) {
    let rows = app.group_rows();
    let name = |g: &Option<String>| g.as_deref().map_or(t!("全部", "all"), group_label).to_string();
    // Write group names in full (a long one widens the box), else groups with the same start get clipped to look alike
    let name_w = rows.iter().map(|(_, g)| name(g).width()).max().unwrap_or(0).min(40);
    modal(f, 48 + name_w.saturating_sub(20) as u16, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("分组", "Groups"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        for (i, (c, g)) in rows.iter().enumerate() {
            let heads: Vec<&Session> = app
                .all
                .iter()
                .filter(|s| s.space == app.space && s.sub.is_none() && !s.blank && g.as_ref().is_none_or(|g| &s.group == g))
                .collect();
            let key_st = if *g == app.group { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let key = if g.is_none() { "␣".to_string() } else { c.to_string() };
            let mut line = menu_row(&key, key_st, &name(g), name_w, &heads);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("切换", "switch")), ("␣", t!("全部", "all"))], vec![], w));
        out
    });
}

/// A state's icon (as in the list) and name (as in the top bar) in `F`'s small list
fn pick_look(p: Pick) -> (Vec<(&'static str, Style)>, &'static str) {
    match p {
        Pick::Run => (vec![icon(State::Running)], t!("运行中", "running")),
        Pick::Wait => (vec![icon(State::Ask), icon(State::Unread)], t!("等待回应 · 未读", "needs you · unread")),
        Pick::Idle => (vec![icon(State::Idle)], t!("空闲", "idle")),
        Pick::Err => (vec![icon(State::Error)], t!("出错", "error")),
        Pick::Off => (vec![icon(State::Exited)], t!("已停止", "stopped")),
        Pick::Reaped => (vec![(aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted))], t!("自动停止", "auto-stopped")),
    }
}

/// F: small list to view only one state. One state per row: first the key that picks it directly (the picked one in accent color), icon,
/// name, how many in this workspace; row 0 is all (Space). The highlighted row gets a background
fn draw_only(f: &mut Frame, app: &App, sel: usize) {
    modal(f, 44, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("按状态筛选", "Only"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let row = |key: &str, on: bool, icons: Vec<(&'static str, Style)>, name: &str, n: usize| -> Line<'static> {
            let key_st = if on { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut spans = vec![Span::styled(key.to_string(), key_st), Span::raw("  ")];
            let iw: usize = icons.iter().map(|(ic, _)| ic.width()).sum();
            spans.extend(icons.into_iter().map(|(ic, st)| Span::styled(ic, st)));
            spans.push(Span::raw(" ".repeat(4usize.saturating_sub(iw))));
            spans.push(Span::styled(name.to_string(), Style::new().add_modifier(Modifier::BOLD)));
            spread(spans, vec![Span::styled(n.to_string(), dim())], w)
        };
        let mut rows = vec![row("␣", app.status.is_none(), vec![], t!("全部", "all"), app.pick_count(None))];
        for p in PICKS {
            let (icons, name) = pick_look(p);
            rows.push(row(&p.label().0.to_string(), app.status == Some(p), icons, name, app.pick_count(Some(p))));
        }
        for (i, mut line) in rows.into_iter().enumerate() {
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("筛选", "only")), ("␣", t!("全部", "all"))], vec![], w));
        out
    });
}

/// t's terminal: the top line says where it's open, below is the shell's screen, the cursor on its cursor
fn draw_term(f: &mut Frame, app: &mut App, key: &str, area: Rect) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let Some(t) = app.terms.get_mut(key) else { return };
    t.resize(body.height, body.width);
    let w = head.width as usize;
    let dir = crate::create::tilde(&t.dir);
    let label = tr!(" 终端 · {} ", " terminal · {} ", clip(&t.title, 16).trim_end());
    let rest = w.saturating_sub(label.width() + 1);
    let head_line = Line::from(vec![Span::styled(label, theme::chip()), Span::raw(" "), Span::styled(clip_left(&dir, rest), dim())]);
    f.render_widget(Paragraph::new(head_line), head);
    f.render_widget(ratatui::widgets::Clear, body);
    // With a popup on top (the login link one), no cursor: keys go to the popup
    let covered = !app.overlays.is_empty();
    if let Some(pos) = app.terms.get(key).and_then(|t| t.render(body, f.buffer_mut())).filter(|_| !covered) {
        f.set_cursor_position(pos);
    }
}

/// Where a popup goes: wide and tall enough (desktop) floats centered with a shadow; narrow (phone) rises from the bottom at full width
fn modal_area(full: Rect, w: u16, h: u16) -> (Rect, bool) {
    if full.width >= 70 && full.height >= 20 {
        let (w, h) = (w.min(full.width.saturating_sub(4)), h.min(full.height.saturating_sub(2)));
        (Rect { x: full.x + (full.width - w) / 2, y: full.y + (full.height - h) / 2, width: w, height: h }, true)
    } else {
        let h = h.min(full.height);
        (Rect { x: full.x, y: full.y + full.height - h, width: full.width, height: h }, false)
    }
}

/// Whether a popup floats on this screen (desktop); on a phone it's a full-width strip at the bottom
fn floating(full: Rect) -> bool {
    modal_area(full, 1, 1).1
}

/// Before turning cell (x, y) into a narrow char: if it's the second half of a wide char, or a wide char itself, clear the other half to a
/// space so chars don't misalign
fn split_wide(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    let area = buf.area;
    if x > area.left() && buf[(x - 1, y)].symbol().width() > 1 {
        buf[(x - 1, y)].set_symbol(" ");
    }
    if buf[(x, y)].symbol().width() > 1 && x + 1 < area.right() {
        buf[(x + 1, y)].set_symbol(" ");
    }
}

/// A popup's backdrop and frame: the whole layer below goes grey (text faint grey, background and weight removed), on desktop a ░ shadow
/// offset one cell down-right, the panel cleared and given a rounded border (accent color normally, red for delete). Returns the inside of
/// the panel (minus border and padding)
fn modal_frame(f: &mut Frame, w: u16, h: u16, border: Color) -> Rect {
    let full = f.area();
    let (r, float) = modal_area(full, w, h);
    let pal = theme::pal();
    let buf = f.buffer_mut();
    for y in full.top()..full.bottom() {
        for x in full.left()..full.right() {
            buf[(x, y)].set_style(Style::reset().fg(pal.fade));
        }
    }
    if float {
        let mut shade = |x: u16, y: u16| {
            if x < full.right() && y < full.bottom() {
                split_wide(buf, x, y);
                buf[(x, y)].set_symbol("░").set_fg(pal.shadow);
            }
        };
        for y in r.y + 1..=r.bottom() {
            shade(r.right(), y);
        }
        for x in r.x + 1..r.right() {
            shade(x, r.bottom());
        }
    }
    panel(f, r, float, border)
}

/// The panel itself: split wide chars sitting on the edge, clear, draw a rounded border. Returns the inside (minus border and padding)
fn panel(f: &mut Frame, r: Rect, float: bool, border: Color) -> Rect {
    let full = f.area();
    let buf = f.buffer_mut();
    for y in r.top()..r.bottom() {
        split_wide(buf, r.left(), y);
        if r.right() < full.right() {
            split_wide(buf, r.right(), y);
        }
    }
    f.render_widget(ratatui::widgets::Clear, r);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(border)).padding(if float {
        Padding::new(2, 2, 1, 1)
    } else {
        Padding::new(1, 1, 0, 0)
    });
    let inner = block.inner(r);
    f.render_widget(block, r);
    inner
}

/// Color theme list. Unlike other popups it doesn't grey out what's below: the aca below is the preview. On desktop it floats in the
/// bottom-left corner (most of the list and preview stay visible), on a phone it rises from the bottom. One theme per row: ● on the saved
/// one, name, its own accent color, state colors (red green yellow), account colors
fn draw_themes(f: &mut Frame, m: &ThemeMenu) {
    let (outer, w) = side_width(f.area(), 50);
    let mut body = vec![modal_title(t!("配色", "Colors"), theme::accent(), esc_hint(t!("返回", "back")), w), Line::raw("")];
    let name_w = m.list.iter().map(|x| x.0.name.width()).max().unwrap_or(0).min(16);
    let mut last = None;
    for (i, (t, _)) in m.list.iter().enumerate() {
        if last != Some(t.dark) {
            if last.is_some() {
                body.push(Line::raw(""));
            }
            body.push(Line::styled(if t.dark { t!("深色", "dark") } else { t!("浅色", "light") }, dim()));
            last = Some(t.dark);
        }
        let p = &t.pal;
        let block = |c: Color, n: usize| Span::styled("█".repeat(n), Style::new().fg(c));
        let mut spans = vec![
            if t.name == m.now { Span::styled("● ", theme::accent()) } else { Span::raw("  ") },
            Span::styled(clip(&t.name, name_w), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            block(p.accent, 2),
            Span::raw(" "),
        ];
        spans.extend([p.red, p.green, p.amber].map(|c| block(c, 1)));
        spans.push(Span::raw(" "));
        spans.extend(p.series.iter().take(w.saturating_sub(name_w + 11)).map(|&c| block(c, 1)));
        let mut line = Line::from(spans);
        if i == m.sel {
            let pad = w.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(pad)));
            line = line.patch_style(Style::new().bg(theme::pal().sel));
        }
        body.push(line);
    }
    body.push(Line::raw(""));
    body.push(keys_line(&[("↑↓", t!("预览", "preview")), ("⏎", t!("应用", "use it"))], vec![], w));
    side_panel(f, outer, body);
}

/// How wide a side panel is: want on desktop (a bit less on a narrow screen), full width on a phone;
/// the second value is the usable inner width
fn side_width(full: Rect, want: u16) -> (u16, usize) {
    let float = floating(full);
    let outer = if float { want.min(full.width.saturating_sub(4)) } else { full.width };
    (outer, outer.saturating_sub(if float { 6 } else { 4 }) as usize)
}

/// A panel that doesn't grey out what's below (the color theme list: the aca below stays visible as the preview). On desktop it floats in
/// the bottom-left corner, on a phone it rises from the bottom
fn side_panel(f: &mut Frame, outer: u16, body: Vec<Line<'static>>) {
    let full = f.area();
    let float = floating(full);
    let h = body.len() as u16 + modal_chrome(full);
    let r = if float {
        let h = h.min(full.height.saturating_sub(2));
        // Two cells off the left, two above the bottom bar
        Rect { x: full.x + 2, y: full.bottom().saturating_sub(h + 2), width: outer, height: h }
    } else {
        modal_area(full, outer, h).0
    };
    let inner = panel(f, r, float, theme::pal().accent);
    f.render_widget(Paragraph::new(body), inner);
}

/// `^S` settings page: a centered popup (greys out what's below like Tab / S; when picking a theme, it becomes the color theme list in the
/// bottom-left that doesn't grey out). One item per row, "key  name  current value", › = go in to pick, ↗ = open in an editor; Problems get
/// their own page, accounts are a separate popup (acctpage.rs)
fn draw_settings(f: &mut Frame, s: &Settings) {
    modal(f, 54, theme::pal().accent, |w| settings_lines(s, w));
}

fn settings_lines(s: &Settings, w: usize) -> Vec<Line<'static>> {
    let pad = |mut line: Line<'static>, on: bool| {
        if on {
            let n = w.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(n)));
            line = line.patch_style(Style::new().bg(theme::pal().sel));
        }
        line
    };
    let mut body;
    match s.page {
        SetPage::Main => {
            body = vec![modal_title(t!("设置", "Settings"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
            let rows = s.rows();
            let name_w = rows.iter().map(|&r| s.name(r).width()).max().unwrap_or(0);
            let claude = s.accts.iter().filter(|a| a.agent == Agent::Claude).count();
            let codex = s.accts.len() - claude;
            for (i, &r) in rows.iter().enumerate() {
                let forced = |on: bool| if on { vec![Span::styled(t!(" · 环境变量", " · env var"), dim())] } else { vec![] };
                let (val, mark): (Vec<Span<'static>>, &str) = match r {
                    SetRow::Theme => (
                        [vec![Span::styled(s.theme.clone(), Style::new().add_modifier(Modifier::BOLD))], forced(theme::forced())].concat(),
                        "›",
                    ),
                    // Show both, the one in use lit up
                    SetRow::Lang => {
                        let opt = |l: Lang| {
                            let st = if l == s.lang { theme::accent().add_modifier(Modifier::BOLD) } else { theme::faint() };
                            Span::styled(l.code(), st)
                        };
                        ([vec![opt(Lang::Zh), Span::raw("  "), opt(Lang::En)], forced(i18n::forced())].concat(), "")
                    }
                    SetRow::Ime => (
                        [
                            vec![Span::styled(
                                if s.ime { t!("开", "on") } else { t!("关", "off") },
                                Style::new().add_modifier(Modifier::BOLD),
                            )],
                            forced(crate::ime::forced()),
                        ]
                        .concat(),
                        "",
                    ),
                    SetRow::Accounts => (
                        vec![
                            Span::styled("Claude ", agent_style(Agent::Claude)),
                            Span::raw(claude.to_string()),
                            Span::styled("  Codex ", agent_style(Agent::Codex)),
                            Span::raw(codex.to_string()),
                        ],
                        "›",
                    ),
                    SetRow::GroupDir => (vec![Span::raw(s.group_dir.clone())], ""),
                    SetRow::Tools | SetRow::Config => (vec![], "↗"),
                    SetRow::Extra(x) => s.extra.get(x).map_or((vec![], ""), |x| (x.value.clone(), x.mark)),
                    SetRow::Problems => (vec![Span::styled(tr!("{} 处错误", "problems: {}", s.bad.len()), theme::warn())], "›"),
                };
                let key = s.key(r).map_or_else(|| Span::styled("⚠", theme::warn()), |c| Span::styled(c.to_string(), key_style()));
                let mut left = vec![key, Span::raw("  ")];
                if r != SetRow::Problems {
                    left.push(Span::raw(clip(s.name(r), name_w)));
                    left.push(Span::raw("  "));
                }
                left.extend(val);
                let right = if mark.is_empty() { vec![] } else { vec![Span::styled(mark, theme::faint())] };
                body.push(pad(spread(left, right, w), i == s.sel));
            }
            body.push(Line::raw(""));
            body.push(keys_line(&[("⏎", t!("修改", "change"))], vec![], w));
        }
        SetPage::Problems => {
            body = vec![modal_title(t!("错误项", "Problems"), theme::accent(), esc_hint(t!("返回", "back")), w), Line::raw("")];
            for b in &s.bad {
                body.extend(wrap_width(b, w).into_iter().map(|l| Line::styled(l, theme::warn())));
            }
            body.push(Line::raw(""));
            body.push(keys_line(&[("e", t!("编辑 config.toml", "edit config.toml"))], vec![], w));
        }
    }
    body
}

/// How many rows the popup's frame plus padding take (by where the popup ends up)
fn modal_chrome(full: Rect) -> u16 {
    if floating(full) { 4 } else { 2 }
}

/// One line: a left part, a right part (right-aligned), spaces in between; when it doesn't fit, drop the right part first
pub fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    let (lw, rw) = (wid(&left), wid(&right));
    let mut v = left;
    if lw + 2 + rw <= w {
        v.push(Span::raw(" ".repeat(w - lw - rw)));
        v.extend(right);
    }
    Line::from(v)
}

/// A popup's title line: the title in accent bold, a part on the right (key to close / page number)
pub fn modal_title(title: &str, st: Style, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    spread(vec![Span::styled(clip(title, w).trim_end().to_string(), st.add_modifier(Modifier::BOLD))], right, w)
}

pub fn esc_hint(what: &str) -> Vec<Span<'static>> {
    vec![Span::styled("Esc", key_style()), Span::styled(format!(" {what}"), dim())]
}

/// A button with an accent background, like " ⏎ create "
pub fn button(t: &str, bg: Color) -> Span<'static> {
    Span::styled(format!(" {t} "), Style::new().fg(theme::pal().on_accent).bg(bg).add_modifier(Modifier::BOLD))
}

/// Input box: "> text▏", only the end when too long, the cursor always visible;
/// a part can hang on the right (like 「新目录」 ("new directory"))
fn input_row(text: &str, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let room = w.saturating_sub(2 + 1 + if right_w > 0 { right_w + 2 } else { 0 });
    let mut shown = text.to_string();
    if shown.width() > room {
        let mut tail: Vec<char> = vec![];
        let mut n = 1; // room for …
        for c in text.chars().rev() {
            n += c.width().unwrap_or(0);
            if n > room {
                break;
            }
            tail.push(c);
        }
        shown = format!("…{}", tail.into_iter().rev().collect::<String>());
    }
    let left = vec![Span::styled("> ", theme::accent().add_modifier(Modifier::BOLD)), Span::raw(shown), Span::styled("▏", theme::accent())];
    spread(left, right, w)
}

/// The line under the input box
fn input_rule(w: usize) -> Line<'static> {
    Line::styled("─".repeat(w), dim())
}

/// One line of key hints (at the bottom of a popup)
pub fn keys_line(items: &[(&str, &str)], right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let (keys, _) = key_spans(items, w.saturating_sub(if right_w > 0 { right_w + 2 } else { 0 }));
    spread(keys, right, w)
}

/// Draw a popup: work out its height (content lines + frame), draw the frame, then fill in the lines
fn modal(f: &mut Frame, w: u16, border: Color, lines: impl Fn(usize) -> Vec<Line<'static>>) {
    let full = f.area();
    let (probe, float) = modal_area(full, w, full.height);
    let inner_w = probe.width.saturating_sub(if float { 6 } else { 4 }) as usize;
    let body = lines(inner_w);
    let h = body.len() as u16 + modal_chrome(full);
    let inner = modal_frame(f, w, h, border);
    f.render_widget(Paragraph::new(body), inner);
}

/// Max height of fixed-size popups (they don't fill a large screen)
const FILL_H: u16 = 44;

/// Fixed-size popups (plugins page, accounts page): on desktop w wide, full screen height (two lines off top and bottom,
/// at most FILL_H; a page that asks for only `rows` lines gets that, plus one line for the message at the bottom),
/// full screen on a phone; exactly h lines inside
fn modal_fill(f: &mut Frame, w: u16, rows: Option<u16>, border: Color, lines: impl Fn(usize, usize) -> Vec<Line<'static>>) {
    let full = f.area();
    let most = full.height.saturating_sub(4).min(FILL_H);
    let h = match rows {
        _ if !floating(full) => full.height,
        Some(r) => most.min(r + 1 + modal_chrome(full)),
        None => most,
    };
    let inner = modal_frame(f, w, h, border);
    let mut body = lines(inner.width as usize, inner.height as usize);
    body.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(body), inner);
}

/// The tool menu shows at most this many rows at a time; more scroll with the highlight
const TOOL_ROWS: usize = 12;

/// What gets written into config.toml first when the settings page opens it in an editor and the file doesn't exist yet
const CONFIG_TEMPLATE: Tr = Tr::new(
    "# aca 的配置:账号、工具、界面的设置。写法见 https://github.com/cheny-00/aca/blob/main/docs/configuration.zh-CN.md\n",
    "# aca config: accounts, tools, interface settings. See https://github.com/cheny-00/aca/blob/main/docs/configuration.md\n",
);

/// `n`'s tool menu: one tool per row, "number name provider quota" (tools you named show the command), name colored by provider, the
/// highlighted row on light purple. `n` / `H`'s tool menu; with pass = picking for a hand off: the title says who's handed off, and
/// mentions if it's running or has a note
fn draw_tools(f: &mut Frame, menu: &[Tool], sel: usize, pass: Option<&Pass>, quotas: &[crate::usage::Quota]) {
    let (at, off) = (epoch() as i64, crate::usage::local_offset());
    modal(f, 72, theme::pal().accent, |w| {
        let name_w = menu.iter().map(|t| t.name.width()).max().unwrap_or(0).min(12);
        let start = (sel + 1).saturating_sub(TOOL_ROWS).min(menu.len().saturating_sub(TOOL_ROWS));
        let title = pass.map_or_else(|| t!("选择工具", "Pick a tool").to_string(), |p| tr!("交接「{}」给", "Hand off “{}” to", p.t.title));
        let mut out = vec![modal_title(&title, theme::accent(), esc_hint(t!("取消", "cancel")), w), Line::raw("")];
        if let Some(p) = pass {
            let mut marks = vec![];
            if p.busy {
                marks.push(Span::styled(t!("仍在运行", "it's still running"), theme::warn()));
            }
            if p.note.is_some() {
                if !marks.is_empty() {
                    marks.push(Span::styled(" · ", theme::faint()));
                }
                marks.push(Span::styled(t!("带上便签", "with the note"), dim()));
            }
            if !marks.is_empty() {
                out.extend([Line::from(marks), Line::raw("")]);
            }
        }
        for (i, t) in menu.iter().enumerate().skip(start).take(TOOL_ROWS) {
            let num = if i < 9 { format!("{} ", i + 1) } else { "  ".into() };
            let name = clip(&t.name, name_w);
            let mut line = vec![Span::styled(num, dim()), Span::raw(""), Span::raw("  ")];
            let room = w.saturating_sub(2 + name_w + 2);
            match &t.problem {
                // Unusable: name not colored, the reason after it
                Some(p) => {
                    line[1] = Span::styled(name, dim());
                    line.push(Span::styled(clip(p, room).trim_end().to_string(), theme::warn()));
                }
                // Account: provider, quota (only Claude and Codex have quota)
                None if matches!(t.agent, Agent::Claude | Agent::Codex) && t.note.is_empty() => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    let fam = if t.agent == Agent::Codex { "Codex " } else { "Claude" };
                    line.push(Span::styled(format!("{fam}  "), dim()));
                    let who = if t.account.is_empty() { &t.name } else { &t.account };
                    line.extend(crate::rehome::quota_spans(crate::rehome::quota_of(quotas, t.agent, who).as_ref(), at, off));
                }
                None if matches!(t.agent, Agent::Pi | Agent::Opencode) && t.note.is_empty() => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    line.push(Span::styled(if t.agent == Agent::Pi { "Pi" } else { "OpenCode" }, dim()));
                }
                None => {
                    line[1] = Span::styled(name, agent_style(t.agent).add_modifier(Modifier::BOLD));
                    line.push(Span::styled(clip(&t.note, room).trim_end().to_string(), dim()));
                }
            }
            let mut line = Line::from(line);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        // No adding accounts during a hand off (pressing + would drop the hand off halfway)
        let keys: &[(&str, &str)] = if pass.is_some() {
            &[("⏎", t!("交接", "hand off"))]
        } else {
            &[("⏎", t!("选择", "select")), ("+", t!("添加账号", "add account"))]
        };
        out.push(keys_line(keys, vec![], w));
        out
    });
}

/// Confirm for delete / restart: delete uses a red box (the state red). First line asks what, then which session, what to watch out for
/// (attached elsewhere, worktree directory), last the y button; any other key cancels
fn draw_confirm(f: &mut Frame, c: &Confirm, all: &[Session]) {
    let danger = theme::pal().red;
    let red = Style::new().fg(danger);
    let warn = theme::warn();
    let (name, title, border, head, what, notes, act): (&str, &str, Color, &str, String, Vec<String>, &str) = match c {
        Confirm::Remove(r) if r.pane.is_some() => {
            let split = r.pane.as_ref().is_some_and(|p| p.1);
            let head = if r.next.is_some() {
                t!("关闭此窗格？", "Close this pane?")
            } else if split {
                t!("关闭窗格？", "Close the pane?")
            } else {
                t!("关闭窗口？", "Close the window?")
            };
            let notes = r.next.iter().map(|n| tr!("其余依次前移，{n}成为首个", "the rest move up, {n} becomes first")).collect();
            (r.name.as_str(), r.title.as_str(), danger, head, String::new(), notes, t!("关闭", "close"))
        }
        Confirm::Remove(r) => {
            // Say the result first: registered ones go to the Trash, recoverable; plain tmux sessions are really closed
            let mut notes = vec![if r.id.is_empty() {
                t!("普通 tmux 会话:将直接关闭,无法撤销", "a plain tmux session: closed for good, no undo").to_string()
            } else {
                t!("移入回收站，保留 1 天 · b 恢复", "goes to the trash; b brings it back within a day").to_string()
            }];
            if r.worktree {
                notes.push(t!("同时删除 worktree 目录", "the worktree directory is deleted too").to_string());
            }
            if r.others > 0 {
                notes.push(tr!("同时关闭其中的 {} 个窗口 / 窗格", "{} windows / panes opened inside close too", r.others));
            }
            if r.attached {
                notes.push(t!("⇄ 在别处打开，将被断开", "⇄ open elsewhere; it will be disconnected").to_string());
            }
            if r.kids > 0 {
                notes.push(tr!("{} 个子会话不会删除", "{} child sessions won't be deleted with it", r.kids));
            }
            (r.name.as_str(), r.title.as_str(), danger, t!("删除会话？", "Delete the session?"), String::new(), notes, t!("删除", "delete"))
        }
        Confirm::Group(g) => {
            let mut notes = vec![];
            if g.sessions > 0 {
                let mut also = vec![];
                if g.busy > 0 {
                    also.push(tr!("{} 个运行中", "{} running", g.busy));
                }
                if g.archived > 0 {
                    also.push(tr!("{} 个已归档", "{} archived", g.archived));
                }
                let also = if also.is_empty() { String::new() } else { tr!("（{}）", " ({})", also.join(t!("，", ", "))) };
                notes.push(tr!("同时删除其中 {} 个会话{also}", "its sessions are deleted too: {}{also}", g.sessions));
            }
            if g.kids > 0 {
                notes.push(tr!("同时删除 {} 个子分组", "subgroups deleted with it: {}", g.kids));
            }
            notes.push(
                t!("移入回收站，保留 1 天 · b 恢复整组", "goes to the trash; b brings the whole group back within a day").to_string(),
            );
            (
                g.group.as_str(),
                group_label(&g.group),
                danger,
                t!("删除分组？", "Delete the group?"),
                String::new(),
                notes,
                t!("删除", "delete"),
            )
        }
        Confirm::Restart { t, why, force } => {
            let border = if *force { danger } else { theme::pal().accent };
            let head = if *force { t!("强制重启？", "Force a restart?") } else { t!("重启会话？", "Restart the session?") };
            let notes = if why.is_empty() { vec![] } else { vec![why.clone()] };
            (
                t.name.as_str(),
                t.title.as_str(),
                border,
                head,
                String::new(),
                notes,
                if *force { t!("强制重启", "force restart") } else { t!("重启", "restart") },
            )
        }
        Confirm::Stop { t, attached } => {
            let notes = if *attached {
                vec![t!("⇄ 在别处打开，将被断开", "⇄ open elsewhere; it will be disconnected").to_string()]
            } else {
                vec![]
            };
            (
                t.name.as_str(),
                t.title.as_str(),
                theme::pal().accent,
                t!("停止会话？", "Stop the session?"),
                String::new(),
                notes,
                t!("停止", "stop"),
            )
        }
        Confirm::Pane { t, row, restart, why, next, .. } => {
            let mut notes: Vec<String> = [why.clone()].into_iter().filter(|w| !w.is_empty()).collect();
            if *restart {
                notes.push(t!("只重启此窗格，其他窗口不受影响", "restarts only this pane; other windows are untouched").into());
            } else {
                notes.push(t!("停止程序，保留窗格 · ⏎ 重新启动", "the program stops, the pane stays, ⏎ starts it again").into());
                notes.extend(next.iter().map(|n| tr!("{n}将成为首个", "{n} moves up to be first")));
            }
            let (head, act) = if *restart {
                (t!("重启此窗格？", "Restart this pane?"), t!("重启", "restart"))
            } else {
                (t!("停止此窗格？", "Stop this pane?"), t!("停止", "stop"))
            };
            (row.as_str(), t.title.as_str(), theme::pal().accent, head, String::new(), notes, act)
        }
        Confirm::Send { t, text, .. } => {
            // What gets sent: the note's text, only the first few lines when too long
            let mut lines: Vec<&str> = text.lines().collect();
            let more = lines.len() > 4;
            lines.truncate(4);
            let mut what = lines.join("\n");
            if more {
                what.push_str("\n…");
            }
            (t.name.as_str(), t.title.as_str(), theme::pal().accent, t!("发送便签？", "Send the note?"), what, vec![], t!("发送", "send"))
        }
        Confirm::Adopt { t, plan } => (
            t.name.as_str(),
            t.title.as_str(),
            theme::pal().accent,
            t!("接管这个会话？", "Adopt this session?"),
            adopt_summary(plan),
            vec![],
            t!("接管", "adopt"),
        ),
        Confirm::Many { act, ts, notes } => return draw_many(f, *act, ts, notes, all),
    };
    let s = all.iter().find(|s| s.name == name);
    let head_style = if border == danger { red } else { theme::accent() };
    modal(f, 60, border, |w| {
        let (ic, sty) = match c {
            Confirm::Group(_) => ("▾", theme::faint()),
            _ => s.map_or(("·", dim()), session_icon),
        };
        let label = s.map(|s| s.agent_label()).unwrap_or_default();
        let who = s.map_or(Style::new(), |s| agent_style(s.agent));
        let mut out = vec![
            modal_title(head, head_style, esc_hint(t!("取消", "cancel")), w),
            Line::raw(""),
            spread(
                vec![
                    Span::styled(ic, sty),
                    Span::raw(" "),
                    Span::styled(
                        clip(title, w.saturating_sub(4 + label.width())).trim_end().to_string(),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                ],
                vec![Span::styled(label, who)],
                w,
            ),
        ];
        out.extend(what.lines().flat_map(|l| wrap_width(l, w)).map(|l| Line::styled(l, dim())));
        out.extend(notes.iter().map(|n| Line::styled(clip(n, w).trim_end().to_string(), warn)));
        out.push(Line::raw(""));
        out.push(Line::from(vec![button(&format!("y {act}"), border)]));
        out
    });
}

/// The multi-select confirm lists at most this many sessions; beyond that it says how many more
const MANY_SHOWN: usize = 6;

/// Confirm for d / D / R after v multi-select: what it asks, one session per row (icon, title, account, as for a single one), what to watch
/// out for, the y button
fn draw_many(f: &mut Frame, act: Batch, ts: &[Target], notes: &[String], all: &[Session]) {
    let danger = theme::pal().red;
    let n = ts.len();
    let (head, label, border) = match act {
        Batch::Remove => (tr!("删除 {n} 个会话？", "Delete {n} sessions?"), t!("删除", "delete"), danger),
        Batch::Stop => (tr!("停止 {n} 个会话？", "Stop {n} sessions?"), t!("停止", "stop"), theme::pal().accent),
        Batch::Restart => (tr!("重启 {n} 个会话？", "Restart {n} sessions?"), t!("重启", "restart"), theme::pal().accent),
    };
    let head_style = if border == danger { Style::new().fg(danger) } else { theme::accent() };
    // Too many: list only the first few, the last line says how many more
    let shown = if n > MANY_SHOWN { MANY_SHOWN - 1 } else { n };
    modal(f, 60, border, |w| {
        let mut out = vec![modal_title(&head, head_style, vec![], w), Line::raw("")];
        for t in &ts[..shown] {
            let s = all.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile);
            let (ic, sty) = s.map_or(("·", dim()), session_icon);
            let who = s.map(|s| s.agent_label()).unwrap_or_default();
            let who_st = s.map_or(Style::new(), |s| agent_style(s.agent));
            let title = clip(&t.title, w.saturating_sub(4 + who.width())).trim_end().to_string();
            out.push(spread(
                vec![Span::styled(ic, sty), Span::raw(" "), Span::styled(title, Style::new().add_modifier(Modifier::BOLD))],
                vec![Span::styled(who, who_st)],
                w,
            ));
        }
        if shown < n {
            out.push(Line::styled(tr!("还有 {} 个", "{} more", n - shown), dim()));
        }
        out.extend(notes.iter().map(|m| Line::styled(clip(m, w).trim_end().to_string(), theme::warn())));
        out.push(Line::raw(""));
        out.push(Line::from(vec![button(&format!("y {label}"), border)]));
        out
    });
}

/// `A`: work out what adopting this tmux session would look like (look only, touch nothing)
fn adopt_plan(target: &str) -> Result<aca_core::adopt::Plan, String> {
    let store = aca_core::store::Store::open()?;
    aca_core::adopt::plan(&store, &aca_core::config::load(), target, &aca_core::adopt::Opts::default())
}

/// In the adopt confirm: what it was recognized as, and what happens after adopting. One thing per line
fn adopt_summary(p: &aca_core::adopt::Plan) -> String {
    let mut v = vec![tr!(
        "登记为 {}{}",
        "register as {}{}",
        p.who(),
        if p.s.group.is_empty() { String::new() } else { tr!(" · 分组 {}", " · group {}", p.s.group) }
    )];
    v.push(match p.s.conv.as_str() {
        "" if p.s.tool == "shell" => t!("其中的程序不受影响", "the program inside is left alone").to_string(),
        "" => t!("无法识别对话：重启后将开始新对话", "couldn't tell which conversation: a restart starts a new one").to_string(),
        c => tr!(
            "对话 {}，进程不受影响，重启后继续",
            "conversation {}; the process is left alone and it continues after a restart",
            c.chars().take(8).collect::<String>()
        ),
    });
    if p.title() != p.tmux {
        v.push(tr!("tmux 会话名改为「{}」", "tmux session renamed to “{}”", p.title()));
    }
    if !p.s.args.is_empty() {
        v.push(tr!("每次启动时附带 {}", "every start adds {}", p.s.args.join(" ")));
    }
    if p.deck.is_some() {
        v.push(
            t!(
                "agent-deck 中会显示为已停止，请勿在 agent-deck 中再次启动",
                "agent-deck will show it as stopped; don't start it there again"
            )
            .to_string(),
        );
    }
    v.join("\n")
}

/// Default title when forking (same as what `session::fork` gives when no title is passed)
fn fork_title(title: &str) -> String {
    tr!("{title} (分叉)", "{title} (fork)")
}

/// Input popup for rename / new group / note (when the preview is off)
fn draw_prompt(f: &mut Frame, p: &Prompt) {
    let (title, keys): (String, &[(&str, &str)]) = match &p.ask {
        Ask::Rename(_) => (t!("重命名", "Rename").into(), &[("⏎", t!("确定", "ok"))]),
        Ask::NewGroup => (t!("新建分组", "New group").into(), &[("⏎", t!("下一步", "next"))]),
        // A note's Esc means done (not cancel): like the bottom bar when writing a note in the preview, first in the key line
        Ask::Note { title, .. } => (
            tr!("便签 · {title}", "Note · {title}"),
            &[("Esc", t!("保存", "done")), ("⏎", t!("换行", "newline")), ("^G", t!("编辑器", "editor"))],
        ),
        Ask::Fork(t) => (tr!("分叉 · {}", "Fork · {}", t.title), &[("⏎", t!("分叉", "fork")), ("^U", t!("清空重写", "clear and rename"))]),
        Ask::Space(c) => (tr!("重命名工作区 {c}", "Rename workspace {c}"), &[("⏎", t!("确定", "ok"))]),
        Ask::GroupDir => (t!("新分组目录", "new group directory").into(), &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete"))]),
        Ask::GroupName(g) => (tr!("改分组 · {g}", "Edit group · {g}"), &[("⏎", t!("下一步", "next"))]),
        Ask::GroupHome { to, .. } => (
            tr!("改分组 · {to} · 默认目录", "Edit group · {to} · default directory"),
            &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete")), ("^U", t!("清空 = 不设置", "empty = forget"))],
        ),
    };
    // Cancel's Esc goes to the right of the title, like other popups
    let esc = if matches!(p.ask, Ask::Note { .. }) { vec![] } else { esc_hint(t!("取消", "cancel")) };
    modal(f, 64, theme::pal().accent, |w| {
        let mut out = vec![modal_title(&title, theme::accent(), esc.clone(), w), Line::raw("")];
        if let Ask::Note { .. } = p.ask {
            // Multi-line: at most 8 lines, keeping the last few (where the cursor is)
            let mut body: Vec<String> = p.text.split('\n').flat_map(|l| wrap_width(l, w.saturating_sub(1))).collect();
            match body.last_mut() {
                Some(l) if l.width() < w => l.push('▏'),
                _ => body.push("▏".into()),
            }
            let skip = body.len().saturating_sub(8);
            out.extend(body.into_iter().skip(skip).map(Line::raw));
        } else {
            out.push(input_row(&p.text, vec![], w));
        }
        out.push(input_rule(w));
        if !p.hint.is_empty() {
            out.push(Line::styled(clip(&p.hint, w).trim_end().to_string(), theme::warn()));
        }
        out.push(Line::raw(""));
        out.push(keys_line(keys, vec![], w));
        out
    });
}

/// Max rows of candidates listed for new
const DRAFT_ROWS: usize = 8;

/// `N`'s new popup: which tool, which group it lands in (follows the path), the directory input box, below it Tab-completion candidates
fn draw_draft(f: &mut Frame, d: &Draft) {
    modal(f, 78, theme::pal().accent, |w| {
        // The label column: Chinese labels are all two chars (4 cells); English ones are padded to the same width
        let lab = |t: &str| Span::styled(format!("{t}{}", " ".repeat(7usize.saturating_sub(t.width()).max(2))), dim());
        let shell = d.is_shell();
        let tool = match () {
            _ if shell => "shell".to_string(),
            _ if d.label.is_empty() => t!("默认", "default").to_string(),
            _ => d.label.clone(),
        };
        let tool_style = match d.label.split('·').next() {
            _ if shell => Style::new(),
            Some("Claude") => agent_style(Agent::Claude),
            Some("Codex") => agent_style(Agent::Codex),
            Some("Pi") => agent_style(Agent::Pi),
            Some("OpenCode") => agent_style(Agent::Opencode),
            _ => Style::new(),
        };
        // ^T switches between the reused tool and a shell (no need if shell was picked in n)
        let swap = match () {
            _ if d.shell => t!("^T 换回", "^T back"),
            _ if shell => "",
            _ => "^T shell",
        };
        let (group, new) = d.group();
        let mut group_line = vec![lab(t!("分组", "group"))];
        if group.is_empty() {
            group_line.push(Span::styled(t!("默认", "default"), dim()));
        } else {
            group_line.push(Span::styled(clip(&group, 24).trim_end().to_string(), Style::new().add_modifier(Modifier::BOLD)));
            if new {
                group_line.push(Span::styled(
                    if d.binds_dir() {
                        t!("  新分组 · 绑定目录", "  new group · bound to the directory")
                    } else {
                        t!("  新分组", "  new group")
                    },
                    theme::warn(),
                ));
            }
        }
        let mark = if d.is_new_dir() { vec![Span::styled(t!("新目录", "new directory"), theme::warn())] } else { vec![] };
        let mut out = vec![
            modal_title(t!("新建会话", "New session"), theme::accent(), esc_hint(t!("取消", "cancel")), w),
            Line::raw(""),
            spread(
                vec![
                    lab(t!("工具", "tool")),
                    Span::styled(
                        clip(&tool, w.saturating_sub(7 + swap.width() + 2)).trim_end().to_string(),
                        tool_style.add_modifier(Modifier::BOLD),
                    ),
                ],
                vec![Span::styled(swap.to_string(), dim())],
                w,
            ),
            Line::from(group_line),
            Line::raw(""),
        ];
        let field = w.saturating_sub(7);
        let mut input = input_row(&d.text, mark, field);
        input.spans.insert(0, lab(t!("目录", "dir")));
        out.push(input);
        out.push(Line::from(vec![Span::raw(" ".repeat(7)), Span::styled("─".repeat(field), dim())]));
        if !d.menu.is_empty() {
            let sel = d.sel.unwrap_or(0);
            let start = (sel + 1).saturating_sub(DRAFT_ROWS).min(d.menu.len().saturating_sub(DRAFT_ROWS));
            for (i, m) in d.menu.iter().enumerate().skip(start).take(DRAFT_ROWS) {
                let mut line = Line::from(vec![Span::raw(" ".repeat(7)), Span::raw(clip(m, field).trim_end().to_string())]);
                if d.sel == Some(i) {
                    let pad = w.saturating_sub(line.width());
                    line.spans.push(Span::raw(" ".repeat(pad)));
                    line = line.patch_style(Style::new().bg(theme::pal().sel).add_modifier(Modifier::BOLD));
                }
                out.push(line);
            }
            let more = d.menu.len().saturating_sub(DRAFT_ROWS);
            if more > 0 {
                out.push(Line::styled(tr!("{}… 还有 {more} 项", "{}… {more} more", " ".repeat(7)), dim()));
            }
        } else if !d.hint.is_empty() {
            out.push(Line::styled(format!("{}{}", " ".repeat(7), clip(&d.hint, field).trim_end()), theme::warn()));
        }
        out.push(Line::raw(""));
        out.push(keys_line(
            &[("Tab", t!("补全", "complete")), ("↓", "fzf")],
            vec![button(t!("⏎ 新建", "⏎ create"), theme::pal().accent)],
            w,
        ));
        out
    });
}

/// `?`'s keys popup. On desktop centered, three columns in one screen; on a phone it rises from the bottom, one column scrolling down, a
/// few list rows showing on top
fn draw_help(f: &mut Frame, app: &mut App) {
    let full = f.area();
    let float = floating(full);
    let w = if float { full.width.saturating_sub(8).min(116) } else { full.width };
    let (probe, _) = modal_area(full, w, full.height);
    let inner_w = probe.width.saturating_sub(if float { 6 } else { 4 }) as usize;
    let hit = app.reader.as_ref().and_then(|r| r.hit.clone());
    let (lines, lit) = crate::help::lines(inner_w, hit.as_ref());
    let total = lines.len();
    // Title, blank line, (content), blank line, a line at the bottom
    let room = full.height.saturating_sub(if float { 2 } else { 3 } + modal_chrome(full) + 4) as usize;
    let body_h = total.min(room).max(1);
    let Some(r) = app.reader.as_mut() else { return };
    r.scroll = r.scroll.min(total - body_h);
    // Just pressed a key: if the first lit row isn't on this screen, scroll to it (stop one row above it); after that, scroll freely
    if let Some(h) = r.hit.as_mut().filter(|h| h.jump) {
        h.jump = false;
        let top = total - body_h - r.scroll;
        if let Some(&y) = lit.first().filter(|&&y| y < top || y >= top + body_h) {
            r.scroll = total - body_h - y.saturating_sub(1).min(total - body_h);
        }
    }
    r.view_h = body_h;
    let end = total - r.scroll;
    let top = end - body_h;
    let pages = total.div_ceil(body_h);
    let page = if r.scroll == 0 { pages } else { top / body_h + 1 };
    let pos = if pages > 1 { vec![Span::styled(format!("{page}/{pages}"), dim())] } else { esc_hint(t!("关闭", "close")) };
    // Left of the bottom bar: say something only when the pressed key isn't a shortcut; for a shortcut, its rows light up by themselves
    let said = match &hit {
        Some(h) if lit.is_empty() => Span::styled(tr!("「{}」不是快捷键", "“{}” isn't a key", h.label), dim()),
        _ => Span::raw(""),
    };
    let right =
        if float && pages == 1 { vec![] } else { key_spans(&[("↑↓", t!("滚动", "scroll")), ("Esc", t!("关闭", "close"))], 30).0 };
    let mut body = vec![modal_title(t!("快捷键", "Keys"), theme::accent(), pos, inner_w), Line::raw("")];
    body.extend(lines[top..end].iter().cloned());
    if !(said.content.is_empty() && right.is_empty()) {
        body.push(Line::raw(""));
        body.push(spread(vec![said], right, inner_w));
    }
    let inner = modal_frame(f, w, body.len() as u16 + modal_chrome(full), theme::pal().accent);
    f.render_widget(Paragraph::new(body), inner);
}

/// Full-screen reader (`z`): an accent-colored line with the title on top (like the preview on a phone), the wrapped output, the bottom bar
fn draw_reader(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let w = area.width as usize;
    let h = body.height as usize;
    let pos = app.list.selected().unwrap_or(0);
    let n = app.view.len();
    let Some(r) = app.reader.as_mut() else { return };
    if r.dirty || r.wrap_w != w {
        let old = r.wrapped.len();
        let old_top = r.top(h);
        let same_width = r.wrap_w == w;
        let (wrapped, starts) = ansi::wrap_rows(&r.lines, w);
        r.wrapped = wrapped;
        if let Some(d) = r.diff.as_mut() {
            d.rows = d.files.iter().filter_map(|&i| starts.get(i).copied()).collect();
            d.starts = starts;
            // Diff refreshed: keep the same line number at the top
            if same_width {
                r.scroll = r.wrapped.len().saturating_sub(old_top + h);
            }
        } else if same_width && r.scroll > 0 {
            // New content grows at the bottom while scrolled up: add the distance from the bottom so the view stays put
            // (not when a rotation changed the width)
            r.scroll = r.scroll.saturating_add(r.wrapped.len().saturating_sub(old));
        }
        r.wrap_w = w;
        r.dirty = false;
    }
    let total = r.wrapped.len();
    r.scroll = r.scroll.min(total.saturating_sub(h));
    let end = total - r.scroll;
    let top = end.saturating_sub(h);
    r.view_h = h;
    let rows: Vec<Styled> = r.wrapped[top..end].to_vec();
    let scroll = r.scroll;
    let name = r.name.clone();
    let empty = r.lines.is_empty();
    // Viewing a diff: uncommitted / with commits, which file you're on
    let diff = r.diff.as_ref().map(|d| {
        let mode = if d.mode == Mode::Uncommitted { t!("未提交", "uncommitted") } else { t!("连同提交", "with commits") };
        let cur = d.rows.iter().filter(|&&x| x <= top).count();
        let at = match (d.loading, d.rows.len()) {
            (true, _) => t!(" · 读取中…", " · loading…").to_string(),
            (_, 0) => String::new(),
            (_, all) if cur == 0 => {
                let w = noun(all as u64, "file", "files");
                tr!(" · {all} 个文件", " · {all} {w}")
            }
            (_, all) => tr!(" · 文件 {cur}/{all}", " · file {cur}/{all}"),
        };
        (d.mode, format!("{mode}{at}"))
    });

    let s = app.all.iter().find(|s| s.name == name);
    let ic = s.map_or(("·", dim()), session_icon);
    let where_ = if scroll == 0 { t!("最新", "latest").to_string() } else { tr!("↑{scroll} 行", "↑{scroll} lines") };
    let title = s.map_or(name.as_str(), |s| s.title.as_str());
    let plugin = r.plugin.is_some();
    let (title, right) = match &diff {
        Some((_, label)) => (tr!("改动 · {title}", "Diff · {title}"), label.clone()),
        // A plugin's output isn't a session: no "which session number"
        None if plugin => (title.to_string(), where_),
        None => (title.to_string(), format!("{}/{n} · {where_}", pos + 1)),
    };
    f.render_widget(Paragraph::new(title_rule(ic, &title, vec![Span::styled(right, dim())], w)), head);
    if empty {
        f.render_widget(Paragraph::new(t!(" 无内容 · 会话可能已结束", " nothing here · the session may have ended")).style(dim()), body);
    } else {
        f.render_widget(Paragraph::new(rows.iter().map(ansi::to_line).collect::<Vec<_>>()), body);
        ansi::paint_links(f.buffer_mut(), body, &rows);
    }
    let foot_line = match &app.msg {
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ => match diff {
            Some((mode, _)) => menu_bar(
                &[
                    // On a phone drop 「滚动」 ("scroll") first (everyone knows it), keep n p and m
                    vec![
                        ("↑↓", t!("滚动", "scroll"), 2),
                        ("n p", t!("切换文件", "file"), 0),
                        (
                            "m",
                            if mode == Mode::Uncommitted {
                                t!("连同提交", "with commits")
                            } else {
                                t!("仅未提交", "uncommitted only")
                            },
                            1,
                        ),
                        ("J K", t!("切换会话", "session"), 5),
                    ],
                    vec![("⏎", t!("打开", "open"), 0), ("e", t!("编辑", "edit"), 4), ("l", "lazygit", 6), ("Tab", t!("记录", "record"), 3)],
                    vec![("Esc", t!("返回", "back"), 0)],
                ],
                Some(1),
                w,
            ),
            None if plugin => menu_bar(
                &[
                    vec![("↑↓", t!("滚动", "scroll"), 0), (t!("空格 b", "Space b"), t!("翻页", "page"), 2)],
                    vec![("r", t!("重新运行", "rerun"), 0), ("c", t!("复制", "copy"), 1)],
                    vec![("Esc", t!("返回", "back"), 0)],
                ],
                Some(1),
                w,
            ),
            None => menu_bar(
                &[
                    vec![
                        ("↑↓", t!("滚动", "scroll"), 0),
                        (t!("空格 b", "Space b"), t!("翻页", "page"), 2),
                        ("J K", t!("切换会话", "session"), 1),
                    ],
                    vec![("⏎", t!("打开", "open"), 0), ("c", t!("复制", "copy"), 3), ("Tab", t!("改动", "diff"), 4)],
                    vec![("Esc", t!("返回", "back"), 0)],
                ],
                Some(1),
                w,
            ),
        },
    };
    f.render_widget(Paragraph::new(foot_line), foot);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Record: split into two panes only this wide (the conversation on the left and the steps on the right each need room for a sentence)
const TRACE_SPLIT: usize = 90;

/// Max lines of a turn shown at first in the conversation (what you said / what Claude said); `o` shows the full text
const ASK_LINES: usize = 6;
const REPLY_LINES: usize = 12;

/// Z session record: full screen, a title line on top (turns · how long it worked · how much it cost); on desktop the conversation left,
/// the selected turn's steps right; on a phone the whole screen is one side; ⏎ views one step full screen; keys at the bottom
fn draw_trace(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (w, h) = (area.width as usize, body.height as usize);
    let live = app.trace_live();
    let now = now_ms();
    let msg = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.clone());
    let Some(v) = app.trace.as_mut() else { return };
    // Theme changed (settings page preview, theme file edited): re-lay everything already laid out
    let pal = theme::pal() as *const theme::Pal as usize;
    if v.pal != pal {
        v.pal = pal;
        v.chat_cache.clear();
        v.steps_cache = None;
        for lv in &mut v.saved {
            lv.chat_cache.clear();
            lv.steps_cache = None;
        }
        if let Some(d) = v.detail.as_mut() {
            d.lines.clear();
            d.wrap_w = 0;
        }
    }
    let s = app.all.iter().find(|s| s.name == v.name);
    let ic = s.map_or(("·", dim()), session_icon);
    let mut title = s.map_or(v.name.as_str(), |s| s.title.as_str()).to_string();
    // Inside a subagent: the title says who it is; running or not goes by its own
    let live = v.live(live, now);
    if let (Some(t), Some((&tool, up))) = (v.t.as_ref(), v.path.split_last())
        && let Some(sub) = t.at(up).sub_of(tool)
    {
        title = tr!(
            "子代理 · {}",
            "Subagent · {}",
            [sub.kind.as_str(), sub.desc.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ")
        );
    }
    let ic = if v.path.is_empty() {
        ic
    } else if live {
        ("●", Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD))
    } else {
        ("✓", dim())
    };
    v.wide = w >= TRACE_SPLIT;
    let foot_line = |groups: &[Vec<MenuItem>], action: Option<usize>| match &msg {
        Some(m) => Line::styled(m.clone(), theme::warn()).centered(),
        None => menu_bar(groups, action, w),
    };
    // One step full screen
    if let (Some(d), Some(t)) = (v.detail.as_mut(), v.t.as_ref().map(|t| t.at(&v.path))) {
        if d.lines.is_empty() {
            d.lines = crate::trace::detail(t, d.node, live, now);
        }
        if d.wrap_w != w {
            d.wrapped = crate::trace::wrap(&d.lines, w.saturating_sub(3).max(1));
            d.wrap_w = w;
        }
        d.view_h = h;
        d.scroll = d.scroll.min(d.wrapped.len().saturating_sub(h));
        let n = &t.nodes[d.node];
        let dic = match n.state {
            crate::trace::St::Err | crate::trace::St::Denied => ("✕", Style::new().fg(theme::pal().red).add_modifier(Modifier::BOLD)),
            _ if t.running(d.node, live) => ("●", Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD)),
            _ => ("✓", dim()),
        };
        let pos = if d.wrapped.len() > h {
            tr!("{}/{} 行", "{}/{} lines", (d.scroll + h).min(d.wrapped.len()), d.wrapped.len())
        } else {
            clip(&title, 24).trim_end().to_string()
        };
        f.render_widget(Paragraph::new(title_rule(dic, &crate::trace::where_of(t, d.node), vec![Span::styled(pos, dim())], w)), head);
        let rows: Vec<Line> = d.wrapped.iter().skip(d.scroll).take(h).map(|l| pad_line(l, "  ")).collect();
        f.render_widget(Paragraph::new(rows), body);
        let keys = vec![
            vec![
                ("↑↓", t!("滚动", "scroll"), 2),
                (t!("空格 b", "Space b"), t!("翻页", "page"), 3),
                ("n p", t!("上 / 下一项", "prev / next"), 1),
            ],
            vec![("Esc", t!("返回", "back"), 0)],
        ];
        f.render_widget(Paragraph::new(foot_line(&keys, None)), foot);
        return;
    }
    // Viewing steps on a phone: the top says which turn
    let phone_steps = !v.wide && v.in_steps;
    let head_t = v.t.as_ref().map(|t| t.at(&v.path));
    let title = if v.path.is_empty() { tr!("记录 · {title}", "Record · {title}") } else { title };
    let (label, right) = match (head_t, v.loading) {
        (_, true) => (title.clone(), t!("读取中…", "loading…").to_string()),
        (None, false) => (title.clone(), String::new()),
        (Some(t), false) if phone_steps && t.nodes.get(v.chat.sel).is_some() => {
            let i = v.chat.sel;
            let running = t.running(i, live);
            let s = t.sum(i);
            let at = crate::trace::where_of(t, i);
            let at = if v.path.is_empty() { at } else { tr!("子代理 · {at}", "Subagent · {at}") };
            (at, format!("{} · {}", crate::trace::fmt_dur(t.dur(i, running, now)), crate::trace::fmt_cost(s.cost)))
        }
        (Some(t), false) => {
            let (turns, dur, s) = t.total(live, now);
            let unit = noun(turns as u64, "turn", "turns");
            let right = if w >= 60 {
                tr!("{turns} 轮 · {} · {}", "{turns} {unit} · {} · {}", crate::trace::fmt_dur(dur), crate::trace::fmt_cost(s.cost))
            } else {
                tr!("{turns} 轮 · {}", "{turns} {unit} · {}", crate::trace::fmt_cost(s.cost))
            };
            (title.clone(), right)
        }
    };
    f.render_widget(Paragraph::new(title_rule(ic, &label, vec![Span::styled(right, dim())], w)), head);
    match (v.t.as_ref().map(|t| t.at(&v.path)), &v.err) {
        (_, Some(e)) => f.render_widget(Paragraph::new(format!(" {e}")).style(dim()), body),
        (None, None) => f.render_widget(Paragraph::new(t!(" 正在读取对话记录…", " reading the conversation record…")).style(dim()), body),
        (Some(t), None) if t.roots.is_empty() => {
            f.render_widget(Paragraph::new(t!(" 暂无对话", " no conversation yet")).style(dim()), body)
        }
        (Some(_), None) if v.wide => {
            let lw = (w / 2).clamp(34, 80) as u16;
            let [left, sep, right] = Layout::horizontal([Constraint::Length(lw), Constraint::Length(1), Constraint::Fill(1)]).areas(body);
            v.split = left.x + left.width;
            draw_chat(f, v, left, live, now);
            f.render_widget(Paragraph::new(vec![Line::styled("│", theme::faint()); h]), sep);
            draw_steps(f, v, right, live, now);
        }
        (Some(_), None) => {
            // Phone: lay out the conversation first (the steps are for the turn selected in it), then draw the side being viewed
            if v.in_steps {
                lay_chat(v, w, h, live, now);
                draw_steps(f, v, body, live, now);
            } else {
                draw_chat(f, v, body, live, now);
            }
        }
    }
    let keys = match (v.wide, v.in_steps) {
        (true, false) => vec![
            vec![("↑↓", t!("移动", "move"), 3), ("→", t!("查看过程", "steps"), 0), ("o", t!("全文", "full text"), 4)],
            vec![("n p", t!("切换轮次", "turn"), 2), ("J K", t!("切换会话", "session"), 5), ("Tab", t!("输出", "output"), 6)],
            vec![("Esc", t!("返回", "back"), 0)],
        ],
        (true, true) => vec![
            vec![("↑↓", t!("移动", "move"), 3), ("o", t!("展开", "unfold"), 0), ("⏎", t!("全屏查看", "full screen"), 1)],
            vec![("← Esc", t!("返回对话", "conversation"), 2), ("n p", t!("切换轮次", "turn"), 4), ("J K", t!("切换会话", "session"), 5)],
        ],
        (false, false) => {
            vec![
                vec![("⏎", t!("查看过程", "steps"), 0), ("o", t!("全文", "full text"), 3)],
                vec![("n p", t!("切换轮次", "turn"), 1), (t!("空格 b", "Space b"), t!("翻页", "page"), 2)],
                vec![("Esc", t!("返回", "back"), 0)],
            ]
        }
        (false, true) => {
            vec![
                vec![("o", t!("展开", "unfold"), 0), ("⏎", t!("详情", "details"), 1)],
                vec![("n p", t!("切换轮次", "turn"), 2), (t!("空格 b", "Space b"), t!("翻页", "page"), 3)],
                vec![("Esc", t!("返回对话", "conversation"), 0)],
            ]
        }
    };
    f.render_widget(Paragraph::new(foot_line(&keys, Some(0))), foot);
}

/// How many cells to indent a wrapped line
fn pad_line(l: &Styled, pad: &'static str) -> Line<'static> {
    let mut line = ansi::to_line(l);
    line.spans.insert(0, Span::raw(pad));
    line
}

/// Keep only the first max lines when there are more, the last saying how many more
fn clip_lines(mut ls: Vec<Styled>, max: usize, pad: &str) -> Vec<Styled> {
    if ls.len() > max {
        let more = ls.len() - max;
        ls.truncate(max);
        let (k, w) = (crate::trace::thousands(more as u64), noun(more as u64, "line", "lines"));
        ls.push(vec![crate::trace::seg(tr!("{pad}… 还有 {k} 行", "{pad}… {k} more {w}"), theme::faint())]);
    }
    ls
}

/// Lay out the conversation on the left: one block per item from the top — a turn is what you said, what Claude said last, a line of
/// numbers; compaction one line; the copied-over block one line (unfolded, its turns follow it)
fn lay_chat(v: &mut TraceView, w: usize, h: usize, live: bool, now: i64) -> Vec<Vec<Line<'static>>> {
    let Some(t) = v.t.as_ref().map(|t| t.at(&v.path)) else { return vec![] };
    // One cell for the vertical bar on the left, one blank, one cell left on the right
    let cw = w.saturating_sub(3).max(1);
    let mut tops = vec![];
    for &r in &t.roots {
        tops.push(r);
        if t.nodes[r].kind == crate::trace::Kind::Copied && v.open.contains(&r) {
            tops.extend(t.nodes[r].kids.iter().copied());
        }
    }
    // First lay out without color and measure each block's height (cached if laid out before), decide which line to start from, then color
    // the visible ones and turn them into screen lines
    let mut sizes = vec![];
    for &i in &tops {
        let len = if t.nodes[i].kind == crate::trace::Kind::Turn {
            let full = v.open.contains(&i);
            let ver = v.version(i, live);
            if !matches!(v.chat_cache.get(&i), Some(c) if c.w == cw && c.ver == ver && c.full == full) {
                let (lines, clipped) = crate::hl::plain(|| chat_lines(t, i, cw, full, live, now));
                v.chat_cache.insert(i, ChatLaid { w: cw, ver, full, lines, clipped, colored: 0 });
            }
            v.chat_cache[&i].lines.len()
        } else {
            2
        };
        sizes.push((i, len));
    }
    v.chat.lay(t, &sizes, h);
    let (top, end) = (v.chat.top, v.chat.top + h);
    let mut moved = false;
    for (&i, &(_, s, e)) in tops.iter().zip(&v.chat.items) {
        if e <= top || s >= end {
            continue;
        }
        // Color down to the bottom of the screen (again once more shows)
        let need = end.min(e) - s;
        if let Some(c) = v.chat_cache.get_mut(&i).filter(|c| c.colored < need) {
            let (lines, _) = crate::hl::upto(need, || chat_lines(t, i, cw, c.full, live, now));
            moved |= lines.len() != c.lines.len();
            c.lines = lines;
            c.colored = need;
        }
    }
    if moved {
        // Coloring shouldn't change the line count; if it really did, reposition by the new one
        let sizes: Vec<(usize, usize)> = tops.iter().map(|&i| (i, v.chat_cache.get(&i).map_or(2, |c| c.lines.len()))).collect();
        v.chat.lay(t, &sizes, h);
    }
    let (top, end) = (v.chat.top, v.chat.top + h);
    tops.iter()
        .zip(&v.chat.items)
        .map(|(&i, &(_, s, e))| match () {
            _ if e <= top || s >= end => vec![],
            _ if t.nodes[i].kind == crate::trace::Kind::Turn => v.chat_cache[&i].lines.iter().map(|l| pad_line(l, " ")).collect(),
            _ => vec![crate::trace::row(t, i, v.open.contains(&i), w.saturating_sub(1)), Line::raw("")],
        })
        .collect()
}

/// One turn of the conversation as lines: what you said, what Claude said last (folded, each shows its first few lines), a line of numbers;
/// second value: whether folding clipped anything
fn chat_lines(t: &crate::trace::Trace, i: usize, cw: usize, full: bool, live: bool, now: i64) -> (Vec<Styled>, bool) {
    let c = crate::trace::chat(t, i, live, now);
    let (ask_max, reply_max) = if full { (usize::MAX, usize::MAX) } else { (ASK_LINES, REPLY_LINES) };
    let (asked, reply) = (crate::trace::wrap(&c.asked, cw), crate::trace::wrap(&c.reply, cw));
    let clipped = asked.len() > ASK_LINES || reply.len() > REPLY_LINES;
    let mut ls = clip_lines(asked, ask_max, "    ");
    if !reply.is_empty() {
        ls.push(vec![]);
        ls.extend(clip_lines(reply, reply_max, "  "));
    }
    ls.extend(crate::trace::wrap(&[c.foot], cw));
    ls.push(vec![]);
    (ls, clipped)
}

/// Lay out the steps on the right: the turn selected in the conversation (a new turn starts from the top; following the latest sticks to
/// the bottom). Like the left: lay out without color first, then color the visible blocks
fn lay_steps(v: &mut TraceView, w: usize, h: usize, live: bool, now: i64) -> Vec<Vec<Line<'static>>> {
    let Some(t) = v.t.as_ref().map(|t| t.at(&v.path)) else { return vec![] };
    let cur = v.chat.sel;
    if t.nodes.get(cur).is_none() {
        return vec![];
    }
    if v.steps_of != Some(cur) {
        v.steps = Blocks { sel: cur, follow: v.chat.follow, ..Blocks::default() };
        v.steps_of = Some(cur);
    }
    let cw = w.saturating_sub(3).max(1);
    let (ver, folds) = (v.version(cur, live), v.unfold_gen);
    let turn = t.nodes[cur].kind == crate::trace::Kind::Turn;
    let unfold = &v.unfold;
    let open = |x: usize| unfold.contains(&x);
    // This item isn't a turn (the copied-over block, compaction): the whole page of details as one block
    let build = |k: usize| {
        if turn {
            crate::trace::step(t, cur, k, live, now, &open)
        } else {
            Some(crate::trace::Step { node: cur, paras: crate::trace::detail(t, cur, live, now), fold: false })
        }
    };
    if !matches!(&v.steps_cache, Some(c) if c.item == cur && c.w == cw && c.ver == ver && c.folds == folds) {
        // Same turn, same width: reuse unchanged blocks (see trace::step_sig), lay out only the changed ones
        let mut old: HashMap<usize, StepLaid> = match v.steps_cache.take() {
            Some(c) if c.item == cur && c.w == cw => c.blocks.into_iter().map(|b| (b.node, b)).collect(),
            _ => HashMap::new(),
        };
        let blocks = if turn {
            let list = crate::trace::step_nodes(t, cur);
            (0..list.len())
                .filter_map(|k| {
                    let sig = crate::trace::step_sig(t, cur, &list, k, live, now, &open);
                    if let Some(b) = old.remove(&list[k]).filter(|b| b.sig == sig) {
                        return Some(b);
                    }
                    let s = crate::hl::plain(|| build(k))?;
                    // Folded thinking is clipped by line count: only clipped at this width is there anything to unfold (a phone clips more)
                    let (lines, clipped) = crate::trace::wrap_clipped(&s.paras, cw);
                    Some(StepLaid { node: s.node, lines, fold: s.fold || clipped, colored: 0, sig })
                })
                .collect()
        } else {
            crate::hl::plain(|| build(0))
                .into_iter()
                .map(|s| StepLaid { node: s.node, lines: crate::trace::wrap(&s.paras, cw), fold: s.fold, colored: 0, sig: 0 })
                .collect()
        };
        v.steps_cache = Some(StepsLaid { item: cur, w: cw, ver, folds, blocks });
    }
    let blocks = &mut v.steps_cache.as_mut().unwrap().blocks;
    let sizes: Vec<(usize, usize)> = blocks.iter().map(|b| (b.node, b.lines.len())).collect();
    v.steps.lay(t, &sizes, h);
    let (top, end) = (v.steps.top, v.steps.top + h);
    let mut moved = false;
    for (k, &(_, s, e)) in v.steps.items.iter().enumerate() {
        let need = end.min(e).saturating_sub(s);
        if e <= top || s >= end || blocks[k].colored >= need {
            continue;
        }
        if let Some(st) = crate::hl::upto(need, || build(k)) {
            let lines = crate::trace::wrap(&st.paras, cw);
            moved |= lines.len() != blocks[k].lines.len();
            blocks[k].lines = lines;
        }
        blocks[k].colored = need;
    }
    if moved {
        let sizes: Vec<(usize, usize)> = blocks.iter().map(|b| (b.node, b.lines.len())).collect();
        v.steps.lay(t, &sizes, h);
    }
    let (top, end) = (v.steps.top, v.steps.top + h);
    blocks
        .iter()
        .zip(&v.steps.items)
        .map(|(b, &(_, s, e))| if e <= top || s >= end { vec![] } else { b.lines.iter().map(|l| pad_line(l, " ")).collect() })
        .collect()
}

/// Draw a column of blocks: a vertical bar left of the selected block — accent color in the column with focus, its first line on light
/// purple; the conversation without focus gets a faint bar (the steps on the right are that turn's), the steps without focus get none
fn draw_blocks(f: &mut Frame, area: Rect, b: &Blocks, blocks: Vec<Vec<Line<'static>>>, focus: bool, faint_bar: bool) {
    let (top, h) = (b.top, area.height as usize);
    let k = b.at();
    let mut out = vec![];
    for (bi, ls) in blocks.into_iter().enumerate() {
        let Some(&(_, bs, be)) = b.items.get(bi) else { break };
        if be <= top || bs >= top + h {
            continue;
        }
        let bar = match (Some(bi) == k, focus) {
            (true, true) => Span::styled("▌", theme::accent()),
            (true, false) if faint_bar => Span::styled("▏", theme::faint()),
            _ => Span::raw(" "),
        };
        for (j, mut line) in ls.into_iter().enumerate() {
            if (top..top + h).contains(&(bs + j)) {
                line.spans.insert(0, bar.clone());
                out.push(line);
            }
        }
    }
    f.render_widget(Paragraph::new(out), area);
    if focus && let Some(s) = k.map(|k| b.items[k].1).filter(|s| (top..top + h).contains(s)) {
        f.buffer_mut()
            .set_style(Rect { x: area.x, y: area.y + (s - top) as u16, width: area.width, height: 1 }, Style::new().bg(theme::pal().sel));
    }
    let below = b.total().saturating_sub(top + h);
    if below > 0 && !b.follow {
        below_tag(f, area, below);
    }
}

/// More below: the bottom row of `area` gives way to a tag at its right saying how many lines are left (that row counted
/// too), rather than covering the end of a line
fn below_tag(f: &mut Frame, area: Rect, below: usize) {
    let n = below + 1;
    let w = noun(n as u64, "line", "lines");
    let tag = tr!(" ↓ {n} 行 ", " ↓ {n} {w} ");
    let tw = tag.width() as u16;
    if area.height < 2 || area.width <= tw + 2 {
        return;
    }
    let row = Rect { y: area.y + area.height - 1, height: 1, ..area };
    f.render_widget(ratatui::widgets::Clear, row);
    let r = Rect { x: row.x + row.width - tw, width: tw, ..row };
    f.render_widget(Paragraph::new(Line::styled(tag, dim())).style(Style::new().bg(theme::pal().sel)), r);
}

fn draw_chat(f: &mut Frame, v: &mut TraceView, area: Rect, live: bool, now: i64) {
    let blocks = lay_chat(v, area.width as usize, area.height as usize, live, now);
    draw_blocks(f, area, &v.chat, blocks, !v.in_steps, true);
}

fn draw_steps(f: &mut Frame, v: &mut TraceView, area: Rect, live: bool, now: i64) {
    let blocks = lay_steps(v, area.width as usize, area.height as usize, live, now);
    draw_blocks(f, area, &v.steps, blocks, v.in_steps, false);
}

/// Run `aca ui --cost-json` to read billing (same program, environment passed as is); only = which accounts to show
fn read_bill(only: &[String]) -> Result<crate::usage::Report, String> {
    let exe = aca_core::agent::self_path().ok_or(t!("找不到 aca 程序", "can't find the aca program"))?;
    let out = std::process::Command::new(exe)
        .args(["ui", "--cost-json"])
        .args(only.iter().flat_map(|o| ["--only", o.as_str()]))
        // Use this side's current language (if the settings page just switched it, ACA_LANG may still be the other)
        .env("ACA_LANG", i18n::lang().code())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    // Upgraded without reopening, the child is the new version and the format doesn't match: say so, reopening fixes it
    serde_json::from_slice(&out.stdout)
        .map_err(|_| t!("数据格式有误(如果刚升级过 aca,按 ^L 重启)", "unexpected format (if aca was upgraded, reopen with ^L)").to_string())
}

/// $ billing panel: full screen, the top says what time the numbers are from, content in the middle (see `bill`), keys at the bottom
fn draw_bill(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (w, h) = (area.width as usize, body.height as usize);
    let Some(b) = app.bill.as_mut() else { return };
    let lines = app.bill_data.as_ref().map(|r| crate::bill::lines(r, b.sel, b.more, w)).unwrap_or_default();
    let more = b.more;
    b.scroll = b.scroll.min(lines.len().saturating_sub(h));
    // Don't mention scrolling when it fits one screen
    let long = lines.len() > h;
    b.view_h = h;
    let scroll = b.scroll;
    let right = match (&app.bill_data, app.bill_loading) {
        (None, _) => t!("读取中…", "loading…").to_string(),
        (Some(r), _) if r.only != app.bill_only => t!("读取中…", "loading…").to_string(),
        (Some(_), true) => t!("更新中…", "updating…").to_string(),
        (Some(r), false) => {
            let t = (r.now + r.off).rem_euclid(86400);
            format!("{:02}:{:02}", t / 3600, t % 3600 / 60)
        }
    };
    // Which accounts are shown goes after the title (by the data shown)
    let title = match app.bill_data.as_ref().map(|r| &r.only) {
        Some(o) if !o.is_empty() => tr!("计费 · {}", "Billing · {}", o.join(" + ")),
        _ => t!("计费", "Billing").into(),
    };
    f.render_widget(Paragraph::new(title_rule(("$", theme::accent()), &title, vec![Span::styled(right, dim())], w)), head);
    if lines.is_empty() {
        f.render_widget(Paragraph::new(t!(" 正在读取对话记录…", " reading the conversation record…")).style(dim()), body);
    } else {
        f.render_widget(Paragraph::new(lines.into_iter().skip(scroll).take(h).collect::<Vec<_>>()), body);
    }
    let foot_line = match &app.msg {
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ => {
            let mut keys = vec![
                ("←→", t!("时段", "period"), 0),
                ("a", t!("账号", "accounts"), 1),
                ("m", if more { t!("收起", "less") } else { t!("更多", "more") }, 1),
                ("↑↓", t!("滚动", "scroll"), 2),
                ("r", t!("重新读取", "reload"), 1),
            ];
            keys.retain(|k| long || k.0 != "↑↓");
            menu_bar(&[keys, vec![("Esc", t!("返回", "back"), 0)]], None, w)
        }
    };
    f.render_widget(Paragraph::new(foot_line), foot);
    if let Some(sel) = app.bill.as_ref().and_then(|b| b.pick.as_ref()).map(|p| p.sel) {
        draw_bill_pick(f, app, sel);
    }
}

/// a's account picker floating window: first row 「全部」 ("all"), then one account per row (number, tick, name colored by provider,
/// spend over all time); the numbers of the ones shown now in accent color, selected ones ticked, the highlighted row gets a background;
/// during a range select the keys at the bottom become range-select ones
fn draw_bill_pick(f: &mut Frame, app: &App, sel: usize) {
    let accts = app.bill_accounts();
    let pick = app.bill.as_ref().and_then(|b| b.pick.as_ref());
    let marked = pick.map(|p| p.chosen(&accts)).unwrap_or_default();
    let ranging = pick.is_some_and(|p| p.anchor.is_some());
    let total: f64 = accts.iter().map(|a| a.cost).sum();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![
            modal_title(
                t!("按账号筛选", "Which accounts"),
                theme::accent(),
                esc_hint(if ranging { t!("确定", "keep") } else { t!("关闭", "close") }),
                w,
            ),
            Line::raw(""),
        ];
        let rows = std::iter::once(("0".to_string(), None))
            .chain(accts.iter().enumerate().map(|(i, a)| (if i < 9 { (i + 1).to_string() } else { " ".into() }, Some(a))));
        for (i, (key, a)) in rows.enumerate() {
            let on = match a {
                None => app.bill_only.is_empty(),
                Some(a) => app.bill_only.contains(&a.name),
            };
            let key_st = if on { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let ticked = a.is_some_and(|a| marked.contains(&a.name));
            let green = Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD);
            let tick = if ticked { Span::styled("✓ ", green) } else { Span::raw("  ") };
            let (name, st, cost) = match a {
                None => (t!("全部", "all").to_string(), Style::new(), total),
                Some(a) => (a.name.clone(), agent_style(a.agent), a.cost),
            };
            let money = crate::bill::money(cost);
            let name_w = w.saturating_sub(1 + 2 + 2 + 2 + money.width());
            let mut line = Line::from(vec![
                Span::styled(key, key_st),
                Span::raw("  "),
                tick,
                Span::styled(clip(&name, name_w), st.add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(money, dim()),
            ]);
            // The highlighted one on light purple, ticked ones on light green (like v in the main list)
            let bg = if i == sel { Some(theme::pal().sel) } else { ticked.then(|| theme::pal().pick) };
            if let Some(bg) = bg {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(bg));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        let keys: &[(&str, &str)] = if ranging && pick.is_some_and(|p| p.unpick) {
            &[("↑↓", t!("取消选中", "range unselect")), ("⏎", t!("查看已选", "show selected"))]
        } else if ranging {
            &[("↑↓", t!("连选", "range")), ("⏎", t!("查看已选", "show selected"))]
        } else {
            &[
                (t!("⏎ 数字", "⏎ digit"), t!("仅显示此项", "only it")),
                ("v", t!("多选", "multi")),
                (t!("空格", "Space"), t!("选中", "tick")),
                ("0", t!("全部", "all")),
            ]
        };
        out.push(keys_line(keys, vec![], w));
        out
    });
}

/// How long the startup quiet period is. by_hand = opened by your own keypress (in a tmux popup, ^L restart): no Moshi is connecting then,
/// the quiet period would only swallow the ⏎ pressed right after (keys pressed before the popup is drawn are read right after the first
/// frame; any quiet period, however short, swallows them)
fn startup_quiet(by_hand: bool) -> Duration {
    if by_hand { Duration::ZERO } else { STARTUP_QUIET }
}

/// Whether to drop this batch of keys (why, see the run loop). quiet = still in the quiet period of startup / just leaving a session. Keys
/// that only move the cursor are never dropped: commands Moshi types and pastes have no arrow keys, while swiping or holding an arrow on a
/// phone sends them one after another — the few landing in the quiet period get dropped, those arriving together extend it, and the whole
/// run is lost
fn drops(keys: &[KeyEvent], quiet: bool, typing: bool) -> bool {
    let nav = |k: &KeyEvent| {
        matches!(
            k.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
        )
    };
    !keys.is_empty() && !keys.iter().all(nav) && (quiet || (keys.len() >= 3 && !typing))
}

/// Keys dropped as automatic input are appended to ~/.cache/aca/ignored-input.log (last 200 lines only, mode 600). With ACA_TMUX_SOCKET set
/// it writes ignored-input.<it>.log, so tests don't mix into the real log
fn log_ignored(keys: &[KeyEvent]) {
    let text: String = keys
        .iter()
        .map(|k| match k.code {
            KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => format!("^{}", c.to_ascii_uppercase()),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "⏎".into(),
            KeyCode::Esc => "⎋".into(),
            KeyCode::Tab => "⇥".into(),
            other => format!("<{other:?}>"),
        })
        .collect();
    let Some(home) = std::env::var_os("HOME") else { return };
    let file = match std::env::var("ACA_TMUX_SOCKET") {
        Ok(s) if !s.is_empty() => format!("ignored-input.{s}.log"),
        _ => "ignored-input.log".into(),
    };
    let path = std::path::Path::new(&home).join(".cache/aca").join(file);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().collect();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let line = format!("{stamp} {text}");
    lines.push(&line);
    let keep = &lines[lines.len().saturating_sub(200)..];
    let _ = std::fs::write(&path, keep.join("\n") + "\n");
    // Anything pasted into the list by mistake gets logged here too: readable only by you
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
}

/// The terminal supports kitty's extended keyboard protocol (desktop kitty): only with it on can Shift+Enter be told from Enter. Phones go
/// through mosh, which doesn't forward this protocol; there only `E` works
static KBD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn kbd(on: bool) {
    if KBD.load(Ordering::Relaxed) {
        let _ = if on {
            execute!(stdout(), PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))
        } else {
            execute!(stdout(), PopKeyboardEnhancementFlags)
        };
    }
}

/// What the settings page lists under Problems: the theme (config.toml's [theme], files in themes/) and a few other items
fn config_bad() -> Vec<String> {
    [theme::problems(), crate::config::problems()].concat()
}

/// Focus events are needed only when managing the input method (desktop);
/// turned off before handing over the terminal, since programs inside don't know them
fn focus_events(on: bool) {
    if crate::ime::active() {
        let _ = if on { execute!(stdout(), EnableFocusChange) } else { execute!(stdout(), DisableFocusChange) };
    }
}

/// When opened directly in kitty: store the highlighted item in this window's variables (OSC 1337 SetUserVar): `aca_dir` its directory,
/// `aca_sid` which session it is. kitty's floating window (kfloat, run by cmd+enter) opens there and is tied to that session. Empty =
/// delete the variable (on exit)
fn tell_var(name: &str, value: &str) {
    use std::io::Write;
    let v = if value.is_empty() { String::new() } else { format!("={}", crate::clip::base64(value.as_bytes())) };
    let mut out = stdout().lock();
    let _ = write!(out, "\x1b]1337;SetUserVar={name}{v}\x07").and_then(|_| out.flush());
}

/// Whether a note can be filled into its input box (E takes it in, s sends it): only Claude / Codex (other programs like vim would take
/// pasted text as commands); not while it waits for you to choose (!) — the text would be taken as a choice
fn cant_fill(s: &Session) -> Option<&'static str> {
    match s.state {
        State::Ask => Some(t!("正在等待回应", "it's waiting for your choice")),
        State::Error | State::Exited | State::Stopped | State::Crashed | State::Dead => Some(t!("未运行", "it isn't running")),
        _ if !matches!(s.agent, Agent::Claude | Agent::Codex | Agent::Pi | Agent::Opencode) => {
            Some(t!("不是内置 agent", "not a built-in agent"))
        }
        _ => None,
    }
}

/// l / e in the diff view, y in the list: hand over the terminal to run lazygit / an editor / yazi, return what went wrong when back
fn run_external(x: &External) -> Option<String> {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let (name, st) = match x {
        External::Lazygit(dir) => ("lazygit".to_string(), std::process::Command::new("lazygit").arg("-p").arg(dir).status()),
        External::Edit { path, line } => {
            let ed = notes::editor();
            let name = notes::editor_name();
            // Each editor writes "open at line N" differently:
            // the VS Code family uses -g file:line, helix / zed / sublime use file:line, the rest +line file
            let cmd = match name.as_str() {
                "code" | "code-insiders" | "cursor" | "windsurf" => format!("{ed} -g {}", q(&format!("{path}:{line}"))),
                "hx" | "helix" | "zed" | "subl" => format!("{ed} {}", q(&format!("{path}:{line}"))),
                _ => format!("{ed} +{line} {}", q(path)),
            };
            (name, std::process::Command::new("sh").arg("-c").arg(cmd).status())
        }
        External::Config { path, template } => {
            let p = std::path::Path::new(path);
            if !p.exists() {
                let made = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(p, template));
                if let Err(e) = made {
                    return Some(tr!("无法写入 {}:{e}", "can't write {}: {e}", create::tilde(path)));
                }
            }
            let cmd = format!("{} {}", notes::editor(), q(path));
            (notes::editor_name(), std::process::Command::new("sh").arg("-c").arg(cmd).status())
        }
        External::View { path, line, temp } => {
            let ed = notes::editor();
            let name = notes::editor_name();
            // Read-only: vim family -M (can't edit or save), no swap file, no prompt if open elsewhere; nano -v; other editors have no
            // read-only mode, open them as usual
            let gui = matches!(name.as_str(), "code" | "code-insiders" | "cursor" | "windsurf" | "zed" | "subl");
            let cmd = match name.as_str() {
                "vi" | "vim" | "nvim" | "view" | "gvim" | "mvim" => format!("{ed} -M -n --cmd 'set shortmess+=A' +{line} {}", q(path)),
                "nano" => format!("{ed} -v +{line} {}", q(path)),
                "code" | "code-insiders" | "cursor" | "windsurf" => format!("{ed} -g {}", q(&format!("{path}:{line}"))),
                "hx" | "helix" | "zed" | "subl" => format!("{ed} {}", q(&format!("{path}:{line}"))),
                _ => format!("{ed} +{line} {}", q(path)),
            };
            let st = std::process::Command::new("sh").arg("-c").arg(cmd).status();
            // Editors that open a window return at once; the file stays (files older than a day are cleared next time)
            if *temp && !gui {
                let _ = std::fs::remove_file(path);
            }
            (name, st)
        }
        External::Yazi(sp) => {
            let dir = crate::workdir::find(&sp.w).dir;
            if let Some(e) = yazi_blocked(&dir) {
                return Some(e);
            }
            ("yazi".to_string(), std::process::Command::new("yazi").arg(&dir).status())
        }
        External::Plugin(req) => {
            let (ok, out) = crate::caps::run(req);
            return (!ok).then_some(out);
        }
    };
    match st {
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(not_found(&name)),
        Err(e) => Some(tr!("无法启动 {name}:{e}", "can't start {name}: {e}")),
    }
}

/// How to install a missing program: brew on macOS; elsewhere (Linux mostly has no brew) use your own package manager
fn not_found(name: &str) -> String {
    if cfg!(target_os = "macos") {
        tr!("未找到 {name}(brew install {name})", "{name} not found (brew install {name})")
    } else {
        tr!("未找到 {name}:请用包管理器安装", "{name} not found: install it with your package manager")
    }
}

/// Write the text for e to a temp file: in a directory only you can enter, named like "turn 3-Bash" (shown at the top of the editor); also
/// clear leftovers older than a day
fn write_view(name: &str, text: &str, ext: &str) -> std::io::Result<String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = std::env::temp_dir().join("aca-trace-view");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let old = |e: &std::fs::DirEntry| {
            e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > Duration::from_secs(86_400)))
        };
        rd.flatten().filter(old).for_each(|e| {
            let _ = std::fs::remove_file(e.path());
        });
    }
    let base: String = name.chars().filter(|c| !c.is_whitespace()).map(|c| if matches!(c, '/' | '·' | ':') { '-' } else { c }).collect();
    let path = dir.join(format!("{}.{ext}", if base.is_empty() { t!("记录", "record") } else { &base }));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Wait for a key, up to max. With a terminal open, its output also wakes it to redraw
fn wait_input(max: Duration, term: bool) -> std::io::Result<bool> {
    if !term {
        return event::poll(max);
    }
    let end = Instant::now() + max;
    loop {
        if event::poll(Duration::from_millis(8))? {
            return Ok(true);
        }
        if crate::term::DIRTY.swap(false, Ordering::Relaxed) || Instant::now() >= end {
            return Ok(false);
        }
    }
}

/// Why yazi can't open: directory gone, not installed
fn yazi_blocked(dir: &str) -> Option<String> {
    if !std::path::Path::new(dir).is_dir() {
        return Some(if dir.is_empty() {
            t!("无法确定会话目录", "no idea which directory it's in").into()
        } else {
            tr!("目录已不存在:{dir}", "the directory is gone: {dir}")
        });
    }
    let found = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("yazi").is_file()));
    (!found).then(|| not_found("yazi"))
}

/// Give the terminal back on error (exitlog's cleanup calls this on the main thread): like a normal exit, turn off keyboard enhancement,
/// focus events and mouse, leave full screen and raw mode. Don't touch the input method: its lock may be held by the code that failed
/// (ime::Guard switches back on exit)
fn release_terminal() -> std::io::Result<()> {
    kbd(false);
    let _ = execute!(stdout(), DisableFocusChange, DisableMouseCapture);
    ratatui::try_restore()
}

/// Hand over the terminal (go into a session / fzf / editor). Turn off the extended keyboard protocol first, else programs inside get key
/// codes they don't know
fn suspend() -> std::io::Result<()> {
    kbd(false);
    focus_events(false);
    crate::ime::english(false);
    execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()
}

/// Take the terminal back and draw a frame right away (with the data at hand: what needs rereading goes to the background and is swapped in
/// when read). First send an "end sync": tmux wraps each frame in sync begin / end; if a detach lands mid-frame (the session is flooding
/// output), the unsent second half of the frame and its "end" are thrown away, kitty stays "in sync" and doesn't refresh, aca's drawn frame
/// stays invisible, stuck up to 2 seconds (measured: 7 of 100 detaches in a flooding session). A terminal not stuck in sync does nothing
/// with it. Entering full screen, clearing and drawing this frame are wrapped in one sync too: the terminal swaps it in when done, with no
/// blank screen in between (it used to clear first and then read the list, blank for tens of milliseconds, seen as a flash). Clear with
/// resize, not clear: to put the cursor back, clear first asks the terminal where the cursor is and waits for the answer; on a phone over
/// ssh, one stall (no answer for 2 seconds) is an error and aca exits with it. resize clears too and the next frame redraws the whole
/// screen, but it doesn't ask
fn resume(t: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EndSynchronizedUpdate, BeginSynchronizedUpdate, EnterAlternateScreen, EnableMouseCapture)?;
    kbd(true);
    focus_events(true);
    t.hide_cursor()?;
    let area = t.size()?.into();
    t.resize(area)?;
    t.draw(|f| draw(f, app))?;
    execute!(stdout(), EndSynchronizedUpdate)
}

/// Whether it's Ctrl+\. The terminal sends 0x1C, which crossterm reads as ^4; in terminals with keyboard enhancement it's ^\ itself
fn closes_popup(k: KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('\\' | '4'))
}

/// Replace this process with itself (same path, i.e. the newly installed one) and keep going: exec, same process, same terminal. Only pass
/// --client (the popup needs it) and --restarted (where to open); not the keywords from startup — they'd go straight into that session. The
/// view (groups, scope, layout, highlighted session) is saved anyway, and the new one restores it
fn relaunch(state: &str) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let Some(exe) = aca_core::agent::self_path() else {
        return anyhow::anyhow!("{}", t!("找不到 aca 程序,未重启", "can't find the aca program; not restarted"));
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("ui");
    if let Some(c) = crate::data::client() {
        cmd.args(["--client", c]);
    }
    if crate::follow::asked() {
        cmd.arg("--follow");
    }
    cmd.args(["--restarted", state]);
    anyhow::anyhow!("{}", tr!("重启界面失败:{}", "restarting the interface failed: {}", cmd.exec()))
}

/// When this aca program was installed (the file's mtime): 11:28 for today, 9/29 11:28 for other days
fn installed_at() -> Option<String> {
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
    let t = secs(std::fs::metadata(aca_core::agent::self_path()?).ok()?.modified().ok()?)?;
    let now = secs(std::time::SystemTime::now())?;
    Some(local_when(t.max(0) as u64, now.max(0) as u64))
}

/// Stop idle sessions on the timer from config.toml's `[idle] stop_after` (see `session::stop_idle`), sending the stopped ones back to the
/// interface. The config is read fresh each time, so changes need no reopen; missing or invalid (listed in the settings page) = no
/// stopping. Ones marked unread with u are kept: core's "seen" doesn't know about that
fn idle_watch(tx: mpsc::Sender<IdleTick>) {
    let mut wait = IDLE_FIRST;
    loop {
        thread::sleep(wait);
        wait = IDLE_EVERY;
        let cfg = aca_core::config::load();
        let Ok(store) = aca_core::store::Store::open() else { continue };
        let stopped = match cfg.idle.stop_after() {
            Ok(Some(after)) => {
                let reads = Reads::open(Reads::default_path());
                aca_core::session::stop_idle(&store, &cfg, after, &|id| reads.unread(id)).unwrap_or_default()
            }
            _ => vec![],
        };
        let remove = cfg.idle.remove_after().ok().flatten().map(|after| (after, archived_sessions(&store)));
        if tx.send(IdleTick { stopped, remove }).is_err() {
            return;
        }
    }
}

/// What `idle_watch` hands back each round: which ones were auto-stopped; if `[idle] remove_after` is set, it and the archived sessions
/// (see `App::sweep`)
struct IdleTick {
    stopped: Vec<aca_core::session::Idled>,
    remove: Option<(u64, Vec<(Target, u64)>)>,
}

/// Archived registered sessions (not listed) and their last activity: when archived, the hook's last report, the latest in the registry
fn archived_sessions(store: &aca_core::store::Store) -> Vec<(Target, u64)> {
    let all = store.sessions(true).unwrap_or_default();
    all.into_iter()
        .filter(|s| s.archived > 0)
        .map(|s| {
            let r = aca_core::runtime::load(&s.id);
            let title = aca_core::status::title(&s, &r);
            let at = s.archived.max(r.ts).max(s.created);
            (Target { name: s.id.clone(), title, profile: crate::data::REGISTERED.into(), id: s.id }, at)
        })
        .collect()
}

/// The bottom bar's line after auto-stopping some idle sessions: which ones (only the first three if many), how to start them again
fn idle_note(v: &[aca_core::session::Idled]) -> String {
    let mut names: Vec<String> = v.iter().take(3).map(|s| tr!("「{}」", "“{}”", clip(&s.title, 16).trim_end())).collect();
    if v.len() > 3 {
        names.push(tr!("等 {} 个", "and {} more", v.len() - 3));
    }
    tr!(
        "{} 自动停止了 {} 个:{} · ⏎ 重新启动",
        "{} auto-stopped {}: {} · ⏎ resumes",
        aca_core::status::IDLE_STOPPED,
        v.len(),
        names.join(t!("", ", "))
    )
}

pub fn run(filter: String, restarted: Option<String>) -> anyhow::Result<()> {
    // Draw the first screen once read (~20ms), to avoid flashing an empty list first; read in the background while switching the input
    // method here (~40ms the first time)
    let first = thread::spawn(crate::data::collect_first);
    // Switch the input method first: keys pressed as soon as the popup appears don't get eaten by pinyin.
    // On the main thread, like later switches
    crate::ime::start();
    let _ime = crate::ime::Guard;
    // Opened with keywords: show only the matches, don't restore the group it was last on
    let view_file = crate::state::state_file("view");
    let restore = filter.is_empty();
    let sessions = first.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
    let mut app = App::new(filter, sessions)
        .with_pins(Pins::open(Pins::default_path()))
        .with_notes(Notes::open(Notes::default_path()))
        .with_marks(Reads::open(Reads::default_path()), Spaces::open(Spaces::default_path()))
        .with_trash(Trash::open(Trash::default_path()))
        .with_group_hints(crate::data::group_hints)
        .with_adopt(adopt_plan)
        .with_new_group_dir(crate::config::new_group_dir)
        .with_tools(|| Ok(crate::tools::load()))
        .with_accounts(|adding| Box::new(crate::acctpage::Page::real(adding)))
        .with_settings(crate::config::ime_auto, config_bad, crate::config::set, crate::accounts::all)
        .with_mcp(crate::mcp::load)
        .with_groups(crate::groups::load)
        .with_handoff(crate::handoff::source)
        .with_rehome(crate::rehome::quotas, crate::accounts::login, true);
    // The remote tasks server is set in the settings page; P is the plugins page (mounts and remote tasks are plugins now, in the : menu)
    app = app.with_plugins(|| Box::new(crate::plugins::Page::new(crate::plugins::load)));
    // Whether Claude starts with aca's mod (quota doesn't rely on the status bar)
    app = app.with_setting(Box::new(crate::limits::ModRow::real()));
    app = app.with_setting(Box::new(crate::trust::TrustRow::real()));
    // Sessions a killed / crashed interface deleted but didn't get to really delete
    thread::spawn(crate::trash::purge);
    // Upgraded without running install.sh (brew and the like): rewrite aca's tmux settings for this version
    thread::spawn(crate::tmuxconf::refresh);
    if let (true, Some(p)) = (restore, &view_file) {
        app = app.restore(View::load(p));
    }
    if let Some((session, pane)) = crate::data::client_at() {
        app.highlight_here(&session, &pane);
        if crate::follow::asked() {
            // Closed while on another session, so `switch_to` didn't run: set up the Ctrl+Q binding here once
            aca_core::tmux::bind_detach();
            app.follow_from(&session, &pane);
        }
    }
    // config.toml is broken: treat accounts and tools as unset; the settings page's Problems says where
    app.sync_failed = aca_core::config::problem().into_iter().collect();
    if let Some(state) = &restarted {
        app.resume(state, installed_at());
    }
    // The last interface went away with its terminal / exited on error: say it once (see exitlog.rs)
    if let Some(m) = crate::exitlog::take_notice() {
        app.flash_for(&m, NOTICE);
    }
    let mut saved = app.view_state();
    let mut lang = i18n::lang();

    // The background rereads every 2 seconds; paused while attached
    let (tx, rx) = mpsc::channel::<Vec<Session>>();
    // Whether plugins have errors: reread along with the list (the ⚠ cell in the top bar)
    let (alert_tx, alert_rx) = mpsc::channel::<Vec<aca_core::plugin::health::Problem>>();
    let (req_tx, req_rx) = mpsc::channel::<()>();
    let paused = Arc::new(AtomicBool::new(false));
    {
        let paused = paused.clone();
        thread::spawn(move || {
            // Reread when it's time, when someone asks, or when a hook just reported (the event stream file changed); stop once the
            // interface is gone. However dense the events, refreshes they trigger stay EVENT_GAP apart
            let stamp = || std::fs::metadata(aca_core::paths::events_log()).and_then(|m| m.modified()).ok();
            let (mut seen, mut last) = (stamp(), Instant::now());
            loop {
                match req_rx.recv_timeout(EVENT_TICK) {
                    Ok(()) => {}
                    Err(RecvTimeoutError::Timeout) => {
                        let due = last.elapsed() >= REFRESH || (stamp() != seen && last.elapsed() >= EVENT_GAP);
                        if !due {
                            continue;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                (seen, last) = (stamp(), Instant::now());
                if !paused.load(Ordering::Relaxed)
                    && (tx.send(collect()).is_err() || alert_tx.send(aca_core::plugin::health::problems()).is_err())
                {
                    break;
                }
            }
        });
    }

    // Stop idle sessions when their time is up: its own thread, not paused with the refresh —
    // it keeps watch whichever session you're attached to
    let (idle_tx, idle_rx) = mpsc::channel::<IdleTick>();
    thread::spawn(move || idle_watch(idle_tx));

    // Restart / rename / move group / delete run in the background and come back when done along with the reread sessions (rereading takes
    // tens of milliseconds too, kept off the interface)
    let (job_tx, job_rx) = mpsc::channel::<(Job, Outcome, Vec<Session>)>();
    // Background refreshes held back while sliding
    let mut pending: Option<Vec<Session>> = None;
    // Diff view: git diff finished in the background
    let (diff_tx, diff_rx) = mpsc::channel::<(String, Mode, diff::Diff)>();
    let (plug_tx, plug_rx) = mpsc::channel::<(crate::caps::Req, bool, String)>();
    let mut diff_busy = false;
    let (glance_tx, glance_rx) = mpsc::channel::<(String, Glance)>();
    let mut glance_busy = false;
    // Session record: the first full read (the largest over a hundred MB, a fraction of a second) is in the background; later additions are
    // read in place on refresh
    let (trace_tx, trace_rx) = mpsc::channel::<(String, Result<crate::trace::Trace, String>)>();
    // Billing panel: the child process aca ui --cost-json does the reading (it needs a hundred-odd MB of memory while reading, returned
    // when the child exits); this thread waits for its result
    let (bill_tx, bill_rx) = mpsc::channel::<Result<crate::usage::Report, String>>();
    let (bill_req_tx, bill_req_rx) = mpsc::channel::<Vec<String>>();
    thread::spawn(move || {
        while let Ok(only) = bill_req_rx.recv() {
            if bill_tx.send(read_bill(&only)).is_err() {
                break;
            }
        }
    });

    let mut quiet_until = Instant::now() + startup_quiet(crate::data::in_popup() || restarted.is_some());
    // ratatui::init installs its own panic cleanup; take out the original one (std's default) first, and after init swap in exitlog's
    let std_hook = std::panic::take_hook();
    let mut terminal = ratatui::init();
    crate::exitlog::install(std_hook, release_terminal);
    execute!(stdout(), EnableMouseCapture)?;
    focus_events(true);
    // In a tmux pane / opened directly in kitty: switching to another pane or app gives the input method back (popups get no focus events)
    let mut focused = true;
    // Ask only in terminals known to support it: asking waits for the terminal's answer, and with a phone behind mosh that never answers,
    // every open would wait 2 seconds for nothing
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let likely = !env("KITTY_WINDOW_ID").is_empty()
        || ["ghostty", "WezTerm"].contains(&env("TERM_PROGRAM").as_str())
        || ["kitty", "ghostty", "foot"].iter().any(|t| env("TERM").contains(t));
    let ok = likely && matches!(ratatui::crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    KBD.store(ok, Ordering::Relaxed);
    kbd(true);
    // Tell kitty the highlighted item's directory and session (see `tell_var`); inside tmux it wouldn't reach kitty. told = what was told
    // last, rewrite only when it changes
    let tell = !env("KITTY_WINDOW_ID").is_empty() && env("TMUX").is_empty();
    let mut told = (String::new(), String::new());
    let res = (|| -> anyhow::Result<()> {
        loop {
            // Terminal gone (window closed, ssh dropped): don't wait for a read / write error, clean up and exit (see exitlog.rs)
            if crate::exitlog::gone() {
                return Ok(());
            }
            // A background thread failed: the interface is still up, say so in the bottom bar
            if let Some(m) = crate::exitlog::take_live() {
                app.flash_for(&m, NOTICE);
            }
            let mut refreshed = false;
            // Just moved: hold it and swap in once you stop: a full redraw in the middle of a slide is a stutter on a phone
            while let Ok(v) = rx.try_recv() {
                pending = Some(v);
            }
            if let Some(v) = pending.take_if(|_| !app.just_moved()) {
                app.set_sessions(v);
                refreshed = true;
            }
            while let Ok(a) = alert_rx.try_recv() {
                app.plugin_alerts = a;
            }
            while let Ok((job, out, fresh)) = job_rx.try_recv() {
                app.job_done(&job, out, fresh);
                refreshed = true;
            }
            while let Ok(t) = idle_rx.try_recv() {
                if !t.stopped.is_empty() {
                    app.flash_for(&idle_note(&t.stopped), NOTICE);
                    let _ = req_tx.send(());
                }
                if let Some((after, archived)) = t.remove {
                    app.sweep(after, aca_core::paths::now(), archived);
                }
            }
            // Full-screen reader: just opened / session changed / every timed refresh, refetch the history
            // (even 3000 lines take only a few ms)
            if let Some(target) = app.reader_target().filter(|_| std::mem::take(&mut app.reader_load) || refreshed) {
                app.set_reader_lines(crate::data::capture_history(&target));
            }
            // Plugin capabilities / views / panes: one thread each, sent back when done
            for req in std::mem::take(&mut app.plugin_q) {
                let tx = plug_tx.clone();
                thread::spawn(move || {
                    let (ok, out) = crate::caps::run(&req);
                    let _ = tx.send((req, ok, out));
                });
            }
            while let Ok((req, ok, out)) = plug_rx.try_recv() {
                app.plugin_done(req, ok, out);
            }
            // Diff view: git runs in the background (a big repo takes a while), only one at a time
            while let Ok((name, mode, d)) = diff_rx.try_recv() {
                diff_busy = false;
                app.set_diff(&name, mode, d);
            }
            if !diff_busy && let Some((name, w, mode)) = app.diff_request(refreshed) {
                diff_busy = true;
                let tx = diff_tx.clone();
                thread::spawn(move || {
                    let _ = tx.send((name, mode, diff::load(&w, mode)));
                });
            }
            while let Ok((name, r)) = trace_rx.try_recv() {
                app.set_trace(&name, r);
            }
            if let Some(s) = app.trace_request() {
                let tx = trace_tx.clone();
                thread::spawn(move || {
                    let r = crate::trace::load(&s);
                    let _ = tx.send((s.name.clone(), r));
                });
            }
            if refreshed {
                app.trace_tail();
            }
            // Diff stats at the top of the preview: for the highlighted one, also read in the background
            while let Ok((name, g)) = glance_rx.try_recv() {
                glance_busy = false;
                app.set_glance(name, g);
            }
            if !glance_busy && let Some((name, w)) = app.glance_request(refreshed) {
                glance_busy = true;
                let tx = glance_tx.clone();
                // A group's preview also lists what's in the directory
                let group = is_group_key(&name);
                thread::spawn(move || {
                    let g = if group { diff::glance_dir(&w) } else { diff::glance(&w) };
                    let _ = tx.send((name, g));
                });
            }
            while let Ok(r) = bill_rx.try_recv() {
                app.set_bill(r);
            }
            if let Some(only) = app.bill_request() {
                let _ = bill_req_tx.send(only);
            }
            crate::ime::english(focused && !app.typing());
            // Colors changed in config.toml get swapped in; so does the input method switch
            // (changes in the settings page take effect at once)
            let cfg_changed = theme::refresh();
            if cfg_changed || std::mem::take(&mut app.ime_changed) {
                let was = crate::ime::active();
                crate::ime::reconfigure();
                match (was, crate::ime::active()) {
                    (false, true) => execute!(stdout(), EnableFocusChange)?,
                    (true, false) => execute!(stdout(), DisableFocusChange)?,
                    _ => {}
                }
            }
            if cfg_changed {
                i18n::reload();
                app.reload_settings();
            }
            if i18n::lang() != lang {
                lang = i18n::lang();
                app.lang_changed();
            }
            terminal.draw(|f| draw(f, &mut app))?;
            if tell {
                let here = (app.here_dir(), app.here_sid());
                if here.0 != told.0 {
                    tell_var("aca_dir", &here.0);
                }
                if here.1 != told.1 {
                    tell_var("aca_sid", &here.1);
                }
                told = here;
            }
            // While sliding, wake up at the time the preview should switch, so it switches on time once you stop
            let mut wait =
                app.settle_left().map_or(Duration::from_millis(150), |d| d.min(Duration::from_millis(150)) + Duration::from_millis(5));
            // Billing is loading: it's back in tens of milliseconds, don't wait the full 150 ms to swap it in
            if app.bill_loading {
                wait = wait.min(Duration::from_millis(15));
            }
            if wait_input(wait, app.term_on.is_some())? {
                // Read all the events that have arrived at once. 3 or more keys arriving together = not typed by hand: after connecting,
                // Moshi types commands into the shell for you (like cd to the latest directory), but aca is what's taking input then — the
                // "/" in it enters filter mode, the path becomes the filter, Enter confirms it, and the list is filtered to empty, looking
                // like a stuck fzf. Pastes likewise. Drop the whole batch.
                let mut batch = vec![event::read()?];
                while batch.len() < 4096 && event::poll(Duration::ZERO)? {
                    batch.push(event::read()?);
                }
                // The sequence kitty sends back when a desktop notification is clicked isn't keys: pick it out first (else the whole
                // sequence would be dropped as typed-for-you, and muted for a while)
                for osc in crate::clicked::take(&mut batch) {
                    if let Some(id) = crate::clicked::session(&osc) {
                        app.open_clicked(id);
                    }
                }
                let keys: Vec<KeyEvent> = batch
                    .iter()
                    .filter_map(|e| match e {
                        Event::Key(k) if k.kind == KeyEventKind::Press => Some(*k),
                        _ => None,
                    })
                    .collect();
                let now = Instant::now();
                let drop = drops(&keys, now < quiet_until, app.typing());
                if drop {
                    // Log only, no hint in the bottom bar
                    log_ignored(&keys);
                    // Only one key dropped = pressed by a person during the quiet period, don't extend:
                    // extending would swallow every key of a fast typist
                    if keys.len() > 1 {
                        quiet_until = quiet_until.max(now + BURST_QUIET);
                    }
                }
                for e in batch {
                    match e {
                        Event::Key(k) if k.kind == KeyEventKind::Press && !drop => app.on_key(k),
                        Event::Mouse(m) => app.on_mouse(m),
                        Event::FocusLost => focused = false,
                        Event::FocusGained => focused = true,
                        _ => {}
                    }
                }
            }
            if std::mem::take(&mut app.refresh) {
                let _ = req_tx.send(());
            }
            if let Some(job) = app.take_job() {
                let tx = job_tx.clone();
                thread::spawn(move || {
                    let out = ops::run(&job);
                    let _ = tx.send((job, out, collect()));
                });
            }
            if let Some(c) = app.clip.take() {
                let (_, what, text) = match c {
                    Clip::Last { id, title, screen, .. } => {
                        let got = (!id.is_empty()).then(|| create::last_response(&id)).flatten();
                        let what =
                            if got.is_some() { t!("最后一条回复", "the last reply") } else { t!("屏幕上的内容", "the screen") };
                        (title, what, got.unwrap_or(screen))
                    }
                    Clip::Info { title, text } => (title, t!("会话信息", "session info"), text),
                    Clip::Text(text) => (String::new(), "", text),
                };
                let m = if text.trim().is_empty() {
                    t!("没有可复制的内容", "nothing to copy").to_string()
                } else {
                    match crate::clip::copy(&text) {
                        Ok(_) if what.is_empty() => tr!("已复制 {text}", "copied {text}"),
                        Ok(_) => tr!("已复制{what}({} 行)", "copied {what} ({} lines)", text.lines().count()),
                        Err(e) => tr!("复制失败:{e}", "copy failed: {e}"),
                    }
                };
                app.flash(&m);
            }
            // Save the view as soon as it changes (when the connection drops aca is killed outright, never reaching exit)
            let now_view = app.view_state();
            if now_view != saved && !app.just_moved() {
                if let Some(p) = &view_file {
                    let _ = now_view.save(p);
                }
                saved = now_view;
            }
            // Following: once the highlight settles, take this client there; Esc takes it back to where it was, then exits
            if let Some(to) = app.follow_step() {
                crate::data::show(&to.session, &to.pane);
            }
            if app.quit {
                app.follow_done();
                return Ok(());
            }
            // fzf before new: Enter in fzf opens it, and new follows right away
            if std::mem::take(&mut app.pick) {
                let size = terminal.size()?;
                if let Some(p) = app.dir_pick(size.width, size.height) {
                    paused.store(true, Ordering::Relaxed);
                    suspend()?;
                    let got = create::pick_dir(&p);
                    app.picked(got);
                    resume(&mut terminal, &mut app)?;
                    paused.store(false, Ordering::Relaxed);
                }
            }
            if let Some(req) = app.create.take() {
                app.flash(t!("正在新建…", "creating…"));
                terminal.draw(|f| draw(f, &mut app))?;
                match create::create(&req) {
                    Ok(id) => {
                        app.adopt(&id);
                        app.attach = Some(id); // go on to the attach below
                    }
                    Err(e) => app.flash(&tr!("新建失败:{e}", "creating failed: {e}")),
                }
                // Keys held up while waiting weren't meant for the new session
                while event::poll(Duration::ZERO)? {
                    let _ = event::read()?;
                }
                let _ = req_tx.send(());
            }
            if let Some(t) = app.start.take() {
                app.flash(&tr!("正在启动「{}」…", "starting “{}”…", clip(&t.title, 16).trim_end()));
                terminal.draw(|f| draw(f, &mut app))?;
                match create::start(&t.id) {
                    Ok(()) => app.attach = Some(t.name),
                    Err(e) => app.flash(&tr!("启动失败:{e}", "start failed: {e}")),
                }
                while event::poll(Duration::ZERO)? {
                    let _ = event::read()?;
                }
                let _ = req_tx.send(());
            }
            if let Some((family, p)) = app.respawn.take() {
                match crate::park::respawn(&p) {
                    Ok(()) => {
                        app.focus = Some(p.id.clone());
                        app.attach = Some(family);
                    }
                    Err(e) => app.flash(&tr!("启动失败:{e}", "start failed: {e}")),
                }
                let _ = req_tx.send(());
            }
            if std::mem::take(&mut app.purge) {
                thread::spawn(crate::trash::purge);
            }
            if let Some(ed) = app.edit_note.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let got = notes::edit_in_editor(&ed.text);
                app.note_editor_done(ed, got);
                resume(&mut terminal, &mut app)?;
                paused.store(false, Ordering::Relaxed);
            }
            // yazi in a tmux popup: it detects tmux and passes through to ask the outer terminal, the popup doesn't forward that, and every
            // exit waits 5 seconds for nothing (no images either) → instead run it in a zoomed split in the session; q quits and closes
            // that pane, leaving you in the session (the popup is for switching over anyway)
            if let Some(External::Yazi(sp)) =
                app.external.take_if(|x| matches!(x, External::Yazi(sp) if !sp.target.is_empty()) && crate::data::in_popup())
            {
                app.split = Some(sp);
            }
            if let Some(x) = app.external.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let err = run_external(&x);
                app.external_done(err);
                resume(&mut terminal, &mut app)?;
                paused.store(false, Ordering::Relaxed);
                let _ = req_tx.send(());
            }
            if let Some(p) = app.pick_group.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let got = ops::pick_group(&p.rows, &p.what(), &p.here);
                app.group_picked(p, got);
                resume(&mut terminal, &mut app)?;
                paused.store(false, Ordering::Relaxed);
            }
            if let Some(r) = app.term_req.take() {
                let dir = crate::workdir::find(&r.w).dir;
                if std::path::Path::new(&dir).is_dir() {
                    // A group's terminal belongs to no session: no account
                    let env = if !r.env.is_empty() || r.family.is_empty() {
                        r.env.clone()
                    } else {
                        let head = app.all.iter().find(|s| s.sub.is_none() && s.name == r.family);
                        crate::data::account_env(head, r.w.pid, r.w.agent)
                    };
                    // Open at half screen first, then resize to the terminal area's exact size when drawing the first frame
                    let (rows, cols) = (app.screen.height / 2, app.screen.width);
                    let spawned = match &r.run {
                        Some(c) => crate::term::Term::spawn_cmd(c, &dir, &env, rows, cols),
                        None => crate::term::Term::spawn(&dir, &env, rows, cols),
                    };
                    match spawned {
                        Ok(mut t) => {
                            t.title = r.title;
                            t.link = r.link;
                            app.terms.insert(r.key.clone(), t);
                            app.term_on = Some(r.key);
                        }
                        Err(e) => app.flash(&tr!("无法打开终端:{e}", "can't open the terminal: {e}")),
                    }
                } else {
                    app.flash(&if dir.is_empty() {
                        t!("无法确定会话目录", "no idea which directory it's in").to_string()
                    } else {
                        tr!("目录已不存在:{dir}", "the directory is gone: {dir}")
                    });
                }
            }
            // Remove ones whose shell exited; if the one being viewed is gone, back to the list
            app.terms.retain(|_, t| !t.exited());
            app.catch_links();
            if app.term_on.as_ref().is_some_and(|k| !app.terms.contains_key(k)) {
                app.term_on = None;
                // The terminal area gone means it was closed: nothing more to say
            }
            if let Some(sp) = app.split.take() {
                let dir = crate::workdir::find(&sp.w).dir;
                let session = app.tmux_of(&sp.name);
                // A session row doesn't know the pane id: split its current pane
                let target = if sp.target.is_empty() { format!("{session}:") } else { sp.target.clone() };
                match sp.yazi.then(|| yazi_blocked(&dir)).flatten() {
                    Some(e) => app.flash(&e),
                    None => match if sp.yazi {
                        crate::data::split(&session, &target, &dir, sp.side, true, &sp.env)
                    } else {
                        crate::data::new_window(&session, &dir, &sp.env)
                    } {
                        Some(pane) => {
                            app.focus = Some(pane);
                            app.attach = Some(sp.name);
                        }
                        None => app.flash(if sp.yazi {
                            t!("无法分屏:会话可能刚结束", "can't split: the session may have just ended")
                        } else {
                            t!("无法打开窗口:会话可能刚结束", "can't open a window: the session may have just ended")
                        }),
                    },
                }
            }
            if let Some(name) = app.attach.take() {
                // name is the session's key; tmux looks it up by session number (the session name is the title, which changes)
                let target = app.tmux_of(&name);
                // Switch to that pane first: the session itself lands on the main pane (where the agent is),
                // a child row on that window / pane
                let focus = app.focus.take();
                if let Some(p) = &focus {
                    crate::data::focus_pane(p);
                }
                // E: paste the note into its input box first (no Enter), so you see it on entering
                if let Some((key, text)) = app.fill.take()
                    && crate::data::paste_into(&focus.unwrap_or_else(|| format!("{target}:")), &text).is_ok()
                {
                    app.note_filled(&key);
                }
                if std::env::var_os("TMUX").is_some() {
                    // Already in tmux (in tmux on desktop, a popup on a phone): switch over directly, no nesting. Switching away exits the
                    // interface; going in counts as seen. Switch first, then hand over the terminal: leave full screen only once the
                    // interface is out of sight, so the shell below isn't seen
                    app.mark_seen(&name);
                    aca_core::tmux::bind_detach();
                    crate::data::switch_to(&target)?;
                    suspend()?;
                    return Ok(());
                }
                // Prepare before handing over the terminal (the interface is still on screen), so tmux comes up showing it
                let entry = crate::data::prepare(&target);
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let left = crate::data::go_in(&entry);
                app.came_back(&name, left.as_deref().unwrap_or_default());
                if left.is_none() {
                    app.flash(t!("无法进入:会话可能刚结束", "can't go in: the session may have just ended"));
                }
                // Draw the first frame back with the list from before going in;
                // the new one is read in the background (tens of ms) and swapped in
                resume(&mut terminal, &mut app)?;
                quiet_until = Instant::now() + BACK_QUIET;
                paused.store(false, Ordering::Relaxed);
                let _ = req_tx.send(());
                if let Some(left) = left {
                    crate::data::remember_left(&entry, &left);
                }
            }
        }
    })();
    kbd(false);
    focus_events(false);
    if !told.0.is_empty() {
        tell_var("aca_dir", "");
    }
    if !told.1.is_empty() {
        tell_var("aca_sid", "");
    }
    crate::ime::english(false);
    let _ = execute!(stdout(), DisableMouseCapture);
    // Terminal gone (window closed, ssh dropped): it can't be given back and there's nowhere to say so: log to exit.log, don't print, don't
    // pass the error on. This used to use ratatui::restore(), which printed to the dead terminal when it couldn't restore, error on error,
    // and the program aborted (see exitlog.rs)
    let restored = ratatui::try_restore();
    let gone = restored.is_err() || crate::exitlog::gone();
    if gone {
        crate::exitlog::term_gone(res.as_ref().err(), restored.as_ref().err());
    }
    // Trash entries that are due: really delete (in a separate process, so exit isn't delayed; deleted even if the terminal is gone)
    if app.trash.due(epoch()) {
        crate::trash::purge_detached();
    }
    if gone {
        return Ok(());
    }
    if app.relaunch && res.is_ok() {
        // A highlight that just moved may not be saved yet (saved once you stop): save it once more before switching
        if let Some(p) = &view_file {
            let _ = app.view_state().save(p);
        }
        return Err(relaunch(&app.relaunch_state()));
    }
    res
}

/// For debugging: render one frame of the given size in memory, feed the keys in keys in order, print it as text (CJK counted as two
/// cells). Draws a frame per key like the interface: some state (which steps of the record can unfold) is only computed while drawing
pub fn snapshot(w: u16, h: u16, keys: &str) -> anyhow::Result<String> {
    // Shows the real pinned ones, but p only changes memory, doesn't write the file
    let mut app = App::new(String::new(), collect())
        .with_pins(Pins::open(Pins::default_path()).detached())
        .with_notes(Notes::open(Notes::default_path()).detached())
        .with_marks(Reads::open(Reads::default_path()).detached(), Spaces::open(Spaces::default_path()).detached())
        .with_trash(Trash::open(Trash::default_path()).detached())
        .with_group_hints(crate::data::group_hints)
        .with_adopt(adopt_plan)
        .with_new_group_dir(crate::config::new_group_dir)
        .with_tools(|| Ok(crate::tools::load()))
        .with_mcp(crate::mcp::load)
        .with_groups(crate::groups::load)
        .with_handoff(crate::handoff::source)
        .with_rehome(crate::rehome::quotas, crate::accounts::login, true);
    let mut t = Terminal::new(TestBackend::new(w, h))?;
    t.draw(|f| draw(f, &mut app))?;
    for c in keys.chars() {
        let code = match c {
            '↓' => KeyCode::Down,
            '↑' => KeyCode::Up,
            '→' => KeyCode::Right,
            '←' => KeyCode::Left,
            '⏎' => KeyCode::Enter,
            '⎋' => KeyCode::Esc,
            '⇥' => KeyCode::Tab,
            '⇤' => KeyCode::BackTab,
            '⌫' => KeyCode::Backspace,
            c => KeyCode::Char(c),
        };
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        // Read in the background in the interface; here it's read on the spot, so the following keys (n / o / →) land on the record
        if let Some(s) = app.trace_request() {
            app.set_trace(&s.name, crate::trace::load(&s));
        }
        t.draw(|f| draw(f, &mut app))?;
    }
    // The frame handed out is drawn on a clean screen: the in-memory screen updates only diffs, so drawing several frames in a row leaves
    // half a wide char from the previous frame
    let mut t = Terminal::new(TestBackend::new(w, h))?;
    t.draw(|f| draw(f, &mut app))?;
    let buf = t.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let cell = &buf[(x, y)];
            out.push_str(&ansi::strip_osc8(cell.symbol()));
            x += cell.cell_width().max(1);
        }
        if (0..w).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED)) {
            out.push_str(" ◀高亮");
        }
        out.push('\n');
    }
    if let Some(a) = app.attach {
        out.push_str(&format!("[attach → {a}]\n"));
    }
    if let Some(sp) = &app.split {
        let how = if !sp.yazi {
            "新窗口"
        } else if sp.side {
            "左右分"
        } else {
            "上下分"
        };
        out.push_str(&format!("[{how} → {}]\n", sp.name));
    }
    if let Some(j) = &app.job {
        out.push_str(&format!("[后台 → {}]\n", j.doing()));
    }
    if let Some(p) = &app.pick_group {
        let groups: Vec<&str> = p.rows.iter().map(|r| r.group.as_str()).collect();
        out.push_str(&format!("[挑分组 → {}]\n", groups.join(" ")));
    }
    // Print only, don't really create
    if let Some(r) = app.create {
        let mut flags = String::new();
        if !r.group.is_empty() {
            flags += &format!(" -g {}", r.group);
        }
        if r.create_dir {
            flags += " (先建目录)";
        }
        out.push_str(&format!("[新建 → aca new {} {}{flags}]\n", r.spec(), create::tilde(&r.dir)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_preview_drops_its_own_terminal_chatter() {
        let lines = ansi::parse_lines(
            &"⏺ Read(src/a.rs)\n  ⎿  Read 12 lines (ctrl+o to expand)\n\n  Thought for 10s (ctrl+o to expand)\n\n⏺ 改好了\n\n\
             ✻ Crunched for 7m 41s\n  ⎿  Referenced file ../.claude/x.py\n  ⎿  Read ../criteria.md (59 lines)\n\n\
             ❯ /compact\n  ⎿  Compacted (ctrl+o to see full summary)\n     PostCompact ['/u/aca' hook claude] completed successfully\n\
               ⎿  Skills restored (sync-weekly)\n\n\
             ⏺ Compacted while idle, before the prompt cache expired · turn off with \"idleCompaction\": false in\n  settings.json"
                .split('\n')
                .collect::<Vec<_>>(),
        );
        let got: Vec<String> = quiet_claude(lines).iter().map(ansi::plain).collect();
        assert_eq!(
            got,
            [
                "⏺ Read(src/a.rs)",
                "  ⎿  Read 12 lines",
                "",
                "  Thought for 10s",
                "",
                "⏺ 改好了",
                "",
                "✻ Crunched for 7m 41s",
                "",
                "❯ /compact",
                "  ⎿  Compacted"
            ]
        );
    }

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

    fn mk(title: &str, group: &str, state: State, idle_for: u64) -> Session {
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

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn titles(app: &App) -> Vec<String> {
        let mut v: Vec<String> = app.visible().0.iter().map(|s| s.title.clone()).collect();
        v.sort();
        v
    }

    /// Order within a group is the order here
    fn sessions() -> Vec<Session> {
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

    fn fixture() -> App {
        App::new(String::new(), sessions())
    }

    /// In list order, unsorted
    fn order(app: &App) -> Vec<String> {
        app.visible().0.iter().map(|s| s.title.clone()).collect()
    }

    fn pin(app: &mut App, title: &str) {
        let p = app.view.iter().position(|&i| app.all[i].title == title).unwrap();
        app.select(Some(p));
        press(app, KeyCode::Char('p'));
    }

    fn ctrl(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    #[test]
    fn ctrl_l_relaunches_aca_unless_something_is_still_running() {
        let mut app = App::new(String::new(), sessions());
        app.busy = Some("正在重启「闲」…".into());
        ctrl(&mut app, 'l');
        assert!(!app.relaunch && !app.quit);
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在重启「闲」…");
        app.busy = None;
        ctrl(&mut app, 'l');
        assert!(app.relaunch && app.quit);
        assert_eq!(app.relaunch_state(), "list");
        // Works in the billing panel too (it's not a period switch); remembers which period was open
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        press(&mut app, KeyCode::Right);
        ctrl(&mut app, 'l');
        assert!(app.relaunch && app.quit);
        assert_eq!(app.relaunch_state(), "bill:2");
        // The new one: reopens the billing panel and says when it was installed
        let mut app = App::new(String::new(), sessions());
        app.resume("bill:2", Some("11:28".into()));
        assert_eq!(app.bill.as_ref().map(|b| b.sel), Some(2));
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启 · 当前 aca 安装于 11:28");
        let mut app = App::new(String::new(), sessions());
        app.resume("list", None);
        assert!(app.bill.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启");
    }

    #[test]
    fn m_on_the_bill_panel_shows_and_hides_the_folded_blocks() {
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        shows(&reader_rows(&mut app, 80, 10), &["m 更多"]);
        press(&mut app, KeyCode::Char('m'));
        assert!(app.bill.as_ref().unwrap().more && app.msg.is_none());
        shows(&reader_rows(&mut app, 80, 10), &["m 收起"]);
        press(&mut app, KeyCode::Char('m'));
        assert!(!app.bill.as_ref().unwrap().more);
    }

    #[test]
    fn a_picks_which_accounts_the_bill_panel_counts() {
        use crate::usage::{Report, Row};
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        assert_eq!(app.bill_request(), Some(vec![]));
        let row = |name: &str, agent, cost| Row { name: name.into(), agent, cost, ..Default::default() };
        let accounts =
            vec![row("Codex big", Agent::Codex, 30.0), row("Claude mid", Agent::Claude, 20.0), row("Claude small", Agent::Claude, 1.0)];
        app.set_bill(Ok(Report { accounts: accounts.clone(), ..Default::default() }));
        app.bill.as_mut().unwrap().scroll = 5;
        // a opens with the highlight on 「全部」 ("all"); ↓ ⏎ = show only the first, back to the top, reread for it right away
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().map(|p| p.sel), Some(0));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.bill.as_ref().unwrap().pick.is_none());
        assert_eq!(app.bill.as_ref().unwrap().scroll, 0);
        assert_eq!(app.bill_request(), Some(vec!["Codex big".to_string()]));
        // Not read back yet: the top still shows the data on display
        assert_eq!(app.bill_request(), None);
        app.set_bill(Ok(Report { only: vec!["Codex big".into()], accounts: accounts.clone(), ..Default::default() }));
        // Reopened, the highlight is on the one shown; Space ticks two (moving down after each), ⏎ shows the ticked ones, sorted by cost
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().map(|p| p.sel), Some(1));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().unwrap().marked, vec!["Claude small".to_string(), "Claude mid".to_string()]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.bill_only, vec!["Claude mid".to_string(), "Claude small".to_string()]);
        // In the floating window 1-4 don't switch periods, they pick accounts directly; 0 = all; Esc changes nothing
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('3'));
        assert_eq!((app.bill_only.clone(), app.bill.as_ref().unwrap().sel), (vec!["Claude small".to_string()], 1));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        assert!(app.bill.is_some() && app.bill_only == vec!["Claude small".to_string()]);
        // The restart carries which ones are shown, and the new one shows just those
        assert_eq!(app.relaunch_state(), "bill:1\tClaude small");
        let mut again = App::new(String::new(), sessions());
        again.resume(&app.relaunch_state(), None);
        assert_eq!(again.bill_only, vec!["Claude small".to_string()]);
        assert_eq!(again.bill_request(), Some(vec!["Claude small".to_string()]));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('0'));
        assert!(app.bill_only.is_empty());
        // Closed and reopened, it still shows only those (here: all)
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('$'));
        assert!(app.bill_only.is_empty());
    }

    #[test]
    fn v_in_the_account_picker_selects_a_run_of_rows() {
        use crate::usage::{Report, Row};
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        let row = |name: &str| Row { name: name.into(), agent: Agent::Claude, cost: 1.0, ..Default::default() };
        app.set_bill(Ok(Report { accounts: vec![row("A"), row("B"), row("C"), row("D")], ..Default::default() }));
        let pick = |app: &App| app.bill.as_ref().unwrap().pick.as_ref().map(|p| (p.sel, p.anchor, p.chosen(&app.bill_accounts())));
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // v on 「全部」 ("all"): starts from the first account; going up doesn't wrap back to 「全部」
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Up);
        assert_eq!(pick(&app), Some((1, Some(1), names(&["A"]))));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(pick(&app), Some((3, Some(1), names(&["A", "B", "C"]))));
        // v again during a range select: the unsettled range is dropped, starting over from the highlighted row; Esc settles it, the
        // floating window stays open
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(pick(&app), Some((3, Some(3), names(&["C"]))));
        press(&mut app, KeyCode::Esc);
        assert_eq!(pick(&app), Some((3, None, names(&["C"]))));
        // v from a ticked row still range-selects; V range-unticks
        keys(&mut app, "kkvjj");
        press(&mut app, KeyCode::Esc);
        assert_eq!(pick(&app), Some((3, None, names(&["A", "B", "C"]))));
        keys(&mut app, "kVj");
        assert_eq!(pick(&app), Some((3, Some(2), names(&["A"]))));
        press(&mut app, KeyCode::Esc);
        // Space during a range select: ticks this one and moves down, still range-selecting
        // (at the last it doesn't wrap back to 「全部」); ⏎ shows the selected
        keys(&mut app, "v ");
        assert_eq!(pick(&app), Some((4, Some(3), names(&["A", "C", "D"]))));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(pick(&app).unwrap().0, 4);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.bill_only, names(&["A", "C", "D"]));
        assert!(app.bill.as_ref().unwrap().pick.is_none());
    }

    fn typ(app: &mut App, text: &str) {
        text.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    #[test]
    fn new_defaults_to_the_selected_sessions_dir_and_group() {
        let mut app = fixture();
        pin(&mut app, "跑"); // select 「跑」 (work group)
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.as_ref().unwrap().text, "/p/work");
        assert!(app.typing());
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.spec().as_str()), ("/p/work", "work", "claude"));
        assert!(r.create_dir); // /p/work doesn't exist
        assert!(app.attach.is_none()); // Enter went to the input box, didn't go into the original session
    }

    #[test]
    fn new_inherits_tool_and_account_from_the_selected_session() {
        let mut v = sessions();
        v[2].tool = "codex".into(); // 「跑」
        v[2].account = "team".into();
        let mut app = App::new(String::new(), v);
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.tool.as_str(), r.account.as_str()), ("codex", "team"));
        pin(&mut app, "闲"); // claude on the default account
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.tool.as_str(), r.account.as_str()), ("claude", ""));
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
    fn down_or_ctrl_f_in_new_asks_for_fzf_and_fills_the_path() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('j')); // select 「跑」
        assert_eq!(app.dir_hints(), ["/p/work", "/p/fun"]); // the selected one first, same directories not repeated
        press(&mut app, KeyCode::Down); // ↓ in the list moves down, doesn't open fzf
        assert!(!app.pick);
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Down);
        assert!(std::mem::take(&mut app.pick));
        ctrl(&mut app, 'f');
        assert!(std::mem::take(&mut app.pick));
        app.picked(Ok(None)); // Esc: unchanged
        assert_eq!(app.new_draft.as_ref().unwrap().text, "/p/work");
        app.picked(Err("没装 fzf".into()));
        assert_eq!(app.new_draft.as_ref().unwrap().hint, "没装 fzf");
        app.picked(Ok(Some(Picked::Edit("~/code/app".into())))); // →: fill it back in and keep typing
        typ(&mut app, "new");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.create.take().unwrap().dir, format!("{}/code/app/new", std::env::var("HOME").unwrap()));
    }

    #[test]
    fn enter_in_fzf_opens_right_away() {
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints { groups: vec![("fun".into(), String::new())] });
        press(&mut app, KeyCode::Char('N'));
        app.picked(Ok(Some(Picked::Open("/p/fun".into()))));
        assert!(app.new_draft.is_none());
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str()), ("/p/fun", "fun"));
    }

    #[test]
    fn fzf_rows_mark_dirs_that_already_have_sessions() {
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new())] });
        press(&mut app, KeyCode::Char('j')); // select 「跑」 (/p/work)
        press(&mut app, KeyCode::Char('N'));
        let cands = [("/p/work", Src::Known), ("/p/gone-quiet", Src::Known), ("/z/often", Src::Zoxide), ("/p/near", Src::Near)];
        let p = app.dir_pick_from(cands.map(|(d, s)| (d.to_string(), s)).to_vec(), 40, 16).unwrap();
        let marks: String = p.rows.iter().map(|r| r.mark).collect();
        assert_eq!(marks, "●◇↗ "); // ● has a session, ◇ registered but no session now
        assert_eq!(p.rows[0].group, ("work".into(), false));
        assert!(p.rows[0].sessions.iter().any(|(_, t)| t == "跑"), "{:?}", p.rows[0].sessions);
        assert_eq!(p.rows[3].group, ("near".into(), true));
        assert_eq!(p.query, "");
    }

    #[test]
    fn ctrl_t_in_new_swaps_the_tool_for_a_shell_and_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('N'));
        shows(&reader_rows(&mut app, 100, 30), &["^T shell"]);
        ctrl(&mut app, 't');
        shows(&reader_rows(&mut app, 100, 30), &["工具   shell", "^T 换回"]);
        assert_eq!(app.new_draft.as_ref().unwrap().finish().unwrap().tool, "shell");
        ctrl(&mut app, 't');
        assert!(!app.new_draft.as_ref().unwrap().is_shell());
        // Phone width: the hint and the tool name squeezed into one row still fit
        let rows = reader_rows(&mut app, 44, 30);
        assert!(rows.iter().any(|r| r.contains("^T shell")), "{rows:#?}");
    }

    #[test]
    fn tab_candidates_list_under_the_input_and_follow_the_pick() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('N'));
        let d = app.new_draft.as_mut().unwrap();
        d.menu = (0..10).map(|i| format!("item{i}/")).collect();
        d.sel = Some(9);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let rows = reader_rows(&mut app, 100, 30);
        // At most 8 listed, scrolling with the one switched to; that one's whole row is filled light purple
        assert!(!rows.iter().any(|r| r.contains("item1/")) && rows.iter().any(|r| r.contains("item2/")), "{rows:#?}");
        let y = rows.iter().position(|r| r.contains("item9/")).unwrap() as u16;
        let x = rows[y as usize].find("item9/").map(|b| rows[y as usize][..b].width() as u16).unwrap();
        assert_eq!(buf[(x, y)].bg, theme::pal().sel);
        assert!(rows.iter().any(|r| r.contains("还有 2 项")), "{rows:#?}");
    }

    #[test]
    fn new_session_group_changes_with_the_path() {
        let mut app = fixture()
            .with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new()), ("fun".into(), String::new())] });
        pin(&mut app, "跑"); // work group
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("work".into(), false));
        app.picked(Ok(Some(Picked::Edit("/p/fun".into())))); // the directory name is the fun group
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("fun".into(), false));
        typ(&mut app, "lab"); // /p/fun/lab: parent is the fun group → nested
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("fun/lab".into(), true));
        app.picked(Ok(Some(Picked::Edit("/p/brand".into()))));
        let rows = reader_rows(&mut app, 60, 20);
        assert!(rows.iter().any(|r| r.contains("brand  新分组")), "{rows:#?}");
        press(&mut app, KeyCode::Enter);
        let req = app.create.take().unwrap();
        // Derived from the path: no ^N-style binding; left to core to claim by directory (recording the default directory)
        assert_eq!((req.group.as_str(), req.bind, req.by_dir), ("brand", false, true));
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

    fn select(app: &mut App, title: &str) {
        let p = app.view.iter().position(|&i| app.all[i].title == title).unwrap();
        app.select(Some(p));
    }

    fn target(title: &str) -> Target {
        Target { name: title.into(), title: title.into(), profile: "default".into(), id: format!("id-{title}") }
    }

    fn hints() -> crate::data::GroupHints {
        let g = |n: &str| (n.to_string(), String::new());
        crate::data::GroupHints { groups: vec![g("work"), g("fun"), g("Zeta"), g("fun")] }
    }

    #[test]
    fn shift_r_restarts_idle_now_and_asks_when_running() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('R'));
        assert_eq!(app.job, Some(Job::Restart { t: target("闲"), force: false }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在重启「闲」…"));
        // No new job until the background one is done
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在重启「闲」…");
        app.job_done(&job, Outcome::Done("已重启「闲」".into()), sessions());
        assert!(app.busy.is_none() && app.confirm.is_none());
        // Running ones ask first; any other key cancels
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none());
        shows(&reader_rows(&mut app, 46, 12), &["重启会话？", "跑", "正在运行,将被中断", "y 重启"]);
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none() && app.attach.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "未重启");
        press(&mut app, KeyCode::Char('R'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.job, Some(Job::Restart { t: target("跑"), force: false }));
    }

    #[test]
    fn f_forks_the_session_and_highlights_the_new_one() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        // Name it first: prefilled with the default, Enter uses it as is (not locked)
        assert!(app.job.is_none() && app.typing());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job, Some(Job::Fork { t: target("闲"), title: None }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在分叉「闲」…"));
        // The fork goes last in the group, the highlight follows it
        let mut fresh = sessions();
        let mut new = mk("闲-fork", "work", State::Running, 0);
        new.slot.1 = 99;
        new.deck_id = "id-新".into();
        fresh.push(new);
        app.job_done(&job, Outcome::Made { msg: "已分出「闲-fork」".into(), id: "id-新".into() }, fresh);
        assert_eq!(app.current().unwrap().title, "闲-fork");
        assert_eq!(app.msg.as_ref().unwrap().0, "已分出「闲-fork」");
        // On failure the highlight stays
        press(&mut app, KeyCode::Char('f'));
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed("分叉失败:还没有对话记录".into()), sessions());
        assert_eq!(app.msg.as_ref().unwrap().0, "分叉失败:还没有对话记录");
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

    #[test]
    fn fork_and_mcp_are_only_for_registered_claude_or_codex_sessions() {
        let mut v = sessions();
        v[3].agent = Agent::None; // 「闲」: a shell session
        v[2].deck_id.clear(); // 「跑」: a plain tmux session
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "该工具不支持分叉");
        press(&mut app, KeyCode::Char('M'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "只有 Claude / Codex 会话有 MCP");
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能分叉");
        press(&mut app, KeyCode::Char('M'));
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能查看 MCP");
    }

    fn fake_source(s: &Session) -> Result<Source, String> {
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
    fn tool(name: &str, tool: &str, account: &str, agent: Agent) -> Tool {
        Tool { name: name.into(), tool: tool.into(), account: account.into(), agent, note: format!("~/.{name}"), problem: None }
    }

    fn handoff_tools() -> Result<Vec<Tool>, String> {
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

    fn rehome_quotas(_: Agent) -> Vec<crate::usage::Quota> {
        let q = |who: &str, five: f64, week: f64| crate::usage::Quota {
            agent: Agent::Claude,
            who: who.into(),
            plan: String::new(),
            wins: vec![
                crate::usage::Win { label: "5h".into(), used: five, resets: None },
                crate::usage::Win { label: "7d".into(), used: week, resets: None },
            ],
            updated: 0,
            note: String::new(),
        };
        vec![q("ding", 98.0, 61.0), q("cw", 12.0, 40.0)]
    }

    /// cw2 isn't logged in, all the others are
    fn rehome_ask(_: Agent, name: &str) -> crate::accounts::Login {
        if name == "cw2" { crate::accounts::Login::Out } else { crate::accounts::Login::In("max".into()) }
    }

    fn rehome_tools() -> Result<Vec<Tool>, String> {
        Ok(vec![
            tool("shell", "shell", "", Agent::None),
            tool("ding", "claude", "ding", Agent::Claude),
            tool("cw", "claude", "cw", Agent::Claude),
            tool("cw2", "claude", "cw2", Agent::Claude),
            tool("cx", "codex", "cx", Agent::Codex),
        ])
    }

    /// All on ding
    fn on_ding() -> Vec<Session> {
        sessions()
            .into_iter()
            .map(|mut s| {
                (s.account, s.who) = ("ding".into(), "ding".into());
                s
            })
            .collect()
    }

    #[test]
    fn x_lists_the_other_accounts_with_their_quota_and_login_and_enter_switches() {
        let mut app = fixture().with_tools(rehome_tools).with_rehome(rehome_quotas, rehome_ask, false);
        app.set_sessions(on_ding());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        // At the top, the current account and quota; below, only the other accounts of the same tool (not itself, no Codex, no
        // shell), whether logged in, quota
        let rows = reader_rows(&mut app, 80, 24);
        shows(
            &rows,
            &[
                "「闲」切换账号继续",
                "当前  Claude·ding   5h 98% · 7d 61%",
                "› 1 cw    ● 已登录      5h 12% · 7d 40%",
                "  2 cw2   ○ 未登录",
                "⏎ 切换",
            ],
        );
        assert!(!rows.iter().any(|r| r.contains(" cx ") || r.contains("将中断")), "{rows:#?}");
        // Can't switch to one that isn't logged in
        press(&mut app, KeyCode::Char('2'));
        assert!(app.job.is_none() && !app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "cw2 未登录 · a 登录");
        // ⏎ switches to the highlighted one, the box closes
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert!(app.overlays.is_empty());
        assert_eq!(app.job, Some(Job::Rehome { t: target("闲"), to: "claude:cw".into(), label: "cw".into() }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在将「闲」切换到 cw…"));
        app.job_done(&job, Outcome::Done("已切换到 cw · 发送「继续」即可接着做".into()), on_ding());
        assert_eq!(
            (app.current().unwrap().title.as_str(), app.msg.as_ref().unwrap().0.as_str()),
            ("闲", "已切换到 cw · 发送「继续」即可接着做")
        );

        // Running: the box says it will be interrupted
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('X'));
        shows(&reader_rows(&mut app, 80, 24), &["⚠ 切换将中断当前任务"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.job.is_none());
    }

    #[test]
    fn x_says_why_it_cannot_switch() {
        // No other accounts: say how to add one
        let mut app =
            fixture().with_tools(|| Ok(vec![tool("ding", "claude", "ding", Agent::Claude)])).with_rehome(rehome_quotas, rehome_ask, false);
        app.set_sessions(on_ding());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        shows(&reader_rows(&mut app, 80, 20), &["没有其他 Claude 账号 · + 添加"]);
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none());
        press(&mut app, KeyCode::Esc);
        // Shell sessions, plain tmux sessions: doesn't open
        let mut v = on_ding();
        v[3].agent = Agent::None; // 「闲」
        v[0].deck_id.clear(); // 「问」
        let mut app = App::new(String::new(), v).with_tools(rehome_tools);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "只有 Claude / Codex 会话可以切换账号");
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('X'));
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能换账号");
    }

    #[test]
    fn a_session_aca_stopped_for_idling_is_drawn_apart_from_one_stopped_by_hand() {
        let mut v = sessions();
        for (title, at) in [("闲", epoch() - 7300), ("错", 0)] {
            let s = v.iter_mut().find(|s| s.title == title).unwrap();
            (s.state, s.idle_stopped, s.idle_for) = (State::Dead, at, 7300);
        }
        let mut app = fixture();
        app.set_sessions(v);
        let by = |t: &str| app.all.iter().find(|s| s.title == t).unwrap().clone();
        // Stopped by aca for being idle: ☾, gray, saying it was auto-stopped and how long ago; stopped by a person: ■ "stopped" as before
        assert_eq!(session_icon(&by("闲")), ("☾", Style::new().fg(theme::pal().muted)));
        assert_eq!(state_word(&by("闲")), "自动停止 · 2 小时前");
        assert_eq!((session_icon(&by("错")).0, state_word(&by("错")).as_str()), ("■", "已停止 · 2 小时前"));
        // Back up again (the moment before the mark is cleared): drawn by its state
        let mut up = by("闲");
        up.state = State::Idle;
        assert_eq!(session_icon(&up).0, "○");
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 100, 30);
        shows(&rows, &["☾", "自动停止 · 2 小时前"]);
    }

    #[test]
    fn a_session_with_its_own_schedule_shows_when_next_and_the_preview_lists_them() {
        use aca_core::schedule::Due;
        let now = epoch();
        let hm = |t: u64| {
            let d = aca_core::schedule::local(t);
            format!("{:02}:{:02}", d.h, d.mi)
        };
        let (soon, later) = (now + 1200, now + 3 * 86400);
        let loop_ = Due { cron: String::new(), recurring: false, prompt: "接着看\n部署".into(), next: Some(soon), inbox: false };
        let daily = Due { cron: "57 8 * * *".into(), recurring: true, prompt: "看一眼".into(), next: Some(later), inbox: false };
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "闲").unwrap().due = vec![loop_.clone(), daily.clone()];
        let mut app = fixture();
        app.set_sessions(v);
        let by = |t: &str| app.all.iter().find(|s| s.title == t).unwrap().clone();
        // After the title: the time of the earliest due one (today's: hour and minute only; other days add month and day), +N
        // when there are several; nothing when none is scheduled
        assert_eq!(due_badge(&by("闲"), now), format!(" ◷ {} +1", hm(soon)));
        assert_eq!(due_badge(&by("跑"), now), "");
        let day = aca_core::schedule::local(later);
        assert_eq!(local_when(later, now), format!("{}/{} {}", day.m, day.d, hm(later)));
        // One row each: kind, cron, time, the prompt (newlines joined into one line); one whose time can't be read says so
        assert_eq!(due_line(&loop_, now), format!("◷ /loop 下一次 {} · 接着看 部署", hm(soon)));
        assert_eq!(due_line(&daily, now), format!("◷ 循环定时 57 8 * * * · 下次 {} · 看一眼", local_when(later, now)));
        let odd = Due { cron: "@daily".into(), recurring: false, prompt: String::new(), next: None, inbox: false };
        assert_eq!(due_line(&odd, now), "◷ 一次性定时 @daily · 无法识别时间");
        // A scheduled message in the inbox (aca msg --in): Codex and pi have them too
        let msg = Due { cron: String::new(), recurring: false, prompt: "回来看看 CI".into(), next: Some(soon), inbox: true };
        assert_eq!(due_line(&msg, now), format!("◷ 定时消息 {} · 回来看看 CI", hm(soon)));
        // The row of a subagent it spawned isn't marked along with it
        let mut parent = by("闲");
        parent.subagents = vec![];
        let a = Subagent {
            id: "a1".into(),
            kind: "Explore".into(),
            desc: "找".into(),
            phase: Phase::Running,
            t0: 0,
            dur: 0,
            tools: 0,
            depth: 0,
            screen: vec![],
            reported: false,
        };
        assert!(agent_row(&parent, 0, &a).due.is_empty());
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 100, 30);
        shows(&rows, &[&format!("闲 ◷ {} +1", hm(soon)), &format!("◷ /loop 下一次 {} · 接着看 部署", hm(soon)), "◷ 循环定时 57 8 * * *"]);
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

    fn shift(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::SHIFT));
    }

    /// What's read back after the write: in-group order as given
    fn reordered(names: &[&str]) -> Vec<Session> {
        sessions()
            .into_iter()
            .map(|mut s| {
                if let Some(p) = names.iter().position(|n| *n == s.title) {
                    s.slot.1 = p as i64;
                }
                s
            })
            .collect()
    }

    #[test]
    fn shift_arrows_swap_within_the_group_right_away_and_write_one_at_a_time() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        select(&mut app, "闲");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["问", "未读", "闲", "跑", "错"]);
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Order { t: target("闲"), by: "id-跑".into(), after: false });
        // Already first in the group: doesn't cross into the group above
        shift(&mut app, KeyCode::Up);
        assert!(app.msg.is_none());
        // Moving again while a write is in progress: the list still moves; of two moves in a row only the last is kept
        shift(&mut app, KeyCode::Down);
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.take_job().is_none());
        shift(&mut app, KeyCode::Down);
        assert!(app.msg.is_none());
        // First write done, the read-back has only the first move: the later move stays applied and is written next
        app.msg = None;
        app.job_done(&job, Outcome::Done(String::new()), reordered(&["闲", "跑", "错"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.msg.is_none());
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Order { t: target("闲"), by: "id-错".into(), after: true });
        // A periodic refresh from before the write finished still reads the old order: doesn't jump back
        app.set_sessions(reordered(&["闲", "跑", "错"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        app.job_done(&job, Outcome::Done(String::new()), reordered(&["跑", "错", "闲"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.take_job().is_none());
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

    /// What's read back after the write: groups in the order given
    fn regrouped(v: Vec<Session>, groups: &[&str]) -> Vec<Session> {
        v.into_iter()
            .map(|mut s| {
                s.slot.0 = groups.iter().position(|g| *g == s.group).unwrap_or(usize::MAX);
                s
            })
            .collect()
    }

    #[test]
    fn shift_arrows_on_a_group_header_move_the_whole_group() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        jump_to_group(&mut app, "work");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        assert_eq!(app.head.as_deref(), Some("work")); // the highlight follows the group
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::GroupOrder { group: "work".into(), by: "fun".into(), after: false });
        shift(&mut app, KeyCode::Up);
        assert!(app.msg.is_none());
        // A periodic refresh from before the write finished still reads the old order: doesn't jump back
        app.set_sessions(sessions());
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        app.job_done(&job, Outcome::Done(String::new()), regrouped(sessions(), &["work", "fun"]));
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        assert!(app.take_job().is_none());
        // Move it back: a folded group (fold row) can move too
        jump_to_group(&mut app, "fun");
        press(&mut app, KeyCode::Enter);
        assert!(app.on_fold_row());
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.take_job().unwrap(), Job::GroupOrder { group: "fun".into(), by: "work".into(), after: false });
        assert!(app.on_fold_row() && app.current().unwrap().group == "fun");
    }

    #[test]
    fn a_group_takes_its_subgroups_along_and_subgroups_stay_under_the_parent() {
        let mut v = sessions();
        v.push(mk("甲", "work/a", State::Idle, 10));
        v.push(mk("乙", "work/b", State::Idle, 10));
        let mut other = mk("壳", "-", State::Idle, 10); // plain tmux session: sinks to the bottom, doesn't move along
        other.deck_id.clear();
        v.push(other);
        let v = regrouped(v, &["fun", "work", "work/a", "work/b"]);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错", "甲", "乙", "壳"]);
        jump_to_group(&mut app, "fun");
        shift(&mut app, KeyCode::Down); // jumps over the whole of work
        assert_eq!(order(&app), ["跑", "闲", "错", "甲", "乙", "问", "未读", "壳"]);
        assert_eq!(app.take_job().unwrap(), Job::GroupOrder { group: "fun".into(), by: "work".into(), after: true });
        shift(&mut app, KeyCode::Down);
        assert!(app.msg.is_none());
        jump_to_group(&mut app, "work/b");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["跑", "闲", "错", "乙", "甲", "问", "未读", "壳"]);
        shift(&mut app, KeyCode::Up); // can't leave work
        assert!(app.msg.is_none());
        jump_to_group(&mut app, "-");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话区域不能移动");
    }

    #[test]
    fn a_failed_group_write_puts_the_groups_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        jump_to_group(&mut app, "work");
        shift(&mut app, KeyCode::Up);
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed("移动分组失败:x".into()), sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        assert_eq!(app.msg.as_ref().unwrap().0, "移动分组失败:x");
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

    fn with_windows() -> App {
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
    fn shift_t_opens_a_new_window_in_its_session_in_its_real_work_dir() {
        let mut app = with_windows();
        // On the window row: opens in the session it belongs to, a new window running a shell (not a split)
        reader_rows(&mut app, 120, 30);
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('T'));
        let sp = app.split.take().unwrap();
        assert_eq!((sp.name.as_str(), sp.yazi), ("闲", false));
        assert_eq!(sp.w.dir, "/p/work"); // the search for the directory it's really working in starts here
        // Same on a phone
        reader_rows(&mut app, 46, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('T'));
        let sp = app.split.take().unwrap();
        assert_eq!((sp.name.as_str(), sp.yazi), ("闲", false));
        // A stopped one can't be split off
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('T'));
        assert!(app.split.is_none() && app.msg.as_ref().unwrap().0.contains("未运行"), "{:?}", app.msg);
    }

    #[test]
    fn t_opens_a_terminal_under_the_list_and_esc_puts_it_away() {
        let mut app = with_windows();
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('t'));
        let r = app.term_req.take().unwrap();
        assert_eq!((r.key.as_str(), r.family.as_str(), r.w.dir.as_str()), (app.current().unwrap().name.as_str(), "闲", "/p/work"));
        // While open, keys go to the terminal: q doesn't quit, ^C doesn't quit, j doesn't move; Esc hides it
        app.term_on = Some(r.key.clone());
        assert!(app.typing()); // paste / fast typing isn't dropped as automated input
        let at = app.current().unwrap().name.clone();
        press(&mut app, KeyCode::Char('q'));
        ctrl(&mut app, 'c');
        press(&mut app, KeyCode::Char('j'));
        assert!(!app.quit && app.current().unwrap().name == at);
        press(&mut app, KeyCode::Esc);
        assert!(app.term_on.is_none());
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
        // A fold row is a group: opens at the group's path (see t_on_a_group_opens_a_terminal_in_the_groups_dir)
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().map(|r| r.key).as_deref(), Some("group:work"));
    }

    #[test]
    fn a_login_link_pops_up_over_the_terminal_and_takes_the_keys_until_it_closes() {
        let mut app = with_windows();
        let file = std::env::temp_dir().join(format!("aca-ui-link-{}.url", std::process::id()));
        std::fs::write(&file, "https://claude.com/cai/oauth/authorize?x=1").unwrap();
        let mut t = crate::term::Term::spawn("/", &[], 5, 20).unwrap();
        t.title = "登录 Claude Code · ding".into();
        t.link = Some(crate::term::Catch { file: file.clone(), screen: false });
        app.terms.insert("account:ding".into(), t);
        app.term_on = Some("account:ding".into());
        app.catch_links();
        assert_eq!(app.overlays.len(), 1);
        // The same link doesn't pop up again
        app.catch_links();
        assert_eq!(app.overlays.len(), 1);
        // Keys go to the popup first: Esc closes it, the terminal stays open
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.term_on.as_deref() == Some("account:ding"));
        assert!(app.msg.as_ref().is_some_and(|(m, _)| m.contains("链接仍在登录终端中")), "{:?}", app.msg);
        // Esc again: hides the terminal
        press(&mut app, KeyCode::Esc);
        assert!(app.term_on.is_none());
        // Terminal closed: the link file is deleted with it
        app.terms.clear();
        assert!(!file.exists());
    }

    #[test]
    fn y_opens_yazi_where_it_really_works() {
        let mut app = with_windows();
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('y'));
        let Some(External::Yazi(sp)) = app.external.take() else { panic!("{:?}", app.msg) };
        assert_eq!(sp.w.dir, "/p/work"); // the directory it's really working in is looked up from here when handing over the terminal
        // Switched in the popup to run it in a split: it's that pane that gets split
        assert_eq!((sp.name.as_str(), sp.target.as_str(), sp.yazi), ("闲", "%2", true));
        // A stopped one can open too (directory from what it recorded), just with no pane to split
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('y'));
        let Some(External::Yazi(sp)) = app.external.take() else { panic!("{:?}", app.msg) };
        assert_eq!(sp.target, "");
        // A fold row doesn't know which one to open
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('y'));
        assert!(app.external.is_none() && app.msg.as_ref().unwrap().0.contains("先展开"), "{:?}", app.msg);
    }

    #[test]
    fn the_popup_opens_on_where_that_client_is() {
        let mut app = with_windows();
        app.highlight_here("$闲", "%2"); // on 「闲」's #2 → highlight that row
        assert_eq!(app.current().unwrap().title, "zsh");
        app.highlight_here("$跑", "%9"); // that pane has no row of its own → the session's row
        assert_eq!(app.current().unwrap().title, "跑");
        app.highlight_here("$别处", ""); // not in the list → no change
        assert_eq!(app.current().unwrap().title, "跑");
    }

    fn place(row: &str, session: &str, pane: &str) -> crate::follow::Place {
        crate::follow::Place { row: row.into(), session: session.into(), pane: pane.into() }
    }

    /// The popup opens on a pane of 「闲」 that has no row of its own (%9), following
    fn following() -> App {
        let mut app = with_windows();
        reader_rows(&mut app, 56, 30);
        app.highlight_here("$闲", "%9");
        app.follow_from("$闲", "%9");
        app
    }

    #[test]
    fn following_takes_the_client_to_the_row_the_highlight_rests_on() {
        let mut app = following();
        assert_eq!((app.follow_step(), app.current().unwrap().title.as_str()), (None, "闲"), "刚打开:不动");
        // No switch while scrolling, switch once settled; once switched, no switching again
        select(&mut app, "跑");
        app.moved = Some((Instant::now(), Duration::ZERO));
        assert_eq!(app.follow_step(), None);
        app.moved = None;
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        assert_eq!(app.follow_step(), None);
        // The row of another window in the session: lands on that pane
        select(&mut app, "zsh");
        assert_eq!(app.follow_step(), Some(place("闲\u{1}%2", "$闲", "%2")));
        // Back to the row it opened on: lands back on the original pane
        select(&mut app, "闲");
        assert_eq!(app.follow_step(), Some(place("闲", "$闲", "%9")));
        // Typing a filter: no switch; switch only once Enter settles it
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('跑'));
        assert_eq!((app.follow_step(), app.current().unwrap().title.as_str()), (None, "跑"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        press(&mut app, KeyCode::Esc); // clears the filter, doesn't exit
        assert!(!app.quit);
        // On a group: the side stays as it is
        press(&mut app, KeyCode::Char('g'));
        assert!(app.on_group_row());
        assert_eq!(app.follow_step(), None);
        // A session that isn't running: can't switch to it
        let mut v = sessions();
        v[2].state = State::Stopped; // 「跑」
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        app.follow_from("$闲", "");
        select(&mut app, "跑");
        assert_eq!(app.follow_step(), None);
    }

    #[test]
    fn esc_while_following_goes_back_and_the_popup_key_stays() {
        // Esc: go back to where it was when opened, then exit; the one left behind doesn't count as seen
        let mut app = following();
        select(&mut app, "跑");
        app.follow_step();
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.quit, app.follow_step()), (true, Some(place("闲", "$闲", "%9"))));
        app.follow_done();
        assert_eq!(app.reads.read_at("id-跑"), None);
        // ^\ (arrives as ^4 in the terminal): close, staying on the one shown beside; it counts as seen
        for key in ['4', '\\'] {
            let mut app = following();
            select(&mut app, "跑");
            app.follow_step();
            ctrl(&mut app, key);
            assert_eq!((app.quit, app.follow_step()), (true, None));
            app.follow_done();
            assert!(app.reads.read_at("id-跑").is_some());
        }
        // Pressed in the keys popup it's only looked up
        let mut app = following();
        press(&mut app, KeyCode::Char('?'));
        ctrl(&mut app, '4');
        assert!(!app.quit);
        // Closed without moving: no need to record the original one
        let mut app = following();
        ctrl(&mut app, '4');
        app.follow_done();
        assert_eq!((app.quit, app.reads.read_at("id-闲")), (true, None));
        // Not following, not in the popup: ^\ doesn't exit
        let mut app = with_windows();
        ctrl(&mut app, '4');
        assert_eq!(app.quit, crate::data::in_popup());
    }

    #[test]
    fn passing_by_an_unread_session_while_following_leaves_it_unread() {
        let mut v = sessions();
        v[2].state = State::Unread; // 「跑」: finished, not looked at yet
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        app.follow_from("$闲", "");
        select(&mut app, "跑");
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        assert!(app.reads.unread("id-跑"));
        // Passing over others: ones that aren't "finished, not looked at yet" aren't recorded
        select(&mut app, "问");
        app.follow_step();
        assert_eq!(app.reads.mark("id-问"), None);
        // Back to the unread one, closing on it: it's seen
        select(&mut app, "跑");
        app.follow_step();
        ctrl(&mut app, '4');
        app.follow_done();
        assert!(app.reads.read_at("id-跑").is_some());
    }

    #[test]
    fn following_draws_no_preview_and_shift_l_leaves_the_saved_layouts_alone() {
        let mut app = with_windows();
        // Not following: on a narrow screen it splits top/bottom, the preview takes the bottom half. Following: only the bottom
        // row is left, saying what it's doing
        let strip = |rows: &[String]| rows.iter().position(|r| r.starts_with('─')).unwrap();
        assert!(strip(&reader_rows(&mut app, 56, 30)) < 20);
        app.highlight_here("$闲", "%9");
        app.follow_from("$闲", "%9");
        let rows = reader_rows(&mut app, 56, 30);
        assert_eq!((app.mode(), strip(&rows)), (Preview::Off, 28), "{rows:#?}");
        // The bottom bar says what these two keys do while following
        assert!(rows[29].contains("Esc 返回") && rows[29].contains("⏎ 留下"), "{}", rows[29]);
        let saved = app.view_state();
        press(&mut app, KeyCode::Char('L'));
        assert!(app.mode() != Preview::Off);
        assert!(app.view_state() == saved, "L 换的只管这一次");
    }

    #[test]
    fn windows_and_split_panes_hang_under_their_session() {
        let mut app = with_windows();
        // Right below its own session, ordered by window number, pane number; the session idle for a day but a window running:
        // the whole family stays
        assert_eq!(order(&app), ["问", "跑", "闲", "htop", "zsh", "错", "构建"]);
        let rows = reader_rows(&mut app, 120, 30);
        // A session's windows are joined with ├, the last one with └
        shows(&rows, &["├ ○ #1.1 htop", "└ ○ #2 zsh", "└ ● #3 构建"]);
        // Window numbers of different lengths, the time column still lines up
        let col = |t: &str| {
            let r = row_of(&rows, t);
            r[..r.find(t).unwrap()].width()
        };
        assert_eq!((col(" 10秒"), col(" 30秒")), (col(" 20秒"), col(" 20秒")), "{rows:#?}");
        // The account follows the title, colored by tool
        let t = rows.iter().find(|r| r.contains("#3 构建")).unwrap();
        assert!(t.contains(" cx "), "{t}");
        // The count in the section header counts sessions only
        assert!(rows.iter().any(|r| r.contains("work  3")), "{rows:#?}");
        // w: an idle session with a running window counts too
        press(&mut app, KeyCode::Char('w'));
        assert!(order(&app).contains(&"构建".to_string()) && order(&app).contains(&"错".to_string()));
        press(&mut app, KeyCode::Char('w'));
        // Enter: go into its session, switching to that pane first
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
        // Operations on registered sessions don't apply to windows (D stops only this pane, see below)
        for k in ['R', 'r', 'm'] {
            press(&mut app, KeyCode::Char(k));
            assert!(app.confirm.is_none() && app.job.is_none() && app.prompt.is_none(), "{k}");
            assert!(app.msg.as_ref().unwrap().0.contains("#2 是窗口"), "{k}: {:?}", app.msg);
        }
        // d: closes only this pane
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭窗口？", "y 关闭"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job();
        assert!(matches!(&job, Some(Job::Remove { t, pane: Some(p) }) if t.title == "zsh" && p == "%2"), "{job:?}");
        assert_eq!(app.doing().as_deref(), Some("正在关闭「zsh」…"));
        // Only this pane is hidden; the session and the other windows stay
        let at = |app: &App, t: &str| app.all.iter().any(|s| s.title == t);
        assert!(!at(&app, "zsh") && at(&app, "闲") && at(&app, "htop"));
        let fresh = with_windows().all; // read before closing: still has it
        app.job_done(&job.unwrap(), Outcome::Done("已关闭「zsh」".into()), fresh);
        assert!(app.doing().is_none() && !at(&app, "zsh"));
        select(&mut app, "htop");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭窗格？"]);
        press(&mut app, KeyCode::Esc);
        // p: pins the session it's in, the windows go up with it
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(order(&app)[..2], ["闲", "htop"]);
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

    #[test]
    fn a_clicked_notification_goes_into_its_session_wherever_the_highlight_is() {
        let mut app = with_windows();
        select(&mut app, "htop");
        // kitty's reply arrives in the input as Alt+] … Alt+\: pick it out, don't treat it as keys
        let k = |c: char, m: KeyModifiers| Event::Key(KeyEvent::new(KeyCode::Char(c), m));
        let mut batch = vec![k(']', KeyModifiers::ALT)];
        batch.extend("99;i=aca-id-闲;".chars().map(|c| k(c, KeyModifiers::NONE)));
        batch.push(k('\\', KeyModifiers::ALT));
        for osc in crate::clicked::take(&mut batch) {
            app.open_clicked(crate::clicked::session(&osc).unwrap());
        }
        assert!(batch.is_empty());
        assert_eq!(app.current().map(|s| s.title.as_str()), Some("闲"));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        // A stopped session: start again continuing the original conversation, same as Enter
        app.all.iter_mut().find(|s| s.title == "错" && s.sub.is_none()).unwrap().state = State::Stopped;
        app.open_clicked("id-错");
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("错")), None));
        // Session removed: say so, no change
        app.open_clicked("id-没了");
        assert_eq!((app.attach.take(), app.start.take()), (None, None));
        assert_eq!(said(&app), "通知中的会话已不存在");
    }

    #[test]
    fn big_d_on_a_window_stops_just_that_pane_and_enter_brings_it_back_in_place() {
        let mut app = with_windows();
        reader_rows(&mut app, 120, 30);
        // Window row: stops only it, stays in place
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('D'));
        shows(&reader_rows(&mut app, 100, 30), &["停止此窗格？", "停止程序，保留窗格 · ⏎ 重新启动", "y 停止"]);
        press(&mut app, KeyCode::Char('y'));
        let pane = |id: &str, label: &str| crate::park::Pane { id: id.into(), label: label.into() };
        let t = |label: &str| Target { name: "闲".into(), title: label.into(), profile: "default".into(), id: String::new() };
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Park { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") });
        // Stopped: record a step (^Z starts it again in place, see below)
        let mut fresh = with_windows().all;
        fresh.iter_mut().find(|s| s.title == "zsh").unwrap().state = State::Dead;
        app.job_done(&job, Outcome::Done("已停「#2「zsh」」 · ⏎ 再启动".into()), fresh.clone());
        // Stopped: D says so, ⏎ starts it again in place (attaching once the run loop is up)
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('D'));
        assert_eq!(said(&app), "已停止 · ⏎ 重新启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.respawn.take(), app.attach.take()), (Some(("闲".to_string(), pane("%2", "#2「zsh」"))), None));
        ctrl(&mut app, 'z');
        assert_eq!(app.take_job(), Some(Job::Respawn { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") }));
        app.job_done(&Job::Respawn { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") }, Outcome::Done("已重启".into()), fresh);
        assert_eq!(said(&app), "已撤销停止:「#2「zsh」」已在原位置重新启动");
        // Registered session row: stops the agent (its pane and the other windows stay), not "this pane"
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('D'));
        assert!(matches!(app.confirm, Some(Confirm::Stop { .. })));
        press(&mut app, KeyCode::Esc);
        // R: restarts the agent in place, other windows untouched
        press(&mut app, KeyCode::Char('R'));
        assert_eq!(app.take_job(), Some(Job::Restart { t: target("闲"), force: false }));
    }

    #[test]
    fn an_agent_that_quit_but_left_its_pane_starts_again_on_enter() {
        // Agent exited (stopped by D or on its own), pane kept with its screen: ■. ⏎ starts again continuing the original
        // conversation; not going straight in, nor rerunning the pane's original command
        let mut v = with_windows().all;
        let h = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (h.state, h.pane) = (State::Dead, "%0".into());
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take(), app.respawn.take()), (Some(target("闲")), None, None));
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "已停止 · ⏎ 重新启动");
        // Another window inside it is entered as usual
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
        // Plain tmux session: D stops its pane, ⏎ starts it again in place with the original command
        let mut s = mk("外", "-", State::Idle, 5);
        (s.deck_id, s.profile, s.pane) = (String::new(), String::new(), "%7".into());
        let mut app = App::new(String::new(), vec![s.clone()]);
        press(&mut app, KeyCode::Char('D'));
        press(&mut app, KeyCode::Char('y'));
        let p = crate::park::Pane { id: "%7".into(), label: "外".into() };
        assert_eq!(
            app.take_job(),
            Some(Job::Park {
                t: Target { name: "外".into(), title: "外".into(), profile: String::new(), id: String::new() },
                p: p.clone()
            })
        );
        s.state = State::Dead;
        let mut app = App::new(String::new(), vec![s]);
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.respawn.take(), app.start.take()), (Some(("外".to_string(), p)), None));
    }

    #[test]
    fn d_on_a_registered_session_takes_its_windows_along_and_a_plain_one_only_loses_its_head() {
        let at = |app: &App, t: &str| app.all.iter().any(|s| s.title == t);
        // Registered session: removes the whole session, other windows in it close too; the confirmation says so
        let mut app = with_windows();
        reader_rows(&mut app, 120, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["删除会话？", "同时关闭其中的 2 个窗口 / 窗格", "y 删除"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Remove { t, pane: None } if t.title == "闲"), "{job:?}");
        assert!(!at(&app, "闲") && !at(&app, "htop") && !at(&app, "zsh"));

        // Plain tmux session row with other windows inside: closes only the first pane, the next one moves up
        let plain = |v: &mut Vec<Session>, pane: &str| {
            v.iter_mut().filter(|s| s.family() == "闲").for_each(|s| s.profile.clear());
            let h = v.iter_mut().find(|s| s.title == "闲").unwrap();
            (h.deck_id, h.pane) = (String::new(), pane.into());
        };
        let mut v = with_windows().all;
        plain(&mut v, "%0");
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 120, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭此窗格？", "其余依次前移，#1.1「htop」成为首个", "y 关闭"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Remove { t, pane: Some(p) } if t.title == "闲" && p == "%0"), "{job:?}");
        // Nothing is hidden while closing: the session row stays until htop moves up, windows aren't hidden along with the
        // session (they'd be lost), and the family never loses its head
        assert!(at(&app, "闲") && at(&app, "htop") && at(&app, "zsh"));
        reader_rows(&mut app, 100, 30);
        // Closed: htop moves up and becomes the session row (session name unchanged)
        let mut fresh: Vec<Session> = with_windows().all.into_iter().filter(|s| s.title != "htop").collect();
        plain(&mut fresh, "%1");
        app.job_done(&job, Outcome::Done("已关闭「闲」".into()), fresh);
        assert!(at(&app, "闲") && at(&app, "zsh"));
        // Only one pane left: d closes the whole session
        let mut v = sessions();
        plain(&mut v, "%1");
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Remove(r)) if r.pane.is_none() && r.next.is_none() && r.others == 0));
    }

    /// 「闲」 spawned two child sessions: 「子跑」 running (with another window opened in it), 「子旧」 idle for three days;
    /// parent_idle = how long 「闲」 has been idle
    fn with_children(parent_idle: u64) -> App {
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
    fn row_of(rows: &[String], want: &str) -> String {
        rows.iter().find(|r| r.contains(want)).unwrap_or_else(|| panic!("缺「{want}」:{rows:#?}")).clone()
    }

    #[test]
    fn children_hang_under_the_session_that_sent_them() {
        let mut app = with_children(10);
        // Last day: follows its parent session (and its windows); a child idle for three days is still hidden
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        for (w, two) in [(46, false), (120, true)] {
            let rows = reader_rows(&mut app, w, 30);
            assert!(row_of(&rows, "子跑").contains("└ ● 子跑"), "{w}: {rows:#?}");
            assert!(row_of(&rows, "zsh").contains("  └ ○ #2 zsh"), "{w}: {rows:#?}");
            // Count in the section header: a child session is a session too
            assert!(rows.iter().any(|r| r.contains("work  3")), "{two}: {rows:#?}");
        }
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "子旧", "错"]);
        // Removing the parent: the children aren't removed with it, say so (hidden ones count too)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 46, 16), &["删除会话？", "2 个子会话不会删除"]);
        press(&mut app, KeyCode::Esc);
        // A child itself, a window: nothing said
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.confirm.as_ref().map(|c| matches!(c, Confirm::Remove(r) if r.kids == 0)), Some(true));
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
    fn with_grandchildren() -> App {
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
    fn sub_sessions_of_sub_sessions_hang_one_level_further_in() {
        let mut app = with_grandchildren();
        // A session is followed by its whole family: windows, children (with their windows and what they spawned), and only
        // then the next one
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        for w in [46, 120] {
            let rows = reader_rows(&mut app, w, 30);
            for want in ["└ ● 子跑", "  ├ ○ #2 zsh", "  └ ● 孙", "    ├ ○ #3 top", "    └ ● 曾孙"] {
                assert!(row_of(&rows, want).contains(want), "{w}: {rows:#?}");
            }
        }
        // 子跑 has a sibling after it (子旧): its level's vertical line runs down to the last row of its family
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙", "子旧", "错"]);
        let rows = reader_rows(&mut app, 46, 30);
        let wants = ["├ ● 子跑", "│ ├ ○ #2 zsh", "│ └ ● 孙", "│   ├ ○ #3 top", "│   └ ● 曾孙", "└ ○ 子旧"];
        for want in wants {
            assert!(row_of(&rows, want).contains(want), "{rows:#?}");
        }
        // h goes out one level at a time
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "孙");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "子跑");
        // Removing the middle one: the confirmation counts the ones it spawned (once it's gone they're top level; deeper ones
        // stay below them)
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.confirm.as_ref().map(|c| matches!(c, Confirm::Remove(r) if r.kids == 1)), Some(true));
        press(&mut app, KeyCode::Esc);
        // Searching the innermost: every level above comes along (with their windows); searching the middle one: the ones
        // below don't
        let v: Vec<Session> = app.all.clone();
        assert_eq!(order(&App::new("曾孙".into(), v.clone())), ["闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        assert_eq!(order(&App::new("子跑".into(), v)), ["闲", "子跑", "zsh"]);
    }

    #[test]
    fn the_tree_stops_indenting_after_a_few_levels() {
        let up = |i: usize| if i == 0 { Up::Top } else { Up::Under(format!("s{}", i - 1), String::new()) };
        let chain: Vec<Session> = (0..7).map(|i| Session { up: up(i), ..mk(&format!("s{i}"), "work", State::Idle, 10) }).collect();
        let view: Vec<&Session> = chain.iter().collect();
        let marks: Vec<String> = tree_marks(&view, true).into_iter().map(|m| m.0).collect();
        // From level TREE_DEEP down, drawn side by side, no further indent
        assert_eq!(marks, ["", "└ ", "  └ ", "    └ ", "      ├ ", "      ├ ", "      └ "]);
        // The parent isn't in the slice above (filtered out): top level counts from there
        let marks: Vec<String> = tree_marks(&view[2..5], true).into_iter().map(|m| m.0).collect();
        assert_eq!(marks, ["", "└ ", "  └ "]);
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

    #[test]
    fn big_d_stops_after_y_and_a_stopped_one_starts_on_enter() {
        // Real activity times: after stopping, idle time keeps counting against the current time
        let real = |v: Vec<Session>| -> Vec<Session> {
            v.into_iter()
                .map(|mut s| {
                    s.active_at = epoch() - s.idle_for;
                    s
                })
                .collect()
        };
        let mut app = App::new(String::new(), real(sessions()));
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('D'));
        shows(&reader_rows(&mut app, 46, 16), &["停止会话？", "y 停止"]);
        press(&mut app, KeyCode::Enter); // only y counts
        assert!(app.job.is_none() && app.attach.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "未停止");
        press(&mut app, KeyCode::Char('D'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.job, Some(Job::Stop { t: target("闲") }));
        let job = app.take_job().unwrap();
        // After stopping: still in the list, marked ■; the tmux activity time is gone, keep using what was seen before stopping,
        // so it isn't hidden by "over 1 day"
        let fresh: Vec<Session> = real(sessions())
            .into_iter()
            .map(|mut s| {
                if s.title == "闲" {
                    (s.state, s.idle_for, s.active_at) = (State::Stopped, 9_000_000, 0);
                }
                s
            })
            .collect();
        app.job_done(&job, Outcome::Done("已停止「闲」· ⏎ 重新启动".into()), fresh);
        let s = app.current().unwrap();
        assert_eq!((s.title.as_str(), s.state), ("闲", State::Stopped));
        assert!(s.idle_for < 60, "{}", s.idle_for);
        assert!(titles(&app).contains(&"闲".to_string()));
        shows(&reader_rows(&mut app, 120, 16), &["已停止", "⏎ 启动 · d 删除"]);
        // D again: already stopped; ⏎ starts it first, then goes in, no direct attach
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已停止 · ⏎ 重新启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
        // Not in w (open ones)
        press(&mut app, KeyCode::Char('w'));
        assert!(!titles(&app).contains(&"闲".to_string()));
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
    fn a_crashed_one_is_a_cross_but_starts_like_a_stopped_one() {
        let mut v = sessions();
        v[3].state = State::Crashed; // 「闲」: the last turn ended in an error, gone from tmux too
        v[4].state = State::Idle; // 「错」: leave just one with an error, to see whether the top bar counts it
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        assert!(row_of(&reader_rows(&mut app, 46, 16), "闲").contains("✕ 闲"));
        shows(&reader_rows(&mut app, 120, 16), &["出错 · 已停止", "⏎ 启动 · d 删除"]);
        assert!(!top_bar(&app, 80).to_string().contains("出错")); // one that isn't running doesn't count as an error
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
        press(&mut app, KeyCode::Char('w')); // nor as open
        assert!(!titles(&app).contains(&"闲".to_string()));
    }

    #[test]
    fn status_icons_use_the_theme_colors_not_dim() {
        let p = theme::pal();
        for (st, fg) in [
            (State::Ask, p.red),
            (State::Running, p.green),
            (State::Unread, p.amber),
            (State::Idle, p.muted),
            (State::Error, p.red),
            (State::Crashed, p.red),
            (State::Stopped, p.muted),
        ] {
            let sty = icon(st).1;
            assert_eq!(sty.fg, Some(fg), "{st:?}");
            assert!(!sty.add_modifier.contains(Modifier::DIM), "{st:?}");
        }
    }

    #[test]
    fn r_renames_in_the_footer_with_the_old_title_filled_in() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('r'));
        shows(&reader_rows(&mut app, 46, 12), &["重命名", "Esc 取消", "> 闲▏", "⏎ 确定"]);
        press(&mut app, KeyCode::Enter); // unchanged
        assert!(app.job.is_none() && app.prompt.is_none());
        assert!(app.msg.is_none());
        press(&mut app, KeyCode::Char('r'));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter); // empty isn't accepted
        shows(&reader_rows(&mut app, 46, 12), &["标题不能为空"]);
        typ(&mut app, "闲着 jk"); // while typing, j k are letters, not up/down
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(Job::Rename { t: target("闲"), to: "闲着 jk".into(), unlock: false }));
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.job.is_none());
        assert!(app.msg.is_none());
        // Refresh moved to ^R
        ctrl(&mut app, 'r');
        assert!(app.refresh && app.prompt.is_none());
    }

    #[test]
    fn m_picks_a_group_and_a_new_name_becomes_a_new_group() {
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑"); // in work
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        // The list's section order first, without its current work, no duplicates; groups with no default directory show
        // their sessions' directory
        let rows: Vec<(&str, &str, bool)> = p.rows.iter().map(|r| (r.group.as_str(), r.dir.as_str(), r.same)).collect();
        assert_eq!(rows, [("fun", "/p/fun", false), ("Zeta", "", false)]);
        assert_eq!(p.here, "/p/work");
        app.group_picked(p, Ok(Some(" side proj/part ".into())));
        let all = ["Zeta", "fun", "work"].map(String::from).to_vec();
        assert_eq!(app.job.take(), Some(Job::Move { t: target("跑"), from: "work".into(), to: "side-proj/part".into(), groups: all }));
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(Some("Work".into())));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已在 Work 中");
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(None)); // Esc pressed in fzf
        assert!(app.job.is_none());
    }

    #[test]
    fn m_lists_the_group_in_its_own_directory_first_and_prefers_the_default_directory() {
        let hints = || {
            let g = |n: &str, d: &str| (n.to_string(), d.to_string());
            crate::data::GroupHints { groups: vec![g("work", ""), g("fun", "/w/fun-home"), g("Zeta", "/p/work")] }
        };
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑"); // in work, directory /p/work
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        let rows: Vec<(&str, &str, bool)> = p.rows.iter().map(|r| (r.group.as_str(), r.dir.as_str(), r.same)).collect();
        // Zeta's default directory is its directory: moved to the front; fun has a default directory, so its sessions' /p/fun
        // isn't looked at
        assert_eq!(rows, [("Zeta", "/p/work", true), ("fun", "/w/fun-home", false)]);
    }

    #[test]
    fn keys_for_registered_sessions_refuse_plain_tmux_sessions() {
        let mut s = mk("外", "-", State::Idle, 5);
        (s.deck_id, s.profile) = (String::new(), String::new());
        let mut app = App::new(String::new(), vec![s]);
        for (key, what) in [('R', "重启"), ('r', "重命名"), ('m', "移到分组")] {
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.msg.as_ref().unwrap().0, format!("普通 tmux 会话 · A 接管后才能{what}"));
        }
        assert!(app.job.is_none() && app.prompt.is_none() && app.pick_group.is_none() && app.confirm.is_none());
    }

    /// What's recognized before adopting (fake): only recognizes the tmux session 「外」
    fn fake_plan(target: &str) -> Result<aca_core::adopt::Plan, String> {
        if target != "$外" {
            return Err(format!("tmux 里没有 {target} 这个会话"));
        }
        Ok(aca_core::adopt::Plan {
            s: aca_core::store::Session {
                id: "id-接过来".into(),
                title: "agentdeck_x".into(),
                tool: "claude".into(),
                account: "sub".into(),
                group: "work".into(),
                conv: "c-1234567890".into(),
                args: vec!["--yolo".into()],
                ..Default::default()
            },
            known: false,
            sid: "$外".into(),
            tmux: "agentdeck_x".into(),
            pane: "%9".into(),
            transcript: String::new(),
            auto_title: "修登录".into(),
            deck: Some(("default".into(), "ad-1".into())),
            notes: vec![],
        })
    }

    #[test]
    fn big_a_adopts_a_plain_tmux_session_after_showing_what_it_recognised() {
        let plain = |title: &str| {
            let mut s = mk(title, "-", State::Idle, 5);
            (s.deck_id, s.profile, s.tool) = (String::new(), String::new(), String::new());
            s
        };
        let mut all = sessions();
        all.extend([plain("外"), plain("没了的")]);
        let mut app = App::new(String::new(), all).with_adopt(fake_plan);
        // Registered session: already is one
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.msg.as_ref().unwrap().0, "它已是 aca 会话");
        assert!(app.confirm.is_none());
        // Can't be recognized (that session just went away): say why, no popup
        select(&mut app, "没了的");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.msg.as_ref().unwrap().0, "无法接管:tmux 里没有 $没了的 这个会话");
        assert!(app.confirm.is_none());

        // Plain session: lay out what was recognized first
        select(&mut app, "外");
        press(&mut app, KeyCode::Char('A'));
        assert!(matches!(app.confirm, Some(Confirm::Adopt { .. })));
        let rows = reader_rows(&mut app, 100, 30).join("\n");
        for want in [
            "接管这个会话？",
            "登记为 claude:sub · 分组 work",
            "对话 c-123456，进程不受影响，重启后继续",
            "tmux 会话名改为「修登录」",
            "每次启动时附带 --yolo",
            "agent-deck 中会显示为已停止",
            "y 接管",
        ] {
            assert!(rows.contains(want), "确认框里没有「{want}」:\n{rows}");
        }
        // Only y counts; any other key means don't adopt
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.msg.as_ref().unwrap().0, "未接管");
        assert!(app.job.is_none() && app.confirm.is_none());
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        let t = Target { name: "外".into(), title: "外".into(), profile: String::new(), id: String::new() };
        assert_eq!(job, Job::Adopt { t, plan: Box::new(fake_plan("$外").unwrap()) });
        assert_eq!(job.doing(), "正在接管「外」…");

        // After adopting, that row becomes the registered one (keyed by id, in its group): the highlight follows
        let mut fresh = sessions();
        let mut got = mk("修登录", "work", State::Idle, 5);
        (got.name, got.deck_id) = ("id-接过来".into(), "id-接过来".into());
        fresh.push(got);
        app.job_done(&job, Outcome::Made { msg: "已接管「修登录」".into(), id: "id-接过来".into() }, fresh);
        assert_eq!(app.current().map(|s| (s.name.as_str(), s.title.as_str())), Some(("id-接过来", "修登录")));
        assert_eq!(app.msg.as_ref().unwrap().0, "已接管「修登录」");
    }

    #[test]
    fn the_adopt_dialog_says_when_nothing_will_be_renamed_or_resumed() {
        let mut p = fake_plan("$外").unwrap();
        // A session name you gave yourself (locked), no conversation recognized, no args, not agent-deck's
        (p.s.title, p.s.title_locked, p.tmux) = ("我的会话".into(), true, "我的会话".into());
        (p.s.conv, p.s.args, p.s.group, p.deck) = (String::new(), vec![], String::new(), None);
        assert_eq!(adopt_summary(&p), "登记为 claude:sub\n无法识别对话：重启后将开始新对话");
        (p.s.tool, p.s.account) = ("shell".into(), String::new());
        assert_eq!(adopt_summary(&p), "登记为 shell\n其中的程序不受影响");
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

    #[test]
    fn question_mark_shows_the_keys_from_the_top() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.reader_target().is_none()); // not a session, the run loop doesn't capture it from tmux
        // On a phone it rises from the bottom, one column scrolling down from the top; a quick reference grouped by purpose,
        // not the text of aca help keys
        shows(&reader_rows(&mut app, 60, 12), &["快捷键", "1/", "导航"]);
        // On a desktop a centered popup, three columns fitting on one screen
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("导航") && r.contains("新建时")), "{rows:#?}");
        // Pressing a key inside = looking it up: lights up the rows describing it, doesn't actually do it
        let lit = |app: &App| crate::help::lines(100, app.reader.as_ref().and_then(|r| r.hit.as_ref())).1.len();
        press(&mut app, KeyCode::Char('T'));
        assert!(app.split.is_none());
        assert_eq!(lit(&app), 1);
        press(&mut app, KeyCode::Char('t'));
        assert!(app.term_req.is_none());
        assert_eq!(lit(&app), 1);
        press(&mut app, KeyCode::Char('y'));
        assert!(app.external.is_none());
        assert_eq!(lit(&app), 1);
        press(&mut app, KeyCode::Char('J')); // doesn't switch sessions
        press(&mut app, KeyCode::Enter); // doesn't enter the session
        assert!(app.reader.is_some() && app.attach.is_none());
        assert_eq!(lit(&app), 1);
        // Only says so for something that isn't a key
        assert!(!reader_rows(&mut app, 150, 40).iter().any(|r| r.contains("不是快捷键")));
        press(&mut app, KeyCode::Char('U'));
        shows(&reader_rows(&mut app, 150, 40), &["「U」不是快捷键"]);
        // On a phone, one column scrolling down: if the lit row isn't on this screen, scroll to it
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(lit(&app), 2);
        shows(&reader_rows(&mut app, 46, 12), &["移到分组"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
        // The bottom bar can't fit every key: drop the least used first; ? and ⏎ always stay
        assert_eq!(reader_rows(&mut app, 46, 12)[11], "n 新建 │ ⏎ 打开 • i 下一个 │ / 搜索 • ? 快捷键");
    }

    #[test]
    fn plus_in_the_tool_menu_opens_the_accounts_page_ready_to_add() {
        let mut app = fixture().with_tools(|| Ok(vec![])).with_accounts(|adding| Box::new(FakePage(if adding { 7 } else { 0 })));
        app.tools_loader = || Ok(vec![tool("work", "claude", "work", Agent::Claude)]);
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 60, 14), &["+ 添加账号"]);
        press(&mut app, KeyCode::Char('+'));
        // The menu closes, into the accounts page (opening on adding an account); closing it returns to the list, not the
        // menu, no settings page
        assert!(app.tool_menu.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 7"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && app.tool_menu.is_none() && app.settings.is_none());
        // By default (snapshots, other tests) there's no accounts page and config isn't touched: + does nothing, the menu stays
        let mut plain = fixture().with_tools(|| Ok(vec![tool("x", "x", "", Agent::Other)]));
        press(&mut plain, KeyCode::Char('n'));
        press(&mut plain, KeyCode::Char('+'));
        assert!(plain.tool_menu.is_some() && plain.overlays.is_empty());
    }

    #[test]
    fn n_picks_a_tool_then_asks_for_the_path() {
        let mut app =
            fixture().with_tools(|| Ok(vec![tool("work", "claude", "work", Agent::Claude), tool("cx", "codex", "cx", Agent::Codex)]));
        pin(&mut app, "跑");
        pin(&mut app, "跑"); // select 「跑」
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 46, 12), &["选择工具", "1 work  ~/.work", "2 cx    ~/.cx", "⏎ 选择"]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j')); // wraps around to the first
        press(&mut app, KeyCode::Esc);
        assert!(app.tool_menu.is_none() && app.new_draft.is_none());
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('2')); // a digit picks directly
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.label.as_str(), d.text.as_str()), ("cx", "/p/work"));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.spec().as_str(), r.dir.as_str()), ("codex:cx", "/p/work"));
        // A row that can't start (the tool names an unregistered account): says why; picking it only shows a message, the
        // menu stays
        let mut bad = fixture().with_tools(|| {
            Ok(vec![Tool { problem: Some("没有 Codex 账号槽 gone".into()), ..tool("cx", "codex", "gone", Agent::Codex) }])
        });
        pin(&mut bad, "跑");
        pin(&mut bad, "跑");
        press(&mut bad, KeyCode::Char('n'));
        shows(&reader_rows(&mut bad, 46, 12), &["1 cx  没有 Codex 账号槽 gone"]);
        press(&mut bad, KeyCode::Char('1'));
        assert!(bad.tool_menu.is_some() && bad.new_draft.is_none());
        assert_eq!(bad.msg.as_ref().unwrap().0, "没有 Codex 账号槽 gone");
        // Empty menu: say where to change it
        let mut empty = fixture();
        press(&mut empty, KeyCode::Char('n'));
        assert!(empty.tool_menu.is_none() && empty.msg.as_ref().unwrap().0.starts_with("工具菜单为空"));
    }

    #[test]
    fn new_path_can_be_edited_or_cancelled() {
        let mut app = fixture();
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('N'));
        typ(&mut app, "q"); // q in the input box is a letter, not quit
        assert!(!app.quit);
        press(&mut app, KeyCode::Esc);
        assert!(app.new_draft.is_none() && app.create.is_none());
        press(&mut app, KeyCode::Char('N'));
        ctrl(&mut app, 'w');
        typ(&mut app, "新项目");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.create.take().unwrap().dir, "/p/新项目");
        press(&mut app, KeyCode::Char('N'));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter); // empty path isn't created, the message stays above the input box
        assert!(app.create.is_none());
        assert_eq!(app.new_draft.as_ref().unwrap().hint, "路径不能为空");
    }

    #[test]
    fn pin_moves_to_top_keeps_highlight_and_survives_refresh() {
        let mut app = fixture();
        assert_eq!(order(&app), ["问", "跑", "闲"]);
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["闲", "问", "跑"]); // pinned ones above "needs you"
        assert_eq!(app.current().unwrap().title, "闲");
        app.set_sessions(sessions()); // 2-second refresh, pinned is false all over the new data
        assert_eq!(order(&app), ["闲", "问", "跑"]);
    }

    #[test]
    fn later_pins_go_below_and_unpin_returns_to_place() {
        let mut app = fixture();
        pin(&mut app, "跑");
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["跑", "闲", "问"]);
        pin(&mut app, "跑");
        assert_eq!(order(&app), ["闲", "问", "跑"]);
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
    fn capital_f_lists_the_states_to_look_at_and_the_row_keys_pick_one() {
        let mut app = fixture();
        assert_eq!(titles(&app), ["跑", "问", "闲"]);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(0));
        // One row per state: at the start the key that picks it directly, icon, name, count; row 0 is all
        let rows = reader_rows(&mut app, 60, 16);
        shows(
            &rows,
            &["按状态筛选", "Esc 关闭", "␣", "全部", "!  ●", "运行中", "@  ◆◐", "等待回应 · 未读", "%  ☾", "自动停止", "⏎ 筛选 • ␣ 全部"],
        );
        // The row's key picks directly and closes the small list; reopened, the highlight is on the picked one
        press(&mut app, KeyCode::Char('!'));
        assert_eq!((app.only_menu, app.status), (None, Some(Pick::Run)));
        assert_eq!(titles(&app), ["跑"]);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(1));
        // ⏎ on the picked one = cancel
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.only_menu, app.status), (None, None));
        // ↓ to "needs you · unread" ⏎; Space = all; Esc only closes the small list
        keys(&mut app, "Fjj⏎");
        assert_eq!((app.status, titles(&app)), (Some(Pick::Wait), vec!["问".to_string()]));
        keys(&mut app, "F ");
        assert_eq!(app.status, None);
        keys(&mut app, "F⎋");
        assert!(app.only_menu.is_none() && !app.quit);
        // f only forks, no longer doubles as F
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt.is_some() && app.only_menu.is_none());
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
    fn header_counts_follow_the_visible_list() {
        let mut app = fixture();
        let header = |app: &App| top_bar(app, 240).to_string();
        assert!(header(&app).contains("◆ 1 等待回应"));
        assert!(header(&app).contains("● 1 运行中"));
        assert!(!header(&app).contains('◐') && !header(&app).contains('✕'));

        app.set_group(Some("work".into()));
        assert!(!header(&app).contains('◆'));
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        assert_eq!(said(&app), "暂无待处理会话");

        app.set_group(None);
        press(&mut app, KeyCode::Char('a'));
        assert!(header(&app).contains("◐ 1 未读") && header(&app).contains("✕ 1 出错"));
        keys(&mut app, "/未读⏎");
        assert!(header(&app).contains("◐ 1 未读"));
        assert!(!header(&app).contains('◆') && !header(&app).contains('●'));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('!'));
        assert!(header(&app).contains("● 1 运行中"));
        assert!(!header(&app).contains('◆') && !header(&app).contains('◐'));

        press(&mut app, KeyCode::Char('!'));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('o'));
        assert!(!header(&app).contains('◆') && !header(&app).contains('◐'));
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Char('o'));
        assert!(header(&app).contains("◆ 1 等待回应") && header(&app).contains("◐ 1 未读"));
    }

    #[test]
    fn empty_working_stays_empty_instead_of_falling_back() {
        let mut app = App::new(String::new(), vec![mk("停", "work", State::Stopped, 5), mk("退", "work", State::Exited, 5)]);
        press(&mut app, KeyCode::Char('w'));
        assert!(app.visible().0.is_empty());
    }

    #[test]
    fn braces_cycle_groups_in_section_order_and_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('}')); // fun first like the sections, though work has more sessions
        assert_eq!(titles(&app), ["问"]);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["跑", "闲"]);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["跑", "问", "闲"]); // back to all groups
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(titles(&app), ["跑", "闲"]); // wraps back to the last group, work
    }

    #[test]
    fn tab_lists_the_groups_and_a_letter_or_enter_switches() {
        let mut app = App::new(String::new(), life_and_work());
        keys(&mut app, "⇥");
        assert_eq!(app.group_rows(), [(' ', None), ('f', Some("fun".into())), ('w', Some("work".into()))]);
        assert_eq!(app.group_menu, Some(0)); // never switched: on the current one (all)
        let rows = reader_rows(&mut app, 80, 24);
        shows(&rows, &["分组", "␣  全部", "f  fun", "w  work", "⏎ 切换"]);
        keys(&mut app, "w");
        assert_eq!((app.group.as_deref(), app.group_menu, titles(&app)), (Some("work"), None, vec!["工作".to_string()]));
        keys(&mut app, "⇥⏎"); // on the last one looked at (all): Tab ⏎ = switch back
        assert_eq!(app.group, None);
        keys(&mut app, "⇥⏎");
        assert_eq!(app.group.as_deref(), Some("work"));
        keys(&mut app, "⇥ "); // Space = all
        assert_eq!(app.group, None);
        keys(&mut app, "⇥↑⏎"); // on work, one up is fun
        assert_eq!(app.group.as_deref(), Some("fun"));
        keys(&mut app, "⇥z"); // a letter with no group: no change (highlight on the last one looked at: all)
        assert_eq!((app.group_menu, app.group.as_deref()), (Some(0), Some("fun")));
        keys(&mut app, "⇥"); // Tab again closes
        keys(&mut app, "⇥q⇥⎋"); // q / Esc close too, don't quit
        assert!(app.group_menu.is_none() && !app.quit && app.group.as_deref() == Some("fun"));
        keys(&mut app, "}"); // { } still switch one step at a time
        assert_eq!(app.group.as_deref(), Some("work"));
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
    fn i_goes_through_waiting_sessions_newest_first() {
        let v = vec![
            mk("旧问", "fun", State::Ask, 50),
            mk("新读", "fun", State::Unread, 10),
            mk("跑", "work", State::Running, 1),
            mk("错", "work", State::Error, 5),
            mk("中读", "work", State::Unread, 30),
        ];
        let mut app = App::new(String::new(), v.clone());
        let next = |app: &mut App| {
            press(app, KeyCode::Char('i'));
            let name = app.attach.take();
            if let Some(n) = &name {
                app.mark_seen(n); // back after looking
            }
            name
        };
        assert_eq!(next(&mut app).as_deref(), Some("新读"));
        assert_eq!(app.current().unwrap().title, "新读"); // back, the highlight is on the one just looked at
        assert_eq!(next(&mut app).as_deref(), Some("中读"));
        assert_eq!(next(&mut app).as_deref(), Some("旧问"));
        assert_eq!(next(&mut app), None); // running and error ones don't count; all seen, so it doesn't enter
        let mut v = v;
        v[1].active_at = epoch() + 10; // 新读 has new output again
        app.set_sessions(v);
        assert_eq!(next(&mut app).as_deref(), Some("新读"));
    }

    #[test]
    fn i_only_looks_at_what_is_shown() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('}'));
        press(&mut app, KeyCode::Char('}')); // nothing in the work group needs you (未读 is in fun, and over a day old)
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Char('}')); // back to all groups: 未读 is over a day old, hidden by "1 day", only 问 is left
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.attach.as_deref(), Some("问"));
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

    fn said(app: &App) -> &str {
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
    fn ticked(app: &mut App) -> Vec<String> {
        let rows = reader_rows(app, 80, 20);
        let titles: Vec<String> = app.all.iter().map(|s| s.title.clone()).collect();
        rows.iter().filter(|r| r.starts_with('▌')).filter_map(|r| titles.iter().find(|t| r.contains(&format!(" {t} "))).cloned()).collect()
    }

    fn many(act: fn(Target) -> Job, titles: &[&str]) -> Job {
        Job::Many(titles.iter().map(|t| act(target(t))).collect())
    }

    #[test]
    fn v_stretches_a_range_and_d_deletes_them_together_as_one_trash_entry() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('v'));
        keys(&mut app, "jj");
        assert_eq!(ticked(&mut app), ["跑", "闲", "错"]);
        let foot = reader_rows(&mut app, 80, 20).pop().unwrap();
        for want in ["连选", "已选 3", "d 删除", "m 移到分组", "Esc 确定"] {
            assert!(foot.contains(want), "缺「{want}」:{foot}");
        }
        // Pulling back, the range shrinks with it
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(ticked(&mut app), ["跑", "闲"]);
        press(&mut app, KeyCode::Char('j'));
        // d: asks first, listing which ones and how many running ones get interrupted; any other key doesn't remove, the
        // selection stays
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 80, 24), &["删除 3 个会话？", "跑", "闲", "错", "1 个正在运行或等待回应，将被中断", "y 删除"]);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((said(&app), app.chosen().len()), ("未删除", 3));
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        assert_eq!(job, many(|t| Job::Remove { t, pane: None }, &["跑", "闲", "错"]));
        // Selection ends; hidden from the list first, removed together in the background
        assert!(!app.picking());
        assert_eq!(order(&app), ["问", "未读"]);
        assert_eq!(app.doing().as_deref(), Some("正在删除 3 个…"));
        let gone = |t: &str| crate::trash::Gone { id: format!("id-{t}"), title: t.into(), group: "work".into(), archived: false };
        let item = crate::trash::Item::sessions("default", vec![gone("跑"), gone("闲"), gone("错")]);
        let out = Outcome::Binned { msg: format!("已删除{} · ^Z 撤销", item.name()), item: Box::new(item) };
        app.job_done(&job, out, sessions().into_iter().filter(|s| s.group != "work").collect());
        assert_eq!(said(&app), "已删除「跑」等 3 个 · ^Z 撤销");
        // One entry in the Trash; ^Z restores them together, ones open before the removal start again (ones stopped on an
        // error don't count)
        assert_eq!(app.trash.items(epoch()).len(), 1);
        ctrl(&mut app, 'z');
        let Some(Job::Restore { item, start }) = app.take_job() else { panic!("{:?}", app.job) };
        assert_eq!((item.sessions.len(), start), (3, vec!["id-跑".to_string(), "id-闲".into(), "id-错".into()]));
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
    fn v_and_shift_v_stretch_runs_that_select_and_unselect_as_in_yazi() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        let foot = |app: &mut App| reader_rows(app, 80, 20).pop().unwrap();
        let has = |foot: String, want: &[&str]| want.iter().for_each(|w| assert!(foot.contains(w), "缺「{w}」:{foot}"));
        // v again while range-selecting: the unsettled range is dropped, starting over from the highlighted row (this row
        // included)
        select(&mut app, "问");
        keys(&mut app, "vjv");
        assert!(app.anchor.is_some());
        assert_eq!(ticked(&mut app), ["未读"]);
        has(foot(&mut app), &["连选", "已选 1", "Esc 确定"]);
        // Esc: this range is settled (ticked), range select ends
        keys(&mut app, "k");
        press(&mut app, KeyCode::Esc);
        assert!(app.anchor.is_none());
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        // Highlight on a ticked row: Space unticks it; v range-selects, V range-unticks, whether or not this row is ticked
        has(foot(&mut app), &["已选 2", "空格 取消选中", "v 连选", "V 连续取消", "Esc 清空选择"]);
        // Space ticks one and moves down a row; back on it, pressing again unticks it
        select(&mut app, "闲");
        has(foot(&mut app), &["空格 选中", "v 连选"]);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.current().unwrap().title, "错");
        assert_eq!(ticked(&mut app), ["问", "未读", "闲"]);
        keys(&mut app, "k ");
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        // v from a ticked row still range-selects; ticked ones stay ticked
        select(&mut app, "问");
        keys(&mut app, "vj");
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        press(&mut app, KeyCode::Esc);
        // V range-unticks; V or v again while ranging: the unsettled range is dropped, starting over from this row with
        // that kind (V / v)
        select(&mut app, "问");
        keys(&mut app, "Vj");
        assert!(ticked(&mut app).is_empty());
        has(foot(&mut app), &["取消选中", "已选 0", "Esc 确定"]);
        press(&mut app, KeyCode::Char('V'));
        assert_eq!(ticked(&mut app), ["问"]);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        has(foot(&mut app), &["连选", "已选 2"]);
        press(&mut app, KeyCode::Esc);
        // Space while ranging: ticks / unticks this one and moves down, still ranging, the range grows with it
        select(&mut app, "跑");
        keys(&mut app, "v ");
        assert!(app.anchor.is_some());
        assert_eq!(app.current().unwrap().title, "闲");
        assert_eq!(ticked(&mut app), ["问", "未读", "跑", "闲"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(ticked(&mut app), ["问", "未读", "跑", "闲"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking());
        // Not ranging: Esc unselects all, pressing it again exits
        select(&mut app, "问");
        keys(&mut app, "vj");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking() && !app.quit && ticked(&mut app).is_empty());
        // V works with nothing ticked (as in yazi): an untick range, none selected; Esc leaves
        press(&mut app, KeyCode::Char('V'));
        assert!(app.picking());
        has(foot(&mut app), &["取消选中", "已选 0", "Esc 确定"]);
        assert!(!foot(&mut app).contains("d 删除"));
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking() && !app.quit);
        // Space on a group header while multi-selecting: ticks all listed in the group, again unticks them all (outside
        // multi-select Space still jumps)
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Esc);
        let p = app.head_pos("work").unwrap();
        app.select_head(p);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(ticked(&mut app), ["问", "跑", "闲", "错"]);
        app.select_head(p);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(ticked(&mut app), ["问"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking());
        // Ranging up from the group's first one to its header: the one below the header isn't included
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.head.is_some());
        assert_eq!(ticked(&mut app), ["未读"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(ticked(&mut app), ["未读"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking());
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit); // at the bottom, Esc doesn't exit
        keys(&mut app, "qq");
        assert!(app.quit);
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
    fn p_u_and_q_on_the_selection_are_one_step_each() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        let pinned = |app: &App| app.all.iter().filter(|s| s.pinned).map(|s| s.title.clone()).collect::<Vec<_>>();
        let st = |app: &App, t: &str| app.all.iter().find(|s| s.title == t).unwrap().state;
        // p: if any aren't pinned, pin them all
        pin(&mut app, "跑");
        select(&mut app, "闲");
        keys(&mut app, "vj");
        app.msg = None;
        let foot = reader_rows(&mut app, 120, 20).pop().unwrap();
        assert!(foot.contains("p 置顶") && foot.contains("Q 移动到工作区"), "{foot}");
        assert!(!reader_rows(&mut app, 80, 20).pop().unwrap().contains("p 置顶")); // when narrow, the later ones are dropped first
        press(&mut app, KeyCode::Char('p'));
        assert_eq!((pinned(&app), said(&app)), (vec!["跑".to_string(), "闲".into(), "错".into()], "已置顶 2 个"));
        assert!(!app.picking());
        // u: if any are unread, mark them read; idle ones untouched
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('v'));
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!((st(&app, "未读"), st(&app, "闲"), said(&app)), (State::Idle, State::Idle, "已将 1 个标为已读"));
        // Q+key: sends the selected ones there together
        select(&mut app, "问");
        keys(&mut app, "vjQw");
        assert_eq!((space(&app, "问"), space(&app, "未读")), ('w', 'w'));
        assert!(said(&app).starts_with("「问」等 2 个 → "), "{}", said(&app));
        // ^Z goes back one step at a time: sending to a workspace, marking read and pinning are one step each
        ctrl(&mut app, 'z');
        assert_eq!((space(&app, "问"), space(&app, "未读")), ('1', '1'));
        ctrl(&mut app, 'z');
        assert_eq!((st(&app, "未读"), said(&app)), (State::Unread, "已撤销标为已读 1 个"));
        ctrl(&mut app, 'z');
        assert_eq!((pinned(&app), said(&app)), (vec!["跑".to_string()], "已撤销置顶 2 个"));
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

    fn keys(app: &mut App, ks: &str) {
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

    fn life_and_work() -> Vec<Session> {
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
    fn q_switches_capital_q_sends_brackets_cycle_and_h_stays_hidden() {
        let v = life_and_work();
        let mut app = App::new(String::new(), v.clone());
        select(&mut app, "zsh"); // Pressed on a window = sends the session it belongs to
        keys(&mut app, "Ql");
        assert_eq!(titles(&app), ["工作", "生活二", "私"]); // Windows and child sessions went along
        select(&mut app, "私");
        keys(&mut app, "Qh");
        assert_eq!(titles(&app), ["工作", "生活二"]);
        assert!(!top_bar(&app, 80).to_string().contains("未读")); // Other workspaces aren't counted
        press(&mut app, KeyCode::Char('i')); // Nor jumped into
        assert!(app.attach.is_none());
        keys(&mut app, "/生活");
        assert_eq!(titles(&app), ["生活二"]); // Nor found by search
        keys(&mut app, "⎋");
        app.set_sessions(v.clone()); // Same after a refresh
        assert_eq!(titles(&app), ["工作", "生活二"]);
        keys(&mut app, "]"); // Skips h: only 1 and l
        assert_eq!((app.space, titles(&app)), ('l', vec!["zsh".to_string(), "子".into(), "生活".into()]));
        assert!(top_bar(&app, 80).to_string().contains(" l ") && top_bar(&app, 80).to_string().contains("1 未读"));
        keys(&mut app, "]");
        assert_eq!(app.space, '1');
        keys(&mut app, "[");
        assert_eq!(app.space, 'l');
        keys(&mut app, "qh");
        assert_eq!(titles(&app), ["私"]);
        keys(&mut app, "]"); // Leaving h carries on forward
        assert_eq!(app.space, 'l');
        keys(&mut app, "q1");
        assert_eq!(at(&app), "生活二"); // Back in 1: the highlight is on the one it was on when we left
        keys(&mut app, "q⎋"); // Cancel, don't quit
        assert!(!app.quit && app.pending.is_none());
        keys(&mut app, "qq");
        assert!(app.quit);
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
    fn backtick_goes_back_to_the_workspace_before_h_included() {
        let mut app = App::new(String::new(), life_and_work());
        keys(&mut app, "`");
        assert_eq!((app.space, app.msg.as_ref().unwrap().0.as_str()), ('1', "还没有切换过工作区"));
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        select(&mut app, "私");
        keys(&mut app, "Qh");
        select(&mut app, "生活二");
        keys(&mut app, "ql`");
        assert_eq!((app.space, at(&app)), ('1', "生活二".into())); // The highlight comes back to where it was too
        keys(&mut app, "`");
        assert_eq!(app.space, 'l');
        keys(&mut app, "qh`"); // Same for h
        assert_eq!(app.space, 'l');
        keys(&mut app, "`");
        assert_eq!(titles(&app), ["私"]);
        keys(&mut app, "]`"); // Switching with [ ] counts too
        assert_eq!(app.space, 'h');
        // Kept in the view: reopening aca (the popup is a fresh one each time) carries on with it
        let v = app.view_state();
        assert_eq!(v.last_space, Some('l'));
        let mut again = App::new(String::new(), life_and_work()).restore(v);
        keys(&mut again, "`");
        assert_eq!(again.space, 'l');
    }

    #[test]
    fn capital_q_on_a_fold_row_sends_the_whole_group_and_new_ones_land_here() {
        let mut app = App::new(String::new(), life_and_work());
        select(&mut app, "生活二");
        keys(&mut app, "o"); // Fold fun, highlight on the fold row
        assert!(app.on_fold_row());
        keys(&mut app, "Q2");
        assert_eq!(titles(&app), ["工作"]);
        keys(&mut app, "q2o"); // Folds are kept per group, so it's folded over here too
        assert_eq!(titles(&app), ["zsh", "子", "生活", "生活二", "私"]);
        app.adopt("id-新"); // Made in 2
        let mut v = life_and_work();
        v.push(mk("新", "work", State::Idle, 1));
        app.set_sessions(v);
        assert!(titles(&app).contains(&"新".to_string()));
        keys(&mut app, "q1");
        assert_eq!(titles(&app), ["工作"]);
    }

    #[test]
    fn capital_s_lists_workspaces_and_ctrl_r_renames() {
        let mut app = App::new(String::new(), life_and_work());
        assert!(top_bar(&app, 80).to_string().contains(" 全部分组 ")); // Only 1 in use: the label reads all groups
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        select(&mut app, "私");
        keys(&mut app, "Qh");
        keys(&mut app, "S");
        assert_eq!(app.space_list(), ['1', 'l']); // h isn't listed
        assert_eq!(app.space_menu, Some(0));
        keys(&mut app, "↓");
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        keys(&mut app, "生活⏎");
        assert_eq!(app.spaces.name('l'), "生活");
        keys(&mut app, "Sl"); // Switch straight there by key
        assert_eq!(app.space, 'l');
        assert!(top_bar(&app, 80).to_string().contains(" 生活 "));
        keys(&mut app, "Sh"); // Unlisted ones can be pressed directly too
        assert_eq!(titles(&app), ["私"]);
    }

    #[test]
    fn the_theme_list_previews_each_theme_on_the_whole_screen_and_esc_puts_it_back() {
        let mut app = App::new(String::new(), life_and_work());
        let latte = theme::pal().accent;
        // Takes no single key (C is pick something to copy)
        keys(&mut app, "C");
        assert!(app.theme_menu.is_none() && app.modal_open());
        keys(&mut app, "⎋");
        app.open_themes();
        let m = app.theme_menu.as_ref().unwrap();
        assert_eq!(m.list.iter().map(|x| x.0.name.as_str()).collect::<Vec<_>>(), ["latte", "mocha"]);
        assert_eq!(m.sel, 0);
        keys(&mut app, "j");
        let mocha = theme::pal().accent;
        assert_ne!(mocha, latte);
        // The layer below isn't dimmed (the layer below is the preview), the popup sits in the bottom-left corner
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        assert!(!buf.content().iter().any(|c| c.fg == theme::pal().fade));
        let rows = reader_rows(&mut app, 100, 30);
        let y = rows.iter().position(|r| r.contains("配色")).unwrap();
        assert!(y > 10 && rows[y].find("配色").unwrap() < 10, "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("● latte")) && rows.iter().any(|r| r.contains("深色")), "{rows:#?}");
        keys(&mut app, "⎋");
        assert!(app.theme_menu.is_none());
        assert_eq!(theme::pal().accent, latte);
        // ⏎ uses it
        app.open_themes();
        keys(&mut app, "k⏎");
        assert!(app.theme_menu.is_none());
        assert_eq!(theme::pal().accent, mocha);
        assert_eq!(app.msg.as_ref().unwrap().0, "配色:mocha");
    }

    fn space(app: &App, title: &str) -> char {
        app.all.iter().find(|s| s.title == title && s.sub.is_none()).unwrap().space
    }

    /// The whole fun group (on its fold row) sent to h
    fn fun_in_h() -> App {
        let mut app = App::new(String::new(), life_and_work());
        select(&mut app, "生活二");
        keys(&mut app, "oQh");
        app.folded.clear();
        app.rebuild();
        app
    }

    #[test]
    fn a_group_sent_to_h_takes_sessions_opened_later_along() {
        let mut app = fun_in_h();
        assert_eq!(titles(&app), ["工作"]);
        assert_eq!(app.spaces.group_of("default", "fun"), Some('h'));
        assert_eq!(app.spaces.of("id-生活"), None); // Following the group is enough, no need to record each one
        // Opened in fun without the interface (aca new in a terminal, a plugin): follows the group into h, doesn't show up in 1
        let mut v = life_and_work();
        v.push(mk("外面开的", "fun", State::Unread, 1));
        app.set_sessions(v);
        assert_eq!(titles(&app), ["工作"]);
        assert!(!top_bar(&app, 80).to_string().contains("未读"));
        assert_eq!(space(&app, "外面开的"), 'h');
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
    fn deck_groups() -> Vec<groups::Row> {
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
    fn fun_binned() -> Outcome {
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

    fn without_fun() -> Vec<Session> {
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

    #[test]
    fn moving_into_a_group_kept_in_h_does_not_hide_the_session() {
        let mut app = fun_in_h().with_group_hints(hints);
        select(&mut app, "工作");
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(Some("fun".into())));
        assert!(matches!(app.job.take(), Some(Job::Move { .. })));
        let mut v = life_and_work();
        v[0].group = "fun".into(); // Moved
        app.set_sessions(v);
        assert_eq!(space(&app, "工作"), '1');
    }

    #[test]
    fn keyword_lookup_and_fzf_rows_only_see_this_workspace() {
        let mut app = fun_in_h();
        assert!(app.matching("生活").is_empty());
        assert_eq!(app.matching("工作").len(), 1);
        keys(&mut app, "qh");
        assert_eq!(app.matching("生活").iter().filter(|s| s.sub.is_none()).count(), 2);
        // N's fzf: sessions in h don't show their titles among 1's candidates
        keys(&mut app, "q1");
        let mut app = app.with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new())] });
        press(&mut app, KeyCode::Char('N'));
        let p = app.dir_pick_from(vec![("/p/fun".into(), Src::Known), ("/p/work".into(), Src::Known)], 40, 16).unwrap();
        assert!(p.rows[0].sessions.is_empty() && p.rows[0].mark == '◇', "{:?}", p.rows[0].sessions);
        assert!(p.rows[1].sessions.iter().any(|(_, t)| t == "工作"));
    }

    fn at(app: &App) -> String {
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
    fn shift_tab_goes_back_to_the_group_seen_before() {
        let mut app = fixture();
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.msg.as_ref().unwrap().0, "还没有切换过分组");
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(app.group.as_deref(), Some("work"));
        press(&mut app, KeyCode::BackTab); // Back to the previous one (all), press again to come back
        assert_eq!(app.group, None);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.group.as_deref(), Some("work"));
    }

    #[test]
    fn h_folds_from_a_session_or_header_and_l_unfolds_then_steps_in() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // All: fun[问 未读] work[跑 闲 错]
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('h')); // On a session = fold its group right away, highlight lands on the fold row
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[▸ work  3]"]);
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('h')); // The fold row is the outermost level: no move, no group change
        assert_eq!((layout(&app).len(), app.group.as_deref()), (4, None));
        press(&mut app, KeyCode::Right); // → same as l: unfolds, stops on the header
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        assert_eq!(app.group_row(), Some("work"));
        press(&mut app, KeyCode::Left); // ← / h on an unfolded header folds it too
        assert!(app.on_fold_row());
        keys(&mut app, "ll"); // Unfold, into the group's first one
        assert_eq!((app.group_row(), at(&app)), (None, "跑".to_string()));
        press(&mut app, KeyCode::Char('l')); // No move on a session (only h folds)
        assert_eq!((app.folds.len(), app.group.as_deref(), at(&app)), (0, None, "跑".to_string()));
        pin(&mut app, "跑"); // The pinned block isn't in a section: h does nothing
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.folds.len(), at(&app)), (0, "跑".to_string()));
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
    fn c_copies_last_response_and_shift_b_the_session_info() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('c'));
        match app.clip.take() {
            Some(Clip::Last { profile, id, title, .. }) => {
                assert_eq!((profile.as_str(), id.as_str(), title.as_str()), ("default", "id-跑", "跑"))
            }
            other => panic!("{other:?}"),
        }
        press(&mut app, KeyCode::Char('B'));
        // Then the config dir Claude uses (this machine's default / account): only that it's there
        match app.clip.take() {
            Some(Clip::Info { title, text }) => {
                let v: Vec<&str> = text.lines().collect();
                assert_eq!((title.as_str(), v[0], v.len()), ("跑", "Path: /p/work", 2));
                assert!(v[1].starts_with("Home: /"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // C: pick something to copy; this row has no conversation id, so s only says there is none
        press(&mut app, KeyCode::Char('C'));
        assert!(app.modal_open());
        press(&mut app, KeyCode::Char('s'));
        assert!(app.modal_open() && app.clip.is_none());
        press(&mut app, KeyCode::Char('p'));
        assert_eq!((app.modal_open(), app.clip.take()), (false, Some(Clip::Text("/p/work".into()))));
        press(&mut app, KeyCode::Char('z')); // Copying works in full-screen reading too
        press(&mut app, KeyCode::Char('c'));
        assert!(matches!(app.clip.take(), Some(Clip::Last { .. })));
    }

    #[test]
    fn jump_hints_follow_agent_decks_algorithm() {
        assert_eq!(jump_hints(1), ["s"]);
        assert_eq!(jump_hints(5), ["a", "d", "f", "j", "s"]);
        assert!(jump_hints(13).iter().all(|h| h.len() == 1));
        for n in [20, 60, 200] {
            let h = jump_hints(n);
            assert_eq!(h.len(), n);
            let uniq: std::collections::HashSet<&String> = h.iter().collect();
            assert_eq!(uniq.len(), n);
            assert!(h.iter().all(|x| x.chars().all(|c| HINT_CHARS.contains(c))));
            // None is the start of another, or pressing halfway through would hit the wrong one
            assert!(h.iter().all(|x| h.iter().filter(|y| y.starts_with(x.as_str())).count() == 1));
        }
        let h = jump_hints(20);
        assert!(h.iter().any(|x| x.len() == 1) && h.iter().any(|x| x.len() == 2));
    }

    #[test]
    fn space_then_letter_enters_that_session() {
        let mut app = fixture(); // 1 day: [fun] 问 [work] 跑 闲 → hints a d f j s
        press(&mut app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), &Folds::new(), 0, false);
        let badges: Vec<&str> = rows.iter().filter(|r| r.pos.is_some()).map(|r| r.lines[0][0].0.as_str()).collect();
        assert_eq!(badges, ["d", "j", "s"]); // Hints take the place of the numbers
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        assert!(app.jump.is_none());
        // Esc / a letter that matches nothing: leave, don't enter
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Esc);
        assert!(app.jump.is_none() && !app.quit);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('z'));
        assert!(app.jump.is_none() && app.attach.is_none());
        // Any other key: leave the jump and handle the key as usual
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('3'));
        assert!(app.jump.is_none());
        assert_eq!(at(&app), "闲");
    }

    #[test]
    fn two_letter_hints_wait_for_the_second_letter() {
        let v: Vec<Session> = (0..20).map(|i| mk(&format!("会话{i:02}"), "work", State::Idle, 10)).collect();
        let mut app = App::new(String::new(), v);
        let hints = jump_hints(21); // The group header takes the first
        let (i, two) = hints.iter().enumerate().find(|(i, h)| *i > 0 && h.len() == 2).unwrap();
        let want = app.visible().0[i - 1].name.clone();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(two.chars().next().unwrap()));
        assert!(app.jump.is_some() && app.attach.is_none());
        press(&mut app, KeyCode::Char(two.chars().nth(1).unwrap()));
        assert_eq!(app.attach, Some(want));
    }

    /// After Space, press a group header's letter: the highlight stops on the header, doesn't enter a session
    fn jump_to_group(app: &mut App, group: &str) {
        press(app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), app.fold_info(), 0, false);
        let pos = view.iter().position(|s| s.group == group).unwrap();
        let row = rows.iter().find(|r| r.head == Some(pos)).unwrap();
        let hint: String = row.lines[0][0].0.trim().into();
        hint.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    #[test]
    fn group_headers_get_letters_and_the_highlight_stops_on_them() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), &Folds::new(), 0, false);
        let heads: Vec<String> =
            rows.iter().filter(|r| r.head.is_some()).map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert_eq!(heads, ["a ▾ fun  1", "f ▾ work  2"]);
        press(&mut app, KeyCode::Char('f'));
        assert!(app.jump.is_none() && app.attach.is_none());
        assert_eq!(app.head.as_deref(), Some("work"));
        assert_eq!(at(&app), "跑"); // Behind it, the group's first one is selected
        // Drawn: highlight on the header row
        let mut t = Terminal::new(TestBackend::new(46, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let bg = |y: u16| buf[(4, y)].bg;
        // Row 0 top bar, 1 [fun], 2 问, 3 [work], 4 跑
        assert_eq!((bg(3), bg(4) == theme::pal().sel), (theme::pal().sel, false));
        // Still on the header after a refresh
        app.set_sessions(sessions());
        assert_eq!(app.head.as_deref(), Some("work"));
        // Showing only one group, there's no header to stop on
        press(&mut app, KeyCode::Char('W'));
        assert!(app.head.is_none());
        // The topmost group's header can be stopped on too; without sections no header gets a letter
        press(&mut app, KeyCode::Char('W'));
        jump_to_group(&mut app, "fun");
        assert_eq!(app.head.as_deref(), Some("fun"));
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
    fn group_pane_shows_the_path_where_it_came_from_and_whats_inside() {
        let text = |l: &[Line]| {
            l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect::<Vec<_>>()
        };
        let sp = Spot { group: "work".into(), dir: "/p/work".into(), from: None };
        // Not read back yet: path only (the group's own default path gets no extra note)
        assert_eq!(text(&group_pane(&sp, None, None, false, 60, 20)), ["/p/work"]);
        let files = diff::Files {
            dirs: vec!["src".into(), "spec".into()],
            files: vec!["Cargo.toml".into(), "README.md".into(), "a".into()],
            hidden: 2,
            err: None,
        };
        let g = Glance {
            repo: Some(diff::Stat { branch: "main".into(), files: 3, add: 12, del: 4 }),
            files: Some(files),
            ..Default::default()
        };
        let t = text(&group_pane(&sp, Some(&g), None, false, 60, 20));
        assert_eq!(t[0], "/p/work  main  3 个文件 +12 −4");
        // Column width = the widest, Cargo.toml's 10 cells, + 2: 60 cells fit 5 columns, directories first; dotfiles neither listed
        // nor counted
        assert_eq!(t[1..], ["", "src/        spec/       Cargo.toml  README.md   a"]);
        // Doesn't fit: the last line counts how many more
        let t = text(&group_pane(&sp, Some(&g), None, false, 24, 4));
        assert_eq!(t[2..], ["src/        spec/", "还有 3 个"]);
        // Following a session's directory; the directory is gone
        let sp = Spot { from: Some("跑".into()), ..sp };
        let gone =
            Glance { files: Some(diff::Files { err: Some("目录已不存在".into()), ..Default::default() }), ..Default::default() };
        assert_eq!(text(&group_pane(&sp, Some(&gone), None, false, 60, 20)), ["/p/work", "「跑」的目录", "", "目录已不存在"]);
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
    fn t_on_a_group_opens_a_terminal_in_the_groups_dir() {
        let mut v = sessions();
        v[3].dir = "/p/闲".into(); // The most recently active one in work is 「跑」
        let mut app = App::new(String::new(), v);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        let r = app.term_req.take().unwrap();
        assert_eq!((r.key.as_str(), r.family.as_str(), r.title.as_str(), r.w.dir.as_str()), ("group:work", "", "work", "/p/work"));
        assert_eq!(app.head.as_deref(), Some("work"));
        // Equally recent: take the one higher in the list
        let mut v = sessions();
        (v[3].active_at, v[3].dir) = (v[2].active_at, "/p/闲".into());
        let mut t = App::new(String::new(), v);
        jump_to_group(&mut t, "work");
        press(&mut t, KeyCode::Char('t'));
        assert_eq!(t.term_req.take().unwrap().w.dir, "/p/work");
        // Opened before: goes back to that one
        app.terms.insert(r.key.clone(), crate::term::Term::spawn("/", &[], 5, 20).unwrap());
        press(&mut app, KeyCode::Char('t'));
        assert!(app.term_req.is_none() && app.term_on.as_deref() == Some("group:work"));
        app.terms.clear();
        app.term_on = None;
        // The group has a default path (directory still there): use it
        let tmp = || std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints {
            groups: vec![("work".into(), std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string())],
        });
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().w.dir, tmp());
        // Default path gone: fall back to the session's directory; a worktree uses its original repository
        let mut v = sessions();
        (v[2].worktree, v[2].info) = (true, "Repo: /repo\nPath: /p/work".into()); // 跑 (running)
        let mut app = App::new(String::new(), v)
            .with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), "/没有这个目录".into())] });
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().w.dir, "/repo");
        // A fold row is a group too
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().key, "group:work");
    }

    #[test]
    fn new_on_a_group_starts_in_the_groups_dir_and_group() {
        let mut v = sessions();
        v[2].tool = "codex".into(); // 跑: the most recently active one in work
        v[2].account = "team".into();
        v[3].dir = "/p/闲".into();
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('N'));
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.text.as_str(), d.group()), ("/p/work", ("work".into(), false)));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.tool.as_str(), r.account.as_str()), ("/p/work", "work", "codex", "team"));
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

    #[test]
    fn account_column_sits_before_the_age_with_a_fixed_width() {
        let mut v = sessions();
        v[2].who = "team".into(); // 跑 (running)
        v[4].who = "alt".into(); // 错 (error)
        v[4].agent = Agent::Codex;
        let text = |app: &App| -> Vec<String> {
            let view = app.visible().0;
            list_rows(&view, app.sectioned(), 46, None, app.fold_info(), app.who_width(), false)
                .iter()
                .filter(|r| r.pos.is_some())
                .map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect::<String>())
                .collect()
        };
        let mut app = App::new(String::new(), v.clone());
        assert_eq!(app.who_width(), 4);
        let rows = text(&app);
        assert!(rows.iter().all(|r| r.width() == 46), "{rows:?}");
        let run = rows.iter().find(|r| r.contains("跑")).unwrap();
        assert!(run.ends_with(" team  5秒 "), "{run}"); // 5秒 right-aligned to 4 cells
        // In English the column is 3 cells
        let en = i18n::with(Lang::En, || text(&app));
        assert!(en.iter().all(|r| r.width() == 46) && en.iter().any(|r| r.ends_with(" team  5s ")), "{en:?}");
        // Only sessions without an account left on show: the column width stays (computed over all sessions), titles don't shift
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["未读", "问"]);
        assert!(text(&app)[0].ends_with(&format!("{}11天 ", " ".repeat(2 + 1 + 4 + 1))), "{:?}", text(&app)); // Mark col + empty account col
        // Too long is cut; when no session has an account the column takes no space
        v[2].who = "a-very-long-slot".into();
        assert_eq!(App::new(String::new(), v.clone()).who_width(), 8);
        assert_eq!(fixture().who_width(), 0);
    }

    fn layout(app: &App) -> Vec<String> {
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
    fn shift_o_folds_all_pins_stay_and_filter_unfolds() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(layout(&app), ["[置顶]", "跑", "[▸ fun  2 ◆1]", "[▸ work  2]"]); // Pinned ones aren't in sections
        let saved = app.view_state();
        assert_eq!(saved.folded, ["fun", "work"]);
        assert_eq!(fixture().restore(saved).folds.len(), 2); // Remembered
        typ(&mut app, "/错"); // Filtering unfolds everything, so search misses nothing
        assert_eq!(order(&app), ["错"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(layout(&app).len(), 8);
        press(&mut app, KeyCode::Char('}')); // Only one group: no sections to fold
        press(&mut app, KeyCode::Char('o'));
        assert!(app.folds.is_empty());
    }

    #[test]
    fn sections_get_a_header_each_and_pins_sit_above_them() {
        let mut app = fixture();
        assert_eq!(layout(&app), ["[fun]", "问", "[work]", "跑", "闲"]);
        // Header: the group name followed by how many this section has
        let view = app.visible().0;
        let head: String =
            list_rows(&view, true, 30, None, app.fold_info(), 0, false)[2].lines[0].iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(head, " ▾ work  2");
        // On desktop it's one session per row too, but always numbered, with a blank line between groups (the blank line is an
        // item too, not selectable)
        let desk = list_rows(&view, true, 46, None, app.fold_info(), 0, true);
        assert_eq!(desk.iter().map(|r| r.lines.len()).collect::<Vec<_>>(), [1, 1, 1, 1, 1, 1]);
        assert!(desk[2].pos.is_none() && desk[2].lines[0].is_empty());
        assert_eq!(desk[1].lines[0][0].0, "  1 ");
        pin(&mut app, "闲");
        assert_eq!(layout(&app), ["[置顶]", "闲", "[fun]", "问", "[work]", "跑"]);
        pin(&mut app, "问"); // fun has only the pinned one left; an empty section draws no header
        assert_eq!(layout(&app), ["[置顶]", "闲", "问", "[work]", "跑"]);
        press(&mut app, KeyCode::Char('}')); // Only one group: no sections
        assert_eq!(layout(&app), ["问"]);
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

    fn sess(state: State, screen: &[&str]) -> Session {
        Session {
            name: "s".into(),
            tmux: "$1".into(),
            title: "t".into(),
            locked: false,
            group: "g".into(),
            state,
            question: String::new(),
            idle_for: 0,
            active_at: 0,
            ran: 0,
            attached: false,
            screen: screen.iter().map(|s| s.to_string()).collect(),
            pinned: false,
            noted: false,
            group_noted: false,
            space: spaces::HOME,
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
    fn preview_cuts_input_box_even_with_panels_below() {
        let bar = "─".repeat(60);
        let mut screen = vec!["⏺ 结果", "· Choreographing… (9m 10s)", "  ⎿  Done", "", &bar, "❯\u{a0}", &bar];
        let panels = [
            "  ━━╌╌ 50% │ 模型",
            "  $1 100m",
            "  ⏵⏵ bypass permissions on",
            "  ◯ task 1",
            "  ◯ task 2",
            "  ◯ task 3",
            "  ◯ task 4",
            "  ◯ task 5",
            "  ◯ task 6",
        ];
        screen.extend(panels);
        let got = preview_lines(&sess(State::Running, &screen));
        assert_eq!(ansi::plain(got.last().unwrap()), "  ⎿  Done");
    }

    #[test]
    fn preview_cuts_input_box_with_session_name_in_border() {
        let top = format!("{} 项目-主会话 ─", "─".repeat(80));
        let bar = "─".repeat(90);
        let screen = ["⏺ 结果", "· Swooping… (14m 28s)", "", top.as_str(), "❯\u{a0}", bar.as_str(), "  ━━╌ 50%"];
        assert_eq!(ansi::plain(preview_lines(&sess(State::Running, &screen)).last().unwrap()), "· Swooping… (14m 28s)");
    }

    #[test]
    fn preview_keeps_dialog_when_asking() {
        let screen = ["问题?", "❯ 1. 是", "  2. 否", "Enter to select"];
        assert_eq!(preview_lines(&sess(State::Ask, &screen)).len(), 4);
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
    fn shows(rows: &[String], wants: &[&str]) {
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

    /// Open the record of 「跑」 and feed it the test transcript (no background thread)
    fn trace_app(name: &str) -> (App, std::path::PathBuf) {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        assert!(app.trace.is_some());
        let s = app.trace_request().unwrap();
        assert_eq!(s.name, "跑");
        assert!(app.trace_request().is_none(), "只读一次");
        let (t, p) = crate::trace_claude::tests::fixture(&format!("ui-{name}"));
        app.set_trace("跑", Ok(t));
        // Keys go by the last draw: in real use it's drawn once after loading before any key comes in
        reader_rows(&mut app, 120, 30);
        (app, p)
    }

    /// The two desktop panes: the left (list) and the right (right of the divider)
    fn trace_panes(app: &mut App, w: u16, h: u16) -> (Vec<String>, Vec<String>) {
        let rows = reader_rows(app, w, h);
        let split = |r: &String| r.split_once('│').map(|(a, b)| (a.to_string(), b.to_string()));
        let body: Vec<(String, String)> = rows[1..rows.len() - 1].iter().filter_map(split).collect();
        (body.iter().map(|x| x.0.clone()).collect(), body.iter().map(|x| x.1.clone()).collect())
    }

    /// What's selected in the pane taking keys: a turn gives its prompt's first line, a call the first thing it said, a tool its
    /// summary (or its name if none)
    fn trace_sel(app: &App) -> String {
        let v = app.trace.as_ref().unwrap();
        let t = v.tr().unwrap();
        let n = &t.nodes[if v.in_steps { v.steps.sel } else { v.chat.sel }];
        if n.label.is_empty() { n.name.clone() } else { n.label.clone() }
    }

    #[test]
    fn enter_on_a_subagent_step_opens_its_own_record_and_esc_comes_back_to_that_step() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        let (p, _, later) = crate::trace_claude::tests::with_subs("ui-subs");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            use std::io::Write;
            writeln!(f, "{l}").unwrap();
        }
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        reader_rows(&mut app, 120, 30);
        // In the first turn's steps, stop on the step that starts Explore
        press(&mut app, KeyCode::Char('p'));
        reader_rows(&mut app, 120, 30);
        press(&mut app, KeyCode::Right);
        reader_rows(&mut app, 120, 30);
        for _ in 0..6 {
            if trace_sel(&app) == "Explore · 找 width" {
                break;
            }
            press(&mut app, KeyCode::Down);
            reader_rows(&mut app, 120, 30);
        }
        assert_eq!(trace_sel(&app), "Explore · 找 width");
        let (_, right) = trace_panes(&mut app, 120, 30);
        shows(&right, &["▸ Agent Explore · 找 width", "3 调用 · $", "找到 4 处:", "… 还有 2 行"]);
        // ⏎: its own record, two panes as well; the left is the task it was given and what it said last, the right its steps
        // (where it starts another one)
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        let (left, right) = trace_panes(&mut app, 120, 30);
        shows(&left, &["任务  找出所有 width( 的调用", "Claude  找到 4 处:"]);
        shows(&right, &["· Agent general-purpose · 细看 ui.rs", "ui.rs 里有 2 处", "Bash  rg -n 'width\\('"]);
        // One level deeper, then Esc back level by level, to the step that started it
        for _ in 0..6 {
            if trace_sel(&app) == "general-purpose · 细看 ui.rs" {
                break;
            }
            press(&mut app, KeyCode::Up);
            reader_rows(&mut app, 120, 30);
        }
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · general-purpose · 细看 ui.rs"), "{}", rows[0]);
        for _ in 0..2 {
            press(&mut app, KeyCode::Esc);
            reader_rows(&mut app, 120, 30);
            press(&mut app, KeyCode::Esc);
            reader_rows(&mut app, 120, 30);
        }
        let v = app.trace.as_ref().unwrap();
        assert!(v.path.is_empty() && v.in_steps);
        assert_eq!(trace_sel(&app), "Explore · 找 width");
        // Phone width: entering a subagent also shows its steps full screen
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 46, 30);
        assert!(rows.iter().any(|r| r.contains("· Agent general-purpose")), "{rows:#?}");
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
    fn with_agents() -> Vec<Session> {
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

    #[test]
    fn enter_on_a_subagent_row_opens_the_record_at_that_subagent() {
        let mut app = App::new(String::new(), with_agents());
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        // ⏎: doesn't enter a session (it has no pane), opens the session's record
        press(&mut app, KeyCode::Enter);
        assert!(app.attach.is_none() && app.start.is_none());
        assert_eq!(app.trace_request().map(|s| s.title), Some("跑".to_string()));
        let (p, _, _) = crate::trace_claude::tests::with_subs("ui-agent-row");
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p.clone(), Agent::Claude, 0).unwrap()));
        // Once loaded, it's at that subagent's level
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        assert_eq!(app.trace.as_ref().unwrap().path.len(), 1);
        let (left, right) = trace_panes(&mut app, 120, 30);
        shows(&left, &["任务  找出所有 width( 的调用", "Claude  找到 4 处:"]);
        shows(&right, &["Bash  rg -n 'width\\('"]);
        // One Esc goes back to the list, highlight still on that row
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none());
        assert_eq!(app.current().unwrap().title, "Explore · 找 width");
        // The nested one: two levels in at once. Z is the same as ⏎
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · general-purpose · 细看 ui.rs"), "{}", rows[0]);
        assert_eq!(app.trace.as_ref().unwrap().path.len(), 2);
        // ← up one level: the subagent that started it (on its steps on the right). Esc from there goes the usual way: first to the
        // left, then out to the session's own record, then back to the list
        press(&mut app, KeyCode::Left);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        for depth in [1, 0] {
            press(&mut app, KeyCode::Esc);
            reader_rows(&mut app, 120, 30);
            assert_eq!(app.trace.as_ref().map(|v| v.path.len()), Some(depth));
        }
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none());
    }

    #[test]
    fn shift_z_shows_the_chat_on_the_left_and_the_turns_steps_on_the_right() {
        let (mut app, _) = trace_app("shows");
        let (left, right) = trace_panes(&mut app, 120, 30);
        // That turn in the record is in the 01:00 UTC minute; drawn in local time (09:00 in UTC+8)
        let numbers = format!("{} · 20.0s · $0.03", crate::trace::clock(3_600_000, crate::usage::local_offset(), false));
        // The left is the conversation: what you said, what Claude said last (Markdown laid out), a line of numbers; what it said in
        // between isn't here
        shows(
            &left,
            &[
                "上一段 · 1 轮 · 不计",
                "你  看看两栏为什么对不齐",
                "Claude  改好了:len() 数的是字节",
                "• 换成按显示宽度算的 width()",
                &numbers,
                "── 压缩 97k → 12k · 21.0s",
                "你  再试一次",
                "✕ 出错",
                "你  跑一下测试",
                "● 运行中",
            ],
        );
        assert!(!left.iter().any(|r| r.contains("先看 bill.rs")), "中间说的在右边:{left:#?}");
        // The right is the steps of the selected turn (following the latest = the running turn)
        shows(
            &right,
            &["1 调用 · 2 工具", "── 4.1s", "▸ Read  src/usage.rs", "5 行", "· Bash  cargo build --release", "运行中 · ", "● 运行中…"],
        );
        shows(&reader_rows(&mut app, 120, 30), &["记录 · 跑", "3 轮", "→ 查看过程"]);
        // Keys on the left: the first line of the selected block on the left gets a pale purple background, the right gets none
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        let rows = reader_rows(&mut app, 120, 30);
        let y = rows.iter().position(|r| r.contains("你  跑一下测试")).unwrap() as u16;
        assert_eq!(buf[(1, y)].bg, theme::pal().sel);
        assert!((0..30u16).all(|y| buf[(100, y)].bg != theme::pal().sel), "右边没在按,不铺底");
        for y in 0..30u16 {
            for x in 0..120u16 {
                let c = &buf[(x, y)];
                assert!(!c.modifier.contains(Modifier::DIM), "({x},{y}) 用了 DIM");
                assert!(!matches!(c.fg, Color::Yellow | Color::Gray | Color::White | Color::DarkGray), "({x},{y}) {:?}", c.fg);
            }
        }
        // Wrapped at every width, it all fits
        let v = app.trace.as_ref().unwrap();
        let tr = v.t.as_ref().unwrap();
        for w in [20, 34, 43, 60, 80] {
            for &r in tr.roots.iter().filter(|&&r| tr.nodes[r].kind == crate::trace::Kind::Turn) {
                let c = crate::trace::chat(tr, r, true, 0);
                let steps: Vec<crate::trace::Para> =
                    crate::trace::steps(tr, r, true, 0, &|_| false).into_iter().flat_map(|x| x.paras).collect();
                for l in [c.asked, c.reply, vec![c.foot], steps].iter().flat_map(|ps| crate::trace::wrap(ps, w)) {
                    assert!(crate::ansi::plain(&l).width() <= w, "{w} 宽折不下:{}", crate::ansi::plain(&l));
                }
            }
        }
        // On a phone the title and the cost still show
        shows(&reader_rows(&mut app, 46, 20), &["记录 · 跑", "3 轮 · $"]);
    }

    #[test]
    fn shift_z_keys_move_through_turns_step_into_the_process_and_back() {
        let (mut app, _) = trace_app("keys");
        // p: previous turn (doesn't stop on the compaction line)
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "再试一次");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        // The right switches to this turn's steps: each call's numbers, thinking, what it said in between, tools with their diffs and
        // the start of their output; the reply is on the left, only one line of it here
        let right = trace_panes(&mut app, 120, 40).1;
        shows(
            &right,
            &[
                "4 调用 · 3 工具",
                "── 4.0s · 1.1k · $0.01 ──",
                "思考 先确认是不是宽字符的问题",
                "先看 bill.rs 的列宽",
                "▸ Grep  width(",
                "src/bill.rs:12:",
                "… 还有 1 行",
                "▸ Edit  src/bill.rs",
                "+2 -2",
                "12 -     let w = s.len();",
                "12 +     let w = s.width();",
                "running 220 tests",
                "回复 改好了:len() 数的是字节",
            ],
        );
        assert!(!right.iter().any(|r| r.contains("• 测试全过了")), "回复全文在左边:{right:#?}");
        // → into the right pane, step by step; ⏎ shows this step full screen
        press(&mut app, KeyCode::Right);
        assert!(app.trace.as_ref().unwrap().in_steps);
        shows(&reader_rows(&mut app, 120, 20), &["← Esc 返回对话"]);
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "先看 bill.rs 的列宽");
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "width(");
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 100, 24), &["Grep · 第 1 轮", "── 输入 ──", "── 输出 ──", "src/ui.rs:90:"]);
        // n in full screen: the next step
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 24), &["第 1 轮 · 第 2 次调用", " · 缓存写 100 · ", "── 调用了 ──", "▸ Edit  src/bill.rs"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(
            &reader_rows(&mut app, 100, 24),
            &["Edit · 第 1 轮", "── 改动 ──", "11   fn col(s: &str)", "12 +     let w = s.width();", "┈┈┈┈┈", "41 +     pad(w.max(1))"],
        );
        press(&mut app, KeyCode::Esc);
        // The first call full screen: thinking first, then what it said, tokens listed separately
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(
            &reader_rows(&mut app, 100, 30),
            &["第 1 轮 · 第 1 次调用", "claude-opus-5-5", "── 思考 ──", "中文一个字占两格", "── 回复 ──", "── 调用了 ──"],
        );
        press(&mut app, KeyCode::Esc);
        // n / p on the right change turns, staying on the right, from the top
        press(&mut app, KeyCode::Char('n'));
        assert!(app.trace.as_ref().unwrap().in_steps);
        assert_eq!(trace_sel(&app), "再试一次");
        shows(&trace_panes(&mut app, 120, 20).1, &["出错", "529 server_error"]);
        // ← / Esc back to the conversation; only another Esc goes back to the list
        press(&mut app, KeyCode::Esc);
        assert!(!app.trace.as_ref().unwrap().in_steps);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Left);
        assert!(app.trace.is_some() && !app.trace.as_ref().unwrap().in_steps);
        // Tab shows the output, Z comes back; Esc back to the list
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_none() && app.reader.is_some());
        press(&mut app, KeyCode::Char('Z'));
        assert!(app.trace.is_some() && app.reader.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none() && app.reader.is_none() && !app.quit);
    }

    #[test]
    fn shift_z_long_messages_show_their_head_and_o_shows_the_rest() {
        use crate::trace_claude::tests::{file, prompt, reply, turn_end};
        use serde_json::json;
        let ask: String = (1..=20).map(|i| format!("第 {i} 行问题\n")).collect();
        let answer: String = (1..=30).map(|i| format!("第 {i} 行回答\n")).collect();
        let p = file(
            "ui-long",
            &[
                prompt("u1", None, 0, &ask),
                reply("a1", "u1", 1000, "m1", json!({"type": "text", "text": answer}), 9, Some("end_turn")),
                turn_end("a1", 1100, 1000),
            ],
        );
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, crate::data::Agent::Claude, 0).unwrap()));
        let left = trace_panes(&mut app, 120, 40).0;
        shows(&left, &["你  第 1 行问题", "第 6 行问题", "… 还有 14 行", "Claude  第 1 行回答", "第 12 行回答", "… 还有 18 行"]);
        assert!(!left.iter().any(|r| r.contains("第 7 行问题")));
        press(&mut app, KeyCode::Char('o'));
        let rows = reader_rows(&mut app, 120, 80);
        shows(&rows, &["第 20 行问题", "第 30 行回答"]);
        assert!(!rows.iter().any(|r| r.contains("… 还有")));
    }

    #[test]
    fn shift_z_o_opens_and_closes_a_step_and_shift_o_the_whole_turn() {
        let (mut app, _) = trace_app("fold");
        // Keys go by the last draw: draw after each one, as in real use
        let key = |app: &mut App, k: KeyCode| {
            press(app, k);
            trace_panes(app, 120, 50)
        };
        key(&mut app, KeyCode::Char('p'));
        key(&mut app, KeyCode::Char('p'));
        // Left: a turn that's already in full, o says so
        key(&mut app, KeyCode::Char('o'));
        assert!(app.msg.is_none());
        // Edit selected on the right: folded, only the changed lines
        key(&mut app, KeyCode::Right);
        for _ in 0..4 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(trace_sel(&app), "src/bill.rs");
        let right = trace_panes(&mut app, 120, 50).1;
        shows(&right, &["▸ Edit  src/bill.rs", "12 -     let w = s.len();"]);
        assert!(!right.iter().any(|r| r.contains("fn col(s: &str)")), "{right:#?}");
        // o: with context and a dotted line between the two hunks; o again folds it
        let right = key(&mut app, KeyCode::Char('o')).1;
        shows(&right, &["▾ Edit  src/bill.rs", "11   fn col(s: &str) -> usize {", "┈┈┈┈┈", "42   }"]);
        let right = key(&mut app, KeyCode::Char('o')).1;
        assert!(!right.iter().any(|r| r.contains("fn col(s: &str)")), "{right:#?}");
        // A step with nothing else (the call with only numbers): says so
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('o'));
        assert!(app.msg.is_none());
        // O: unfold the whole turn (all the thinking, every search hit, the whole command and all output), O again folds everything
        let right = key(&mut app, KeyCode::Char('O')).1;
        shows(
            &right,
            &[
                "中文一个字占两格",
                "▾ Grep",
                "src/ui.rs:90:",
                "▾ Edit",
                "fn col(s: &str)",
                "$ cargo test --release",
                "test result: ok. 220 passed",
            ],
        );
        let right = key(&mut app, KeyCode::Char('O')).1;
        shows(&right, &["▸ Grep", "▸ Edit", "▸ Bash", "… 还有 3 行"]);
        // Short thinking: in full even folded, flattened to one line
        shows(&right, &["思考 先确认是不是宽字符的问题。中文一个字占两格"]);
    }

    #[test]
    fn shift_z_e_opens_the_selected_step_read_only_in_the_editor() {
        let (mut app, _) = trace_app("view");
        let key = |app: &mut App, k: KeyCode| {
            press(app, k);
            trace_panes(app, 120, 50)
        };
        key(&mut app, KeyCode::Char('p'));
        key(&mut app, KeyCode::Char('p'));
        // Bash selected on the right (the record's files aren't on this machine): written to a temporary Markdown file, deleted
        // after viewing
        key(&mut app, KeyCode::Right);
        for _ in 0..6 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(trace_sel(&app), "cargo test --release");
        key(&mut app, KeyCode::Char('e'));
        let Some(External::View { path, line: 1, temp: true }) = app.external.take() else { panic!("{:?}", app.msg) };
        assert!(path.ends_with("/aca-trace-view/Bash-第1轮.md"), "{path}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Bash · cargo test --release") && text.contains("test result: ok. 220 passed"), "{text}");
        let _ = std::fs::remove_file(&path);
        // e works in full screen too
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::View { temp: true, .. })));
        key(&mut app, KeyCode::Esc);
        // Left: a turn shows the whole turn; the compaction line has nothing to view
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('e'));
        let Some(External::View { path, .. }) = app.external.take() else { panic!() };
        assert!(std::fs::read_to_string(&path).unwrap().contains("## 你\n\n看看两栏为什么对不齐"));
        let _ = std::fs::remove_file(&path);
        key(&mut app, KeyCode::Char('n'));
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('e'));
        assert!(app.external.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "此行没有可查看的内容");
    }

    #[test]
    fn shift_z_colors_only_what_is_on_screen() {
        let (mut app, _) = trace_app("color");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        // Short screen: the right shows only the top few blocks, the ones below aren't colored yet
        reader_rows(&mut app, 120, 12);
        let v = app.trace.as_ref().unwrap();
        let c = v.steps_cache.as_ref().unwrap();
        let (top, h) = (v.steps.top, v.steps.view_h);
        for (b, &(_, s, e)) in c.blocks.iter().zip(&v.steps.items) {
            let seen = e > top && s < top + h;
            assert_eq!(b.colored > 0, seen, "块 {} 在 {s}..{e},屏幕 {top}..{}", b.node, top + h);
        }
        // Tall screen: Edit is in view, and on screen let has the keyword color
        let mut term = Terminal::new(TestBackend::new(120, 50)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = reader_rows(&mut app, 120, 50);
        let y = rows.iter().position(|r| r.contains("12 +     let w = s.width();")).unwrap();
        let x = (0..120u16)
            .find(|&x| {
                buf[(x, y as u16)].symbol() == "l" && buf[(x + 1, y as u16)].symbol() == "e" && buf[(x + 2, y as u16)].symbol() == "t"
            })
            .unwrap();
        assert_eq!(buf[(x, y as u16)].fg, theme::pal().syn_kw);
        assert_eq!(buf[(x, y as u16)].bg, theme::pal().add_bg);
    }

    #[test]
    fn shift_z_ctrl_keys_are_not_letters_and_ctrl_l_restarts() {
        let (mut app, _) = trace_app("ctrl");
        // ^Z isn't z (view output), ^O isn't o
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.trace.is_some() && app.reader.is_none());
        assert!(app.trace.as_ref().unwrap().open.is_empty());
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(app.relaunch && app.quit);
    }

    #[test]
    fn shift_z_redraws_in_the_new_colors_after_a_theme_switch() {
        let (mut app, _) = trace_app("theme");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        let kw_at = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(120, 50)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            let rows = reader_rows(app, 120, 50);
            let y = rows.iter().position(|r| r.contains("12 +     let w = s.width();")).unwrap() as u16;
            let x = (0..118u16)
                .find(|&x| buf[(x, y)].symbol() == "l" && buf[(x + 1, y)].symbol() == "e" && buf[(x + 2, y)].symbol() == "t")
                .unwrap();
            (buf[(x, y)].fg, buf[(x, y)].bg)
        };
        assert_eq!(kw_at(&mut app), (theme::pal().syn_kw, theme::pal().add_bg));
        // Previewing another theme on the settings page: already laid-out lines have the old colors and need laying out again
        let mut p = *theme::pal();
        (p.syn_kw, p.add_bg) = (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6));
        theme::set(Box::leak(Box::new(p)));
        assert_eq!(kw_at(&mut app), (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6)));
    }

    #[test]
    fn shift_z_each_side_scrolls_on_its_own() {
        let (mut app, _) = trace_app("scroll");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        let (_, right) = trace_panes(&mut app, 120, 12);
        assert!(right.iter().any(|r| r.contains("↓") && r.contains("行")), "右边底下还有要说:{right:#?}");
        // Wheel: scrolls whichever side the mouse is on
        let wheel = |app: &mut App, column: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column, row: 5, modifiers: KeyModifiers::NONE })
        };
        wheel(&mut app, 100);
        let v = app.trace.as_ref().unwrap();
        assert_eq!((v.steps.top, v.chat.top), (3, v.chat.top));
        let chat_top = v.chat.top;
        wheel(&mut app, 5);
        assert!(app.trace.as_ref().unwrap().chat.top > chat_top);
        // Turn changed: the right starts from the top
        press(&mut app, KeyCode::Char('n'));
        trace_panes(&mut app, 120, 12);
        assert_eq!(app.trace.as_ref().unwrap().steps.top, 0);
    }

    #[test]
    fn one_line_left_is_a_line_in_english() {
        let last = |n: usize| i18n::with(Lang::En, || clip_lines(vec![vec![]; n], 2, "").last().unwrap()[0].text.clone());
        assert_eq!((last(3), last(4)), ("… 1 more line".to_string(), "… 2 more lines".to_string()));
    }

    #[test]
    fn more_below_in_the_record_takes_the_bottom_row_rather_than_covering_a_line() {
        let (mut app, _) = trace_app("phone");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        let rows = reader_rows(&mut app, 46, 8);
        assert!(rows.iter().any(|r| r.trim().starts_with("↓ ") && r.trim().ends_with('行')), "{rows:#?}");
    }

    #[test]
    fn shift_z_on_a_phone_is_the_chat_and_a_turns_steps_open_full_screen() {
        let (mut app, _) = trace_app("phone");
        // Just opened: the whole screen is the conversation, at the latest
        let rows = reader_rows(&mut app, 46, 16);
        shows(&rows, &["你  跑一下测试", "● 运行中", "⏎ 查看过程"]);
        assert!(!rows[1..rows.len() - 1].iter().any(|r| r.contains('│')), "手机上不分两栏:{rows:#?}");
        assert_eq!(trace_sel(&app), "跑一下测试");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        // A block taller than the screen: ↓ scrolls inside it first
        reader_rows(&mut app, 46, 8);
        let top = app.trace.as_ref().unwrap().chat.top;
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        assert_eq!(app.trace.as_ref().unwrap().chat.top, top + 1);
        // ⏎ shows this turn's steps full screen, with the turn number on top
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 46, 30);
        shows(&rows, &["第 1 轮", "── 4.0s", "▸ Grep  width(", "▸ Edit  src/bill.rs", "o 展开", "Esc 返回对话"]);
        assert!(!rows.iter().any(|r| r.contains("你  看看两栏")));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 46, 20), &["Grep · 第 1 轮", "── 输出 ──"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        shows(&reader_rows(&mut app, 46, 16), &["你  看看两栏为什么对不齐", "⏎ 查看过程"]);
        // G back to the latest
        press(&mut app, KeyCode::Char('G'));
        reader_rows(&mut app, 46, 16);
        assert_eq!(trace_sel(&app), "跑一下测试");
    }

    #[test]
    fn shift_z_follows_new_turns_while_it_sits_at_the_bottom() {
        use std::io::Write;
        let (mut app, p) = trace_app("follows");
        let add = |v: serde_json::Value| writeln!(std::fs::OpenOptions::new().append(true).open(&p).unwrap(), "{v}").unwrap();
        trace_panes(&mut app, 120, 20);
        add(crate::trace_claude::tests::done("r3", "a5", 50_000, "t3", false));
        add(crate::trace_claude::tests::prompt("u4", Some("r3"), 60_000, "下一句"));
        app.trace_tail();
        // A new turn arrives: the left selects it, stuck to the bottom; the right switches to its steps
        let (left, right) = trace_panes(&mut app, 120, 20);
        assert_eq!(trace_sel(&app), "下一句");
        shows(&left, &["你  下一句"]);
        assert!(!right.iter().any(|r| r.contains("cargo build")), "{right:#?}");
        shows(&reader_rows(&mut app, 120, 20), &["4 轮"]);
        // Moved up: no longer follows; another new one doesn't move it
        press(&mut app, KeyCode::Up);
        add(crate::trace_claude::tests::prompt("u5", Some("u4"), 70_000, "又一句"));
        app.trace_tail();
        trace_panes(&mut app, 120, 20);
        assert_ne!(trace_sel(&app), "又一句");
        // On a phone, following the latest also sticks to the bottom
        let (mut app, p) = trace_app("follows2");
        reader_rows(&mut app, 46, 10);
        writeln!(
            std::fs::OpenOptions::new().append(true).open(&p).unwrap(),
            "{}",
            crate::trace_claude::tests::prompt("u4", Some("a5"), 60_000, "下一句")
        )
        .unwrap();
        app.trace_tail();
        shows(&reader_rows(&mut app, 46, 10), &["你  下一句"]);
        assert_eq!(trace_sel(&app), "下一句");
    }

    #[test]
    fn shift_z_says_why_when_there_is_nothing_to_read() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Err("找不到记录".into()));
        shows(&reader_rows(&mut app, 60, 10), &["记录 · 跑", "找不到记录"]);
        // An old result coming back after the session changed is dropped
        press(&mut app, KeyCode::Char('J'));
        app.set_trace("跑", Err("旧的".into()));
        assert!(app.trace.as_ref().unwrap().err.is_none());
    }

    /// Look at it by hand: cargo test --release trace_look -- --ignored --nocapture
    #[test]
    #[ignore]
    fn trace_look() {
        let show = |app: &mut App, w: u16, h: u16, what: &str| {
            println!("[{what} · {w}x{h}]");
            println!("{}", reader_rows(app, w, h).join("\n"));
            println!("{}", "=".repeat(w as usize));
        };
        let (mut app, _) = trace_app("look");
        show(&mut app, 120, 26, "桌面 · 刚打开");
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        show(&mut app, 120, 34, "桌面 · 第 1 轮");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        show(&mut app, 120, 34, "桌面 · 进过程、选到 Edit");
        let (mut app, _) = trace_app("look2");
        show(&mut app, 46, 34, "手机 · 对话");
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        press(&mut app, KeyCode::Enter);
        show(&mut app, 46, 40, "手机 · 第 1 轮的过程");
    }

    /// Check the colors by hand: ACA_TRACE_HTML=<dir> cargo test --release trace_html -- --ignored. Draws a few screens into a web page
    /// Draw one screen and turn it into colored HTML (for checking the layout yourself)
    fn html_page(app: &mut App, w: u16, h: u16) -> String {
        let css = |c: Color| match c {
            Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
            _ => None,
        };
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            let mut x = 0;
            while x < w {
                let c = &buf[(x, y)];
                let mut st = format!("display:inline-block;width:{}ch;", c.cell_width().max(1));
                if let Some(f) = css(c.fg) {
                    st += &format!("color:{f};");
                }
                if let Some(b) = css(c.bg) {
                    st += &format!("background:{b};");
                }
                if c.modifier.contains(Modifier::BOLD) {
                    st += "font-weight:bold;";
                }
                let sym = match c.symbol() {
                    " " => "&nbsp;".to_string(),
                    s => s.replace('&', "&amp;").replace('<', "&lt;"),
                };
                out += &format!("<span style=\"{st}\">{sym}</span>");
                x += c.cell_width().max(1);
            }
            out += "<br>";
        }
        format!(
            "<div style=\"display:inline-block;vertical-align:top;margin:12px;padding:6px;background:#eff1f5;color:#4c4f69;font:14px/1.25 Menlo,'PingFang SC',monospace;white-space:pre\">{out}</div>"
        )
    }

    #[test]
    #[ignore]
    fn trace_html() {
        let Ok(dir) = std::env::var("ACA_TRACE_HTML") else { return };
        let page = html_page;
        let (mut app, _) = trace_app("html");
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        let mut html = page(&mut app, 120, 30);
        press(&mut app, KeyCode::Right);
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        html += &page(&mut app, 120, 30);
        // o unfolds Edit
        press(&mut app, KeyCode::Char('o'));
        html += &page(&mut app, 120, 30);
        // Unfold Read in the last turn (keys go by the last draw, so draw after each)
        for k in [KeyCode::Char('n'), KeyCode::Char('n'), KeyCode::Down, KeyCode::Down, KeyCode::Char('o')] {
            press(&mut app, k);
            page(&mut app, 120, 30);
        }
        html += &page(&mut app, 120, 30);
        // Just wide enough for two panes
        for k in [KeyCode::Char('p'), KeyCode::Char('p'), KeyCode::Char('O')] {
            press(&mut app, k);
            page(&mut app, 90, 40);
        }
        html += &page(&mut app, 90, 40);
        let (mut app, _) = trace_app("html2");
        html += &page(&mut app, 46, 30);
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('p'));
        }
        press(&mut app, KeyCode::Enter);
        html += &page(&mut app, 46, 30);
        for k in [KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Char('o')] {
            press(&mut app, k);
            page(&mut app, 46, 30);
        }
        html += &page(&mut app, 46, 30);
        std::fs::write(format!("{dir}/trace.html"), format!("<html><body style=\"background:#dce0e8;margin:0\">{html}</body></html>"))
            .unwrap();
    }

    /// The subagent screens: ACA_TRACE_HTML=<dir> cargo test --release trace_html_subs -- --ignored
    #[test]
    #[ignore]
    fn trace_html_subs() {
        let Ok(dir) = std::env::var("ACA_TRACE_HTML") else { return };
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        let (p, _, later) = crate::trace_claude::tests::with_subs("html-subs");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            use std::io::Write;
            writeln!(f, "{l}").unwrap();
        }
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        let mut html = String::new();
        for k in [KeyCode::Char('p'), KeyCode::Right, KeyCode::Down, KeyCode::Down] {
            html_page(&mut app, 120, 30);
            press(&mut app, k);
        }
        html += &html_page(&mut app, 120, 30);
        press(&mut app, KeyCode::Char('o'));
        html += &html_page(&mut app, 120, 30);
        press(&mut app, KeyCode::Enter);
        html += &html_page(&mut app, 120, 30);
        html += &html_page(&mut app, 46, 30);
        std::fs::write(format!("{dir}/trace-subs.html"), format!("<html><body style=\"background:#dce0e8;margin:0\">{html}</body></html>"))
            .unwrap();
    }

    fn reader_rows(app: &mut App, w: u16, h: u16) -> Vec<String> {
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
    fn e_writes_a_note_saving_every_key_and_esc_just_leaves() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        assert!(app.typing());
        typ(&mut app, "先跑测试");
        press(&mut app, KeyCode::Enter); // ⏎ is a newline
        typ(&mut app, "再提交");
        assert_eq!(app.notes.get("id-闲"), Some("先跑测试\n再提交")); // Saved as you type, no waiting for Esc
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("✎ 便签")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("再提交▏")), "光标在预览里:{rows:#?}");
        // Written in the preview, so the bottom bar only has the keys, without repeating the text
        assert!(rows.last().unwrap().trim_start().starts_with("Esc 保存"), "{rows:#?}");
        assert!(!rows.last().unwrap().contains("先跑测试"), "{rows:#?}");
        // With no preview (off), written in a popup
        app.preview = Preview::Off;
        shows(&reader_rows(&mut app, 100, 24), &["便签 · 闲", "先跑测试", "再提交▏", "Esc 保存"]);
        app.preview = Preview::Auto;
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "便签已保存");
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("○ 闲") && r.contains('✎')), "{rows:#?}");
        for want in ["空闲 · 10 秒前", "/p/work", "✎ 先跑测试", "再提交"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        // e again: the old note prefilled; ^U clears it and deletes it on the spot
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "先跑测试\n再提交");
        ctrl(&mut app, 'u');
        assert_eq!(app.notes.get("id-闲"), None);
        press(&mut app, KeyCode::Esc);
        assert!(!reader_rows(&mut app, 100, 24).iter().any(|r| r.contains('✎')));
    }

    #[test]
    fn ctrl_g_hands_the_note_to_the_editor() {
        let mut app = fixture();
        select(&mut app, "闲");
        // ^G halfway through: takes what's written to the editor, then back to writing in the bottom bar
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "草稿");
        ctrl(&mut app, 'g');
        assert!(app.prompt.is_none());
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "草稿");
        app.note_editor_done(ed, Ok("草稿\n编辑器里补的".into()));
        assert_eq!(app.notes.get("id-闲"), Some("草稿\n编辑器里补的"));
        assert_eq!(app.prompt.as_ref().unwrap().text, "草稿\n编辑器里补的");
        press(&mut app, KeyCode::Esc);
        // ^G straight from the list; the editor not exiting cleanly (:cq) = no change
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "草稿\n编辑器里补的");
        app.note_editor_done(ed, Err("nvim 未正常退出,便签未修改".into()));
        assert_eq!(app.notes.get("id-闲"), Some("草稿\n编辑器里补的"));
        assert!(app.prompt.is_none());
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        app.note_editor_done(ed, Ok(String::new())); // Emptied in the editor = note deleted
        assert_eq!(app.notes.get("id-闲"), None);
    }

    #[test]
    fn e_on_a_group_writes_the_groups_own_note() {
        let mut app = fixture();
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "这一组:先把测试补齐");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "再看性能");
        assert_eq!(app.notes.get("group:work"), Some("这一组:先把测试补齐\n再看性能"));
        // Written in the group's preview, cursor at the end; in a popup when the preview is off
        shows(&reader_rows(&mut app, 100, 24), &["✎ 便签", "再看性能▏"]);
        app.preview = Preview::Off;
        shows(&reader_rows(&mut app, 100, 24), &["便签 · work", "再看性能▏"]);
        app.preview = Preview::Auto;
        press(&mut app, KeyCode::Esc);
        assert_eq!(said(&app), "便签已保存");
        // ✎ after the group name, shown in the preview; the group's sessions have no note of their own
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("▾ work") && r.contains('✎')), "{rows:#?}");
        shows(&rows, &["✎ 这一组:先把测试补齐", "再看性能"]);
        assert!(app.all.iter().filter(|s| s.group == "work").all(|s| s.group_noted && !s.noted));
        // e on a session still writes the session's own
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "");
        press(&mut app, KeyCode::Esc);
        // Same on the fold row: marked ✎, the old note prefilled; ^Z undoes the edit
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("▸ work") && r.contains('✎')), "{rows:#?}");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "这一组:先把测试补齐\n再看性能");
        typ(&mut app, "!");
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "已撤销修改便签「work」");
        assert_eq!(app.notes.get("group:work"), Some("这一组:先把测试补齐\n再看性能"));
        // ^G: what's written in the editor is the group's too; emptied = note deleted
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "这一组:先把测试补齐\n再看性能");
        app.note_editor_done(ed, Ok(String::new()));
        assert_eq!(app.notes.get("group:work"), None);
        assert!(!reader_rows(&mut app, 100, 24).iter().any(|r| r.contains('✎')));
    }

    #[test]
    fn group_pane_puts_the_note_under_the_path_and_gives_it_half_the_pane() {
        let sp = Spot { group: "work".into(), dir: "/p/work".into(), from: None };
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        let long = (1..=20).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        let got = text(&group_pane(&sp, None, Some(&long), false, 30, 20));
        assert_eq!(got[..3], ["/p/work", "✎ 第1行", "  第2行"]);
        assert_eq!(got.len(), 10, "{got:#?}");
        assert_eq!(got[9], "  … 还有 12 行");
        // While writing: cursor at the end, still half
        let got = text(&group_pane(&sp, None, Some(&long), true, 30, 20));
        assert_eq!((got.len(), got[1].as_str(), got[9].as_str()), (10, "✎ 便签", "  第20行▏"));
    }

    #[test]
    fn a_deleted_groups_note_goes_to_the_trash_and_comes_back_with_it() {
        let mut app = App::new(String::new(), life_and_work()).with_groups(deck_groups);
        app.notes.set("group:fun", "整体的想法").unwrap();
        let bin = |app: &mut App| {
            jump_to_group(app, "fun");
            keys(app, "dy");
            let job = app.take_job().unwrap();
            app.job_done(&job, fun_binned(), without_fun());
        };
        let restore = |app: &mut App| {
            ctrl(app, 'z');
            let job = app.take_job().unwrap();
            app.job_done(&job, Outcome::Done("已恢复分组 fun · 4 个会话".into()), life_and_work());
        };
        bin(&mut app);
        // Kept in the Trash entry and cleared from the notes: a new fun starts empty
        assert_eq!(app.trash.get("x1").unwrap().groups.iter().map(|g| g.note.as_str()).collect::<Vec<_>>(), ["整体的想法", ""]);
        assert_eq!(app.notes.get("group:fun"), None);
        restore(&mut app);
        assert_eq!(app.notes.get("group:fun"), Some("整体的想法"));
        // A fun created in the meantime got a note: the restored one is appended after it
        bin(&mut app);
        app.notes.set("group:fun", "新的").unwrap();
        restore(&mut app);
        assert_eq!(app.notes.get("group:fun"), Some("新的\n\n整体的想法"));
    }

    #[test]
    fn only_shift_e_takes_the_note_in_and_only_when_it_is_safe() {
        let mut app = fixture();
        for t in ["闲", "跑", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的")).unwrap();
        }
        app.set_sessions(sessions());
        press(&mut app, KeyCode::Char('a')); // All (「未读」 has been quiet for five days)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Enter); // ⏎: the note stays a note
        assert_eq!((app.attach.take().as_deref(), app.fill.take()), (Some("闲"), None));
        press(&mut app, KeyCode::Char('E'));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        assert_eq!(app.fill.take(), Some(("id-闲".into(), "给闲的".into())));
        select(&mut app, "跑"); // Fills while running too (Claude takes typing as it runs, ⏎ queues it); Shift+Enter in kitty too
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.fill.take().map(|f| f.0), Some("id-跑".into()));
        app.attach = None;
        select(&mut app, "问"); // Waiting for your choice: text would be taken as an option → doesn't go in, says why
        press(&mut app, KeyCode::Char('E'));
        assert_eq!((app.attach.take(), app.fill.take()), (None, None));
        assert!(app.msg.as_ref().unwrap().0.contains("等待回应"));
        select(&mut app, "未读"); // No note: same as ⏎
        press(&mut app, KeyCode::Char('E'));
        assert_eq!((app.attach.take().as_deref(), app.fill.take()), (Some("未读"), None));
        // Deleted once filled, and the ✎ goes with it
        app.note_filled("id-闲");
        assert_eq!(app.notes.get("id-闲"), None);
        assert!(!app.all.iter().find(|s| s.title == "闲").unwrap().noted);
        // Not a built-in agent: no telling what program is in front, so no fill
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "跑").unwrap().agent = Agent::None;
        app.set_sessions(v);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('E'));
        assert_eq!(app.attach.take(), None);
        assert!(app.msg.as_ref().unwrap().0.contains("不是内置 agent"));
    }

    #[test]
    fn s_sends_the_note_only_after_y_and_nothing_without_one() {
        let mut app = fixture();
        for t in ["闲", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的\n第二行")).unwrap();
        }
        app.set_sessions(sessions());
        select(&mut app, "跑"); // No note: sends nothing, just says so
        press(&mut app, KeyCode::Char('s'));
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert!(app.msg.as_ref().unwrap().0.contains("没有便签"), "{:?}", app.msg);
        app.msg = None; // The message holds the bottom bar
        assert!(!reader_rows(&mut app, 120, 20).iter().any(|r| r.contains("发便签")));
        // With a note it still asks first: to whom and what (multiple lines kept as lines); s shows in the bottom bar only with a note
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 120, 20);
        assert!(rows.iter().any(|r| r.contains("s 发送")), "{rows:#?}");
        press(&mut app, KeyCode::Char('s'));
        let rows = reader_rows(&mut app, 46, 16);
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("缺「{t}」:{rows:#?}"));
        assert!(at("发送便签？") < at("给闲的") && at("给闲的") < at("第二行") && at("第二行") < at("y 发送"), "{rows:#?}");
        press(&mut app, KeyCode::Char('s')); // Pressed twice by mistake: the second one just cancels
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "未发送");
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        let Job::Send { t, pane, text, key } = &job else { panic!("{job:?}") };
        assert_eq!((t.name.as_str(), pane.as_str(), text.as_str(), key.as_str()), ("闲", "=闲:", "给闲的\n第二行", "id-闲"));
        // Once sent, the note is deleted
        app.job_done(&job, Outcome::Done("已发给「闲」".into()), sessions());
        assert_eq!(app.notes.get("id-闲"), None);
        // Waiting for your choice: text would be taken as an option, so not sent
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.confirm.is_none());
        assert!(app.msg.as_ref().unwrap().0.contains("等待回应"));
    }

    #[test]
    fn preview_head_states_the_facts_and_caps_long_notes() {
        let mut s = mk("闲", "work", State::Idle, 70);
        s.dir = "/p/work".into();
        s.info = "Repo: /r\nPath: /p/work\nBranch: feat".into();
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        // State · how long ago, directory, branch, then a blank line before the screen content
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10)), ["空闲 · 1 分钟前   /p/work  feat", ""]);
        // Session id and open-elsewhere aren't in the preview (B copies the id; ⇄ is on the list row)
        s.info.push_str("\nSession: 0000aaaa-1111-2222-3333-444455556666");
        s.attached = true;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10)), ["空闲 · 1 分钟前   /p/work  feat", ""]);
        // Narrow: cut from the end
        assert_eq!(text(&preview_head(&s, None, None, false, 24, 10))[0], "空闲 · 1 分钟前   /p/wo…");
        // Child session: who dispatched it; also says so when the parent is deleted
        s.up = Up::Apart("p".into(), "派活的".into());
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "「派活的」的子会话");
        s.up = Up::Gone;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "子会话 · 父会话已删除");
        s.up = Up::Top;
        // A fork: forked from whom; also says so when the original is deleted
        s.fork = Fork::Of("原来的".into());
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "分叉自「原来的」");
        s.fork = Fork::Gone;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "分叉 · 原会话已删除");
        s.fork = Fork::No;
        // A note gets at most three lines; beyond that, says how many more
        let long = (1..=20).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        let got = text(&preview_head(&s, None, Some(&long), false, 30, 20));
        assert_eq!(got[1..], ["✎ 第1行", "  第2行", "  第3行", "  … 还有 17 行", ""]);
        // While writing: even a short one takes the fixed top half; a long one keeps its last lines (where the cursor is)
        let got = text(&preview_head(&s, None, Some("一行"), true, 30, 20));
        assert_eq!(got.len(), 10, "{got:#?}");
        assert_eq!((got[1].as_str(), got[2].as_str()), ("✎ 便签", "  一行▏"));
        let got = text(&preview_head(&s, None, Some(&long), true, 30, 20));
        assert_eq!((got[2].as_str(), got[8].as_str()), ("  … 上面还有 14 行", "  第20行▏"));
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

    #[test]
    fn z_reads_without_attaching_and_scrolls_history() {
        let mut app = fixture(); // 1 day: 问 跑 闲
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('z'));
        assert!(app.reader_load && app.attach.is_none());
        assert_eq!(app.reader_target().as_deref(), Some("=跑:")); // Fake sessions have no pane id, so capture by session name
        app.reader_load = false;
        // History captured by the run loop: 100 lines + the input box at the bottom (to be stripped)
        let bar = "─".repeat(20);
        let mut hist: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        hist.extend([bar.clone(), "❯ ".into(), bar]);
        app.set_reader_lines(hist);
        let rows = reader_rows(&mut app, 30, 10); // Title + 8 lines + bottom bar
        assert!(rows[0].contains("跑") && rows[0].contains("2/3 · 最新"), "{rows:?}");
        assert_eq!(rows[8], "line 100"); // Input box stripped, the last line is content
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('b')); // 1 + one page (10 - 3 = 7)
        let rows = reader_rows(&mut app, 30, 10);
        assert_eq!(rows[8], "line 92");
        assert!(rows[0].contains("↑8 行"), "{rows:?}");
        // New content grows at the bottom while scrolled up: the view stays put
        let mut more: Vec<String> = (1..=105).map(|i| format!("line {i}")).collect();
        more.push(String::new());
        app.set_reader_lines(more);
        assert_eq!(reader_rows(&mut app, 30, 10)[8], "line 92");
        press(&mut app, KeyCode::Char('g')); // To the top, no overrun
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(reader_rows(&mut app, 30, 10)[1], "line 1");
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(reader_rows(&mut app, 30, 10)[8], "line 105");
    }

    #[test]
    fn reader_j_k_switch_sessions_and_enter_attaches() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.reader_target().as_deref(), Some("=跑:")); // Fake sessions have no pane id, so capture by session name
        press(&mut app, KeyCode::Char('J'));
        press(&mut app, KeyCode::Char('J')); // At the end: a message, still viewing 闲
        assert_eq!(app.reader_target().as_deref(), Some("=闲:"));
        assert!(app.msg.is_none());
        press(&mut app, KeyCode::Char('q')); // q only leaves reading, doesn't quit aca
        assert!(app.reader.is_none() && !app.quit);
        assert_eq!(at(&app), "闲"); // Back on the list, highlight on the last one viewed
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Enter);
        assert!(app.reader.is_none());
        assert_eq!(app.attach.as_deref(), Some("闲"));
    }

    #[test]
    fn x_shows_the_diff_and_n_p_m_tab_get_around() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('x'));
        assert!(app.reader_target().is_none()); // Doesn't capture output from tmux
        let (name, _, mode) = app.diff_request(false).unwrap();
        assert_eq!((name.as_str(), mode), ("跑", Mode::Uncommitted));
        assert!(app.diff_request(false).is_none()); // Already read
        assert!(app.diff_request(true).is_none()); // Timed refresh, nothing new from it: no reread
        app.all.iter_mut().find(|s| s.name == "跑").unwrap().active_at += 1; // Reread only once there's activity
        assert!(app.diff_request(true).is_some());
        assert!(app.diff_request(true).is_none());
        shows(&reader_rows(&mut app, 40, 12), &["改动 · 跑", "未提交 · 读取中…", "正在读取 git…"]);
        // Three files, 10 lines each
        let patch = |files: &[&str]| {
            files
                .iter()
                .map(|f| {
                    format!(
                        "diff --git a/{f} b/{f}\n--- a/{f}\n+++ b/{f}\n@@ -1 +1,10 @@\n{}",
                        (1..=10).map(|j| format!("+{f}.{j}\n")).collect::<String>()
                    )
                })
                .collect::<String>()
        };
        let head = diff::Head { dir: "~/p".into(), branch: "main".into(), ..Default::default() };
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["f1", "f2", "f3"]), &[]));
        let rows = reader_rows(&mut app, 40, 12); // Title + 10 lines + bottom bar
        shows(&rows, &["未提交 · 3 个文件", "n p 切换文件"]);
        assert_eq!(rows[1], " ~/p  main +30");
        let top = |app: &mut App| reader_rows(app, 40, 12)[1].clone();
        // n: the next file's header goes to the very top
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(top(&mut app), "▌M f1 +10");
        shows(&reader_rows(&mut app, 40, 12), &["文件 1/3"]);
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(top(&mut app), "▌M f3 +10");
        press(&mut app, KeyCode::Char('n'));
        assert!(app.msg.is_none());
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(top(&mut app), "▌M f1 +10");
        press(&mut app, KeyCode::Char('p')); // Back past the first: to the file list
        assert_eq!(top(&mut app), " ~/p  main +30");
        press(&mut app, KeyCode::Char('p'));
        assert!(app.msg.is_none());
        // The diff grew on refresh (the file list gained a line): the top line stays
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('j')); // The new result comes before a redraw; this key still counts
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["f0", "f1", "f2", "f3"]), &[]));
        // Pinned by line number (the top was line 7, f1's @@): the file list gained a line, so line 7 is now f0's header
        let rows = reader_rows(&mut app, 40, 12);
        assert_eq!(rows[1], "▌M f0 +10", "{rows:#?}");
        // m: switch to with commits, read again; an old result still on its way is dropped
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.diff_request(false).map(|r| r.2), Some(Mode::Branch));
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["old"]), &[]));
        app.msg = None; // Keep the bottom bar free of messages
        shows(&reader_rows(&mut app, 46, 12), &["连同提交 · 读取中…", "m 仅未提交"]);
        // Tab cycles: diff → record → output → diff, the mode carried over
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_some() && app.reader.is_none());
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_none());
        assert_eq!(app.reader_target().as_deref(), Some("=跑:"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.diff_request(false).map(|r| r.2), Some(Mode::Branch));
        // J switches session, still on the diff
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.diff_request(false).map(|r| r.0), Some("闲".to_string()));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.reader.is_none() && !app.quit);
        assert_eq!(at(&app), "闲");
    }

    #[test]
    fn diff_e_opens_the_editor_where_the_top_line_is_and_l_opens_lazygit() {
        let tmp = std::env::temp_dir().join(format!("aca-ui-edit-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("f1"), "x\n".repeat(20)).unwrap();
        let mut app = fixture();
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.msg.as_ref().unwrap().0, "正在读取 git,请稍候");
        let patch = "diff --git a/f1 b/f1\n--- a/f1\n+++ b/f1\n@@ -5,3 +5,4 @@\n a\n-b\n+B\n+C\n c\ndiff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let mut d = diff::render(&diff::Head { dir: "~/p".into(), ..Default::default() }, patch, &[]);
        d.top = tmp.to_string_lossy().into_owned();
        let name = app.reader.as_ref().unwrap().name.clone();
        app.set_diff(&name, Mode::Uncommitted, d);
        // Only 3 lines per screen, so it can scroll
        reader_rows(&mut app, 40, 5);
        let f1 = tmp.join("f1").to_string_lossy().into_owned();
        // File list on top: opens the first change in the first file
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Edit { path: f1.clone(), line: 6 })); // -b is on line 6
        // Scrolled so the +C line is at the very top: line 7
        press(&mut app, KeyCode::Char('n'));
        for _ in 0..5 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(reader_rows(&mut app, 40, 5)[1], "+C");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Edit { path: f1, line: 7 }));
        // A deleted file can't be opened
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('e'));
        assert!(app.external.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "该文件已被删除");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.external.take(), Some(External::Lazygit(tmp.to_string_lossy().into_owned())));
        // The diff is reread after coming back
        let _ = app.diff_request(false);
        app.external_done(None);
        assert!(app.diff_request(false).is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn preview_head_puts_the_change_count_first_and_trims_the_dir_from_the_left() {
        let mut s = mk("闲", "work", State::Idle, 70);
        s.dir = "/p/ws".into();
        s.info = "Path: /p/ws".into();
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        let g = Glance {
            work: crate::workdir::Found { dir: "/p/ws/some-long-project-name/app".into(), how: "Claude 现在在这" },
            repo: Some(diff::Stat { branch: "main".into(), files: 3, add: 12, del: 4 }),
            files: None,
        };
        // Uses the directory it really works in and the branch there
        assert_eq!(
            text(&preview_head(&s, Some(&g), None, false, 80, 10))[0],
            "空闲 · 1 分钟前  3 个文件 +12 −4   /p/ws/some-long-project-name/app  main"
        );
        // Narrow: stats and branch stay, the directory is cut from the left
        assert_eq!(text(&preview_head(&s, Some(&g), None, false, 46, 10))[0], "空闲 · 1 分钟前  3 个文件 +12 −4   …/app  main");
        let clean = Glance { repo: Some(diff::Stat { branch: "main".into(), ..Default::default() }), ..g };
        assert_eq!(
            text(&preview_head(&s, Some(&clean), None, false, 80, 10))[0],
            "空闲 · 1 分钟前   /p/ws/some-long-project-name/app  main"
        );
    }

    #[test]
    fn the_change_count_is_read_for_the_highlighted_one_when_it_changes_or_ticks() {
        let mut app = fixture();
        assert_ne!(app.mode(), Preview::Off);
        let first = app.glance_request(false).map(|r| r.0);
        assert!(first.is_some());
        assert!(app.glance_request(false).is_none()); // Same one, not read again
        assert!(app.glance_request(true).is_none()); // Timed refresh, nothing new: no read (no git every 2s while on a big repo)
        // It has activity (running: pane output; idle: a hook reported something): read again
        let i = app.view[app.list.selected().unwrap()];
        app.all[i].active_at += 1;
        assert!(app.glance_request(true).is_some());
        assert!(app.glance_request(true).is_none());
        // Also read once GLANCE_EVERY has passed: files changed elsewhere show up too
        app.glance_for.as_mut().unwrap().2 = Instant::now() - GLANCE_EVERY;
        assert!(app.glance_request(true).is_some());
        select(&mut app, "跑");
        let next = app.glance_request(false).map(|r| r.0);
        assert!(next.is_some() && next != first);
        press(&mut app, KeyCode::Char('z')); // Not read in full screen
        assert!(app.glance_request(true).is_none());
    }

    #[test]
    fn clip_pads_by_display_width() {
        assert_eq!(clip_left("~/workspace/fun/app", 9), "…/fun/app");
        assert_eq!(clip_left("~/a", 9), "~/a");
        assert_eq!(clip("中文标题", 10).width(), 10);
        assert_eq!(clip("一个很长很长的中文标题", 10).width(), 10);
        assert!(clip("一个很长很长的中文标题", 10).contains('…'));
    }

    thread_local! {
        /// What the settings page writes to config.toml (the fake config_set records it here)
        static WROTE: std::cell::RefCell<Vec<(String, String, String)>> = const { std::cell::RefCell::new(vec![]) };
    }

    fn wrote() -> Vec<(String, String, String)> {
        WROTE.with(|w| w.borrow_mut().drain(..).collect())
    }

    fn settings_app() -> App {
        fixture().with_settings(
            || true,
            Vec::new,
            |sec, key, v| {
                WROTE.with(|w| w.borrow_mut().push((sec.into(), key.into(), v.to_string())));
                Ok(())
            },
            || {
                let acct = |agent, name: &str, dir: &str| crate::accounts::Account { agent, name: name.into(), dir: dir.into() };
                vec![acct(Agent::Claude, "work", "~/.claude-work"), acct(Agent::Codex, "cx", "~/.codex-cx")]
            },
        )
    }

    #[test]
    fn ctrl_s_opens_settings_letters_act_and_plain_s_still_sends() {
        let mut app = settings_app();
        wrote();
        ctrl(&mut app, 's');
        assert!(app.settings.is_some() && app.confirm.is_none());
        press(&mut app, KeyCode::Char('i'));
        if cfg!(target_os = "macos") {
            assert_eq!(wrote(), [("ime".to_string(), "auto".to_string(), "false".to_string())]);
            assert!(!app.settings.as_ref().unwrap().ime && app.ime_changed);
            press(&mut app, KeyCode::Enter); // Highlight stays on the row just pressed: ⏎ switches back
            assert_eq!(wrote()[0].2, "true");
        } else {
            // Auto English input is macOS-only: elsewhere the row isn't there, and pressing writes nothing
            assert!(wrote().is_empty() && !app.ime_changed);
        }
        ctrl(&mut app, 's');
        assert!(app.settings.is_none());
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Esc);
        assert!(app.settings.is_none() && !app.quit);
    }

    #[test]
    fn l_in_settings_switches_the_language_right_away_and_saves_it() {
        i18n::with(Lang::Zh, || {
            let mut app = settings_app();
            wrote();
            ctrl(&mut app, 's');
            shows(&reader_rows(&mut app, 100, 30), &["l  语言", "zh  en"]);
            press(&mut app, KeyCode::Char('l'));
            assert_eq!(wrote(), [("ui".to_string(), "lang".to_string(), "\"en\"".to_string())]);
            assert_eq!(i18n::lang(), Lang::En);
            // Settings page still open, already in English
            shows(&reader_rows(&mut app, 100, 30), &["Settings", "l  language", "zh  en"]);
            press(&mut app, KeyCode::Enter);
            assert_eq!((wrote()[0].2.as_str(), i18n::lang()), ("\"zh\"", Lang::Zh));
            shows(&reader_rows(&mut app, 100, 30), &["设置", "l  语言"]);
        });
    }

    /// A row a feature adds has its name and value asked when the settings page opens: a language change (the run loop calls
    /// lang_changed when it sees one) asks again on the spot
    struct SaysRow;

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

    #[test]
    fn a_language_change_rereads_what_was_asked_when_settings_opened() {
        i18n::with(Lang::Zh, || {
            let mut app = settings_app().with_setting(Box::new(SaysRow));
            app.bill_got = Some(Instant::now());
            ctrl(&mut app, 's');
            shows(&reader_rows(&mut app, 100, 30), &["假功能", "关着"]);
            press(&mut app, KeyCode::Char('l'));
            app.lang_changed();
            shows(&reader_rows(&mut app, 100, 30), &["fake feature", "off", "language"]);
            assert!(app.bill_got.is_none() && app.bill_data.is_none());
        });
    }

    /// A settings row and popup a feature adds itself (see overlay.rs): the popup remembers which layer it is, +10 per reload
    struct FakeRow;
    struct FakePage(u8);

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
    fn a_feature_adds_a_settings_row_and_its_own_popup() {
        let mut app = settings_app().with_setting(Box::new(FakeRow));
        ctrl(&mut app, 's');
        shows(&reader_rows(&mut app, 100, 30), &["d  假功能", "现在关着"]);
        // Pressing its key: the settings page closes, its popup opens, and all keys go to it (j doesn't move the highlight)
        press(&mut app, KeyCode::Char('d'));
        assert!(app.settings.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        shows(&reader_rows(&mut app, 44, 30), &["假页面 0"]);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.msg.as_ref().unwrap().0, "说一句");
        // On a phone the popup covers the bottom bar: the message goes inside the popup
        let rows = reader_rows(&mut app, 44, 30);
        let at = |want: &str| rows.iter().position(|r| r.contains(want)).unwrap_or_else(|| panic!("没有「{want}」:{rows:#?}"));
        assert!(at("说一句") > at("假页面 0") && rows[at("说一句")].starts_with('│'), "{rows:#?}");
        // Open another layer; closing it goes back to the one before
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 30), &["假页面 1"]);
        // The top layer says it's typing: several keys at once are a paste, which the run loop doesn't drop
        assert!(app.typing());
        press(&mut app, KeyCode::Esc);
        assert!(!app.typing());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // Background work goes to the run loop; another one before it's done is refused; when it's done the popup reloads
        press(&mut app, KeyCode::Char('j'));
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Stop { t: target("闲") });
        // While it's being done, the popup says 「正在…」 (e.g. "stopping …")
        shows(&reader_rows(&mut app, 44, 30), &["│ 正在停止「闲」…"]);
        press(&mut app, KeyCode::Char('j'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在停止「闲」…");
        app.job_done(&job, Outcome::Done("好了".into()), sessions());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 10"]);
        // What to copy goes to the run loop (it writes the clipboard, the bottom bar says what was copied); the popup stays open
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.clip.take(), Some(Clip::Text("https://a.ts.net/".into())));
        assert!(!app.overlays.is_empty());
        // Opening an editor: back in the popup afterwards, reloaded
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Lazygit("/p".into())));
        app.external_done(None);
        assert!(app.settings.is_none());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 20"]);
        // Closed: back to the settings page, highlight still on its row
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        let s = app.settings.as_ref().unwrap();
        assert_eq!(s.rows()[s.sel], SetRow::Extra(0));
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && !app.quit);
    }

    #[test]
    fn shift_p_opens_the_plugins_page_from_the_list_not_from_settings() {
        // Opens straight from the list (on a group too), closing goes back to the list, never via the settings page
        let mut app = fixture().with_plugins(|| Box::new(FakePage(0)));
        press(&mut app, KeyCode::Char('P'));
        assert!(app.modal_open() && app.settings.is_none());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // Into a page from the menu (another layer), Esc back to the menu, another Esc to the list
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 30), &["假页面 1"]);
        press(&mut app, KeyCode::Esc);
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && app.settings.is_none() && !app.quit);
        // The settings page has no row for it
        let mut app = settings_app().with_plugins(|| Box::new(FakePage(0)));
        ctrl(&mut app, 's');
        assert!(!reader_rows(&mut app, 100, 30).iter().any(|r| r.contains("tailnet")));
    }

    #[test]
    fn settings_come_back_after_the_theme_list_the_dir_prompt_and_the_editor() {
        let mut app = settings_app();
        wrote();
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('t'));
        assert!(app.theme_menu.is_some() && app.settings.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.theme_menu.is_none() && app.settings.as_ref().is_some_and(|s| s.sel == 0));

        press(&mut app, KeyCode::Char('g'));
        assert!(matches!(app.prompt.as_ref().map(|p| (&p.ask, p.text.as_str())), Some((Ask::GroupDir, "~/workspace"))));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.as_ref().is_some_and(|p| p.hint == "目录不能为空") && wrote().is_empty());
        for c in "~/code".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(wrote(), [("new_group".to_string(), "default_path".to_string(), "\"~/code\"".to_string())]);
        assert!(app.prompt.is_none() && app.settings.as_ref().is_some_and(|s| s.rows()[s.sel] == SetRow::GroupDir));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.settings.is_some() && app.msg.is_none());

        press(&mut app, KeyCode::Char('n'));
        let Some(External::Config { path, template }) = app.external.take() else { panic!("{:?}", app.external) };
        assert!(path.ends_with("config.toml") && template.starts_with('#'));
        assert!(app.settings.is_none());
        app.external_done(None);
        assert!(app.settings.as_ref().is_some_and(|s| s.rows()[s.sel] == SetRow::Tools));
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::Config { path, .. }) if path.ends_with("config.toml")));
        // An editor not opened from the settings page doesn't bring the settings page back on return
        app.external_done(None);
        app.settings = None;
        app.external_done(None);
        assert!(app.settings.is_none());
    }

    #[test]
    fn a_in_settings_opens_the_accounts_page_and_closing_it_comes_back() {
        let mut app = settings_app().with_accounts(|adding| Box::new(FakePage(if adding { 7 } else { 0 })));
        ctrl(&mut app, 's');
        // The settings row only counts how many of each; accounts are managed in their own popup (acctpage.rs)
        shows(&reader_rows(&mut app, 100, 30), &["a  账号", "Claude 1  Codex 1"]);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.settings.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // Closed: back to the settings page, highlight still on the accounts row
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        assert_eq!(app.settings.as_ref().map(|s| (s.page, s.rows()[s.sel])), Some((SetPage::Main, SetRow::Accounts)));
        // Without an accounts page hooked up (the unit test default): pressing does nothing
        let mut plain = settings_app();
        ctrl(&mut plain, 's');
        press(&mut plain, KeyCode::Char('a'));
        assert!(plain.settings.is_some() && plain.overlays.is_empty());
    }

    #[test]
    fn problems_are_listed_only_when_there_are_some() {
        let mut app = settings_app();
        ctrl(&mut app, 's');
        assert!(!app.settings.as_ref().unwrap().rows().contains(&SetRow::Problems));
        app.settings = None;
        app.sync_failed = vec!["config.toml 有误".into()];
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('k')); // Up from the top = the last row
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.settings.as_ref().unwrap().page, SetPage::Problems);
        let rows = reader_rows(&mut app, 120, 36);
        assert!(rows.iter().any(|r| r.contains("config.toml 有误")), "{rows:#?}");
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::Config { path, .. }) if path.ends_with("config.toml")));
    }

    #[test]
    fn the_settings_popup_is_centered_on_desktop_and_fills_the_width_on_a_phone() {
        let mut app = settings_app();
        ctrl(&mut app, 's');
        let rows = reader_rows(&mut app, 120, 36);
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        for t in [
            "设置",
            "t  主题         latte",
            "l  语言         zh  en",
            "Claude 1  Codex 1",
            "g  新分组目录   ~/workspace",
            "e  config.toml",
        ] {
            at(t);
        }
        // Auto English input is macOS-only
        assert_eq!(rows.iter().any(|r| r.contains("i  自动切英文   开")), cfg!(target_os = "macos"), "{rows:#?}");
        // Centered, the layer below dimmed (like Tab / S)
        let top = at("设置") - 2;
        // Counted by display width (there may be Chinese on the left)
        let col = |c: char| rows[top][..rows[top].find(c).unwrap()].width();
        let (l, r) = (col('╭'), col('╮'));
        assert!((l as i32 - (120 - 1 - r as i32)).abs() <= 1, "{l} {r}: {rows:#?}");
        let mut t = Terminal::new(TestBackend::new(120, 36)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        assert!(t.backend().buffer().content().iter().any(|c| c.fg == theme::pal().fade));
        let rows = reader_rows(&mut app, 46, 20);
        let head = rows.iter().position(|r| r.contains("设置")).unwrap();
        assert!(rows[head - 1].starts_with('╭') && rows[head - 1].ends_with('╮'), "{rows:#?}");
    }

    fn plugin_req(id: &str, kind: aca_core::plugin::ItemKind) -> crate::caps::Req {
        crate::caps::Req {
            item: aca_core::plugin::MenuItem {
                plugin: "p.x".into(),
                plugin_name: "PX".into(),
                id: id.into(),
                kind,
                title: "看 CI".into(),
                ..Default::default()
            },
            session: "id-闲".into(),
            ..Default::default()
        }
    }

    #[test]
    fn colon_opens_the_plugin_menu_on_the_highlighted_session() {
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            app.selected = Some("闲".into());
            app.rebuild();
            press(&mut app, KeyCode::Char(':'));
            shows(&reader_rows(&mut app, 100, 30), &["命令 · 对「闲」", "n  新建", "aca"]);
            press(&mut app, KeyCode::Esc);
            assert!(app.overlays.is_empty());
        });
    }

    #[test]
    fn a_plugins_short_output_goes_to_the_bottom_bar_and_a_long_one_or_a_view_to_the_reader() {
        use aca_core::plugin::ItemKind;
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            app.plugin_done(plugin_req("ci", ItemKind::Command), true, "全绿\n".into());
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("✓ PX · 看 CI:全绿"));
            app.plugin_done(plugin_req("ci", ItemKind::Command), false, String::new());
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("✕ PX · 看 CI 出错"));
            assert!(app.reader.is_none());
            // A view: opens the reading page even for a single line; r reruns, c copies all, Esc comes back
            app.plugin_done(plugin_req("ci", ItemKind::View), true, "build  ok\ntest   ok".into());
            app.msg = None;
            let rows = reader_rows(&mut app, 100, 20);
            shows(&rows, &["build  ok", "test   ok", "r 重新运行", "c 复制"]);
            assert!(!rows[0].contains("1/") && rows[0].contains("✓ PX · 看 CI"), "{}", rows[0]);
            press(&mut app, KeyCode::Char('r'));
            assert_eq!(app.plugin_q.len(), 1);
            press(&mut app, KeyCode::Char('c'));
            assert_eq!(app.clip, Some(Clip::Text("build  ok\ntest   ok".into())));
            // Nothing to go into: ⏎ just closes it
            press(&mut app, KeyCode::Enter);
            assert!(app.reader.is_none() && app.external.is_none());
        });
    }

    #[test]
    fn a_list_view_opens_as_its_own_page_and_its_keys_results_land_there_and_reload_it() {
        use aca_core::plugin::ItemKind;
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            let mut view = plugin_req("tasks", ItemKind::View);
            view.item.rows = true;
            let list = r#"{"rows":[{"id":"t1","mark":"●","text":"nas  备份","right":"在等"}]}"#;
            app.plugin_done(view.clone(), true, list.into());
            assert_eq!(app.overlays.len(), 1);
            assert!(app.reader.is_none());
            shows(&reader_rows(&mut app, 100, 30), &["● nas  备份", "在等"]);
            // One of its keys finished: the result goes at the bottom of that page, the list is queued to reload; no bottom-bar
            // message, no reading page
            let refresh = crate::caps::Req { refresh: true, ..view.clone() };
            let rm = crate::caps::Req { then: Some(Box::new(refresh.clone())), ..plugin_req("rm", ItemKind::Command) };
            app.plugin_done(rm, true, "已撤 nas 上的「备份」".into());
            assert!(app.msg.is_none() && app.reader.is_none());
            assert_eq!(app.plugin_q, vec![refresh.clone()]);
            app.plugin_done(refresh.clone(), true, r#"{"empty":"没有远端任务"}"#.into());
            assert_eq!(app.overlays.len(), 1);
            shows(&reader_rows(&mut app, 100, 30), &["没有远端任务", "已撤 nas 上的「备份」"]);
            // A reload that comes back after it closed: doesn't open another page
            app.overlays.clear();
            app.plugin_done(refresh, true, list.into());
            assert!(app.overlays.is_empty());
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
