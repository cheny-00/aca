//! `X`: carry on under another account. For when an account's quota ran out before the work is done: restart in place on
//! another account of the same kind and resume this conversation (the conversation record is copied over and the context comes
//! along whole, see `aca_core::session::rehome`).
//!
//! The box shows what you need to pick an account: on top the account it uses now and that account's quota; below, only the
//! ones it can move to (other accounts of the same kind), one row each: logged in or not (asked in the background, shown the
//! frame after the answer arrives) and quota (5h / 7d used; amber past 70%, red past 90%, as on the billing page).
//! ⏎ / a digit switches directly, no second box; a logged-out one can't be picked. a goes to the accounts page to log in, +
//! adds an account (this box closes first; press X again once logged in).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aca_core::{t, tr};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::accounts::Login;
use crate::data::{Agent, Session, State};
use crate::ops::{Job, Target};
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;
use crate::tools::Tool;
use crate::ui::agent_style;
use crate::usage::{Quota, Source};

/// Provided from outside: asking about logins, opening the accounts page. Tests swap in fakes that answer on the spot
#[derive(Clone, Copy)]
pub struct Env {
    pub ask: fn(Agent, &str) -> Login,
    /// Ask in the background (in the interface); tests ask on the spot
    pub background: bool,
    /// The accounts page; the argument = go straight to adding an account
    pub accounts: Option<fn(bool) -> Box<dyn Overlay>>,
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
}

/// The current quota of every account of this kind: Claude's as recorded by its status line (`limits`), Codex's read from its
/// latest conversation record (`usage::codex_now`; none for ones logged in with an API key, see `usage::subscribed`)
pub fn quotas(agent: Agent) -> Vec<Quota> {
    let cfg = aca_core::config::load();
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = crate::data::accounts(&cfg);
    let srcs: Vec<Source> = crate::usage::sources(&accounts, &home);
    match agent {
        Agent::Claude => crate::limits::dir()
            .map(|d| crate::limits::quotas(&d, &srcs, crate::limits::now()))
            .unwrap_or_default()
            .into_iter()
            .filter(|q| q.agent == Agent::Claude)
            .collect(),
        Agent::Codex => {
            let now = srcs.iter().filter(|s| s.agent == Agent::Codex).filter_map(crate::usage::codex_now).collect();
            crate::usage::subscribed(now, &accounts, &home)
        }
        _ => vec![],
    }
}

/// Match quotas by account name (two accounts sharing one directory are recorded as "A·B"; either name matches)
pub fn quota_of(qs: &[Quota], agent: Agent, name: &str) -> Option<Quota> {
    qs.iter().find(|q| q.agent == agent && !name.is_empty() && (q.who == name || q.who.split('·').any(|w| w == name))).cloned()
}

/// "5h 98% · 7d 61%  resets today 17:00": colors as on the billing page; a used-up window says when it resets, numbers over an
/// hour old say so; nothing recorded = empty.
/// at is when the quota was fetched, off the local timezone offset. The `n` menu uses it too
pub fn quota_spans(q: Option<&Quota>, at: i64, off: i64) -> Vec<Span<'static>> {
    let Some(q) = q else { return vec![] };
    let p = theme::pal();
    let mut v = vec![];
    for (i, win) in q.wins.iter().enumerate() {
        if i > 0 {
            v.push(Span::styled(" · ", theme::faint()));
        }
        v.push(Span::styled(format!("{} ", win.label), kit::dim()));
        // Past the reset time with no new numbers: usage now is unknown, only that it reset
        if win.resets.is_some_and(|r| r <= at) {
            v.push(Span::styled(t!("已重置", "reset"), Style::new().fg(p.green)));
            continue;
        }
        let color = match win.used {
            u if u >= 90.0 => Some(p.red),
            u if u >= 70.0 => Some(p.amber),
            _ => None,
        };
        v.push(Span::styled(format!("{:.0}%", win.used), color.map_or(Style::new(), |c| Style::new().fg(c)).add_modifier(Modifier::BOLD)));
    }
    let full = q.wins.iter().filter(|w| w.used >= 100.0).filter_map(|w| w.resets).filter(|r| *r > at).max();
    if let Some(r) = full {
        v.push(Span::styled(tr!("  {} 重置", "  resets {}", crate::bill::when(r, at, off)), kit::dim()));
    }
    if q.updated > 0 && at - q.updated > 3600 {
        v.push(Span::styled(tr!("  ({}的数据)", "  (as of {})", crate::bill::ago(q.updated, at)), kit::dim()));
    }
    v
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
            fx.flash = Some(tr!("{} 未登录 · a 登录", "{} isn't logged in · a to log in", r.name));
            return Flow::Stay;
        }
        fx.job = Some(Job::Rehome { t: self.t.clone(), to: r.spec.clone(), label: r.name.clone() });
        Flow::Close
    }

    fn quota_spans(&self, q: Option<&Quota>) -> Vec<Span<'static>> {
        quota_spans(q, self.at, self.off)
    }

    /// The login column, fixed width (so the quotas after it line up)
    fn login_span(&self, r: &Acct) -> Span<'static> {
        let p = theme::pal();
        let (text, st) = match (&r.problem, r.account.is_empty(), self.login(r)) {
            (Some(_), _, _) => (t!("不可用", "unusable"), theme::warn()),
            (_, true, _) => ("", Style::new()),
            (_, _, None) => ("…", kit::dim()),
            (_, _, Some(Login::In(_))) => (t!("● 已登录", "● logged in"), Style::new().fg(p.green)),
            (_, _, Some(Login::Out)) => (t!("○ 未登录", "○ logged out"), theme::warn().add_modifier(Modifier::BOLD)),
            (_, _, Some(Login::Unknown(_))) => (t!("? 未知", "? unknown"), kit::dim()),
        };
        Span::styled(format!("{text}{}", " ".repeat(LOGIN_W.saturating_sub(text.width()))), st)
    }
}

/// How many columns the login column takes
const LOGIN_W: usize = 14;

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
        for (i, r) in self.rows.iter().enumerate() {
            let on = i == self.sel;
            let name = kit::clip(&r.name, name_w);
            let name_st = if r.problem.is_some() { kit::dim() } else { agent_style(self.agent).add_modifier(Modifier::BOLD) };
            let mut line = vec![
                Span::styled(if on { "› " } else { "  " }, theme::accent()),
                Span::styled(if i < 9 { format!("{} ", i + 1) } else { "  ".into() }, kit::dim()),
                Span::styled(format!("{name}{}", " ".repeat(name_w.saturating_sub(name.width()))), name_st),
                Span::raw("  "),
                self.login_span(r),
            ];
            match &r.problem {
                Some(p) => line.push(Span::styled(p.clone(), theme::warn())),
                None => line.extend(self.quota_spans(r.quota.as_ref())),
            }
            out.push(kit::picked(Line::from(line), w, on));
        }
        out.push(Line::raw(""));
        if self.busy {
            out.push(Line::styled(t!("⚠ 切换将中断当前任务", "⚠ switching interrupts this turn"), theme::warn()));
            out.push(Line::raw(""));
        }
        out.push(kit::keys_line(
            &[("⏎", t!("切换", "switch")), ("a", t!("账号页", "accounts")), ("+", t!("添加账号", "add account"))],
            vec![],
            w,
        ));
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let n = self.rows.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Close,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Enter if n > 0 => return self.go(self.sel, fx),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => return self.go(c as usize - '1' as usize, fx),
            // Logging in / adding an account happens on the accounts page; this box closes, press X again once logged in
            // (login state is asked again)
            KeyCode::Char(c @ ('a' | '+')) => {
                if let Some(page) = self.env.accounts {
                    fx.open = Some(page(c == '+'));
                    return Flow::Close;
                }
            }
            _ => {}
        }
        Flow::Stay
    }
}
