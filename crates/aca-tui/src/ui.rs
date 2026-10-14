//! 界面:会话列表 + 高亮会话的实时画面预览。每 2 秒后台刷新,attach 时让出终端,detach 回来接着显示。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::stdout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::buffer::CellWidth;
use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Padding, Paragraph};
use ratatui::{DefaultTerminal, Frame, Terminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use aca_core::i18n::{self, Lang, Tr, noun};
use aca_core::{t, tr};

use crate::ansi::{self, Styled};
use crate::create::{self, DirPick, DirRow, Draft, NewReq, Picked, Removal, Src};
use crate::data::{Agent, DEEP, Fork, Session, State, Sub, Up, collect, hangs, sort_sessions};
use crate::diff::{self, Glance, Mode};
use crate::follow::{Follow, Place};
use crate::groups;
use crate::handoff::Source;
use crate::mcp;
use crate::notes::{self, Notes};
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

/// 「近几天」可选的天数,`*` 按这个顺序轮换(最后是全部)。默认一天
pub const DAYS: [u64; 3] = [1, 3, 7];

fn days_label(d: u64) -> String {
    tr!("{d} 天", "{d}d")
}
const REFRESH: Duration = Duration::from_secs(2);
/// 底栏那句话留多久
const FLASH: Duration = Duration::from_secs(3);
/// 上次的界面怎么没的、后台出了错:开界面那一下人可能还没看过来,留久一点
const NOTICE: Duration = Duration::from_secs(12);
/// 多久看一眼事件流的文件变没变(一次 stat);变了就不等 REFRESH,马上重读
const EVENT_TICK: Duration = Duration::from_millis(150);
/// 事件触发的刷新最密隔这么久
const EVENT_GAP: Duration = Duration::from_millis(400);
/// d 删掉的,做完后再藏这么久:删之前开始的那次后台刷新可能晚到,别让它又冒出来一下
const GONE_LINGER: Duration = Duration::from_secs(5);
/// 连按数字的间隔超过这个就重新开始(按 1 再按 2 = 12 号)
const DIGIT_GAP: Duration = Duration::from_millis(1200);
/// 启动后这段时间内的按键一律丢掉:Moshi 连上后代敲命令就发生在这时,而人不可能这么快操作。
/// 自己按键打开的不用(见 startup_quiet)
const STARTUP_QUIET: Duration = Duration::from_millis(800);
/// 从会话退回来后这么久的按键丢掉:退出那一下连按的键、终端的回话还在路上。再长就吞掉紧跟着按的键了
const BACK_QUIET: Duration = Duration::from_millis(100);
/// 计费面板开着时多久重读一次(每次都是读完全部记录,一两秒)
const BILL_EVERY: Duration = Duration::from_secs(300);
/// 丢掉一段自动输入后再静默这么久:命令和结尾的回车可能分两次到,回车单独到达会被当成「进入」
const BURST_QUIET: Duration = Duration::from_millis(500);
/// 连着快速挪高亮(手机上滑动、按住方向键,两下之间不到这么久)时预览先不换,停下来这么久再换。
/// 每挪一格就换一次预览的话,屏幕变化是只挪高亮的三倍(实测 ~800 对 ~270 字节),mosh 传不过来,画面跟不上手指
const SETTLE: Duration = Duration::from_millis(180);
/// 挪完这么久之内,后台刷新来的新数据先攒着、视图也先不存:滑动途中不插进一次整屏重画
const HOLD: Duration = Duration::from_millis(600);
/// 看完回来后这么多秒内的输出不算「有新动静」(detach 时可能重画一下)
pub(crate) const SEEN_SLACK: u64 = 3;

/// 列哪些会话
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// 近几天内动过的(默认一天,`*` 换天数)
    Recent,
    All,
    /// 开着的:除了没在跑的(`^` 那些)都列,不管多久以前
    Working,
}

/// 只看某种状态(`!` `@` `#` `&` `^`,键同 agent-deck):和分组、范围叠加,搜索时不管。不存,关掉就没了
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    /// ● 在跑
    Run,
    /// ◐ 在等你:待确认、未读都算
    Wait,
    /// ○ 空闲
    Idle,
    /// ✕ 出错:会话还开着、卡在出错上的(要你动手)
    Err,
    /// ■ 没在跑:已退出、停掉的、关掉的、归档的;没在跑的出错(✕,多半是关掉的)也算
    Off,
}

impl Pick {
    fn hit(self, st: State) -> bool {
        match self {
            Pick::Run => st == State::Running,
            Pick::Wait => matches!(st, State::Ask | State::Unread),
            Pick::Idle => st == State::Idle,
            Pick::Err => st == State::Error,
            Pick::Off => matches!(st, State::Exited | State::Dead) || st.gone(),
        }
    }

    /// 键和列表空了时的那句(「没有运行中的 · ! 返回」)
    fn label(self) -> (char, &'static str) {
        match self {
            Pick::Run => ('!', t!("没有运行中的", "nothing running")),
            Pick::Wait => ('@', t!("没有在等你的", "nothing waiting for you")),
            Pick::Idle => ('#', t!("没有空闲的", "nothing idle")),
            Pick::Err => ('&', t!("没有出错的", "no errors")),
            Pick::Off => ('^', t!("没有停着的", "nothing stopped")),
        }
    }
}

/// 布局(预览放哪)。宽屏(桌面)和窄屏(手机)各记各的,`v` 只在当前这类屏幕放得下的几种里轮换:
/// 宽屏 右侧预览 → 下方预览 → 只看列表;窄屏 分屏 → 预览为主 → 只看列表
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preview {
    /// 默认:宽屏放右边,窄屏上下分屏
    Auto,
    /// 宽屏放下面
    Below,
    /// 窄屏「预览为主」:列表只留高亮附近几行,其余都给预览
    Focus,
    /// 只看列表(底下一行写着高亮的那个在干什么)
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Below,
    Right,
    Focus,
}

/// 这么宽算宽屏:预览放右边(列表 40 列 + 预览 60 列)。窄于这个是手机(竖屏 / 横屏)
const AUTO_RIGHT_W: u16 = 100;
/// 放下面至少要这么高
const BELOW_MIN_H: u16 = 16;
/// 「预览为主」时列表只留这么几行
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

/// 这类屏幕上 `v` 轮换的几种布局,按顺序
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

/// 开着的(w):没在跑的(^)以外都算
fn open(s: &Session) -> bool {
    !Pick::Off.hit(s.state)
}

/// 采集给的状态,叠上界面自己记的已读 / 未读(见 reads.rs):
/// 看过之后没再出输出的不算未读;按 u 标成未读的,闲着也算
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

/// 在等你:要你审批 / 选择(`!`),或者跑完了等你下一句(`●`)。出错的不算
fn waiting(s: &Session) -> bool {
    matches!(s.state, State::Ask | State::Unread)
}

fn epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 次要的字。不用终端的 DIM:浅底上淡到看不清(见 theme.rs)
pub fn dim() -> Style {
    theme::minor()
}

/// 按键提示里「键」的样式:粗体,说明是暗的,一眼扫到键
pub fn key_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// 按键提示之间的分隔
const SEP: &str = " • ";

/// 一串「键 说明」,用 • 隔开,从前往后排到 room 宽为止(越往后越不常用,放不下的就不放);返回片段和占了多宽
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

/// 底栏的一项:键、说明、放不下时丢的先后(大的先丢,0 = 不丢)
type MenuItem<'a> = (&'a str, &'a str, u8);

/// 底栏:几组按键,组内用 • 隔开、组间用 │;第 action 组是主操作,用强调色。整行居中。
/// 放不下时按优先级从低到高丢,直到放得下
fn menu_bar(groups: &[Vec<MenuItem>], action: Option<usize>, w: usize) -> Line<'static> {
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
        let main = action == Some(gi);
        for (i, (k, d, _)) in g.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(SEP, theme::faint()));
            }
            let (ks, ds) = if main { (theme::accent().add_modifier(Modifier::BOLD), theme::accent()) } else { (key_style(), dim()) };
            spans.push(Span::styled(k.to_string(), ks));
            spans.push(Span::styled(format!(" {d}"), ds));
        }
    }
    Line::from(spans).centered()
}

/// 状态图标,和 agent-deck 一套:● 在跑、◐ 在等(红 = 屏幕上在问你,黄 = 跑完了等你下一句)、○ 空闲、✕ 出错、■ 已退出 / 停掉的。
/// 颜色见 theme.rs;有颜色的都加粗,灰的不加(字形本身就和别的不一样)
pub fn icon(s: State) -> (&'static str, Style) {
    let p = theme::pal();
    let bold = |c| Style::new().fg(c).add_modifier(Modifier::BOLD);
    match s {
        State::Ask => ("◐", bold(p.red)),
        State::Running => ("●", bold(p.green)),
        State::Unread => ("◐", bold(p.amber)),
        State::Idle => ("○", Style::new().fg(p.muted)),
        State::Error => ("✕", bold(p.red)),
        // 没在跑的出错:红,不加粗 —— 不算开着的,别抢在跑的出错的眼
        State::Crashed => ("✕", Style::new().fg(p.red)),
        State::Exited | State::Stopped | State::Dead => ("■", Style::new().fg(p.muted)),
    }
}

/// 子代理那一行的图标:在跑和会话一个样;跑完的打勾(会话没有这个),出错、被停的照会话的画
fn agent_icon(p: Phase) -> (&'static str, Style) {
    let pal = theme::pal();
    match p {
        Phase::Running => icon(State::Running),
        Phase::Done => ("✓", Style::new().fg(pal.muted)),
        Phase::Failed => ("✕", Style::new().fg(pal.red)),
        Phase::Stopped => ("■", Style::new().fg(pal.muted)),
    }
}

/// 子代理那一行的键:「会话的键 \u{2} 子代理 id」(窗格行是 \u{1})
const AGENT_SEP: char = '\u{2}';

/// 会话 parent 派出去的第 k 个子代理拼成一行(`l` 展开才有,见 `App::lay_agents`)。它没有窗格、不是会话:
/// `sub` 记着它挂在谁下面(于是不算会话、跟着那一家进出、`h` 回得去),别的照父会话的抄(分组、工作区、置顶跟着走);
/// 最后一列两个数都填它跑了多久 —— 在跑的是跑到现在,跑完的是一共用了多久
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
        pid: 0,
        deck_id: String::new(),
        info: String::new(),
        worktree: false,
        pane: String::new(),
        sub: Some(Sub { parent: parent.name.clone(), parent_title: parent.title.clone(), win: 0, pane: k as u32, split: false }),
        subagents: vec![],
        subagent: Some(a.clone()),
        ..parent.clone()
    }
}

/// 账号名的颜色:Claude 橙、Codex 蓝(见 theme.rs)
pub fn agent_style(a: Agent) -> Style {
    match a {
        Agent::Claude => Style::new().fg(theme::pal().claude),
        Agent::Codex => Style::new().fg(theme::pal().codex),
        Agent::Other => dim(),
        Agent::None => Style::new(),
    }
}

/// 列表里账号那一列最多这么宽,再长截断
const WHO_MAX: usize = 8;

/// 按显示宽度截断并补齐(中文占两格)
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

/// 从左边截(路径留住最后几级:…/fun/app),不补齐
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

/// 列表里的一项:会话(可见列表里第几个)或分区标题(pos = None)
pub struct Row {
    pub pos: Option<usize>,
    /// 展开的分组标题:组里第一个会话在可见列表里第几个(高亮能停在标题上,见 `App::head`)
    pub head: Option<usize>,
    pub lines: Vec<Vec<(String, Style)>>,
}

/// 跳转模式的提示字母,和 agent-deck 一样(Vimium 默认,主键区)
const HINT_CHARS: &str = "sadfjklewcmpgh";

/// n 行各自的提示,照搬 agent-deck(Vimium)的算法:尽量一个字母,13 行以内全是单字母;
/// 两个字母的插在单字母中间。同样的行数给出和 agent-deck 一模一样的提示
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

/// 高亮能停的地方(跳转、j / k 都按这个走):会话(可见列表第几个),或展开的分组标题(记组里第一个会话)
#[derive(Clone, Copy, Debug, PartialEq)]
enum Jump {
    Row(usize),
    Head(usize),
}

/// 跳转目标,按屏幕上从上到下排:分区时展开的分组标题排在它的会话前面。
/// 置顶块的标题不算;折叠行本来就是一项(跳过去是展开)
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
        v.push(Jump::Row(pos));
    }
    v
}

enum HintMatch {
    /// 唯一对上第几行
    Hit(usize),
    /// 还是某个提示的开头,等下一个字母
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

/// 折叠的分区在列表里只剩一行(由该区第一个会话占位):分组 → (折起来几个, 其中几个在问你)
pub type Folds = HashMap<String, (usize, usize)>;

/// 列表够宽才写编号(手机上省下这 3 格给标题;数字照样能按,跳转也有提示字母)
const NUM_MIN_W: usize = 56;

/// 整个列表。分区时每个分组前插一行标题(置顶的在最上面,标题「★ 置顶」);一个会话一行,在问你的下面跟一行它问的。
/// desk = 桌面:总写编号,组和组之间空一行(目录、分支在预览头上,列表里不写)。
/// 跳转模式(jump = 已经按了的字母)下,对得上的会话把编号换成提示,分组标题的 ▾ 前面也写上。
/// TUI 和 `aca -l` 共用,保证两边长得一样。
pub fn list_rows(view: &[&Session], sections: bool, width: usize, jump: Option<&str>, folds: &Folds, who_w: usize, desk: bool) -> Vec<Row> {
    let mut rows = vec![];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in view.iter().filter(|s| !s.pinned && s.sub.is_none()) {
        *counts.entry(s.group.as_str()).or_default() += 1;
    }
    let nums = desk || width >= NUM_MIN_W;
    let trees = tree_marks(view, sections);
    // 桌面上标题那一栏按最长的标题定宽,账号和时间紧跟着,不被推到列表最右边
    let fit = if desk {
        view.iter()
            .zip(&trees)
            .map(|(s, (t, _))| t.width() + row_loc(s).width() + s.title.width() + agents_badge(s).width() + marks(s).width())
            .max()
            .unwrap_or(0)
    } else {
        usize::MAX
    };
    // 子代理那些行不编号:它们来来去去,编了号别的会话的号跟着跳
    let mut n = 0;
    let numbers: Vec<Option<usize>> = view
        .iter()
        .map(|s| {
            s.subagent.is_none().then(|| {
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
            rows.push(Row { pos: Some(pos), head: None, lines: vec![folded_line(&s.group, n, asks, width, hint)] });
            continue;
        }
        if new_sec {
            let (line, head) = match s.pinned {
                true => (pinned_line(), None),
                false => (section_line(&s.group, counts[s.group.as_str()], width, hint_of(Jump::Head(pos))), Some(pos)),
            };
            rows.push(Row { pos: None, head, lines: vec![line] });
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

/// 分区的键:置顶块一段,别的按分组
fn section_key(s: &Session) -> (bool, &str) {
    (s.pinned, if s.pinned { "" } else { s.group.as_str() })
}

/// 树枝最多画这么多层(一层两格),再深的和这一层并排着画:窄屏上标题不至于被挤没
const TREE_DEEP: usize = 4;

/// 每行图标前的树枝和它续行(它问的那句)上的竖线,像 tree 命令那样连起来:
/// 0 层 = 顶格的会话;会话的窗口 / 子代理 / 子会话比它深一层(「├ 」「└ 」),前面带着上面各层的「│ 」。几层都一样,最多画 TREE_DEEP 层。
/// 父会话就在上面那一串里才挂上去(父会话被过滤掉 / 在置顶块里时顶格)
fn tree_marks(view: &[&Session], sections: bool) -> Vec<(String, String)> {
    let mut levels = Vec::with_capacity(view.len());
    let mut secs = Vec::with_capacity(view.len());
    // 上一行所在的那一串:从顶格的会话一层层到它所在的会话
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
            // 会话:父会话在这一串里就挂在它下面,不在就顶格
            None => {
                let lvl = s.under().and_then(at).map_or(0, |p| p + 1);
                chain.truncate(lvl);
                chain.push(&s.name);
                lvl
            }
            // 窗口 / 子代理:比自己的会话深一层
            Some(x) => {
                let lvl = at(&x.parent).map_or(1, |p| p + 1);
                chain.truncate(lvl);
                lvl
            }
        };
        levels.push(lvl.min(TREE_DEEP));
        secs.push(sec);
    }
    // pos 后面、回到更浅的一层之前,还有没有 lvl 层的(有就画 ├,没有就 └)
    let more =
        |pos: usize, lvl: usize| (pos + 1..view.len()).take_while(|&j| secs[j] == secs[pos] && levels[j] >= lvl).any(|j| levels[j] == lvl);
    // 下一行挂在它下面
    let kids = |pos: usize| pos + 1 < view.len() && secs[pos + 1] == secs[pos] && levels[pos + 1] > levels[pos];
    let bar = |on: bool| if on { "│ " } else { "  " };
    let fork = |on: bool| if on { "├ " } else { "└ " };
    (0..view.len())
        .map(|pos| match levels[pos] {
            0 => (String::new(), bar(kids(pos)).to_string()),
            lvl => {
                // 上面各层的竖线:那一层后面还有兄弟才画
                let ups: String = (1..lvl).map(|l| bar(more(pos, l))).collect();
                let on = more(pos, lvl);
                (format!("{ups}{}", fork(on)), format!("{ups}{}{}", bar(on), bar(kids(pos))))
            }
        })
        .collect()
}

/// 分区标题上分组名的样式:粗体,不上色(颜色留给状态)
fn group_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// 折起来的分区:" ▸ work  3 ◐1",和展开的标题一个样子、只是 ▾ 换成 ▸;可以选中,回车 / o 展开
fn folded_line(group: &str, n: usize, asks: usize, width: usize, hint: Option<Vec<(String, Style)>>) -> Vec<(String, Style)> {
    let mut v = hint.unwrap_or_else(|| vec![(" ".into(), Style::new())]);
    let count = format!("  {n}");
    let ask = if asks > 0 { format!(" ◐{asks}") } else { String::new() };
    let name = clip(group_label(group), width.saturating_sub(4 + count.width() + ask.width())).trim_end().to_string();
    v.extend([("▸ ".into(), theme::faint()), (name, group_style()), (count, dim()), (ask, icon(State::Ask).1)]);
    v
}

/// 提示字母:已经按了的灰底,还要按的黄底(同 agent-deck),补够两格
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

/// 分区标题上的名字:普通 tmux 会话的分组记成 "-"
/// 会话在哪个工作区:自己分过的 → 子会话跟父会话(父会话也没分过就再往上,几层都一样)→ 跟最上面那个的组 → 1。窗口跟着自己的会话。
/// mine = false 不看它自己分过的,就是「不记的话会在哪」
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

/// 分组列表里每组按哪个字母:组名(最后一级优先)里第一个还没被占的字母,没有就按跳转的字母表顺着给;
/// q 留着关列表,空格是全部分组。组的顺序不变,字母就不变,按熟了不用看
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

/// " ▾ 分组名  4":这一区有几个跟在名字后面。跳转模式下提示字母写在 ▾ 前面(同折叠行)
fn section_line(group: &str, n: usize, width: usize, hint: Option<Vec<(String, Style)>>) -> Vec<(String, Style)> {
    let mut v = match hint {
        Some(h) => [h, vec![("▾ ".into(), theme::faint())]].concat(),
        None => vec![(" ▾ ".into(), theme::faint())],
    };
    let count = format!("  {n}");
    let name = clip(group_label(group), width.saturating_sub(4 + count.width())).trim_end().to_string();
    v.extend([(name, group_style()), (count, dim())]);
    v
}

/// 置顶那一块的标题
fn pinned_line() -> Vec<(String, Style)> {
    vec![(" ★ ".into(), theme::warn()), (t!("置顶", "Pinned").into(), group_style())]
}

/// 一个会话:(文字, 样式) 片段,一行;在问你的下面再挂一行它问的,和标题对齐。lead = 行首的编号 / 缩进(或跳转提示)
/// `who_w`:账号列宽,0 = 不显示(一个登记的会话都没有时)
/// `tree` / `cont`:图标前的树枝、续行上的竖线(见 tree_marks)
/// `fit`:标题栏最多这么宽(连树枝、窗口号),usize::MAX = 占满
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
    let (ic, ic_style) = s.subagent.as_ref().map_or_else(|| icon(s.state), |a| agent_icon(a.phase));
    // 子代理那一行:不是会话,没有便签、账号,字写灰的
    let own = s.subagent.is_none();
    let flags: String = [(s.noted, '✎'), (s.attached, '⇄')].iter().filter(|f| f.0 && own).map(|f| f.1).collect();
    let flag_style = if s.noted { theme::warn() } else { dim() };
    let when = s.when();
    let when = format!("{}{when}", " ".repeat(3usize.saturating_sub(when.width())));
    let who_cols = if who_w > 0 { who_w + 1 } else { 0 };
    let tail_w = 2 + 1 + who_cols + 3 + 1;
    let pre = tree;
    let loc = row_loc(s);
    let badge = agents_badge(s);
    // 至少 8 格只管屏幕太窄;按最长标题定的宽度不再各行各自撑开,不然窗口号长的那行时间会错开
    let title_w =
        width.saturating_sub(lead_w + pre.width() + 2 + tail_w + loc.width()).max(8).min(fit.saturating_sub(pre.width() + loc.width()));
    let mut first = lead;
    if !pre.is_empty() {
        first.push((pre.into(), theme::faint()));
    }
    first.extend([(ic.to_string(), ic_style), (" ".into(), Style::new()), (loc, dim())]);
    // 标题后面紧跟两样:子代理的 +N(绿)、插件挂的字(灰);标题先占,至少留它三分之二
    let marks = marks(s);
    let tail = badge.width() + marks.width();
    if tail == 0 {
        first.push((clip(&s.title, title_w), if own { Style::new() } else { dim() }));
    } else {
        let tw = s.title.width().min(title_w.saturating_sub(tail).max(title_w * 2 / 3));
        let shown = clip(&s.title, tw).trim_end().to_string();
        let room = title_w.saturating_sub(shown.width());
        let badge = clip(&badge, room).trim_end().to_string();
        // 插件的字补满这一栏(没有就是空格),后面的时间才对得齐
        let marks = clip(&marks, room.saturating_sub(badge.width()));
        first.extend([
            (shown, if own { Style::new() } else { dim() }),
            (badge, Style::new().fg(theme::pal().green)),
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
    // 额度用完 / 没登录停下的:原话,后面写能干什么
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

/// 因为账号(额度用完、没登录、欠费)停下的登记的 Claude / Codex 会话:换个账号就能接着跑(`X`)
fn can_rehome(s: &Session) -> bool {
    s.state == State::Error
        && s.error_kind == "account"
        && s.sub.is_none()
        && !s.deck_id.is_empty()
        && matches!(s.agent, Agent::Claude | Agent::Codex)
}

/// 标题前面那一小段:窗口 / 窗格写 tmux 的编号(#2 / #2.1);子代理没有编号,是别的子代理再起的写个 ↳
fn row_loc(s: &Session) -> String {
    match (&s.subagent, &s.sub) {
        (Some(a), _) if a.depth > 0 => format!("{}↳ ", "  ".repeat(a.depth as usize - 1)),
        (Some(_), _) | (None, None) => String::new(),
        (None, Some(x)) => format!("{} ", x.loc()),
    }
}

/// 会话那行标题后面的「 +2」:它派出去的子代理有几个还在跑(没有在跑的不写;`l` 展开看是哪几个)
fn agents_badge(s: &Session) -> String {
    match s.subagents.iter().filter(|a| a.running()).count() {
        0 => String::new(),
        n => format!(" +{n}"),
    }
}

/// 标题后面灰色的字:分叉出来的先写「 分自 X」,再是插件挂的(`aca meta set`):" [pr #12] [ci ✓]"。
/// 标题里已经带着原会话的名字(默认起的「X (分叉)」)就不再写一遍
fn marks(s: &Session) -> String {
    let fork = match &s.fork {
        Fork::Of(t) if s.sub.is_none() && !s.title.contains(t.as_str()) => tr!(" 分自 {t}", " fork of {t}"),
        _ => String::new(),
    };
    s.tokens.iter().fold(fork, |out, (k, v)| out + &format!(" [{k} {v}]"))
}

/// worktree 会话的分支(info 里的 Branch 那一行)
fn branch_of(s: &Session) -> Option<&str> {
    s.info.lines().find_map(|l| l.strip_prefix("Branch: ")).filter(|b| !b.is_empty())
}

/// 预览画哪个会话:高亮的那个;正滑动时先停在上一个画的(见 SETTLE)
fn preview_name(app: &mut App) -> Option<String> {
    let cur = app.current().map(|s| s.name.clone());
    let keep = app.sliding() && app.shown.as_ref().is_some_and(|n| app.all.iter().any(|s| &s.name == n));
    if !keep {
        app.shown = cur;
    }
    app.shown.clone()
}

/// 多久以前:刚刚 / 5 分钟前
fn ago(sec: u64) -> String {
    if sec == 0 { t!("刚刚", "just now").into() } else { tr!("{}前", "{} ago", crate::data::dur(sec)) }
}

/// 状态写成字:待确认 / 运行中 · 12 分钟 / 未读 · 5 分钟前 …
fn state_word(s: &Session) -> String {
    if let Some(a) = &s.subagent {
        let took = crate::data::dur((a.dur.max(0) / 1000) as u64);
        return match a.phase {
            Phase::Running => tr!("在跑 · {took}", "running · {took}"),
            Phase::Done => tr!("跑完了 · 用了 {took}", "finished · took {took}"),
            Phase::Failed => tr!("出错 · 用了 {took}", "failed · took {took}"),
            Phase::Stopped => tr!("被停了 · 跑了 {took}", "stopped · ran {took}"),
        };
    }
    match s.state {
        State::Ask => t!("待确认", "needs you").to_string(),
        State::Running if s.ran > 0 => tr!("运行中 · {}", "running · {}", crate::data::dur(s.ran)),
        State::Running => t!("运行中", "running").into(),
        State::Unread => tr!("未读 · {}", "unread · {}", ago(s.idle_for)),
        State::Idle => tr!("空闲 · {}", "idle · {}", ago(s.idle_for)),
        State::Error if s.error_kind == "account" => t!("出错 · 额度 / 登录", "error · quota / login").into(),
        State::Error => t!("出错", "error").into(),
        State::Exited => tr!("已退出 · {}", "exited · {}", ago(s.idle_for)),
        State::Stopped => t!("已停止", "stopped").into(),
        State::Dead => tr!("停着 · {}", "stopped · {}", ago(s.idle_for)),
        State::Crashed => t!("出错 · 没在跑", "error · not running").into(),
    }
}

/// 未提交的改动:「  没改动」/「  3 个文件 +12 −4」
fn stat_spans(r: &diff::Stat) -> Vec<(String, Style)> {
    if r.files == 0 {
        return vec![(t!("  没改动", "  no changes").into(), dim())];
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

/// 片段依次排进一行,超过 w 的截掉
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

/// 目录里的东西最宽排这么多格一栏(再长的截)
const LS_COL: usize = 28;

/// 高亮停在分组上时的预览:组的路径、分支、改动;路径从哪来;再空一行,目录里有什么
/// (子目录在前、带 /,排成几栏;放不下的、点开头的只数个数)。g 还没读回来时只有路径
fn group_pane(sp: &Spot, g: Option<&Glance>, w: usize, h: usize) -> Vec<Line<'static>> {
    let repo = g.and_then(|g| g.repo.as_ref());
    let mut tail: Vec<(String, Style)> = vec![];
    if let Some(r) = repo {
        if !r.branch.is_empty() {
            tail.push((format!("  {}", r.branch), dim()));
        }
        tail.extend(stat_spans(r));
    }
    // 放得下分支和改动,路径就用剩下的地方,从左边截
    let room = w.saturating_sub(tail.iter().map(|(t, _)| t.width()).sum::<usize>());
    let dir = create::tilde(&sp.dir);
    let dir = if room >= 8 { clip_left(&dir, room) } else { dir };
    let mut out = vec![fit_line([vec![(dir, Style::new().add_modifier(Modifier::BOLD))], tail].concat(), w)];
    let from = match &sp.from {
        None => t!("组的默认路径", "the group's default path").to_string(),
        Some(t) => tr!("「{t}」的目录 · 组没设默认路径", "“{t}”'s directory · the group has no default path"),
    };
    out.push(Line::styled(clip(&from, w).trim_end().to_string(), dim()));
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
        let t = if f.hidden > 0 {
            tr!("只有 {} 个点开头的", "only {} dotfiles", f.hidden)
        } else {
            t!("空目录", "empty directory").into()
        };
        out.push(Line::styled(t, dim()));
        return out;
    }
    let cw = items.iter().map(|(t, _)| t.width()).max().unwrap_or(1).min(LS_COL) + 2;
    let cols = (w / cw).max(1);
    // 最后一行留给「还有几个」
    let room = h.saturating_sub(out.len());
    let need = items.len().div_ceil(cols);
    let rows = if need + usize::from(f.hidden > 0) <= room { need } else { room.saturating_sub(1) };
    for r in 0..rows {
        let row: Vec<(String, Style)> = items.iter().skip(r * cols).take(cols).map(|(t, st)| (clip(t, cw - 2) + "  ", *st)).collect();
        out.push(fit_line(row, w));
    }
    let left = items.len().saturating_sub(rows * cols);
    let foot: Vec<String> =
        [(left > 0).then(|| tr!("还有 {left} 个", "{left} more")), (f.hidden > 0).then(|| tr!("{} 个点开头的", "{} dotfiles", f.hidden))]
            .into_iter()
            .flatten()
            .collect();
    if !foot.is_empty() && out.len() < h {
        out.push(Line::styled(foot.join(" · "), dim()));
    }
    out
}

/// 预览顶上第二行:它派出去的子代理几个在跑、几个完了,后面跟着这时候能按的键(收着写 l 展开,展开了写 h 收起)。没派的 = None
fn agents_line(s: &Session, open: bool, w: usize) -> Option<Line<'static>> {
    if s.subagent.is_some() || s.subagents.is_empty() {
        return None;
    }
    let run = s.subagents.iter().filter(|a| a.running()).count();
    let rest = s.subagents.len() - run;
    let many = noun(s.subagents.len() as u64, "subagent", "subagents");
    let key = if open { t!("h 收起", "h folds") } else { t!("l 展开", "l unfolds") };
    let text = match (run, rest) {
        (r, 0) => tr!("子代理 {r} 个在跑 · {key}", "{r} {many} running · {key}"),
        (0, d) => tr!("子代理 {d} 个跑完了 · {key}", "{d} {many} finished · {key}"),
        (r, d) => tr!("子代理 {r} 个在跑 · {d} 个完了 · {key}", "subagents: {r} running · {d} finished · {key}"),
    };
    Some(Line::styled(clip(&text, w).trim_end().to_string(), dim()))
}

/// 预览顶上几行:状态(照图标的颜色)、改动、目录、分支;便签;再空一行接屏幕内容。
/// 便签只在写了的时候占地方(最多三行,多的写还有几行);正在写的时候整段显示,带光标,最多占预览的一半
/// g:后台读好的真正干活的目录、分支、未提交的改动(见 `diff::glance`),还没读到时用登记的目录和分支
fn preview_head(s: &Session, g: Option<&Glance>, note: Option<&str>, editing: bool, w: usize, h: usize) -> Vec<Line<'static>> {
    // 子代理那一行:状态和用了几个工具;第二行说它是谁派的、怎么细看。它没有目录、改动、便签
    if let (Some(a), Some(x)) = (&s.subagent, &s.sub) {
        let (n, steps) = (a.tools, noun(a.tools as u64, "step", "steps"));
        let steps = if n == 0 { String::new() } else { tr!("   {n} 步", "   {n} {steps}") };
        return vec![
            fit_line(vec![(state_word(s), agent_icon(a.phase).1), (steps, dim())], w),
            Line::styled(
                clip(&tr!("「{}」的子代理 · ⏎ 看它的记录", "subagent of “{}” · ⏎ opens its record", x.parent_title), w)
                    .trim_end()
                    .to_string(),
                dim(),
            ),
        ];
    }
    let (_, sty) = icon(s.state);
    let word = state_word(s);
    let used = word.width();
    let dir = create::tilde(g.map(|g| g.work.dir.as_str()).filter(|d| !d.is_empty()).unwrap_or(&s.dir));
    let repo = g.and_then(|g| g.repo.as_ref());
    let branch = repo.map(|r| r.branch.clone()).filter(|b| !b.is_empty()).or_else(|| branch_of(s).map(String::from));
    // 改动统计紧跟状态(窄了也留得住),目录在它后面
    let stats = repo.map(stat_spans).unwrap_or_default();
    let mut tail: Vec<(String, Style)> = vec![];
    if let Some(b) = branch {
        tail.push((format!("  {b}"), dim()));
    }
    let width = |v: &[(String, Style)]| v.iter().map(|(t, _)| t.width()).sum::<usize>();
    // 放得下别的,目录就用剩下的地方,从左边截(最后一级最有用:…/app)
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
        let kind = if x.split { t!("分出来的窗格", "split pane") } else { t!("窗口", "window") };
        out.push(Line::styled(
            clip(&tr!("「{}」里的{kind} {}", "{kind} {1} in “{0}”", x.parent_title, x.loc()), w).trim_end().to_string(),
            dim(),
        ));
    } else {
        match &s.up {
            Up::Under(_, t) | Up::Apart(_, t) => {
                out.push(Line::styled(clip(&tr!("「{t}」的子会话", "child of “{t}”"), w).trim_end().to_string(), dim()))
            }
            Up::Gone => out.push(Line::styled(t!("子会话 · 父会话已删", "child session · parent deleted"), dim())),
            Up::Top => {}
        }
        match &s.fork {
            Fork::Of(t) => out.push(Line::styled(clip(&tr!("分自「{t}」", "forked from “{t}”"), w).trim_end().to_string(), dim())),
            Fork::Gone => out.push(Line::styled(t!("分叉出来的 · 原会话已删", "a fork · original deleted"), dim())),
            Fork::No => {}
        }
    }
    // 出错停下的:原话(最多两行);因为账号停下的,写上 X 能干什么
    if s.state == State::Error && !s.error.is_empty() {
        let red = Style::new().fg(theme::pal().red);
        for (i, l) in wrap_width(&s.error, w.saturating_sub(2)).into_iter().take(2).enumerate() {
            out.push(Line::from(vec![Span::styled(if i == 0 { "✕ " } else { "  " }, red), Span::styled(l, red)]));
        }
        if can_rehome(s) {
            out.push(Line::from(vec![
                button(t!("X 换账号", "X switch account"), theme::pal().accent),
                Span::raw(" "),
                Span::raw(
                    clip(
                        t!("对话整段带到别的账号,在原地接着跑", "carry the whole conversation on under another account, in place"),
                        w.saturating_sub(14),
                    )
                    .trim_end()
                    .to_string(),
                ),
            ]));
        }
    }
    let warn = theme::warn();
    if let Some(text) = note {
        let mut body: Vec<String> = text.split('\n').flat_map(|l| wrap_width(l, w.saturating_sub(2))).collect();
        if editing {
            out.push(Line::styled(t!("✎ 便签", "✎ Note"), warn.add_modifier(Modifier::BOLD)));
            // 光标在最后
            match body.last_mut() {
                Some(l) if l.width() < w.saturating_sub(2) => l.push('▏'),
                _ => body.push("▏".into()),
            }
            // 最多占预览的一半,屏幕内容总要露出来;留住最后几行(光标在那)
            let room = (h / 2).saturating_sub(out.len() + 1).max(1);
            if body.len() > room {
                let more = body.len() - room + 1;
                body.drain(..more);
                body.insert(0, tr!("… 上面还有 {more} 行", "… {more} more lines above"));
            } else {
                body.resize(room, String::new());
            }
            out.extend(body.into_iter().map(|l| Line::raw(format!("  {l}"))));
        } else {
            let more = body.len().saturating_sub(3);
            body.truncate(3);
            for (i, l) in body.into_iter().enumerate() {
                let mark = if i == 0 { Span::styled("✎ ", warn) } else { Span::raw("  ") };
                out.push(Line::from(vec![mark, Span::raw(l)]));
            }
            if more > 0 {
                out.push(Line::styled(tr!("  … 还有 {more} 行", "  … {more} more lines"), dim()));
            }
        }
    }
    if s.state.gone() {
        out.push(Line::raw(""));
        out.push(Line::styled(t!("⏎ 启动 · d 删除", "⏎ start · d delete"), dim()));
    }
    out.push(Line::raw(""));
    out
}

/// 按显示宽度折行(中文占两格);空行保留成一行空的
pub fn wrap_width(s: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        let cur = out.last_mut().unwrap();
        if cur.width() + cw > w.max(1) {
            // 英文的词不从中间断:正写到一个词的半截,就把这半截带到下一行(一个词比一行还宽时照样硬断)
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

/// 预览里去掉 Claude 的输入框和底部状态栏,只留对话内容。等你操作时不裁:那个框正是要看的。
fn preview_lines(s: &Session) -> Vec<Styled> {
    // 子代理的「屏幕」是照它的记录写的几行(见 subagent.rs),没有输入框可裁
    if s.subagent.is_some() {
        return ansi::parse_lines(&s.screen);
    }
    strip_prompt(ansi::parse_lines(&s.screen), s.state == State::Ask)
}

/// 去掉底部的输入框和它下面的状态栏;等你操作时不去(那个框正是要看的)
fn strip_prompt(mut styled: Vec<Styled>, asking: bool) -> Vec<Styled> {
    let lines: Vec<String> = styled.iter().map(ansi::plain).collect();
    if !asking {
        // 输入框 = 分隔线 + "❯ …";它下面还可能挂着状态栏、后台任务 / agent 列表,所以往上找 20 行
        // 上边框可能嵌着会话名:"────── 项目-主会话 ─"
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

/// 要复制什么(run 循环去取内容、写剪贴板)
#[derive(Debug, PartialEq)]
pub enum Clip {
    /// c:最后一条回复;登记的会话取 hook 记下的,拿不到就用屏幕上的字
    Last { profile: String, id: String, title: String, screen: String },
    /// B:路径 / 分支 / 会话 id
    Info { title: String, text: String },
    /// 弹窗交回来的一段字(`Fx::copy`):一个地址之类,底栏照原样说
    Text(String),
}

/// 全屏阅读(`z`):不进会话,只看它的输出,能往上翻历史
struct Reader {
    /// tmux 会话名
    name: String,
    /// 历史 + 屏幕,已去掉输入框
    lines: Vec<Styled>,
    /// 离底部几行(折行后算),0 = 跟着最新
    scroll: usize,
    /// 折行结果缓存,按 wrap_w 宽折的;内容换了就 dirty
    wrapped: Vec<Styled>,
    wrap_w: usize,
    dirty: bool,
    /// `?` 打开的按键说明,不是会话
    help: bool,
    /// 上次画出来一屏几行(快捷键弹窗翻页按这个翻)
    view_h: usize,
    /// `x`:看的是改动(git diff),不是输出
    diff: Option<DiffView>,
    /// 快捷键弹窗里刚按的键:亮起说明它的行
    hit: Option<crate::help::Hit>,
    /// 看的是插件的输出(视图、能力跑完的):`r` 重跑它
    plugin: Option<crate::caps::Req>,
}

impl Reader {
    /// 一屏 h 行时最顶上是折好的第几行(按键改过 scroll 还没重画也算得对)
    fn top(&self, h: usize) -> usize {
        let total = self.wrapped.len();
        (total - self.scroll.min(total.saturating_sub(h))).saturating_sub(h)
    }
}

/// `$` 计费面板(内容见 `bill`,数据见 `usage`)
#[derive(Default)]
struct BillView {
    /// 选中的时段(usage::RANGES 的下标)
    sel: usize,
    /// 离顶部几行
    scroll: usize,
    /// 上次画出来一屏几行(翻页按这个翻)
    view_h: usize,
    /// a 挑账号的浮窗开着
    pick: Option<BillPick>,
}

/// 挑账号的浮窗:第 0 行是「全部」,往下是各账号(见 `usage::Report::accounts`)
#[derive(Default)]
struct BillPick {
    sel: usize,
    /// 勾上的(多选);没勾就是只看高亮的那个
    marked: Vec<String>,
    /// 按了 v 在连选:从这一行连到高亮那行都算选上(再按 v 定下来,并进 marked)
    anchor: Option<usize>,
}

impl BillPick {
    /// 选上的账号:勾上的加上连选着的,按浮窗里的顺序(花得多的在前)
    fn chosen(&self, accts: &[crate::usage::Row]) -> Vec<String> {
        let range = self.anchor.map(|a| a.min(self.sel)..=a.max(self.sel));
        accts
            .iter()
            .enumerate()
            .filter(|(i, a)| self.marked.contains(&a.name) || range.as_ref().is_some_and(|r| r.contains(&(i + 1))))
            .map(|(_, a)| a.name.clone())
            .collect()
    }
}

/// 全屏阅读换成看改动(见 `diff`)。git 由 run 循环在后台跑,跑完 `set_diff` 送回来
struct DiffView {
    mode: Mode,
    /// 去哪儿找它真正干活的目录(见 `workdir`)
    w: Where,
    /// 每个文件的标题在 lines 里的下标,和折好行后的位置(n / p 跳)
    files: Vec<usize>,
    rows: Vec<usize>,
    /// lines 每行从折好的第几行开始(e 找屏幕顶上是哪一行)
    starts: Vec<usize>,
    /// 每行对应的 (第几个文件, 行号)、各文件的路径、仓库根:见 `diff::Diff`
    at: Vec<Option<(usize, usize)>>,
    paths: Vec<Option<String>>,
    top: String,
    /// 还没拿到结果
    loading: bool,
    /// 要重新读:刚打开 / 换了模式(定时刷新另算)
    want: bool,
}

/// d 确认过、先藏起来的一行
#[derive(Debug)]
struct Vanish {
    job: Job,
    /// 删完了,藏到这时候为止;None = 还在删
    until: Option<Instant>,
    /// 删之前在跑(^Z 撤回后再启动;藏起来以后列表里就查不到了)
    running: bool,
}

impl Vanish {
    /// 会话连同它的窗口一起藏;只关一格的只藏那一格(按窗格 id:会话名是整个会话的)。
    /// 关的是会话头一格时不藏:窗口不能没了头,关完一刷新,接着当头的那格就顶到会话那行上
    fn hides(&self, s: &Session) -> bool {
        let Job::Remove { t, pane } = &self.job else { return false };
        match pane {
            Some(p) => s.sub.is_some() && s.pane == *p,
            None => s.family() == t.name,
        }
    }
}

/// `Z` 会话记录(见 `trace`)。桌面两栏:左边是对话(一轮一块:你说的、Claude 最后说的、一行数字),
/// 右边是选中那一轮的中间过程(每次调用、每个工具一块);`→` 进右边一步步看,`←` 回来。
/// 手机放不下两栏:整屏是对话,`⏎` 整屏看这一轮的过程。记录由 run 循环在后台读,之后每次刷新只读新增的那截。
/// 起了子代理的那一步 `⏎` 进去,同样的两栏看它自己的记录,`Esc` 回来接着看
struct TraceView {
    /// tmux 会话名
    name: String,
    t: Option<crate::trace::Trace>,
    /// 读不出来的原因
    err: Option<String>,
    /// 要去后台读:刚打开 / 换了会话
    want: bool,
    loading: bool,
    /// 左边展开的:看全文的轮、抄过来的那一块
    open: HashSet<usize>,
    /// 右边展开的:调用(想的、说的全文)、工具(改动带上下文、文件内容、输出前 200 行)
    unfold: HashSet<usize>,
    /// unfold 改过几回(右边排好的过程跟着它重排)
    unfold_gen: u64,
    /// 左边的对话
    chat: Blocks,
    /// 右边的过程
    steps: Blocks,
    /// 在按右边(手机上 = 整屏是过程)
    in_steps: bool,
    /// 右边是哪一项的过程(换了就从头看)
    steps_of: Option<usize>,
    detail: Option<TraceDetail>,
    /// 上次画的是两栏还是手机的
    wide: bool,
    /// 左边那栏到哪一列为止(鼠标在哪边滚)
    split: u16,
    /// 排好的对话:轮 → 排好的行
    chat_cache: HashMap<usize, ChatLaid>,
    /// 排好的过程(右边那一项的)
    steps_cache: Option<StepsLaid>,
    /// 排的时候用的哪套颜色(`theme::pal()` 的地址):换了主题,排好的行里的颜色都旧了
    pal: usize,
    /// 进了哪个子代理:一路上起它的那些工具(空 = 会话自己的记录)
    path: Vec<usize>,
    /// 进子代理之前每一层看到哪了(回来接着看)
    saved: Vec<Level>,
    /// 从列表里子代理那一行开的:读好了直接进到这个子代理(它的 id)
    goto: Option<String>,
    /// 那样进来时落在第几层(0 = 不是那样进来的):在这一层按 Esc 直接回列表,不用一层层退
    home: usize,
}

/// 进子代理之前那一层的样子
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

/// 对话里排好的一轮。先不上色地排(量高度),露出来了再上色(见 `hl::plain`)
struct ChatLaid {
    w: usize,
    ver: u64,
    /// 全文(o)
    full: bool,
    lines: Vec<Styled>,
    /// 收着的时候截没截(没截的 o 了也一样)
    clipped: bool,
    /// 上了色的有几行(从头数;上色是从一段代码的头喂起的,只能省后面的)
    colored: usize,
}

/// 右边排好的过程:哪一项、宽度、版本、展开改过几回,一块块的
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
    /// 能不能 o
    fold: bool,
    /// 上了色的有几行(同 ChatLaid)
    colored: usize,
    /// 排出来取决于的东西(trace::step_sig):重排整轮时一样的就照旧用
    sig: u64,
}

/// 整屏看一项(⏎)
struct TraceDetail {
    node: usize,
    lines: Vec<crate::trace::Para>,
    wrapped: Vec<Styled>,
    wrap_w: usize,
    /// 离顶部几行
    scroll: usize,
    view_h: usize,
}

impl TraceDetail {
    fn new(node: usize) -> Self {
        TraceDetail { node, lines: vec![], wrapped: vec![], wrap_w: 0, scroll: 0, view_h: 0 }
    }
}

/// 一列能选的块(左边的对话、右边的过程)。按键照上次画的走:每块 (节点, 从第几行起, 到第几行前)
#[derive(Default)]
struct Blocks {
    items: Vec<(usize, usize, usize)>,
    /// 选中的节点
    sel: usize,
    /// 最顶上是第几行
    top: usize,
    view_h: usize,
    /// 贴着最新:新的来了选最后一块、贴底
    follow: bool,
    /// 选中的块刚展开:下回排好了尽量整块露出来(头不出屏)
    reveal: bool,
}

impl Blocks {
    fn at(&self) -> Option<usize> {
        self.items.iter().position(|x| x.0 == self.sel)
    }

    fn total(&self) -> usize {
        self.items.last().map_or(0, |x| x.2)
    }

    /// ↑↓:一块比一屏高时先在块里滚,滚到头了才换块;最后一块看完了接着跟最新
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

    /// 整列滚 d 行;选中的滚出去了就换成屏幕里的第一块
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

    /// 选中某一块,它的头放在最上面
    fn jump(&mut self, n: usize) {
        self.sel = n;
        if let Some(k) = self.at() {
            self.top = self.items[k].1;
        }
        self.follow = false;
    }

    /// 排好了:记下每块在哪;跟着最新就选最后一块、贴底;选中的没了(被折起来)就选它的上一层;整块在屏外就让它的头露出来
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

    /// 在看的那一层的记录(进了子代理就是子代理的)
    fn tr(&self) -> Option<&crate::trace::Trace> {
        self.t.as_ref().map(|t| t.at(&self.path))
    }

    /// 这一层在不在跑(子代理:会话在跑、它也还没干完)
    fn live(&self, live: bool, now: i64) -> bool {
        self.t.as_ref().is_some_and(|t| t.live_at(&self.path, live, now))
    }

    /// ⏎ 在起了子代理的那一步上:进去看它的记录(同样的两栏,停在最新),这一层看到哪了记下
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

    /// 回到上一层,接着看进来之前的那一处
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

    /// 读好了:贴着最新
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

    /// 进到 id 那个子代理:一路上起它的工具一层层进去(嵌套的要过它上面那几层),每一层停在起它的那一轮,Esc 回来就在那
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
        // 停在左边(派给它的话、它交回来的话),右边照样是它的过程;Esc 一下就回列表
        self.in_steps = false;
        self.home = self.path.len();
    }

    /// 读新增的那截(跟着最新的,画的时候自己会选到最后)
    fn tail(&mut self) {
        let Some(t) = self.t.as_mut() else { return };
        if !matches!(t.tail(), Ok(true)) {
            return;
        }
        if let Some(d) = self.detail.as_mut() {
            // 整屏开着:内容可能长了(工具有了结果),下次画的时候重读
            d.wrap_w = 0;
            d.lines.clear();
        }
    }

    /// 在按的那一列
    fn cur(&mut self) -> &mut Blocks {
        if self.in_steps { &mut self.steps } else { &mut self.chat }
    }

    /// 左边换到下一轮 / 上一轮(压缩那行不停)
    fn turn_step(&mut self, next: bool) -> bool {
        let Some(t) = self.t.as_ref().map(|t| t.at(&self.path)) else { return false };
        let Some(k) = self.chat.at() else { return false };
        let turnish = |x: &&(usize, usize, usize)| matches!(t.nodes[x.0].kind, crate::trace::Kind::Turn | crate::trace::Kind::Copied);
        let items = &self.chat.items;
        let to = if next { items.iter().skip(k + 1).find(turnish) } else { items[..k].iter().rev().find(turnish) };
        let Some(&(n, ..)) = to else { return false };
        self.chat.jump(n);
        // 右边换成这一轮的过程,从头看
        self.steps = Blocks { sel: n, ..Blocks::default() };
        self.steps_of = Some(n);
        true
    }

    /// 内容会不会变:在最后一轮里的(还在长)跟着记录读到哪、会话在不在跑变;前面的轮一般不会变,
    /// 除非它起的后台子代理还在长,或者后来被回退了
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

/// 等 y 确认的事
enum Confirm {
    Remove(Removal),
    /// why:为什么要问(正在跑 / 在等你选);force:现在不用
    Restart {
        t: Target,
        why: String,
        force: bool,
    },
    /// D:attached = 别处正连着,会被断开
    Stop {
        t: Target,
        attached: bool,
    },
    /// 会话里不止一格时的 D / R:只停(挪到队尾)/ 在原地重启这一格,见 park.rs。
    /// restart:是 R;why:为什么要问(正在跑…);next:停的是头一格,接着当头的那格
    Pane {
        t: Target,
        p: crate::park::Pane,
        row: String,
        restart: bool,
        why: String,
        next: Option<String>,
    },
    /// s:把便签发给它。pane:发到哪一格;key:便签的键
    Send {
        t: Target,
        pane: String,
        key: String,
        text: String,
    },
    /// A:接管这个普通 tmux 会话。plan:认出来的、打算怎么登记(给人看的就是它,y 之后照它做)
    Adopt {
        t: Target,
        plan: Box<aca_core::adopt::Plan>,
    },
}

/// 底栏里的单行输入
struct Prompt {
    ask: Ask,
    text: String,
    /// 输入框下面那行的提示(出错时),空 = 显示说明
    hint: String,
}

enum Ask {
    /// `r`:新标题
    Rename(Target),
    /// `G`:新分组名,回车后接着填路径
    NewGroup,
    /// `e`:便签(键见 `notes::key`);orig:打开时的便签,写完改了就记一步撤销
    Note { key: String, title: String, orig: String },
    /// `f`:分叉出来的会话叫什么(后面是 Claude 会话分叉前要核对的,见 `Job::Fork`)
    Fork(Target),
    /// S 里 ^R:工作区的新名字
    Space(char),
    /// 设置页:`^N` 的默认目录
    GroupDir,
    /// 分组上 `r`:新的分组名(可以带上级),回车后接着改默认目录
    GroupName(String),
    /// 接着填它的默认目录:group 改名成 to,was 是原来的默认目录(空 = 没记)
    GroupHome { group: String, to: String, was: String },
}

/// 按了 q / Q,等下一个键
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// q:切到哪个工作区
    Switch,
    /// Q:把高亮的会话送到哪个工作区
    Send,
}

/// 让出终端跑的外部程序(看改动里的 `l` / `e`,列表里的 `y`)
#[derive(Debug, PartialEq)]
pub enum External {
    /// lazygit,开在这个仓库
    Lazygit(String),
    /// $EDITOR 打开文件,停在这一行
    Edit { path: String, line: usize },
    /// 会话记录里的 e:编辑器只读地看,停在这一行;temp = 看完删掉的临时文件
    View { path: String, line: usize, temp: bool },
    /// 设置页用编辑器打开的配置文件(config.toml):还没有就先写上模板
    Config { path: String, template: String },
    /// yazi,开在这个会话真正干活的目录(让出终端时再找,和 T 一样)。
    /// tmux 弹窗里改成在会话里分一格放大的跑它(见 run 循环);`target` 空 = 会话没在跑,只能原地开
    Yazi(Split),
    /// 插件的窗格,界面不在 tmux 里:在前台跑(见 caps.rs)
    Plugin(crate::caps::Req),
}

/// ^G 交给编辑器写的便签
pub struct NoteEdit {
    key: String,
    title: String,
    /// 开始写之前的便签(见 `Ask::Note`)
    orig: String,
    pub text: String,
    /// 从底栏的便签编辑里按的:写完回到那里接着写
    from_prompt: bool,
}

/// T:在它的会话里新开一个窗口跑 shell(同 prefix c);弹窗里的 y:在它那格旁边分一格跑 yazi。
/// 目录是它真正干活的目录(run 循环去找目录、开窗口 / 分屏,再进去落在新的那格)
#[derive(Debug, PartialEq)]
pub struct Split {
    /// 分一格跑 yazi(放大占满,退出就关);否则新开窗口跑 shell
    pub yazi: bool,
    /// 进哪个 tmux 会话
    pub name: String,
    /// 分哪一格(yazi):窗格 id,没有就是那个会话当前那格
    pub target: String,
    pub w: Where,
    /// 左右分(桌面);手机上上下分(yazi)
    pub side: bool,
    /// 新的一格带的账号(`data::shell_env`)
    pub env: Vec<(String, String)>,
}

/// t:给这一行开临时终端(run 循环去找目录、开 shell)
#[derive(Debug, PartialEq)]
pub struct TermReq {
    /// 按行记:再按 t 回到同一个
    pub key: String,
    /// 它的 tmux 会话(取账号的环境)
    pub family: String,
    pub title: String,
    pub w: Where,
    /// 另外要带的环境(新账号登录时的配置目录);有就不按 family 取
    pub env: Vec<(String, String)>,
    /// 不开 shell,直接跑这条命令(新账号登录),它退出终端就关
    pub run: Option<String>,
    /// 登录终端:登录链接出来了弹出来挑浏览器(见 browser.rs)
    pub link: Option<crate::term::Catch>,
}

/// 高亮停在分组上时预览里写的:哪个组、它的路径;路径不是组的默认路径时,是跟着组里哪个会话(标题)
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    pub group: String,
    pub dir: String,
    pub from: Option<String>,
}

/// 分组在 terms / glances 里的键。tmux 会话名里没有「:」,和会话的撞不上
fn group_key(g: &str) -> String {
    format!("group:{g}")
}

fn is_group_key(k: &str) -> bool {
    k.starts_with("group:")
}

/// `H` 按下、还没挑工具时:交接谁、新会话照什么建(照它的目录、分组),菜单关掉就丢
struct Pass {
    t: Target,
    from: Box<Source>,
    draft: Draft,
    /// 它的便签带过去了:便签的键(新会话建成就删)
    note: Option<String>,
    /// 它还在跑 / 在等你选:菜单里提一句,两边会一起动同一处
    busy: bool,
}

/// `m` 挑分组要的
pub struct GroupPick {
    pub t: Target,
    /// 它现在的分组
    from: String,
    /// fzf 的候选:列表里的分区顺序,再是列表里没有的分组;不含它现在的
    pub cands: Vec<String>,
    /// 这个 profile 已有的全部分组(挪进没有的要先建)
    groups: Vec<String>,
}

/// `^S` 设置页的一行
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SetRow {
    Theme,
    Lang,
    Ime,
    Accounts,
    Tools,
    GroupDir,
    Config,
    /// 功能自己加的(`App::with_setting`):`Settings::extra` 里第几个
    Extra(usize),
    /// 写错了的(没有就不列)
    Problems,
}

impl SetRow {
    /// 行首的键;写错那行没有,↑↓ 挪过去。功能自己加的问 `Settings::key`
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
            SetRow::Ime => t!("桌面切英文", "desktop English input"),
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

/// `^S` 的设置页。显示的值打开时现读(改了、config.toml 被改了再读),不每帧读文件
#[derive(Clone, Debug)]
struct Settings {
    page: SetPage,
    /// 主页高亮第几行
    sel: usize,
    theme: String,
    lang: Lang,
    ime: bool,
    group_dir: String,
    bad: Vec<String>,
    accts: Vec<crate::accounts::Account>,
    /// 功能自己加的行,打开时问来的
    extra: Vec<ExtraRow>,
}

/// 功能自己加的一行(`overlay::Setting`)打开设置页时的样子
#[derive(Clone, Debug)]
struct ExtraRow {
    key: char,
    name: &'static str,
    value: Vec<Span<'static>>,
    mark: &'static str,
}

impl Settings {
    fn rows(&self) -> Vec<SetRow> {
        let mut v = vec![SetRow::Theme, SetRow::Lang, SetRow::Ime, SetRow::Accounts, SetRow::Tools, SetRow::GroupDir, SetRow::Config];
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

/// 配色列表(`^S` 里的「主题」,见 `open_themes`):所有主题(每套的颜色先备好,移到哪套就把整个界面换成哪套)、高亮第几个、
/// 打开前用的那套(Esc 换回去)、记在 config 里的那套(打 ●)
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
    /// 选中的是可见列表(view)里第几个;画的时候换算成含分区标题的行号,放进 disp
    list: ListState,
    /// 实际画出来的列表(含分区标题)的滚动位置
    disp: ListState,
    /// 画出来的每一项:对应 view 里第几个(标题 = None)、是哪个组的标题(见 `Row::head`)、占几行。点击时用
    drawn: Vec<(Option<usize>, Option<usize>, u16)>,
    selected: Option<String>,
    scope: Scope,
    /// w / a 进「开着的」/「全部」之前的范围,再按一次回到这里
    back: Scope,
    /// ! @ # & ^ 挑的状态
    status: Option<Pick>,
    /// 只看这个分组(None = 全部分组)
    group: Option<String>,
    /// 当前分组里一共几个(顶栏的分母)
    base_len: usize,
    /// 「近几天」一个都没有,退回显示了全部 —— 顶栏要如实写「全部」
    fell_back: bool,
    /// 出错了的插件(钩子、定时上次没跑成,gateway 的服务停下了):顶栏右边一格 ⚠,P 里看是哪个。后台跟着列表一起重读
    pub plugin_alerts: Vec<aca_core::plugin::health::Problem>,
    filter: String,
    filtering: bool,
    digits: String,
    digits_at: Instant,
    /// 宽屏 / 窄屏各自的布局(见 Preview)
    preview: Preview,
    preview_narrow: Preview,
    /// 刚按过 v:底栏亮一会儿「布局 分屏 → 预览为主 → 只看列表」
    layout_at: Option<Instant>,
    msg: Option<(String, Instant)>,
    /// msg 在底栏留多久(一般 3 秒;上次界面怎么没的那句留久一点)
    msg_ttl: Duration,
    /// 上一帧的屏幕大小,`v` 的提示里说「自动」现在落在哪边
    screen: Rect,
    pins: Pins,
    notes: Notes,
    /// 会话都分在哪个工作区、工作区的名字(见 spaces.rs)
    spaces: Spaces,
    /// 现在看的工作区
    space: char,
    /// 切到现在这个之前待的工作区:` 回那
    last_space: Option<char>,
    /// 按了 q / Q,等下一个键(工作区的键)
    pending: Option<Pending>,
    /// S 打开的工作区列表:高亮的第几个
    space_menu: Option<usize>,
    /// Tab 打开的分组列表:高亮的第几行(第 0 行是全部分组)
    group_menu: Option<usize>,
    /// 配色列表(见 `open_themes`)
    theme_menu: Option<ThemeMenu>,
    /// `^S` 的设置页
    settings: Option<Settings>,
    /// 从设置页进去的(主题列表、改目录、加账号、编辑器):做完回到设置页的这里
    settings_back: Option<Settings>,
    /// 打开 aca 时 config.toml 读不了的原因(设置页的写错里列出来,那时再拼成一句)
    pub sync_failed: Vec<String>,
    /// 设置页改了输入法:run 循环按新设置重新决定管不管
    pub ime_changed: bool,
    /// 要在后台跑的插件能力 / 视图 / 窗格(`:` 菜单、阅读页里的 r):run 循环取走,一个一个线程
    pub plugin_q: Vec<crate::caps::Req>,
    /// 换到现在这个分组之前看的(None = 全部分组):分组列表打开时高亮停在它,Tab ⏎ 就是换回去
    prev_group: Option<Option<String>>,
    /// 每个工作区最后高亮的会话:切回去时停在那
    space_sel: HashMap<char, String>,
    /// E:进会话前要粘进它输入框的便签(键, 内容)
    pub fill: Option<(String, String)>,
    /// 高亮上一次被挪动的时刻,和它离前一次挪动隔了多久(见 SETTLE)
    moved: Option<(Instant, Duration)>,
    /// 预览现在画的是哪个会话
    shown: Option<String>,
    /// ^G:要交给编辑器写的便签
    pub edit_note: Option<NoteEdit>,
    /// 已读 / 未读(见 reads.rs):从 aca 进去看过的记下时刻,之后没有新输出的不算 ◐,`i` 也跳过
    reads: Reads,
    /// 跳转模式(空格进入)下已经按了的提示字母;None = 不在跳转模式
    jump: Option<String>,
    /// 高亮停在这个分组的标题上(跳转跳过去的),不在它第一个会话上:t / N / n 对着组做。
    /// 这时 `list` 选的还是组里第一个会话(滚动、刷新都跟着它);挪高亮就回到会话上
    head: Option<String>,
    reader: Option<Reader>,
    trace: Option<TraceView>,
    /// `$` 计费面板开着时:选中的时段、滚到哪
    bill: Option<BillView>,
    /// 计费的数(后台读的):最近一份和拿到的时刻,关了再开先显示它
    bill_data: Option<crate::usage::Report>,
    bill_got: Option<Instant>,
    /// 已经请后台去读、还没回来;按了 r 要立即重读
    bill_loading: bool,
    bill_want: bool,
    /// 只看哪些账号(空 = 全部),关了面板再开还是它
    bill_only: Vec<String>,
    /// 看改动看的是未提交的还是连同提交的(`m` 换,换会话时沿用)
    diff_mode: Mode,
    /// 看改动里按了 l / e:run 循环让出终端去跑
    pub external: Option<External>,
    /// 按了 T(弹窗里的 y):run 循环去开窗口(分屏)
    pub split: Option<Split>,
    /// t 开的临时终端,按行;Esc 收起后留着,里面 exit 了才去掉
    pub terms: HashMap<String, crate::term::Term>,
    /// 正显示在列表下面、按键都给它的那个
    pub term_on: Option<String>,
    /// 按了 t、还没开的:run 循环去开
    pub term_req: Option<TermReq>,
    /// 预览顶上的改动统计,按会话名(后台读,见 `glance_request`)
    glances: HashMap<String, Glance>,
    /// 上次去读统计的是哪个会话(换了高亮就再读)
    glance_for: Option<String>,
    /// 高亮停着的分组的路径(见 `spot`)
    spot: Option<Spot>,
    pub clip: Option<Clip>,
    /// 折起来的分组(记进视图)
    folded: BTreeSet<String>,
    /// 这次 rebuild 真正折起来的:分组 → (几个, 几个待确认);过滤时不折,就是空的
    folds: Folds,
    /// `l` 展开了子代理的会话(键):它派出去的各占一行(见 subagent.rs)。默认都收着,只在会话那行写个 +N
    agents_open: HashSet<String>,
    /// 全屏阅读换了会话 / 刚打开,要去 tmux 抓历史(抓取由 run 循环做,单元测试里没有 tmux)
    pub reader_load: bool,
    /// 按了 N,正在编辑新会话的路径
    new_draft: Option<Draft>,
    list_area: Rect,
    /// 顶栏左边的标签:点一下开分组列表
    chip_area: Rect,
    pub quit: bool,
    /// 跟随着(`--follow`,见 follow.rs):高亮挪到哪,这台客户端就切到哪
    pub following: Option<Follow>,
    /// ^L:退出界面后换成(新装的)自己接着跑,见 `relaunch`
    pub relaunch: bool,
    pub attach: Option<String>,
    /// attach 前先切到这一格(窗格 id):会话本身落在主窗格,子行落在那个窗口 / 窗格
    pub focus: Option<String>,
    /// ⏎ 在停掉的会话上:run 循环先启动它,起来了再 attach。bool = 它是 shell 会话(不用等程序起来)
    pub start: Option<Target>,
    /// ⏎ 在 D 停着的那一格上:run 循环先在原地再启动它(见 park.rs),再 attach 过去
    pub respawn: Option<(String, crate::park::Pane)>,
    /// 每个会话最后一次有动静的时刻(活着时看到的)。停掉后 tmux 活动时间没了,拿这个接着算「多久没动」,
    /// 免得刚按 D 就因为「超过 1 天」被藏起来
    last_live: HashMap<String, u64>,
    pub create: Option<NewReq>,
    /// 新建时按了 ↓ / ^F:run 循环去开 fzf 找目录(要让出终端,单元测试里没有)
    pub pick: bool,
    /// `n` 的工具菜单从哪读(`with_tools`);默认是空的,单元测试不碰真实文件
    tools_loader: fn() -> Result<Vec<Tool>, String>,
    /// 账号页(`with_accounts`,见 acctpage.rs;给 true = 一开就进加账号);单元测试里没有,不碰真实的配置
    accounts_page: Option<fn(bool) -> Box<dyn Overlay>>,
    /// `n` 打开的工具菜单、高亮的第几个
    tool_menu: Option<(Vec<Tool>, usize)>,
    /// `H` 打开的工具菜单是给交接挑的
    handoff: Option<Pass>,
    /// `H` 从哪认出要交接的对话(`with_handoff`);默认认不出,单元测试不碰真实文件
    source_loader: fn(&Session) -> Result<Source, String>,
    /// `X` 换账号的框:各账号的额度、问登录着没有(`with_rehome`);默认什么都没有、问不出,单元测试不碰真实文件
    rehome_quotas: fn(Agent) -> Vec<crate::usage::Quota>,
    rehome_ask: fn(Agent, &str) -> crate::accounts::Login,
    rehome_background: bool,
    /// 等 y 确认的:删除(`d`)/ 重启(`R`)/ 停止 / 发便签。确认了放进 `job`,由 run 循环放到后台去做
    confirm: Option<Confirm>,
    /// 底栏里正在输入的:`r` 新标题 / `G` 新分组名
    prompt: Option<Prompt>,
    /// 要在后台做的(重启 / 改名 / 挪分组 / 删除…),run 循环放进后台线程
    pub job: Option<Job>,
    /// 后台正在做的,底栏一直显示;做完才能再来一个(删除不算,见 `vanish`)
    busy: Option<String>,
    /// d 确认了就先从列表里拿掉(`drop_vanished`),后台再删;可以接着删别的。删失败放回来
    vanish: Vec<Vanish>,
    /// 后台在重启的正是高亮的会话(它的 tmux 名会变,做完让高亮跟过去)
    follow: bool,
    /// ⇧↑ ⇧↓ 挪过、还没写进登记表的:`Job::Order`(会话)/ `Job::GroupOrder`(分组)。列表先照着挪好(见 `apply_reorders`),
    /// 空下来就按先后一个个交给后台写,写完一个去掉一个
    reorders: VecDeque<Job>,
    /// 排头那个正在写
    reorder_writing: bool,
    /// 按了 m:run 循环开 fzf 挑分组(要让出终端,单元测试里没有)
    pub pick_group: Option<GroupPick>,
    /// 「近几天」的几天(DAYS 里的一个)
    days: u64,
    /// 新建时按路径定分组的线索从哪来(`with_group_hints`);默认没有,单元测试不碰真实的库
    group_hints: fn() -> crate::data::GroupHints,
    /// `A` 接管前怎么认这个 tmux 会话(`with_adopt`):给 `$会话号` / `%窗格号`;默认认不了,单元测试不碰真实的 tmux 和进程
    adopt_planner: fn(&str) -> Result<aca_core::adopt::Plan, String>,
    /// `^N` 的默认目录从哪来(`with_new_group_dir`);默认 ~/workspace,单元测试不碰真实的配置
    new_group_dir: fn() -> String,
    /// 设置页读写的(`with_settings`):输入法开没开、写错的、改一项、账号;默认不读不写,单元测试不碰真实的配置
    ime_auto: fn() -> bool,
    config_bad: fn() -> Vec<String>,
    config_set: fn(&str, &str, toml::Value) -> Result<(), String>,
    accounts_lister: fn() -> Vec<crate::accounts::Account>,
    /// 功能自己的弹窗(见 overlay.rs),键给最上面那层;`M` 的 MCP 是第一个
    overlays: Vec<Box<dyn Overlay>>,
    /// 设置页里功能自己加的行(`with_setting`);默认没有,单元测试不碰真实的系统
    setting_rows: Vec<Box<dyn Setting>>,
    /// `M` 从哪读 MCP(`with_mcp`);默认是空的,单元测试不碰真实文件
    mcp_loader: fn(&Session) -> mcp::Info,
    /// `P` 开的插件页(`with_plugins`,见 plugins.rs);单元测试里没有,不碰真实的登记表
    plugins_page: Option<fn() -> Box<dyn Overlay>>,
    /// 分组上 `d` 的那一页从哪读登记表里的分组(`with_groups`);默认是空的,单元测试不碰真实的库
    groups_loader: fn() -> Vec<groups::Row>,
    pub refresh: bool,
    /// ^Z 能撤的,最近做的在最后(见 undo.rs)
    undo: Vec<Undo>,
    /// d 删了、还没真删的(见 trash.rs):列表里不列
    pub trash: Trash,
    /// 后台在跑的是 ^Z 的撤销:(那个活, 撤的哪步, 做成了说什么;None = 用它自己的话)。失败了那步放回去
    reverting: Option<(Job, Undo, Option<String>)>,
    /// 撤销记录挤掉了一个删除:run 循环去真删
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
            back: Scope::Recent,
            status: None,
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
            pins: Pins::default(),
            notes: Notes::default(),
            spaces: Spaces::default(),
            space: spaces::HOME,
            last_space: None,
            pending: None,
            space_menu: None,
            group_menu: None,
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
            handoff: None,
            source_loader: |_| Err(t!("找不到它的对话记录", "can't find its conversation record").into()),
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
            purge: false,
        };
        app.set_sessions(sessions);
        app
    }

    /// 接上置顶文件(不接 = 置顶只在内存里,单元测试用)
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

    /// 设置页加一行(排在自带的后面)
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

    /// `X` 换账号的框:额度从哪读、登录怎么问、放不放到后台问
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

    /// 接上便签文件(不接 = 只在内存里)
    pub fn with_notes(mut self, notes: Notes) -> Self {
        self.notes = notes;
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    /// 只看这个工作区(`aca -l` 用上次停在的那个)
    pub fn in_space(mut self, c: char) -> Self {
        self.space = c;
        self.rebuild();
        self
    }

    /// `aca 关键词` / `-n`:标题、分组、tmux 名里带这个词的,和 / 一样只找这个工作区的
    pub fn matching(&self, kw: &str) -> Vec<&Session> {
        let k = kw.to_lowercase();
        self.all
            .iter()
            .filter(|s| s.space == self.space && format!("{} {} {}", s.title, s.group, s.name).to_lowercase().contains(&k))
            .collect()
    }

    /// 接上删了等真删的记录(不接 = 只在内存里)
    pub fn with_trash(mut self, trash: Trash) -> Self {
        self.trash = trash;
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    /// 接上已读 / 工作区的文件(不接 = 只在内存里)
    pub fn with_marks(mut self, reads: Reads, spaces: Spaces) -> Self {
        (self.reads, self.spaces) = (reads, spaces);
        let v = std::mem::take(&mut self.all);
        self.set_sessions(v);
        self
    }

    pub fn set_sessions(&mut self, mut v: Vec<Session>) {
        // d 删了、等着真删的(这个或另一个 aca 删的)不列
        self.trash.reload();
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
        self.pins.reload(); // 另一个 aca(手机 / 桌面)可能刚改过
        self.notes.reload();
        self.spaces.reload();
        self.reads.reload();
        for s in &mut self.all {
            s.state = read_state(s, &self.reads);
        }
        self.apply_pins();
        // 选中的分组里(这个工作区的)会话都没了 → 回到全部分组
        if self.group.as_ref().is_some_and(|g| !self.all.iter().any(|s| &s.group == g && s.space == self.space)) {
            self.group = None;
        }
        self.rebuild();
    }

    /// 展开了的会话(`agents_open`):它派出去的子代理各拼一行,挂在它下面(排在哪由 `sort_sessions` 定,之后要 `apply_pins`)。
    /// 先把上回拼的去掉再拼,所以展开 / 收起之后再调一次就行。子代理都不列了的会话(跑完、换了一轮)不再记着展开:
    /// 下回再派,还是先收着
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
        // 高亮停着的那个子代理不列了:回到它的会话上
        if let Some(n) = self.selected.as_ref().filter(|n| n.contains(AGENT_SEP))
            && !self.all.iter().any(|s| &s.name == n)
        {
            self.selected = n.split(AGENT_SEP).next().map(String::from);
        }
    }

    /// 列表里编号 n 的那一行在可见列表第几个(子代理那些行不编号)
    fn numbered(&self, n: usize) -> Option<usize> {
        self.view.iter().enumerate().filter(|&(_, &i)| self.all[i].subagent.is_none()).nth(n.checked_sub(1)?).map(|(p, _)| p)
    }

    /// 高亮停在子代理那一行:它没有窗格、不是会话,只能看。⏎ / Z 看它的记录;走动、换范围、开面板的键照常
    /// (返回 false,接着走列表的键);对着会话做的键(改名、删除、便签、看输出……)不理,说一声
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
            KeyCode::Char(c) => c.is_ascii_digit() || " kjJKgGi/aw!@#&^{}hloOq[]`Sv W*?$P".contains(c),
            _ => false,
        };
        if !pass {
            self.flash(t!("子代理只能 ⏎(看它的记录)", "a subagent: only ⏎ (its record)"));
        }
        !pass
    }

    /// 这个工作区里的分组,按分区的顺序(和列表里从上到下一致)
    fn groups(&self) -> Vec<String> {
        let mut v: Vec<(usize, &str)> = self.all.iter().filter(|s| s.space == self.space).map(|s| (s.slot.0, s.group.as_str())).collect();
        v.sort();
        v.dedup();
        v.into_iter().map(|(_, g)| g.to_string()).collect()
    }

    /// 全部分组时按分组分区(插标题);只看一个分组时不用
    pub fn sectioned(&self) -> bool {
        self.group.is_none()
    }

    /// 轮换:全部分组 → 各分组(按分区顺序)→ 全部分组
    fn cycle_group(&mut self, d: isize) {
        let gs = self.groups();
        let cur = self.group.as_ref().and_then(|g| gs.iter().position(|n| n == g)).map_or(0, |p| p as isize + 1);
        let next = (cur + d).rem_euclid(gs.len() as isize + 1);
        self.set_group(if next == 0 { None } else { Some(gs[next as usize - 1].clone()) });
    }

    /// ⇧Tab:换回刚才看的分组(同 Tab ⏎),再按又换回来
    fn back_group(&mut self) {
        match self.prev_group.clone() {
            Some(Some(g)) if !self.groups().contains(&g) => {
                self.flash(&tr!("「{}」里已经没有会话了", "no sessions left in {}", group_label(&g)))
            }
            Some(g) => self.set_group(g),
            None => self.flash(t!("还没换过分组", "no group switched yet")),
        }
    }

    /// 换成只看这个分组(None = 全部分组),记下换之前看的
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

    /// 开着弹窗 / 输入框 / 全屏的页面(键都给它,顶栏标签没画出来)
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
            || self.theme_menu.is_some()
            || self.settings.is_some()
            || self.confirm.is_some()
            || self.pick_group.is_some()
    }

    /// 分组列表的行:(字母, 分组),第 0 行是全部分组(按空格)
    fn group_rows(&self) -> Vec<(char, Option<String>)> {
        let gs = self.groups();
        let keys = group_keys(&gs);
        std::iter::once((' ', None)).chain(keys.into_iter().zip(gs.into_iter().map(Some))).collect()
    }

    /// Tab:这个工作区的分组列表,高亮停在上一个看的分组(没换过就停在现在这个)
    fn open_group_menu(&mut self) {
        let rows = self.group_rows();
        let at = |g: &Option<String>| rows.iter().position(|(_, x)| x == g);
        self.group_menu = Some(self.prev_group.as_ref().and_then(at).or_else(|| at(&self.group)).unwrap_or(0));
    }

    /// 分组列表里:↑↓ 挑、⏎ 换过去,直接按字母也换,空格 = 全部分组;Esc / Tab / q 关
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

    /// 置顶的按置顶先后排在最上面,其余照常排。先整体重排一遍,刚取消置顶的才会回到原位
    fn apply_pins(&mut self) {
        self.apply_group_reorders();
        sort_sessions(&mut self.all);
        self.apply_reorders();
        // 子窗格跟着自己的会话走(一起置顶);挂着的子会话自己没置顶的,跟着上面最近一个置顶了的走(父会话、再上一层……)
        let hangs = hangs(&self.all);
        let rank = |pins: &Pins, s: &Session| pins.rank(s.family()).or_else(|| s.ancestors(&hangs).find_map(|p| pins.rank(p)));
        let space: Vec<char> = self.all.iter().map(|s| space_of(&self.all, &self.spaces, s, true)).collect();
        for (s, c) in self.all.iter_mut().zip(space) {
            s.pinned = rank(&self.pins, s).is_some();
            s.noted = self.notes.get(&notes::key(s)).is_some();
            s.space = c;
        }
        let pins = &self.pins;
        self.all.sort_by_key(|s| rank(pins, s).unwrap_or(usize::MAX));
    }

    /// 还没写进登记表的 ⇧↑ ⇧↓ 先在列表里挪好:整家(窗口、挂在下面的各层子会话)搬到那个会话整家的紧前面 / 紧后面。
    /// 写完之前每次刷新读到的还是旧顺序,照样再挪一遍;已经写进去的再挪一遍也不变
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

    /// 还没写进登记表的分组上 ⇧↑ ⇧↓:改分组的先后(slot 的第一格),排的时候跟着走。
    /// 分组连着子分组搬到 by 那一家的紧前面 / 紧后面;「其他 tmux 会话」一直沉底。再挪一遍也不变
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

    /// ⇧↑ ⇧↓:和同一组里上 / 下一个会话换位置(子会话在同一个父会话下面换),写进登记表里的组内顺序。
    /// 只看列表里看得到的:中间隔着的(归档的、别的工作区的、筛掉的)留在另一边
    fn reorder(&mut self, d: isize) {
        if let Some(g) = self.group_row().map(String::from) {
            return self.reorder_group(&g, d);
        }
        let Some(p) = self.list.selected().filter(|&p| p < self.view.len()) else { return };
        let s = &self.all[self.view[p]];
        let m = match () {
            _ if s.sub.is_some() => {
                Some(tr!("{} 是窗口,只能 ⏎ / d / D", "{} is a window: only ⏎ / d / D", s.sub.as_ref().map(|x| x.loc()).unwrap_or_default()))
            }
            _ if s.deck_id.is_empty() => {
                Some(t!("普通 tmux 会话不能挪(没在 aca 里登记)", "ordinary tmux sessions can't be moved (not registered in aca)").into())
            }
            _ if self.pins.rank(&s.name).is_some() => Some(t!("置顶的不按组排", "pinned ones aren't ordered by group").into()),
            _ => None,
        };
        if let Some(m) = m {
            return self.flash(&m);
        }
        // 能换的:同一个 profile、同一组、同一层(顶格的和顶格的,子会话和同一个父会话下面的),自己没置顶
        let peer = |o: &Session| {
            o.sub.is_none()
                && !o.deck_id.is_empty()
                && o.profile == s.profile
                && o.group == s.group
                && o.under() == s.under()
                && self.pins.rank(&o.name).is_none()
        };
        // 出了这一组(子会话:出了父会话那一家)就不往下找了;兄弟下面挂着的各层子会话还在这一家里,跳过去接着找
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
        let Some(by) = hit else {
            return self.flash(if d < 0 {
                t!("已在组内最上", "already at the top of the group")
            } else {
                t!("已在组内最下", "already at the bottom of the group")
            });
        };
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        self.queue_reorder(Job::Order { t, by, after: d > 0 });
    }

    /// 分组上 ⇧↑ ⇧↓:和上 / 下一个看得到的同级分组换位置(连着各自的子分组),写进登记表里的分组先后。
    /// 子分组出不了父分组;中间隔着看不到的(没有会话的、别的工作区的、筛掉的)留在另一边
    fn reorder_group(&mut self, g: &str, d: isize) {
        if g == "-" {
            return self.flash(t!("普通 tmux 会话那一区不能挪", "the ordinary tmux sessions section can't be moved"));
        }
        let secs: Vec<&str> =
            self.section_starts().into_iter().map(|p| &self.all[self.view[p]]).filter(|s| !s.pinned).map(|s| s.group.as_str()).collect();
        let Some(cur) = secs.iter().position(|h| *h == g) else { return };
        let depth = g.split('/').count();
        let parent = |p: &str| p.rsplit_once('/').map(|x| x.0.to_string());
        // 分区 h 在 g 这一层上是哪个分组(g 自己 = h 是它的子分组);出了 g 的父分组 = None
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
        let Some(by) = hit else {
            let m = match (parent(g), d < 0) {
                (Some(p), true) => tr!("已在 {p} 里最上", "already at the top of {p}"),
                (Some(p), false) => tr!("已在 {p} 里最下", "already at the bottom of {p}"),
                (None, true) => t!("分组已在最上", "the group is already at the top").into(),
                (None, false) => t!("分组已在最下", "the group is already at the bottom").into(),
            };
            return self.flash(&m);
        };
        self.queue_reorder(Job::GroupOrder { group: g.to_string(), by, after: d > 0 });
    }

    /// ⇧↑ ⇧↓ 记下来,列表马上照着挪。连着挪同一个(会话 / 分组)只留最后一下(只动它自己,别的之间的先后不变);
    /// 正在写的那个不能改
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

    /// p:子窗格上按 = 置顶它所在的会话;跟着上面的会话置顶的子会话上按 = 取消那个会话的置顶(父会话,或者再上面置顶了的那一层)
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
                    tr!("取消置顶「{title}」", "unpinned “{title}”")
                }
            }
            Err(e) => tr!("置顶没存上:{e}", "pin not saved: {e}"),
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild(); // 高亮跟着会话走,一起挪到上面
    }

    /// 进可见列表里最近一个在等你的(红 / 黄 `◐`)。进去看过、之后没新输出的跳过 ——
    /// 看完 prefix+d 回来再按 `i` 就是下一个,挨个过一遍
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
            None => self.flash(t!("没有待处理的", "nothing needs you")),
        }
    }

    /// 跳转模式下的按键,同 agent-deck:小写字母拼提示,对上就进那个会话(分组标题就停在标题上);Esc 退出;
    /// 别的键退出跳转并照常处理(返回 false)
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

    /// 现在这块屏幕用的布局
    fn mode(&self) -> Preview {
        match &self.following {
            Some(f) => f.layout,
            None if narrow(self.screen.width) => self.preview_narrow,
            None => self.preview,
        }
    }

    /// v:在这类屏幕的几种布局里轮换,底栏亮一会儿轮到了哪个
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
            self.flash(t!("屏幕太矮,放不下预览", "the screen is too short for a preview"));
        }
    }

    /// 全屏阅读高亮的会话:先用已有的屏幕内容顶上,历史由 run 循环接着抓
    fn open_reader(&mut self) {
        if self.on_group(t!("看全文", "read in full")) {
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

    /// `x`:全屏看高亮会话目录的改动,从顶上看起;模式沿用上次的
    fn open_diff(&mut self) {
        if self.on_group(t!("看改动", "view the diff")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let (name, w) = (s.name.clone(), Where::of(s));
        self.trace = None;
        let lines = vec![vec![ansi::Seg { text: t!(" 正在读 git…", " reading git…").into(), style: dim(), link: None }]];
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

    /// 看改动时要不要跑 git:刚打开、换了模式,或者到了定时刷新。给 run 循环 (会话名, 目录, 模式)
    pub fn diff_request(&mut self, refreshed: bool) -> Option<(String, Where, Mode)> {
        let r = self.reader.as_mut()?;
        let d = r.diff.as_mut()?;
        (std::mem::take(&mut d.want) || refreshed).then(|| (r.name.clone(), d.w.clone(), d.mode))
    }

    /// 跑完的 diff。已经换了会话 / 模式的不要;和上次一样的不动(免得重折行)
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

    /// `Z`:高亮会话的记录(见 `trace`)
    fn open_trace(&mut self) {
        if self.on_fold_row() {
            return self.flash(t!("先按回车展开", "press Enter to unfold first"));
        }
        let Some(s) = self.current() else { return };
        // 子代理那一行:开它所在会话的记录,读好了直接进到它那一层(Esc 回到起它的那一轮)
        let (name, goto) = match &s.subagent {
            Some(a) => (s.family().to_string(), Some(a.id.clone())),
            None => (s.name.clone(), None),
        };
        self.reader = None;
        self.trace = Some(TraceView { goto, ..TraceView::new(name) });
    }

    /// 要不要去后台读记录:刚打开 / 换了会话。给 run 循环那个会话
    pub fn trace_request(&mut self) -> Option<Session> {
        let v = self.trace.as_mut()?;
        if !std::mem::take(&mut v.want) {
            return None;
        }
        let s = self.all.iter().find(|s| s.name == v.name).cloned();
        if s.is_none() {
            v.loading = false;
            v.err = Some(t!("会话不在了", "the session is gone").into());
        }
        s
    }

    /// 后台读完了。已经换了会话的不要
    pub fn set_trace(&mut self, name: &str, r: Result<crate::trace::Trace, String>) {
        let Some(v) = self.trace.as_mut().filter(|v| v.name == name) else { return };
        v.loading = false;
        match r {
            Ok(t) => v.set(t),
            Err(e) => v.err = Some(e),
        }
    }

    /// 定时刷新:记录变长了读新增的那截(平时就几 KB,几毫秒)
    pub fn trace_tail(&mut self) {
        if let Some(v) = self.trace.as_mut() {
            v.tail();
        }
    }

    /// 记录的会话在不在跑(在跑的一轮、还没回结果的工具算到现在)
    fn trace_live(&self) -> bool {
        self.trace.as_ref().is_some_and(|v| self.all.iter().any(|s| s.name == v.name && matches!(s.state, State::Running | State::Ask)))
    }

    /// e:用编辑器只读地看选中的这一项 —— 读 / 改文件的工具看文件本身,别的写成一份临时的 Markdown
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
            None => self.flash(t!("这一行没有内容可看", "nothing to see on this row")),
            Some(crate::trace::View::File { path, line }) => self.external = Some(External::View { path, line, temp: false }),
            Some(crate::trace::View::Text { text, ext, line }) => match write_view(&crate::trace::where_of(t, i), &text, &ext) {
                Ok(path) => self.external = Some(External::View { path, line, temp: true }),
                Err(e) => self.flash(&tr!("临时文件写不了:{e}", "can't write the temp file: {e}")),
            },
        }
    }

    fn on_trace_key(&mut self, k: KeyEvent) {
        // Ctrl 组合键不当字母用(^Z 不是 z 看输出):^L 原地重启(同列表、计费面板),别的不理
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
                KeyCode::End | KeyCode::Char('G') => d.scroll = usize::MAX, // 画的时候收到最底
                KeyCode::Char(c @ ('n' | 'p')) => {
                    // 同一列里的下一块 / 上一块
                    let b = v.cur();
                    let at = b.at().unwrap_or(0);
                    let to = if c == 'n' { b.items.get(at + 1) } else { at.checked_sub(1).and_then(|a| b.items.get(a)) };
                    match to.map(|x| x.0) {
                        Some(x) => {
                            b.jump(x);
                            v.detail = Some(TraceDetail::new(x));
                        }
                        None => self.flash(if c == 'n' {
                            t!("已经是最后一项", "already the last item")
                        } else {
                            t!("已经是第一项", "already the first item")
                        }),
                    }
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q' | 'Z') => return self.trace = None,
            // 在子代理里:回上一层
            // 从列表里子代理那一行直接进来的:Esc 回列表(← / h 还是往上一层,看是谁起的它)
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
            // 右边:⏎ 整屏看这一步(起了子代理的:进去看它的记录);← / Esc 回对话
            KeyCode::Enter if v.in_steps => {
                if !v.enter_sub(sel) {
                    v.detail = Some(TraceDetail::new(sel));
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc if v.in_steps => v.in_steps = false,
            // 左边:抄过来的那一块 ⏎ / → 展开;一轮 ⏎ / → 看它的过程
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
                    return self.flash(t!("这一轮已经是全文", "this turn is already in full"));
                }
                if !v.open.remove(&sel) {
                    v.open.insert(sel);
                    v.chat.reveal = true;
                }
            }
            // 右边:o 展开 / 收起这一步,O 这一轮的全部
            KeyCode::Char('o') => {
                let fold = v.steps_cache.as_ref().and_then(|c| c.blocks.iter().find(|x| x.node == sel)).is_some_and(|x| x.fold);
                if !fold {
                    return self.flash(t!("这一步没有别的内容", "this step has nothing more"));
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
            KeyCode::Char(c @ ('n' | 'p')) if !v.turn_step(c == 'n') => self.flash(if c == 'n' {
                t!("已经是最后一轮", "already the last turn")
            } else {
                t!("已经是第一轮", "already the first turn")
            }),
            _ => {}
        }
    }

    /// J / K:换到列表里下一个 / 上一个会话接着看记录
    fn trace_step(&mut self, d: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        let mut next = cur + d;
        while next >= 0 && next < self.view.len() as isize && self.is_fold_row(next as usize) {
            next += d;
        }
        if next < 0 || next >= self.view.len() as isize {
            self.flash(if d > 0 {
                t!("已经是最后一个", "already the last one")
            } else {
                t!("已经是第一个", "already the first one")
            });
            return;
        }
        self.select(Some(next as usize));
        self.open_trace();
    }

    /// 预览顶上的改动统计要不要读:高亮的会话换了,或者到了定时刷新。预览关着、在看全屏时不读
    pub fn glance_request(&mut self, refreshed: bool) -> Option<(String, Where)> {
        if self.reader.is_some() || self.trace.is_some() || self.mode() == Preview::Off {
            return None;
        }
        if refreshed {
            self.spot = None;
        }
        // 停在分组上:读组的路径(键见 group_key,run 循环据此连目录里的东西一起读)
        let (key, w) = if self.on_group_row() {
            let sp = self.spot()?;
            (group_key(&sp.group), Where { dir: sp.dir, ..Where::default() })
        } else {
            let s = self.current().filter(|s| s.subagent.is_none())?;
            (s.name.clone(), Where::of(s))
        };
        let changed = self.glance_for.as_deref() != Some(key.as_str());
        let req = (refreshed || changed).then(|| (key.clone(), w));
        if req.is_some() {
            self.glance_for = Some(key);
        }
        req
    }

    pub fn set_glance(&mut self, name: String, g: Glance) {
        self.glances.insert(name, g);
    }

    /// lazygit / 编辑器回来了:改动可能变了(提交了、改了文件),重读;没起来就说一声
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
        // 弹窗里开的编辑器:回到弹窗;它关了再回设置页
        if self.overlays.is_empty() {
            self.back_to_settings();
        }
    }

    /// l:在这个仓库开 lazygit
    fn diff_lazygit(&mut self) {
        let Some(v) = self.reader.as_ref().and_then(|r| r.diff.as_ref()) else { return };
        match () {
            _ if v.loading => self.flash(t!("还在读 git,等一下", "still reading git, wait a moment")),
            _ if v.top.is_empty() => self.flash(t!("不是 git 仓库", "not a git repository")),
            _ => self.external = Some(External::Lazygit(v.top.clone())),
        }
    }

    /// e:用编辑器打开屏幕最顶上那一处(顶上是清单 / 空行就往下找第一个有位置的)
    fn diff_edit(&mut self) {
        let Some(r) = self.reader.as_ref() else { return };
        let Some(v) = r.diff.as_ref() else { return };
        let top = r.top(r.view_h.max(1));
        let line = v.starts.partition_point(|&x| x <= top).saturating_sub(1);
        let Some((file, n)) = v.at.iter().skip(line).find_map(|a| *a) else {
            return self.flash(if v.loading {
                t!("还在读 git,等一下", "still reading git, wait a moment")
            } else {
                t!("没有可以打开的文件", "no file to open")
            });
        };
        match v.paths.get(file).cloned().flatten() {
            None => self.flash(t!("这个文件已经删掉了", "this file has been deleted")),
            Some(p) => {
                let path = std::path::Path::new(&v.top).join(p);
                if path.is_file() {
                    self.external = Some(External::Edit { path: path.to_string_lossy().into_owned(), line: n });
                } else {
                    self.flash(t!("文件不在了", "the file is gone"));
                }
            }
        }
    }

    /// n / p:下一个 / 上一个文件的标题放到最顶上(p 翻过第一个就回到清单)
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
        match scroll {
            Some(s) if s != r.scroll.min(total.saturating_sub(h)) => r.scroll = s,
            _ if v.rows.is_empty() => self.flash(t!("没有改动", "no changes")),
            _ if total <= h => self.flash(t!("都在这一屏了", "it all fits on this screen")),
            _ if d > 0 => self.flash(t!("已经是最后一个文件", "already the last file")),
            _ => self.flash(t!("已经在最上面", "already at the top")),
        }
    }

    /// ?:按键速查(见 `help`),借全屏阅读的翻页,从顶上开始。按屏幕宽度排栏,所以画的时候再排
    /// $:计费面板,默认看近 7 天。数在后台读(第一次要读完全部对话记录),读好之前先显示上次的
    /// ^L:重启 aca(换上新装的程序)。后台还有活在跑就先不重启 —— 重启了它做完的结果就没人收了
    fn ask_relaunch(&mut self) {
        if let Some(b) = self.doing() {
            let m = tr!("还在忙:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if !std::env::current_exe().is_ok_and(|p| p.exists()) {
            return self.flash(t!("找不到 aca 程序,没重启", "can't find the aca program; not restarted"));
        }
        self.relaunch = true;
        self.quit = true;
    }

    /// 重启时记下开到哪,交给新的那个(`--restarted`):计费面板开着就是 bill:时段,不然 list;
    /// 只看着哪些账号的,后面接着 Tab 分开的账号名
    pub fn relaunch_state(&self) -> String {
        let Some(b) = &self.bill else { return "list".into() };
        std::iter::once(format!("bill:{}", b.sel)).chain(self.bill_only.iter().cloned()).collect::<Vec<_>>().join("\t")
    }

    /// 刚被 ^L 重启:说一声这个 aca 是什么时候装的(看得出新版换上了没有),计费面板开着的接着开
    pub fn resume(&mut self, state: &str, installed: Option<String>) {
        let mut parts = state.split('\t');
        if let Some(sel) = parts.next().and_then(|x| x.strip_prefix("bill:")).and_then(|x| x.parse::<usize>().ok()) {
            self.bill_only = parts.map(String::from).collect();
            self.open_bill();
            if let Some(b) = self.bill.as_mut() {
                b.sel = sel.min(crate::usage::RANGES.len() - 1);
            }
        }
        let m = installed.map_or(t!("已重启", "restarted").into(), |t| {
            tr!("已重启 · 这个 aca 是 {t} 装的", "restarted · this aca was installed {t}")
        });
        self.flash(&m);
    }

    fn open_bill(&mut self) {
        self.bill = Some(BillView { sel: 1, ..Default::default() });
        self.bill_want = true;
    }

    /// run 循环问:要不要让后台读一份新的(要的话给出只看哪些账号)。开着面板时一分钟一次,按 r、换了账号立即;同时只读一份
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

    /// 后台读完了;读失败时留着上次的数,底栏说一声
    pub fn set_bill(&mut self, r: Result<crate::usage::Report, String>) {
        self.bill_got = Some(Instant::now());
        self.bill_loading = false;
        match r {
            Ok(r) => self.bill_data = Some(r),
            Err(e) => self.flash(&tr!("计费没读出来:{e}", "couldn't read billing: {e}")),
        }
    }

    /// 挑账号浮窗里的账号(第 1 行起):最近一份数里所有账号,花得多的在前
    fn bill_accounts(&self) -> Vec<crate::usage::Row> {
        self.bill_data.as_ref().map(|r| r.accounts.clone()).unwrap_or_default()
    }

    /// a:打开挑账号的浮窗,高亮停在现在只看的第一个上(全部就停在「全部」)
    fn open_bill_pick(&mut self) {
        let accts = self.bill_accounts();
        if accts.is_empty() {
            return self.flash(t!("还没读到账号", "no accounts read yet"));
        }
        let sel = self.bill_only.first().and_then(|o| accts.iter().position(|a| &a.name == o)).map_or(0, |i| i + 1);
        if let Some(b) = self.bill.as_mut() {
            b.pick = Some(BillPick { sel, ..Default::default() });
        }
    }

    /// 换成只看这些账号(空 = 全部):关浮窗,回到顶上,让后台按它重读
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

    /// 挑账号浮窗里的键:↑↓ 挑,⏎ / 数字 只看它(选了几个就看选上的),空格 勾上 / 去掉,
    /// v 连选(↑↓ 拉,再按 v 或空格定下,Esc 不要这段),0 全部,Esc / a / q 关
    fn on_bill_pick_key(&mut self, k: KeyEvent) {
        let accts = self.bill_accounts();
        let Some(p) = self.bill.as_mut().and_then(|b| b.pick.as_mut()) else { return };
        let n = accts.len() + 1;
        let ranging = p.anchor.is_some();
        match k.code {
            // 连选着:只在账号里上下(不绕回「全部」)
            KeyCode::Esc if ranging => p.anchor = None,
            KeyCode::Up | KeyCode::Char('k') if ranging => p.sel = (p.sel - 1).max(1),
            KeyCode::Down | KeyCode::Char('j') if ranging => p.sel = (p.sel + 1).min(n - 1),
            KeyCode::Char('v' | ' ') if ranging => {
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
            KeyCode::Char('v') if n > 1 => {
                p.sel = p.sel.max(1);
                p.anchor = Some(p.sel);
            }
            KeyCode::Char(' ') => match accts.get(p.sel.wrapping_sub(1)) {
                Some(a) => {
                    match p.marked.iter().position(|m| *m == a.name) {
                        Some(i) => drop(p.marked.remove(i)),
                        None => p.marked.push(a.name.clone()),
                    }
                    p.sel = (p.sel + 1) % n;
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
            KeyCode::Up | KeyCode::Char('k') => b.scroll = b.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => b.scroll = b.scroll.saturating_add(1),
            KeyCode::PageUp | KeyCode::Char('b') => b.scroll = b.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => b.scroll = b.scroll.saturating_add(page),
            KeyCode::Home | KeyCode::Char('g') => b.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => b.scroll = usize::MAX, // 画的时候收到最底
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

    /// 全屏阅读正在看的那一格(run 循环据此抓历史):窗格 id,停掉的会话没有就按会话名
    pub fn reader_target(&self) -> Option<String> {
        let r = self.reader.as_ref().filter(|r| !r.help && r.diff.is_none() && r.plugin.is_none())?;
        let pane = self.all.iter().find(|s| s.name == r.name).map(|s| s.pane.clone()).filter(|p| !p.is_empty());
        Some(pane.unwrap_or_else(|| format!("={}:", r.name)))
    }

    /// 抓回来的历史。往上翻着的时候,底下长出来的新内容不把画面顶走
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
            // 快捷键弹窗:Esc / ? / q 关,方向键、翻页键滚;别的键都是「查它」—— 亮起说明它的行,滚过去
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
            // 插件的输出:能翻、能复制、r 重跑;它不是会话,进不去、换不了别的会话看
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?') => self.reader = None,
                KeyCode::Char('r') => {
                    self.flash(&tr!("正在重跑 {}…", "re-running {}…", req.title()));
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
            // z 看输出、x 看改动:按的是正在看的那个就是返回
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
            KeyCode::Home | KeyCode::Char('g') => r.scroll = usize::MAX, // 画的时候收到最顶
            KeyCode::End | KeyCode::Char('G') => r.scroll = 0,
            KeyCode::Char('J') => self.reader_step(1),
            KeyCode::Char('K') => self.reader_step(-1),
            KeyCode::Char('c') => self.ask_copy(true),
            KeyCode::Char('B') => self.ask_copy(false),
            _ => {}
        }
    }

    /// J / K:换到列表里下一个 / 上一个会话接着看
    fn reader_step(&mut self, d: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        let mut next = cur + d;
        // 折叠行不是会话,跳过
        while next >= 0 && next < self.view.len() as isize && self.is_fold_row(next as usize) {
            next += d;
        }
        if next < 0 || next >= self.view.len() as isize {
            self.flash(if d > 0 {
                t!("已经是最后一个", "already the last one")
            } else {
                t!("已经是第一个", "already the first one")
            });
            return;
        }
        self.select(Some(next as usize));
        if self.reader.as_ref().is_some_and(|r| r.diff.is_some()) {
            self.open_diff();
        } else {
            self.open_reader();
        }
    }

    /// 现在的视图(要存下来的那部分)
    pub fn view_state(&self) -> View {
        View {
            group: self.group.clone(),
            scope: self.scope,
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

    /// 恢复上次的视图。那个分组已经没有会话了就回到全部分组,那个会话不在了就高亮第一个
    pub fn restore(mut self, v: View) -> Self {
        self.group = v.group;
        self.scope = v.scope;
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

    /// 会话的键 → tmux 的会话号(`$3`,进会话、分屏都按它找:tmux 的会话名就是标题,会变)。
    /// 给的已经是会话号(^` 回上一个会话)就原样;刚建 / 刚启动的还没读进列表,现查
    pub fn tmux_of(&self, key: &str) -> String {
        if key.starts_with('$') {
            return key.to_string();
        }
        let listed = self.all.iter().find(|s| s.sub.is_none() && s.name == key).map(|s| s.tmux.clone()).filter(|t| !t.is_empty());
        listed.or_else(|| aca_core::tmux::find(key).map(|p| p.sid)).unwrap_or_default()
    }

    /// 进了 / 刚从这个会话回来:算看过了
    pub fn mark_seen(&mut self, name: &str) {
        let Some(s) = self.all.iter().find(|s| s.sub.is_none() && (s.name == name || s.tmux == name)) else { return };
        let key = notes::key(s);
        let _ = self.reads.mark_read(&key, epoch(), &self.keep_marks());
    }

    /// 已读文件里留哪些:还列着的会话,和删了还能撤回的。一个都没读到(采集失败)时全留
    fn keep_marks(&self) -> impl Fn(&str) -> bool + use<> {
        let mut keys: HashSet<String> = self.all.iter().filter(|s| s.sub.is_none()).map(notes::key).collect();
        let none = keys.is_empty();
        keys.extend(self.trash.ids().map(String::from));
        move |k: &str| none || keys.contains(k)
    }

    /// u:未读的标成已读,闲着的标成未读(进去之前一直 ◐)
    fn toggle_read(&mut self) {
        if self.on_group(t!("标", "mark")) {
            return;
        }
        let Some(s) = self.current() else { return };
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            return self.flash(&m);
        }
        let (key, title, name) = (notes::key(s), s.title.clone(), s.name.clone());
        let registered = s.profile == crate::data::REGISTERED;
        let keep = self.keep_marks();
        let (mark, state) = (self.reads.mark(&key), s.state);
        let r = match s.state {
            State::Unread => self.reads.mark_read(&key, epoch(), &keep).map(|_| (State::Idle, t!("已读", "read"))),
            State::Idle => self.reads.mark_unread(&key, &keep).map(|_| (State::Unread, t!("未读", "unread"))),
            _ => return self.flash(t!("只能标闲着 / 未读的", "only idle / unread ones can be marked")),
        };
        match r {
            Ok((st, what)) => {
                if let Some(s) = self.all.iter_mut().find(|s| s.name == name) {
                    s.state = st;
                }
                // 登记的会话「看过」记到核心那份上(同 `aca seen`):已读文件里的时刻是拿窗口最后有输出的时刻比的,屏幕一重画就又算未读了
                let seen = (st == State::Idle && registered && self.reads.live())
                    .then(|| aca_core::runtime::update(&name, |r| r.seen = epoch()).0.seen);
                self.remember(Undo::Read { key, name, title: title.clone(), what: what.into(), mark, state, seen });
                self.flash(&tr!("{what}「{title}」", "marked {what} “{title}”"));
                self.rebuild(); // 挑着 @ 时标成已读的就不列了
            }
            Err(e) => self.flash(&tr!("没存上:{e}", "not saved: {e}")),
        }
    }

    /// 列得出来的工作区(有会话的,再加上现在这个),h 不算;按键排(数字在字母前)
    fn space_list(&self) -> Vec<char> {
        let mut v: Vec<char> = self.all.iter().map(|s| s.space).chain([self.space]).filter(|&c| c != spaces::HIDDEN).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// 顶栏 / 列表里怎么称呼现在这个工作区:只用着 1、也没改过名时就是「会话」,和以前一样
    fn space_label(&self) -> String {
        if self.space == spaces::HOME && !self.spaces.renamed(spaces::HOME) && self.space_list().len() <= 1 {
            t!("会话", "Sessions").into()
        } else {
            self.spaces.name(self.space)
        }
    }

    /// q+键 / [ ] / ` / S 里选:切到这个工作区,高亮停在上次在那高亮的会话
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

    /// [ ]:前 / 后一个工作区(h 跳过),到头绕回来
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

    /// `:回到刚才待的工作区(h 也算),再按一次回来
    fn back_space(&mut self) {
        match self.last_space.filter(|&c| c != self.space) {
            Some(c) => self.switch_space(c),
            None => self.flash(t!("还没换过工作区", "no workspace switched yet")),
        }
    }

    /// ^`(弹窗里):这台客户端切回上一个会话,落在它离开时的那一格;切走 aca 就退了
    fn back_session(&mut self) {
        match crate::data::last_session() {
            Some(name) => {
                self.focus = None;
                self.attach = Some(name);
            }
            None => self.flash(t!("没有上一个会话", "no previous session")),
        }
    }

    /// Q+键:把高亮的会话送到那个工作区(子行 = 它所在的会话),自己没分过的子会话跟着走(它们的子会话也一样);
    /// 折叠行上 = 这个工作区里的整组,连组一起记,以后这个组里新开的(终端里 `aca new` 开的也算)直接落在那边。人留在这里
    fn send_to_space(&mut self, c: char) {
        if c == self.space {
            return self.flash(t!("已经在这个工作区", "already in this workspace"));
        }
        let fold = self.on_group_row();
        let Some(s) = self.current() else { return };
        let (fam, group) = (s.family().to_string(), s.group.clone());
        let heads = || self.all.iter().filter(|x| x.sub.is_none());
        let go: Vec<&Session> = if fold {
            heads().filter(|x| x.space == self.space && x.group == group).collect()
        } else {
            heads().filter(|x| x.name == fam).collect()
        };
        let Some(first) = go.first() else { return };
        let what =
            if fold { tr!("「{}」整组", "group “{}”", group_label(&group)) } else { tr!("「{}」", "“{}”", first.title) };
        let mut names: HashSet<&str> = go.iter().map(|x| x.name.as_str()).collect();
        // 自己没分过的子会话一层层加进来,加到不再多为止
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
        // 同名的组可能在几个 profile 里,各记一条;送回 1 = 组不记
        let mut groups: Vec<(spaces::Group, Option<char>)> = vec![];
        if fold {
            for x in &go {
                let g = (x.profile.clone(), x.group.clone());
                if !groups.iter().any(|(y, _)| *y == g) {
                    groups.push((g, (c != spaces::HOME).then_some(c)));
                }
            }
        }
        let sessions = self.settle(&self.all, &want, &groups);
        let keep = self.keep_marks();
        // 撤销时放回去的:这些会话、组原来记的
        let before = (
            sessions.iter().map(|(k, _)| (k.clone(), self.spaces.of(k))).collect(),
            groups.iter().map(|(g, _)| (g.clone(), self.spaces.group_of(&g.0, &g.1))).collect(),
        );
        let m = match self.spaces.assign(&sessions, &groups, &keep) {
            Ok(()) => {
                let what = format!("{what} → {}", self.spaces.name(c));
                self.remember(Undo::Space { what: what.clone(), sessions: before.0, groups: before.1 });
                what
            }
            Err(e) => tr!("没存上:{e}", "not saved: {e}"),
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// 让每个会话落在 want 写的工作区(没写的留在现在的地方)要改哪些会话的记录;groups 是同时要改的组的记录。
    /// after = 改完之后的会话(挪分组时分组已经换了)。记录尽量少:和不记也会在的地方一样就不记
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
        let heads: Vec<&Session> = after.iter().filter(|s| s.sub.is_none()).collect();
        let goal = |s: &Session| want.get(&notes::key(s)).copied().unwrap_or(s.space);
        // 不在该在的地方就记上。子会话跟着父会话,父会话的记录变了子会话也变,所以算到不再变为止
        // (几层都是两遍就够:上面的记录一变,下面跟着变了的第二遍各自记上,不再牵动别人)
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

    /// 新建 / 分叉出来的会话落在现在这个工作区(它的组记在别处也一样)
    pub fn adopt(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        let keep = self.keep_marks();
        let sessions = if self.all.iter().any(|s| s.sub.is_none() && notes::key(s) == id) {
            self.settle(&self.all, &HashMap::from([(id.to_string(), self.space)]), &[])
        } else {
            // 列表里还没有它:先记上,多余的下次送别的会话时顺手去掉
            vec![(id.to_string(), Some(self.space))]
        };
        let _ = self.spaces.assign(&sessions, &[], &keep);
    }

    /// q / Q 之后的那个键:工作区的键就切 / 送;q 之后再按 q 退出;别的键(Esc 等)取消
    fn on_pending_key(&mut self, p: Pending, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match (p, k.code) {
            (Pending::Switch, KeyCode::Char('q')) if !ctrl => self.quit = true,
            (Pending::Switch, KeyCode::Char(c)) if !ctrl && spaces::is_key(c) => self.switch_space(c),
            (Pending::Send, KeyCode::Char(c)) if !ctrl && spaces::is_key(c) => self.send_to_space(c),
            _ => {}
        }
    }

    /// S 的工作区列表:↑↓ 挑、⏎ 切过去,直接按键也切(h 这样不列的也行);^R 改名
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

    /// 配色列表(设置页的「主题」):列出主题,高亮停在现在用的那套
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

    /// 配色列表:↑↓ 移到哪套整个界面就换成哪套,⏎ 用它(记进 config.toml),Esc 换回打开前的
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

    /// `^S`:设置页。不常用的都在这,不占单键
    fn open_settings(&mut self) {
        self.settings = Some(self.load_settings(SetPage::Main, 0));
    }

    /// 设置页显示的值,现读
    fn load_settings(&self, page: SetPage, sel: usize) -> Settings {
        let mut bad = (self.config_bad)();
        bad.extend(self.sync_failed.iter().map(|e| {
            tr!("config.toml 写法有问题,账号和工具都当成没写:{e}", "config.toml has a problem; accounts and tools are ignored: {e}")
        }));
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

    /// 换了语言(设置页里按 l、别的 aca 改了 config.toml):按旧语言写好存着的字作废,该重读的马上重读。
    /// 列表、弹窗的字每帧现拼,不用管;这里管的是后台读回来、打开时算好的
    pub fn lang_changed(&mut self) {
        // 预览顶上的改动统计(分支「没在分支上」、目录读不了的原因)
        self.glances.clear();
        self.glance_for = None;
        // 计费:额度的说明是子进程按当时的语言写的;开着就马上重读,关着的下次打开再读
        self.bill_data = None;
        self.bill_got = None;
        self.bill_want = self.bill.is_some();
        // 记录:节点的标签是读的时候定的,整份重读
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

    /// config.toml 被改了(编辑器里、另一个 aca):开着的设置页跟着重读
    pub fn reload_settings(&mut self) {
        if let Some(s) = self.settings.take() {
            self.settings = Some(self.load_settings(s.page, s.sel));
        }
    }

    /// 从设置页进去做的事做完了(或取消了):回到设置页原来那里,值重读
    fn back_to_settings(&mut self) {
        if let Some(s) = self.settings_back.take() {
            self.settings = Some(self.load_settings(s.page, s.sel));
        }
    }

    /// 离开设置页去做一件事,做完回来
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

    /// 设置页的一行:切换的当场切,别的进去
    fn setting(&mut self, r: SetRow) {
        let Some(s) = self.settings.as_mut() else { return };
        match r {
            SetRow::Theme => {
                self.leave_settings();
                self.open_themes();
            }
            // 当场换(整个界面下一帧就是那种话),也记进 config.toml 下次照用
            SetRow::Lang => {
                let to = if s.lang == Lang::Zh { Lang::En } else { Lang::Zh };
                match (self.config_set)("ui", "lang", toml::Value::String(to.code().into())) {
                    Ok(()) => {
                        i18n::set(to);
                        s.lang = to;
                        if i18n::forced() {
                            self.flash(t!(
                                "换了;这个终端设了 ACA_LANG,下次打开还是听它的",
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
                            self.flash(t!("这个终端设了 ACA_IME,听它的", "this terminal sets ACA_IME, which wins"));
                        }
                    }
                    Err(e) => self.flash(&e),
                }
            }
            // 账号页是它自己的弹窗(acctpage.rs):关了回设置页这一行,几个账号重数
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
                // 没进到哪去(当场切的):回设置页,值重读
                if self.overlays.is_empty() && self.external.is_none() {
                    self.back_to_settings();
                }
            }
            SetRow::Problems => s.page = SetPage::Problems,
        }
    }

    /// 路径默认填选中会话的目录 —— 列表本身就是目录选择器,多数时候直接回车,手机上不用打路径
    /// fzf 候选的开头:选中会话的目录,再是列表里其他会话的
    pub fn dir_hints(&self) -> Vec<String> {
        let first = self.current().map(|s| s.dir.clone());
        first.clone().into_iter().chain(self.view.iter().map(|&i| self.all[i].dir.clone()).filter(|d| Some(d) != first.as_ref())).collect()
    }

    /// ↓ / ^F 要打开的 fzf:候选目录,每个标上图标、会进哪个组、里面已有的会话。
    /// 要扫目录、问 zoxide(~0.1s),在让出终端之前备好,免得露出底下的 shell 画面
    pub fn dir_pick(&self, width: u16, height: u16) -> Option<DirPick> {
        let base = self.new_draft.as_ref()?.base.clone();
        self.dir_pick_from(create::dir_candidates(base.as_deref(), &self.dir_hints()), width, height)
    }

    fn dir_pick_from(&self, cands: Vec<(String, Src)>, width: u16, height: u16) -> Option<DirPick> {
        let d = self.new_draft.as_ref()?;
        let rows = cands
            .into_iter()
            .map(|(dir, src)| {
                // 只数这个工作区的:别的工作区(尤其 h)的标题不在这露出来
                let here: Vec<&Session> =
                    self.all.iter().filter(|s| s.space == self.space && !s.dir.is_empty() && create::tilde(&s.dir) == dir).collect();
                let mark = match src {
                    _ if !here.is_empty() => '●',
                    Src::Known => '◆',
                    Src::Zoxide => '↗',
                    Src::Near => ' ',
                };
                DirRow {
                    group: d.group_at(&dir),
                    sessions: here.iter().map(|s| (icon(s.state).0.to_string(), s.title.clone())).collect(),
                    dir,
                    mark,
                }
            })
            .collect();
        Some(DirPick { rows, what: d.label.clone(), query: d.pick_query(), width, height })
    }

    /// fzf 回来了:回车 / 双击就开在那;→ 填进路径框接着打;Esc 什么都不变;出错写在输入框下面
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

    /// `n`:先选工具(账号和 config.toml 里自己起名的工具),再和 N 一样填路径
    fn open_tools(&mut self) {
        match (self.tools_loader)() {
            Ok(v) if v.is_empty() => self.flash(t!("工具菜单是空的", "the tool menu is empty")),
            Ok(v) => self.tool_menu = Some((v, 0)),
            Err(e) => self.flash(&e),
        }
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
            // 加账号在账号页里(先挑是 Claude Code 还是 Codex);加完留在那一页,看得到它登录着没有
            KeyCode::Char('+') if self.handoff.is_none() => {
                if let Some(page) = self.accounts_page {
                    self.tool_menu = None;
                    self.open(page(true));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.tool_menu = None;
                let m = if self.handoff.take().is_some() {
                    t!("已取消交接", "handoff canceled")
                } else {
                    t!("已取消新建", "creation canceled")
                };
                self.flash(m);
            }
            _ => {}
        }
    }

    fn pick_tool(&mut self, i: usize) {
        // 起不了的那行(账号没登记、工具写坏了)选不了:菜单留着,底栏说为什么
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
            }
            Err(e) => self.flash(&e),
        }
    }

    fn ask_remove(&mut self) {
        // 别的删除还在删不碍事,接着删;别的活(重启、改名…)做完才行
        if let Some(b) = self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)) {
            let m = tr!("还在忙:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        let Some(s) = self.current() else { return };
        // 普通 tmux 会话那行、里面还有别的窗口 / 窗格:只关它这一格,编号最小的接着当头(tmux 的 renumber-windows 把 #2 挪成 #1)。
        // 登记的会话删的是整个会话:里面另开的窗口一起关(确认框里写着几个)
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

    /// R / r / m 要的:高亮的登记过的会话。后台还在忙、停在折叠行上、是普通 tmux 会话,说一声返回 None
    fn deck_target(&mut self, what: &str) -> Option<Target> {
        if let Some(b) = self.doing() {
            let m = tr!("还在忙:{b}", "still busy: {b}");
            self.flash(&m);
            return None;
        }
        if self.on_group(what) {
            return None;
        }
        let s = self.current()?;
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            self.flash(&m);
            return None;
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        if t.id.is_empty() {
            let m = tr!(
                "普通 tmux 会话不能{what}(没在 aca 里登记) · A 接管",
                "can't {what} an ordinary tmux session (not registered in aca) · A adopts it"
            );
            self.flash(&m);
            return None;
        }
        Some(t)
    }

    /// s 那一格,交给 park.rs
    fn pane_of(&self, s: &Session) -> crate::park::Pane {
        let title = clip(&s.title, 16).trim_end().to_string();
        crate::park::Pane {
            id: s.pane.clone(),
            label: s.sub.as_ref().map_or_else(|| title.clone(), |x| tr!("{}「{title}」", "{} “{title}”", x.loc())),
        }
    }

    /// 不是登记的 agent 那一格(会话里另开的窗口 / 窗格、普通 tmux 会话)上的 D:只停这一格,窗格连最后的画面留着(见 park.rs)。
    /// 先问一句。返回 false = 不归这里管(登记的会话那行:停的是 agent,走 `session::stop`)
    fn ask_pane(&mut self) -> bool {
        if self.on_group_row() {
            return false;
        }
        let Some(s) = self.current() else { return false };
        if s.pane.is_empty() || (s.sub.is_none() && !s.deck_id.is_empty()) {
            return false;
        }
        if let Some(b) = self.doing() {
            self.flash(&tr!("还在忙:{b}", "still busy: {b}"));
            return true;
        }
        if s.state == State::Dead {
            self.flash(t!("已经停了 · ⏎ 再启动", "already stopped · ⏎ starts it again"));
            return true;
        }
        let p = self.pane_of(s);
        let why = match s.state {
            State::Running => t!("正在跑,会打断它", "it's running; this interrupts it"),
            State::Ask => t!("在等你选,会打断它", "it's waiting for your choice; this interrupts it"),
            _ => "",
        };
        let t = Target { name: s.family().to_string(), title: p.label.clone(), profile: s.profile.clone(), id: String::new() };
        let row = s.name.clone();
        self.confirm = Some(Confirm::Pane { t, p, row, restart: false, why: why.into(), next: None });
        true
    }

    /// R:在跑 / 在等你选的先问一句(重启会打断它),别的直接重启
    fn ask_restart(&mut self) {
        let why = match self.current().map(|s| s.state) {
            Some(State::Running) => t!("正在跑,会打断它", "it's running; this interrupts it"),
            Some(State::Ask) => t!("在等你选,会打断它", "it's waiting for your choice; this interrupts it"),
            _ => "",
        };
        let Some(t) = self.deck_target(t!("重启", "restart")) else { return };
        if why.is_empty() {
            self.job = Some(Job::Restart { t, force: false });
        } else {
            self.confirm = Some(Confirm::Restart { t, why: why.into(), force: false });
        }
    }

    /// D:停掉 —— agent 退出,窗格连最后的画面留着,登记也留着,之后 ⏎ 在原地再启动接着聊。先问一句
    fn ask_stop(&mut self) {
        if self.ask_pane() {
            return;
        }
        let Some(t) = self.deck_target(t!("停止", "stop")) else { return };
        let (state, attached) = self.current().map_or((State::Idle, false), |s| (s.state, s.attached));
        if state.gone() || state == State::Dead {
            self.flash(t!("已经停了 · ⏎ 重新启动", "already stopped · ⏎ starts it again"));
        } else {
            self.confirm = Some(Confirm::Stop { t, attached });
        }
    }

    /// A:接管高亮的普通 tmux 会话(不是 aca 起的)—— 登记成 aca 的,进程不动。先认一遍,把认出来的摆在确认框里。
    /// 停在普通会话里另开的窗口上:接管它所在的会话,agent 算这一格的
    fn ask_adopt(&mut self) {
        if let Some(b) = self.doing() {
            let m = tr!("还在忙:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("接管", "adopt")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let registered = |name: &str| self.all.iter().any(|x| x.sub.is_none() && x.name == name && !x.deck_id.is_empty());
        if !s.deck_id.is_empty() || registered(s.family()) {
            return self.flash(t!("它已经是 aca 的会话了", "it's already an aca session"));
        }
        if s.state == State::Dead {
            return self.flash(t!("那一格的程序已经退了,没有可以接管的", "the program in that pane has exited; nothing to adopt"));
        }
        let target = if s.sub.is_some() { s.pane.clone() } else { s.tmux.clone() };
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: String::new() };
        match (self.adopt_planner)(&target) {
            Ok(plan) => self.confirm = Some(Confirm::Adopt { t, plan: Box::new(plan) }),
            Err(e) => self.flash(&tr!("接管不了:{e}", "can't adopt: {e}")),
        }
    }

    /// r:弹窗里改标题,原标题先填好。停在分组上:改分组(名字、默认目录)
    fn ask_rename(&mut self) {
        if let Some(g) = self.group_row().map(String::from) {
            return self.ask_group_edit(g);
        }
        let Some(t) = self.deck_target(t!("改名", "rename")) else { return };
        self.prompt = Some(Prompt { text: t.title.clone(), ask: Ask::Rename(t), hint: String::new() });
    }

    /// 分组上 r:先改名(原名填好,可以带上级),回车后改默认目录。只改登记表里有的组(「未分组」、普通 tmux 会话那一区不是)
    fn ask_group_edit(&mut self, g: String) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("还在忙:{b}", "still busy: {b}"));
        }
        if g == "-" {
            return self.flash(t!("这些是普通 tmux 会话,没有分组可改", "these are ordinary tmux sessions; no group to edit"));
        }
        if !(self.group_hints)().groups.iter().any(|(x, _)| *x == g) {
            return self.flash(&tr!("「{g}」不是登记的分组:要放进分组用 m", "“{g}” isn't a registered group: use m to put it in a group"));
        }
        self.prompt = Some(Prompt { text: g.clone(), ask: Ask::GroupName(g), hint: String::new() });
    }

    /// 分组的新名字填好了:没撞上别的组,就接着填默认目录(原来的先填好)
    fn group_name_given(&mut self, group: String, text: String) {
        let hints = (self.group_hints)().groups;
        let to = match ops::clean_group(&text) {
            None => Err(t!("分组名是空的", "the group name is empty").to_string()),
            Some(to) if to.strip_prefix(&group).is_some_and(|r| r.starts_with('/')) => {
                Err(tr!("{group} 不能挪到它自己下面", "{group} can't move under itself"))
            }
            Some(to) => match hints.iter().find(|(x, _)| x.eq_ignore_ascii_case(&to) && *x != group) {
                Some((x, _)) => Err(tr!("已经有 {x} 这个分组了", "group {x} already exists")),
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

    /// 默认目录也填好了:空 = 不记;不空要是已有的目录、不是别的组的默认目录。名字、目录都没变就不动
    fn group_home_given(&mut self, group: String, to: String, was: String, text: String) {
        let real = aca_core::paths::real;
        let dir = match text.trim() {
            "" => Ok(String::new()),
            // 没改:不再查(原来那个目录没了也让人只改名)
            t if !was.is_empty() && real(&create::expand(t)) == real(&was) => Ok(was.clone()),
            t => {
                let abs = create::expand(t);
                let taken = (self.group_hints)().groups.into_iter().find(|(x, d)| *x != group && !d.is_empty() && real(d) == real(&abs));
                match taken {
                    _ if !std::path::Path::new(&abs).is_dir() => Err(tr!("{} 不是目录", "{} is not a directory", create::tilde(&abs))),
                    Some((x, _)) => Err(tr!("这个目录已经是分组 {x} 的默认目录了", "this directory is already the default of group {x}")),
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
            return self.flash(t!("分组没改", "group unchanged"));
        }
        let dir = changed.then_some((was, dir));
        self.job = Some(Job::Group(groups::Act::Edit { profile: crate::data::REGISTERED.into(), group, to, dir }));
    }

    /// 分组改了名(撤销改回来的也算):界面里记着它的 —— 在看的组、折起来的、高亮停着的组标题、工作区里记的 —— 跟过去
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
        // 工作区文件里的已经在后台改好了(`groups::rename_all`)
        self.spaces.reload();
    }

    /// m:用 fzf 挑一个分组挪进去
    fn ask_move(&mut self) {
        let Some(t) = self.deck_target(t!("挪分组", "move")) else { return };
        let from = self.current().map(|s| s.group.clone()).unwrap_or_default();
        let mut groups: Vec<String> = (self.group_hints)().groups.into_iter().map(|(g, _)| g).collect();
        groups.sort();
        groups.dedup();
        // 先按列表里的分区顺序,再是列表里没有的(空的)
        let mut cands: Vec<String> = self.groups().into_iter().filter(|g| groups.contains(g)).collect();
        let rest: Vec<String> = groups.iter().filter(|g| !cands.contains(g)).cloned().collect();
        cands.extend(rest);
        cands.retain(|g| *g != from);
        self.pick_group = Some(GroupPick { t, from, cands, groups });
    }

    /// f:带着对话分叉出一个新会话,先起标题(分出来的和原来的一模一样,不起名分不清):
    /// 先填好默认的「标题 (分叉)」,直接 ⏎ 就用它
    fn ask_fork(&mut self) {
        let Some(t) = self.deck_target(t!("分叉", "fork")) else { return };
        if self.current().is_none_or(|s| !matches!(s.agent, Agent::Claude | Agent::Codex)) {
            return self.flash(t!("只能分叉 Claude / Codex 会话", "only Claude / Codex sessions can be forked"));
        }
        self.prompt = Some(Prompt { text: fork_title(&t.title), ask: Ask::Fork(t), hint: String::new() });
    }

    /// X:换个账号接着跑 —— 框里是它现在的账号和额度、能换过去的账号(登录着没有、额度),⏎ 直接换(见 `rehome`)
    fn ask_rehome(&mut self) {
        self.digits.clear();
        let Some(t) = self.deck_target(t!("换账号", "switch accounts of")) else { return };
        let Some(s) = self.current().cloned() else { return };
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只能给 Claude / Codex 会话换账号", "only Claude / Codex sessions can switch accounts"));
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

    /// H:交接给新会话 —— 先挑新会话的工具 / 账号(`n` 的菜单,没有 shell),在它的目录、分组里建好,
    /// 把交接说明发过去,让新会话读它的对话接着干(见 `handoff`)。它的便签一起带过去
    fn ask_handoff(&mut self) {
        self.digits.clear();
        if let Some(b) = self.doing() {
            let m = tr!("还在忙:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("交接", "hand off")) {
            return;
        }
        let Some(s) = self.current().cloned() else { return };
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
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
            return self.flash(t!("工具菜单里没有能接手的(只有 shell)", "nothing in the tool menu can take over (only shell)"));
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        let note = from.note.is_some().then_some(key);
        let busy = matches!(s.state, State::Running | State::Ask);
        self.handoff = Some(Pass { t, from: Box::new(from), draft, note, busy });
        self.tool_menu = Some((menu, 0));
    }

    /// M:这个会话的 MCP
    fn open_mcp(&mut self) {
        let Some(t) = self.deck_target(t!("看 MCP", "view the MCPs of")) else { return };
        let Some(s) = self.current() else { return };
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只有 Claude / Codex 会话有 MCP", "only Claude / Codex sessions have MCPs"));
        }
        let info = (self.mcp_loader)(s);
        let v = mcp::View::new(t, s.dir.clone(), info.rows);
        self.open(Box::new(v));
    }

    /// P:挂载 / 远端任务的菜单(不对着哪个会话,停在分组上也能开)
    fn open_plugins(&mut self) {
        if let Some(page) = self.plugins_page {
            self.open(page());
        }
    }

    /// 分组上 d:登记表里的分组那一页(没会话的也列),高亮停在这个组上,再按一次 d 删(见 groups.rs)
    fn open_groups(&mut self) {
        let Some(g) = self.group_row().map(String::from) else { return };
        if g == "-" {
            return self.flash(t!("这些是普通 tmux 会话,没有分组可删", "these are ordinary tmux sessions; no group to delete"));
        }
        let profile = self.all.iter().find(|s| s.group == g && s.space == self.space).map(|s| s.profile.clone()).unwrap_or_default();
        // 藏在隐藏工作区里的组,人不在那边时不列
        let hide: Vec<spaces::Group> = if self.space == spaces::HIDDEN {
            vec![]
        } else {
            let there = self.all.iter().filter(|s| s.space == spaces::HIDDEN).map(|s| (s.profile.clone(), s.group.clone()));
            there.chain(self.spaces.groups_in(spaces::HIDDEN)).collect()
        };
        self.open(Box::new(groups::Page::new(self.groups_loader, hide, (&profile, &g))));
    }

    /// 删分组之前:里面的会话要挪到上级分组去,跟着组的会跟到上级记的工作区;先记住它们现在在哪,删完留在原处
    fn stay_on_group(&mut self, a: &groups::Act) {
        if !matches!(a, groups::Act::Delete { .. }) {
            return;
        }
        let to = a.dest();
        let mut after = self.all.clone();
        for s in after.iter_mut().filter(|s| a.covers(&s.profile, &s.group)) {
            s.group = to.clone();
        }
        let sessions = self.settle(&after, &HashMap::new(), &[]);
        if !sessions.is_empty() {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&sessions, &[], &keep);
        }
    }

    /// 开一个功能自己的弹窗(见 overlay.rs)
    fn open(&mut self, o: Box<dyn Overlay>) {
        self.overlays.push(o);
    }

    /// `:`:插件菜单,冲着高亮的那行(会话 / 分组;普通 tmux 会话、另开的窗口当成别处)
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

    /// 插件跑完了:视图、输出长的放进全屏阅读页;短的写在底栏
    pub fn plugin_done(&mut self, req: crate::caps::Req, ok: bool, out: String) {
        use aca_core::plugin::ItemKind;
        // 列表视图:开着的那一页换上新的;没开着的开一页(重读的、它已经关了的不再开)
        if req.item.kind == ItemKind::View && req.item.rows {
            // 「正在跑 / 正在重读…」那句到这就过时了:列表自己摆在那
            self.msg = None;
            if let Some(o) = self.overlays.iter_mut().find(|o| o.takes(&req)) {
                o.feed(ok, &out);
            } else if !req.refresh {
                self.open(Box::new(crate::caps::List::new(req, ok, &out)));
            }
            return;
        }
        // 列表里的键跑完了:结果写在那一页下面,列表重读;那一页已经关了就照平常的来
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
                tr!("✓ {title} 跑完了", "✓ {title} done")
            } else {
                tr!("✕ {title} 出错了", "✕ {title} failed")
            }),
            ItemKind::Command | ItemKind::Pane if one_line => self.flash(&format!("{mark} {title}:{text}")),
            _ => {
                let lines = ansi::parse_lines(&text.lines().map(str::to_string).collect::<Vec<_>>());
                // 重跑的那个开着:换上新的,翻到的位置留着
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

    /// 键给最上面那层弹窗;它要做的事照办,它关了就露出下面那层(没有了、又是从设置页进来的,回设置页)
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

    /// 弹窗 / 设置行要界面做的事
    fn apply(&mut self, fx: Fx) {
        if let Some(m) = fx.flash {
            self.flash(&m);
        }
        if let Some(j) = fx.job {
            match self.doing() {
                Some(b) => self.flash(&tr!("还在忙:{b}", "still busy: {b}")),
                None => {
                    if let Job::Group(a) = &j {
                        self.stay_on_group(a);
                    }
                    self.job = Some(j);
                }
            }
        }
        if let Some(x) = fx.external {
            self.external = Some(x);
        }
        if let Some(t) = fx.term {
            // 终端占着下半屏,弹窗都收起来,关了终端也不回设置页
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
    }

    /// 后台的活做完了、从编辑器回来了:开着的弹窗重读
    fn reload_overlays(&mut self) {
        for o in &mut self.overlays {
            o.reload();
        }
    }

    /// m 挪分组之前:跟着组的会话换了组会跟到新组记的工作区,先记住它现在在哪,挪完留在原处
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

    /// fzf 挑完分组回来:选中的或者新打的名字
    pub fn group_picked(&mut self, p: GroupPick, got: Result<Option<String>, String>) {
        match got {
            Ok(Some(g)) => match ops::clean_group(&g) {
                Some(to) if to.eq_ignore_ascii_case(&p.from) => self.flash(&tr!("已经在 {to} 里了", "already in {to}")),
                Some(to) => {
                    self.stay_on_move(&p.t.name, &to);
                    self.job = Some(Job::Move { t: p.t, from: p.from, to, groups: p.groups });
                }
                None => {}
            },
            Ok(None) => {}
            Err(e) => self.flash(&e),
        }
    }

    /// e:底栏里写便签,已有的先填好
    fn ask_note(&mut self) {
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        self.prompt = Some(Prompt { ask: Ask::Note { key, title: s.title.clone(), orig: text.clone() }, text, hint: String::new() });
    }

    /// E / Shift+Enter:带着便签进去 —— 进去前粘进它的输入框(不发送),填完删掉。没便签就和回车一样。
    /// 只填 Claude / Codex(别的程序比如 vim,粘进去的字会被当成命令);在等你选(!)时不填 —— 字会被当成选项。
    /// 填不了就说为什么、不进去(免得以为填了)
    fn enter_with_note(&mut self) {
        if self.on_group_row() {
            return self.toggle_fold();
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.enter() };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("没带便签进去:{w}", "didn't go in with the note: {w}"));
        }
        self.fill = Some((key, text));
        self.enter();
    }

    /// s:把便签直接发给它(粘进输入框再回车),人留在列表里。没便签就什么都不发;有也先问一句,免得误触
    fn ask_send(&mut self) {
        self.digits.clear();
        if let Some(b) = self.doing() {
            let m = tr!("还在忙:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("发便签", "send the note")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.flash(t!("没有便签", "no note")) };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("没发:{w}", "not sent: {w}"));
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        let pane = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        self.confirm = Some(Confirm::Send { t, pane, key, text });
    }

    /// ^G(列表里):用编辑器写高亮会话的便签
    fn note_in_editor(&mut self) {
        if self.on_group(t!("写便签", "write a note")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        self.edit_note = Some(NoteEdit { key, title: s.title.clone(), orig: text.clone(), text, from_prompt: false });
    }

    /// 编辑器写完回来:存上;从底栏编辑里去的,回到那里接着写
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

    /// 便签改了就存(每个字都存:手机断线时 aca 是被直接杀掉的,等不到退出)
    fn save_note(&mut self, key: &str, text: &str) {
        if let Err(e) = self.notes.set(key, text) {
            self.flash(&tr!("便签没存上:{e}", "note not saved: {e}"));
        }
        self.apply_pins();
    }

    /// 便签写完了:和开始写之前不一样就记一步撤销
    fn note_done(&mut self, key: String, title: String, orig: String) {
        if self.notes.get(&key).unwrap_or_default() != orig {
            self.remember(Undo::Note { key, title, text: orig });
        }
    }

    fn note_saved_flash(&mut self, text: &str) {
        self.flash(if text.trim().is_empty() { t!("便签已清空", "note cleared") } else { t!("便签已存", "note saved") });
    }

    /// 便签填进去了:删掉
    pub fn note_filled(&mut self, key: &str) {
        let _ = self.notes.take(key);
        self.apply_pins();
    }

    /// ^N:先起新分组的名字,回车后直接进 fzf 挑目录(第一行是默认目录),新会话开在这个组里
    fn ask_new_group(&mut self) {
        self.prompt = Some(Prompt { ask: Ask::NewGroup, text: String::new(), hint: String::new() });
    }

    fn on_prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = self.prompt.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Ask::Note { key, title, orig } = &p.ask {
            // 便签:回车换行,Esc 退出(早就存好了),^G 交给编辑器
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
                KeyCode::Char('j') if ctrl => p.text.push('\n'), // 粘贴进来的换行
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
                let m = match p.ask {
                    Ask::Rename(_) => t!("没改名", "not renamed"),
                    Ask::GroupName(_) | Ask::GroupHome { .. } => t!("分组没改", "group unchanged"),
                    Ask::Fork(..) => t!("没分叉", "not forked"),
                    Ask::Space(_) => t!("没改名", "not renamed"),
                    Ask::GroupDir => "",
                    _ => t!("已取消新建", "creation canceled"),
                };
                self.prompt = None;
                if self.settings_back.is_some() {
                    return self.back_to_settings();
                }
                return self.flash(m);
            }
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Char('u') if ctrl => p.text.clear(),
            // 删掉最后一个词(连同它前面的空格 / 分组的 /)
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
                    self.prompt = Some(Prompt { ask: Ask::Rename(t), text, hint: t!("标题是空的", "the title is empty").into() });
                } else if to == t.title {
                    self.flash(t!("没改名(和原来一样)", "not renamed (same as before)"));
                } else {
                    self.job = Some(Job::Rename { t, to, unlock: false });
                }
            }
            Ask::Fork(t) => {
                let to = text.trim().to_string();
                if to.is_empty() {
                    self.prompt = Some(Prompt { ask: Ask::Fork(t), text, hint: t!("标题是空的", "the title is empty").into() });
                } else {
                    // 没改就用默认的(不锁,Claude 之后还能改);改了的锁住
                    let title = (to != fork_title(&t.title)).then_some(to);
                    self.job = Some(Job::Fork { t, title });
                }
            }
            // 便签的回车是换行,不会走到这
            Ask::Note { .. } => {}
            Ask::GroupDir => {
                let dir = text.trim().to_string();
                let r = if dir.is_empty() {
                    Err(t!("目录是空的", "the directory is empty").to_string())
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
                    Err(e) => tr!("没存上:{e}", "not saved: {e}"),
                };
                self.flash(&m);
            }
            Ask::NewGroup => {
                let Some(mut name) = ops::clean_group(&text) else {
                    self.prompt =
                        Some(Prompt { ask: Ask::NewGroup, text, hint: t!("分组名是空的", "the group name is empty").into() });
                    return;
                };
                match self.draft() {
                    Ok(mut d) => {
                        // 其实已经有了(大小写不同):用已有的
                        if let Some((g, _)) = d.hints.groups.iter().find(|(g, _)| g.eq_ignore_ascii_case(&name)) {
                            name = g.clone();
                        }
                        d.start_group(name, &(self.new_group_dir)());
                        self.new_draft = Some(d);
                        self.pick = true; // Esc 出来就是填着默认目录的输入框
                    }
                    Err(e) => self.flash(&e),
                }
            }
        }
    }

    /// run 循环要放进后台的活;同时记下「忙」和要不要让高亮跟着重启走。删除不记「忙」(`vanish` 里记着)
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
        // 之前的提示让位给「正在…」;⇧↑ ⇧↓ 的写入不算(连按时「已在组内最上」会被第一下的写入冲掉)
        if !matches!(job, Job::Order { .. } | Job::GroupOrder { .. }) {
            self.msg = None;
        }
        if !matches!(job, Job::Remove { .. }) {
            self.busy = Some(job.doing());
            self.follow = job.target().is_some_and(|t| self.selected.as_deref() == Some(t.name.as_str()));
        }
        Some(job)
    }

    /// 底栏上的「正在…」,也是别的操作要等的:别的活,或者还在删的(可以好几个)
    fn doing(&self) -> Option<String> {
        if let Some(b) = &self.busy {
            return Some(b.clone());
        }
        let live: Vec<&Job> = self.vanish.iter().filter(|v| v.until.is_none()).map(|v| &v.job).collect();
        match live[..] {
            [] => None,
            [j] => Some(j.doing()),
            _ if live.iter().all(|j| matches!(j, Job::Remove { pane: Some(_), .. })) => {
                Some(tr!("正在关闭 {} 个…", "closing {}…", live.len()))
            }
            _ => Some(tr!("正在删除 {} 个…", "deleting {}…", live.len())),
        }
    }

    /// 从 all 里拿掉 d 删掉的(还在删的、刚删完的);挂在它下面的子会话照删完的样子算(删父会话不连带子会话)
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
            .filter_map(|v| match &v.job {
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

    /// 后台做完了。fresh 是做完后重读的会话:重启换了 tmux 会话名,置顶、高亮、看过的记录跟过去
    pub fn job_done(&mut self, job: &Job, out: Outcome, fresh: Vec<Session>) {
        let mut was_running = None;
        if matches!(job, Job::Remove { .. }) {
            // 删掉了(归档了):再藏一会儿;没删掉:放回来
            let ok = matches!(out, Outcome::Done(_) | Outcome::Trashed { .. });
            let at = self.vanish.iter().position(|v| v.until.is_none() && v.job == *job);
            was_running = at.map(|i| self.vanish[i].running);
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
            // 写失败了,后面排着的也不作数,列表回到登记表里的顺序
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
            return self.reverted(u, msg, out, fresh);
        }
        // 做成了、撤得回的记一步
        // 做之前它什么样(在不在跑、标题锁没锁)
        let (running, locked) = self
            .all
            .iter()
            .find(|s| s.sub.is_none() && job.target().is_some_and(|t| s.deck_id == t.id && s.profile == t.profile))
            .map_or((false, true), |s| (!s.state.gone(), s.locked));
        let running = was_running.unwrap_or(running);
        let mut out = out;
        // d 删的登记的会话归档了:记下等真删(列表里就不列了),^Z 能撤回
        if let (Job::Remove { t, .. }, Outcome::Trashed { was, .. }) = (job, &out) {
            let u = Undo::Remove { t: t.clone(), unarchive: !was, start: running };
            match self.trash.add(&t.profile, &t.id) {
                Ok(()) => self.remember(u),
                Err(e) => {
                    out = Outcome::Failed(tr!(
                        "已归档「{}」,没记上:{e}",
                        "archived “{}”, but not recorded: {e}",
                        clip(&t.title, 16).trim_end()
                    ))
                }
            }
        }
        if let Outcome::Done(_) = &out {
            let u = match job {
                // 关掉的窗格 / 普通 tmux 会话:撤不回,也记一步(见 undo.rs)
                Job::Remove { t, .. } => Some(Undo::Gone { title: t.title.clone() }),
                Job::Stop { t } => Some(Undo::Stop { t: t.clone() }),
                Job::Park { t, p } => Some(Undo::Park { t: t.clone(), p: p.clone() }),
                Job::Move { t, from, groups, .. } => Some(Undo::Move { t: t.clone(), from: from.clone(), groups: groups.clone() }),
                Job::Rename { t, .. } => Some(Undo::Rename { t: t.clone(), from: t.title.clone(), unlock: !locked }),
                Job::Mcp { t, dir, on, off } => Some(Undo::Mcp { t: t.clone(), dir: dir.clone(), on: off.clone(), off: on.clone() }),
                Job::Group(a) => a.undo().map(Undo::Group),
                _ => None,
            };
            if let Some(u) = u {
                self.remember(u);
            }
        }
        if let (Job::Restart { t, .. } | Job::Rehome { t, .. }, Outcome::Done(_)) = (job, &out) {
            let new = fresh.iter().find(|s| s.deck_id == t.id && s.profile == t.profile).map(|s| s.name.clone());
            if let Some(new) = new.filter(|n| *n != t.name) {
                let _ = self.pins.rename(&t.name, &new);
                if follow {
                    self.selected = Some(new);
                }
            }
        }
        // 接管的:那一行的键从 tmux 会话名换成了登记的 id,置顶跟过去
        if let (Job::Adopt { t, .. }, Outcome::Made { id, .. }) = (job, &out) {
            let _ = self.pins.rename(&t.name, id);
        }
        if let (Job::Send { key, .. }, Outcome::Done(_)) = (job, &out) {
            self.note_filled(key);
        }
        // 分组删掉了:它在工作区里的记录不留
        if let (Job::Group(a), Outcome::Done(_)) = (job, &out) {
            let _ = self.spaces.forget_groups(&|p, g| a.covers(p, g));
        }
        // 交接:便签跟着说明带到新会话去了
        if let (Job::Handoff { note: Some(key), .. }, Outcome::Made { .. } | Outcome::Handed { .. }) = (job, &out) {
            self.note_filled(key);
        }
        self.set_sessions(fresh);
        self.reload_overlays();
        match out {
            Outcome::Done(m) | Outcome::Failed(m) | Outcome::Trashed { msg: m, .. } if !m.is_empty() => self.flash(&m),
            Outcome::Done(_) | Outcome::Failed(_) | Outcome::Trashed { .. } => {}
            Outcome::Made { msg, id } => self.made(&id, &msg),
            Outcome::Handed { msg, id, note } => {
                self.save_note(&id, &note);
                self.made(&id, &msg);
            }
        }
    }

    /// 分叉 / 交接出来的新会话:落在现在这个工作区,高亮它(被当前的范围 / 过滤筛掉了就留在原处)
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

    /// 记一步 ^Z 能撤的。挤出去的删除撤不回了:现在就真删
    fn remember(&mut self, u: Undo) {
        if let Some(Undo::Remove { t, .. }) = undo::push(&mut self.undo, u) {
            let _ = self.trash.release(Some((&t.profile, &t.id)));
            self.purge = true;
        }
    }

    /// 撤销要找的会话:还列着的那个(tmux 名可能因为重启换了)。不在了说一声,返回 None
    fn still_here(&mut self, t: &Target) -> Option<(Target, &Session)> {
        let found = self.all.iter().position(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile);
        let Some(i) = found else {
            self.flash(&tr!("「{}」不在了", "“{}” is gone", clip(&t.title, 16).trim_end()));
            return None;
        };
        let s = &self.all[i];
        Some((Target { name: s.name.clone(), title: s.title.clone(), ..t.clone() }, s))
    }

    /// 按 key(`notes::key`)找会话现在的 tmux 名(重启会换),找不到就用记下的
    fn name_now(&self, key: &str, name: String) -> String {
        self.all.iter().find(|s| s.sub.is_none() && notes::key(s) == key).map_or(name, |s| s.name.clone())
    }

    /// ^Z:撤销上一步(见 undo.rs)
    fn undo_last(&mut self) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("还在忙:{b}", "still busy: {b}"));
        }
        let Some(u) = self.undo.pop() else { return self.flash(t!("没有能撤销的", "nothing to undo")) };
        let keep = self.keep_marks();
        let short = |t: &str| clip(t, 16).trim_end().to_string();
        match u.clone() {
            Undo::Gone { title } => self.flash(&tr!("「{}」撤不回", "“{}” can't be undone", short(&title))),
            Undo::Pin { key, name, title, rank } => {
                let name = self.name_now(&key, name);
                let m = match self.pins.put(&name, rank) {
                    Ok(()) => tr!(
                        "已撤销{}「{}」",
                        "undid {} “{}”",
                        if rank.is_some() { t!("取消置顶", "unpin") } else { t!("置顶", "pin") },
                        short(&title)
                    ),
                    Err(e) => tr!("没存上:{e}", "not saved: {e}"),
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
                    Ok(()) => tr!("已撤销标成{what}「{}」", "undid marking “{}” {what}", short(&title)),
                    Err(e) => tr!("没存上:{e}", "not saved: {e}"),
                };
                // 列表里的状态是叠过已读记录的,重算会丢掉原来的:直接放回去,下次刷新照新记录算
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
                    Err(e) => tr!("没存上:{e}", "not saved: {e}"),
                };
                self.flash(&m);
                self.apply_pins();
                self.rebuild();
            }
            Undo::Note { key, title, text } => {
                let m = match self.notes.set(&key, &text) {
                    Ok(()) => tr!("已撤销改便签「{}」", "undid the note change on “{}”", short(&title)),
                    Err(e) => tr!("便签没存上:{e}", "note not saved: {e}"),
                };
                self.flash(&m);
                self.apply_pins();
            }
            Undo::Remove { t, unarchive, start } => {
                self.revert(Job::Revive { t, unarchive, start }, u, None);
            }
            Undo::Park { t, p } => {
                if !self.all.iter().any(|s| s.pane == p.id && s.state == State::Dead) {
                    return self.flash(&tr!("「{}」不在停着了", "“{}” isn't stopped any more", short(&p.label)));
                }
                let m = tr!("已撤销停止:「{}」在原地再启动了", "undid the stop: “{}” started again in place", short(&p.label));
                self.revert(Job::Respawn { t, p }, u, Some(m));
            }
            Undo::Stop { t } => {
                let Some((t, s)) = self.still_here(&t) else { return };
                if !s.state.gone() && s.state != State::Dead {
                    return self.flash(&tr!("「{}」已经在跑了", "“{}” is already running", short(&t.title)));
                }
                self.revert(Job::Revive { t, unarchive: false, start: true }, u, None);
            }
            Undo::Move { t, from, groups } => {
                let Some((t, s)) = self.still_here(&t) else { return };
                let now = s.group.clone();
                if now.eq_ignore_ascii_case(&from) {
                    return self.flash(&tr!("已经在 {from} 里了", "already in {from}"));
                }
                let m = tr!("已撤销挪分组:「{}」回到 {from}", "undid the move: “{}” is back in {from}", short(&t.title));
                self.stay_on_move(&t.name, &from);
                self.revert(Job::Move { t, from: now, to: from, groups }, u, Some(m));
            }
            Undo::Rename { t, from, unlock } => {
                let Some((t, _)) = self.still_here(&t) else { return };
                let m = tr!("已撤销改名:回到「{}」", "undid the rename: back to “{}”", short(&from));
                self.revert(Job::Rename { t, to: from, unlock }, u, Some(m));
            }
            Undo::Mcp { t, dir, on, off } => {
                let Some((t, _)) = self.still_here(&t) else { return };
                let m =
                    tr!("已撤销改 MCP「{}」 · 重启后生效", "undid the MCP change on “{}” · takes effect after a restart", short(&t.title));
                self.revert(Job::Mcp { t, dir, on, off }, u, Some(m));
            }
            Undo::Group(a) => {
                let m = match &a {
                    groups::Act::Edit { group, to, .. } if group != to => {
                        tr!("已撤销改分组:{group} 改回 {to}", "undid the group change: {group} back to {to}")
                    }
                    _ => t!("已撤销改分组的默认目录", "undid the change to the group's default directory").to_string(),
                };
                self.revert(Job::Group(a), u, Some(m));
            }
        }
    }

    /// 撤销要在后台做的:放进后台,做完在 `reverted` 收尾
    fn revert(&mut self, job: Job, u: Undo, msg: Option<String>) {
        self.reverting = Some((job.clone(), u, msg));
        self.job = Some(job);
    }

    fn reverted(&mut self, u: Undo, msg: Option<String>, out: Outcome, fresh: Vec<Session>) {
        let m = match out {
            Outcome::Done(m) => {
                if let Undo::Remove { t, .. } = &u {
                    let _ = self.trash.forget(&t.profile, &t.id);
                    self.vanish.retain(|v| !matches!(&v.job, Job::Remove { t: g, pane: None } if g.id == t.id && g.profile == t.profile));
                }
                // 高亮停在撤回来的那个上
                if let Some(t) = u.target()
                    && let Some(s) = fresh.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile)
                {
                    self.selected = Some(s.name.clone());
                }
                msg.unwrap_or(m)
            }
            // 没撤成:这步放回去,能再按 ^Z
            Outcome::Failed(m) => {
                self.undo.push(u);
                m
            }
            Outcome::Made { msg, .. } | Outcome::Handed { msg, .. } | Outcome::Trashed { msg, .. } => msg,
        };
        self.set_sessions(fresh);
        self.flash(&m);
    }

    /// W:只看高亮会话所在的分组;已经只看一个分组了就回到全部分组
    fn toggle_own_group(&mut self) {
        let to = match (&self.group, self.current()) {
            (Some(_), _) => None,
            (None, Some(s)) => Some(s.group.clone()),
            (None, None) => return,
        };
        self.set_group(to);
    }

    /// w / a:进「开着的」/「全部」,再按一次回到进来之前的(比如从全部进的开着的就回全部;近几天的天数本来就不丢)
    fn toggle_scope(&mut self, to: Scope) {
        if self.scope == to {
            self.scope = if self.back == to { Scope::Recent } else { self.back };
        } else {
            self.back = self.scope;
            self.scope = to;
        }
        // 挑着 ^ 进「开着的」:两个对不上,状态让位
        if self.scope == Scope::Working && self.status == Some(Pick::Off) {
            self.status = None;
        }
        self.rebuild();
    }

    /// a:全部、不挑状态,一下看全。已经是这样了再按,回到进来之前的范围
    fn show_all(&mut self) {
        if self.status.take().is_some() && self.scope == Scope::All {
            return self.rebuild();
        }
        self.toggle_scope(Scope::All);
    }

    /// ! @ # & ^:只看这种状态,再按一次取消。在「开着的」里挑 ^,先退出开着的(同再按一次 w)
    fn toggle_status(&mut self, p: Pick) {
        self.status = if self.status == Some(p) { None } else { Some(p) };
        if self.scope == Scope::Working && self.status == Some(Pick::Off) {
            return self.toggle_scope(Scope::Working);
        }
        self.rebuild();
    }

    /// *:近一天 → 近三天 → 近一周 → 全部 → 近一天。在「开着的」时按也回到近一天。
    /// 轮到的「全部」是从近几天来的,a 退出时回近几天
    fn cycle_days(&mut self) {
        self.back = Scope::Recent;
        let next = DAYS.iter().position(|&d| d == self.days).map_or(DAYS.len(), |i| i + 1);
        match self.scope {
            Scope::Recent if next < DAYS.len() => self.days = DAYS[next],
            Scope::Recent => self.scope = Scope::All,
            _ => {
                self.scope = Scope::Recent;
                self.days = DAYS[0];
            }
        }
        let m = if self.scope == Scope::All { t!("全部", "all").to_string() } else { days_label(self.days) };
        self.flash(&m);
        self.rebuild();
    }

    fn ask_new(&mut self) {
        match self.draft() {
            Ok(d) => self.new_draft = Some(d),
            Err(e) => self.flash(&e),
        }
    }

    /// N / n 从哪开始填:停在会话上是照它再开一个;停在分组上是组的路径(见 `group_dir`)、进这个组
    fn draft(&self) -> Result<Draft, String> {
        match self.group_row().and_then(|g| Some((g, self.group_dir(g)?))) {
            Some((g, (s, dir, _))) => Ok(Draft::in_group(s, g, &dir, self.group_hints)),
            None => Ok(Draft::from(self.current(), self.group_hints)),
        }
    }

    /// 正在打字(过滤词 / 新会话路径):这时一次到达好几个键是输入法联想或粘贴,不是 Moshi 代敲
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
            KeyCode::Esc => {
                self.new_draft = None;
                self.flash(t!("已取消新建", "creation canceled"));
            }
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
        // 只列现在这个工作区的
        let base: Vec<usize> = (0..self.all.len())
            .filter(|&i| self.all[i].space == self.space && self.group.as_ref().is_none_or(|g| &self.all[i].group == g))
            .collect();
        // 个数只数会话(子窗格不单算)
        let heads = |v: &[usize]| v.iter().filter(|&&i| self.all[i].sub.is_none()).count();
        self.base_len = heads(&base);
        self.fell_back = false;
        // 会话和它下面的窗口同进同出:有一个符合就整家留下(窗口里在跑,会话本身闲着,也要看得到)
        // 子会话留下,它挂的父会话也留下,一直到顶格的那个(不然顶格,看不出是谁派的);反过来不带,免得父会话一动早跑完的子会话全翻出来
        let hangs = hangs(&self.all);
        let families = |pass: &dyn Fn(&Session) -> bool| -> Vec<usize> {
            let mut keep: HashSet<&str> = base.iter().map(|&i| &self.all[i]).filter(|s| pass(s)).map(|s| s.family()).collect();
            let ups: Vec<&str> =
                base.iter().map(|&i| &self.all[i]).filter(|s| keep.contains(s.family())).flat_map(|s| s.ancestors(&hangs)).collect();
            keep.extend(ups);
            base.iter().copied().filter(|&i| keep.contains(self.all[i].family())).collect()
        };
        // ! @ # & ^ 挑的状态和范围叠加;搜索时和范围一样不管
        let pick = self.status;
        let st = |s: &Session| pick.is_none_or(|p| p.hit(s.state));
        let view = if !kw.is_empty() {
            families(&|s| format!("{} {} {}", s.title, s.group, s.name).to_lowercase().contains(&kw))
        } else {
            match self.scope {
                Scope::All if pick.is_none() => base.clone(),
                Scope::All => families(&st),
                Scope::Working => families(&|s| open(s) && st(s)),
                Scope::Recent => {
                    let keep = families(&|s| (s.idle_for < window || s.state == State::Ask || s.attached || s.pinned) && st(s));
                    // 挑了状态是明说要看这种,没有就是空的,不退回全部
                    if keep.is_empty() && pick.is_none() {
                        self.fell_back = true;
                        base.clone()
                    } else {
                        keep
                    }
                }
            }
        };
        self.view = view;
        // 放宽天数能多出来几个;挑了状态就只数这种状态的(view 是它的子集,减不出负数)
        self.hidden = if kw.is_empty() && self.scope == Scope::Recent {
            let reach = if pick.is_some() { heads(&families(&st)) } else { self.base_len };
            reach - heads(&self.view)
        } else {
            0
        };
        // 折叠:每个折起来的分区只留第一个会话占位(置顶的不在分区里,不受影响)。过滤时全展开,免得搜不到
        self.folds.clear();
        if self.sectioned() && kw.is_empty() && !self.folded.is_empty() {
            let mut kept = vec![];
            for &i in &self.view {
                let s = &self.all[i];
                if !s.pinned && self.folded.contains(&s.group) {
                    let first = !self.folds.contains_key(&s.group);
                    let e = self.folds.entry(s.group.clone()).or_default();
                    e.0 += usize::from(s.sub.is_none());
                    e.1 += usize::from(s.state == State::Ask);
                    if !first {
                        continue;
                    }
                }
                kept.push(i);
            }
            self.view = kept;
        }
        // 刷新后高亮跟着会话走,而不是停在原来的行号
        let pos = self.selected.as_ref().and_then(|n| self.view.iter().position(|&i| &self.all[i].name == n));
        let idx = match pos {
            Some(p) => Some(p),
            None if self.view.is_empty() => None,
            None => Some(self.list.selected().unwrap_or(0).min(self.view.len() - 1)),
        };
        let head = self.head.take();
        self.select(idx);
        // 停在分组标题上的,刷新后还停在那;组折起来了就落在折叠行上,不在了就跟着会话走
        if let Some(p) = head.as_deref().and_then(|g| self.head_pos(g)) {
            self.select_head(p);
        }
    }

    /// 分组 g 展开着的标题下面第一个会话在可见列表第几个;不分区、折着、不在列表里 = None
    fn head_pos(&self, g: &str) -> Option<usize> {
        if !self.sectioned() || self.folds.contains_key(g) {
            return None;
        }
        self.view.iter().position(|&i| !self.all[i].pinned && self.all[i].group == g)
    }

    /// 高亮停在可见列表第 p 个会话所在分组的标题上(p 是组里第一个)
    fn select_head(&mut self, p: usize) {
        self.select(Some(p));
        self.head = Some(self.all[self.view[p]].group.clone());
    }

    /// 高亮停在哪个分组上:展开的组标题,或者折叠行。None = 停在会话上
    fn group_row(&self) -> Option<&str> {
        match &self.head {
            Some(g) => Some(g),
            None => self.on_fold_row().then(|| self.current().map(|s| s.group.as_str())).flatten(),
        }
    }

    fn on_group_row(&self) -> bool {
        self.group_row().is_some()
    }

    /// 高亮在分组上时,只能对会话做的事说一声、不做:返回 true
    fn on_group(&mut self, what: &str) -> bool {
        let m = match () {
            _ if self.head.is_some() => tr!("分组上不能{what}", "can't {what} on a group"),
            _ if self.on_fold_row() => tr!("先展开再{what}", "unfold first, then {what}"),
            _ => return false,
        };
        self.flash(&m);
        true
    }

    /// 分组的路径(在分组上 t / N 用):组的默认路径(目录还在的话),
    /// 不然是组里最近有动静的会话的目录(worktree 用原仓库;一样新的取列表里靠上的)。
    /// 连带给出那个会话(N 沿用它的工具 / 账号)、是不是组的默认路径
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

    /// 高亮停着的分组和它的路径(预览里写)。换了组、定时刷新(见 `glance_request`)才重算,不用每画一帧都去读库
    fn spot(&mut self) -> Option<Spot> {
        let g = self.group_row()?.to_string();
        if self.spot.as_ref().is_none_or(|s| s.group != g) {
            let sp = self.group_dir(&g).map(|(s, dir, set)| Spot { group: g.clone(), dir, from: (!set).then(|| s.title.clone()) });
            self.spot = sp;
        }
        self.spot.clone()
    }

    /// 当前可见的会话 + 被「近几天」藏起来的个数(给 `aca -l` 用)
    pub fn visible(&self) -> (Vec<&Session>, usize) {
        (self.view.iter().map(|&i| &self.all[i]).collect(), self.hidden)
    }

    fn select(&mut self, idx: Option<usize>) {
        self.head = None;
        self.list.select(idx);
        self.selected = idx.map(|i| self.all[self.view[i]].name.clone());
    }

    fn current(&self) -> Option<&Session> {
        self.list.selected().and_then(|i| self.view.get(i)).map(|&i| &self.all[i])
    }

    /// 可见列表里每一段的开头:分区时一个分组一段(置顶块单独一段),不分区时整个列表一段
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

    /// J:下一段的第一个,没有下一段就到最后一个。K:本段第一个,已经在了就上一段的第一个
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

    /// 正连着快速挪(滑动途中):预览先不换。单独挪一下(和上一下隔得够久)照样马上换
    fn sliding(&self) -> bool {
        self.moved.is_some_and(|(t, gap)| gap < SETTLE && t.elapsed() < SETTLE)
    }

    /// 还要等多久预览才换(run 循环按这个定下次醒来的时间,停下来就按时换上)
    pub fn settle_left(&self) -> Option<Duration> {
        self.sliding().then(|| self.moved.map_or(Duration::ZERO, |(t, _)| SETTLE.saturating_sub(t.elapsed())))
    }

    /// 刚挪过(见 HOLD)
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
        // 按屏幕上的顺序一格一格走,展开的分组标题也是一格
        let stops = self.stops();
        let cur = stops.iter().position(|&t| Some(t) == self.at()).unwrap_or(0) as isize;
        match stops[(cur + d).clamp(0, stops.len() as isize - 1) as usize] {
            Jump::Row(p) => self.select(Some(p)),
            Jump::Head(p) => self.select_head(p),
        }
    }

    /// 高亮能停的地方,按屏幕上从上到下(见 `jump_targets`)
    fn stops(&self) -> Vec<Jump> {
        let view: Vec<&Session> = self.view.iter().map(|&i| &self.all[i]).collect();
        jump_targets(&view, self.sectioned(), &self.folds)
    }

    /// 高亮现在停在哪
    fn at(&self) -> Option<Jump> {
        let p = self.list.selected()?;
        Some(if self.head.is_some() { Jump::Head(p) } else { Jump::Row(p) })
    }

    /// 回车:进会话;停在分组上就是折叠 / 展开
    fn enter(&mut self) {
        self.digits.clear();
        if self.on_group_row() {
            return self.toggle_fold();
        }
        // 子代理那一行没有窗格可进:看它的记录
        if self.current().is_some_and(|s| s.subagent.is_some()) {
            return self.open_trace();
        }
        // 登记的会话那行:agent 退了(窗格连画面还留着)、tmux 里整个没了,都是接着原来的对话再起(`session::start`)
        let own = |s: &Session| s.sub.is_none() && !s.deck_id.is_empty();
        if let Some((family, p)) =
            self.current().filter(|s| s.state == State::Dead && !own(s)).map(|s| (s.family().to_string(), self.pane_of(s)))
        {
            self.respawn = Some((family, p));
            return;
        }
        match self.current() {
            Some(s) if own(s) && (s.state.gone() || s.state == State::Dead) => {
                let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
                self.start = Some(t)
            }
            Some(s) => {
                let (focus, family) = ((!s.pane.is_empty()).then(|| s.pane.clone()), s.family().to_string());
                self.focus = focus;
                self.attach = Some(family);
            }
            None => {}
        }
    }

    /// t:在它真正干活的目录开个临时终端,画在列表下面;这一行开过就回到那个。停着的会话也行。
    /// 停在分组上:开在组的路径(见 `group_dir`),一个组一个
    fn open_term(&mut self) {
        self.digits.clear();
        if let Some(g) = self.group_row().map(String::from) {
            let key = group_key(&g);
            if self.terms.contains_key(&key) {
                self.term_on = Some(key);
                return;
            }
            let Some((_, dir, _)) = self.group_dir(&g) else {
                return self.flash(t!("不知道这个组在哪个目录", "no idea which directory this group is in"));
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

    /// 登录终端里出了登录链接(收起来了的也算):弹出来挑用哪个浏览器开
    fn catch_links(&mut self) {
        let mut got: Vec<(String, crate::term::Link)> =
            self.terms.values_mut().filter_map(|t| t.new_link().map(|l| (t.title.clone(), l))).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        for (title, link) in got {
            self.open(Box::new(crate::browser::Pick::new(&title, link)));
        }
    }

    /// 终端开着:按键都给它(包括 ^C),只有 Esc 收起 —— shell 接着跑,再按 t 回来
    fn on_term_key(&mut self, key: &str, k: KeyEvent) {
        if k.code == KeyCode::Esc && k.modifiers.is_empty() {
            self.term_on = None;
        } else if let Some(t) = self.terms.get_mut(key) {
            t.key(k);
        }
    }

    /// T:在它的会话里新开一个窗口跑 shell(同 prefix c),目录是它真正干活的目录
    fn window_here(&mut self) {
        self.digits.clear();
        if self.on_group(t!("开窗口", "open a window")) {
            return;
        }
        let Some(s) = self.current() else { return };
        if s.state.gone() {
            return self.flash(t!("它没在跑,先回车启动", "it isn't running; press Enter to start it first"));
        }
        self.split = Some(self.split_of(s, false));
    }

    /// 在 s 那格旁边分屏:窗口行分的是那一格,会话行不知道窗格 id 就分它当前那格
    fn split_of(&self, s: &Session, yazi: bool) -> Split {
        let target = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        // 会话里另开的窗口那一行:账号跟它所在的会话
        let head = if s.sub.is_none() { Some(s) } else { self.all.iter().find(|x| x.sub.is_none() && x.name == s.family()) };
        let env = head.map(crate::data::shell_env).unwrap_or_default();
        Split { yazi, name: s.family().to_string(), target, w: Where::of(s), side: self.screen.width >= 100, env }
    }

    /// y:在它真正干活的目录开 yazi,q 退出就回到 aca。停着的会话也行(目录用它记下的)
    fn open_yazi(&mut self) {
        self.digits.clear();
        if self.on_group(t!("开 yazi", "open yazi")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let mut sp = self.split_of(s, true);
        if s.state.gone() {
            sp.target.clear();
        }
        self.external = Some(External::Yazi(sp));
    }

    /// 弹窗里的界面:这台客户端现在停在哪(tmux 的会话号、窗格)→ 高亮它(那一格有自己的一行就高亮那行,不然是会话那行)
    pub fn highlight_here(&mut self, session: &str, pane: &str) {
        let row = self.all.iter().find(|s| !pane.is_empty() && s.pane == pane && s.tmux == session);
        let Some(name) = row.or_else(|| self.all.iter().find(|s| s.sub.is_none() && s.tmux == session)).map(|s| s.name.clone()) else {
            return;
        };
        self.selected = Some(name);
        let all = std::mem::take(&mut self.all);
        self.set_sessions(all);
    }

    /// 开始跟随(见 follow.rs):session / pane 是这台客户端现在停着的地方。先 `highlight_here`,高亮落在哪一行就认哪一行是「原来那个」
    pub fn follow_from(&mut self, session: &str, pane: &str) {
        let row = self.current().filter(|_| !self.on_group_row()).map(|s| s.name.clone()).unwrap_or_default();
        self.following = Some(Follow::new(Place { row, session: session.into(), pane: pane.into() }));
    }

    /// 高亮的这一行要把客户端带到哪:分组行、没在跑的会话 = 哪也不去
    fn follow_want(&self) -> Option<Place> {
        let s = self.current().filter(|_| !self.on_group_row())?;
        // 子代理那一行没有窗格:带到它的会话那一格
        let pane = match &s.subagent {
            Some(_) => self.all.iter().find(|x| x.sub.is_none() && x.name == s.family()).map_or_else(String::new, |x| x.pane.clone()),
            None => s.pane.clone(),
        };
        (!s.tmux.is_empty() && !s.state.gone()).then(|| Place { row: s.name.clone(), session: s.tmux.clone(), pane })
    }

    /// 跟随的一步(run 循环每轮问一次):高亮停稳了、该换地方了就给出切到哪。滑动途中、正在打过滤词时不切
    pub fn follow_step(&mut self) -> Option<Place> {
        let want = if self.sliding() || self.filtering { None } else { self.follow_want() };
        let f = self.following.as_mut()?;
        let to = f.step(want)?;
        // 路过不算看过:切过去 tmux 就记它「连进去过」,核心那边的「跑完没看」跟着没了 → 记成未读,⏎ 留下、关在它上面才清
        let passing = !f.is_home(&to.session);
        if let Some(key) =
            self.all.iter().find(|s| passing && s.sub.is_none() && s.tmux == to.session && s.state == State::Unread).map(notes::key)
        {
            let _ = self.reads.mark_unread(&key, &self.keep_marks());
        }
        Some(to)
    }

    /// 跟随着关掉了(^\ / qq;⏎ 和 Esc 各有各的去处):人留在旁边露出来的那个会话,它算看过
    pub fn follow_done(&mut self) {
        if let Some(session) = self.following.as_ref().and_then(|f| f.stayed()).map(String::from) {
            self.mark_seen(&session);
        }
    }

    /// 可见列表第 pos 个是不是折叠行(折起来的分区的占位)
    fn is_fold_row(&self, pos: usize) -> bool {
        self.view.get(pos).is_some_and(|&i| {
            let s = &self.all[i];
            !s.pinned && self.folds.contains_key(&s.group)
        })
    }

    fn on_fold_row(&self) -> bool {
        self.list.selected().is_some_and(|p| self.is_fold_row(p))
    }

    /// 账号列宽:按所有会话里最长的定(不按当前显示的),切范围 / 分组时标题不跟着伸缩
    pub fn who_width(&self) -> usize {
        self.all.iter().map(|s| s.who.width()).max().unwrap_or(0).min(WHO_MAX)
    }

    pub fn fold_info(&self) -> &Folds {
        &self.folds
    }

    /// o:折叠 / 展开当前分区(停在折叠行上就展开)。折起来后高亮落在折叠行上
    fn toggle_fold(&mut self) {
        let Some(s) = self.current() else { return };
        if !self.sectioned() {
            return self.flash(t!("单个分组里不能折", "can't fold inside a single group"));
        }
        if s.pinned {
            return self.flash(t!("置顶的不能折", "pinned ones can't fold"));
        }
        let g = s.group.clone();
        match self.folded.contains(&g) {
            false => {
                // 占位的是这个分区第一个会话,高亮跟过去
                let first = self.view.iter().map(|&i| &self.all[i]).find(|x| !x.pinned && x.group == g).map(|x| x.name.clone());
                self.folded.insert(g.clone());
                self.selected = first;
                self.flash(&tr!("折叠「{}」", "folded “{}”", group_label(&g)));
            }
            true => {
                self.folded.remove(&g);
                self.flash(&tr!("展开「{}」", "unfolded “{}”", group_label(&g)));
            }
        }
        self.rebuild();
        // 展开了:高亮还在这个组上(停在标题上)
        if let Some(p) = self.head_pos(&g).filter(|_| !self.folded.contains(&g)) {
            self.select_head(p);
        }
    }

    /// h / ←:往外一层。窗口 / 子代理 / 子会话 → 它挂的会话;展开着子代理的会话上 = 把它们收起来;
    /// 会话、展开的标题上 = 把分组折起来(只有 h 折,只有 l 展);
    /// 只看一个分组时回到全部分组(高亮留在原处)。折叠行、置顶块里的没有外层,不动
    fn step_out(&mut self) {
        if self.head.is_some() {
            return self.toggle_fold();
        }
        let Some(s) = self.current() else { return };
        // 展开着子代理的会话:先把它们收起来,再按一次才轮到分组
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

    /// l / →:往里一层。折叠行上 = 展开(高亮停在标题上),分组标题上 = 进到组里第一个会话;
    /// 会话上:它派了子代理就把它们展开成行(高亮不动),已经展开了就进到第一个;没派的不动
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

    /// O:有折着的就全展开,没有就把所有分区都折起来
    fn toggle_fold_all(&mut self) {
        if !self.sectioned() {
            return self.flash(t!("单个分组里不能折", "can't fold inside a single group"));
        }
        if self.folded.is_empty() {
            self.folded.extend(self.view.iter().map(|&i| &self.all[i]).filter(|s| !s.pinned).map(|s| s.group.clone()));
            self.flash(t!("全部折叠", "all folded"));
        } else {
            self.folded.clear();
            self.flash(t!("全部展开", "all unfolded"));
        }
        self.rebuild();
    }

    /// c:复制最后一条回复;B:复制路径 / 分支 / 会话 id(同 agent-deck)。真正的复制由 run 循环做
    fn ask_copy(&mut self, last: bool) {
        if self.on_group(t!("复制", "copy")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let title = s.title.clone();
        self.clip = Some(if last {
            // 拿不到(普通 tmux 会话、还没跑完过一轮)时用屏幕上的字
            let screen = preview_lines(s).iter().map(ansi::plain).collect::<Vec<_>>().join("\n");
            Clip::Last { profile: s.profile.clone(), id: s.deck_id.clone(), title, screen }
        } else {
            Clip::Info { title, text: s.info.clone() }
        });
    }

    /// C:挑一样复制(aca id / session id / path,见 copies.rs)
    fn open_copy_menu(&mut self) {
        if self.on_group(t!("复制", "copy")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let id = if s.profile == crate::data::REGISTERED { s.deck_id.as_str() } else { "" };
        let m = crate::copies::Menu::new(&s.title, id, &s.info);
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
        // ^`:回到上一个会话,同 tmux 里的 ^`。弹窗开着时 tmux 不走键位,把它转成 NUL 给 aca,到手是 ^空格
        // (真的 ^空格 被 macOS 拿去换输入法,到不了这里);快捷键弹窗里只是查它
        if crate::data::in_popup()
            && k.code == KeyCode::Char(' ')
            && k.modifiers.contains(KeyModifiers::CONTROL)
            && !self.reader.as_ref().is_some_and(|r| r.help)
        {
            return self.back_session();
        }
        // 终端上面盖着弹窗(登录链接出来时弹的,见 browser.rs):键先给弹窗,关了再回终端
        if self.term_on.is_some() && !self.overlays.is_empty() {
            return self.on_overlay_key(k);
        }
        if let Some(key) = self.term_on.clone() {
            return self.on_term_key(&key, k);
        }
        // ^\:开弹窗的那个键,再按一下关掉(弹窗开着时 tmux 不走键位,键到了这里)。人留在旁边现在露出来的那个会话;
        // 快捷键弹窗里只是查它
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
        if self.theme_menu.is_some() {
            return self.on_theme_key(k);
        }
        if self.settings.is_some() {
            return self.on_settings_key(k);
        }
        // 确认:只认 y,别的键(包括回车)一律取消 —— 删了就回不来,重启会打断它
        if let Some(c) = self.confirm.take() {
            let yes = k.code == KeyCode::Char('y');
            match c {
                Confirm::Remove(r) if yes => {
                    let t = Target { name: r.name, title: r.title, profile: r.profile, id: r.id };
                    // 先从列表里拿掉,不等后台删完(要等进程退干净)
                    let running = !t.id.is_empty()
                        && self.all.iter().any(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile && !s.state.gone());
                    let job = Job::Remove { t, pane: r.pane.map(|(p, _)| p) };
                    self.vanish.push(Vanish { job: job.clone(), until: None, running });
                    self.job = Some(job);
                    self.drop_vanished();
                    self.apply_pins();
                    self.rebuild();
                }
                Confirm::Remove(r) if r.pane.is_some() => self.flash(t!("没关", "not closed")),
                Confirm::Remove(_) => self.flash(t!("没删", "not deleted")),
                Confirm::Restart { t, force, .. } if yes => self.job = Some(Job::Restart { t, force }),
                Confirm::Restart { .. } => self.flash(t!("没重启", "not restarted")),
                Confirm::Stop { t, .. } if yes => self.job = Some(Job::Stop { t }),
                Confirm::Stop { .. } => self.flash(t!("没停", "not stopped")),
                Confirm::Pane { t, p, restart, .. } if yes => {
                    self.job = Some(if restart { Job::Respawn { t, p } } else { Job::Park { t, p } })
                }
                Confirm::Pane { restart, .. } => {
                    self.flash(if restart { t!("没重启", "not restarted") } else { t!("没停", "not stopped") })
                }
                Confirm::Send { t, pane, key, text } if yes => self.job = Some(Job::Send { t, pane, text, key }),
                Confirm::Send { .. } => self.flash(t!("没发", "not sent")),
                Confirm::Adopt { t, plan } if yes => self.job = Some(Job::Adopt { t, plan }),
                Confirm::Adopt { .. } => self.flash(t!("没接管", "not adopted")),
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
            KeyCode::Esc if self.status.is_some() => {
                self.status = None;
                self.rebuild();
            }
            KeyCode::Esc => {
                // 跟随着:回到打开时停着的地方再退(run 循环里 `follow_step` 切)
                if let Some(f) = self.following.as_mut() {
                    f.back();
                }
                self.quit = true
            }
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
            KeyCode::Char('a') => self.show_all(),
            KeyCode::Char('w') => self.toggle_scope(Scope::Working),
            KeyCode::Char('!') => self.toggle_status(Pick::Run),
            KeyCode::Char('@') => self.toggle_status(Pick::Wait),
            KeyCode::Char('#') => self.toggle_status(Pick::Idle),
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
            KeyCode::Char('d') if self.on_group_row() => self.open_groups(),
            KeyCode::Char('d') => self.ask_remove(),
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
            KeyCode::Char('v') => self.cycle_preview(),
            KeyCode::Char('r') if ctrl => self.refresh = true,
            KeyCode::Char('r') => self.ask_rename(),
            KeyCode::Char('R') => self.ask_restart(),
            KeyCode::Char('D') => self.ask_stop(),
            KeyCode::Char('m') => self.ask_move(),
            // F 是原来的「起名再分叉」,现在 f 就是
            KeyCode::Char('f' | 'F') => self.ask_fork(),
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
            KeyCode::Char('e') if self.on_group_row() => {
                self.on_group(t!("写便签", "write a note"));
            }
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
                    // 12 号不存在 → 当作重新按了 2
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
                // 两栏:鼠标在哪边滚哪边
                None if v.wide && m.column >= v.split => v.steps.scroll(d),
                None if v.wide => v.chat.scroll(d),
                None => v.cur().scroll(d),
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.move_by(-1),
            MouseEventKind::ScrollDown => self.move_by(1),
            // 分组列表开着:点哪都是关
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
                // 列表项有一行也有两行(带问题的),还夹着分区标题;从当前滚动位置往下累加高度找被点的那项
                let mut y = a.y;
                for &(pos, head, h) in self.drawn.iter().skip(self.disp.offset()) {
                    if m.row < y + h {
                        let on = self.list.selected();
                        // 点已高亮的那行 = 回车(进入 / 折叠);分组标题也点得中,空行和置顶块的标题不算
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

/// 桌面(够宽够高):四周留白(左右 3 格、上下 1 行,顶栏下、底栏上各空一行);列表写编号、组和组之间空一行
fn roomy(area: Rect) -> bool {
    area.width >= 100 && area.height >= 30
}

/// 顶栏左边的标签:工作区的名字(只用着 1 时是「会话」);只看一个分组时换成那个分组
fn chip_text(app: &App) -> String {
    let label = app.group.as_ref().map_or_else(|| app.space_label(), |g| group_label(g).to_string());
    format!(" {} ", clip(&label, 14).trim_end())
}

/// 顶栏:左边是在看什么(强调色的标签 + 天数 / 全部 / 开着的,或者正在搜的词),右边是各状态几个。
/// 窄了先把计数写短(◐1),再只留当前那个范围
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
        tab(t!("开着的", "Open").into(), app.scope == Scope::Working);
        if app.fell_back {
            v.push(Span::styled(t!("  没有动静，显示全部", "  nothing active, showing all"), dim()));
        }
        v
    };
    let mut extra = vec![];
    if !app.digits.is_empty() && app.digits_at.elapsed() <= DIGIT_GAP {
        extra.push(Span::styled(format!("  #{}", app.digits), theme::accent().add_modifier(Modifier::BOLD)));
    }
    // ! @ # & ^ 挑的状态:那几格铺上选中的底色(搜索时不管状态,也就不铺)。空闲、■ 平时不数,挑了才列,0 个也列
    let pick = app.status.filter(|_| app.filter.is_empty());
    // 只数这个工作区的;■ 那格数 ^ 挑的全部(已退出、没在跑的出错也算)
    let count = |st: State| {
        let off = st == State::Stopped;
        app.all.iter().filter(|s| s.subagent.is_none() && s.space == app.space && (s.state == st || off && Pick::Off.hit(s.state))).count()
    };
    let counts = |long: bool| -> Vec<Span<'static>> {
        let mut v = vec![];
        for (st, name) in [
            (State::Ask, t!("待确认", "needs you")),
            (State::Running, t!("运行中", "running")),
            (State::Unread, t!("未读", "unread")),
            (State::Error, t!("出错", "error")),
            (State::Idle, t!("空闲", "idle")),
            (State::Stopped, t!("没在跑", "stopped")),
        ] {
            let n = count(st);
            let on = pick.is_some_and(|p| p.hit(st));
            if !on && (n == 0 || matches!(st, State::Idle | State::Stopped)) {
                continue;
            }
            let (ic, sty) = icon(st);
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
        // 出错了的插件:几个插件,不是几处
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
            v.push(Span::styled(t!("没有要管的", "nothing to attend to"), dim()));
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

/// 下面放预览时的分隔:一条淡线,标题嵌在线上 ——「─ ● 标题 ───── Claude·team ─」
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
    // 只看列表:底下留一行,写高亮的那个在干什么
    let summary = side.is_none() && app.mode() == Preview::Off && !app.view.is_empty();
    // 便签正写在预览里(带光标):底栏就不再重复一遍内容,只留按键
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
    // t 的终端:占下面一半多一点,上面照常是列表 / 预览
    let term = app.term_on.clone().filter(|k| app.terms.contains_key(k));
    let (body, term_area) = match term {
        Some(_) => {
            let h = (body.height * 11 / 20).max(5).min(body.height.saturating_sub(3));
            let [b, t] = Layout::vertical([Constraint::Fill(1), Constraint::Length(h)]).areas(body);
            (b, Some(t))
        }
        None => (body, None),
    };
    // 分割比例固定,不随会话多少伸缩 —— 会话一多一少,预览的边界不会上下跳
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
    let mut rows = {
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        list_rows(&view, app.sectioned(), list_area.width as usize, app.jump.as_deref(), &app.folds, app.who_width(), roomy)
    };
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

    // 列表
    if app.view.is_empty() {
        let msg = if !app.filter.is_empty() {
            tr!(" 没有匹配「{}」的", " nothing matches “{}”", app.filter)
        } else if let Some(p) = app.status {
            let (key, none) = p.label();
            match app.hidden {
                0 => tr!(" {none} · {key} 返回", " {none} · {key} back"),
                n => tr!(" {none} · 还有 {n} 个更早的 · * 放宽", " {none} · {n} more, older · * widens"),
            }
        } else if app.scope == Scope::Working {
            t!(" 没有开着的 · w 返回", " nothing open · w back").into()
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
            .map(|r| {
                let lines: Vec<Line> = r
                    .lines
                    .into_iter()
                    .map(|segs| Line::from(segs.into_iter().map(|(t, s)| Span::styled(t, s)).collect::<Vec<_>>()))
                    .collect();
                ListItem::new(Text::from(lines))
            })
            .collect();
        // 选中的整块铺淡紫底,各段的颜色照留(状态、账号一眼还看得出)。
        // 留一项余量:滚到某组第一个会话时,它上面的分区标题也露出来
        let list = List::new(items).highlight_style(Style::new().bg(theme::pal().sel)).scroll_padding(1);
        f.render_stateful_widget(list, list_area, &mut app.disp);
    }

    // 预览:高亮会话的屏幕底部。放右边是全屏唯一一个框(强调色,标题嵌在上边框);
    // 放下面是一条嵌着标题的强调色的线,同时把列表和预览隔开
    let spot = if app.on_group_row() { app.spot() } else { None };
    let shown = if app.on_group_row() { None } else { preview_name(app).and_then(|n| app.all.iter().position(|s| s.name == n)) };
    if let Some(pa) = prev_area {
        let (ic, title, label) = match shown.map(|i| &app.all[i]) {
            Some(s) => {
                // 账号名太长时截它,标题优先
                let label = clip(&s.agent_label(), (pa.width as usize / 3).max(12)).trim_end().to_string();
                let label = if label.is_empty() { vec![] } else { vec![Span::styled(label, agent_style(s.agent))] };
                match &s.subagent {
                    Some(a) => (agent_icon(a.phase), s.title.clone(), vec![Span::styled(t!("子代理", "subagent"), dim())]),
                    None => (icon(s.state), s.title.clone(), label),
                }
            }
            None => (("▸", dim()), app.current().map(|s| group_label(&s.group).to_string()).unwrap_or_default(), vec![]),
        };
        // 列表露得少(预览为主 / 只看列表)时,线上写着高亮的是第几个
        let at = Span::styled(format!("{}/{}", app.list.selected().map_or(0, |p| p + 1), app.view.len()), dim());
        let label = if side == Some(Side::Focus) { [label, vec![Span::raw("  "), at.clone()]].concat() } else { label };
        let inner = if summary {
            // 只看列表:预览收成一条线,写状态和目录
            let what = match (shown.map(|i| &app.all[i]), &spot) {
                (Some(s), _) => format!("{} · {}", state_word(s), create::tilde(&s.dir)),
                (None, Some(sp)) => format!("{title} · {}", create::tilde(&sp.dir)),
                (None, None) => title.clone(),
            };
            f.render_widget(Paragraph::new(title_rule(ic, &what, vec![at], pa.width as usize)), pa);
            Rect { height: 0, ..pa }
        } else if side == Some(Side::Right) {
            let t = clip(&title, (pa.width as usize).saturating_sub(12 + label.iter().map(|s| s.content.width()).sum::<usize>()).max(6));
            // 淡色的框:列表旁边不再立着一大块浓色;标题照常是深色粗体(不写前景色的话会沾上边框的颜色)
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
                if let Some(sp) = &spot {
                    let lines = group_pane(sp, app.glances.get(&group_key(&sp.group)), inner.width as usize, inner.height as usize);
                    f.render_widget(Paragraph::new(lines), inner);
                }
            }
            Some(i) => {
                let s = &app.all[i];
                // 正在写这个会话的便签:显示正在写的
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
                // 屏幕露底下那截(最新的在最下面);交回话了的子代理从头露(它交回来的话排在最前面)
                let tail = match &s.subagent {
                    Some(a) if a.reported => &lines[..lines.len().min(body.height as usize)],
                    _ => &lines[lines.len().saturating_sub(body.height as usize)..],
                };
                f.render_widget(Paragraph::new(tail.iter().map(ansi::to_line).collect::<Vec<_>>()), body);
                ansi::paint_links(f.buffer_mut(), body, tail);
            }
        }
    }

    // 底栏:按键提示,或者短暂显示一条消息。新建、改名、确认、选工具、快捷键都是弹窗,盖在这一整层上面
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
            &[vec![("Esc", t!("写完", "done"), 0), ("⏎", t!("换行", "newline"), 0), ("^G", t!("编辑器", "editor"), 1)]],
            Some(0),
            w,
        ),
        // 按了 q / Q:比刚才的提示要紧
        _ if app.pending.is_some() => pending_bar(app, w),
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ if term_area.is_some() => menu_bar(&[vec![("Esc", t!("收起", "hide"), 0), ("exit", t!("关闭", "close"), 0)]], Some(0), w),
        _ if layout_strip.is_some() => layout_strip.unwrap_or_default(),
        _ if doing.is_some() => Line::styled(doing.clone().unwrap_or_default(), theme::warn()).centered(),
        _ if app.filtering => {
            menu_bar(&[vec![("⏎", t!("确定", "ok"), 0), ("↑↓", t!("选择", "select"), 0), ("Esc", t!("清除", "clear"), 0)]], Some(0), w)
        }
        _ if app.jump.is_some() => {
            Line::styled(t!("按字母打开 / 停在分组上 · Esc 取消", "press a letter to open / land on a group · Esc cancels"), theme::warn())
                .centered()
        }
        _ => {
            // 三组:管会话 │ 对着高亮的这个做(主操作)│ 看。只放最常用的,别的都在 ? 里;放不下时先丢排在后面的,? 和 ⏎ 永远在
            // 停在子代理那一行:只能看它的记录、回它的会话,管会话的那几个键不列
            let on_agent = app.head.is_none() && app.current().is_some_and(|s| s.subagent.is_some());
            let enter = match () {
                _ if app.head.is_some() => t!("折叠", "fold"),
                _ if app.on_fold_row() => t!("展开", "unfold"),
                _ if on_agent => t!("看记录", "record"),
                // 跟随着:旁边已经是它了
                _ if app.following.is_some() => t!("留下", "stay"),
                _ => t!("打开", "open"),
            };
            let mut act: Vec<MenuItem> = vec![];
            if !app.filter.is_empty() {
                act.push(("Esc", t!("清除", "clear"), 0));
            } else if app.following.is_some() && app.status.is_none() {
                act.push(("Esc", t!("回去", "go back"), 0));
            }
            act.push(("⏎", enter, 0));
            if app.view.iter().any(|&i| waiting(&app.all[i])) {
                act.push(("i", t!("待处理", "next"), 1));
            }
            if on_agent {
                act.push(("h", t!("回会话", "its session"), 6));
            } else {
                act.push(("e", t!("便签", "note"), 6));
            }
            // 有便签才有 s 可按
            if !app.on_group_row() && !on_agent && app.current().is_some_and(|s| s.noted) {
                act.push(("s", t!("发送", "send"), 6));
            }
            menu_bar(
                &[
                    if on_agent { vec![] } else { vec![("N", t!("新建", "new"), 2), ("d", t!("删除", "delete"), 7)] },
                    act,
                    vec![("/", t!("搜索", "search"), 3), ("v", t!("布局", "layout"), 4), ("?", t!("快捷键", "keys"), 0)],
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

    // 弹窗
    if app.reader.as_ref().is_some_and(|r| r.help) {
        draw_help(f, app);
    } else if let Some(d) = &app.new_draft {
        draw_draft(f, d);
    } else if let Some(p) = app.prompt.as_ref().filter(|_| !note_in_preview) {
        draw_prompt(f, p);
    } else if let Some((menu, sel)) = &app.tool_menu {
        draw_tools(f, menu, *sel, app.handoff.as_ref());
    } else if let Some(o) = app.overlays.last() {
        // 底栏的那句话(刚说的,不然是后台在做的)也写在弹窗最下面:手机上弹窗盖着底栏,开着弹窗做的事结果就看不到了
        let note = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.clone()).or_else(|| app.doing());
        if o.fill() {
            // 框的大小不变:最底下留一行给那句话(长了两行),没话说空着,页面的行不跟着跳
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
    } else if let Some(m) = &app.theme_menu {
        draw_themes(f, m);
    } else if let Some(st) = &app.settings {
        draw_settings(f, st);
    } else if let Some(c) = &app.confirm {
        draw_confirm(f, c, &app.all);
    }
}

/// 按了 q / Q 等下一个键:底栏列出能去的工作区(h 不列,现在这个也不列)
fn pending_bar(app: &App, w: usize) -> Line<'static> {
    let names: Vec<(String, String)> =
        app.space_list().into_iter().filter(|&c| c != app.space).map(|c| (c.to_string(), app.spaces.name(c))).collect();
    let mut items: Vec<MenuItem> = names.iter().enumerate().map(|(i, (k, n))| (k.as_str(), n.as_str(), (i + 1).min(200) as u8)).collect();
    let (what, tail): (&str, Vec<MenuItem>) = match app.pending {
        Some(Pending::Send) => (t!("送到", "Send to"), vec![("Esc", t!("取消", "cancel"), 0)]),
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

/// S:工作区列表。一行一个:键、名字、几个会话、各状态几个;现在这个的键用强调色,高亮的整行铺底色
fn draw_spaces(f: &mut Frame, app: &App, sel: usize) {
    let list = app.space_list();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("工作区", "Workspace"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let name_w = list.iter().map(|&c| app.spaces.name(c).width()).max().unwrap_or(0).min(16);
        for (i, &c) in list.iter().enumerate() {
            let heads: Vec<&Session> = app.all.iter().filter(|s| s.space == c && s.sub.is_none()).collect();
            let key_st = if c == app.space { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut line = menu_row(&c.to_string(), key_st, &app.spaces.name(c), name_w, &heads);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[(t!("⏎ 键", "⏎ key"), t!("切换", "switch")), ("^R", t!("改名", "rename"))], vec![], w));
        out
    });
}

/// 工作区 / 分组列表的一行:键、名字、几个会话、各状态几个
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

/// Tab:这个工作区的分组列表。一行一个:字母(全部分组是 ␣)、组名、几个会话、各状态几个;
/// 现在看着的那组字母用强调色,高亮的整行铺底色
fn draw_groups(f: &mut Frame, app: &App, sel: usize) {
    let rows = app.group_rows();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("分组", "Groups"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let name = |g: &Option<String>| g.as_deref().map_or(t!("全部", "all"), group_label).to_string();
        let name_w = rows.iter().map(|(_, g)| name(g).width()).max().unwrap_or(0).min(20);
        for (i, (c, g)) in rows.iter().enumerate() {
            let heads: Vec<&Session> =
                app.all.iter().filter(|s| s.space == app.space && s.sub.is_none() && g.as_ref().is_none_or(|g| &s.group == g)).collect();
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
        out.push(keys_line(&[(t!("⏎ 字母", "⏎ letter"), t!("切换", "switch")), ("␣", t!("全部", "all"))], vec![], w));
        out
    });
}

/// t 的终端:顶上一行写开在哪,下面是 shell 的画面,光标放在它的光标上
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
    // 上面盖着弹窗(登录链接的)时不放光标:键在弹窗那
    let covered = !app.overlays.is_empty();
    if let Some(pos) = app.terms.get(key).and_then(|t| t.render(body, f.buffer_mut())).filter(|_| !covered) {
        f.set_cursor_position(pos);
    }
}

/// 弹窗放哪:够宽够高(桌面)居中浮着、带阴影;窄(手机)从底下升起、占满宽
fn modal_area(full: Rect, w: u16, h: u16) -> (Rect, bool) {
    if full.width >= 70 && full.height >= 20 {
        let (w, h) = (w.min(full.width.saturating_sub(4)), h.min(full.height.saturating_sub(2)));
        (Rect { x: full.x + (full.width - w) / 2, y: full.y + (full.height - h) / 2, width: w, height: h }, true)
    } else {
        let h = h.min(full.height);
        (Rect { x: full.x, y: full.y + full.height - h, width: full.width, height: h }, false)
    }
}

/// 弹窗在这块屏幕上是不是浮着的(桌面);手机上是底部的一整条
fn floating(full: Rect) -> bool {
    modal_area(full, 1, 1).1
}

/// 把 (x, y) 那格换成窄字符之前:它要是宽字符的后半格、或者本身是宽字符,把另一半清成空格,免得字错位
fn split_wide(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    let area = buf.area;
    if x > area.left() && buf[(x - 1, y)].symbol().width() > 1 {
        buf[(x - 1, y)].set_symbol(" ");
    }
    if buf[(x, y)].symbol().width() > 1 && x + 1 < area.right() {
        buf[(x + 1, y)].set_symbol(" ");
    }
}

/// 弹窗的底和框:底下整层变灰(字换成淡灰,去掉底色和粗细),桌面上右下错开一格铺 ░ 阴影,
/// 面板清空后画圆角边框(平常强调色,删除用红)。返回面板里面(去掉边框和内边距)
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

/// 面板本身:压在边上的宽字符先拆开,清空,画圆角边框。返回里面(去掉边框和内边距)
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

/// 配色列表。不像别的弹窗把底下变灰:底下的 aca 就是预览。桌面上贴左下角浮着(列表、预览大半露在外面),
/// 手机上从底下升起。一行一套:记着的那套打 ●,名字,它自己的强调色、状态(红绿黄)、账号的颜色
fn draw_themes(f: &mut Frame, m: &ThemeMenu) {
    let (outer, w) = side_width(f.area(), 50);
    let mut body = vec![modal_title(t!("配色", "Colors"), theme::accent(), esc_hint(t!("退回", "back")), w), Line::raw("")];
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
    body.push(keys_line(&[("↑↓", t!("预览", "preview")), ("⏎", t!("用它", "use it"))], vec![], w));
    side_panel(f, outer, body);
}

/// 边上的面板要多宽:桌面上 want(屏幕窄了收一点),手机上占满;第二个是里面能写多宽
fn side_width(full: Rect, want: u16) -> (u16, usize) {
    let float = floating(full);
    let outer = if float { want.min(full.width.saturating_sub(4)) } else { full.width };
    (outer, outer.saturating_sub(if float { 6 } else { 4 }) as usize)
}

/// 不把底下变灰的面板(配色列表:底下的 aca 照常看得见,就是预览)。
/// 桌面上贴左下角浮着,手机上从底下升起
fn side_panel(f: &mut Frame, outer: u16, body: Vec<Line<'static>>) {
    let full = f.area();
    let float = floating(full);
    let h = body.len() as u16 + modal_chrome(full);
    let r = if float {
        let h = h.min(full.height.saturating_sub(2));
        // 左边、底栏上面各留两格
        Rect { x: full.x + 2, y: full.bottom().saturating_sub(h + 2), width: outer, height: h }
    } else {
        modal_area(full, outer, h).0
    };
    let inner = panel(f, r, float, theme::pal().accent);
    f.render_widget(Paragraph::new(body), inner);
}

/// `^S` 设置页:居中的弹窗(和 Tab / S 一样底下变灰;挑主题时换成左下角不变灰的配色列表)。
/// 一行一项,「键  名字  现在的值」,› = 进去再挑,↗ = 用编辑器打开;写错的是它自己的一页,账号是另开的弹窗(acctpage.rs)
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
                    // 两种都摆出来,在用的那种亮
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
                    SetRow::Problems => (vec![Span::styled(tr!("{} 处写错", "problems: {}", s.bad.len()), theme::warn())], "›"),
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
            body.push(keys_line(&[(t!("⏎ 键", "⏎ key"), t!("改", "change"))], vec![], w));
        }
        SetPage::Problems => {
            body = vec![modal_title(t!("写错的", "Problems"), theme::accent(), esc_hint(t!("退回", "back")), w), Line::raw("")];
            for b in &s.bad {
                body.extend(wrap_width(b, w).into_iter().map(|l| Line::styled(l, theme::warn())));
            }
            body.push(Line::raw(""));
            body.push(keys_line(&[("e", t!("编辑 config.toml", "edit config.toml"))], vec![], w));
        }
    }
    body
}

/// 弹窗外框加内边距一共占几行(按弹窗最后落在哪算)
fn modal_chrome(full: Rect) -> u16 {
    if floating(full) { 4 } else { 2 }
}

/// 一行:左边一段、右边一段(靠右),中间补空格;放不下时先丢右边
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

/// 弹窗的标题行:强调色粗体的标题,右边一段(关掉的键 / 第几页)
pub fn modal_title(title: &str, st: Style, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    spread(vec![Span::styled(clip(title, w).trim_end().to_string(), st.add_modifier(Modifier::BOLD))], right, w)
}

pub fn esc_hint(what: &str) -> Vec<Span<'static>> {
    vec![Span::styled("Esc", key_style()), Span::styled(format!(" {what}"), dim())]
}

/// 强调色底的按钮,比如「 ⏎ 新建 」
pub fn button(t: &str, bg: Color) -> Span<'static> {
    Span::styled(format!(" {t} "), Style::new().fg(theme::pal().on_accent).bg(bg).add_modifier(Modifier::BOLD))
}

/// 输入框:「> 内容▏」,太长只显示结尾,光标永远看得见;右边可以挂一段(比如「新目录」)
fn input_row(text: &str, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let room = w.saturating_sub(2 + 1 + if right_w > 0 { right_w + 2 } else { 0 });
    let mut shown = text.to_string();
    if shown.width() > room {
        let mut tail: Vec<char> = vec![];
        let mut n = 1; // 留给 …
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

/// 输入框下面那条线
fn input_rule(w: usize) -> Line<'static> {
    Line::styled("─".repeat(w), dim())
}

/// 按键说明一行(弹窗底下)
pub fn keys_line(items: &[(&str, &str)], right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let (keys, _) = key_spans(items, w.saturating_sub(if right_w > 0 { right_w + 2 } else { 0 }));
    spread(keys, right, w)
}

/// 画一个弹窗:算好高度(内容行数 + 外框),画框,再把行填进去
fn modal(f: &mut Frame, w: u16, border: Color, lines: impl Fn(usize) -> Vec<Line<'static>>) {
    let full = f.area();
    let (probe, float) = modal_area(full, w, full.height);
    let inner_w = probe.width.saturating_sub(if float { 6 } else { 4 }) as usize;
    let body = lines(inner_w);
    let h = body.len() as u16 + modal_chrome(full);
    let inner = modal_frame(f, w, h, border);
    f.render_widget(Paragraph::new(body), inner);
}

/// 大小固定的弹窗最多这么高(大屏上不撑满)
const FILL_H: u16 = 44;

/// 大小固定的弹窗(插件页、账号页):桌面上宽 w、高占满屏幕(上下各留两行,最多 FILL_H;页面说了只要 rows 行的照它,
/// 再加底下那句话的一行),手机上占满;里面正好 h 行
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

/// 工具菜单最多同时显示这么多行,再多就跟着高亮滚
const TOOL_ROWS: usize = 12;

/// 设置页里用编辑器开 config.toml,文件还没有时先写进去的
const CONFIG_TEMPLATE: Tr =
    Tr::new("# aca 的配置:账号、工具、界面的设置(写法见 README)\n", "# aca config: accounts, tools, interface settings (see the README)\n");

/// `n` 的工具菜单:每个工具一行「编号 名字 说明」,名字按哪家上色,高亮的整行铺淡紫
/// `n` / `H` 的工具菜单;pass 有 = 给交接挑的:标题写交接谁,它在跑、带便签的提一句
fn draw_tools(f: &mut Frame, menu: &[Tool], sel: usize, pass: Option<&Pass>) {
    modal(f, 72, theme::pal().accent, |w| {
        let name_w = menu.iter().map(|t| t.name.width()).max().unwrap_or(0).min(12);
        let start = (sel + 1).saturating_sub(TOOL_ROWS).min(menu.len().saturating_sub(TOOL_ROWS));
        let title = pass.map_or_else(|| t!("选择工具", "Pick a tool").to_string(), |p| tr!("交接「{}」给", "Hand off “{}” to", p.t.title));
        let mut out = vec![modal_title(&title, theme::accent(), esc_hint(t!("取消", "cancel")), w), Line::raw("")];
        if let Some(p) = pass {
            let mut marks = vec![];
            if p.busy {
                marks.push(Span::styled(t!("它还在跑", "it's still running"), theme::warn()));
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
            // 用不了的:名字不上色,说明那里写原因
            let (name_st, text, text_st) = match &t.problem {
                Some(p) => (dim(), p.clone(), theme::warn()),
                None => (agent_style(t.agent).add_modifier(Modifier::BOLD), t.note.clone(), dim()),
            };
            let text = clip(&text, w.saturating_sub(2 + name_w + 2)).trim_end().to_string();
            let mut line = Line::from(vec![
                Span::styled(num, dim()),
                Span::styled(clip(&t.name, name_w), name_st),
                Span::raw("  "),
                Span::styled(text, text_st),
            ]);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        // 交接时不加账号(按 + 会把交接丢在半路)
        let keys: &[(&str, &str)] = if pass.is_some() {
            &[("⏎", t!("交接", "hand off"))]
        } else {
            &[("⏎", t!("选定", "select")), ("+", t!("加账号", "add account"))]
        };
        out.push(keys_line(keys, vec![], w));
        out
    });
}

/// 删除 / 重启的确认:删除用红框(状态的红)。第一行问什么,接着是哪个会话、要注意的(别处连着、worktree 目录),最后 y 的按钮;别的键都是取消
fn draw_confirm(f: &mut Frame, c: &Confirm, all: &[Session]) {
    let danger = theme::pal().red;
    let red = Style::new().fg(danger);
    let warn = theme::warn();
    let (name, title, border, head, what, notes, act): (&str, &str, Color, &str, String, Vec<String>, &str) = match c {
        Confirm::Remove(r) if r.pane.is_some() => {
            let split = r.pane.as_ref().is_some_and(|p| p.1);
            let head = if r.next.is_some() {
                t!("关闭这一格？", "Close this pane?")
            } else if split {
                t!("关闭窗格？", "Close the pane?")
            } else {
                t!("关闭窗口？", "Close the window?")
            };
            let notes = r.next.iter().map(|n| tr!("后面的往前挪，{n}当头", "the rest move up, {n} becomes first")).collect();
            (r.name.as_str(), r.title.as_str(), danger, head, String::new(), notes, t!("关闭", "close"))
        }
        Confirm::Remove(r) => {
            let mut notes = vec![];
            if r.worktree {
                notes.push(t!("worktree 目录一起删", "the worktree directory is deleted too").to_string());
            }
            if r.others > 0 {
                notes.push(tr!("里面另开的 {} 个窗口 / 窗格一起关", "{} windows / panes opened inside close too", r.others));
            }
            if r.attached {
                notes.push(t!("⇄ 别处开着，会被断开", "⇄ open elsewhere; it will be disconnected").to_string());
            }
            if r.kids > 0 {
                notes.push(tr!("有 {} 个子会话，不会一起删", "{} child sessions won't be deleted with it", r.kids));
            }
            (r.name.as_str(), r.title.as_str(), danger, t!("删除会话？", "Delete the session?"), String::new(), notes, t!("删除", "delete"))
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
                vec![t!("⇄ 别处开着，会被断开", "⇄ open elsewhere; it will be disconnected").to_string()]
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
                notes.push(t!("只重启这一格，别的窗口不动", "restarts only this pane; other windows are untouched").into());
            } else {
                notes.push(t!("程序停掉，这一格留在原地，⏎ 再启动", "the program stops, the pane stays, ⏎ starts it again").into());
                notes.extend(next.iter().map(|n| tr!("{n}顶上来当头", "{n} moves up to be first")));
            }
            let (head, act) = if *restart {
                (t!("重启这一格？", "Restart this pane?"), t!("重启", "restart"))
            } else {
                (t!("停止这一格？", "Stop this pane?"), t!("停止", "stop"))
            };
            (row.as_str(), t.title.as_str(), theme::pal().accent, head, String::new(), notes, act)
        }
        Confirm::Send { t, text, .. } => {
            // 发什么:便签原文,太长只露前几行
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
    };
    let s = all.iter().find(|s| s.name == name);
    let head_style = if border == danger { red } else { theme::accent() };
    modal(f, 60, border, |w| {
        let (ic, sty) = s.map_or(("·", dim()), |s| icon(s.state));
        let label = s.map(|s| s.agent_label()).unwrap_or_default();
        let who = s.map_or(Style::new(), |s| agent_style(s.agent));
        let mut out = vec![
            modal_title(head, head_style, vec![], w),
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

/// `A`:认一遍这个 tmux 会话接管过来会是什么样(只看不动)
fn adopt_plan(target: &str) -> Result<aca_core::adopt::Plan, String> {
    let store = aca_core::store::Store::open()?;
    aca_core::adopt::plan(&store, &aca_core::config::load(), target, &aca_core::adopt::Opts::default())
}

/// 接管的确认框里:认出来的是什么、接过来之后会怎样。一行一件
fn adopt_summary(p: &aca_core::adopt::Plan) -> String {
    let mut v = vec![tr!(
        "登记成 {}{}",
        "register as {}{}",
        p.who(),
        if p.s.group.is_empty() { String::new() } else { tr!(" · 分组 {}", " · group {}", p.s.group) }
    )];
    v.push(match p.s.conv.as_str() {
        "" if p.s.tool == "shell" => t!("里面的程序不动", "the program inside is left alone").to_string(),
        "" => t!("没认出是哪段对话：重启会是新的一段", "couldn't tell which conversation: a restart starts a new one").to_string(),
        c => tr!(
            "对话 {}，进程不动，重启后接着来",
            "conversation {}; the process is left alone and it continues after a restart",
            c.chars().take(8).collect::<String>()
        ),
    });
    if p.title() != p.tmux {
        v.push(tr!("tmux 会话名改成「{}」", "tmux session renamed to “{}”", p.title()));
    }
    if !p.s.args.is_empty() {
        v.push(tr!("每次启动带 {}", "every start adds {}", p.s.args.join(" ")));
    }
    if p.deck.is_some() {
        v.push(
            t!("agent-deck 那边会显示成停了，别在那边再启动它", "agent-deck will show it as stopped; don't start it there again")
                .to_string(),
        );
    }
    v.join("\n")
}

/// 分叉时默认起的标题(和 `session::fork` 不给标题时起的一样)
fn fork_title(title: &str) -> String {
    tr!("{title} (分叉)", "{title} (fork)")
}

/// 改名 / 新分组 / 便签(预览关着时)的输入弹窗
fn draw_prompt(f: &mut Frame, p: &Prompt) {
    let (title, keys): (String, &[(&str, &str)]) = match &p.ask {
        Ask::Rename(_) => (t!("重命名", "Rename").into(), &[("⏎", t!("确定", "ok")), ("Esc", t!("取消", "cancel"))]),
        Ask::NewGroup => (t!("新建分组", "New group").into(), &[("⏎", t!("下一步", "next")), ("Esc", t!("取消", "cancel"))]),
        Ask::Note { title, .. } => (
            tr!("便签 · {title}", "Note · {title}"),
            &[("Esc", t!("写完", "done")), ("⏎", t!("换行", "newline")), ("^G", t!("编辑器", "editor"))],
        ),
        Ask::Fork(t) => (
            tr!("分叉 · {}", "Fork · {}", t.title),
            &[("⏎", t!("分叉", "fork")), ("^U", t!("清空重起", "clear and rename")), ("Esc", t!("取消", "cancel"))],
        ),
        Ask::Space(c) => (tr!("工作区 {c} 改名", "Rename workspace {c}"), &[("⏎", t!("确定", "ok")), ("Esc", t!("取消", "cancel"))]),
        Ask::GroupDir => (
            t!("新分组目录", "new group directory").into(),
            &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete")), ("Esc", t!("取消", "cancel"))],
        ),
        Ask::GroupName(g) => (tr!("改分组 · {g}", "Edit group · {g}"), &[("⏎", t!("下一步", "next")), ("Esc", t!("取消", "cancel"))]),
        Ask::GroupHome { to, .. } => (
            tr!("改分组 · {to} · 默认目录", "Edit group · {to} · default directory"),
            &[
                ("⏎", t!("确定", "ok")),
                ("Tab", t!("补全", "complete")),
                ("^U", t!("清空 = 不记", "empty = forget")),
                ("Esc", t!("取消", "cancel")),
            ],
        ),
    };
    modal(f, 64, theme::pal().accent, |w| {
        let mut out = vec![modal_title(&title, theme::accent(), vec![], w), Line::raw("")];
        if let Ask::Note { .. } = p.ask {
            // 多行:最多 8 行,留住最后几行(光标在那)
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

/// 新建的候选最多列这么多行
const DRAFT_ROWS: usize = 8;

/// `N` 的新建弹窗:用什么工具、落在哪个分组(跟着路径变)、目录输入框,下面是 Tab 补全的候选
fn draw_draft(f: &mut Frame, d: &Draft) {
    modal(f, 78, theme::pal().accent, |w| {
        // 名字那一栏:中文都是两个字(4 格);英文的补到一样宽
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
            _ => Style::new(),
        };
        // ^T 在沿用的工具和 shell 之间换(n 里选了 shell 的就不用换了)
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

/// `?` 的快捷键弹窗。桌面上居中、三栏一屏看完;手机上从底下升起、一栏往下翻,顶上露出几行列表
fn draw_help(f: &mut Frame, app: &mut App) {
    let full = f.area();
    let float = floating(full);
    let w = if float { full.width.saturating_sub(8).min(116) } else { full.width };
    let (probe, _) = modal_area(full, w, full.height);
    let inner_w = probe.width.saturating_sub(if float { 6 } else { 4 }) as usize;
    let hit = app.reader.as_ref().and_then(|r| r.hit.clone());
    let (lines, lit) = crate::help::lines(inner_w, hit.as_ref());
    let total = lines.len();
    // 标题、空行、(内容)、空行、底下一行
    let room = full.height.saturating_sub(if float { 2 } else { 3 } + modal_chrome(full) + 4) as usize;
    let body_h = total.min(room).max(1);
    let Some(r) = app.reader.as_mut() else { return };
    r.scroll = r.scroll.min(total - body_h);
    // 刚按了键:亮起来的第一行不在这一屏就滚过去(停在它上面一行);之后随便翻
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
    // 底栏左边:按了不是快捷键的键才说一声;是快捷键的,那几行自己会亮
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

/// 全屏阅读(`z`):顶上一条嵌着标题的强调色线(和手机上的预览一样),折好行的输出,底栏
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
            // 改动刷新了:顶上还是第几行就是第几行
            if same_width {
                r.scroll = r.wrapped.len().saturating_sub(old_top + h);
            }
        } else if same_width && r.scroll > 0 {
            // 往上翻着时底下长出新内容:把离底部的距离加上,画面停在原处(转屏换了宽度就不管了)
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
    // 看改动:未提交 / 连同提交,看到第几个文件
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
    let ic = s.map_or(("·", dim()), |s| icon(s.state));
    let where_ = if scroll == 0 { t!("最新", "latest").to_string() } else { tr!("↑{scroll} 行", "↑{scroll} lines") };
    let title = s.map_or(name.as_str(), |s| s.title.as_str());
    let plugin = r.plugin.is_some();
    let (title, right) = match &diff {
        Some((_, label)) => (tr!("改动 · {title}", "Diff · {title}"), label.clone()),
        // 插件的输出不是会话:没有「第几个会话」
        None if plugin => (title.to_string(), where_),
        None => (title.to_string(), format!("{}/{n} · {where_}", pos + 1)),
    };
    f.render_widget(Paragraph::new(title_rule(ic, &title, vec![Span::styled(right, dim())], w)), head);
    if empty {
        f.render_widget(
            Paragraph::new(t!(" 没有内容 · 会话可能已经结束", " nothing here · the session may have ended")).style(dim()),
            body,
        );
    } else {
        f.render_widget(Paragraph::new(rows.iter().map(ansi::to_line).collect::<Vec<_>>()), body);
        ansi::paint_links(f.buffer_mut(), body, &rows);
    }
    let foot_line = match &app.msg {
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ => match diff {
            Some((mode, _)) => menu_bar(
                &[
                    // 手机上先丢「滚动」(谁都会),留住 n p 和 m
                    vec![
                        ("↑↓", t!("滚动", "scroll"), 2),
                        ("n p", t!("换文件", "file"), 0),
                        (
                            "m",
                            if mode == Mode::Uncommitted {
                                t!("连同提交", "with commits")
                            } else {
                                t!("只看未提交", "uncommitted only")
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
                    vec![("r", t!("重跑", "rerun"), 0), ("c", t!("复制", "copy"), 1)],
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

/// 记录:这么宽才分两栏(左边的对话、右边的过程都要放得下一句话)
const TRACE_SPLIT: usize = 90;

/// 对话里一轮最多先露几行(你说的 / Claude 说的),多的 `o` 看全文
const ASK_LINES: usize = 6;
const REPLY_LINES: usize = 12;

/// Z 会话记录:整屏,顶上一行标题线(几轮 · 干了多久 · 多少钱);桌面左边对话、右边选中那一轮的过程,
/// 手机上整屏是其中一边;⏎ 整屏看一步;底下按键
fn draw_trace(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (w, h) = (area.width as usize, body.height as usize);
    let live = app.trace_live();
    let now = now_ms();
    let msg = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.clone());
    let Some(v) = app.trace.as_mut() else { return };
    // 换了主题(设置页预览、主题文件改了):排好的全重排
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
    let ic = s.map_or(("·", dim()), |s| icon(s.state));
    let mut title = s.map_or(v.name.as_str(), |s| s.title.as_str()).to_string();
    // 在子代理里:标题写它是谁,在不在跑按它自己的
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
        ("◆", dim())
    };
    v.wide = w >= TRACE_SPLIT;
    let foot_line = |groups: &[Vec<MenuItem>], action: Option<usize>| match &msg {
        Some(m) => Line::styled(m.clone(), theme::warn()).centered(),
        None => menu_bar(groups, action, w),
    };
    // 整屏看一步
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
            _ => ("◆", dim()),
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
    // 手机上在看过程:顶上写第几轮
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
        (None, None) => f.render_widget(Paragraph::new(t!(" 正在读对话记录…", " reading the conversation record…")).style(dim()), body),
        (Some(t), None) if t.roots.is_empty() => {
            f.render_widget(Paragraph::new(t!(" 还没有对话", " no conversation yet")).style(dim()), body)
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
            // 手机:先排对话(过程看的是对话里选中的那一轮),再画在看的那边
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
            vec![("↑↓", t!("移动", "move"), 3), ("→", t!("看过程", "steps"), 0), ("o", t!("全文", "full text"), 4)],
            vec![("n p", t!("换轮", "turn"), 2), ("J K", t!("换会话", "session"), 5), ("Tab", t!("输出", "output"), 6)],
            vec![("Esc", t!("返回", "back"), 0)],
        ],
        (true, true) => vec![
            vec![("↑↓", t!("移动", "move"), 3), ("o", t!("展开", "unfold"), 0), ("⏎", t!("整屏看", "full screen"), 1)],
            vec![("← Esc", t!("回对话", "conversation"), 2), ("n p", t!("换轮", "turn"), 4), ("J K", t!("换会话", "session"), 5)],
        ],
        (false, false) => {
            vec![
                vec![("⏎", t!("看过程", "steps"), 0), ("o", t!("全文", "full text"), 3)],
                vec![("n p", t!("换轮", "turn"), 1), (t!("空格 b", "Space b"), t!("翻页", "page"), 2)],
                vec![("Esc", t!("返回", "back"), 0)],
            ]
        }
        (false, true) => {
            vec![
                vec![("o", t!("展开", "unfold"), 0), ("⏎", t!("详情", "details"), 1)],
                vec![("n p", t!("换轮", "turn"), 2), (t!("空格 b", "Space b"), t!("翻页", "page"), 3)],
                vec![("Esc", t!("回对话", "conversation"), 0)],
            ]
        }
    };
    f.render_widget(Paragraph::new(foot_line(&keys, Some(0))), foot);
}

/// 折好的一行前面空几格
fn pad_line(l: &Styled, pad: &'static str) -> Line<'static> {
    let mut line = ansi::to_line(l);
    line.spans.insert(0, Span::raw(pad));
    line
}

/// 多了只留前 max 行,末尾写还有几行
fn clip_lines(mut ls: Vec<Styled>, max: usize, pad: &str) -> Vec<Styled> {
    if ls.len() > max {
        let more = ls.len() - max;
        ls.truncate(max);
        ls.push(vec![crate::trace::seg(
            tr!("{pad}… 另 {} 行", "{pad}… {} more lines", crate::trace::thousands(more as u64)),
            theme::faint(),
        )]);
    }
    ls
}

/// 排左边的对话:顶上一层一块 —— 一轮是你说的、Claude 最后说的、一行数字;压缩一行;抄过来的那一块一行(展开了接着它的几轮)
fn lay_chat(v: &mut TraceView, w: usize, h: usize, live: bool, now: i64) -> Vec<Vec<Line<'static>>> {
    let Some(t) = v.t.as_ref().map(|t| t.at(&v.path)) else { return vec![] };
    // 左边一格竖线、再空一格,右边留一格
    let cw = w.saturating_sub(3).max(1);
    let mut tops = vec![];
    for &r in &t.roots {
        tops.push(r);
        if t.nodes[r].kind == crate::trace::Kind::Copied && v.open.contains(&r) {
            tops.extend(t.nodes[r].kids.iter().copied());
        }
    }
    // 先不上色地排好、量出每块多高(排过的用缓存),定了从哪行看起,再给看得见的上色、转成屏幕行
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
        // 上色到屏幕底为止(露出更多了再上一回)
        let need = end.min(e) - s;
        if let Some(c) = v.chat_cache.get_mut(&i).filter(|c| c.colored < need) {
            let (lines, _) = crate::hl::upto(need, || chat_lines(t, i, cw, c.full, live, now));
            moved |= lines.len() != c.lines.len();
            c.lines = lines;
            c.colored = need;
        }
    }
    if moved {
        // 上了色行数不该变;真变了就按新的重新定位
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

/// 对话里的一轮排成行:你说的、Claude 最后说的(收着各露前几行)、一行数字;第二个值:收着的时候截没截
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

/// 排右边的过程:对话里选中的那一轮(换了轮就从头看;跟着最新的贴底)。同左边:先不上色地排,看得见的块再上色
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
    // 这一项不是一轮(抄过来的那块、压缩):整页详情当一块
    let build = |k: usize| {
        if turn {
            crate::trace::step(t, cur, k, live, now, &open)
        } else {
            Some(crate::trace::Step { node: cur, paras: crate::trace::detail(t, cur, live, now), fold: false })
        }
    };
    if !matches!(&v.steps_cache, Some(c) if c.item == cur && c.w == cw && c.ver == ver && c.folds == folds) {
        // 同一轮、同样宽:没变的块(见 trace::step_sig)照旧用,只排变了的
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
                    // 收着的思考按行数截:这个宽度下截了才有东西可展开(手机上截得多)
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

/// 画一列块:选中的那块左边一条竖线 —— 在按的这一列强调色、头一行淡紫底;
/// 不在按的对话画淡的竖线(右边是哪一轮的过程),不在按的过程不画
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
    // 底下还有:右下角说还剩几行
    let below = b.total().saturating_sub(top + h);
    if below > 0 && !b.follow {
        let tag = tr!(" ↓ {below} 行 ", " ↓ {below} lines ");
        let tw = tag.width() as u16;
        if area.width > tw + 2 {
            let r = Rect { x: area.x + area.width - tw, y: area.y + area.height - 1, width: tw, height: 1 };
            f.render_widget(Paragraph::new(Line::styled(tag, dim())).style(Style::new().bg(theme::pal().sel)), r);
        }
    }
}

fn draw_chat(f: &mut Frame, v: &mut TraceView, area: Rect, live: bool, now: i64) {
    let blocks = lay_chat(v, area.width as usize, area.height as usize, live, now);
    draw_blocks(f, area, &v.chat, blocks, !v.in_steps, true);
}

fn draw_steps(f: &mut Frame, v: &mut TraceView, area: Rect, live: bool, now: i64) {
    let blocks = lay_steps(v, area.width as usize, area.height as usize, live, now);
    draw_blocks(f, area, &v.steps, blocks, v.in_steps, false);
}

/// 起 `aca ui --cost-json` 读计费(同一个程序,环境原样带过去);only = 只看哪些账号
fn read_bill(only: &[String]) -> Result<crate::usage::Report, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = std::process::Command::new(exe)
        .args(["ui", "--cost-json"])
        .args(only.iter().flat_map(|o| ["--only", o.as_str()]))
        // 照这边现在的语言(设置页刚换过的话 ACA_LANG 可能还是另一种)
        .env("ACA_LANG", i18n::lang().code())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    // 换了新版没重开,子进程是新的、格式对不上:说一声,重开就好
    serde_json::from_slice(&out.stdout).map_err(|_| {
        t!("读出来的格式不对(aca 升级过的话 ^L 重开一下)", "unexpected format (if aca was upgraded, reopen with ^L)").to_string()
    })
}

/// $ 计费面板:整屏,顶上写是几点的数,中间是内容(见 `bill`),底下按键
fn draw_bill(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let (w, h) = (area.width as usize, body.height as usize);
    let Some(b) = app.bill.as_mut() else { return };
    let lines = app.bill_data.as_ref().map(|r| crate::bill::lines(r, b.sel, w)).unwrap_or_default();
    b.scroll = b.scroll.min(lines.len().saturating_sub(h));
    // 一屏放得下就不提滚动
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
    // 只看了哪些账号写在标题后面(按显示着的这份数写)
    let title = match app.bill_data.as_ref().map(|r| &r.only) {
        Some(o) if !o.is_empty() => tr!("计费 · {}", "Billing · {}", o.join(" + ")),
        _ => t!("计费", "Billing").into(),
    };
    f.render_widget(Paragraph::new(title_rule(("$", theme::accent()), &title, vec![Span::styled(right, dim())], w)), head);
    if lines.is_empty() {
        f.render_widget(Paragraph::new(t!(" 正在读对话记录…", " reading the conversation record…")).style(dim()), body);
    } else {
        f.render_widget(Paragraph::new(lines.into_iter().skip(scroll).take(h).collect::<Vec<_>>()), body);
    }
    let foot_line = match &app.msg {
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ => {
            let mut keys = vec![
                ("←→", t!("时段", "period"), 0),
                ("a", t!("账号", "accounts"), 1),
                ("↑↓", t!("滚动", "scroll"), 2),
                ("r", t!("重读", "reload"), 1),
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

/// a 的挑账号浮窗:第一行「全部」,往下一个账号一行(数字、勾、名字按哪家上色、全部时段花了多少);
/// 现在只看着的那几行数字用强调色,选上的打勾,高亮的整行铺底色;连选着的时候底下换成连选的键
fn draw_bill_pick(f: &mut Frame, app: &App, sel: usize) {
    let accts = app.bill_accounts();
    let pick = app.bill.as_ref().and_then(|b| b.pick.as_ref());
    let marked = pick.map(|p| p.chosen(&accts)).unwrap_or_default();
    let ranging = pick.is_some_and(|p| p.anchor.is_some());
    let total: f64 = accts.iter().map(|a| a.cost).sum();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![
            modal_title(
                t!("只看哪个账号", "Which accounts"),
                theme::accent(),
                esc_hint(if ranging { t!("不要这段", "drop this range") } else { t!("关闭", "close") }),
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
            let tick = if a.is_some_and(|a| marked.contains(&a.name)) { Span::styled("✓ ", theme::accent()) } else { Span::raw("  ") };
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
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        let keys: &[(&str, &str)] = if ranging {
            &[("↑↓", t!("连着选", "range")), ("⏎", t!("看选上的", "show selected")), ("v", t!("定下", "set"))]
        } else {
            &[
                (t!("⏎ 数字", "⏎ digit"), t!("只看它", "only it")),
                ("v", t!("多选", "multi")),
                (t!("空格", "Space"), t!("勾选", "tick")),
                ("0", t!("全部", "all")),
            ]
        };
        out.push(keys_line(keys, vec![], w));
        out
    });
}

/// 启动静默多久。by_hand = 自己按键打开的(开在 tmux 弹窗里、^L 重启):这时没有 Moshi 在连,静默只会吞掉紧跟着按的 ⏎
/// (弹窗画出来之前按的键,画完第一帧马上就读到,多短的静默都会吞)
fn startup_quiet(by_hand: bool) -> Duration {
    if by_hand { Duration::ZERO } else { STARTUP_QUIET }
}

/// 这批按键要不要丢(为什么丢见 run 循环)。quiet = 还在启动 / 刚退出会话的静默期里。
/// 只挪光标的键从来不丢:Moshi 代敲的命令和粘贴里没有方向键,而手机上滑动、按住方向键会一下接一下地来 ——
/// 落进静默期的那几下被丢,一起到的几下还会续上静默期,整串就全没了
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

/// 被当成自动输入丢掉的按键,追加到 ~/.cache/aca/ignored-input.log(只留最近 200 行,权限 600)。
/// 设了 ACA_TMUX_SOCKET 就写 ignored-input.<它>.log,测试不混进真日志
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
    // 误粘贴进列表界面的东西也会记在这,只给自己读
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
}

/// 终端支持 kitty 的扩展键盘协议(桌面的 kitty):开了才分得出 Shift+Enter 和回车。
/// 手机走 mosh,mosh 不转发这套协议,那边只能按 `E`
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

/// 设置页写错的:主题(config.toml 的 [theme]、themes/ 里的文件)和别的几项
fn config_bad() -> Vec<String> {
    [theme::problems(), crate::config::problems()].concat()
}

/// 焦点事件只在管输入法时要(桌面);让出终端前关掉,里面的程序不认得
fn focus_events(on: bool) {
    if crate::ime::active() {
        let _ = if on { execute!(stdout(), EnableFocusChange) } else { execute!(stdout(), DisableFocusChange) };
    }
}

/// 便签能不能往它的输入框里填(E 带进去、s 发出去):只填 Claude / Codex(别的程序比如 vim,粘进去的字会被当成命令);
/// 在等你选(!)时不填 —— 字会被当成选项
fn cant_fill(s: &Session) -> Option<&'static str> {
    match s.state {
        State::Ask => Some(t!("它在等你选", "it's waiting for your choice")),
        State::Error | State::Exited | State::Stopped | State::Crashed | State::Dead => Some(t!("它没在跑", "it isn't running")),
        _ if !matches!(s.agent, Agent::Claude | Agent::Codex) => Some(t!("不是 Claude / Codex", "not Claude / Codex")),
        _ => None,
    }
}

/// 看改动里的 l / e、列表里的 y:让出终端跑 lazygit / 编辑器 / yazi,回来返回出了什么错
fn run_external(x: &External) -> Option<String> {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let (name, st) = match x {
        External::Lazygit(dir) => ("lazygit".to_string(), std::process::Command::new("lazygit").arg("-p").arg(dir).status()),
        External::Edit { path, line } => {
            let ed = notes::editor();
            let name = notes::editor_name();
            // 各家「打开到第几行」的写法不一样:VS Code 系用 -g 文件:行,helix / zed / sublime 用 文件:行,其余 +行 文件
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
                    return Some(tr!("写不了 {}:{e}", "can't write {}: {e}", create::tilde(path)));
                }
            }
            let cmd = format!("{} {}", notes::editor(), q(path));
            (notes::editor_name(), std::process::Command::new("sh").arg("-c").arg(cmd).status())
        }
        External::View { path, line, temp } => {
            let ed = notes::editor();
            let name = notes::editor_name();
            // 只读:vim 系 -M(改不了也存不了)、不建交换文件、别处开着也不问;nano -v;别的编辑器没有只读的开法,照常打开
            let gui = matches!(name.as_str(), "code" | "code-insiders" | "cursor" | "windsurf" | "zed" | "subl");
            let cmd = match name.as_str() {
                "vi" | "vim" | "nvim" | "view" | "gvim" | "mvim" => format!("{ed} -M -n --cmd 'set shortmess+=A' +{line} {}", q(path)),
                "nano" => format!("{ed} -v +{line} {}", q(path)),
                "code" | "code-insiders" | "cursor" | "windsurf" => format!("{ed} -g {}", q(&format!("{path}:{line}"))),
                "hx" | "helix" | "zed" | "subl" => format!("{ed} {}", q(&format!("{path}:{line}"))),
                _ => format!("{ed} +{line} {}", q(path)),
            };
            let st = std::process::Command::new("sh").arg("-c").arg(cmd).status();
            // 开窗口的编辑器马上就回来了,文件留着(下次看的时候清掉一天前的)
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(tr!("没找到 {name}(brew install {name})", "{name} not found (brew install {name})"))
        }
        Err(e) => Some(tr!("{name} 起不来:{e}", "can't start {name}: {e}")),
    }
}

/// e 要看的字写成临时文件:放在只有自己能进的目录里,名字是「第 3 轮-Bash」这样(编辑器顶上看得到);顺手清掉一天前没删的
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

/// 等按键,最多 max。开着终端时它一有输出也醒来重画
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

/// yazi 开不了的原因:目录没了、没装
fn yazi_blocked(dir: &str) -> Option<String> {
    if !std::path::Path::new(dir).is_dir() {
        return Some(if dir.is_empty() {
            t!("不知道它在哪个目录", "no idea which directory it's in").into()
        } else {
            tr!("目录不在了:{dir}", "the directory is gone: {dir}")
        });
    }
    let found = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("yazi").is_file()));
    (!found).then(|| t!("没找到 yazi(brew install yazi)", "yazi not found (brew install yazi)").into())
}

/// 出错时把终端还回去(exitlog 的善后在主线程上调):和正常退出一样关掉键盘增强、焦点事件、鼠标,退出全屏和 raw 模式。
/// 不碰输入法:它的锁可能正被出错的那段占着(ime::Guard 退出时会切回去)
fn release_terminal() -> std::io::Result<()> {
    kbd(false);
    let _ = execute!(stdout(), DisableFocusChange, DisableMouseCapture);
    ratatui::try_restore()
}

/// 让出终端(进会话 / fzf / 编辑器)。扩展键盘协议先关掉,不然里面的程序收到的是它不认识的按键编码
fn suspend() -> std::io::Result<()> {
    kbd(false);
    focus_events(false);
    crate::ime::english(false);
    execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()
}

/// 先补一个「同步结束」:tmux 画一帧前后夹着同步开始 / 结束,detach 正赶上画到一半(会话在刷屏)时,
/// 没写出去的后半帧连同「结束」被它扔掉,kitty 停在「同步中」不刷新,aca 画好了也看不见,最多卡 2 秒
/// (实测刷屏的会话里 100 次 detach 有 7 次)。没停在同步中的终端收到它什么也不做。
/// 清屏用 resize 不用 clear:clear 为了把光标放回原处,先问终端光标在哪、等它答,ssh 连着的手机线路一卡
/// (2 秒没答)就报错,aca 跟着退出。resize 一样清屏、下一帧整屏重画,但不问
fn resume(t: &mut DefaultTerminal) -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EndSynchronizedUpdate, EnterAlternateScreen, EnableMouseCapture)?;
    kbd(true);
    focus_events(true);
    t.hide_cursor()?;
    let area = t.size()?.into();
    t.resize(area)
}

/// 是不是 Ctrl+\。终端发的是 0x1C,crossterm 读成 ^4;开了键盘增强的终端里是 ^\ 本身
fn closes_popup(k: KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('\\' | '4'))
}

/// 换成自己(同一个路径,也就是新装的那个)接着跑:exec,同一个进程、同一个终端。
/// 只带 --client(弹窗里要它)和 --restarted(开到哪);不带启动时的关键词 —— 带了又会直接进那个会话。
/// 视图(分组、范围、布局、高亮的会话)本来就记着,新的照样恢复
fn relaunch(state: &str) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return e.into(),
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

/// 这个 aca 程序什么时候装的(文件的修改时间):今天的写 11:28,别的天写 9/29 11:28
fn installed_at() -> Option<String> {
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64);
    let t = secs(std::fs::metadata(std::env::current_exe().ok()?).ok()?.modified().ok()?)?;
    let now = secs(std::time::SystemTime::now())?;
    let off = crate::usage::local_offset();
    let (day, hm) = ((t + off).div_euclid(86400), (t + off).rem_euclid(86400));
    let clock = format!("{:02}:{:02}", hm / 3600, hm % 3600 / 60);
    if day == (now + off).div_euclid(86400) {
        return Some(clock);
    }
    let (_, m, d) = crate::usage::civil_from_days(day);
    Some(format!("{m}/{d} {clock}"))
}

pub fn run(filter: String, restarted: Option<String>) -> anyhow::Result<()> {
    // 首屏等读完再画(~20ms),免得先闪一下空列表;在后台读,同时在这边切输入法(第一次要 ~40ms)
    let first = thread::spawn(collect);
    // 先切输入法:弹窗一出来就按的键不被拼音吃掉。和之后的切来切去一样在主线程
    crate::ime::start();
    let _ime = crate::ime::Guard;
    // 带着关键词打开时只看匹配的,不恢复上次停在哪个分组
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
    // 远端任务的 server 在设置页里填;P 是插件页(挂载、远端任务都是插件了,在 : 菜单里)
    app = app.with_plugins(|| Box::new(crate::plugins::Page::new(crate::plugins::load)));
    // 起 Claude 时带不带 aca 的 mod(额度不靠状态栏)
    app = app.with_setting(Box::new(crate::limits::ModRow::real()));
    app = app.with_setting(Box::new(crate::trust::TrustRow::real()));
    // 上次被杀掉 / 崩了的界面删了没来得及真删的
    thread::spawn(crate::trash::purge);
    if let (true, Some(p)) = (restore, &view_file) {
        app = app.restore(View::load(p));
    }
    if let Some((session, pane)) = crate::data::client_at() {
        app.highlight_here(&session, &pane);
        if crate::follow::asked() {
            // 关在别的会话上时没走 `switch_to`:Ctrl+Q 的绑定这里先补一次
            aca_core::tmux::bind_detach();
            app.follow_from(&session, &pane);
        }
    }
    // config.toml 写坏了:账号和工具都当成没写,设置页的「写错的」里说是哪
    app.sync_failed = aca_core::config::problem().into_iter().collect();
    if let Some(state) = &restarted {
        app.resume(state, installed_at());
    }
    // 上次的界面是随终端关掉 / 出错退出的:说一次(见 exitlog.rs)
    if let Some(m) = crate::exitlog::take_notice() {
        app.flash_for(&m, NOTICE);
    }
    let mut saved = app.view_state();
    let mut lang = i18n::lang();

    // 后台每 2 秒重读一次;attach 期间暂停
    let (tx, rx) = mpsc::channel::<Vec<Session>>();
    // 插件出错了没有:跟着列表一起重读(顶栏那一格 ⚠)
    let (alert_tx, alert_rx) = mpsc::channel::<Vec<aca_core::plugin::health::Problem>>();
    let (req_tx, req_rx) = mpsc::channel::<()>();
    let paused = Arc::new(AtomicBool::new(false));
    {
        let paused = paused.clone();
        thread::spawn(move || {
            // 到点了、有人催了、hook 刚报了事(事件流的文件变了)都重读一次;界面那头没了就停。
            // 事件来得再密,靠它触发的刷新也隔开 EVENT_GAP
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

    // 重启 / 改名 / 挪分组 / 删除在后台跑,做完连同重读的会话一起送回来(重读也要几十毫秒,不占界面)
    let (job_tx, job_rx) = mpsc::channel::<(Job, Outcome, Vec<Session>)>();
    // 滑动途中攒着的后台刷新
    let mut pending: Option<Vec<Session>> = None;
    // 看改动:后台跑完的 git diff
    let (diff_tx, diff_rx) = mpsc::channel::<(String, Mode, diff::Diff)>();
    let (plug_tx, plug_rx) = mpsc::channel::<(crate::caps::Req, bool, String)>();
    let mut diff_busy = false;
    let (glance_tx, glance_rx) = mpsc::channel::<(String, Glance)>();
    let mut glance_busy = false;
    // 会话记录:第一次读整份(最大的一百多 MB,零点几秒)在后台,之后的新增在刷新时就地读
    let (trace_tx, trace_rx) = mpsc::channel::<(String, Result<crate::trace::Trace, String>)>();
    // 计费面板:让子进程 aca ui --cost-json 去读(读的时候内存要一百来 MB,子进程退出就还了),这个线程等它交回结果
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
    // ratatui::init 会装自己的出错善后,先把原来的(std 默认的)拿出来,init 之后换成 exitlog 的
    let std_hook = std::panic::take_hook();
    let mut terminal = ratatui::init();
    crate::exitlog::install(std_hook, release_terminal);
    execute!(stdout(), EnableMouseCapture)?;
    focus_events(true);
    // 在 tmux 窗格里 / 直接开在 kitty 里时,切到别的窗格、别的 App 就把输入法还回去(弹窗收不到焦点事件)
    let mut focused = true;
    // 只在看得出是支持的终端里才问:问的时候要等终端回话,手机那边隔着 mosh,它不回的话每次打开都白等 2 秒
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let likely = !env("KITTY_WINDOW_ID").is_empty()
        || ["ghostty", "WezTerm"].contains(&env("TERM_PROGRAM").as_str())
        || ["kitty", "ghostty", "foot"].iter().any(|t| env("TERM").contains(t));
    let ok = likely && matches!(ratatui::crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    KBD.store(ok, Ordering::Relaxed);
    kbd(true);
    let res = (|| -> anyhow::Result<()> {
        loop {
            // 终端没了(窗口被关、ssh 断了):不等读写出错,收尾退出(见 exitlog.rs)
            if crate::exitlog::gone() {
                return Ok(());
            }
            // 后台线程出错了:界面还在,底栏说一声
            if let Some(m) = crate::exitlog::take_live() {
                app.flash_for(&m, NOTICE);
            }
            let mut refreshed = false;
            // 刚挪过就先攒着,停手再换上:滑动途中插进一次整屏重画,手机上就是一顿
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
            // 全屏阅读:刚打开 / 换了会话 / 每次定时刷新,重抓历史(3000 行也只要几毫秒)
            if let Some(target) = app.reader_target().filter(|_| std::mem::take(&mut app.reader_load) || refreshed) {
                app.set_reader_lines(crate::data::capture_history(&target));
            }
            // 插件的能力 / 视图 / 窗格:一个一个线程,做完送回来
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
            // 看改动:git 在后台跑(大仓库要一会儿),同时只跑一个
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
            // 预览顶上的改动统计:高亮的那个,同样在后台读
            while let Ok((name, g)) = glance_rx.try_recv() {
                glance_busy = false;
                app.set_glance(name, g);
            }
            if !glance_busy && let Some((name, w)) = app.glance_request(refreshed) {
                glance_busy = true;
                let tx = glance_tx.clone();
                // 分组的预览还要列目录里的东西
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
            // config.toml 里的颜色改了就换上;输入法开关也跟着(设置页里改的当场生效)
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
            // 正滑动时按预览该换的时刻醒来,停下来就按时换上
            let mut wait =
                app.settle_left().map_or(Duration::from_millis(150), |d| d.min(Duration::from_millis(150)) + Duration::from_millis(5));
            // 计费在读:几十毫秒就回来,别等满 150 毫秒才换上
            if app.bill_loading {
                wait = wait.min(Duration::from_millis(15));
            }
            if wait_input(wait, app.term_on.is_some())? {
                // 把已经到达的事件一次读完。3 个以上按键同时到 = 不是人手敲的:Moshi 连上后会往 shell
                // 里代敲命令(比如 cd 到最近目录),而这时接收输入的是 aca —— 其中的 "/" 会进过滤模式、
                // 路径变成过滤词、回车再把它确认,列表被筛空,看着就像卡死的 fzf。粘贴同理。整段丢掉。
                let mut batch = vec![event::read()?];
                while batch.len() < 4096 && event::poll(Duration::ZERO)? {
                    batch.push(event::read()?);
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
                    // 只记进日志,不在底栏提示
                    log_ignored(&keys);
                    // 只丢了一个键 = 静默期里人按的,不续:续了手快的会一下接一下全被吞
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
            // 视图一变就存(断线时 aca 是被直接杀掉的,等不到退出)
            let now_view = app.view_state();
            if now_view != saved && !app.just_moved() {
                if let Some(p) = &view_file {
                    let _ = now_view.save(p);
                }
                saved = now_view;
            }
            // 跟随:高亮停稳了就把这台客户端带过去;Esc 是带回原处再退
            if let Some(to) = app.follow_step() {
                crate::data::show(&to.session, &to.pane);
            }
            if app.quit {
                app.follow_done();
                return Ok(());
            }
            // fzf 放在新建前面:在 fzf 里回车就开,紧接着就新建
            if std::mem::take(&mut app.pick) {
                let size = terminal.size()?;
                if let Some(p) = app.dir_pick(size.width, size.height) {
                    paused.store(true, Ordering::Relaxed);
                    suspend()?;
                    let got = create::pick_dir(&p);
                    resume(&mut terminal)?;
                    paused.store(false, Ordering::Relaxed);
                    app.picked(got);
                }
            }
            if let Some(req) = app.create.take() {
                app.flash(t!("正在新建…", "creating…"));
                terminal.draw(|f| draw(f, &mut app))?;
                match create::create(&req) {
                    Ok(id) => {
                        app.adopt(&id);
                        app.attach = Some(id); // 接着走下面的 attach
                    }
                    Err(e) => app.flash(&tr!("新建失败:{e}", "creating failed: {e}")),
                }
                // 等待期间攒下的按键不是冲着新会话按的
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
                resume(&mut terminal)?;
                paused.store(false, Ordering::Relaxed);
                app.note_editor_done(ed, got);
            }
            // tmux 弹窗里的 yazi:它认出 tmux 后会穿透出去问外面的终端,弹窗不转发,退出时每次干等 5 秒(图也出不来)
            // → 改在会话里分一格放大的跑它,q 退出这格就关了,人留在会话里(弹窗本来就是切过去用的)
            if let Some(External::Yazi(sp)) =
                app.external.take_if(|x| matches!(x, External::Yazi(sp) if !sp.target.is_empty()) && crate::data::in_popup())
            {
                app.split = Some(sp);
            }
            if let Some(x) = app.external.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let err = run_external(&x);
                resume(&mut terminal)?;
                paused.store(false, Ordering::Relaxed);
                app.external_done(err);
                let _ = req_tx.send(());
            }
            if let Some(p) = app.pick_group.take() {
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let got = ops::pick_group(&p.cands, &p.t.title);
                resume(&mut terminal)?;
                paused.store(false, Ordering::Relaxed);
                app.group_picked(p, got);
            }
            if let Some(r) = app.term_req.take() {
                let dir = crate::workdir::find(&r.w).dir;
                if std::path::Path::new(&dir).is_dir() {
                    // 分组的终端不属于哪个会话,不带账号
                    let env = if !r.env.is_empty() || r.family.is_empty() {
                        r.env.clone()
                    } else {
                        let head = app.all.iter().find(|s| s.sub.is_none() && s.name == r.family);
                        crate::data::account_env(head, r.w.pid, r.w.agent)
                    };
                    // 先按半屏开,画第一帧时跟着终端那块改成准确的大小
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
                        Err(e) => app.flash(&tr!("终端开不了:{e}", "can't open the terminal: {e}")),
                    }
                } else {
                    app.flash(&if dir.is_empty() {
                        t!("不知道它在哪个目录", "no idea which directory it's in").to_string()
                    } else {
                        tr!("目录不在了:{dir}", "the directory is gone: {dir}")
                    });
                }
            }
            // 里面 exit 了的去掉;正看着的那个没了就回列表
            app.terms.retain(|_, t| !t.exited());
            app.catch_links();
            if app.term_on.as_ref().is_some_and(|k| !app.terms.contains_key(k)) {
                app.term_on = None;
                app.flash(t!("终端关了", "terminal closed"));
            }
            if let Some(sp) = app.split.take() {
                let dir = crate::workdir::find(&sp.w).dir;
                let session = app.tmux_of(&sp.name);
                // 会话行不知道窗格 id:分它当前那格
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
                            t!("分不了屏:会话可能刚结束", "can't split: the session may have just ended")
                        } else {
                            t!("开不了窗口:会话可能刚结束", "can't open a window: the session may have just ended")
                        }),
                    },
                }
            }
            if let Some(name) = app.attach.take() {
                // name 是会话的键;tmux 里按会话号找(会话名就是标题,会变)
                let target = app.tmux_of(&name);
                // 先切到那一格:会话本身落在主窗格(agent 在的那格),子行落在那个窗口 / 窗格
                let focus = app.focus.take();
                if let Some(p) = &focus {
                    crate::data::focus_pane(p);
                }
                // E:先把便签粘进它的输入框(不回车),进去就看到
                if let Some((key, text)) = app.fill.take()
                    && crate::data::paste_into(&focus.unwrap_or_else(|| format!("{target}:")), &text).is_ok()
                {
                    app.note_filled(&key);
                }
                if std::env::var_os("TMUX").is_some() {
                    // 已经在 tmux 里(桌面上开在 tmux 里、手机上的弹窗):直接切过去,不嵌套。切走界面就退了,进去就算看过
                    app.mark_seen(&name);
                    suspend()?;
                    crate::data::switch_to(&target)?;
                    return Ok(());
                }
                paused.store(true, Ordering::Relaxed);
                suspend()?;
                let ok = crate::data::attach(&target);
                resume(&mut terminal)?;
                app.mark_seen(&name);
                quiet_until = Instant::now() + BACK_QUIET;
                paused.store(false, Ordering::Relaxed);
                // 回来时高亮停在刚进去的那个(新建的、交接出来的还没被选中过);本来就停在它或它的窗口上的不动
                if !name.starts_with('$') && app.current().is_none_or(|s| s.family() != name) {
                    app.selected = Some(name.clone());
                }
                app.set_sessions(collect());
                if !ok {
                    app.flash(t!("进不去:会话可能刚结束", "can't go in: the session may have just ended"));
                }
            }
        }
    })();
    kbd(false);
    focus_events(false);
    crate::ime::english(false);
    let _ = execute!(stdout(), DisableMouseCapture);
    // 终端没了(窗口被关、ssh 断了)就还不回去,也没处说:记进 exit.log,不打印、错误不往外交。
    // 以前这里用 ratatui::restore(),还不回去就往没了的终端打印,出错再出错,程序直接中止(见 exitlog.rs)
    let restored = ratatui::try_restore();
    let gone = restored.is_err() || crate::exitlog::gone();
    if gone {
        crate::exitlog::term_gone(res.as_ref().err(), restored.as_ref().err());
    }
    // 这次删的撤不回了(^L 重启也一样,撤销记录在内存里):真删(另起进程去删,不耽误退出;终端没了也照删)
    if app.trash.release(None).unwrap_or(0) > 0 {
        crate::trash::purge_detached();
    }
    if gone {
        return Ok(());
    }
    if app.relaunch && res.is_ok() {
        // 刚挪过的高亮可能还没记(停手才记),换之前补记一次
        if let Some(p) = &view_file {
            let _ = app.view_state().save(p);
        }
        return Err(relaunch(&app.relaunch_state()));
    }
    res
}

/// 调试用:按给定尺寸渲染一帧到内存,按顺序喂 keys 里的按键,打印成文本(中文按两格处理)。
/// 和界面一样每个键画一帧:有些状态(记录里哪些步能展开)是画的时候才算出来的
pub fn snapshot(w: u16, h: u16, keys: &str) -> anyhow::Result<String> {
    // 显示真的置顶,但按 p 只改内存,不写文件
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
        // 记录界面里是后台读的;这里当场读完,后面的按键(n / o / →)才落在记录上
        if let Some(s) = app.trace_request() {
            app.set_trace(&s.name, crate::trace::load(&s));
        }
        t.draw(|f| draw(f, &mut app))?;
    }
    // 交出去的这帧画在干净的屏上:内存屏只按差异更新,连着画几帧会留下前一帧的半个宽字
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
        out.push_str(&format!("[挑分组 → {}]\n", p.cands.join(" ")));
    }
    // 只打印,不真的新建
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
    fn swipes_and_arrow_runs_are_never_taken_for_auto_input() {
        let k = |c| KeyEvent::new(c, KeyModifiers::NONE);
        assert!(!drops(&[k(KeyCode::Down)], true, false)); // 刚退出会话就滑
        assert!(!drops(&[k(KeyCode::Up); 5], false, false)); // 网络一卡,一串一起到
        assert!(drops(&[k(KeyCode::Enter)], true, false)); // 静默期里单独到的回车照丢
        let typed: Vec<_> = "cd ~/x".chars().map(|c| k(KeyCode::Char(c))).collect();
        assert!(drops(&typed, false, false));
        assert!(!drops(&typed, false, true)); // 在输入框里打字不算
        assert!(drops(&[k(KeyCode::Char('j')), k(KeyCode::Down), k(KeyCode::Down)], false, false));
    }

    #[test]
    fn popup_starts_without_the_quiet_period() {
        assert_eq!(startup_quiet(false), STARTUP_QUIET); // 直接开(Moshi 连上那条路)照旧
        assert_eq!(startup_quiet(true), Duration::ZERO); // 弹窗:C-\ 紧跟着 ⏎ 不能吞
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
            // 分区顺序:fun 在 work 前面
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

    /// 组内顺序就是这里的先后
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

    /// 按列表顺序,不排序
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
        assert_eq!(app.msg.as_ref().unwrap().0, "还在忙:正在重启「闲」…");
        app.busy = None;
        ctrl(&mut app, 'l');
        assert!(app.relaunch && app.quit);
        assert_eq!(app.relaunch_state(), "list");
        // 计费面板里也行(不是换时段),记着开在哪个时段
        let mut app = App::new(String::new(), sessions());
        press(&mut app, KeyCode::Char('$'));
        press(&mut app, KeyCode::Right);
        ctrl(&mut app, 'l');
        assert!(app.relaunch && app.quit);
        assert_eq!(app.relaunch_state(), "bill:2");
        // 新的那个:接着开计费面板,说一声是几点装的
        let mut app = App::new(String::new(), sessions());
        app.resume("bill:2", Some("11:28".into()));
        assert_eq!(app.bill.as_ref().map(|b| b.sel), Some(2));
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启 · 这个 aca 是 11:28 装的");
        let mut app = App::new(String::new(), sessions());
        app.resume("list", None);
        assert!(app.bill.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已重启");
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
        // a 打开,高亮在「全部」;↓ ⏎ = 只看第一个,回到顶上,马上按它重读
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.bill.as_ref().unwrap().pick.as_ref().map(|p| p.sel), Some(0));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.bill.as_ref().unwrap().pick.is_none());
        assert_eq!(app.bill.as_ref().unwrap().scroll, 0);
        assert_eq!(app.bill_request(), Some(vec!["Codex big".to_string()]));
        // 还没读回来:顶上照旧是显示着的那份
        assert_eq!(app.bill_request(), None);
        app.set_bill(Ok(Report { only: vec!["Codex big".into()], accounts: accounts.clone(), ..Default::default() }));
        // 再打开高亮停在只看着的那个;空格勾上两个(勾完往下走),⏎ 看勾上的,按花费排
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
        // 浮窗里 1-4 不是换时段,是直接挑账号;0 = 全部;Esc 不改
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('3'));
        assert_eq!((app.bill_only.clone(), app.bill.as_ref().unwrap().sel), (vec!["Claude small".to_string()], 1));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        assert!(app.bill.is_some() && app.bill_only == vec!["Claude small".to_string()]);
        // 重启带着只看哪些,新的那个照样只看它们
        assert_eq!(app.relaunch_state(), "bill:1\tClaude small");
        let mut again = App::new(String::new(), sessions());
        again.resume(&app.relaunch_state(), None);
        assert_eq!(again.bill_only, vec!["Claude small".to_string()]);
        assert_eq!(again.bill_request(), Some(vec!["Claude small".to_string()]));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('0'));
        assert!(app.bill_only.is_empty());
        // 关了再开还是只看的那些(这里是全部)
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
        // 停在「全部」上按 v:从第一个账号开始;往上拉不绕回「全部」
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Up);
        assert_eq!(pick(&app), Some((1, Some(1), names(&["A"]))));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(pick(&app), Some((3, Some(1), names(&["A", "B", "C"]))));
        // Esc 只丢这一段,浮窗还开着
        press(&mut app, KeyCode::Esc);
        assert_eq!(pick(&app), Some((3, None, vec![])));
        // v 拉一段、v 定下,再挪去别处拉一段,⏎ 看两段合起来的
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(pick(&app), Some((2, None, names(&["B"]))));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(pick(&app).unwrap().2, names(&["B", "D"]));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.bill_only, names(&["B", "D"]));
        assert!(app.bill.as_ref().unwrap().pick.is_none());
    }

    fn typ(app: &mut App, text: &str) {
        text.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    #[test]
    fn new_defaults_to_the_selected_sessions_dir_and_group() {
        let mut app = fixture();
        pin(&mut app, "跑"); // 选中「跑」(work 分组)
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.as_ref().unwrap().text, "/p/work");
        assert!(app.typing());
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.spec().as_str()), ("/p/work", "work", "claude"));
        assert!(r.create_dir); // /p/work 不存在
        assert!(app.attach.is_none()); // 回车被输入框吃掉了,没进原来的会话
    }

    #[test]
    fn new_inherits_tool_and_account_from_the_selected_session() {
        let mut v = sessions();
        v[2].tool = "codex".into(); // 跑
        v[2].account = "team".into();
        let mut app = App::new(String::new(), v);
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.tool.as_str(), r.account.as_str()), ("codex", "team"));
        pin(&mut app, "闲"); // 默认账号的 claude
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
            back: Scope::Working,
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
        // 分组没了 → 全部分组;会话没了 → 高亮第一个
        let gone = View { group: Some("旧分组".into()), selected: Some("没了".into()), ..v };
        let app = fixture().restore(gone);
        assert_eq!(app.group, None);
        assert_eq!(app.current().unwrap().title, "问");
    }

    #[test]
    fn down_or_ctrl_f_in_new_asks_for_fzf_and_fills_the_path() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('j')); // 选中「跑」
        assert_eq!(app.dir_hints(), ["/p/work", "/p/fun"]); // 选中的在前,同目录的不重复
        press(&mut app, KeyCode::Down); // 列表里 ↓ 是往下走,不开 fzf
        assert!(!app.pick);
        press(&mut app, KeyCode::Char('N'));
        press(&mut app, KeyCode::Down);
        assert!(std::mem::take(&mut app.pick));
        ctrl(&mut app, 'f');
        assert!(std::mem::take(&mut app.pick));
        app.picked(Ok(None)); // Esc:原样
        assert_eq!(app.new_draft.as_ref().unwrap().text, "/p/work");
        app.picked(Err("没装 fzf".into()));
        assert_eq!(app.new_draft.as_ref().unwrap().hint, "没装 fzf");
        app.picked(Ok(Some(Picked::Edit("~/code/app".into())))); // →:填回去接着打
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
        press(&mut app, KeyCode::Char('j')); // 选中「跑」(/p/work)
        press(&mut app, KeyCode::Char('N'));
        let cands = [("/p/work", Src::Known), ("/p/gone-quiet", Src::Known), ("/z/often", Src::Zoxide), ("/p/near", Src::Near)];
        let p = app.dir_pick_from(cands.map(|(d, s)| (d.to_string(), s)).to_vec(), 40, 16).unwrap();
        let marks: String = p.rows.iter().map(|r| r.mark).collect();
        assert_eq!(marks, "●◆↗ "); // 有会话的 ●,登记过但现在没会话的 ◆
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
        // 手机宽度:提示和工具名挤一行也不出界
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
        // 最多列 8 个,跟着换到的那个滚;换到的那个整行铺淡紫
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
        pin(&mut app, "跑"); // work 分组
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("work".into(), false));
        app.picked(Ok(Some(Picked::Edit("/p/fun".into())))); // 目录名就是 fun 组
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("fun".into(), false));
        typ(&mut app, "lab"); // /p/fun/lab:上一级是 fun 组 → 嵌套
        assert_eq!(app.new_draft.as_ref().unwrap().group(), ("fun/lab".into(), true));
        app.picked(Ok(Some(Picked::Edit("/p/brand".into()))));
        let rows = reader_rows(&mut app, 60, 20);
        assert!(rows.iter().any(|r| r.contains("brand  新分组")), "{rows:#?}");
        press(&mut app, KeyCode::Enter);
        let req = app.create.take().unwrap();
        // 按路径推出来的:不用 ^N 那种绑,交给核心按目录认领(记上默认目录)
        assert_eq!((req.group.as_str(), req.bind, req.by_dir), ("brand", false, true));
    }

    #[test]
    fn d_asks_first_and_only_y_deletes() {
        let mut app = fixture();
        pin(&mut app, "跑");
        pin(&mut app, "跑"); // 选中「跑」
        for cancel in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('q')] {
            press(&mut app, KeyCode::Char('d'));
            let rows = reader_rows(&mut app, 46, 12);
            // 手机上从底下升起的一块:问什么、哪个会话、会怎样,最后 y 的按钮
            let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("缺「{t}」:{rows:#?}"));
            let (head, y) = (at("删除会话？"), at("y 删除"));
            assert!(head < y && rows[head..y].iter().any(|r| r.contains("跑")), "{rows:#?}");
            press(&mut app, cancel);
            assert!(app.job.is_none() && !app.quit, "{cancel:?}");
            assert_eq!(app.msg.as_ref().unwrap().0, "没删");
        }
        press(&mut app, KeyCode::Char('d'));
        let Some(Confirm::Remove(r)) = &app.confirm else { panic!("没问") };
        let r = r.clone();
        press(&mut app, KeyCode::Char('y'));
        // 放进后台跑(删会话要等进程退干净),界面不等它:先从列表里拿掉,高亮落到下一个
        assert_eq!(app.job, Some(Job::Remove { t: target("跑"), pane: None }));
        let has = |app: &App, t: &str| order(app).contains(&t.to_string());
        assert!(!has(&app, "跑") && app.current().is_some_and(|s| s.title != "跑"));
        let job = app.take_job().unwrap();
        assert!(app.busy.is_none());
        assert_eq!(app.doing().as_deref(), Some("正在删除「跑」…"));
        press(&mut app, KeyCode::Char('y')); // 确认只管一次
        assert!(app.job.is_none());
        // 删完之前的刷新读到的还有它:照样藏着
        app.set_sessions(sessions());
        assert!(!has(&app, "跑"));
        // 还在删也能接着删别的;别的操作要等删完
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none() && app.confirm.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "还在忙:正在删除「跑」…");
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
        // 删掉之后晚到的旧刷新也不让它冒出来;删失败的放回来
        app.job_done(&job2, Outcome::Failed("删除失败:不让删".into()), sessions());
        assert!(!has(&app, "跑") && has(&app, "闲") && app.doing().is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "删除失败:不让删");
        // 藏够了就不管了(真还在就是真还在)
        app.vanish[0].until = Some(Instant::now());
        app.set_sessions(sessions());
        assert!(has(&app, "跑") && app.vanish.is_empty());
        // worktree / 别处连着要说;窄了先截标题,按键说明留着
        let r = Removal {
            title: "很长很长很长很长很长很长很长很长很长很长的标题".into(), worktree: true, attached: true, ..r
        };
        app.confirm = Some(Confirm::Remove(r));
        let rows = reader_rows(&mut app, 46, 14);
        for want in ["很长很长", "worktree 目录一起删", "⇄ 别处开着，会被断开", "y 删除"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        app.confirm = None;
        // 折叠行上 d 不是删会话:开的是分组那一页
        press(&mut app, KeyCode::Char('O'));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm.is_none() && !app.overlays.is_empty());
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
        // 后台做完前不接新的
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "还在忙:正在重启「闲」…");
        app.job_done(&job, Outcome::Done("已重启「闲」".into()), sessions());
        assert!(app.busy.is_none() && app.confirm.is_none());
        // 在跑的先问,别的键取消
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none());
        shows(&reader_rows(&mut app, 46, 12), &["重启会话？", "跑", "正在跑,会打断它", "y 重启"]);
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none() && app.attach.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "没重启");
        press(&mut app, KeyCode::Char('R'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.job, Some(Job::Restart { t: target("跑"), force: false }));
    }

    #[test]
    fn f_forks_the_session_and_highlights_the_new_one() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        // 先起名:填好默认的,直接回车就用它(不锁)
        assert!(app.job.is_none() && app.typing());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job, Some(Job::Fork { t: target("闲"), title: None }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在分叉「闲」…"));
        // 分出来的排在组里最后,高亮跟过去
        let mut fresh = sessions();
        let mut new = mk("闲-fork", "work", State::Running, 0);
        new.slot.1 = 99;
        new.deck_id = "id-新".into();
        fresh.push(new);
        app.job_done(&job, Outcome::Made { msg: "已分出「闲-fork」".into(), id: "id-新".into() }, fresh);
        assert_eq!(app.current().unwrap().title, "闲-fork");
        assert_eq!(app.msg.as_ref().unwrap().0, "已分出「闲-fork」");
        // 失败了高亮不动
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
        // F(原来的起名再分叉)和 f 一样
        press(&mut app, KeyCode::Char('F'));
        shows(&reader_rows(&mut app, 46, 12), &["分叉 · 闲", "闲 (分叉)", "⏎ 分叉", "^U 清空重起"]);
        assert!(app.typing());
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none());
        shows(&reader_rows(&mut app, 46, 12), &["标题是空的"]);
        typ(&mut app, "试试 看");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(Job::Fork { t: target("闲"), title: Some("试试 看".into()) }));
        press(&mut app, KeyCode::Char('F'));
        press(&mut app, KeyCode::Esc);
        assert!(app.job.is_none() && app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "没分叉");
    }

    #[test]
    fn fork_and_mcp_are_only_for_registered_claude_or_codex_sessions() {
        let mut v = sessions();
        v[3].agent = Agent::None; // 闲:shell 会话
        v[2].deck_id.clear(); // 跑:普通 tmux 会话
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "只能分叉 Claude / Codex 会话");
        press(&mut app, KeyCode::Char('M'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "只有 Claude / Codex 会话有 MCP");
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('F'));
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话不能分叉(没在 aca 里登记) · A 接管");
        press(&mut app, KeyCode::Char('M'));
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话不能看 MCP(没在 aca 里登记) · A 接管");
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

    /// 菜单里的一项:名字就是账号名,说明写它的配置目录
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
        // 菜单同 n,但没有 shell;标题写交接谁,它在跑、带着便签都提一句
        let rows = reader_rows(&mut app, 60, 14);
        shows(&rows, &["交接「跑」给", "它还在跑 · 带上便签", "1 cw  ~/.cw", "2 cx  ~/.cx", "⏎ 交接"]);
        assert!(!rows.iter().any(|r| r.contains("shell")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        assert!(app.tool_menu.is_none() && app.handoff.is_none() && app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已取消交接");
        // 再按 n:还是原来的新建,不带交接
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 60, 14), &["选择工具", "⏎ 选定"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.msg.as_ref().unwrap().0, "已取消新建");

        press(&mut app, KeyCode::Char('H'));
        press(&mut app, KeyCode::Char('2'));
        assert!(app.tool_menu.is_none() && app.new_draft.is_none() && app.create.is_none());
        let Some(Job::Handoff { t, from, req, to, note }) = app.job.clone() else { panic!("{:?}", app.job) };
        assert_eq!(t, target("跑"));
        assert_eq!((from.sid.as_str(), from.note.as_deref()), ("sid-跑", Some("顺便补测试")));
        // 新会话开在它的目录、分组里,工具 / 账号用菜单选的
        assert_eq!((req.dir.as_str(), req.group.as_str(), req.spec().as_str()), ("/p/work", "work", "codex:cx"));
        assert_eq!((to, note.as_deref()), (Agent::Codex, Some("id-跑")));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在交接「跑」…"));

        // 建好了:高亮新会话,便签跟过去了就删
        let mut fresh = sessions();
        let mut new = mk("接手", "work", State::Idle, 0);
        new.slot.1 = 99;
        fresh.push(new);
        app.job_done(&job, Outcome::Made { msg: "已交接".into(), id: "id-接手".into() }, fresh.clone());
        assert_eq!(app.current().unwrap().title, "接手");
        assert_eq!(app.msg.as_ref().unwrap().0, "已交接");
        assert_eq!(app.notes.get("id-跑"), None);

        // 没发出去:说明留在新会话的便签里。H 只管交接:挑同一家的别的账号也是新开一个(原地换账号是 X)
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

    /// cw2 没登录,别的都登录着
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

    /// 都在 ding 上
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
        // 顶上是现在的账号和额度;下面只有同一家的别的账号(没有它自己、没有 Codex、没有 shell),登录着没有、额度
        let rows = reader_rows(&mut app, 80, 24);
        shows(
            &rows,
            &[
                "「闲」换个账号接着跑",
                "现在  Claude·ding   5h 98% · 7d 61%",
                "换到哪个账号 —— 对话整段带过去,在原地重启:",
                "› 1 cw    ● 登录着      5h 12% · 7d 40%",
                "  2 cw2   ○ 没登录      额度没记到",
                "新账号的缓存是空的",
                "⏎ 换过去",
            ],
        );
        assert!(!rows.iter().any(|r| r.contains(" cx ") || r.contains("会打断")), "{rows:#?}");
        // 没登录的不让换
        press(&mut app, KeyCode::Char('2'));
        assert!(app.job.is_none() && !app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "cw2 没登录 · a 去账号页登录,登好再按 X");
        // ⏎ 换到高亮的那个,框收起
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert!(app.overlays.is_empty());
        assert_eq!(app.job, Some(Job::Rehome { t: target("闲"), to: "claude:cw".into(), label: "cw".into() }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在把「闲」换到 cw…"));
        app.job_done(&job, Outcome::Done("已换到 cw,对话接着来".into()), on_ding());
        assert_eq!((app.current().unwrap().title.as_str(), app.msg.as_ref().unwrap().0.as_str()), ("闲", "已换到 cw,对话接着来"));

        // 在跑的:框里说会打断它
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('X'));
        shows(&reader_rows(&mut app, 80, 24), &["⚠ 它正在跑 / 在等你选:换账号会打断这一轮"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.job.is_none());
    }

    #[test]
    fn x_says_why_it_cannot_switch() {
        // 没有别的账号:说怎么加
        let mut app =
            fixture().with_tools(|| Ok(vec![tool("ding", "claude", "ding", Agent::Claude)])).with_rehome(rehome_quotas, rehome_ask, false);
        app.set_sessions(on_ding());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        shows(&reader_rows(&mut app, 80, 20), &["没有别的 Claude 账号可以换。", "按 + 加一个,登录好再按 X。", "+ 加账号"]);
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none());
        press(&mut app, KeyCode::Esc);
        // shell 会话、普通 tmux 会话:不开
        let mut v = on_ding();
        v[3].agent = Agent::None; // 闲
        v[0].deck_id.clear(); // 问
        let mut app = App::new(String::new(), v).with_tools(rehome_tools);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "只能给 Claude / Codex 会话换账号");
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('X'));
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话不能换账号(没在 aca 里登记) · A 接管");
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
        // 列表那一行下面:原话(放不下截短)、X 能干什么;预览:状态、原话、按钮
        shows(
            &rows,
            &[
                "You've hit your session li…  X 换账号",
                "出错 · 额度 / 登录",
                "✕ You've hit your session limit",
                "X 换账号  对话整段带到别的账号",
            ],
        );
        // 别的出错(不是账号的):只在预览里写原话,不提 X
        app.set_sessions(fail("prompt is too long", "fatal"));
        let rows = reader_rows(&mut app, 100, 30);
        shows(&rows, &["✕ prompt is too long"]);
        assert!(!rows.iter().any(|r| r.contains("X 换账号")), "{rows:#?}");
    }

    #[test]
    fn shift_h_only_hands_off_claude_or_codex_with_a_conversation() {
        let mut v = sessions();
        v[3].agent = Agent::None; // 闲:shell 会话
        let mut app = App::new(String::new(), v).with_tools(handoff_tools);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "只能交接 Claude / Codex 会话");
        // 找不到对话记录(默认的 loader 就是这样)
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "找不到它的对话记录");
        // 菜单里只有 shell
        let mut app = fixture().with_handoff(fake_source).with_tools(|| Ok(vec![tool("shell", "shell", "", Agent::None)]));
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none() && app.handoff.is_none());
        assert!(app.msg.as_ref().unwrap().0.starts_with("工具菜单里没有能接手的"), "{:?}", app.msg);
        // 在忙:等做完
        let mut app = fixture().with_handoff(fake_source).with_tools(handoff_tools);
        app.busy = Some("正在重启「闲」…".into());
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('H'));
        assert!(app.tool_menu.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "还在忙:正在重启「闲」…");
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
        // 没改就回车:不做
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none() && app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "MCP 没改");
        press(&mut app, KeyCode::Char('M'));
        press(&mut app, KeyCode::Char(' ')); // alpha 挂上
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' ')); // beta 卸下
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' ')); // 全局的改不了
        assert_eq!(app.msg.as_ref().unwrap().0, "这一项只读");
        shows(&reader_rows(&mut app, 80, 20), &["[x] alpha", "[ ] beta"]);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job, Some(Job::Mcp { t: target("闲"), dir: "/p/work".into(), on: vec!["alpha".into()], off: vec!["beta".into()] }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在改「闲」的 MCP…"));
        app.job_done(&job, Outcome::Done("已挂上 alpha · 已卸下 beta · 重启后生效".into()), sessions());
        assert!(app.busy.is_none());
        // Esc 什么都不改
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
        // 手机上从底下升起,也放得下
        shows(&reader_rows(&mut app, 44, 30), &["MCP · 闲", "·  lint"]);
    }

    #[test]
    fn restart_moves_pin_and_highlight_to_the_new_tmux_name() {
        let mut app = fixture();
        pin(&mut app, "闲");
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('R'));
        let job = app.take_job().unwrap();
        // 改名后 tmux 会话名换了,登记的 id 不变
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

    /// parent 会话里另开的窗口(split = 分出来的窗格)
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

    /// 写完之后读回来的:组内按给的先后
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
        // 组里第一个了:不跨到上一组
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最上");
        // 正在写的时候接着挪:列表照挪,连着的两下只记最后一下
        shift(&mut app, KeyCode::Down);
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.take_job().is_none());
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最下");
        // 第一下写完,读回来的只有第一下:后面那下照样挪着,接着写
        app.msg = None;
        app.job_done(&job, Outcome::Done(String::new()), reordered(&["闲", "跑", "错"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.msg.is_none());
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Order { t: target("闲"), by: "id-错".into(), after: true });
        // 写完之前的定时刷新读到的还是旧的,不跳回去
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
        shift(&mut app, KeyCode::Up); // 两下一起到,run 循环这时才开始写第一下
        assert!(matches!(app.take_job(), Some(Job::Order { .. })));
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最上");
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
        app.job_done(&job, Outcome::Failed("挪位置失败:x".into()), sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        assert_eq!(app.msg.as_ref().unwrap().0, "挪位置失败:x");
        assert!(app.take_job().is_none());
    }

    /// 写完之后读回来的:分组按给的先后
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
        assert_eq!(app.head.as_deref(), Some("work")); // 高亮跟着分组走
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::GroupOrder { group: "work".into(), by: "fun".into(), after: false });
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "分组已在最上");
        // 写完之前的定时刷新读到的还是旧的,不跳回去
        app.set_sessions(sessions());
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        app.job_done(&job, Outcome::Done(String::new()), regrouped(sessions(), &["work", "fun"]));
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        assert!(app.take_job().is_none());
        // 挪回去:折着的分组(折叠行)也能挪
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
        let mut other = mk("壳", "-", State::Idle, 10); // 普通 tmux 会话:沉底,不跟着挪
        other.deck_id.clear();
        v.push(other);
        let v = regrouped(v, &["fun", "work", "work/a", "work/b"]);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错", "甲", "乙", "壳"]);
        jump_to_group(&mut app, "fun");
        shift(&mut app, KeyCode::Down); // 越过 work 一整家
        assert_eq!(order(&app), ["跑", "闲", "错", "甲", "乙", "问", "未读", "壳"]);
        assert_eq!(app.take_job().unwrap(), Job::GroupOrder { group: "fun".into(), by: "work".into(), after: true });
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "分组已在最下");
        jump_to_group(&mut app, "work/b");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["跑", "闲", "错", "乙", "甲", "问", "未读", "壳"]);
        shift(&mut app, KeyCode::Up); // 出不了 work
        assert_eq!(app.msg.as_ref().unwrap().0, "已在 work 里最上");
        jump_to_group(&mut app, "-");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话那一区不能挪");
    }

    #[test]
    fn a_failed_group_write_puts_the_groups_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        jump_to_group(&mut app, "work");
        shift(&mut app, KeyCode::Up);
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed("挪分组失败:x".into()), sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        assert_eq!(app.msg.as_ref().unwrap().0, "挪分组失败:x");
        assert!(app.take_job().is_none());
    }

    #[test]
    fn a_session_takes_its_windows_and_sub_sessions_along_and_sub_sessions_stay_under_the_parent() {
        let mut v = sessions();
        let idle = v[3].clone(); // 闲
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
        shift(&mut app, KeyCode::Up); // 出不了父会话
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最上");
        select(&mut app, "跑"); // 越过「闲」一整家
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "闲", "zsh", "子二", "子一", "跑", "错"]);
        select(&mut app, "zsh");
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "#2 是窗口,只能 ⏎ / d / D");
    }

    #[test]
    fn pinned_and_plain_tmux_sessions_stay_put() {
        let mut v = sessions();
        v[4].deck_id.clear(); // 错:普通 tmux 会话
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "错");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话不能挪(没在 aca 里登记)");
        select(&mut app, "闲"); // 下面只剩普通 tmux 会话
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最下");
        pin(&mut app, "跑");
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "置顶的不按组排");
        assert!(app.take_job().is_none());
    }

    fn with_windows() -> App {
        let mut v = sessions();
        let idle = v.iter().find(|s| s.title == "闲").unwrap().clone();
        let err = v.iter().find(|s| s.title == "错").unwrap().clone();
        v.push(sub_of(&idle, 2, false, "zsh", State::Idle, 20));
        v.push(sub_of(&idle, 1, true, "htop", State::Idle, 30));
        // 「错」一天多没动了,但它另开的窗口里在跑
        let mut build = sub_of(&err, 3, false, "构建", State::Running, 5);
        (build.agent, build.who) = (Agent::Codex, "cx".into()); // 窗口里手动起的 codex
        v.push(build);
        App::new(String::new(), v)
    }

    #[test]
    fn shift_t_opens_a_new_window_in_its_session_in_its_real_work_dir() {
        let mut app = with_windows();
        // 窗口那行:开在它所在的会话里,新窗口跑 shell(不是分屏)
        reader_rows(&mut app, 120, 30);
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('T'));
        let sp = app.split.take().unwrap();
        assert_eq!((sp.name.as_str(), sp.yazi), ("闲", false));
        assert_eq!(sp.w.dir, "/p/work"); // 去找真正干活的目录从这里起步
        // 手机上也一样
        reader_rows(&mut app, 46, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('T'));
        let sp = app.split.take().unwrap();
        assert_eq!((sp.name.as_str(), sp.yazi), ("闲", false));
        // 停掉的没法分
        let mut v = sessions();
        v[3].state = State::Stopped; // 闲
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('T'));
        assert!(app.split.is_none() && app.msg.as_ref().unwrap().0.contains("没在跑"), "{:?}", app.msg);
    }

    #[test]
    fn t_opens_a_terminal_under_the_list_and_esc_puts_it_away() {
        let mut app = with_windows();
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('t'));
        let r = app.term_req.take().unwrap();
        assert_eq!((r.key.as_str(), r.family.as_str(), r.w.dir.as_str()), (app.current().unwrap().name.as_str(), "闲", "/p/work"));
        // 开着时按键都给终端:q 不退出、^C 不退出、j 不移动,Esc 收起
        app.term_on = Some(r.key.clone());
        assert!(app.typing()); // 粘贴 / 连敲不会被当成自动输入丢掉
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
        // 折叠行是分组:开在组的路径(见 t_on_a_group_opens_a_terminal_in_the_groups_dir)
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
        // 同一个链接不再弹
        app.catch_links();
        assert_eq!(app.overlays.len(), 1);
        // 键先给弹窗:Esc 关的是它,终端还开着
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.term_on.as_deref() == Some("account:ding"));
        assert!(app.msg.as_ref().is_some_and(|(m, _)| m.contains("链接还在登录终端里")), "{:?}", app.msg);
        // 再 Esc:收起终端
        press(&mut app, KeyCode::Esc);
        assert!(app.term_on.is_none());
        // 终端关了,链接文件一起删
        app.terms.clear();
        assert!(!file.exists());
    }

    #[test]
    fn y_opens_yazi_where_it_really_works() {
        let mut app = with_windows();
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('y'));
        let Some(External::Yazi(sp)) = app.external.take() else { panic!("{:?}", app.msg) };
        assert_eq!(sp.w.dir, "/p/work"); // 真正干活的目录让出终端时再从这里找
        // 弹窗里改成分一格跑它:分的是那一格
        assert_eq!((sp.name.as_str(), sp.target.as_str(), sp.yazi), ("闲", "%2", true));
        // 停着的也能开(目录用它记下的),只是没有能分的格
        let mut v = sessions();
        v[3].state = State::Stopped; // 闲
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('y'));
        let Some(External::Yazi(sp)) = app.external.take() else { panic!("{:?}", app.msg) };
        assert_eq!(sp.target, "");
        // 折叠行不知道开哪个
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('y'));
        assert!(app.external.is_none() && app.msg.as_ref().unwrap().0.contains("先展开"), "{:?}", app.msg);
    }

    #[test]
    fn the_popup_opens_on_where_that_client_is() {
        let mut app = with_windows();
        app.highlight_here("$闲", "%2"); // 停在「闲」的 #2 → 高亮那一行
        assert_eq!(app.current().unwrap().title, "zsh");
        app.highlight_here("$跑", "%9"); // 那一格没有自己的行 → 会话那行
        assert_eq!(app.current().unwrap().title, "跑");
        app.highlight_here("$别处", ""); // 列表里没有 → 不动
        assert_eq!(app.current().unwrap().title, "跑");
    }

    fn place(row: &str, session: &str, pane: &str) -> crate::follow::Place {
        crate::follow::Place { row: row.into(), session: session.into(), pane: pane.into() }
    }

    /// 弹窗开在「闲」的一个没有自己那行的格(%9)上,跟随着
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
        // 滑动途中不切,停稳了切;切过了不再切
        select(&mut app, "跑");
        app.moved = Some((Instant::now(), Duration::ZERO));
        assert_eq!(app.follow_step(), None);
        app.moved = None;
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        assert_eq!(app.follow_step(), None);
        // 会话里另开的窗口那一行:落在那一格
        select(&mut app, "zsh");
        assert_eq!(app.follow_step(), Some(place("闲\u{1}%2", "$闲", "%2")));
        // 挪回打开时那一行:落回原来那一格
        select(&mut app, "闲");
        assert_eq!(app.follow_step(), Some(place("闲", "$闲", "%9")));
        // 正在打过滤词:不切,回车定下来才切
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('跑'));
        assert_eq!((app.follow_step(), app.current().unwrap().title.as_str()), (None, "跑"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        press(&mut app, KeyCode::Esc); // 清掉过滤词,不是退出
        assert!(!app.quit);
        // 停在分组上:旁边不动
        press(&mut app, KeyCode::Char('g'));
        assert!(app.on_group_row());
        assert_eq!(app.follow_step(), None);
        // 没在跑的会话:切不过去
        let mut v = sessions();
        v[2].state = State::Stopped; // 跑
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        app.follow_from("$闲", "");
        select(&mut app, "跑");
        assert_eq!(app.follow_step(), None);
    }

    #[test]
    fn esc_while_following_goes_back_and_the_popup_key_stays() {
        // Esc:回到打开时停着的地方再退,留下的那个不算看过
        let mut app = following();
        select(&mut app, "跑");
        app.follow_step();
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.quit, app.follow_step()), (true, Some(place("闲", "$闲", "%9"))));
        app.follow_done();
        assert_eq!(app.reads.read_at("id-跑"), None);
        // ^\(终端里到手是 ^4):关掉,人留在旁边露出来的那个,它算看过
        for key in ['4', '\\'] {
            let mut app = following();
            select(&mut app, "跑");
            app.follow_step();
            ctrl(&mut app, key);
            assert_eq!((app.quit, app.follow_step()), (true, None));
            app.follow_done();
            assert!(app.reads.read_at("id-跑").is_some());
        }
        // 快捷键弹窗里按它只是查它
        let mut app = following();
        press(&mut app, KeyCode::Char('?'));
        ctrl(&mut app, '4');
        assert!(!app.quit);
        // 没挪过就关:原来那个不用记
        let mut app = following();
        ctrl(&mut app, '4');
        app.follow_done();
        assert_eq!((app.quit, app.reads.read_at("id-闲")), (true, None));
        // 不跟随、也不在弹窗里:^\ 不是退出
        let mut app = with_windows();
        ctrl(&mut app, '4');
        assert_eq!(app.quit, crate::data::in_popup());
    }

    #[test]
    fn passing_by_an_unread_session_while_following_leaves_it_unread() {
        let mut v = sessions();
        v[2].state = State::Unread; // 跑:跑完没看
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        app.follow_from("$闲", "");
        select(&mut app, "跑");
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        assert!(app.reads.unread("id-跑"));
        // 路过别的:不是「跑完没看」的不记
        select(&mut app, "问");
        app.follow_step();
        assert_eq!(app.reads.mark("id-问"), None);
        // 回到未读的那个、关在它上面:看过了
        select(&mut app, "跑");
        app.follow_step();
        ctrl(&mut app, '4');
        app.follow_done();
        assert!(app.reads.read_at("id-跑").is_some());
    }

    #[test]
    fn following_draws_no_preview_and_v_leaves_the_saved_layouts_alone() {
        let mut app = with_windows();
        // 不跟随:窄屏上下分屏,预览占下半屏。跟随着:只剩底下那一行写着它在干什么
        let strip = |rows: &[String]| rows.iter().position(|r| r.starts_with('─')).unwrap();
        assert!(strip(&reader_rows(&mut app, 56, 30)) < 20);
        app.highlight_here("$闲", "%9");
        app.follow_from("$闲", "%9");
        let rows = reader_rows(&mut app, 56, 30);
        assert_eq!((app.mode(), strip(&rows)), (Preview::Off, 28), "{rows:#?}");
        // 底栏说的是跟随时这两个键做什么
        assert!(rows[29].contains("Esc 回去") && rows[29].contains("⏎ 留下"), "{}", rows[29]);
        let saved = app.view_state();
        press(&mut app, KeyCode::Char('v'));
        assert!(app.mode() != Preview::Off);
        assert!(app.view_state() == saved, "v 换的只管这一次");
    }

    #[test]
    fn windows_and_split_panes_hang_under_their_session() {
        let mut app = with_windows();
        // 紧跟在自己的会话下面,窗口号、格号排;会话一天没动但窗口在跑,整家都在
        assert_eq!(order(&app), ["问", "跑", "闲", "htop", "zsh", "错", "构建"]);
        let rows = reader_rows(&mut app, 120, 30);
        // 同一个会话的几个窗口用 ├ 串起来,最后一个 └
        shows(&rows, &["├ ○ #1.1 htop", "└ ○ #2 zsh", "└ ● #3 构建"]);
        // 窗口号长短不一,时间那一栏照样对齐
        let col = |t: &str| {
            let r = row_of(&rows, t);
            r[..r.find(t).unwrap()].width()
        };
        assert_eq!((col(" 10s"), col(" 30s")), (col(" 20s"), col(" 20s")), "{rows:#?}");
        // 账号跟在标题后面,按哪家上色
        let t = rows.iter().find(|r| r.contains("#3 构建")).unwrap();
        assert!(t.contains(" cx "), "{t}");
        // 分区标题的个数只数会话
        assert!(rows.iter().any(|r| r.contains("work  3")), "{rows:#?}");
        // w:会话闲着、窗口在跑也算
        press(&mut app, KeyCode::Char('w'));
        assert!(order(&app).contains(&"构建".to_string()) && order(&app).contains(&"错".to_string()));
        press(&mut app, KeyCode::Char('w'));
        // 回车:进它所在的会话,先切到那一格
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
        // 对登记会话的操作不对窗口(D 只停这一格,见下面)
        for k in ['R', 'r', 'm'] {
            press(&mut app, KeyCode::Char(k));
            assert!(app.confirm.is_none() && app.job.is_none() && app.prompt.is_none(), "{k}");
            assert!(app.msg.as_ref().unwrap().0.contains("#2 是窗口"), "{k}: {:?}", app.msg);
        }
        // d:只关这一格
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭窗口？", "y 关闭"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job();
        assert!(matches!(&job, Some(Job::Remove { t, pane: Some(p) }) if t.title == "zsh" && p == "%2"), "{job:?}");
        assert_eq!(app.doing().as_deref(), Some("正在关闭「zsh」…"));
        // 只藏这一格,会话和别的窗口都在
        let at = |app: &App, t: &str| app.all.iter().any(|s| s.title == t);
        assert!(!at(&app, "zsh") && at(&app, "闲") && at(&app, "htop"));
        let fresh = with_windows().all; // 关之前读的,还有它
        app.job_done(&job.unwrap(), Outcome::Done("已关闭「zsh」".into()), fresh);
        assert!(app.doing().is_none() && !at(&app, "zsh"));
        select(&mut app, "htop");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭窗格？"]);
        press(&mut app, KeyCode::Esc);
        // p:置顶它所在的会话,窗口跟着一起上去
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(order(&app)[..2], ["闲", "htop"]);
    }

    #[test]
    fn big_d_on_a_window_stops_just_that_pane_and_enter_brings_it_back_in_place() {
        let mut app = with_windows();
        reader_rows(&mut app, 120, 30);
        // 窗口那行:只停它,留在原地
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('D'));
        shows(&reader_rows(&mut app, 100, 30), &["停止这一格？", "程序停掉，这一格留在原地，⏎ 再启动", "y 停止"]);
        press(&mut app, KeyCode::Char('y'));
        let pane = |id: &str, label: &str| crate::park::Pane { id: id.into(), label: label.into() };
        let t = |label: &str| Target { name: "闲".into(), title: label.into(), profile: "default".into(), id: String::new() };
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Park { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") });
        // 停完:记一步(^Z 在原地再启动,见下面)
        let mut fresh = with_windows().all;
        fresh.iter_mut().find(|s| s.title == "zsh").unwrap().state = State::Dead;
        app.job_done(&job, Outcome::Done("已停「#2「zsh」」 · ⏎ 再启动".into()), fresh.clone());
        // 停着的:D 说一声,⏎ 在原地再启动(run 循环起来了再 attach)
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('D'));
        assert_eq!(said(&app), "已经停了 · ⏎ 再启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.respawn.take(), app.attach.take()), (Some(("闲".to_string(), pane("%2", "#2「zsh」"))), None));
        ctrl(&mut app, 'z');
        assert_eq!(app.take_job(), Some(Job::Respawn { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") }));
        app.job_done(&Job::Respawn { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") }, Outcome::Done("已重启".into()), fresh);
        assert_eq!(said(&app), "已撤销停止:「#2「zsh」」在原地再启动了");
        // 登记的会话那行:停的是 agent(它的窗格、别的窗口都留着),不是「这一格」
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('D'));
        assert!(matches!(app.confirm, Some(Confirm::Stop { .. })));
        press(&mut app, KeyCode::Esc);
        // R:在原地重启 agent,别的窗口不动
        press(&mut app, KeyCode::Char('R'));
        assert_eq!(app.take_job(), Some(Job::Restart { t: target("闲"), force: false }));
    }

    #[test]
    fn an_agent_that_quit_but_left_its_pane_starts_again_on_enter() {
        // agent 退了(D 停的、自己退的),窗格连画面留着:■。⏎ 接着原来的对话再起,不是直接进去,也不是照窗格原来的命令重跑
        let mut v = with_windows().all;
        let h = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (h.state, h.pane) = (State::Dead, "%0".into());
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take(), app.respawn.take()), (Some(target("闲")), None, None));
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "已经停了 · ⏎ 重新启动");
        // 它里面另开的窗口照常进
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
        // 普通 tmux 会话:D 停的是它那一格,⏎ 照原来的命令在原地再起
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
        // 登记的会话:删的是整个会话,里面另开的窗口一起关,确认框里说
        let mut app = with_windows();
        reader_rows(&mut app, 120, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["删除会话？", "里面另开的 2 个窗口 / 窗格一起关", "y 删除"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Remove { t, pane: None } if t.title == "闲"), "{job:?}");
        assert!(!at(&app, "闲") && !at(&app, "htop") && !at(&app, "zsh"));

        // 普通 tmux 会话那行、里面还有别的窗口:只关头一格,下一个顶上来
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
        shows(&reader_rows(&mut app, 100, 30), &["关闭这一格？", "后面的往前挪，#1.1「htop」当头", "y 关闭"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Remove { t, pane: Some(p) } if t.title == "闲" && p == "%0"), "{job:?}");
        // 关的时候谁都不藏:会话那行留着等 htop 顶上来,窗口不跟着会话一起藏(藏了就找不到了),也不会没了头
        assert!(at(&app, "闲") && at(&app, "htop") && at(&app, "zsh"));
        reader_rows(&mut app, 100, 30);
        // 关完:htop 顶上来成了会话那行(会话名没变)
        let mut fresh: Vec<Session> = with_windows().all.into_iter().filter(|s| s.title != "htop").collect();
        plain(&mut fresh, "%1");
        app.job_done(&job, Outcome::Done("已关闭「闲」".into()), fresh);
        assert!(at(&app, "闲") && at(&app, "zsh"));
        // 只剩一格了:d 就是关掉整个会话
        let mut v = sessions();
        plain(&mut v, "%1");
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Remove(r)) if r.pane.is_none() && r.next.is_none() && r.others == 0));
    }

    /// 「闲」派出去两个子会话:「子跑」在跑(里面另开了个窗口)、「子旧」三天没动;parent_idle = 「闲」多久没动
    fn with_children(parent_idle: u64) -> App {
        let mut v = sessions();
        let idle = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (idle.idle_for, idle.active_at) = (parent_idle, 1_000_000 - parent_idle);
        let child = |title: &str, state: State, idle_for: u64, order: i64| Session {
            up: Up::Under("闲".into(), "闲".into()),
            slot: (1, order, 0),
            ..mk(title, "work", state, idle_for)
        };
        let run = child("子跑", State::Running, 5, -1); // 组内顺序比谁都靠前,照样跟着父会话
        v.push(sub_of(&run, 2, false, "zsh", State::Idle, 20));
        v.push(run);
        v.push(child("子旧", State::Idle, 3 * 86400, 9));
        App::new(String::new(), v)
    }

    /// 列表里含 want 的那一行
    fn row_of(rows: &[String], want: &str) -> String {
        rows.iter().find(|r| r.contains(want)).unwrap_or_else(|| panic!("缺「{want}」:{rows:#?}")).clone()
    }

    #[test]
    fn children_hang_under_the_session_that_sent_them() {
        let mut app = with_children(10);
        // 近一天:跟在父会话(和它的窗口)后面;三天没动的子会话照样藏
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        for (w, two) in [(46, false), (120, true)] {
            let rows = reader_rows(&mut app, w, 30);
            assert!(row_of(&rows, "子跑").contains("└ ● 子跑"), "{w}: {rows:#?}");
            assert!(row_of(&rows, "zsh").contains("  └ ○ #2 zsh"), "{w}: {rows:#?}");
            // 分区标题的个数:子会话也是会话
            assert!(rows.iter().any(|r| r.contains("work  3")), "{two}: {rows:#?}");
        }
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "子旧", "错"]);
        // 删父会话:子会话不会一起删,说一声(藏着的也算)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 46, 16), &["删除会话？", "有 2 个子会话，不会一起删"]);
        press(&mut app, KeyCode::Esc);
        // 子会话自己、窗口都不说
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.confirm.as_ref().map(|c| matches!(c, Confirm::Remove(r) if r.kids == 0)), Some(true));
    }

    #[test]
    fn a_kept_child_keeps_its_parent_but_not_the_other_way_round() {
        // 父会话三天没动,子会话在跑:父会话也留下,子会话挂在它下面
        let mut app = with_children(3 * 86400);
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        assert!(row_of(&reader_rows(&mut app, 46, 30), "子跑").contains("└ ● 子跑"));
        // 搜子会话:父会话跟着出来;搜父会话:子会话不跟
        let v: Vec<Session> = app.all.clone();
        let mut app = App::new("子跑".into(), v.clone());
        assert_eq!(order(&app), ["闲", "子跑", "zsh"]);
        assert!(row_of(&reader_rows(&mut app, 46, 30), "● 子跑").contains("└ ● 子跑")); // 顶栏的搜索词也有「子跑」
        assert_eq!(order(&App::new("闲".into(), v)), ["闲"]);
    }

    #[test]
    fn tree_branches_connect_through_the_question_line() {
        let mut v = with_children(10).all;
        let p = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (p.state, p.question) = (State::Ask, "Allow?".into());
        let mut app = App::new(String::new(), v);
        let rows = reader_rows(&mut app, 46, 30);
        // 它问的那句夹在父会话和子会话中间:竖线从那句旁边穿过去,问句和标题对齐
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        let (p, q, c, w) = (at("◐ 闲"), at("Allow?"), at("● 子跑"), at("#2 zsh"));
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
        // 树枝连起来:子跑后面还有子旧,所以是 ├;子跑的窗口前面带着它的 │
        let rr = reader_rows(&mut app, 46, 30);
        for want in ["├ ● 子跑", "│ └ ○ #2 zsh", "└ ○ 子旧"] {
            assert!(row_of(&rr, want).contains(want), "{rr:#?}");
        }
        // 跟着上去的子会话上按 p:取消的是父会话
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "取消置顶「闲」");
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        // 子会话自己置顶:在置顶块里顶格,父会话留在分组里
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已置顶「子跑」");
        assert_eq!(order(&app), ["子跑", "zsh", "问", "跑", "闲"]);
        let r = row_of(&reader_rows(&mut app, 46, 30), "子跑");
        assert!(r.contains("● 子跑") && !r.contains('└'), "{r}");
    }

    /// [`with_children`] 再往下两层:「子跑」派了「孙」(里面另开了个窗口),「孙」又派了「曾孙」,都在跑
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
        // 一个会话后面是它整家:窗口、子会话(连它的窗口、它再派出去的),然后才轮到下一个
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        for w in [46, 120] {
            let rows = reader_rows(&mut app, w, 30);
            for want in ["└ ● 子跑", "  ├ ○ #2 zsh", "  └ ● 孙", "    ├ ○ #3 top", "    └ ● 曾孙"] {
                assert!(row_of(&rows, want).contains(want), "{w}: {rows:#?}");
            }
        }
        // 子跑后面还有兄弟(子旧):它那一层的竖线一直带到它整家的最后一行
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙", "子旧", "错"]);
        let rows = reader_rows(&mut app, 46, 30);
        let wants = ["├ ● 子跑", "│ ├ ○ #2 zsh", "│ └ ● 孙", "│   ├ ○ #3 top", "│   └ ● 曾孙", "└ ○ 子旧"];
        for want in wants {
            assert!(row_of(&rows, want).contains(want), "{rows:#?}");
        }
        // h 一次往外一层
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "孙");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "子跑");
        // 删中间那个:确认框里数的是它自己派的(删了它,它们顶格;再往下的还挂在它们下面)
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.confirm.as_ref().map(|c| matches!(c, Confirm::Remove(r) if r.kids == 1)), Some(true));
        press(&mut app, KeyCode::Esc);
        // 搜最里面那个:上面各层都跟着出来(连它们的窗口);搜中间的:它下面的不跟
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
        // 第 TREE_DEEP 层往下并排着画,不再往右缩
        assert_eq!(marks, ["", "└ ", "  └ ", "    └ ", "      ├ ", "      ├ ", "      └ "]);
        // 父会话不在上面那一串里(筛掉了):从它那里起算顶格
        let marks: Vec<String> = tree_marks(&view[2..5], true).into_iter().map(|m| m.0).collect();
        assert_eq!(marks, ["", "└ ", "  └ "]);
    }

    #[test]
    fn a_pin_carries_every_level_below_it_and_p_down_there_undoes_the_nearest_one() {
        let mut app = with_grandchildren();
        pin(&mut app, "闲");
        assert_eq!(order(&app)[..6], ["闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        // 跟着上去的曾孙上按 p:取消的是最上面置顶了的那个
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "取消置顶「闲」");
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        // 中间那个自己置顶:带着它下面的在置顶块里顶格,上面的留在分组里
        pin(&mut app, "孙");
        assert_eq!(order(&app), ["孙", "top", "曾孙", "问", "跑", "闲", "子跑", "zsh"]);
        let rows = reader_rows(&mut app, 46, 30);
        assert!(!row_of(&rows, "● 孙").contains('└'), "{rows:#?}");
        for want in ["├ ○ #3 top", "└ ● 曾孙", "└ ● 子跑", "  └ ○ #2 zsh"] {
            assert!(row_of(&rows, want).contains(want), "{rows:#?}");
        }
        // 上面的也置顶了:各跟各的,曾孙跟的是离它最近的那个
        pin(&mut app, "闲");
        assert_eq!(order(&app)[..6], ["孙", "top", "曾孙", "闲", "子跑", "zsh"]);
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "取消置顶「孙」");
        assert_eq!(order(&app)[..6], ["闲", "子跑", "zsh", "孙", "top", "曾孙"]);
    }

    #[test]
    fn shift_arrows_move_a_sub_session_with_everything_under_it() {
        let mut app = with_grandchildren();
        press(&mut app, KeyCode::Char('a'));
        // 子跑往下:越过自己下面那几层,和兄弟(子旧)换,整家一起走
        select(&mut app, "子跑");
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子旧", "子跑", "zsh", "孙", "top", "曾孙", "错"]);
        assert_eq!(app.take_job().unwrap(), Job::Order { t: target("子跑"), by: "id-子旧".into(), after: true });
        // 没有兄弟的:出不了父会话那一家
        select(&mut app, "孙");
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最下");
        select(&mut app, "曾孙");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "已在组内最上");
        // 顶格的越过别人一整家(几层都算)
        select(&mut app, "跑");
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "闲", "子旧", "子跑", "zsh", "孙", "top", "曾孙", "跑", "错"]);
    }

    #[test]
    fn big_d_stops_after_y_and_a_stopped_one_starts_on_enter() {
        // 活动时间用真的:停掉后按现在的时间接着算多久没动
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
        press(&mut app, KeyCode::Enter); // 只认 y
        assert!(app.job.is_none() && app.attach.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "没停");
        press(&mut app, KeyCode::Char('D'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.job, Some(Job::Stop { t: target("闲") }));
        let job = app.take_job().unwrap();
        // 停掉后:还在列表里,标 ■;tmux 活动时间没了,接着用停之前看到的,不会被「超过 1 天」藏起来
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
        // 再按 D:已经停了;⏎ 先启动再进去,不直接 attach
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已经停了 · ⏎ 重新启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
        // 不在 w(开着的)里
        press(&mut app, KeyCode::Char('w'));
        assert!(!titles(&app).contains(&"闲".to_string()));
    }

    /// 出错了的插件:顶栏右边一格 ⚠,数的是几个插件;宽的写上「插件出错(P)」
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
        v[3].state = State::Crashed; // 闲:上一轮出错收场,tmux 里也没有了
        v[4].state = State::Idle; // 错:只留一个出错的,看顶栏数不数它
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        assert!(row_of(&reader_rows(&mut app, 46, 16), "闲").contains("✕ 闲"));
        shows(&reader_rows(&mut app, 120, 16), &["出错 · 没在跑", "⏎ 启动 · d 删除"]);
        assert!(!top_bar(&app, 80).to_string().contains("出错")); // 没在跑的不算出错
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
        press(&mut app, KeyCode::Char('w')); // 也不算开着的
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
        shows(&reader_rows(&mut app, 46, 12), &["重命名", "> 闲▏", "⏎ 确定 • Esc 取消"]);
        press(&mut app, KeyCode::Enter); // 没改
        assert!(app.job.is_none() && app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "没改名(和原来一样)");
        press(&mut app, KeyCode::Char('r'));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter); // 空的不收
        shows(&reader_rows(&mut app, 46, 12), &["标题是空的"]);
        typ(&mut app, "闲着 jk"); // 打字时 j k 是字,不是上下
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(Job::Rename { t: target("闲"), to: "闲着 jk".into(), unlock: false }));
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "没改名");
        // 刷新挪到了 ^R
        ctrl(&mut app, 'r');
        assert!(app.refresh && app.prompt.is_none());
    }

    #[test]
    fn m_picks_a_group_and_a_new_name_becomes_a_new_group() {
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑"); // 在 work
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        assert_eq!(p.cands, ["fun", "Zeta"]); // 列表里的分区顺序在前,不含它现在的 work,不重复
        app.group_picked(p, Ok(Some(" side proj/part ".into())));
        let all = ["Zeta", "fun", "work"].map(String::from).to_vec();
        assert_eq!(app.job.take(), Some(Job::Move { t: target("跑"), from: "work".into(), to: "side-proj/part".into(), groups: all }));
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(Some("Work".into())));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已经在 Work 里了");
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(None)); // fzf 里按了 Esc
        assert!(app.job.is_none());
    }

    #[test]
    fn keys_for_registered_sessions_refuse_plain_tmux_sessions() {
        let mut s = mk("外", "-", State::Idle, 5);
        (s.deck_id, s.profile) = (String::new(), String::new());
        let mut app = App::new(String::new(), vec![s]);
        for (key, what) in [('R', "重启"), ('r', "改名"), ('m', "挪分组")] {
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.msg.as_ref().unwrap().0, format!("普通 tmux 会话不能{what}(没在 aca 里登记) · A 接管"));
        }
        assert!(app.job.is_none() && app.prompt.is_none() && app.pick_group.is_none() && app.confirm.is_none());
    }

    /// 接管前认出来的那份(假的):只认「外」这个 tmux 会话
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
        // 登记过的会话:已经是了
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.msg.as_ref().unwrap().0, "它已经是 aca 的会话了");
        assert!(app.confirm.is_none());
        // 认不了的(那个会话刚好没了):说为什么,不弹框
        select(&mut app, "没了的");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.msg.as_ref().unwrap().0, "接管不了:tmux 里没有 $没了的 这个会话");
        assert!(app.confirm.is_none());

        // 普通会话:先把认出来的摆出来
        select(&mut app, "外");
        press(&mut app, KeyCode::Char('A'));
        assert!(matches!(app.confirm, Some(Confirm::Adopt { .. })));
        let rows = reader_rows(&mut app, 100, 30).join("\n");
        for want in [
            "接管这个会话？",
            "登记成 claude:sub · 分组 work",
            "对话 c-123456，进程不动，重启后接着来",
            "tmux 会话名改成「修登录」",
            "每次启动带 --yolo",
            "agent-deck 那边会显示成停了",
            "y 接管",
        ] {
            assert!(rows.contains(want), "确认框里没有「{want}」:\n{rows}");
        }
        // 只认 y,别的键都是不接
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.msg.as_ref().unwrap().0, "没接管");
        assert!(app.job.is_none() && app.confirm.is_none());
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        let t = Target { name: "外".into(), title: "外".into(), profile: String::new(), id: String::new() };
        assert_eq!(job, Job::Adopt { t, plan: Box::new(fake_plan("$外").unwrap()) });
        assert_eq!(job.doing(), "正在接管「外」…");

        // 接过来之后那一行换成了登记的(键是 id、进了它的分组):高亮跟过去
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
        // 你自己起的会话名(锁着)、没认出对话、不带参数、不是 agent-deck 的
        (p.s.title, p.s.title_locked, p.tmux) = ("我的会话".into(), true, "我的会话".into());
        (p.s.conv, p.s.args, p.s.group, p.deck) = (String::new(), vec![], String::new(), None);
        assert_eq!(adopt_summary(&p), "登记成 claude:sub\n没认出是哪段对话：重启会是新的一段");
        (p.s.tool, p.s.account) = ("shell".into(), String::new());
        assert_eq!(adopt_summary(&p), "登记成 shell\n里面的程序不动");
    }

    #[test]
    fn ctrl_n_names_a_group_then_asks_for_the_path() {
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑");
        ctrl(&mut app, 'n');
        shows(&reader_rows(&mut app, 46, 12), &["新建分组", "> ▏", "⏎ 下一步"]);
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 46, 12), &["分组名是空的"]);
        typ(&mut app, "side proj");
        press(&mut app, KeyCode::Enter);
        // 直接进 fzf,不带搜索词;Esc 回来输入框里是默认目录
        assert!(std::mem::take(&mut app.pick));
        let p = app.dir_pick_from(vec![("~/workspace".into(), Src::Near), ("/p/fun".into(), Src::Known)], 40, 16).unwrap();
        assert_eq!((p.query.as_str(), p.rows[0].group.clone()), ("", ("side-proj".into(), true)));
        app.picked(Ok(None));
        assert_eq!(app.new_draft.as_ref().unwrap().text, "~/workspace/");
        shows(&reader_rows(&mut app, 60, 14), &["新建会话", "side-proj  新分组 · 绑定目录", "~/workspace/"]);
        ctrl(&mut app, 'w'); // 路径改了,分组还是它
        typ(&mut app, "elsewhere");
        let p = app.dir_pick_from(vec![], 40, 16).unwrap();
        assert_eq!(p.query, "elsewhere");
        press(&mut app, KeyCode::Enter);
        let req = app.create.take().unwrap();
        // 新组记下填的目录当默认路径
        assert_eq!((req.group.as_str(), req.bind, req.by_dir), ("side-proj", true, false));
        // fzf 里回车就开在那
        let mut app = app.with_new_group_dir(|| "/p".into());
        ctrl(&mut app, 'n');
        typ(&mut app, "lab");
        press(&mut app, KeyCode::Enter);
        assert!(std::mem::take(&mut app.pick));
        assert_eq!(app.new_draft.as_ref().unwrap().base.as_deref(), Some("/p"));
        app.picked(Ok(Some(Picked::Open("/p/fun".into()))));
        let req = app.create.take().unwrap();
        assert_eq!((req.dir.as_str(), req.group.as_str(), req.bind), ("/p/fun", "lab", true));
        // 其实已经有了(大小写不同):用已有的,不改它的默认路径
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
        press(&mut app, KeyCode::Char('a')); // 全部:fun[问 未读] work[跑 闲 错]
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.group.as_deref(), Some("work"));
        assert_eq!((titles(&app), at(&app)), (vec!["跑".to_string(), "错".into(), "闲".into()], "闲".to_string()));
        press(&mut app, KeyCode::Char('j')); // 组里换一个再回去,高亮跟着
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), at(&app).as_str()), (None, "错"));
        press(&mut app, KeyCode::Char('}')); // } 进的分组,W 也是回到全部
        press(&mut app, KeyCode::Char('W'));
        assert_eq!(app.group, None);
    }

    #[test]
    fn star_cycles_one_three_seven_days_then_all() {
        let mut app = fixture();
        assert_eq!(titles(&app), ["跑", "问", "闲"]);
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(titles(&app), ["跑", "错", "问", "闲"]); // 错一天多
        assert_eq!(active_tab(&mut app), "3 天");
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(titles(&app), ["未读", "跑", "错", "问", "闲"]); // 未读快六天
        assert_eq!(active_tab(&mut app), "7 天");
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(app.scope, Scope::All);
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((app.scope, app.days), (Scope::Recent, 1));
        // a 进全部再按回来,天数不丢;在开着的按 * 回到近一天
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.scope, app.days), (Scope::Recent, 3));
        assert_eq!(app.view_state().days, 3);
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((app.scope, app.days), (Scope::Recent, 1));
    }

    #[test]
    fn question_mark_shows_the_keys_from_the_top() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.reader_target().is_none()); // 不是会话,run 循环不去 tmux 抓
        // 手机上从底下升起、从最上面开始一栏往下翻;按用途分组的速查,不是 aca help keys 的原文
        shows(&reader_rows(&mut app, 60, 12), &["快捷键", "1/", "导航"]);
        // 桌面上居中的弹窗,三栏一屏放下
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("导航") && r.contains("新建时")), "{rows:#?}");
        // 在里面按键 = 查它:亮起说明它的行,不真的去做
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
        press(&mut app, KeyCode::Char('J')); // 不换会话
        press(&mut app, KeyCode::Enter); // 不进会话
        assert!(app.reader.is_some() && app.attach.is_none());
        assert_eq!(lit(&app), 1);
        // 不是快捷键的才说一声
        assert!(!reader_rows(&mut app, 150, 40).iter().any(|r| r.contains("不是快捷键")));
        press(&mut app, KeyCode::Char('U'));
        shows(&reader_rows(&mut app, 150, 40), &["「U」不是快捷键"]);
        // 手机上一栏往下翻:亮的那行不在这一屏就滚过去
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(lit(&app), 2);
        shows(&reader_rows(&mut app, 46, 12), &["挪分组"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
        // 底栏放不下所有键:从不常用的开始丢,? 和 ⏎ 永远在
        assert_eq!(reader_rows(&mut app, 46, 12)[11], "N 新建 │ ⏎ 打开 • i 待处理 │ / 搜索 • ? 快捷键");
    }

    #[test]
    fn plus_in_the_tool_menu_opens_the_accounts_page_ready_to_add() {
        let mut app = fixture().with_tools(|| Ok(vec![])).with_accounts(|adding| Box::new(FakePage(if adding { 7 } else { 0 })));
        app.tools_loader = || Ok(vec![tool("work", "claude", "work", Agent::Claude)]);
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 60, 14), &["+ 加账号"]);
        press(&mut app, KeyCode::Char('+'));
        // 菜单收起,进账号页(一开就是加账号);关了回列表,不回菜单、不弹设置页
        assert!(app.tool_menu.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 7"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && app.tool_menu.is_none() && app.settings.is_none());
        // 默认(快照、别的测试)没有账号页,不碰配置:+ 不动,菜单还在
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
        pin(&mut app, "跑"); // 选中「跑」
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 46, 12), &["选择工具", "1 work  ~/.work", "2 cx    ~/.cx", "⏎ 选定"]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j')); // 转一圈回到第一个
        press(&mut app, KeyCode::Esc);
        assert!(app.tool_menu.is_none() && app.new_draft.is_none());
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('2')); // 数字直接选
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.label.as_str(), d.text.as_str()), ("cx", "/p/work"));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.spec().as_str(), r.dir.as_str()), ("codex:cx", "/p/work"));
        // 起不了的那行(工具写的账号没登记):写原因,选了只提示、菜单还在
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
        // 菜单是空的:提示去哪改
        let mut empty = fixture();
        press(&mut empty, KeyCode::Char('n'));
        assert!(empty.tool_menu.is_none() && empty.msg.as_ref().unwrap().0.starts_with("工具菜单是空的"));
    }

    #[test]
    fn new_path_can_be_edited_or_cancelled() {
        let mut app = fixture();
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('N'));
        typ(&mut app, "q"); // 输入框里的 q 是字,不是退出
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
        press(&mut app, KeyCode::Enter); // 空路径不建,提示留在输入框上方
        assert!(app.create.is_none());
        assert_eq!(app.new_draft.as_ref().unwrap().hint, "路径是空的");
    }

    #[test]
    fn pin_moves_to_top_keeps_highlight_and_survives_refresh() {
        let mut app = fixture();
        assert_eq!(order(&app), ["问", "跑", "闲"]);
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["闲", "问", "跑"]); // 置顶的在「等你操作」上面
        assert_eq!(app.current().unwrap().title, "闲");
        app.set_sessions(sessions()); // 2 秒刷新,新数据里 pinned 都是 false
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
        v[3].state = State::Stopped; // 闲:停掉了
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        pin(&mut app, "错"); // 25 小时没动
        pin(&mut app, "闲");
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["错", "闲", "问", "跑"]);
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(order(&app), ["错", "问", "未读", "跑"]); // 停掉的不算开着的,置顶的也一样
    }

    #[test]
    fn w_shows_every_open_one_regardless_of_age() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        v.push(mk("退", "work", State::Exited, 10));
        v.push(mk("关", "work", State::Crashed, 10));
        let mut app = App::new(String::new(), v);
        assert_eq!(titles(&app), ["停", "关", "跑", "退", "问", "闲"]); // 近一天:未读、错都超过一天被藏,问永远在
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["未读", "跑", "错", "问", "闲"]); // 空闲也在,没在跑的(■ 和关掉的 ✕)不在
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["停", "关", "跑", "退", "问", "闲"]);
    }

    #[test]
    fn w_and_a_go_back_to_where_they_came_from() {
        let mut app = fixture();
        let scope = |app: &App| (app.scope, app.days);
        // 全部 → w → w:回到全部,不是近几天
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(scope(&app), (Scope::All, 1));
        // 近 7 天 → w → w:还是近 7 天
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(scope(&app), (Scope::Recent, 7));
        // 开着的 → a → a:回开着的;再按 w 退出开着的,回不到更早的就回近几天
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(scope(&app), (Scope::Working, 7));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(scope(&app), (Scope::Recent, 7));
        // * 轮到的全部,a 退回近几天;关掉再开也记得从哪来的
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(scope(&app), (Scope::All, 7));
        press(&mut app, KeyCode::Char('w'));
        let mut app = fixture().restore(app.view_state());
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(scope(&app), (Scope::All, 7));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(scope(&app), (Scope::Recent, 7));
    }

    #[test]
    fn status_keys_narrow_the_range_and_toggle_off() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(titles(&app), ["跑"]);
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(titles(&app), ["跑", "问", "闲"]);
        // 近一天里的在等你:待确认永远在,未读超过一天被藏,最后写还有一个更早的;* 放宽时状态还在
        press(&mut app, KeyCode::Char('@'));
        assert_eq!((titles(&app), app.hidden), (vec!["问".to_string()], 1));
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('*'));
        assert_eq!((titles(&app), app.hidden), (vec!["未读".to_string(), "问".to_string()], 0));
        // 一次一种:换一个键就换过去
        press(&mut app, KeyCode::Char('&'));
        assert_eq!(titles(&app), ["错"]);
        press(&mut app, KeyCode::Char('#'));
        assert_eq!(titles(&app), ["闲"]);
        // Esc 先清状态,再按才退出
        press(&mut app, KeyCode::Esc);
        assert_eq!(titles(&app).len(), 5);
        assert!(!app.quit);
        press(&mut app, KeyCode::Esc);
        assert!(app.quit);
    }

    #[test]
    fn an_empty_status_stays_empty_and_counts_only_its_own_older_ones() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('&')); // 「错」一天多没动了
        assert!(app.visible().0.is_empty() && !app.fell_back);
        assert_eq!(app.hidden, 1); // 只数出错的,不是另外藏着的「未读」
        shows(&reader_rows(&mut app, 46, 20), &["没有出错的 · 还有 1 个更早的 · * 放宽"]);
        press(&mut app, KeyCode::Char('*'));
        press(&mut app, KeyCode::Char('*'));
        assert_eq!(titles(&app), ["错"]);
        press(&mut app, KeyCode::Char('^'));
        shows(&reader_rows(&mut app, 46, 20), &["没有停着的 · ^ 返回"]);
    }

    #[test]
    fn a_crashed_one_counts_as_not_running_not_as_an_error() {
        let mut v = sessions();
        v[3].state = State::Crashed; // 闲:上一轮出错收场,tmux 里也没有了
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('&')); // 只有还开着、卡在出错上的
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["错".to_string()], "✕ 1 出错".to_string()));
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((titles(&app), lit_counts(&mut app, 120)), (vec!["闲".to_string()], "■ 1 没在跑".to_string()));
        // 挑着 ^ 按 w:状态让位,开着的里没有它
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, titles(&app)), (None, ["未读", "跑", "错", "问"].map(String::from).to_vec()));
    }

    #[test]
    fn status_inside_working_narrows_it_or_leaves_it() {
        let mut v = sessions();
        v.push(mk("停", "work", State::Stopped, 10));
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('!'));
        assert_eq!((titles(&app), app.scope), (vec!["跑".to_string()], Scope::Working));
        press(&mut app, KeyCode::Char('#')); // 空闲也是开着的
        assert_eq!((titles(&app), app.scope), (vec!["闲".to_string()], Scope::Working));
        // 开着的里没有 ^ 那些:退出开着的,回到进来之前的「全部」
        press(&mut app, KeyCode::Char('^'));
        assert_eq!((titles(&app), app.scope), (vec!["停".to_string()], Scope::All));
        // 挑着 ^ 按 w:状态让位
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.status, titles(&app).len()), (None, 5));
    }

    #[test]
    fn a_shows_everything_at_once_and_clears_the_status() {
        let mut app = fixture();
        let now = |app: &App| (app.scope, app.status, titles(app).len());
        // 近一天挑着 !:a 一下就是全部、不挑状态;再按回近一天
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::All, None, 5));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::Recent, None, 3));
        // 已经在全部、挑着 @:a 只清状态,不跳回去
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('@'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::All, None, 5));
        // 开着的里挑着 !:a 进全部,再按回开着的
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::All, None, 5));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(now(&app), (Scope::Working, None, 5));
    }

    #[test]
    fn search_ignores_the_status_and_it_is_not_remembered() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Char('/'));
        typ(&mut app, "闲");
        press(&mut app, KeyCode::Enter);
        assert_eq!(titles(&app), ["闲"]);
        assert_eq!(lit_counts(&mut app, 120), ""); // 搜着的时候顶栏也不铺
        press(&mut app, KeyCode::Esc); // 先清搜索词,状态还在
        assert_eq!(titles(&app), ["跑"]);
        let app = fixture().restore(app.view_state());
        assert_eq!((app.status, titles(&app)), (None, vec!["跑".to_string(), "问".to_string(), "闲".to_string()]));
    }

    #[test]
    fn status_keeps_families_and_parents_together() {
        // 会话出错一天多了,另开的窗口里在跑:整家留下
        let mut app = with_windows();
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(order(&app), ["跑", "错", "构建"]);
        // 子会话在跑,五天没动的父会话也列出来;父会话自己不带出子会话
        let mut app = with_children(5 * 86400);
        press(&mut app, KeyCode::Char('!'));
        assert_eq!(order(&app), ["跑", "闲", "子跑", "zsh"]);
    }

    /// 顶栏上铺着选中底色的字(挑中的状态那几格)
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
        assert_eq!(lit_counts(&mut app, 120), "◐ 1 待确认 ◐ 1 未读");
        // 空闲、■ 平时不数,挑了才列,0 个也列
        assert!(!reader_rows(&mut app, 120, 20)[0].contains("空闲"));
        press(&mut app, KeyCode::Char('#'));
        assert_eq!(lit_counts(&mut app, 120), "○ 1 空闲");
        press(&mut app, KeyCode::Char('^'));
        assert_eq!(lit_counts(&mut app, 120), "■ 0 没在跑");
        // 手机上计数写短,照样铺
        assert_eq!(lit_counts(&mut app, 46), "■0");
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
        press(&mut app, KeyCode::Char('}')); // 和分区一样 fun 在前,虽然 work 会话多
        assert_eq!(titles(&app), ["问"]);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["跑", "闲"]);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["跑", "问", "闲"]); // 回到全部分组
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(titles(&app), ["跑", "闲"]); // 往回转到最后一个分组 work
    }

    #[test]
    fn tab_lists_the_groups_and_a_letter_or_enter_switches() {
        let mut app = App::new(String::new(), life_and_work());
        keys(&mut app, "⇥");
        assert_eq!(app.group_rows(), [(' ', None), ('f', Some("fun".into())), ('w', Some("work".into()))]);
        assert_eq!(app.group_menu, Some(0)); // 没换过:停在现在这个(全部)
        let rows = reader_rows(&mut app, 80, 24);
        shows(&rows, &["分组", "␣  全部", "f  fun", "w  work", "⏎ 字母 切换"]);
        keys(&mut app, "w");
        assert_eq!((app.group.as_deref(), app.group_menu, titles(&app)), (Some("work"), None, vec!["工作".to_string()]));
        keys(&mut app, "⇥⏎"); // 停在上一个看的(全部):Tab ⏎ = 换回去
        assert_eq!(app.group, None);
        keys(&mut app, "⇥⏎");
        assert_eq!(app.group.as_deref(), Some("work"));
        keys(&mut app, "⇥ "); // 空格 = 全部
        assert_eq!(app.group, None);
        keys(&mut app, "⇥↑⏎"); // 停在 work,往上一个是 fun
        assert_eq!(app.group.as_deref(), Some("fun"));
        keys(&mut app, "⇥z"); // 没有的字母:不动(高亮停在上一个看的:全部)
        assert_eq!((app.group_menu, app.group.as_deref()), (Some(0), Some("fun")));
        keys(&mut app, "⇥"); // 再按 Tab 关
        keys(&mut app, "⇥q⇥⎋"); // q / Esc 也是关,不退出
        assert!(app.group_menu.is_none() && !app.quit && app.group.as_deref() == Some("fun"));
        keys(&mut app, "}"); // { } 照旧一格一格换
        assert_eq!(app.group.as_deref(), Some("work"));
    }

    #[test]
    fn the_group_list_keeps_to_this_workspace_and_letters_come_from_names() {
        let mut app = fun_in_h();
        assert_eq!(app.group_rows(), [(' ', None), ('w', Some("work".into()))]); // fun 整组在 h:1 里不列
        keys(&mut app, "qh");
        assert_eq!(app.group_rows(), [(' ', None), ('f', Some("fun".into()))]);
        let gs = ["work", "web", "-", "ops/qa", "中文", "Wq"].map(String::from);
        assert_eq!(group_keys(&gs), ['w', 'e', 't', 'a', 's', 'd']); // 撞了往后找;「其他 tmux 会话」是 t;q 不给;没有字母的按跳转字母表
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
        tap(&mut app, x + 1, y + 5); // 开着时点哪都是关
        assert!(app.group_menu.is_none());
        tap(&mut app, x + w + 3, y); // 标签外面不开
        assert!(app.group_menu.is_none());
    }

    #[test]
    fn group_and_working_combine() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('}'));
        press(&mut app, KeyCode::Char('}'));
        press(&mut app, KeyCode::Char('w'));
        assert_eq!(titles(&app), ["跑", "错", "闲"]); // 只有 work 的,空闲也是开着的
    }

    #[test]
    fn rows_stay_put_when_states_and_activity_change() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        let mut v = sessions();
        v[0].state = State::Idle; // 问 答完了
        v[3].state = State::Ask; // 闲 开始问
        v[3].idle_for = 0;
        v[2].state = State::Exited; // 跑 退出了
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
                app.mark_seen(n); // 看完回来
            }
            name
        };
        assert_eq!(next(&mut app).as_deref(), Some("新读"));
        assert_eq!(app.current().unwrap().title, "新读"); // 回来时高亮停在刚看的那个
        assert_eq!(next(&mut app).as_deref(), Some("中读"));
        assert_eq!(next(&mut app).as_deref(), Some("旧问"));
        assert_eq!(next(&mut app), None); // 在跑的、出错的不算;都看过了就不进
        let mut v = v;
        v[1].active_at = epoch() + 10; // 新读 又有了新输出
        app.set_sessions(v);
        assert_eq!(next(&mut app).as_deref(), Some("新读"));
    }

    #[test]
    fn i_only_looks_at_what_is_shown() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('}'));
        press(&mut app, KeyCode::Char('}')); // work 分组里没有在等你的(未读在 fun,还超过一天)
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Char('}')); // 回到全部分组:未读超过一天被「近一天」藏了,只剩问
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
        app.set_sessions(v.clone()); // 刷新:采集还说它跑完了没看,照样算已读
        assert_eq!(st(&app, "未读"), State::Idle);
        press(&mut app, KeyCode::Char('i')); // i 也不进了
        assert!(app.attach.is_none());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('u'));
        app.set_sessions(v.clone());
        assert_eq!(st(&app, "闲"), State::Unread);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        app.mark_seen("闲"); // 进去看过就不算未读了
        app.set_sessions(v.clone());
        assert_eq!(st(&app, "闲"), State::Idle);
        let mut v2 = v.clone();
        v2[0].active_at = epoch() + 10; // 又跑了一轮、跑完了
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
        assert_eq!(said(&app), "没有能撤销的");
        let st = |app: &App, t: &str| app.all.iter().find(|s| s.title == t).unwrap().state;
        let pinned = |app: &App| app.all.iter().filter(|s| s.pinned).map(|s| s.title.clone()).collect::<Vec<_>>();
        for t in ["闲", "跑", "错"] {
            pin(&mut app, t);
        }
        pin(&mut app, "跑"); // 取消中间那个
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
        // 倒着一步步撤
        ctrl(&mut app, 'z');
        assert_eq!((app.notes.get("id-闲"), said(&app)), (None, "已撤销改便签「闲」"));
        ctrl(&mut app, 'z');
        assert_eq!((space(&app, "问"), said(&app)), ('1', "已撤销 「问」 → w"));
        ctrl(&mut app, 'z');
        assert_eq!((st(&app, "未读"), said(&app)), (State::Unread, "已撤销标成已读「未读」"));
        assert_eq!(app.current().unwrap().title, "未读"); // 高亮停在撤的那个
        ctrl(&mut app, 'z');
        assert_eq!(pinned(&app), ["闲", "跑", "错"]); // 回到原来的位置
        assert_eq!(said(&app), "已撤销取消置顶「跑」");
        for _ in 0..3 {
            ctrl(&mut app, 'z');
        }
        assert!(pinned(&app).is_empty());
        assert_eq!(said(&app), "已撤销置顶「闲」");
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有能撤销的");
        // 没改的便签不记
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有能撤销的");
    }

    #[test]
    fn d_hides_the_session_until_ctrl_z_brings_it_back() {
        let mut app = fixture();
        select(&mut app, "跑");
        keys(&mut app, "Qwqw"); // 删之前记在 w
        select(&mut app, "跑");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Remove { t: target("跑"), pane: None });
        // 还在归档(这步还没记上):不越过它去撤 Q
        ctrl(&mut app, 'z');
        assert!(said(&app).starts_with("还在忙"), "{}", said(&app));
        app.job_done(&job, Outcome::Trashed { msg: "已删除「跑」 · ^Z 撤回".into(), was: false }, sessions());
        assert_eq!(said(&app), "已删除「跑」 · ^Z 撤回");
        app.set_sessions(sessions()); // 还登记着(归档了),不列
        assert!(!app.all.iter().any(|s| s.title == "跑"));
        // 等着真删的时候,它的工作区记录不被顺手清掉
        keys(&mut app, "q1");
        select(&mut app, "闲");
        keys(&mut app, "Qw");
        assert_eq!(app.spaces.of("id-跑"), Some('w'));
        ctrl(&mut app, 'z'); // 先撤 Q
        assert_eq!(said(&app), "已撤销 「闲」 → w");
        ctrl(&mut app, 'z');
        let want = Job::Revive { t: target("跑"), unarchive: true, start: true };
        assert_eq!(app.job.as_ref(), Some(&want));
        let job = app.take_job().unwrap();
        ctrl(&mut app, 'z');
        assert!(said(&app).starts_with("还在忙"), "{}", said(&app));
        app.job_done(&job, Outcome::Done("已撤销删除「跑」".into()), sessions());
        assert!(!app.trash.has("default", "id-跑"));
        assert_eq!(said(&app), "已撤销删除「跑」");
        keys(&mut app, "qw");
        assert_eq!(app.current().unwrap().title, "跑"); // 回到原来的工作区
        // 撤回失败:这步放回去,还在等着真删,能再按 ^Z
        keys(&mut app, "q1");
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Trashed { msg: String::new(), was: true }, sessions());
        ctrl(&mut app, 'z');
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Revive { unarchive: false, start: true, .. }));
        app.job_done(&job, Outcome::Failed("撤回失败:x".into()), sessions());
        assert_eq!(said(&app), "撤回失败:x");
        assert!(app.trash.has("default", "id-闲") && !app.all.iter().any(|s| s.title == "闲"));
        ctrl(&mut app, 'z');
        assert_eq!(app.job, Some(job));
    }

    #[test]
    fn closed_panes_cannot_come_back_and_ctrl_z_says_so_first() {
        let mut app = fixture();
        pin(&mut app, "闲");
        let job = Job::Remove { t: Target { id: String::new(), ..target("闲") }, pane: Some("%3".into()) };
        app.job_done(&job, Outcome::Done("已关闭「闲」".into()), sessions());
        assert_eq!(said(&app), "已关闭「闲」");
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "「闲」撤不回");
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
        // m:挪回原来的组
        let j = undo(
            &mut app,
            Job::Move { t: target("闲"), from: "work".into(), to: "fun".into(), groups: vec![] },
            with(&|s| s.group = "fun".into()),
            sessions(),
        );
        assert_eq!(j, Job::Move { t: target("闲"), from: "fun".into(), to: "work".into(), groups: vec![] });
        assert_eq!(said(&app), "已撤销挪分组:「闲」回到 work");
        // r:改回去;原来没锁的解锁
        let renamed = || with(&|s| (s.title, s.locked) = ("新名".into(), true));
        let j = undo(&mut app, Job::Rename { t: target("闲"), to: "新名".into(), unlock: false }, renamed(), sessions());
        assert_eq!(j, Job::Rename { t: Target { title: "新名".into(), ..target("闲") }, to: "闲".into(), unlock: true });
        assert_eq!(said(&app), "已撤销改名:回到「闲」");
        app.set_sessions(with(&|s| s.locked = true));
        let j = undo(&mut app, Job::Rename { t: target("闲"), to: "新名".into(), unlock: false }, renamed(), sessions());
        assert!(matches!(j, Job::Rename { unlock: false, .. }));
        // D:再启动;已经又跑起来了就不用
        let stopped = || with(&|s| s.state = State::Stopped);
        let j = undo(&mut app, Job::Stop { t: target("闲") }, stopped(), sessions());
        assert_eq!(j, Job::Revive { t: target("闲"), unarchive: false, start: true });
        assert_eq!(said(&app), "做完了");
        app.job_done(&Job::Stop { t: target("闲") }, Outcome::Done(String::new()), sessions());
        ctrl(&mut app, 'z');
        assert!(app.job.is_none());
        assert_eq!(said(&app), "「闲」已经在跑了");
        // M:反过来挂卸
        let mcp = Job::Mcp { t: target("闲"), dir: "/p".into(), on: vec!["a".into()], off: vec!["b".into()] };
        let j = undo(&mut app, mcp, sessions(), sessions());
        assert_eq!(j, Job::Mcp { t: target("闲"), dir: "/p".into(), on: vec!["b".into()], off: vec!["a".into()] });
        // 没做成的、重启不记
        app.job_done(
            &Job::Move { t: target("闲"), from: "work".into(), to: "fun".into(), groups: vec![] },
            Outcome::Failed("x".into()),
            sessions(),
        );
        app.job_done(&Job::Restart { t: target("闲"), force: false }, Outcome::Done(String::new()), sessions());
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "没有能撤销的");
        // 会话不在了
        app.job_done(&Job::Stop { t: target("闲") }, Outcome::Done(String::new()), stopped());
        app.set_sessions(sessions().into_iter().filter(|s| s.title != "闲").collect());
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "「闲」不在了");
    }

    #[test]
    fn children_of_a_deleted_parent_stay_listed() {
        let mut app = with_children(10);
        press(&mut app, KeyCode::Char('a'));
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        let v = app.all.clone();
        app.job_done(&job, Outcome::Trashed { msg: String::new(), was: false }, v);
        let t = titles(&app);
        assert!(!t.contains(&"闲".to_string()) && t.contains(&"子跑".to_string()) && t.contains(&"子旧".to_string()), "{t:?}");
        let rows = reader_rows(&mut app, 110, 20);
        assert!(rows.iter().any(|r| r.contains("子跑")), "{rows:#?}");
    }

    #[test]
    fn a_delete_pushed_out_of_the_ten_steps_is_deleted_for_real() {
        let mut app = fixture();
        select(&mut app, "闲");
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Trashed { msg: String::new(), was: false }, sessions());
        for _ in 0..undo::KEEP - 1 {
            pin(&mut app, "跑");
        }
        assert!(!app.purge);
        pin(&mut app, "跑");
        assert!(std::mem::take(&mut app.purge));
        assert_eq!(app.undo.len(), undo::KEEP);
        assert!(!app.undo.iter().any(|u| matches!(u, Undo::Remove { .. })));
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
        // 子会话:自己没分过就跟着父会话
        v.push(Session { up: Up::Under("生活".into(), "生活".into()), ..mk("子", "fun", State::Idle, 10) });
        v
    }

    #[test]
    fn d_hides_the_session_with_its_windows_and_leaves_its_sub_sessions_orphaned() {
        let mut app = App::new(String::new(), life_and_work());
        select(&mut app, "生活");
        keys(&mut app, "dy");
        assert!(matches!(app.take_job(), Some(Job::Remove { pane: None, .. })));
        // 窗口跟着会话一起藏;子会话还在,照删完的样子顶格
        assert_eq!(titles(&app), ["子", "工作", "生活二", "私"]);
        assert_eq!(app.all.iter().find(|s| s.title == "子").map(|s| &s.up), Some(&Up::Gone));
        app.set_sessions(life_and_work());
        assert_eq!(titles(&app), ["子", "工作", "生活二", "私"]);
    }

    #[test]
    fn q_switches_capital_q_sends_brackets_cycle_and_h_stays_hidden() {
        let v = life_and_work();
        let mut app = App::new(String::new(), v.clone());
        select(&mut app, "zsh"); // 窗口上按 = 送它所在的会话
        keys(&mut app, "Ql");
        assert_eq!(titles(&app), ["工作", "生活二", "私"]); // 窗口、子会话跟着走了
        select(&mut app, "私");
        keys(&mut app, "Qh");
        assert_eq!(titles(&app), ["工作", "生活二"]);
        assert!(!top_bar(&app, 80).to_string().contains("未读")); // 别的工作区不数
        press(&mut app, KeyCode::Char('i')); // 也不往里跳
        assert!(app.attach.is_none());
        keys(&mut app, "/生活");
        assert_eq!(titles(&app), ["生活二"]); // 也搜不到
        keys(&mut app, "⎋");
        app.set_sessions(v.clone()); // 刷新后照旧
        assert_eq!(titles(&app), ["工作", "生活二"]);
        keys(&mut app, "]"); // h 跳过:只有 1 和 l
        assert_eq!((app.space, titles(&app)), ('l', vec!["zsh".to_string(), "子".into(), "生活".into()]));
        assert!(top_bar(&app, 80).to_string().contains(" l ") && top_bar(&app, 80).to_string().contains("1 未读"));
        keys(&mut app, "]");
        assert_eq!(app.space, '1');
        keys(&mut app, "[");
        assert_eq!(app.space, 'l');
        keys(&mut app, "qh");
        assert_eq!(titles(&app), ["私"]);
        keys(&mut app, "]"); // 从 h 出来接着往后
        assert_eq!(app.space, 'l');
        keys(&mut app, "q1");
        assert_eq!(at(&app), "生活二"); // 回到 1:高亮停在离开时的那个
        keys(&mut app, "q⎋"); // 取消,不退出
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
        assert_eq!(titles(&app), ["工作", "生活二", "私"]); // 子会话、它的子会话都跟着走了
        keys(&mut app, "ql");
        assert_eq!(titles(&app), ["zsh", "子", "孙", "生活"]);
        // 中间那个自己分过:留在自己那边,它下面的跟着它,不跟再上面的
        select(&mut app, "子");
        keys(&mut app, "Qh");
        assert_eq!(titles(&app), ["zsh", "生活"]);
        select(&mut app, "生活");
        keys(&mut app, "Q1");
        keys(&mut app, "q1");
        assert_eq!(titles(&app), ["zsh", "工作", "生活", "生活二", "私"]);
        keys(&mut app, "qh");
        assert_eq!(titles(&app), ["子", "孙"]);
        app.set_sessions(v); // 刷新后照旧
        assert_eq!(titles(&app), ["子", "孙"]);
    }

    #[test]
    fn backtick_goes_back_to_the_workspace_before_h_included() {
        let mut app = App::new(String::new(), life_and_work());
        keys(&mut app, "`");
        assert_eq!((app.space, app.msg.as_ref().unwrap().0.as_str()), ('1', "还没换过工作区"));
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        select(&mut app, "私");
        keys(&mut app, "Qh");
        select(&mut app, "生活二");
        keys(&mut app, "ql`");
        assert_eq!((app.space, at(&app)), ('1', "生活二".into())); // 高亮也回到离开时的
        keys(&mut app, "`");
        assert_eq!(app.space, 'l');
        keys(&mut app, "qh`"); // h 也一样
        assert_eq!(app.space, 'l');
        keys(&mut app, "`");
        assert_eq!(titles(&app), ["私"]);
        keys(&mut app, "]`"); // [ ] 换的也算
        assert_eq!(app.space, 'h');
        // 记在视图里:重开 aca(弹窗每次都是新开的)接着用
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
        keys(&mut app, "o"); // 折起 fun,高亮在折叠行上
        assert!(app.on_fold_row());
        keys(&mut app, "Q2");
        assert_eq!(titles(&app), ["工作"]);
        keys(&mut app, "q2o"); // 折叠按分组记,到这边也折着
        assert_eq!(titles(&app), ["zsh", "子", "生活", "生活二", "私"]);
        app.adopt("id-新"); // 在 2 里新建的
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
        assert!(top_bar(&app, 80).to_string().contains(" 会话 ")); // 只用着 1:和以前一样
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        select(&mut app, "私");
        keys(&mut app, "Qh");
        keys(&mut app, "S");
        assert_eq!(app.space_list(), ['1', 'l']); // h 不列
        assert_eq!(app.space_menu, Some(0));
        keys(&mut app, "↓");
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        keys(&mut app, "生活⏎");
        assert_eq!(app.spaces.name('l'), "生活");
        keys(&mut app, "Sl"); // 直接按键切过去
        assert_eq!(app.space, 'l');
        assert!(top_bar(&app, 80).to_string().contains(" 生活 "));
        keys(&mut app, "Sh"); // 不列的也能直接按
        assert_eq!(titles(&app), ["私"]);
    }

    #[test]
    fn the_theme_list_previews_each_theme_on_the_whole_screen_and_esc_puts_it_back() {
        let mut app = App::new(String::new(), life_and_work());
        let latte = theme::pal().accent;
        // 不占单键(C 是挑一样复制)
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
        // 底下不变灰(底下就是预览),弹窗贴在左下角
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
        // ⏎ 用它
        app.open_themes();
        keys(&mut app, "k⏎");
        assert!(app.theme_menu.is_none());
        assert_eq!(theme::pal().accent, mocha);
        assert_eq!(app.msg.as_ref().unwrap().0, "配色:mocha");
    }

    fn space(app: &App, title: &str) -> char {
        app.all.iter().find(|s| s.title == title && s.sub.is_none()).unwrap().space
    }

    /// fun 整组(折叠行上)送进 h
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
        assert_eq!(app.spaces.of("id-生活"), None); // 跟着组就行,不用一个个记
        // 不经界面(终端里 aca new、插件)往 fun 新开的:跟着组进 h,不在 1 露出来
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
        keys(&mut app, "Q1"); // 组在 h:送回 1 要单独记着,不然又跟组回去
        assert_eq!(app.spaces.of("id-私"), Some('1'));
        app.set_sessions(life_and_work());
        assert_eq!(space(&app, "私"), '1');
        // 在 1 把 fun(这边只有「私」)整组送去 2:组改记 2,留在 h 的那几个照样在 h
        keys(&mut app, "q1");
        select(&mut app, "私");
        keys(&mut app, "oQ2");
        assert_eq!(app.spaces.group_of("default", "fun"), Some('2'));
        assert_eq!((space(&app, "私"), space(&app, "生活"), space(&app, "生活二"), space(&app, "子")), ('2', 'h', 'h', 'h'));
        assert_eq!(app.spaces.of("id-私"), None); // 跟组了
        // 送回 1:组不记了
        keys(&mut app, "q2");
        select(&mut app, "私");
        keys(&mut app, "Q1");
        assert_eq!((space(&app, "私"), space(&app, "生活"), app.spaces.group_of("default", "fun")), ('1', 'h', None));
    }

    /// 登记表里的分组(假的):列表里有会话的两个,再加两个没会话的
    fn deck_groups() -> Vec<groups::Row> {
        let row = |path: &str, live| groups::Row { profile: "default".into(), path: path.into(), live, archived: 0 };
        vec![row("fun", 4), row("work", 1), row("empty", 0), row("misc", 0)]
    }

    fn delete_group(group: &str, force: bool) -> Job {
        Job::Group(groups::Act::Delete { profile: "default".into(), group: group.into(), force })
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
        // 撞上已有的(大小写不同也算)、挪到自己下面:不往下走
        ctrl(&mut app, 'u');
        keys(&mut app, "other⏎");
        assert_eq!(prompt_of(&app), ("other".into(), "已经有 Other 这个分组了".into()));
        ctrl(&mut app, 'u');
        keys(&mut app, "work/x⏎");
        assert_eq!(prompt_of(&app).1, "work 不能挪到它自己下面");
        // 空格换成 -;接着填默认目录,原来的填好
        ctrl(&mut app, 'u');
        keys(&mut app, "my job⏎");
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::GroupHome { to, .. }) if to == "my-job"));
        assert_eq!(prompt_of(&app).0, "/w/work");
        // 不是目录、是别的组的默认目录:不收
        ctrl(&mut app, 'u');
        keys(&mut app, "/no/such/dir⏎");
        assert_eq!(prompt_of(&app).1, "/no/such/dir 不是目录");
        ctrl(&mut app, 'u');
        keys(&mut app, "/⏎");
        assert_eq!(prompt_of(&app).1, "这个目录已经是分组 Other 的默认目录了");
        // 原来那个目录(没了也行)直接回车:只改名
        ctrl(&mut app, 'u');
        keys(&mut app, "/w/work/⏎");
        let job = app.take_job().unwrap();
        assert_eq!(job, edit_group("work", "my-job", None));
        // 做完:在看的组、折着的(连子分组)跟过去,记一步撤销
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
        assert_eq!(app.msg.as_ref().unwrap().0, "已撤销改分组:my-job 改回 work");
        assert_eq!((app.group.as_deref(), app.folded.contains("work/sub")), (Some("work"), true));
        // 只改目录:清空 = 不记;撤回把原来的记回去
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
        // 什么都没改
        app.job_done(&back, Outcome::Done(String::new()), moved("work"));
        jump_to_group(&mut app, "work");
        keys(&mut app, "r⏎⏎");
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "分组没改");
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
        assert_eq!(app.msg.as_ref().unwrap().0, "「未分组」不是登记的分组:要放进分组用 m");
        select(&mut app, "裸");
        keys(&mut app, "or");
        assert_eq!(app.msg.as_ref().unwrap().0, "这些是普通 tmux 会话,没有分组可改");
        // 会话上的 r 还是改会话的标题
        select(&mut app, "生活");
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::Rename(_))));
    }

    #[test]
    fn d_on_a_group_opens_the_groups_page_and_a_second_d_deletes_it() {
        let mut app = App::new(String::new(), life_and_work()).with_groups(deck_groups);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('d'));
        assert!(app.modal_open() && app.confirm.is_none() && app.job.is_none());
        // 没会话的组也列着;高亮在 work 上,写着删掉它会怎样
        shows(&reader_rows(&mut app, 100, 30), &["empty", "再按 d 删掉 work", "里面 1 个会话挪到 未分组", "d 删掉"]);
        shows(&reader_rows(&mut app, 46, 30), &["empty", "再按 d 删掉 work", "里面 1 个会话挪到 未分组"]);
        press(&mut app, KeyCode::Char('d'));
        let job = app.take_job().unwrap();
        assert_eq!(job, delete_group("work", true));
        shows(&reader_rows(&mut app, 46, 30), &["│ 正在删分组 work…"]);
        // 删完:会话在上级分组里照样列着,页面留着(接着删别的)
        let mut v = life_and_work();
        v[0].group = "未分组".into();
        app.job_done(&job, Outcome::Done("已删分组 work · 1 个会话挪到 未分组".into()), v);
        assert_eq!(app.msg.as_ref().unwrap().0, "已删分组 work · 1 个会话挪到 未分组");
        assert!(app.modal_open() && app.all.iter().any(|s| s.title == "工作" && s.group == "未分组"));
        // 挪到空的那个上:按两次,不带 --force
        keys(&mut app, "jdd");
        assert_eq!(app.take_job(), Some(delete_group("empty", false)));
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && !app.quit);
        // 折叠行上一样;普通 tmux 会话那一区没有分组可删
        let mut v = life_and_work();
        v.push(Session { profile: String::new(), deck_id: String::new(), ..mk("裸", "-", State::Idle, 10) });
        let mut app = App::new(String::new(), v).with_groups(deck_groups);
        select(&mut app, "生活");
        keys(&mut app, "od");
        shows(&reader_rows(&mut app, 100, 30), &["再按 d 删掉 fun", "里面 4 个会话挪到 未分组"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.msg.as_ref().unwrap().0, "没删");
        select(&mut app, "裸");
        keys(&mut app, "od");
        assert!(!app.modal_open());
        assert_eq!(app.msg.as_ref().unwrap().0, "这些是普通 tmux 会话,没有分组可删");
    }

    #[test]
    fn deleting_a_group_kept_in_h_leaves_its_sessions_there_and_forgets_the_group() {
        let mut app = fun_in_h().with_groups(deck_groups);
        // 人在 1:h 里的组不列
        app.msg = None;
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('d'));
        let rows = reader_rows(&mut app, 100, 30);
        assert!(rows.iter().any(|r| r.contains("empty")) && !rows.iter().any(|r| r.contains("fun")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        // 进 h 删 fun:会话要挪到 my-sessions(没记在 h),先一个个记住它们在 h
        keys(&mut app, "qh");
        jump_to_group(&mut app, "fun");
        keys(&mut app, "dd");
        let job = app.take_job().unwrap();
        assert_eq!(job, delete_group("fun", true));
        assert_eq!((app.spaces.of("id-生活"), app.spaces.of("id-生活二"), app.spaces.of("id-私")), (Some('h'), Some('h'), Some('h')));
        assert_eq!(app.spaces.of("id-子"), None); // 子会话跟着父会话,不用记
        let mut v = life_and_work();
        v.iter_mut().filter(|s| s.group == "fun").for_each(|s| s.group = "my-sessions".into());
        app.job_done(&job, Outcome::Done("已删分组 fun".into()), v.clone());
        assert_eq!(["生活", "生活二", "私", "子"].map(|t| space(&app, t)), ['h'; 4]);
        // 组的记录清了:以后再建一个 fun,不会又掉进 h
        assert_eq!(app.spaces.group_of("default", "fun"), None);
        v.push(mk("新的", "fun", State::Idle, 1));
        app.set_sessions(v);
        assert_eq!(space(&app, "新的"), '1');
        // 没删成的:组的记录留着
        let mut app = fun_in_h().with_groups(deck_groups);
        keys(&mut app, "qh");
        jump_to_group(&mut app, "fun");
        keys(&mut app, "dd");
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed("删分组失败:database is locked".into()), life_and_work());
        assert_eq!(app.spaces.group_of("default", "fun"), Some('h'));
        assert_eq!(space(&app, "生活"), 'h');
    }

    #[test]
    fn made_here_stays_here_even_if_its_group_is_kept_elsewhere() {
        let mut app = fun_in_h();
        // 列表里已经有它了
        let mut v = life_and_work();
        v.push(mk("新", "fun", State::Idle, 1));
        app.set_sessions(v.clone());
        app.adopt("id-新");
        app.apply_pins();
        assert_eq!((space(&app, "新"), app.spaces.of("id-新")), ('1', Some('1')));
        // 还没刷到它:先记上
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
        v[0].group = "fun".into(); // 挪完了
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
        // N 的 fzf:h 里的会话不在 1 的候选里露标题
        keys(&mut app, "q1");
        let mut app = app.with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), String::new())] });
        press(&mut app, KeyCode::Char('N'));
        let p = app.dir_pick_from(vec![("/p/fun".into(), Src::Known), ("/p/work".into(), Src::Known)], 40, 16).unwrap();
        assert!(p.rows[0].sessions.is_empty() && p.rows[0].mark == '◆', "{:?}", p.rows[0].sessions);
        assert!(p.rows[1].sessions.iter().any(|(_, t)| t == "工作"));
    }

    fn at(app: &App) -> String {
        app.current().unwrap().title.clone()
    }

    #[test]
    fn shift_j_k_jump_between_sections() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // 全部:fun[问 未读] work[跑 闲 错]
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "跑");
        press(&mut app, KeyCode::Char('J')); // 没有下一段 → 最后一个
        assert_eq!(at(&app), "错");
        press(&mut app, KeyCode::Char('K')); // 先回本段开头
        assert_eq!(at(&app), "跑");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
        pin(&mut app, "闲"); // 置顶块自成一段:[闲] fun[问 未读] work[跑 错]
        press(&mut app, KeyCode::Char('G')); // 到底 / 到顶,像 vim
        assert_eq!(at(&app), "错");
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(at(&app), "闲");
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "问");
        press(&mut app, KeyCode::Char('}')); // 只看 fun:不分区,J/K = 到底 / 到顶
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(at(&app), "未读");
        press(&mut app, KeyCode::Char('K'));
        assert_eq!(at(&app), "问");
    }

    #[test]
    fn shift_tab_goes_back_to_the_group_seen_before() {
        let mut app = fixture();
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.msg.as_ref().unwrap().0, "还没换过分组");
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(app.group.as_deref(), Some("work"));
        press(&mut app, KeyCode::BackTab); // 回到刚才的(全部),再按又回来
        assert_eq!(app.group, None);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.group.as_deref(), Some("work"));
    }

    #[test]
    fn h_folds_from_a_session_or_header_and_l_unfolds_then_steps_in() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // 全部:fun[问 未读] work[跑 闲 错]
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('h')); // 会话上 = 直接折起它的分组,高亮落在折叠行上
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[▸ work  3]"]);
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('h')); // 折叠行是最外层:不动,也不换分组
        assert_eq!((layout(&app).len(), app.group.as_deref()), (4, None));
        press(&mut app, KeyCode::Right); // → 同 l:展开,停在标题上
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        assert_eq!(app.group_row(), Some("work"));
        press(&mut app, KeyCode::Left); // 展开的标题上 ← / h 也是折起来
        assert!(app.on_fold_row());
        keys(&mut app, "ll"); // 展开、进到组里第一个
        assert_eq!((app.group_row(), at(&app)), (None, "跑".to_string()));
        press(&mut app, KeyCode::Char('l')); // 会话上不动(只有 h 折)
        assert_eq!((app.folds.len(), app.group.as_deref(), at(&app)), (0, None, "跑".to_string()));
        pin(&mut app, "跑"); // 置顶块不在分区里:h 不动
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.folds.len(), at(&app)), (0, "跑".to_string()));
    }

    #[test]
    fn h_from_a_window_or_sub_session_goes_to_its_session_and_out_of_a_single_group() {
        let mut v = sessions();
        let idle = v[3].clone(); // 闲
        v.push(sub_of(&idle, 2, false, "zsh", State::Idle, 20));
        let mut k = Session { up: Up::Under("闲".into(), "闲".into()), ..mk("子", "work", State::Idle, 10) };
        k.slot.1 = 10;
        v.push(k);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "zsh", "子", "错"]);
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Left); // ← 同 h:窗口 → 它所在的会话
        assert_eq!(at(&app), "闲");
        select(&mut app, "子");
        press(&mut app, KeyCode::Char('h')); // 子会话 → 父会话
        assert_eq!(at(&app), "闲");
        press(&mut app, KeyCode::Char('W')); // 只看 work:没有标题,h 回到全部分组,高亮不动
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
        assert_eq!(app.clip.take(), Some(Clip::Info { title: "跑".into(), text: "Path: /p/work".into() }));
        // C:挑一样复制;这一行没有对话 id,按 s 只说没有
        press(&mut app, KeyCode::Char('C'));
        assert!(app.modal_open());
        press(&mut app, KeyCode::Char('s'));
        assert!(app.modal_open() && app.clip.is_none());
        press(&mut app, KeyCode::Char('p'));
        assert_eq!((app.modal_open(), app.clip.take()), (false, Some(Clip::Text("/p/work".into()))));
        press(&mut app, KeyCode::Char('z')); // 全屏阅读里也能复制
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
            // 谁也不是别人的开头,否则按到一半就会误中
            assert!(h.iter().all(|x| h.iter().filter(|y| y.starts_with(x.as_str())).count() == 1));
        }
        let h = jump_hints(20);
        assert!(h.iter().any(|x| x.len() == 1) && h.iter().any(|x| x.len() == 2));
    }

    #[test]
    fn space_then_letter_enters_that_session() {
        let mut app = fixture(); // 近一天:[fun] 问 [work] 跑 闲 → 提示 a d f j s
        press(&mut app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), &Folds::new(), 0, false);
        let badges: Vec<&str> = rows.iter().filter(|r| r.pos.is_some()).map(|r| r.lines[0][0].0.as_str()).collect();
        assert_eq!(badges, ["d", "j", "s"]); // 编号的位置换成提示
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        assert!(app.jump.is_none());
        // Esc / 对不上的字母:退出,不进
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Esc);
        assert!(app.jump.is_none() && !app.quit);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('z'));
        assert!(app.jump.is_none() && app.attach.is_none());
        // 别的键:退出跳转并照常处理
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('3'));
        assert!(app.jump.is_none());
        assert_eq!(at(&app), "闲");
    }

    #[test]
    fn two_letter_hints_wait_for_the_second_letter() {
        let v: Vec<Session> = (0..20).map(|i| mk(&format!("会话{i:02}"), "work", State::Idle, 10)).collect();
        let mut app = App::new(String::new(), v);
        let hints = jump_hints(21); // 分组标题占第一个
        let (i, two) = hints.iter().enumerate().find(|(i, h)| *i > 0 && h.len() == 2).unwrap();
        let want = app.visible().0[i - 1].name.clone();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(two.chars().next().unwrap()));
        assert!(app.jump.is_some() && app.attach.is_none());
        press(&mut app, KeyCode::Char(two.chars().nth(1).unwrap()));
        assert_eq!(app.attach, Some(want));
    }

    /// 空格后按分组标题的字母:高亮停在标题上,不进会话
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
        assert_eq!(at(&app), "跑"); // 背后选的是组里第一个
        // 画出来:高亮在标题那行
        let mut t = Terminal::new(TestBackend::new(46, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let bg = |y: u16| buf[(4, y)].bg;
        // 第 0 行顶栏,1 [fun],2 问,3 [work],4 跑
        assert_eq!((bg(3), bg(4) == theme::pal().sel), (theme::pal().sel, false));
        // 刷新后还停在标题上
        app.set_sessions(sessions());
        assert_eq!(app.head.as_deref(), Some("work"));
        // 只看一个分组时没有标题可停
        press(&mut app, KeyCode::Char('W'));
        assert!(app.head.is_none());
        // 最上面那个组的标题也能停,不分区时没有标题的字母
        press(&mut app, KeyCode::Char('W'));
        jump_to_group(&mut app, "fun");
        assert_eq!(app.head.as_deref(), Some("fun"));
    }

    #[test]
    fn j_k_stop_on_group_headers_too() {
        let mut app = fixture(); // 近一天:[fun] 问 [work] 跑 闲
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
        press(&mut app, KeyCode::Char('2')); // 编号照常是会话
        assert_eq!(spot(&app), "跑");
        // 折起来的组只剩折叠行一格;展开后高亮还在组上
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        assert_eq!(walk(&mut app, 'k', 2), ["问", "[fun]"]);
        assert_eq!(walk(&mut app, 'j', 2), ["问", "跑"]);
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Enter);
        assert_eq!(spot(&app), "[work]");
        // J / K 照旧落在会话上
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
        // 还没读回来:只有路径
        assert_eq!(text(&group_pane(&sp, None, 60, 20)), ["/p/work", "组的默认路径"]);
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
        let t = text(&group_pane(&sp, Some(&g), 60, 20));
        assert_eq!(t[0], "/p/work  main  3 个文件 +12 −4");
        // 一栏按最宽的 Cargo.toml 10 格 + 2:60 格排 5 栏,目录在前
        assert_eq!(t[1..], ["组的默认路径", "", "src/        spec/       Cargo.toml  README.md   a", "2 个点开头的"]);
        // 放不下:最后一行数还有几个
        let t = text(&group_pane(&sp, Some(&g), 24, 5));
        assert_eq!(t[3..], ["src/        spec/", "还有 3 个 · 2 个点开头的"]);
        // 跟着会话的目录;目录不在了
        let sp = Spot { from: Some("跑".into()), ..sp };
        let gone = Glance { files: Some(diff::Files { err: Some("目录不在了".into()), ..Default::default() }), ..Default::default() };
        assert_eq!(text(&group_pane(&sp, Some(&gone), 60, 20)), ["/p/work", "「跑」的目录 · 组没设默认路径", "", "目录不在了"]);
    }

    #[test]
    fn on_a_group_the_preview_reads_and_shows_the_groups_dir() {
        let mut app = fixture();
        jump_to_group(&mut app, "work");
        let (key, w) = app.glance_request(false).unwrap();
        assert_eq!((key.as_str(), w.dir.as_str(), w.pid), ("group:work", "/p/work", 0));
        assert!(app.glance_request(false).is_none()); // 同一个组不重复读
        let files = diff::Files { dirs: vec!["src".into()], files: vec!["Cargo.toml".into()], ..Default::default() };
        app.set_glance(key, Glance { files: Some(files), ..Default::default() });
        // 桌面:右边的框里;手机:下面
        for (w, h) in [(100, 30), (46, 24)] {
            let rows = reader_rows(&mut app, w, h);
            assert!(rows.iter().any(|r| r.contains("▸ work")), "{rows:#?}");
            assert!(rows.iter().any(|r| r.contains("「跑」的目录")), "{rows:#?}");
            assert!(rows.iter().any(|r| r.contains("src/") && r.contains("Cargo.toml")), "{rows:#?}");
        }
        // 挪回会话上:读会话的
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.glance_request(false).map(|r| r.0).as_deref(), Some("跑"));
        // 折叠行也是组
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.glance_request(false).map(|r| r.0).as_deref(), Some("group:work"));
    }

    #[test]
    fn t_on_a_group_opens_a_terminal_in_the_groups_dir() {
        let mut v = sessions();
        v[3].dir = "/p/闲".into(); // work 里最近有动静的是「跑」
        let mut app = App::new(String::new(), v);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        let r = app.term_req.take().unwrap();
        assert_eq!((r.key.as_str(), r.family.as_str(), r.title.as_str(), r.w.dir.as_str()), ("group:work", "", "work", "/p/work"));
        assert_eq!(app.head.as_deref(), Some("work"));
        // 一样新:取列表里靠上的
        let mut v = sessions();
        (v[3].active_at, v[3].dir) = (v[2].active_at, "/p/闲".into());
        let mut t = App::new(String::new(), v);
        jump_to_group(&mut t, "work");
        press(&mut t, KeyCode::Char('t'));
        assert_eq!(t.term_req.take().unwrap().w.dir, "/p/work");
        // 开过就回到那个
        app.terms.insert(r.key.clone(), crate::term::Term::spawn("/", &[], 5, 20).unwrap());
        press(&mut app, KeyCode::Char('t'));
        assert!(app.term_req.is_none() && app.term_on.as_deref() == Some("group:work"));
        app.terms.clear();
        app.term_on = None;
        // 组设了默认路径(目录还在):用它
        let tmp = || std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints {
            groups: vec![("work".into(), std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string())],
        });
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().w.dir, tmp());
        // 默认路径不在了:退回会话的目录;worktree 用原仓库
        let mut v = sessions();
        (v[2].worktree, v[2].info) = (true, "Repo: /repo\nPath: /p/work".into()); // 跑
        let mut app = App::new(String::new(), v)
            .with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), "/没有这个目录".into())] });
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().w.dir, "/repo");
        // 折叠行也是分组
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().key, "group:work");
    }

    #[test]
    fn new_on_a_group_starts_in_the_groups_dir_and_group() {
        let mut v = sessions();
        v[2].tool = "codex".into(); // 跑:work 里最近有动静的
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
        // r 在分组上改的是分组(见 r_on_a_group_…)
        for c in ['p', 'e', 'u', 'z', 'T'] {
            app.msg = None;
            press(&mut app, KeyCode::Char(c));
            assert!(app.msg.as_ref().is_some_and(|m| m.0.starts_with("分组上不能")), "{c}: {:?}", app.msg);
            assert!(app.confirm.is_none() && app.prompt.is_none() && app.split.is_none() && app.reader.is_none(), "{c}");
            assert_eq!(app.head.as_deref(), Some("work"), "{c}");
        }
        assert!(app.pins.rank("跑").is_none());
        let before = order(&app);
        shift(&mut app, KeyCode::Down); // 分组上 ⇧↑ ⇧↓ 挪的是分组,work 已经在最下
        assert!(app.msg.as_ref().is_some_and(|m| m.0 == "分组已在最下") && before == order(&app));
        assert!(app.reorders.is_empty());
        // 回车 = 折起来,高亮落在折叠行上
        press(&mut app, KeyCode::Enter);
        assert!(app.on_fold_row() && app.head.is_none() && app.attach.is_none());
        press(&mut app, KeyCode::Enter);
        assert!(!app.on_fold_row() && app.attach.is_none());
    }

    #[test]
    fn account_column_sits_before_the_age_with_a_fixed_width() {
        let mut v = sessions();
        v[2].who = "team".into(); // 跑
        v[4].who = "alt".into(); // 错
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
        assert!(run.ends_with(" team  5s "), "{run}"); // 5s 右对齐到 3 格
        // 只剩没账号的会话在显示,列宽也不变(按全部会话算),标题不会伸缩
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["未读", "问"]);
        assert!(text(&app)[0].ends_with(&format!("{}11d ", " ".repeat(2 + 1 + 4 + 1))), "{:?}", text(&app)); // 标记列 + 空着的账号列
        // 太长的截断;一个有账号的都没有就不占列
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
                None => format!("[{}]", r.lines[0][1].0), // 标题行的分组名(置顶那块是「置顶」)
            })
            .collect()
    }

    #[test]
    fn o_folds_the_current_section_and_enter_unfolds() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // 全部
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        pin(&mut app, "闲");
        pin(&mut app, "闲"); // 选中闲(置顶又取消,位置不变)
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[▸ work  3]"]);
        assert!(app.on_fold_row()); // 高亮落在折叠行上
        press(&mut app, KeyCode::Char('z')); // 折叠行不能阅读
        assert!(app.reader.is_none());
        press(&mut app, KeyCode::Enter); // 回车 = 展开,不是进会话
        assert!(app.attach.is_none());
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('o')); // fun 里有个 !,折起来也看得到
        assert_eq!(layout(&app), ["[▸ fun  2 ◐1]", "[work]", "跑", "闲", "错"]);
        press(&mut app, KeyCode::Char('i')); // 在等你的都折起来了:不进
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Char('c')); // 折叠行上不能复制
        assert!(app.clip.is_none());
        press(&mut app, KeyCode::Char('o')); // 停在折叠行上 o = 展开
        assert_eq!(layout(&app)[0], "[fun]");
    }

    #[test]
    fn shift_o_folds_all_pins_stay_and_filter_unfolds() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(layout(&app), ["[置顶]", "跑", "[▸ fun  2 ◐1]", "[▸ work  2]"]); // 置顶的不在分区里
        let saved = app.view_state();
        assert_eq!(saved.folded, ["fun", "work"]);
        assert_eq!(fixture().restore(saved).folds.len(), 2); // 记住了
        typ(&mut app, "/错"); // 过滤时全展开,免得搜不到
        assert_eq!(order(&app), ["错"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(layout(&app).len(), 8);
        press(&mut app, KeyCode::Char('}')); // 只看一个分组:没有分区可折
        press(&mut app, KeyCode::Char('o'));
        assert!(app.folds.is_empty());
    }

    #[test]
    fn sections_get_a_header_each_and_pins_sit_above_them() {
        let mut app = fixture();
        assert_eq!(layout(&app), ["[fun]", "问", "[work]", "跑", "闲"]);
        // 标题:分组名后面跟这一区有几个
        let view = app.visible().0;
        let head: String =
            list_rows(&view, true, 30, None, app.fold_info(), 0, false)[2].lines[0].iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(head, " ▾ work  2");
        // 桌面上也是一个会话一行,但总写编号,组和组之间空一行(空行也是一项,选不中)
        let desk = list_rows(&view, true, 46, None, app.fold_info(), 0, true);
        assert_eq!(desk.iter().map(|r| r.lines.len()).collect::<Vec<_>>(), [1, 1, 1, 1, 1, 1]);
        assert!(desk[2].pos.is_none() && desk[2].lines[0].is_empty());
        assert_eq!(desk[1].lines[0][0].0, "  1 ");
        pin(&mut app, "闲");
        assert_eq!(layout(&app), ["[置顶]", "闲", "[fun]", "问", "[work]", "跑"]);
        pin(&mut app, "问"); // fun 只剩置顶的那个,空分区不画标题
        assert_eq!(layout(&app), ["[置顶]", "闲", "问", "[work]", "跑"]);
        press(&mut app, KeyCode::Char('}')); // 只看一个分组:不分区
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
        // 四周留白:上下各空一行、左右 3 格;顶栏下面、底栏上面各再空一行
        let hh = h as usize;
        assert!(rows[0].is_empty() && rows[1].starts_with("    会话 ") && rows[2].is_empty(), "{rows:#?}");
        assert!(rows[3].starts_with("    ▾ fun"), "{rows:#?}");
        assert!(rows[hh - 1].is_empty() && rows[hh - 2].contains("? 快捷键") && rows[hh - 3].is_empty(), "{rows:#?}");
        // 一个会话一行:账号紧跟在(最长的)标题后面,目录不写在列表里;在问你的下面挂一行它问的,和标题对齐
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        let col = |y: usize, t: &str| rows[y][..rows[y].find(t).unwrap()].width();
        let run = at("2 ● 跑");
        assert!(col(run, "team") - col(run, "跑") <= 8 + 3, "标题栏至少 8 格,后面是标记和空格:{:?}", rows[run]);
        assert!(!rows.iter().any(|r| r.contains("/p/somewhere")), "{rows:#?}");
        let (ask, q) = (at("1 ◐ 问"), at("Allow edit?"));
        assert_eq!((q, col(q, "Allow")), (ask + 1, col(ask, "问")));
        assert_eq!((at("▾ work"), at("3 ○ 闲")), (q + 2, run + 1));
        assert_eq!(run, q + 3);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer();
        // 预览的框是淡色的,框上的标题是深色
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
        // 第 0 行顶栏,1 [fun],2 问,3 [work],4 跑
        let click = |app: &mut App, row: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 5, row, modifiers: KeyModifiers::NONE })
        };
        click(&mut app, 4);
        assert_eq!(app.current().unwrap().title, "跑");
        click(&mut app, 3);
        assert_eq!((app.current().unwrap().title.as_str(), app.head.as_deref()), ("跑", Some("work")));
        click(&mut app, 4); // 高亮在标题上时点它下面的会话:选中,不进
        assert!(app.head.is_none() && app.attach.is_none());
        click(&mut app, 3);
        click(&mut app, 3); // 再点一下标题 = 折起来
        assert!(app.on_fold_row() && app.attach.is_none());
        press(&mut app, KeyCode::Enter);
        t.draw(|f| draw(f, &mut app)).unwrap();
        click(&mut app, 2);
        assert_eq!(app.current().unwrap().title, "问");
        assert!(app.attach.is_none());
        click(&mut app, 2); // 再点一下进入
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
        }
    }

    #[test]
    fn preview_cuts_input_box_even_with_panels_below() {
        let bar = "─".repeat(60);
        let mut screen = vec!["⏺ 结果", "· Choreographing… (9m 10s)", "  ⎿  Tip: …", "", &bar, "❯\u{a0}", &bar];
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
        assert_eq!(ansi::plain(got.last().unwrap()), "  ⎿  Tip: …");
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
        assert_eq!(preview_side(Auto, 46, 30), Some(Side::Below)); // 手机竖屏:分屏
        assert_eq!(preview_side(Auto, 110, 20), Some(Side::Right)); // 手机横屏 / 桌面
        assert_eq!(preview_side(Auto, 90, 30), Some(Side::Below));
        assert_eq!(preview_side(Focus, 46, 30), Some(Side::Focus)); // 预览为主
        assert_eq!(preview_side(Focus, 150, 40), Some(Side::Below)); // 宽屏没有这一种(手机转屏才会碰到)
        assert_eq!(preview_side(Below, 200, 50), Some(Side::Below));
        assert_eq!(preview_side(Below, 46, 12), None); // 太矮
        assert_eq!(preview_side(Off, 200, 50), None);
    }

    #[test]
    fn v_cycles_what_this_screen_can_show_and_phone_and_desk_keep_their_own() {
        use Preview::*;
        let mut app = fixture();
        app.screen = Rect::new(0, 0, 46, 30); // 手机
        let mut seen = vec![];
        for _ in 0..3 {
            press(&mut app, KeyCode::Char('v'));
            seen.push(app.mode());
        }
        assert_eq!(seen, [Focus, Off, Auto]); // 分屏 → 预览为主 → 只看列表 → 分屏,每下都不一样
        press(&mut app, KeyCode::Char('v'));
        let rows = reader_rows(&mut app, 46, 30);
        // 底栏亮一会儿:这几种里轮到了哪个
        assert_eq!(rows[29].trim(), "布局  分屏 →  预览为主  → 只看列表", "{rows:#?}"); // 当前的是强调色底的块
        // 预览为主:列表只剩几行,嵌着标题的线上写着高亮的是第几个
        let rule = rows.iter().position(|r| r.starts_with("─ ")).unwrap();
        assert!(rule <= 1 + FOCUS_ROWS as usize && rows[rule].contains("1/3"), "{rows:#?}");
        // 桌面各记各的:手机上改了不影响桌面
        app.screen = Rect::new(0, 0, 150, 40);
        assert_eq!(app.mode(), Auto);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.mode(), Below);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.mode(), Off);
        // 只看列表:底下一行写高亮的那个在干什么
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("─ ◐ 待确认 · ") && r.contains("1/3")), "{rows:#?}");
        assert_eq!((app.view_state().preview, app.view_state().preview_narrow), (Off, Focus));
    }

    #[test]
    fn wrap_breaks_by_display_width_and_prefers_spaces() {
        let wrap_lines = |xs: &[&str], w: usize| ansi::wrap(&ansi::parse_lines(xs), w).iter().map(ansi::plain).collect::<Vec<_>>();
        let v = |xs: &'static [&'static str]| xs;
        assert_eq!(wrap_lines(v(&["hello world foo"]), 11), ["hello world", "foo"]);
        assert_eq!(wrap_lines(v(&["abcdefghij"]), 4), ["abcd", "efgh", "ij"]); // 没空格就硬断
        assert_eq!(wrap_lines(v(&["中文一二三"]), 4), ["中文", "一二", "三"]); // 一个字两格
        assert_eq!(wrap_lines(v(&["a", "", "b"]), 10), ["a", "", "b"]); // 空行保留
        assert_eq!(wrap_lines(v(&["x yyyyyyyyy"]), 6), ["x yyyy", "yyyyy"]); // 空格太靠前就不在那断
    }

    /// 屏幕上每一样都有(按顺序出现在某一行里)
    fn shows(rows: &[String], wants: &[&str]) {
        for w in wants {
            assert!(rows.iter().any(|r| r.contains(w)), "缺「{w}」:{rows:#?}");
        }
    }

    /// 顶栏上当前的范围(强调色、带下划线的那个)
    fn active_tab(app: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer();
        (0..80u16).map(|x| &buf[(x, 0)]).filter(|c| c.modifier.contains(Modifier::UNDERLINED)).map(|c| c.symbol().to_string()).collect()
    }

    /// 打开「跑」的记录,喂进测试用的那一段(不起后台线程)
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
        // 按键照上次画的走:真用时读好了先画一遍才有键进来
        reader_rows(&mut app, 120, 30);
        (app, p)
    }

    /// 桌面两栏的左栏(列表)和右栏(分隔线右边)
    fn trace_panes(app: &mut App, w: u16, h: u16) -> (Vec<String>, Vec<String>) {
        let rows = reader_rows(app, w, h);
        let split = |r: &String| r.split_once('│').map(|(a, b)| (a.to_string(), b.to_string()));
        let body: Vec<(String, String)> = rows[1..rows.len() - 1].iter().filter_map(split).collect();
        (body.iter().map(|x| x.0.clone()).collect(), body.iter().map(|x| x.1.clone()).collect())
    }

    /// 在按的那一列选中的:一轮写提问的第一行,调用写说的第一句,工具写摘要(没有就写名字)
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
        // 第一轮的过程里,停到起 Explore 的那一步
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
        shows(&right, &["▸ Agent Explore · 找 width", "3 调用 · $", "找到 4 处:", "… 另 2 行"]);
        // ⏎:它自己的记录,同样两栏;左边是派给它的任务和它最后说的,右边是它的过程(里面又起了一个)
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        let (left, right) = trace_panes(&mut app, 120, 30);
        shows(&left, &["任务  找出所有 width( 的调用", "Claude  找到 4 处:"]);
        shows(&right, &["· Agent general-purpose · 细看 ui.rs", "ui.rs 里有 2 处", "Bash  rg -n 'width\\('"]);
        // 再进一层、Esc 一层层回来,回到起它的那一步
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
        // 手机宽度:进子代理也是整屏看它的过程
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 46, 30);
        assert!(rows.iter().any(|r| r.contains("· Agent general-purpose")), "{rows:#?}");
    }

    /// 列表里的子代理(和 `with_subs` 那份记录里的三个对得上):Explore 在跑,它再起的那个也在跑,fork 的跑完了
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

    /// 「跑」另开了个窗口,又派了三个子代理(窗口是采集时先拼的,不带着子代理)
    fn with_agents() -> Vec<Session> {
        let mut v = sessions();
        let win = sub_of(v.iter().find(|s| s.title == "跑").unwrap(), 2, false, "zsh", State::Idle, 20);
        v.iter_mut().find(|s| s.title == "跑").unwrap().subagents = agents();
        v.push(win);
        v
    }

    /// 拿本机真的记录看列表长什么样(不在 CI 里跑):
    /// `ACA_SUBAGENT_PROBE=<会话的记录> cargo test -p aca-tui agents_look -- --ignored --nocapture`。
    /// 那段对话起过的子代理全挂到「跑」上(每三个里算一个还在跑),展开,高亮停在中间那个
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
        // 收着:那一行写着几个在跑,预览顶上说一句;不占行
        let rows = reader_rows(&mut app, 120, 30);
        shows(&rows, &[" 3 ● 跑 +2", "子代理 2 个在跑 · 1 个完了 · l 展开", "● 1 运行中"]);
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
        // l:展开成行,紧跟在会话下面、排在它的窗口前面;高亮不动
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(order(&app), ["问", "未读", "跑", "Explore · 找 width", "细看 ui.rs", "fork · 接着想", "zsh", "闲", "错"]);
        assert_eq!(app.current().unwrap().title, "跑");
        let rows = reader_rows(&mut app, 120, 30);
        // 不编号(别的行的号不跟着跳)、字前面是树枝;再起的那个带 ↳;跑完的打勾;最后一列是它跑了多久。顶上数的还是会话;
        // 预览顶上那句跟着换成这时候能按的键
        shows(
            &rows,
            &[
                "子代理 2 个在跑 · 1 个完了 · h 收起",
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
        assert!(row(&rows, "├ ● Explore").contains(" 1m"), "{rows:#?}");
        assert!(row(&rows, "├ ✓ fork").contains("40s"), "{rows:#?}");
        // 数字键照编号走:4 是窗口,不是子代理
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.current().unwrap().title, "zsh");
        // 再按 l:进到第一个子代理;预览画的是它的那几行,顶上是它的状态、谁派的
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.current().unwrap().title, "Explore · 找 width");
        let rows = reader_rows(&mut app, 120, 30);
        shows(
            &rows,
            &[
                "● Explore · 找 width",
                "子代理",
                "在跑 · 1 分钟   3 步",
                "「跑」的子代理 · ⏎ 看它的记录",
                "派给它的",
                "09:01 Bash  rg -n width",
            ],
        );
        // 对着会话做的键不理;走动的照常
        for c in ['d', 'r', 'D', 'e', 'z', 'x', 'p', 'N'] {
            app.msg = None;
            press(&mut app, KeyCode::Char(c));
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("子代理只能 ⏎(看它的记录)"), "{c}");
            assert!(app.confirm.is_none() && app.prompt.is_none() && app.reader.is_none() && app.new_draft.is_none(), "{c}");
        }
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "细看 ui.rs");
        // h:回到它的会话;再 h:收起来(分组没折);再 h 才折分组
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
        // 还在跑的:照屏幕那样露底下那截(最新的一步在最下面)
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows.iter().any(|r| r.contains("第 80 行")) && !rows.iter().any(|r| r.contains("第 1 行")), "{rows:#?}");
        // 交回话了的:从头露,先看到它交回来的话的开头
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().title, "fork · 接着想");
        // 连着按了两下:预览等高亮停稳才换(见 SETTLE),这里当它停稳了
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
        // 刷新:Explore 跑完了,它再起的那个不列了 → 高亮回到会话上;别的行照新的画
        let mut v = with_agents();
        let run = v.iter_mut().find(|s| s.title == "跑").unwrap();
        run.subagents.remove(1);
        run.subagents[0].phase = Phase::Done;
        app.set_sessions(v);
        assert_eq!(order(&app), ["问", "未读", "跑", "Explore · 找 width", "fork · 接着想", "zsh", "闲", "错"]);
        assert_eq!(app.current().unwrap().title, "跑");
        let rows = reader_rows(&mut app, 120, 30);
        shows(&rows, &[" 3 ● 跑", "    ├ ✓ Explore · 找 width", "子代理 2 个跑完了 · h 收起"]);
        assert!(!rows.iter().any(|r| r.contains("跑 +")), "{rows:#?}");
        // 一个都不列了:行没了,也不再记着展开 —— 下回再派还是先收着
        app.set_sessions(sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        app.set_sessions(with_agents());
        assert_eq!(order(&app), ["问", "未读", "跑", "zsh", "闲", "错"]);
        // 没派子代理的会话上 l 照旧不动
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
        // ⏎:不是进会话(它没有窗格),是开会话的记录
        press(&mut app, KeyCode::Enter);
        assert!(app.attach.is_none() && app.start.is_none());
        assert_eq!(app.trace_request().map(|s| s.title), Some("跑".to_string()));
        let (p, _, _) = crate::trace_claude::tests::with_subs("ui-agent-row");
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p.clone(), Agent::Claude, 0).unwrap()));
        // 读好了就在它那一层
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · Explore · 找 width"), "{}", rows[0]);
        assert_eq!(app.trace.as_ref().unwrap().path.len(), 1);
        let (left, right) = trace_panes(&mut app, 120, 30);
        shows(&left, &["任务  找出所有 width( 的调用", "Claude  找到 4 处:"]);
        shows(&right, &["Bash  rg -n 'width\\('"]);
        // Esc 一下就回列表,高亮还在那一行
        press(&mut app, KeyCode::Esc);
        assert!(app.trace.is_none());
        assert_eq!(app.current().unwrap().title, "Explore · 找 width");
        // 再起的那个:一路进两层。Z 和 ⏎ 一样
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('Z'));
        let _ = app.trace_request();
        app.set_trace("跑", Ok(crate::trace::Trace::open_born(p, Agent::Claude, 0).unwrap()));
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows[0].contains("子代理 · general-purpose · 细看 ui.rs"), "{}", rows[0]);
        assert_eq!(app.trace.as_ref().unwrap().path.len(), 2);
        // ← 往上一层:起它的那个子代理(停在右边它的过程上)。从那 Esc 是照旧的走法:先回左边,再退到会话自己的记录,再回列表
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
        // 左边是对话:你说的、Claude 最后说的(Markdown 排过)、一行数字;中间说的话不在这
        shows(
            &left,
            &[
                "接着上一段对话 · 1 轮 · 不计",
                "你  看看两栏为什么对不齐",
                "Claude  改好了:len() 数的是字节",
                "• 换成按显示宽度算的 width()",
                "09:00 · 20.0s · 4 调用 · 3 工具 · $0.03",
                "── 压缩 97k → 12k · 21.0s",
                "你  再试一次",
                "✕ 出错",
                "你  跑一下测试",
                "● 在跑",
            ],
        );
        assert!(!left.iter().any(|r| r.contains("先看 bill.rs")), "中间说的在右边:{left:#?}");
        // 右边是选中那一轮(跟着最新 = 在跑的那一轮)的过程
        shows(
            &right,
            &["1 调用 · 2 工具", "── 4.1s", "▸ Read  src/usage.rs", "5 行", "· Bash  cargo build --release", "在跑 · ", "● 在跑…"],
        );
        shows(&reader_rows(&mut app, 120, 30), &["记录 · 跑", "3 轮", "→ 看过程"]);
        // 在按左边:左边选中那块头一行淡紫底,右边不铺
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
        // 各种宽度折好的都放得下
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
        // 手机上照样有标题和钱
        shows(&reader_rows(&mut app, 46, 20), &["记录 · 跑", "3 轮 · $"]);
    }

    #[test]
    fn shift_z_keys_move_through_turns_step_into_the_process_and_back() {
        let (mut app, _) = trace_app("keys");
        // p:上一轮(压缩那行不停)
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "再试一次");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        // 右边换成这一轮的过程:每次调用的数字、想的、中间说的、工具带改动和输出的开头;回复在左边,这里只留一句
        let right = trace_panes(&mut app, 120, 40).1;
        shows(
            &right,
            &[
                "4 调用 · 3 工具",
                "── 4.0s · 上下文 1.1k · 出 640 · $0.01 ──",
                "思考 先确认是不是宽字符的问题",
                "先看 bill.rs 的列宽",
                "▸ Grep  width(",
                "src/bill.rs:12:",
                "… 另 1 行",
                "▸ Edit  src/bill.rs",
                "+2 -2",
                "12 -     let w = s.len();",
                "12 +     let w = s.width();",
                "running 220 tests",
                "回复 改好了:len() 数的是字节",
            ],
        );
        assert!(!right.iter().any(|r| r.contains("• 测试全过了")), "回复全文在左边:{right:#?}");
        // → 进右边,一步步走;⏎ 整屏看这一步
        press(&mut app, KeyCode::Right);
        assert!(app.trace.as_ref().unwrap().in_steps);
        shows(&reader_rows(&mut app, 120, 20), &["← Esc 回对话"]);
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "先看 bill.rs 的列宽");
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "width(");
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 100, 24), &["Grep · 第 1 轮", "── 输入 ──", "── 输出 ──", "src/ui.rs:90:"]);
        // 整屏里 n:过程里的下一步
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 24), &["第 1 轮 · 第 2 次调用", "缓存写 100(1 小时)", "── 调用了 ──", "▸ Edit  src/bill.rs"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(
            &reader_rows(&mut app, 100, 24),
            &["Edit · 第 1 轮", "── 改动 ──", "11   fn col(s: &str)", "12 +     let w = s.width();", "┈┈┈┈┈", "41 +     pad(w.max(1))"],
        );
        press(&mut app, KeyCode::Esc);
        // 第一次调用的整屏:先想、再说,token 分开写
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(
            &reader_rows(&mut app, 100, 30),
            &["第 1 轮 · 第 1 次调用", "claude-opus-5-5", "── 思考 ──", "中文一个字占两格", "── 回复 ──", "── 调用了 ──"],
        );
        press(&mut app, KeyCode::Esc);
        // 右边 n / p 换轮,还在右边、从头看
        press(&mut app, KeyCode::Char('n'));
        assert!(app.trace.as_ref().unwrap().in_steps);
        assert_eq!(trace_sel(&app), "再试一次");
        shows(&trace_panes(&mut app, 120, 20).1, &["出错", "529 server_error"]);
        // ← / Esc 回对话,再 Esc 才回列表
        press(&mut app, KeyCode::Esc);
        assert!(!app.trace.as_ref().unwrap().in_steps);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Left);
        assert!(app.trace.is_some() && !app.trace.as_ref().unwrap().in_steps);
        // Tab 看输出,Z 回来;Esc 回到列表
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
        shows(&left, &["你  第 1 行问题", "第 6 行问题", "… 另 14 行", "Claude  第 1 行回答", "第 12 行回答", "… 另 18 行"]);
        assert!(!left.iter().any(|r| r.contains("第 7 行问题")));
        press(&mut app, KeyCode::Char('o'));
        let rows = reader_rows(&mut app, 120, 80);
        shows(&rows, &["第 20 行问题", "第 30 行回答"]);
        assert!(!rows.iter().any(|r| r.contains("… 另")));
    }

    #[test]
    fn shift_z_o_opens_and_closes_a_step_and_shift_o_the_whole_turn() {
        let (mut app, _) = trace_app("fold");
        // 按键照上次画的走:每下画一遍,同真用
        let key = |app: &mut App, k: KeyCode| {
            press(app, k);
            trace_panes(app, 120, 50)
        };
        key(&mut app, KeyCode::Char('p'));
        key(&mut app, KeyCode::Char('p'));
        // 左边:本来就是全文的一轮,o 说一声
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(app.msg.as_ref().unwrap().0, "这一轮已经是全文");
        // 右边选到 Edit:收着只有改动的行
        key(&mut app, KeyCode::Right);
        for _ in 0..4 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(trace_sel(&app), "src/bill.rs");
        let right = trace_panes(&mut app, 120, 50).1;
        shows(&right, &["▸ Edit  src/bill.rs", "12 -     let w = s.len();"]);
        assert!(!right.iter().any(|r| r.contains("fn col(s: &str)")), "{right:#?}");
        // o:带上下文、两处中间点线;再 o 收回去
        let right = key(&mut app, KeyCode::Char('o')).1;
        shows(&right, &["▾ Edit  src/bill.rs", "11   fn col(s: &str) -> usize {", "┈┈┈┈┈", "42   }"]);
        let right = key(&mut app, KeyCode::Char('o')).1;
        assert!(!right.iter().any(|r| r.contains("fn col(s: &str)")), "{right:#?}");
        // 没有别的内容的一步(只有数字的那次调用):说一声
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(app.msg.as_ref().unwrap().0, "这一步没有别的内容");
        // O:这一轮全部展开(想的全文、搜到的全部、整条命令和全部输出),再 O 全收
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
        shows(&right, &["▸ Grep", "▸ Edit", "▸ Bash", "… 另 3 行"]);
        // 想的短,收着也是全文,摊成一行
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
        // 右边选到 Bash(记录里的文件不在本机):写成一份 Markdown 的临时文件,看完删
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
        // 整屏里也能 e
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::View { temp: true, .. })));
        key(&mut app, KeyCode::Esc);
        // 左边:一轮看整轮;压缩那行没有可看的
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('e'));
        let Some(External::View { path, .. }) = app.external.take() else { panic!() };
        assert!(std::fs::read_to_string(&path).unwrap().contains("## 你\n\n看看两栏为什么对不齐"));
        let _ = std::fs::remove_file(&path);
        key(&mut app, KeyCode::Char('n'));
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('e'));
        assert!(app.external.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "这一行没有内容可看");
    }

    #[test]
    fn shift_z_colors_only_what_is_on_screen() {
        let (mut app, _) = trace_app("color");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        // 矮屏:右边只露出上面几块,底下的先不上色
        reader_rows(&mut app, 120, 12);
        let v = app.trace.as_ref().unwrap();
        let c = v.steps_cache.as_ref().unwrap();
        let (top, h) = (v.steps.top, v.steps.view_h);
        for (b, &(_, s, e)) in c.blocks.iter().zip(&v.steps.items) {
            let seen = e > top && s < top + h;
            assert_eq!(b.colored > 0, seen, "块 {} 在 {s}..{e},屏幕 {top}..{}", b.node, top + h);
        }
        // 高屏:Edit 露出来了,屏幕上 let 是关键字的颜色
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
        // ^Z 不是 z(看输出)、^O 不是 o
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
        // 设置页里预览别的主题:排好的行是旧颜色,要重排
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
        // 滚轮:鼠标在哪边滚哪边
        let wheel = |app: &mut App, column: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column, row: 5, modifiers: KeyModifiers::NONE })
        };
        wheel(&mut app, 100);
        let v = app.trace.as_ref().unwrap();
        assert_eq!((v.steps.top, v.chat.top), (3, v.chat.top));
        let chat_top = v.chat.top;
        wheel(&mut app, 5);
        assert!(app.trace.as_ref().unwrap().chat.top > chat_top);
        // 换了轮,右边从头看
        press(&mut app, KeyCode::Char('n'));
        trace_panes(&mut app, 120, 12);
        assert_eq!(app.trace.as_ref().unwrap().steps.top, 0);
    }

    #[test]
    fn shift_z_on_a_phone_is_the_chat_and_a_turns_steps_open_full_screen() {
        let (mut app, _) = trace_app("phone");
        // 刚打开:整屏是对话,贴着最新
        let rows = reader_rows(&mut app, 46, 16);
        shows(&rows, &["你  跑一下测试", "● 在跑", "⏎ 看过程"]);
        assert!(!rows[1..rows.len() - 1].iter().any(|r| r.contains('│')), "手机上不分两栏:{rows:#?}");
        assert_eq!(trace_sel(&app), "跑一下测试");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        // 一块比一屏高:↓ 先在块里滚
        reader_rows(&mut app, 46, 8);
        let top = app.trace.as_ref().unwrap().chat.top;
        press(&mut app, KeyCode::Down);
        assert_eq!(trace_sel(&app), "看看两栏为什么对不齐");
        assert_eq!(app.trace.as_ref().unwrap().chat.top, top + 1);
        // ⏎ 整屏看这一轮的过程,顶上写第几轮
        press(&mut app, KeyCode::Enter);
        let rows = reader_rows(&mut app, 46, 30);
        shows(&rows, &["第 1 轮", "── 4.0s", "▸ Grep  width(", "▸ Edit  src/bill.rs", "o 展开", "Esc 回对话"]);
        assert!(!rows.iter().any(|r| r.contains("你  看看两栏")));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 46, 20), &["Grep · 第 1 轮", "── 输出 ──"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        shows(&reader_rows(&mut app, 46, 16), &["你  看看两栏为什么对不齐", "⏎ 看过程"]);
        // G 回到最新
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
        // 新的一轮来了:左边选到它、贴底,右边换成它的过程
        let (left, right) = trace_panes(&mut app, 120, 20);
        assert_eq!(trace_sel(&app), "下一句");
        shows(&left, &["你  下一句"]);
        assert!(!right.iter().any(|r| r.contains("cargo build")), "{right:#?}");
        shows(&reader_rows(&mut app, 120, 20), &["4 轮"]);
        // 往上挪了就不跟:再来新的不动
        press(&mut app, KeyCode::Up);
        add(crate::trace_claude::tests::prompt("u5", Some("u4"), 70_000, "又一句"));
        app.trace_tail();
        trace_panes(&mut app, 120, 20);
        assert_ne!(trace_sel(&app), "又一句");
        // 手机上跟着最新也是贴底
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
        // 换了会话后才回来的旧结果不要
        press(&mut app, KeyCode::Char('J'));
        app.set_trace("跑", Err("旧的".into()));
        assert!(app.trace.as_ref().unwrap().err.is_none());
    }

    /// 手动看样子:cargo test --release trace_look -- --ignored --nocapture
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

    /// 手动看颜色:ACA_TRACE_HTML=<目录> cargo test --release trace_html -- --ignored。把几屏画成网页
    /// 画一屏,转成带颜色的 HTML(自查排版用)
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
        // o 展开 Edit
        press(&mut app, KeyCode::Char('o'));
        html += &page(&mut app, 120, 30);
        // 最后一轮的 Read 展开(按键照上次画的走,每下都画一遍)
        for k in [KeyCode::Char('n'), KeyCode::Char('n'), KeyCode::Down, KeyCode::Down, KeyCode::Char('o')] {
            press(&mut app, k);
            page(&mut app, 120, 30);
        }
        html += &page(&mut app, 120, 30);
        // 刚够两栏的宽度
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

    /// 子代理那几屏:ACA_TRACE_HTML=<目录> cargo test --release trace_html_subs -- --ignored
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
                // 宽字符后面那格是占位,跳过
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
        press(&mut app, KeyCode::Enter); // 回车是换行
        typ(&mut app, "再提交");
        assert_eq!(app.notes.get("id-闲"), Some("先跑测试\n再提交")); // 边写边存,不用等 Esc
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("✎ 便签")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("再提交▏")), "光标在预览里:{rows:#?}");
        // 写在预览里了,底栏只剩按键,不再重复一遍内容
        assert!(rows.last().unwrap().trim_start().starts_with("Esc 写完"), "{rows:#?}");
        assert!(!rows.last().unwrap().contains("先跑测试"), "{rows:#?}");
        // 没有预览(关了)时,在弹窗里写
        app.preview = Preview::Off;
        shows(&reader_rows(&mut app, 100, 24), &["便签 · 闲", "先跑测试", "再提交▏", "Esc 写完"]);
        app.preview = Preview::Auto;
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "便签已存");
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("○ 闲") && r.contains('✎')), "{rows:#?}");
        for want in ["空闲 · 10 秒前", "/p/work", "✎ 先跑测试", "再提交"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        // 再按 e:原来的先填好;^U 清空当场就删
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
        // 写到一半按 ^G:带着写了的去编辑器,回来接着在底栏写
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
        // 列表里直接 ^G;编辑器没正常退出(:cq)= 不改
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "草稿\n编辑器里补的");
        app.note_editor_done(ed, Err("nvim 没正常退出,便签没改".into()));
        assert_eq!(app.notes.get("id-闲"), Some("草稿\n编辑器里补的"));
        assert!(app.prompt.is_none());
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        app.note_editor_done(ed, Ok(String::new())); // 编辑器里删光 = 删便签
        assert_eq!(app.notes.get("id-闲"), None);
    }

    #[test]
    fn only_shift_e_takes_the_note_in_and_only_when_it_is_safe() {
        let mut app = fixture();
        for t in ["闲", "跑", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的")).unwrap();
        }
        app.set_sessions(sessions());
        press(&mut app, KeyCode::Char('a')); // 全部(「未读」五天没动)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Enter); // 回车:便签照旧是便签
        assert_eq!((app.attach.take().as_deref(), app.fill.take()), (Some("闲"), None));
        press(&mut app, KeyCode::Char('E'));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        assert_eq!(app.fill.take(), Some(("id-闲".into(), "给闲的".into())));
        select(&mut app, "跑"); // 在跑也能填(Claude 边跑边打字,回车就排队);kitty 里 Shift+Enter 一样
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.fill.take().map(|f| f.0), Some("id-跑".into()));
        app.attach = None;
        select(&mut app, "问"); // 在等你选:字会被当成选项 → 不进去,说为什么
        press(&mut app, KeyCode::Char('E'));
        assert_eq!((app.attach.take(), app.fill.take()), (None, None));
        assert!(app.msg.as_ref().unwrap().0.contains("等你选"));
        select(&mut app, "未读"); // 没便签:和回车一样
        press(&mut app, KeyCode::Char('E'));
        assert_eq!((app.attach.take().as_deref(), app.fill.take()), (Some("未读"), None));
        // 填完删掉,✎ 跟着没了
        app.note_filled("id-闲");
        assert_eq!(app.notes.get("id-闲"), None);
        assert!(!app.all.iter().find(|s| s.title == "闲").unwrap().noted);
        // 不是 Claude / Codex:不知道前台是什么程序,不填
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "跑").unwrap().agent = Agent::None;
        app.set_sessions(v);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('E'));
        assert_eq!(app.attach.take(), None);
        assert!(app.msg.as_ref().unwrap().0.contains("不是 Claude"));
    }

    #[test]
    fn s_sends_the_note_only_after_y_and_nothing_without_one() {
        let mut app = fixture();
        for t in ["闲", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的\n第二行")).unwrap();
        }
        app.set_sessions(sessions());
        select(&mut app, "跑"); // 没便签:什么都不发,只说一声
        press(&mut app, KeyCode::Char('s'));
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert!(app.msg.as_ref().unwrap().0.contains("没有便签"), "{:?}", app.msg);
        app.msg = None; // 提示占着底栏
        assert!(!reader_rows(&mut app, 120, 20).iter().any(|r| r.contains("发便签")));
        // 有便签也先问:发给谁、发什么(多行照样分行);有便签时底栏才出现 s
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 120, 20);
        assert!(rows.iter().any(|r| r.contains("s 发送")), "{rows:#?}");
        press(&mut app, KeyCode::Char('s'));
        let rows = reader_rows(&mut app, 46, 16);
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("缺「{t}」:{rows:#?}"));
        assert!(at("发送便签？") < at("给闲的") && at("给闲的") < at("第二行") && at("第二行") < at("y 发送"), "{rows:#?}");
        press(&mut app, KeyCode::Char('s')); // 误触两下:第二下只是取消
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "没发");
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        let Job::Send { t, pane, text, key } = &job else { panic!("{job:?}") };
        assert_eq!((t.name.as_str(), pane.as_str(), text.as_str(), key.as_str()), ("闲", "=闲:", "给闲的\n第二行", "id-闲"));
        // 发出去了便签就删
        app.job_done(&job, Outcome::Done("已发给「闲」".into()), sessions());
        assert_eq!(app.notes.get("id-闲"), None);
        // 在等你选:字会被当成选项,不发
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.confirm.is_none());
        assert!(app.msg.as_ref().unwrap().0.contains("等你选"));
    }

    #[test]
    fn preview_head_states_the_facts_and_caps_long_notes() {
        let mut s = mk("闲", "work", State::Idle, 70);
        s.dir = "/p/work".into();
        s.info = "Repo: /r\nPath: /p/work\nBranch: feat".into();
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        // 状态 · 多久以前、目录、分支,最后空一行再接屏幕内容
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10)), ["空闲 · 1 分钟前   /p/work  feat", ""]);
        // 会话 id、别处打开不上预览(B 复制 id;⇄ 列表那行有)
        s.info.push_str("\nSession: 0000aaaa-1111-2222-3333-444455556666");
        s.attached = true;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10)), ["空闲 · 1 分钟前   /p/work  feat", ""]);
        // 窄了从后面截
        assert_eq!(text(&preview_head(&s, None, None, false, 24, 10))[0], "空闲 · 1 分钟前   /p/wo…");
        // 子会话:谁派的;父会话删了也说
        s.up = Up::Apart("p".into(), "派活的".into());
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "「派活的」的子会话");
        s.up = Up::Gone;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "子会话 · 父会话已删");
        s.up = Up::Top;
        // 分叉出来的:分自谁;原会话删了也说
        s.fork = Fork::Of("原来的".into());
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "分自「原来的」");
        s.fork = Fork::Gone;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "分叉出来的 · 原会话已删");
        s.fork = Fork::No;
        // 便签最多三行,多的写还有几行
        let long = (1..=20).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        let got = text(&preview_head(&s, None, Some(&long), false, 30, 20));
        assert_eq!(got[1..], ["✎ 第1行", "  第2行", "  第3行", "  … 还有 17 行", ""]);
        // 正在写:短的也固定占上半区;长了留住最后几行(光标在那)
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
        select(&mut app, "跑"); // 往下两个都是会话,不经过分组标题
        let border = |app: &mut App| reader_rows(app, 100, 24).into_iter().find(|r| r.contains('╭')).unwrap();
        let cur = |app: &App| app.current().unwrap().title.clone();
        border(&mut app);
        press(&mut app, KeyCode::Down); // 单独挪一下:马上换
        let one = cur(&app);
        assert!(border(&mut app).contains(&one));
        press(&mut app, KeyCode::Down); // 紧接着又挪:在滑动,预览先停在上一个
        assert_ne!(cur(&app), one);
        assert!(border(&mut app).contains(&one), "滑动途中不换预览");
        assert!(app.settle_left().is_some());
        // 停下来够久了:换上
        app.moved = app.moved.map(|(_, gap)| (Instant::now() - SETTLE * 2, gap));
        assert!(app.settle_left().is_none());
        let now = cur(&app);
        assert!(border(&mut app).contains(&now));
    }

    #[test]
    fn preview_title_and_list_show_vendor_and_account() {
        let mut v = sessions();
        v[0].who = "team".into(); // 问,高亮的第一个
        v[2].who = "alt".into();
        v[2].agent = Agent::Codex;
        let mut app = App::new(String::new(), v);
        let rows = reader_rows(&mut app, 46, 20);
        // 手机上预览在下面:标题嵌在分隔线上,后面是哪家·哪个账号
        assert!(rows.iter().any(|r| r.starts_with("─ ◐ 问 ─") && r.ends_with(" Claude·team ─")), "{rows:#?}");
        // 列表里的账号名按哪家上色
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
        // 高亮的那行:整行铺淡紫底,各段自己的颜色照留
        let y = rows.iter().position(|r| r.contains(" 闲")).unwrap() as u16;
        let (mut x, mut bad) = (0u16, vec![]);
        while x < 46 {
            let c = &buf[(x, y)];
            if c.bg != theme::pal().sel {
                bad.push(x);
            }
            x += c.cell_width().max(1); // 宽字符后面那格是占位,终端里被前一格盖住
        }
        assert_eq!(buf[(2, y)].fg, theme::pal().muted, "图标 ○ 是灰的,不是反色");
        assert!(bad.is_empty(), "{:?} {bad:?}", rows[y as usize]);
    }

    #[test]
    fn z_reads_without_attaching_and_scrolls_history() {
        let mut app = fixture(); // 近一天:问 跑 闲
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('z'));
        assert!(app.reader_load && app.attach.is_none());
        assert_eq!(app.reader_target().as_deref(), Some("=跑:")); // 假会话没有窗格 id,按会话名抓
        app.reader_load = false;
        // run 循环抓回来的历史:100 行 + 底部输入框(要去掉)
        let bar = "─".repeat(20);
        let mut hist: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        hist.extend([bar.clone(), "❯ ".into(), bar]);
        app.set_reader_lines(hist);
        let rows = reader_rows(&mut app, 30, 10); // 标题 + 8 行 + 底栏
        assert!(rows[0].contains("跑") && rows[0].contains("2/3 · 最新"), "{rows:?}");
        assert_eq!(rows[8], "line 100"); // 输入框去掉了,最后一行是内容
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('b')); // 1 + 一页(10 - 3 = 7)
        let rows = reader_rows(&mut app, 30, 10);
        assert_eq!(rows[8], "line 92");
        assert!(rows[0].contains("↑8 行"), "{rows:?}");
        // 往上翻着时底下长出新内容,画面停在原处
        let mut more: Vec<String> = (1..=105).map(|i| format!("line {i}")).collect();
        more.push(String::new());
        app.set_reader_lines(more);
        assert_eq!(reader_rows(&mut app, 30, 10)[8], "line 92");
        press(&mut app, KeyCode::Char('g')); // 到顶,不会越界
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
        assert_eq!(app.reader_target().as_deref(), Some("=跑:")); // 假会话没有窗格 id,按会话名抓
        press(&mut app, KeyCode::Char('J'));
        press(&mut app, KeyCode::Char('J')); // 到头了:提示,还在看闲
        assert_eq!(app.reader_target().as_deref(), Some("=闲:"));
        assert_eq!(app.msg.as_ref().unwrap().0, "已经是最后一个");
        press(&mut app, KeyCode::Char('q')); // q 只是退出阅读,不退出 aca
        assert!(app.reader.is_none() && !app.quit);
        assert_eq!(at(&app), "闲"); // 回到列表,高亮停在最后看的那个
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
        assert!(app.reader_target().is_none()); // 不去 tmux 抓输出
        let (name, _, mode) = app.diff_request(false).unwrap();
        assert_eq!((name.as_str(), mode), ("跑", Mode::Uncommitted));
        assert!(app.diff_request(false).is_none()); // 读过了,等定时刷新再读
        assert!(app.diff_request(true).is_some());
        shows(&reader_rows(&mut app, 40, 12), &["改动 · 跑", "未提交 · 读取中…", "正在读 git…"]);
        // 三个文件,各 10 行
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
        let rows = reader_rows(&mut app, 40, 12); // 标题 + 10 行 + 底栏
        shows(&rows, &["未提交 · 3 个文件", "n p 换文件"]);
        assert_eq!(rows[1], " ~/p  main");
        let top = |app: &mut App| reader_rows(app, 40, 12)[1].clone();
        // n:下一个文件的标题放到最顶上
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(top(&mut app), "▌M f1 +10");
        shows(&reader_rows(&mut app, 40, 12), &["文件 1/3"]);
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(top(&mut app), "▌M f3 +10");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已经是最后一个文件");
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(top(&mut app), "▌M f1 +10");
        press(&mut app, KeyCode::Char('p')); // 翻过第一个:回到清单
        assert_eq!(top(&mut app), " ~/p  main");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.msg.as_ref().unwrap().0, "已经在最上面");
        // 刷新时改动变多了(清单长了一行),顶上那行不动
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('j')); // 还没重画就来了新结果,这一下也算数
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["f0", "f1", "f2", "f3"]), &[]));
        // 按行号钉住(顶上原来是第 8 行,f1 的 @@):清单多了一行,第 8 行现在是 f0 的标题
        let rows = reader_rows(&mut app, 40, 12);
        assert_eq!(rows[1], "▌M f0 +10", "{rows:#?}");
        // m:换成连同提交,重新读;还在路上的旧结果不要
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.diff_request(false).map(|r| r.2), Some(Mode::Branch));
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["old"]), &[]));
        app.msg = None; // 底栏别被提示占着
        shows(&reader_rows(&mut app, 46, 12), &["连同提交 · 读取中…", "m 只看未提交"]);
        // Tab 轮换:改动 → 记录 → 输出 → 改动,模式沿用
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_some() && app.reader.is_none());
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_none());
        assert_eq!(app.reader_target().as_deref(), Some("=跑:"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.diff_request(false).map(|r| r.2), Some(Mode::Branch));
        // J 换会话还是看改动
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
        assert_eq!(app.msg.as_ref().unwrap().0, "还在读 git,等一下");
        let patch = "diff --git a/f1 b/f1\n--- a/f1\n+++ b/f1\n@@ -5,3 +5,4 @@\n a\n-b\n+B\n+C\n c\ndiff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let mut d = diff::render(&diff::Head { dir: "~/p".into(), ..Default::default() }, patch, &[]);
        d.top = tmp.to_string_lossy().into_owned();
        let name = app.reader.as_ref().unwrap().name.clone();
        app.set_diff(&name, Mode::Uncommitted, d);
        // 一屏只放 3 行,才滚得动
        reader_rows(&mut app, 40, 5);
        let f1 = tmp.join("f1").to_string_lossy().into_owned();
        // 顶上是清单:打开第一个文件的第一处改动
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Edit { path: f1.clone(), line: 6 })); // -b 在第 6 行
        // 滚到 +C 那行在最顶上:第 7 行
        press(&mut app, KeyCode::Char('n'));
        for _ in 0..5 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(reader_rows(&mut app, 40, 5)[1], "+C");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Edit { path: f1, line: 7 }));
        // 删掉的文件打不开
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('e'));
        assert!(app.external.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "这个文件已经删掉了");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.external.take(), Some(External::Lazygit(tmp.to_string_lossy().into_owned())));
        // 回来以后重读改动
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
        // 用它真正干活的目录和那里的分支
        assert_eq!(
            text(&preview_head(&s, Some(&g), None, false, 80, 10))[0],
            "空闲 · 1 分钟前  3 个文件 +12 −4   /p/ws/some-long-project-name/app  main"
        );
        // 窄了:统计和分支留着,目录从左边截
        assert_eq!(text(&preview_head(&s, Some(&g), None, false, 46, 10))[0], "空闲 · 1 分钟前  3 个文件 +12 −4   …/app  main");
        let clean = Glance { repo: Some(diff::Stat { branch: "main".into(), ..Default::default() }), ..g };
        assert_eq!(
            text(&preview_head(&s, Some(&clean), None, false, 80, 10))[0],
            "空闲 · 1 分钟前  没改动   /p/ws/some-long-project-name/app  main"
        );
    }

    #[test]
    fn the_change_count_is_read_for_the_highlighted_one_when_it_changes_or_ticks() {
        let mut app = fixture();
        assert_ne!(app.mode(), Preview::Off);
        let first = app.glance_request(false).map(|r| r.0);
        assert!(first.is_some());
        assert!(app.glance_request(false).is_none()); // 同一个,不重复读
        assert!(app.glance_request(true).is_some()); // 定时刷新
        select(&mut app, "跑");
        let next = app.glance_request(false).map(|r| r.0);
        assert!(next.is_some() && next != first);
        press(&mut app, KeyCode::Char('z')); // 全屏里不读
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
        /// 设置页写进 config.toml 的(假的 config_set 记在这)
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
        assert_eq!(wrote(), [("ime".to_string(), "auto".to_string(), "false".to_string())]);
        assert!(!app.settings.as_ref().unwrap().ime && app.ime_changed);
        press(&mut app, KeyCode::Enter); // 高亮停在刚按的那行:⏎ 再切回来
        assert_eq!(wrote()[0].2, "true");
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
            // 设置页还开着,已经是英文
            shows(&reader_rows(&mut app, 100, 30), &["Settings", "l  language", "zh  en"]);
            press(&mut app, KeyCode::Enter);
            assert_eq!((wrote()[0].2.as_str(), i18n::lang()), ("\"zh\"", Lang::Zh));
            shows(&reader_rows(&mut app, 100, 30), &["设置", "l  语言"]);
        });
    }

    /// 功能自己加的行打开设置页时就问好了名字和值:换了语言(run 循环看到了就调 lang_changed)当场重问
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

    /// 一个功能自己加的设置行和弹窗(见 overlay.rs):弹窗记着自己是第几层,重读一次加 10
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
        // 按它的键:设置页收起,进它的弹窗,键都归它(j 不是挪高亮)
        press(&mut app, KeyCode::Char('d'));
        assert!(app.settings.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        shows(&reader_rows(&mut app, 44, 30), &["假页面 0"]);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.msg.as_ref().unwrap().0, "说一句");
        // 手机上弹窗盖着底栏:这句话写在弹窗里
        let rows = reader_rows(&mut app, 44, 30);
        let at = |want: &str| rows.iter().position(|r| r.contains(want)).unwrap_or_else(|| panic!("没有「{want}」:{rows:#?}"));
        assert!(at("说一句") > at("假页面 0") && rows[at("说一句")].starts_with('│'), "{rows:#?}");
        // 再开一层,关了回到上一层
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 30), &["假页面 1"]);
        // 最上面那层说它在打字:一下到好几个键是粘贴,run 循环不丢
        assert!(app.typing());
        press(&mut app, KeyCode::Esc);
        assert!(!app.typing());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // 后台的活交给 run 循环;还没做完再来一个不接;做完弹窗重读
        press(&mut app, KeyCode::Char('j'));
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Stop { t: target("闲") });
        // 在做的时候弹窗里写着「正在…」
        shows(&reader_rows(&mut app, 44, 30), &["│ 正在停止「闲」…"]);
        press(&mut app, KeyCode::Char('j'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "还在忙:正在停止「闲」…");
        app.job_done(&job, Outcome::Done("好了".into()), sessions());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 10"]);
        // 要复制的交给 run 循环(它去写剪贴板,底栏说复制了什么),弹窗还开着
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.clip.take(), Some(Clip::Text("https://a.ts.net/".into())));
        assert!(!app.overlays.is_empty());
        // 开编辑器:回来还在弹窗里,重读
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Lazygit("/p".into())));
        app.external_done(None);
        assert!(app.settings.is_none());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 20"]);
        // 关了:回设置页,高亮还在它那一行
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        let s = app.settings.as_ref().unwrap();
        assert_eq!(s.rows()[s.sel], SetRow::Extra(0));
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && !app.quit);
    }

    #[test]
    fn shift_p_opens_the_plugins_page_from_the_list_not_from_settings() {
        // 列表上直接开(停在分组上也行),关了回列表,不经过设置页
        let mut app = fixture().with_plugins(|| Box::new(FakePage(0)));
        press(&mut app, KeyCode::Char('P'));
        assert!(app.modal_open() && app.settings.is_none());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // 菜单里进一页(再开一层),Esc 退回菜单,再 Esc 回列表
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 30), &["假页面 1"]);
        press(&mut app, KeyCode::Esc);
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && app.settings.is_none() && !app.quit);
        // 设置页里没有它的行
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
        assert!(app.prompt.as_ref().is_some_and(|p| p.hint == "目录是空的") && wrote().is_empty());
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
        // 不是从设置页开的编辑器,回来不弹设置页
        app.external_done(None);
        app.settings = None;
        app.external_done(None);
        assert!(app.settings.is_none());
    }

    #[test]
    fn a_in_settings_opens_the_accounts_page_and_closing_it_comes_back() {
        let mut app = settings_app().with_accounts(|adding| Box::new(FakePage(if adding { 7 } else { 0 })));
        ctrl(&mut app, 's');
        // 设置页那一行只数各有几个;管账号在它自己的弹窗里(acctpage.rs)
        shows(&reader_rows(&mut app, 100, 30), &["a  账号", "Claude 1  Codex 1"]);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.settings.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // 关了:回设置页,高亮还在账号那一行
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        assert_eq!(app.settings.as_ref().map(|s| (s.page, s.rows()[s.sel])), Some((SetPage::Main, SetRow::Accounts)));
        // 没接账号页的(单元测试默认):按了不动
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
        app.sync_failed = vec!["config.toml 写法有问题".into()];
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('k')); // 从顶上往上 = 最后一行
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.settings.as_ref().unwrap().page, SetPage::Problems);
        let rows = reader_rows(&mut app, 120, 36);
        assert!(rows.iter().any(|r| r.contains("config.toml 写法有问题")), "{rows:#?}");
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
            "i  桌面切英文   开",
            "Claude 1  Codex 1",
            "g  新分组目录   ~/workspace",
            "e  config.toml",
        ] {
            at(t);
        }
        // 居中,底下变灰(和 Tab / S 一样)
        let top = at("设置") - 2;
        // 按显示宽度数(左边可能有中文)
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
            shows(&reader_rows(&mut app, 100, 30), &["插件菜单 · 对「闲」"]);
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
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("✕ PX · 看 CI 出错了"));
            assert!(app.reader.is_none());
            // 视图:哪怕只有一行也进阅读页;r 重跑,c 复制全部,Esc 回来
            app.plugin_done(plugin_req("ci", ItemKind::View), true, "build  ok\ntest   ok".into());
            app.msg = None;
            let rows = reader_rows(&mut app, 100, 20);
            shows(&rows, &["build  ok", "test   ok", "r 重跑", "c 复制"]);
            assert!(!rows[0].contains("1/") && rows[0].contains("✓ PX · 看 CI"), "{}", rows[0]);
            press(&mut app, KeyCode::Char('r'));
            assert_eq!(app.plugin_q.len(), 1);
            press(&mut app, KeyCode::Char('c'));
            assert_eq!(app.clip, Some(Clip::Text("build  ok\ntest   ok".into())));
            // 进不去:回车只是关掉
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
            // 它的一个键跑完了:结果写在那一页下面,列表排队重读;不弹底栏、不进阅读页
            let refresh = crate::caps::Req { refresh: true, ..view.clone() };
            let rm = crate::caps::Req { then: Some(Box::new(refresh.clone())), ..plugin_req("rm", ItemKind::Command) };
            app.plugin_done(rm, true, "已撤 nas 上的「备份」".into());
            assert!(app.msg.is_none() && app.reader.is_none());
            assert_eq!(app.plugin_q, vec![refresh.clone()]);
            app.plugin_done(refresh.clone(), true, r#"{"empty":"没有远端任务"}"#.into());
            assert_eq!(app.overlays.len(), 1);
            shows(&reader_rows(&mut app, 100, 30), &["没有远端任务", "已撤 nas 上的「备份」"]);
            // 关了以后才回来的重读:不再开一页
            app.overlays.clear();
            app.plugin_done(refresh, true, list.into());
            assert!(app.overlays.is_empty());
        });
    }

    #[test]
    fn what_a_plugin_hangs_on_a_row_follows_its_title() {
        let mut v = sessions();
        v[3].tokens = vec![("pr".into(), "#12".into()), ("ci".into(), "✓".into())];
        let mut app = App::new(String::new(), v);
        let rows = reader_rows(&mut app, 100, 30);
        assert!(rows.iter().any(|r| r.contains("闲 [pr #12] [ci ✓]")), "{rows:#?}");
    }

    /// 分叉出来的:标题后面灰字写分自谁,排在插件的字前面;标题里已经带着原会话名字的(默认起的)不再写一遍
    #[test]
    fn a_fork_says_who_it_came_from_after_its_title_unless_the_title_already_does() {
        let mut v = sessions();
        v[3].fork = Fork::Of("跑".into());
        v[3].tokens = vec![("pr".into(), "#12".into())];
        let mut app = App::new(String::new(), v.clone());
        let rows = reader_rows(&mut app, 100, 30);
        assert!(rows.iter().any(|r| r.contains("闲 分自 跑 [pr #12]")), "{rows:#?}");
        // 默认的标题「跑 (分叉)」:名字已经在标题里了
        v[3].title = "跑 (分叉)".into();
        let mut app = App::new(String::new(), v.clone());
        let rows = reader_rows(&mut app, 100, 30);
        assert!(rows.iter().any(|r| r.contains("跑 (分叉) [pr #12]")) && !rows.iter().any(|r| r.contains("分自")), "{rows:#?}");
        // 原会话删了:行上不写(预览里说)
        v[3].fork = Fork::Gone;
        let mut app = App::new(String::new(), v);
        assert!(!reader_rows(&mut app, 100, 30).iter().any(|r| r.contains("分自")));
    }
}
