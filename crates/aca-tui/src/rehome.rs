//! `X`: carry on under another account. For when an account's quota ran out before the work is done: restart in place on
//! another account of the same kind and resume this conversation (the conversation record is copied over and the context comes
//! along whole, see `aca_core::session::rehome`).
//!
//! The box shows what you need to pick an account: on top the account it uses now and that account's quota; below, only the
//! ones it can move to (other accounts of the same kind), one row each: logged in or not (asked in the background, shown the
//! frame after the answer arrives) and quota (5h / 7d used; amber past 70%, red past 90%, as on the billing page).
//! ⏎ / a digit switches directly, no second box; a logged-out one can't be picked. i logs the highlighted one in here, a opens
//! the accounts page on it, + adds an account (this box closes first; press X again once logged in).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aca_core::{t, tr};
use aca_records::agent::Agent;
use aca_records::usage::{Quota, Source, Win};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::accounts::Login;
use crate::data::{Session, State};
use crate::kit::agent_style;
use crate::ops::{Job, Target};
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;
use crate::tools::Tool;

/// Provided from outside: asking about logins, opening the accounts page. Tests swap in fakes that answer on the spot
#[derive(Clone, Copy)]
pub struct Env {
    pub ask: fn(Agent, &str) -> Login,
    /// Ask in the background (in the interface); tests ask on the spot
    pub background: bool,
    /// The accounts page, opened on an account or straight into adding one
    pub accounts: Option<fn(crate::acctpage::Opening) -> Box<dyn Overlay>>,
}

/// An account it can move to (its entry in the `n` menu)
struct Acct {
    /// The name in the menu
    name: String,
    account: String,
    /// The "tool:account" to move to
    spec: String,
    /// Why it can't start (the account isn't registered properly, the tool is misconfigured)
    problem: Option<String>,
    quota: Option<Quota>,
}

pub struct Menu {
    t: Target,
    agent: Agent,
    /// "Claude·ding"
    label: String,
    now: Option<Quota>,
    rows: Vec<Acct>,
    sel: usize,
    /// It's running / needs you to choose: switching interrupts this turn
    busy: bool,
    /// When the quota was fetched (for reset times and "numbers from N hours ago") and the local timezone offset
    at: i64,
    off: i64,
    env: Env,
    /// "tool:account" → logged in or not; ones not answered yet aren't in it
    logins: Arc<Mutex<HashMap<String, Login>>>,
    /// i on a logged-in account: asking whether to log it in again (only y does)
    relogin: bool,
}

/// The current quota of every account of this kind: Claude's as recorded by its status line (`limits`), Codex's read from its
/// latest conversation record (`usage::codex_now`; none for ones logged in with an API key, see `usage::subscribed`)
pub fn quotas(agent: Agent) -> Vec<Quota> {
    let cfg = aca_core::config::load();
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = aca_records::agent::accounts(&cfg);
    let srcs: Vec<Source> = if agent == Agent::Agy {
        let Some(src) = Source::agy(&cfg, &home, crate::accounts::on_path) else { return vec![] };
        vec![src]
    } else {
        aca_records::usage::sources(&accounts, &home)
    };
    match agent {
        Agent::Claude | Agent::Agy => aca_records::limits::dir()
            .map(|d| aca_records::limits::quotas(&d, &srcs, aca_records::limits::now()))
            .unwrap_or_default()
            .into_iter()
            .filter(|q| q.agent == agent)
            .collect(),
        Agent::Codex => {
            let now = srcs.iter().filter(|s| s.agent == Agent::Codex).filter_map(aca_records::usage::codex_now).collect();
            aca_records::usage::subscribed(now, &accounts, &home)
        }
        _ => vec![],
    }
}

/// Match quotas by account name (two accounts sharing one directory are recorded as "A·B"; either name matches)
pub fn quota_of(qs: &[Quota], agent: Agent, name: &str) -> Option<Quota> {
    qs.iter().find(|q| q.agent == agent && !name.is_empty() && (q.who == name || q.who.split('·').any(|w| w == name))).cloned()
}

/// How many columns the login cell of an account row takes: the longest of what it says and a space (English "logged out" is
/// 10 wide, the Chinese ones at most 8)
pub fn login_w() -> usize {
    if aca_core::i18n::lang() == aca_core::i18n::Lang::En { 11 } else { 10 }
}

/// The login cell of an account row, fixed width so the quotas after it line up: ✓ logged in, 未登录 (logged out), … still
/// asking, ? 未知 (can't tell)
pub fn login_cell(l: Option<&Login>) -> Span<'static> {
    let (text, st) = match l {
        None => ("…", kit::dim()),
        Some(Login::In(_)) => ("✓", Style::new().fg(theme::pal().green)),
        Some(Login::Out) => (t!("未登录", "logged out"), theme::warn().add_modifier(Modifier::BOLD)),
        Some(Login::Unknown(_)) => (t!("? 未知", "? unknown"), kit::dim()),
        Some(Login::Unsafe(_)) => (t!("⚠ 不可用", "⚠ unusable"), theme::warn()),
    };
    Span::styled(format!("{text}{}", " ".repeat(login_w().saturating_sub(text.width()))), st)
}

/// The tool cell of an account row, fixed width
pub fn tool_cell(a: Agent) -> Span<'static> {
    Span::styled(if a == Agent::Codex { "Codex   " } else { "Claude  " }, kit::dim())
}

/// The quota columns of one table of accounts (a menu, the accounts page): every window any account in it has, 5h first, then 7d,
/// the rest in the order they first come; each as wide as its widest cell, the numbers right-aligned. An account without a window
/// leaves that column blank, so a window sits in the same column on every row
#[derive(Clone, Debug, Default)]
pub struct Cols {
    /// Window name and how wide its numbers get
    wins: Vec<(String, usize)>,
    /// Some row has a login cell: a row without one (the tool's own directory) leaves that cell blank
    login: bool,
    /// The windows each account of the table has (ones with none left out): whether dropping columns leaves one with nothing
    has: Vec<Vec<String>>,
}

impl Cols {
    /// at is when the quotas were fetched (a window past its reset says so, which is wider than a number)
    pub fn of<'a>(qs: impl IntoIterator<Item = &'a Quota>, at: i64) -> Cols {
        let mut c = Cols::gather(qs, at);
        // A stable sort: the rest keep the order they came in
        c.wins.sort_by_key(|(l, _)| match l.as_str() {
            "5h" => 0,
            "7d" => 1,
            _ => 2,
        });
        c
    }

    /// One account's own windows, in the order it has them: a row on its own, or a row of a table that isn't in columns
    fn own(q: &Quota, at: i64) -> Cols {
        Cols::gather([q], at)
    }

    fn gather<'a>(qs: impl IntoIterator<Item = &'a Quota>, at: i64) -> Cols {
        let (mut wins, mut has): (Vec<(String, usize)>, Vec<Vec<String>>) = (vec![], vec![]);
        for q in qs {
            for w in &q.wins {
                let n = shown(w, at).width();
                match wins.iter_mut().find(|(l, _)| *l == w.label) {
                    Some(c) => c.1 = c.1.max(n),
                    None => wins.push((w.label.clone(), n)),
                }
            }
            if !q.wins.is_empty() {
                has.push(q.wins.iter().map(|w| w.label.clone()).collect());
            }
        }
        Cols { wins, login: false, has }
    }

    /// How many columns fit after used cells within room, and how wide they are together
    fn fit(&self, used: usize, room: usize) -> (usize, usize) {
        let (mut fit, mut cols_w) = (0, 0);
        for (i, (label, n)) in self.wins.iter().enumerate() {
            let w = if i > 0 { SEP.width() } else { 0 } + label.width() + 1 + n;
            if used + cols_w + w > room {
                break;
            }
            (fit, cols_w) = (fit + 1, cols_w + w);
        }
        (fit, cols_w)
    }

    /// Keeping only the first fit columns leaves some account of the table with none of its windows
    fn leaves_one_bare(&self, fit: usize) -> bool {
        let kept = &self.wins[..fit];
        self.has.iter().any(|h| !h.iter().any(|l| kept.iter().any(|(c, _)| c == l)))
    }

    /// Whether some row of the table has a login cell
    pub fn logins(mut self, on: bool) -> Cols {
        self.login = on;
        self
    }

    /// The windows' names, in column order (the billing page draws its own cells)
    pub fn names(self) -> Vec<String> {
        self.wins.into_iter().map(|(l, _)| l).collect()
    }
}

/// A window's number: how much is used, or that it reset (past the reset time with no new numbers: usage now is unknown)
fn shown(w: &Win, at: i64) -> String {
    if w.resets.is_some_and(|r| r <= at) { t!("已重置", "reset").to_string() } else { format!("{:.0}%", w.used) }
}

/// An account the same way wherever accounts are listed (the `n` / `H` / `^E` menus, `X`, the accounts page), after its name:
/// tool (None where a heading already says it), login (None for a row with no login of its own), quota and how old its
/// numbers are. A row on its own: its columns are its own windows (see [`account_row`] for a table)
pub fn account_tail(
    tool: Option<Agent>,
    login: Option<Option<&Login>>,
    q: Option<&Quota>,
    (at, off): (i64, i64),
    room: usize,
) -> Vec<Span<'static>> {
    let cols = q.map_or_else(Cols::default, |q| Cols::own(q, at)).logins(login.is_some());
    account_row(&cols, tool, login, q, (at, off), room)
}

/// One row of a table of accounts: tool, login, then each of the table's windows in its column, then when a used-up window resets
/// and how old the numbers are. Within room columns: what doesn't fit is dropped from the right, a whole column at a time (the
/// same columns on every row: room and what comes before are the same on each). If that would leave some account of the table
/// with none of its windows, the table isn't in columns at this width: each row writes its own windows from the left as a row
/// on its own does, a whole window at a time dropped from the right
pub fn account_row(
    cols: &Cols,
    tool: Option<Agent>,
    login: Option<Option<&Login>>,
    q: Option<&Quota>,
    (at, off): (i64, i64),
    room: usize,
) -> Vec<Span<'static>> {
    let mut v: Vec<Span<'static>> = tool.map(tool_cell).into_iter().collect();
    match login {
        Some(l) => v.push(login_cell(l)),
        None if cols.login => v.push(Span::raw(" ".repeat(login_w()))),
        None => {}
    }
    let Some(q) = q else { return v };
    let mut used: usize = v.iter().map(|s| s.content.width()).sum();
    // How many columns fit, and how wide they are together
    let (mut fit, mut cols_w) = cols.fit(used, room);
    let own;
    let cols = match cols.leaves_one_bare(fit) {
        true => {
            own = Cols::own(q, at);
            (fit, cols_w) = own.fit(used, room);
            &own
        }
        false => cols,
    };
    let mut cells = vec![];
    let mut prev = false;
    for (i, (label, n)) in cols.wins.iter().take(fit).enumerate() {
        let win = q.wins.iter().find(|w| &w.label == label);
        if i > 0 {
            cells.push(Span::styled(if prev && win.is_some() { SEP } else { "   " }, theme::faint()));
        }
        prev = win.is_some();
        let Some(win) = win else {
            cells.push(Span::raw(" ".repeat(label.width() + 1 + n)));
            continue;
        };
        let text = shown(win, at);
        let st = if win.resets.is_some_and(|r| r <= at) {
            Style::new().fg(theme::pal().green)
        } else {
            used_color(win.used).map_or(Style::new(), |c| Style::new().fg(c)).add_modifier(Modifier::BOLD)
        };
        cells.push(Span::styled(format!("{label} "), kit::dim()));
        cells.push(Span::styled(format!("{}{text}", " ".repeat(n.saturating_sub(text.width()))), st));
    }
    // Blank columns at the end are left out unless something follows them
    while cells.last().is_some_and(|s| s.content.trim().is_empty()) {
        cells.pop();
    }
    let quota_w: usize = cells.iter().map(|s| s.content.width()).sum();
    used += quota_w;
    v.extend(cells);
    let mut extra = vec![];
    if let Some(r) = q.wins.iter().filter(|w| w.used >= 100.0).filter_map(|w| w.resets).filter(|r| *r > at).max() {
        extra.push(tr!("  {} 重置", "  resets {}", crate::bill::when(r, at, off)));
    }
    if q.updated > 0 && at - q.updated > 3600 {
        extra.push(format!("    {}", crate::bill::ago(q.updated, at)));
    }
    // What follows starts after the table's columns, so it lines up too
    let mut pad = cols_w - quota_w;
    for e in extra {
        if used + pad + e.width() > room {
            break;
        }
        used += pad + e.width();
        v.push(Span::styled(format!("{}{e}", " ".repeat(std::mem::take(&mut pad))), kit::dim()));
    }
    v
}

/// Between two windows of a row
const SEP: &str = " · ";

#[cfg(test)]
pub(crate) fn test_agy_quota() -> Quota {
    let wins =
        [("5h", 1.28622), ("7d", 1.071554)].into_iter().map(|(label, used)| Win { label: label.into(), used, resets: None }).collect();
    Quota { agent: Agent::Agy, who: "agy".into(), plan: "Google AI Pro".into(), wins, updated: 0, note: String::new() }
}

/// Amber past 70% used, red past 90%, as on the billing page
fn used_color(used: f64) -> Option<ratatui::style::Color> {
    let p = theme::pal();
    match used {
        u if u >= 90.0 => Some(p.red),
        u if u >= 70.0 => Some(p.amber),
        _ => None,
    }
}

/// One account's quota on its own line: "5h 98% · 7d 61%  resets today 17:00    3 hours ago" (see [`account_row`])
pub fn quota_spans(q: Option<&Quota>, at: i64, off: i64) -> Vec<Span<'static>> {
    account_tail(None, None, q, (at, off), usize::MAX)
}

/// Wrap a quota field on a phone, keeping the window and percentage styles across lines.
pub fn wrap_quota(spans: Vec<Span<'static>>, room: usize) -> Vec<Line<'static>> {
    let line = Line::from(spans);
    if line.width() <= room {
        return vec![line];
    }
    let styled = line.spans.into_iter().map(|s| crate::ansi::Seg { text: s.content.into_owned(), style: s.style, link: None }).collect();
    crate::ansi::wrap(&[styled], room).iter().map(crate::ansi::to_line).collect()
}

impl Menu {
    /// s is the highlighted session (a registered Claude / Codex), tools the `n` menu, quotas the quota of each account of this
    /// kind
    pub fn new(t: Target, s: &Session, tools: Vec<Tool>, quotas: &[Quota], (at, off): (i64, i64), env: Env) -> Menu {
        let current = if s.account.is_empty() { s.who.as_str() } else { s.account.as_str() };
        let rows = tools
            .into_iter()
            .filter(|x| x.agent == s.agent && !(x.tool == s.tool && x.account == s.account))
            .map(|x| Acct {
                spec: if x.account.is_empty() { x.tool.clone() } else { format!("{}:{}", x.tool, x.account) },
                quota: quota_of(quotas, s.agent, if x.account.is_empty() { &x.name } else { &x.account }),
                name: x.name,
                account: x.account,
                problem: x.problem,
            })
            .collect();
        let m = Menu {
            t,
            agent: s.agent,
            label: s.agent_label(),
            now: quota_of(quotas, s.agent, current),
            rows,
            sel: 0,
            busy: matches!(s.state, State::Running | State::Ask),
            at,
            off,
            env,
            logins: Arc::default(),
            relogin: false,
        };
        m.ask_logins();
        m
    }

    fn ask_logins(&self) {
        for r in self.rows.iter().filter(|r| r.problem.is_none() && !r.account.is_empty()) {
            let (logins, ask, agent, spec, account) = (self.logins.clone(), self.env.ask, self.agent, r.spec.clone(), r.account.clone());
            let go = move || {
                let got = ask(agent, &account);
                logins.lock().unwrap_or_else(|e| e.into_inner()).insert(spec, got);
            };
            if self.env.background {
                std::thread::spawn(go);
            } else {
                go();
            }
        }
    }

    fn login(&self, r: &Acct) -> Option<Login> {
        self.logins.lock().unwrap_or_else(|e| e.into_inner()).get(&r.spec).cloned()
    }

    /// Move over: not for one that can't start or isn't logged in; say why
    fn go(&mut self, i: usize, fx: &mut Fx) -> Flow {
        self.sel = i;
        let r = &self.rows[i];
        if let Some(p) = &r.problem {
            fx.flash = Some(p.clone());
            return Flow::Stay;
        }
        if self.login(r) == Some(Login::Out) {
            fx.flash = Some(tr!("{} 未登录 · i 登录", "{} isn't logged in · i logs in", r.name));
            return Flow::Stay;
        }
        fx.job = Some(Job::Rehome { t: self.t.clone(), to: r.spec.clone(), label: r.name.clone() });
        Flow::Close
    }

    fn quota_spans(&self, q: Option<&Quota>) -> Vec<Span<'static>> {
        quota_spans(q, self.at, self.off)
    }

    /// i: log the highlighted account in, here, as on the accounts page; a logged-in one (or one not asked about yet) asks first
    fn ask_login(&mut self, fx: &mut Fx) {
        let Some(r) = self.rows.get(self.sel) else { return };
        if let Some(p) = &r.problem {
            return fx.say(p.clone());
        }
        if r.account.is_empty() {
            return fx.say(tr!(
                "{} 用的是工具自己的目录,在账号页登录",
                "{} uses the tool's own directory: log in on the accounts page",
                r.name
            ));
        }
        match self.login(r) {
            Some(Login::Unsafe(why)) => fx.say(why),
            Some(Login::Out) => self.start_login(fx),
            _ => self.relogin = true,
        }
    }

    /// The highlighted account's own login, in a terminal at the bottom; this box closes (press X again once logged in)
    fn start_login(&self, fx: &mut Fx) {
        let Some(r) = self.rows.get(self.sel) else { return };
        // Not answered yet when y was pressed: look at the directory once more before writing into it
        if let Some(why) = crate::accounts::unsafe_dir(self.agent, &r.account) {
            return fx.say(why);
        }
        let (home, remote) = (std::env::var("HOME").unwrap_or_default(), aca_core::agent::remote(&aca_core::config::load()));
        let run = crate::accounts::relogin_cmd(self.agent, &r.account);
        fx.term = Some(crate::acctpage::login_term(&aca_core::paths::state_dir(), &home, remote, &r.account, self.agent, run));
    }
}

/// X's keys, for the bottom bar and `?`
fn keys() -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    vec![
        key("⏎", t!("切换", "switch")),
        key("i", t!("登录", "log in")),
        key("a", t!("账号页", "accounts")),
        key("+", t!("添加账号", "add account")),
        more("1-9", t!("切换到这一个", "switch to this one")),
        more("↑↓", t!("选择", "select")),
        more("Esc", t!("取消", "cancel")),
    ]
}

impl Overlay for Menu {
    fn width(&self) -> u16 {
        68
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let title = tr!("「{}」切换账号继续", "Carry “{}” on under another account", self.t.title);
        let mut out = vec![kit::modal_title(&title, theme::accent(), kit::esc_hint(t!("取消", "cancel")), w), Line::raw("")];
        let mut now = vec![
            Span::styled(t!("当前  ", "now   "), kit::dim()),
            Span::styled(self.label.clone(), agent_style(self.agent).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
        ];
        now.extend(self.quota_spans(self.now.as_ref()));
        out.extend([Line::from(now), Line::raw("")]);
        if self.rows.is_empty() {
            let kind = if self.agent == Agent::Codex { "Codex" } else { "Claude" };
            out.push(Line::raw(tr!("没有其他 {kind} 账号 · + 添加", "No other {kind} account · + adds one")));
            return out;
        }
        let name_w = self.rows.iter().map(|r| r.name.width()).max().unwrap_or(0).clamp(4, 12);
        // The accounts it can move to are one table: each window in its own column
        let usable = || self.rows.iter().filter(|r| r.problem.is_none());
        let cols = Cols::of(usable().filter_map(|r| r.quota.as_ref()), self.at).logins(usable().any(|r| !r.account.is_empty()));
        for (i, r) in self.rows.iter().enumerate() {
            let on = i == self.sel;
            let name = kit::clip(&r.name, name_w);
            let name_st = if r.problem.is_some() { kit::dim() } else { agent_style(self.agent).add_modifier(Modifier::BOLD) };
            let mut line = vec![
                Span::raw("  "),
                Span::styled(if i < 9 { format!("{} ", i + 1) } else { "  ".into() }, kit::dim()),
                Span::styled(format!("{name}{}", " ".repeat(name_w.saturating_sub(name.width()))), name_st),
                Span::raw("  "),
            ];
            match &r.problem {
                Some(p) => line.push(Span::styled(p.clone(), theme::warn())),
                None => {
                    let login = (!r.account.is_empty()).then(|| self.login(r));
                    let room = w.saturating_sub(line.iter().map(|s| s.content.width()).sum());
                    let login = login.as_ref().map(Option::as_ref);
                    line.extend(account_row(&cols, Some(self.agent), login, r.quota.as_ref(), (self.at, self.off), room))
                }
            }
            out.push(kit::picked(Line::from(line), w, on));
        }
        out.push(Line::raw(""));
        // i on a logged-in account: what logging in again loses, only y goes ahead
        if let Some(r) = self.rows.get(self.sel).filter(|_| self.relogin) {
            out.extend(crate::acctpage::relogin_lines(self.agent, &r.name, self.login(r).as_ref(), w));
            return out;
        }
        if self.busy {
            out.push(Line::styled(t!("⚠ 切换将中断当前任务", "⚠ switching interrupts this turn"), theme::warn()));
            out.push(Line::raw(""));
        }
        out.push(crate::kit::keys_line_help(&crate::sheet::foot(&keys()), vec![], w));
        out
    }

    /// Asking whether to log in again: ? is "any other key" there
    fn sheet(&self) -> Option<Vec<crate::sheet::Key>> {
        (!self.relogin).then(keys)
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        if std::mem::take(&mut self.relogin) {
            if k.code != KeyCode::Char('y') {
                return Flow::Stay;
            }
            self.start_login(fx);
            return Flow::Close;
        }
        let n = self.rows.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q' | 'X') => return Flow::Close,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Enter if n > 0 => return self.go(self.sel, fx),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => return self.go(c as usize - '1' as usize, fx),
            // Logging in starts here; once logged in, X again (login state is asked again)
            KeyCode::Char('i') if n > 0 => {
                self.ask_login(fx);
                if fx.term.is_some() {
                    return Flow::Close;
                }
            }
            // The accounts page, on the highlighted account; or straight into adding one. This box closes
            KeyCode::Char(c @ ('a' | '+')) => {
                if let Some(page) = self.env.accounts {
                    let at = self.rows.get(self.sel).filter(|r| !r.account.is_empty());
                    fx.open = Some(page(match (c, at) {
                        ('+', _) => crate::acctpage::Opening::Adding,
                        (_, Some(r)) => crate::acctpage::Opening::At(self.agent, r.account.clone()),
                        _ => crate::acctpage::Opening::First,
                    }));
                    return Flow::Close;
                }
            }
            _ => {}
        }
        Flow::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aca_core::i18n::{self, Lang};

    fn quota(who: &str, wins: &[(&str, f64)]) -> Quota {
        let wins = wins.iter().map(|&(label, used)| Win { label: label.into(), used, resets: None }).collect();
        Quota { agent: Agent::Claude, who: who.into(), plan: String::new(), wins, updated: 0, note: String::new() }
    }

    #[test]
    fn the_login_cell_is_one_width_for_every_state_with_a_space_after_it() {
        for lg in [Lang::Zh, Lang::En] {
            i18n::with(lg, || {
                let all = [
                    None,
                    Some(Login::In(String::new())),
                    Some(Login::Out),
                    Some(Login::Unknown(String::new())),
                    Some(Login::Unsafe(String::new())),
                ];
                for l in &all {
                    let cell = login_cell(l.as_ref());
                    assert!(cell.content.width() == login_w() && cell.content.ends_with(' '), "{lg:?}: {:?}", cell.content);
                }
                assert_eq!(login_cell(Some(&Login::Out)).content.trim_end(), if lg == Lang::Zh { "未登录" } else { "logged out" });
            });
        }
    }

    #[test]
    fn a_table_puts_each_window_in_one_column_and_drops_the_same_columns_when_narrow() {
        i18n::with(Lang::Zh, || {
            let qs = [
                quota("a", &[("5h", 5.0), ("7d", 57.0), ("Fable", 31.0)]),
                quota("b", &[("Fable", 9.0), ("7d", 100.0)]),
                quota("c", &[("7d", 7.0)]),
            ];
            let cols = Cols::of(&qs, 0).logins(true);
            let login = Login::In(String::new());
            let row = |q: &Quota, own: bool, room: usize| -> String {
                let login = own.then_some(Some(&login));
                account_row(&cols, Some(Agent::Claude), login, Some(q), (0, 0), room).iter().map(|s| s.content.to_string()).collect()
            };
            // The tool's own directory has no login of its own: its cell is left blank, the quota still in its columns
            let rows: Vec<String> = [(&qs[0], true), (&qs[1], true), (&qs[2], false)].iter().map(|(q, own)| row(q, *own, 80)).collect();
            assert_eq!(
                rows,
                [
                    "Claude  ✓         5h 5% · 7d  57% · Fable 31%",
                    "Claude  ✓                 7d 100% · Fable  9%",
                    "Claude                    7d   7%",
                ]
                .map(String::from),
                "{rows:#?}"
            );
            // Narrow: the same column goes on every row (Fable), not each row's own last one
            let narrow: Vec<String> = qs.iter().map(|q| row(q, true, 40)).collect();
            assert!(narrow.iter().all(|r| !r.contains("Fable") && r.width() <= 40), "{narrow:#?}");
            assert!(narrow[1].ends_with("7d 100%") && narrow[2].ends_with("7d   7%"), "{narrow:#?}");
            // One account alone is as compact as before
            assert_eq!(quota_spans(Some(&qs[2]), 0, 0).iter().map(|s| s.content.to_string()).collect::<String>(), "7d 7%");
        });
    }

    #[test]
    fn when_dropping_columns_would_leave_an_account_with_nothing_each_row_writes_its_own() {
        i18n::with(Lang::Zh, || {
            let at = 1_000_000;
            let win =
                |label: &str, used, reset: bool| Win { label: label.into(), used, resets: Some(if reset { at - 60 } else { at + 3600 }) };
            let q = |who: &str, wins| Quota {
                agent: Agent::Claude,
                who: who.into(),
                plan: String::new(),
                wins,
                updated: 0,
                note: String::new(),
            };
            let qs = [
                q("ding", vec![win("5h", 14.0, false), win("7d", 58.0, false)]),
                q("sub", vec![win("7d", 44.0, false)]),
                q("codex", vec![win("5h", 0.0, true), win("7d", 0.0, true)]),
            ];
            // No tool or login cell (as on a page whose heading says the tool): only the quota is looked at here
            let cols = Cols::of(&qs, at);
            let rows = |room: usize| -> Vec<String> {
                qs.iter()
                    .map(|x| {
                        let spans = account_row(&cols, None, None, Some(x), (at, 0), room);
                        spans.iter().map(|s| s.content.to_string()).collect::<String>().trim_end().to_string()
                    })
                    .collect()
            };
            // Room for both columns: in columns, sub's 7d under the others'
            assert_eq!(rows(80), ["5h    14% · 7d    58%", "            7d    44%", "5h 已重置 · 7d 已重置"].map(String::from));
            // Room for one column only: keeping 5h would leave sub with nothing, so each row writes its own windows from the left
            let one = rows(9 + 2);
            assert_eq!(one, ["5h 14%", "7d 44%", "5h 已重置"].map(String::from), "{one:#?}");
            // Every account keeps one of its windows in what's left: still in columns, the same column dropped on every row
            let qs = [
                q("a", vec![win("5h", 5.0, false), win("7d", 57.0, false), win("Fable", 31.0, false)]),
                q("b", vec![win("7d", 8.0, false)]),
            ];
            let cols = Cols::of(&qs, at);
            let b = account_row(&cols, None, None, Some(&qs[1]), (at, 0), 18).iter().map(|s| s.content.to_string()).collect::<String>();
            assert_eq!(b.trim_end(), "        7d  8%");
        });
    }
}
