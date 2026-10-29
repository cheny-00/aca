//! Accounts page: `^S` → `a`; `+` in the `n` menu also lands here (straight into adding an account).
//!
//! Like the plugins page it's a big fixed-size popup: the left column lists the registered accounts in two sections, Claude Code
//! and Codex, one row each: "name  logged in or not" (asked of the tool itself, see `login` in accounts.rs); the right column is
//! the highlighted account's whole page — config directory, who's logged in, quota (same as the `n` menu), whose status bar is in
//! use now, launch args; sessions using it, being the default account, or being hidden from the `n` menu are written only when
//! so; a missing directory, or one that actually belongs to the other tool, gets told what to do here.
//! Section headers (Claude Code, Codex) can be highlighted too: the right side shows the launch args every account of that tool
//! gets (`[tools.<tool>] args`); for a tool with no registered accounts, also whether the directory used without one is logged
//! in. A tool that's installed but has no registered accounts still gets a section header.
//! On a narrow screen (phone) the two columns stack. The add-account form and the confirm-before-remove also sit in the right
//! column; the frame doesn't change.
//!
//! ⏎ log in (opens a terminal in the bottom half running the tool's own login) · `+` add · `d` remove (first says what it
//! touches and who uses it; only y removes; it drops just the registry line, the directory stays) ·
//! ⇧↑ ⇧↓ reorder (that's the `n` menu order, kept in `[ui] menu`) · `p` edit launch args (right in the right column): on an
//! account, its own, saved to its line in config.toml; on a section header, those every account of the tool gets ·
//! `s` each account's status bar (the table in setup.rs) · `r` ask again for logins and quotas.
//!
//! [`Form`] is the three steps of adding an account (which tool → name → config directory); the `aca setup` table uses it too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aca_core::{t, tr};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::accounts::{self, Account, Login};
use crate::data::Agent;
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::plugins::{DIVIDER, SPLIT_W, flat, pad, scroll};
use crate::setup::{fold, tail};
use crate::theme;
use crate::ui::{TermReq, agent_style};
use crate::usage::Quota;

/// Which tool, in headers and forms: its color, bold (Claude Code vs Codex at a glance)
pub(crate) fn who_span(a: Agent) -> Span<'static> {
    Span::styled(accounts::who(a), agent_style(a).add_modifier(Modifier::BOLD))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Tool,
    Name,
    Dir,
}

/// The three steps of adding an account. Which tool is its own step, picked from two rows: it used to be a Tab on the name
/// step, differing by just one word in the title, which made it easy to register a Codex directory as a Claude account
#[derive(Clone, Debug)]
pub(crate) struct Form {
    step: Step,
    pub agent: Agent,
    /// Set once the name step is done
    pub name: String,
    /// Text in the input box: the name on the name step, the directory on the directory step
    pub text: String,
    pub hint: String,
}

/// What one key did to the form
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Went {
    Stay,
    /// Esc on the first step: not adding after all
    Left,
    /// Enter after the directory: `agent` / `name` / `text` go to the page that opened the form to check and carry out; if that
    /// fails it writes `hint` and the form stays
    Filled,
}

impl Form {
    pub fn new() -> Form {
        Form { step: Step::Tool, agent: Agent::Claude, name: String::new(), text: String::new(), hint: String::new() }
    }

    /// Typing (not on the tool step: there j / k pick)
    pub fn typing(&self) -> bool {
        self.step != Step::Tool
    }

    /// `taken`: this tool already has an account by that name
    pub fn key(&mut self, k: KeyEvent, taken: impl Fn(Agent, &str) -> bool) -> Went {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.step == Step::Tool {
            match k.code {
                KeyCode::Esc => return Went::Left,
                KeyCode::Enter => self.step = Step::Name,
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => self.other(),
                KeyCode::Char('j' | 'k' | 'h' | 'l' | ' ') if !ctrl => self.other(),
                _ => {}
            }
            return Went::Stay;
        }
        match k.code {
            // Back one step: directory → name → which tool
            KeyCode::Esc => {
                if self.step == Step::Dir {
                    (self.step, self.text) = (Step::Name, std::mem::take(&mut self.name));
                } else {
                    self.step = Step::Tool;
                }
                self.hint.clear();
            }
            KeyCode::Enter if self.step == Step::Dir => return Went::Filled,
            KeyCode::Enter => {
                let name = self.text.trim().to_string();
                if name.is_empty() {
                    self.hint = t!("账号名不能为空", "the account name is empty").into();
                } else if !accounts::valid_name(&name) {
                    self.hint = t!("账号名只能包含字母、数字、- 和 _", "an account name is letters, digits, - and _").into();
                } else if taken(self.agent, &name) {
                    self.hint = tr!("账号已存在:{name}", "that account already exists: {name}");
                } else {
                    (self.step, self.text, self.hint) = (Step::Dir, accounts::default_dir(self.agent, &name), String::new());
                    self.name = name;
                }
            }
            KeyCode::Backspace => {
                self.text.pop();
                self.hint.clear();
            }
            KeyCode::Char('u') if ctrl => self.text.clear(),
            KeyCode::Char(c) if !ctrl => {
                self.text.push(c);
                self.hint.clear();
            }
            _ => {}
        }
        Went::Stay
    }

    fn other(&mut self) {
        self.agent = if self.agent == Agent::Codex { Agent::Claude } else { Agent::Codex };
    }

    /// `done`: what Enter on the directory step is labeled ("add" / "add to the table")
    pub fn lines(&self, w: usize, done: &str) -> Vec<Line<'static>> {
        let bold = theme::accent().add_modifier(Modifier::BOLD);
        if self.step == Step::Tool {
            let mut out = vec![Line::styled(t!("添加账号 · 选择工具", "Add an account · for which tool"), bold)];
            let name_w = accounts::who(Agent::Claude).width();
            for a in [Agent::Claude, Agent::Codex] {
                let on = a == self.agent;
                let note = tr!("默认目录 {}", "directory defaults to {}", accounts::default_dir(a, t!("<名字>", "<NAME>")));
                let mut name = who_span(a);
                name.content = format!("{:name_w$}", name.content).into();
                let room = w.saturating_sub(2 + name_w + 2);
                let line = Line::from(vec![
                    Span::styled(if on { "› " } else { "  " }, theme::accent()),
                    name,
                    Span::raw("  "),
                    Span::styled(kit::clip(&note, room).trim_end().to_string(), kit::dim()),
                ]);
                out.push(kit::picked(line, w, on));
            }
            out.push(Line::raw(""));
            out.push(kit::keys_line(&[("⏎", t!("下一步", "next")), ("↑↓", t!("切换", "switch")), ("Esc", t!("返回", "back"))], vec![], w));
            return out;
        }
        let (mut title, tip, enter) = match self.step {
            Step::Dir => (
                vec![Span::styled(tr!(" 账号 · {} · 目录", " account · {} · directory", self.name), bold)],
                t!("不存在时自动创建", "Created if missing"),
                done,
            ),
            _ => (
                vec![Span::styled(t!(" 账号 · 名字", " account · name"), bold)],
                t!("字母、数字、- 和 _", "Letters, digits, - and _"),
                t!("下一步", "next"),
            ),
        };
        title.splice(0..0, [Span::styled(t!("添加 ", "Add a "), bold), who_span(self.agent)]);
        let input = vec![Span::styled("> ", bold), Span::raw(tail(&self.text, w.saturating_sub(3))), Span::styled("▏", theme::accent())];
        let mut out = vec![Line::from(title), Line::from(input)];
        if self.hint.is_empty() {
            out.push(Line::styled(kit::clip(tip, w).trim_end().to_string(), kit::dim()));
        } else {
            out.extend(fold(&self.hint, w).into_iter().map(|l| Line::styled(l, theme::warn())));
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("⏎", enter), ("Esc", t!("返回", "back"))], vec![], w));
        out
    }
}

/// Where it reads and writes (tests swap in temp directories and fake logins)
#[derive(Clone)]
pub struct Env {
    pub config: PathBuf,
    pub home: String,
    /// Ask the tool whether this account is logged in
    pub ask: fn(Agent, &str) -> Login,
    /// Ask in the background and show the answer on the next frame (in the UI); tests ask on the spot
    pub background: bool,
    /// Login links are handed over through files in this directory (see `aca_core::agent::LOGIN_LINK`)
    pub links: PathBuf,
    /// The user isn't at this machine (`aca_core::agent::remote`): Claude doesn't hand over a link either; it's read off the screen
    pub remote: bool,
    /// Whether this program is installed (with no accounts registered, the installed tool's own one is listed)
    pub installed: fn(&str) -> bool,
    /// Each account's subscription quota (Claude's as recorded by the status bar / mod, Codex's read from its latest transcript);
    /// fetched in the background, like logins
    pub quotas: fn() -> Vec<Quota>,
}

/// A registered session: which tool, the account it uses, its title
pub(crate) type Use = (Agent, String, String);

struct Row {
    a: Account,
    /// The config directory exists (an account that doesn't switch directories counts as existing)
    there: bool,
    /// The directory belongs to the other tool: which one, and why we think so
    foreign: Option<(Agent, String)>,
    /// Titles of the sessions using it
    used: Vec<String>,
    /// Tools that use it when no account is given
    default_of: Vec<String>,
    /// Listed in the `n` menu (hidden ones in `[ui] hide` don't count)
    in_menu: bool,
    /// Claude accounts with a sound directory: whose status bar is in use now, and whether aca gets to record the quota
    bar: Option<(&'static str, bool)>,
    /// The account's own launch args (`[accounts.<tool>] <name> = { args = […] }`)
    args: Vec<String>,
}

/// A tool listed with a section header
struct Fam {
    agent: Agent,
    /// Launch args every account of this tool gets (`[tools.<tool>] args`); at launch they go before the account's own
    args: Vec<String>,
}

/// Something in the left column that can be highlighted: a tool's section header, or an account (index into `rows`)
#[derive(Clone, Copy, Debug, PartialEq)]
enum Item {
    Head(Agent),
    Acct(usize),
}

impl Row {
    fn spec(&self) -> String {
        format!("{}:{}", accounts::fam(self.a.agent).unwrap_or("claude"), self.a.name)
    }

    /// Login can be asked: the directory exists and belongs to its own tool (asking about a missing directory would create it; it
    /// shouldn't run in the other tool's directory)
    fn askable(&self) -> bool {
        self.there && self.foreign.is_none()
    }
}

/// What the right column is asking, waiting for y
#[derive(Clone, Copy, Debug, PartialEq)]
enum Asking {
    /// Pressed d: remove this registration
    Remove,
    /// ⏎ on a logged-in account: log in again. Once the tool's login starts, the old login is gone (tested on Codex 0.162:
    /// `codex login` deletes auth.json as soon as it starts, so not finishing in the browser means logged out), hence asking first
    Relogin,
}

/// The tool's own one, used when unregistered; its login is kept under this name (`aca run claude` with no account)
fn own_spec(a: Agent) -> String {
    accounts::fam(a).unwrap_or("claude").to_string()
}

/// How a login answer is shown: mark, a phrase, color. None = still asking
fn login_said(l: Option<Login>) -> (&'static str, String, Style) {
    match l {
        None => ("…", t!("查询中", "asking").into(), kit::dim()),
        Some(Login::In(who)) if who.is_empty() => ("●", t!("已登录", "logged in").into(), Style::new().fg(theme::pal().green)),
        Some(Login::In(who)) => ("●", who, Style::new().fg(theme::pal().green)),
        Some(Login::Out) => ("○", t!("未登录", "not logged in").into(), theme::warn()),
        Some(Login::Unknown(said)) => ("?", said, kit::dim()),
    }
}

/// At most this many session titles in the right column
const TITLES: usize = 5;
/// Max rows of the add-account form (when the hint wraps to two lines)
const FORM_ROWS: usize = 7;
/// Max length of the launch args line
const ARGS_MAX: usize = 2000;

/// Labels of the fields in the right column
fn labels() -> [&'static str; 7] {
    [
        t!("目录", "directory"),
        t!("登录", "login"),
        t!("额度", "quota"),
        t!("状态栏", "status bar"),
        t!("启动参数", "launch args"),
        t!("会话", "sessions"),
        t!("n 菜单", "n menu"),
    ]
}

/// Width of the label column
fn label_w() -> usize {
    labels().iter().map(|l| l.width()).max().unwrap_or(0) + 2
}

/// One field in the right column: "label  value"; a long value wraps below, aligned with the value; lead is the mark before
/// the value (● ○ ⚠)
fn field(out: &mut Vec<Line<'static>>, w: usize, lw: usize, label: &str, lead: Option<(&str, Style)>, text: &str, st: Style) {
    let lead_w = lead.map_or(0, |(m, _)| m.width() + 1);
    for (i, part) in fold(text, w.saturating_sub(lw + lead_w)).into_iter().enumerate() {
        let mut spans = vec![Span::styled(kit::clip(if i == 0 { label } else { "" }, lw), kit::dim())];
        match lead {
            Some((m, mst)) if i == 0 => spans.push(Span::styled(format!("{m} "), mst)),
            Some(_) => spans.push(Span::raw(" ".repeat(lead_w))),
            None => {}
        }
        spans.push(Span::styled(part, st));
        out.push(Line::from(spans));
    }
}

/// While edited, launch args are one line of text, split the shell way: separated by spaces, ones with spaces quoted (`'…'`
/// verbatim, `\"` `\\` escaped inside `"…"`). An unmatched quote = Err
fn split(s: &str) -> Result<Vec<String>, String> {
    let unclosed = || t!("引号不成对", "a quote isn't closed").to_string();
    let mut out = vec![];
    // None = not inside a word (adjacent quotes and characters form one word: `--x='a b'`)
    let mut cur: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            out.extend(cur.take());
            continue;
        }
        let w = cur.get_or_insert_with(String::new);
        match c {
            '\'' => loop {
                match chars.next() {
                    Some('\'') => break,
                    Some(x) => w.push(x),
                    None => return Err(unclosed()),
                }
            },
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some(x @ ('"' | '\\')) => w.push(x),
                        Some(x) => w.extend(['\\', x]),
                        None => return Err(unclosed()),
                    },
                    Some(x) => w.push(x),
                    None => return Err(unclosed()),
                }
            },
            '\\' => w.push(chars.next().unwrap_or('\\')),
            c => w.push(c),
        }
    }
    out.extend(cur);
    Ok(out)
}

/// Join args back into one line: empty ones and ones with spaces or quotes go in single quotes, so `split` gives back the same
fn join(args: &[String]) -> String {
    let quote = |a: &String| {
        if !a.is_empty() && a.chars().all(|c| !c.is_whitespace() && !matches!(c, '\'' | '"' | '\\')) {
            a.clone()
        } else {
            format!("'{}'", a.replace('\'', "'\\''"))
        }
    };
    args.iter().map(quote).collect::<Vec<_>>().join(" ")
}

pub struct Page {
    env: Env,
    rows: Vec<Row>,
    /// Tools listed with a section header: those with accounts, installed, or with launch args set
    fams: Vec<Fam>,
    /// What each tool uses when unregistered (`accounts::defaults`): for a tool with no registered accounts, its section header
    /// page says whether this is logged in
    own: Vec<Account>,
    /// Index of the highlighted item in `items()`
    sel: usize,
    uses: Vec<Use>,
    /// Login answers, keyed by "tool:account"; missing = still asking
    logins: Arc<Mutex<HashMap<String, Login>>>,
    form: Option<Form>,
    asking: Option<Asking>,
    /// Fetched quotas; still fetching = None
    quotas: Arc<Mutex<Option<Vec<Quota>>>>,
    /// When the quotas were fetched (for resets and "N hours old") and the local UTC offset
    at: i64,
    off: i64,
    /// Claude mod is on: quotas don't depend on the status bar
    mods: bool,
    /// After p, editing the launch args of the highlighted account (on a section header: those every account of that tool gets):
    /// the input box text
    editing: Option<String>,
    /// Why the args weren't saved
    args_err: String,
}

impl Page {
    /// `adding`: open straight into adding an account (`+` in the `n` menu)
    pub fn real(adding: bool) -> Page {
        let env = Env {
            config: crate::config::path(),
            home: std::env::var("HOME").unwrap_or_default(),
            ask: accounts::login,
            background: true,
            links: aca_core::paths::state_dir(),
            remote: aca_core::agent::remote(&aca_core::config::load()),
            installed: accounts::on_path,
            quotas: || [Agent::Claude, Agent::Codex].into_iter().flat_map(crate::rehome::quotas).collect(),
        };
        Page::load(env, crate::setup::session_accounts(), adding)
    }

    /// `uses`: what each registered session uses (tool, account name, title)
    pub fn load(env: Env, uses: Vec<Use>, adding: bool) -> Page {
        let mut p = Page {
            env,
            rows: vec![],
            fams: vec![],
            own: vec![],
            sel: 0,
            uses,
            logins: Arc::default(),
            form: adding.then(Form::new),
            asking: None,
            quotas: Arc::default(),
            at: 0,
            off: crate::usage::local_offset(),
            mods: false,
            editing: None,
            args_err: String::new(),
        };
        p.read();
        // On open, highlight the first account (with no accounts, the first section header)
        p.sel = p.items().iter().position(|i| matches!(i, Item::Acct(_))).unwrap_or(0);
        p.fetch_quotas();
        p
    }

    /// What can be highlighted in the left column, top to bottom: each tool's section header, then its accounts
    fn items(&self) -> Vec<Item> {
        let mut out = vec![];
        for f in &self.fams {
            out.push(Item::Head(f.agent));
            out.extend(self.rows.iter().enumerate().filter(|(_, r)| r.a.agent == f.agent).map(|(i, _)| Item::Acct(i)));
        }
        out
    }

    fn item(&self) -> Option<Item> {
        self.items().get(self.sel).copied()
    }

    /// The highlighted account; a section header highlighted = None
    fn cur(&self) -> Option<&Row> {
        match self.item()? {
            Item::Acct(i) => self.rows.get(i),
            Item::Head(_) => None,
        }
    }

    /// Launch args every account of this tool gets
    fn common(&self, a: Agent) -> &[String] {
        self.fams.iter().find(|f| f.agent == a).map_or(&[], |f| &f.args)
    }

    /// Relist from what config.toml says now; the highlight stays on the same item
    fn read(&mut self) {
        let keep = self.item().map(|i| match i {
            Item::Head(a) => (a, None),
            Item::Acct(i) => (self.rows[i].a.agent, Some(self.rows[i].a.name.clone())),
        });
        let text = std::fs::read_to_string(&self.env.config).unwrap_or_default();
        let cfg = aca_core::config::parse(&text);
        let lay =
            crate::tools::Layout { menu: crate::config::list_in(&text, "ui", "menu"), hide: crate::config::list_in(&text, "ui", "hide") };
        let menu = crate::tools::menu(&cfg, &lay);
        let (home, uses) = (&self.env.home, &self.uses);
        self.rows = accounts::listed(&text, home)
            .into_iter()
            .map(|(a, abs)| {
                let fam = accounts::fam(a.agent).unwrap_or_default();
                let there = abs.is_empty() || Path::new(&abs).is_dir();
                let foreign = if abs.is_empty() { None } else { accounts::foreign(a.agent, &abs, home) };
                let wired = a.agent == Agent::Claude && !abs.is_empty() && there && foreign.is_none();
                Row {
                    used: uses.iter().filter(|(g, n, _)| *g == a.agent && *n == a.name).map(|(_, _, title)| title.clone()).collect(),
                    default_of: accounts::default_of(&cfg, a.agent, &a.name),
                    in_menu: menu.iter().any(|t| t.tool == fam && t.account == a.name),
                    bar: wired.then(|| crate::setup::bar_of(Path::new(&abs))),
                    args: cfg.account(fam, &a.name).map(|x| x.args.clone()).unwrap_or_default(),
                    there,
                    foreign,
                    a,
                }
            })
            .collect();
        self.mods = cfg.tool("claude").is_some_and(|t| t.mods);
        self.own = accounts::defaults(&text, home, self.env.installed);
        let (rows, own) = (&self.rows, &self.own);
        self.fams = [Agent::Claude, Agent::Codex]
            .into_iter()
            .filter_map(|a| {
                let args = cfg.tool(accounts::fam(a)?).map(|t| t.args).unwrap_or_default();
                let shown = rows.iter().any(|r| r.a.agent == a) || own.iter().any(|o| o.agent == a) || !args.is_empty();
                shown.then_some(Fam { agent: a, args })
            })
            .collect();
        let items = self.items();
        let at = keep.and_then(|(g, n)| {
            items.iter().position(|i| match (i, &n) {
                (Item::Head(x), None) => *x == g,
                (Item::Acct(i), Some(n)) => self.rows[*i].a.agent == g && self.rows[*i].a.name == *n,
                _ => false,
            })
        });
        self.sel = at.unwrap_or(self.sel).min(items.len().saturating_sub(1));
        self.ask(false);
    }

    /// Width of each column (w inside in all): the left fits the longest name, the right gets the rest
    fn split(&self, w: usize) -> (usize, usize) {
        let names = self.rows.iter().map(|r| r.a.name.width()).max().unwrap_or(0).min(14);
        let lw = (2 + names + 2 + 2 + 16).clamp(26, 38);
        (lw, w.saturating_sub(lw + DIVIDER.width()))
    }

    fn at(&mut self, a: Agent, name: &str) {
        let hit = |i: &Item| matches!(i, Item::Acct(i) if self.rows[*i].a.agent == a && self.rows[*i].a.name == name);
        if let Some(i) = self.items().iter().position(hit) {
            self.sel = i;
        }
    }

    /// Ask whether accounts not asked about yet are logged in; `again` = ask all again
    fn ask(&self, again: bool) {
        let mut known = self.logins.lock().unwrap_or_else(|e| e.into_inner());
        if again {
            known.clear();
        }
        let mut todo: Vec<(String, Agent, String)> = self
            .rows
            .iter()
            .filter(|r| r.askable() && !known.contains_key(&r.spec()))
            .map(|r| (r.spec(), r.a.agent, r.a.name.clone()))
            .collect();
        // No accounts registered for this tool: ask about its own one (the section header page says it)
        let bare = self.own.iter().filter(|o| !self.rows.iter().any(|r| r.a.agent == o.agent));
        todo.extend(bare.map(|a| (own_spec(a.agent), a.agent, String::new())).filter(|(s, ..)| !known.contains_key(s)));
        drop(known);
        for (spec, a, name) in todo {
            let (logins, ask) = (self.logins.clone(), self.env.ask);
            let go = move || {
                let got = ask(a, &name);
                logins.lock().unwrap_or_else(|e| e.into_inner()).insert(spec, got);
            };
            if self.env.background {
                std::thread::spawn(go);
            } else {
                go();
            }
        }
    }

    /// Refetch every account's quota
    fn fetch_quotas(&mut self) {
        self.at = crate::limits::now();
        let (slot, get) = (self.quotas.clone(), self.env.quotas);
        let go = move || {
            let got = get();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(got);
        };
        if self.env.background {
            std::thread::spawn(go);
        } else {
            go();
        }
    }

    fn quota_of(&self, r: &Row) -> Option<Quota> {
        let got = self.quotas.lock().unwrap_or_else(|e| e.into_inner());
        got.as_deref().and_then(|qs| crate::rehome::quota_of(qs, r.a.agent, &r.a.name))
    }

    fn login_of(&self, r: &Row) -> Option<Login> {
        self.logins.lock().unwrap_or_else(|e| e.into_inner()).get(&r.spec()).cloned()
    }

    /// This account's state now: mark, a phrase, color. The left list shows only the phrase's first part
    fn state(&self, r: &Row) -> (&'static str, String, Style) {
        if let Some((other, _)) = &r.foreign {
            return ("⚠", tr!("是 {} 的目录", "a {} directory", accounts::who(*other)), theme::warn());
        }
        if !r.there {
            return ("⚠", t!("目录不存在", "directory missing").into(), theme::warn());
        }
        login_said(self.login_of(r))
    }

    /// Left column: two sections (Claude Code, Codex), each header on its own line, one account per line "name  mark first-part".
    /// Plus which line the highlight is on
    fn list(&self, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let name_w = self.rows.iter().map(|r| r.a.name.width()).max().unwrap_or(0).min(14);
        let (mut out, mut at) = (vec![], None);
        let items = self.items();
        for (n, item) in items.iter().enumerate() {
            let on = n == self.sel;
            let i = match *item {
                Item::Head(a) => {
                    if n > 0 {
                        out.push(Line::raw(""));
                    }
                    if on {
                        at = Some(out.len());
                    }
                    out.push(kit::picked(Line::from(who_span(a)), w, on && self.form.is_none()));
                    if !matches!(items.get(n + 1), Some(Item::Acct(_))) {
                        out.push(Line::styled(t!("  暂无账号", "  no accounts yet"), kit::dim()));
                    }
                    continue;
                }
                Item::Acct(i) => i,
            };
            let r = &self.rows[i];
            let (mark, said, st) = self.state(r);
            let short = said.split(" · ").next().unwrap_or_default();
            let room = w.saturating_sub(2 + name_w + 2 + 2);
            let line = Line::from(vec![
                Span::styled(if on { "› " } else { "  " }, theme::accent()),
                Span::styled(kit::clip(&r.a.name, name_w), agent_style(r.a.agent)),
                Span::raw("  "),
                Span::styled(format!("{mark} "), st),
                Span::styled(kit::clip(short, room).trim_end().to_string(), if mark == "●" { Style::new() } else { st }),
            ]);
            if on {
                at = Some(out.len());
            }
            out.push(kit::picked(line, w, on && self.form.is_none()));
        }
        if items.is_empty() {
            out.push(Line::styled(t!("暂无账号", "no accounts yet"), kit::dim()));
        }
        (out, at)
    }

    /// Right column: the form while adding an account, what's being removed after d, the tool's page when a section header is
    /// highlighted, otherwise the highlighted account's whole page
    fn right(&self, w: usize) -> Vec<Line<'static>> {
        if let Some(f) = &self.form {
            return f.lines(w, t!("添加", "add"));
        }
        match self.item() {
            // No tool installed or registered (the key bar has +)
            None => {
                let none = t!(
                    "默认使用 ~/.claude、~/.codex · + 添加账号",
                    "Without accounts, ~/.claude and ~/.codex are used; for another subscription or key: + adds an account"
                );
                fold(none, w).into_iter().map(|l| Line::styled(l, kit::dim())).collect()
            }
            Some(Item::Head(a)) => self.head(a, w),
            Some(Item::Acct(i)) => {
                let r = &self.rows[i];
                match self.asking {
                    Some(Asking::Remove) => self.confirm(r, w),
                    Some(Asking::Relogin) => self.relogin(r, w),
                    None => self.detail(r, w),
                }
            }
        }
    }

    /// The section header page: launch args every account of this tool gets. For a tool with no registered accounts, it first says
    /// whether the directory used without one is logged in, and how to log in (a new user who hasn't logged in can tell here)
    fn head(&self, a: Agent, w: usize) -> Vec<Line<'static>> {
        let (labels, lw) = (labels(), label_w());
        let mut out = vec![Line::from(who_span(a)), Line::raw("")];
        let bare = !self.rows.iter().any(|r| r.a.agent == a);
        if let Some(own) = self.own.iter().find(|o| o.agent == a).filter(|_| bare) {
            let none = tr!(
                "默认使用 {} · + 添加账号",
                "Without accounts, {} is used; for another subscription or key: + adds an account",
                own.dir
            );
            out.extend(fold(&none, w).into_iter().map(|l| Line::styled(l, kit::dim())));
            out.push(Line::raw(""));
            let login = self.logins.lock().unwrap_or_else(|e| e.into_inner()).get(&own_spec(a)).cloned();
            let out_now = login == Some(Login::Out);
            let (mark, said, st) = login_said(login);
            field(&mut out, w, lw, labels[1], Some((mark, st)), &said, if mark == "●" { Style::new() } else { st });
            if out_now {
                let how = if a == Agent::Codex { "login" } else { "auth login" };
                let go = tr!("登录:aca run {} -- {how}", "to log in: aca run {} -- {how}", own_spec(a));
                field(&mut out, w, lw, "", None, &go, kit::dim());
            }
        }
        self.args_field(&mut out, w, &join(self.common(a)));
        if self.editing.is_some() {
            self.args_keys(&mut out, w);
        }
        out
    }

    /// The launch args field (`now` is the current value, omitted when empty); an input box while editing
    fn args_field(&self, out: &mut Vec<Line<'static>>, w: usize, now: &str) {
        let (label, lw) = (labels()[4], label_w());
        match &self.editing {
            Some(text) => field(out, w, lw, label, None, &format!("{text}▏"), theme::accent()),
            None if !now.is_empty() => field(out, w, lw, label, None, now, Style::new()),
            None => {}
        }
    }

    /// Below the input box while editing launch args: why it didn't save, and the keys
    fn args_keys(&self, out: &mut Vec<Line<'static>>, w: usize) {
        if !self.args_err.is_empty() {
            field(out, w, label_w(), "", None, &self.args_err, theme::warn());
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("⏎", t!("保存", "save")), ("^U", t!("清空", "clear"))], vec![], w));
    }

    /// The 「默认」 ("default") tag after the name: tools that use it when no account is given. If it's only its own tool, it
    /// doesn't say which
    fn default_tag(&self, r: &Row) -> Option<String> {
        let fam = accounts::fam(r.a.agent).unwrap_or_default();
        match r.default_of.as_slice() {
            [] => None,
            [only] if only == fam => Some(t!("默认", "default").into()),
            tools => Some(tr!("默认:{}", "default: {}", tools.join(t!("、", ", ")))),
        }
    }

    /// The highlighted account's whole page: which tool, whether default, directory, who's logged in, quota, status bar, launch args;
    /// sessions using it and being hidden from the n menu only when so. While editing launch args, the input box and keys go under
    /// the launch args field, and nothing after it is shown
    fn detail(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let (labels, lw) = (labels(), label_w());
        let put = |out: &mut Vec<Line<'static>>, label: &str, lead: Option<(&str, Style)>, text: &str, st: Style| {
            field(out, w, lw, label, lead, text, st)
        };
        let plain = Style::new();
        let mut head = vec![Span::styled(r.a.name.clone(), agent_style(r.a.agent).add_modifier(Modifier::BOLD))];
        if let Some(tag) = self.default_tag(r) {
            head.push(Span::styled(format!("  {tag}"), kit::dim()));
        }
        let mut out = vec![kit::spread(head, vec![who_span(r.a.agent)], w), Line::raw("")];
        if let Some((other, why)) = &r.foreign {
            let theirs = accounts::who(*other);
            let said = [
                tr!("⚠ 这是 {theirs} 的目录({why})", "⚠ This is a {theirs} directory ({why})"),
                tr!("d 删除 · + 添加为 {theirs} 账号", "d removes it; to use it, + adds it for {theirs}"),
            ];
            out.extend(said.iter().flat_map(|s| fold(s, w)).map(|l| Line::styled(l, theme::warn())));
            out.push(Line::raw(""));
        }
        let dir = if r.a.dir.is_empty() { t!("默认目录", "default directory") } else { &r.a.dir };
        put(&mut out, labels[0], None, dir, plain);
        if r.foreign.is_none() {
            let (mark, said, st) = self.state(r);
            let said = match (r.there, mark) {
                (false, _) => t!("目录不存在 · d 删除", "directory missing · d removes it").to_string(),
                (_, "○") => t!("未登录 · ⏎ 登录", "not logged in · ⏎ logs in").to_string(),
                (_, "?") => tr!("无法确认登录状态:{said}", "can't tell whether it's logged in: {said}"),
                _ => said,
            };
            put(&mut out, labels[1], Some((mark, st)), &said, if mark == "●" { plain } else { st });
        }
        // Quota only once recorded; a Claude account with no status bar wired up (and no mod) can't record it, which the status bar
        // field says
        if let Some(q) = self.quota_of(r) {
            let mut spans = vec![Span::styled(kit::clip(labels[2], lw), kit::dim())];
            spans.extend(crate::rehome::quota_spans(Some(&q), self.at, self.off));
            if !q.note.is_empty() {
                spans.push(Span::styled(format!("  {}", q.note), kit::dim()));
            }
            out.push(Line::from(spans));
        }
        if let Some((bar, feeds)) = r.bar {
            let said = if feeds || self.mods { bar.to_string() } else { tr!("{bar} · 无额度信息", "{bar} · no quotas") };
            put(&mut out, labels[3], None, &said, plain);
        }
        // Launch args: the account's own, and those every account of this tool gets (they go first; edited on the section header)
        let own = join(&r.args);
        self.args_field(&mut out, w, &own);
        let common = self.common(r.a.agent);
        if !common.is_empty() {
            let every = tr!("所有 {} 账号都附带:{}", "every {} account gets: {}", accounts::who(r.a.agent), join(common));
            let label = if self.editing.is_none() && own.is_empty() { labels[4] } else { "" };
            put(&mut out, label, None, &every, kit::dim());
        }
        if self.editing.is_some() {
            self.args_keys(&mut out, w);
            return out;
        }
        // Fields shown only when non-empty, a blank line above them
        let (mut more, n) = (vec![], r.used.len());
        if n > 0 {
            put(&mut more, labels[5], None, &tr!("{n} 个正在使用", "{n} using it"), plain);
            let room = w.saturating_sub(lw + 2);
            for title in r.used.iter().take(if n > TITLES { TITLES - 1 } else { TITLES }) {
                more.push(Line::styled(format!("{}· {}", " ".repeat(lw), kit::clip(title, room).trim_end()), kit::dim()));
            }
            if n > TITLES {
                let rest = tr!("…还有 {} 个", "…and {} more", n + 1 - TITLES);
                more.push(Line::styled(format!("{}· {rest}", " ".repeat(lw)), kit::dim()));
            }
        }
        if !r.in_menu {
            put(&mut more, labels[6], None, t!("不显示", "not shown"), kit::dim());
        }
        if !more.is_empty() {
            out.push(Line::raw(""));
            out.extend(more);
        }
        out
    }

    /// After d: what it touches and who uses it
    fn confirm(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let me = accounts::who(r.a.agent);
        let kept = if r.a.dir.is_empty() || !r.there {
            t!("仅从 aca 中移除。", "It's only taken out of aca.")
        } else {
            t!("仅从 aca 中移除,目录和登录信息保留。", "It's only taken out of aca; the directory and its login stay.")
        };
        let mut said = vec![kept.to_string()];
        if !r.used.is_empty() {
            said.push(tr!(
                "{} 个会话正在使用它,删除后这些会话将无法重启。",
                "{} sessions use it; once removed they can't restart.",
                r.used.len()
            ));
        }
        for tool in &r.default_of {
            said.push(tr!(
                "它是 {tool} 的默认账号,删除后未指定账号的 {tool} 会话将无法启动。",
                "It's {tool}'s default account; once removed, {tool} won't start without an account given."
            ));
        }
        let title = tr!("删除 {me} 账号 {}?", "Remove the {me} account {}?", r.a.name);
        let mut out = vec![Line::styled(title, theme::warn().add_modifier(Modifier::BOLD)), Line::raw("")];
        out.extend(said.iter().flat_map(|s| fold(s, w)).map(|l| Line::styled(l, Style::new())));
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("y", t!("删除", "remove")), (t!("其他键", "any other key"), t!("取消", "keep"))], vec![], w));
        out
    }

    /// The tool said it isn't logged in (a missing directory counts too: there's no login in it to lose). Logged in, not asked yet
    /// and can't tell don't count
    fn logged_out(&self, r: &Row) -> bool {
        !r.there || self.login_of(r) == Some(Login::Out)
    }

    /// ⏎ on a logged-in account: what logging in again loses; only y goes ahead
    fn relogin(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let me = accounts::who(r.a.agent);
        // Not asked yet: say nothing about it; ask as if it's logged in
        let now = match self.login_of(r) {
            Some(Login::In(who)) if !who.is_empty() => tr!("当前已登录:{who}。", "It is logged in now: {who}."),
            Some(Login::In(_)) => t!("当前已登录。", "It is logged in now.").to_string(),
            Some(Login::Unknown(said)) => tr!("无法确认登录状态:{said}", "can't tell whether it's logged in: {said}"),
            _ => String::new(),
        };
        let lost = if r.a.agent == Agent::Codex {
            t!(
                "Codex 开始登录时会清除当前登录,中途退出将变为未登录。",
                "Codex's login deletes the current one as it starts: if you don't finish, this account is left logged out."
            )
        } else {
            t!("重新登录将替换当前登录。", "Logging in again replaces the current login.")
        };
        let title = tr!("重新登录 {me} 账号 {}?", "Log the {me} account {} in again?", r.a.name);
        let mut out = vec![Line::styled(title, theme::warn().add_modifier(Modifier::BOLD)), Line::raw("")];
        if !now.is_empty() {
            out.extend(fold(&now, w).into_iter().map(Line::raw));
        }
        out.extend(fold(lost, w).into_iter().map(|l| Line::styled(l, theme::warn())));
        out.push(Line::raw(""));
        let keys = [("y", t!("重新登录", "log in again")), (t!("其他键", "any other key"), t!("取消", "leave it"))];
        out.push(kit::keys_line(&keys, vec![], w));
        out
    }

    /// ⏎ on the highlighted row: logged out goes straight to login; logged in asks first (see [`Asking::Relogin`])
    fn login(&mut self, fx: &mut Fx) {
        let Some(r) = self.cur() else { return };
        if let Some((other, _)) = &r.foreign {
            return fx.say(tr!("是 {} 的目录,不能在此登录", "a {} directory, not logging in here", accounts::who(*other)));
        }
        if self.logged_out(r) { self.open_login(fx) } else { self.asking = Some(Asking::Relogin) }
    }

    /// Open a terminal in the bottom half running the tool's own login for the highlighted account
    fn open_login(&self, fx: &mut Fx) {
        let Some(r) = self.cur() else { return };
        fx.term = Some(self.term(&r.a.name, r.a.agent, accounts::relogin_cmd(r.a.agent, &r.a.name)));
    }

    /// Login doesn't let the tool open the default browser itself: the link goes to the UI (a file), which pops up a choice of what
    /// to open it with (see browser.rs). Codex doesn't hand it over, nor does Claude when remote (it gives the paste-a-code one), so
    /// the UI reads it off the screen; a file left from last time is deleted first so the old link doesn't pop up right away
    fn term(&self, name: &str, a: Agent, run: String) -> TermReq {
        let tool = if a == Agent::Codex { "codex" } else { "claude" };
        let file = self.env.links.join(format!("login-{tool}-{name}.url"));
        let _ = std::fs::create_dir_all(&self.env.links);
        let _ = std::fs::remove_file(&file);
        TermReq {
            key: format!("account:{name}"),
            family: String::new(),
            title: tr!("登录 {} · {name}", "log in {} · {name}", accounts::who(a)),
            w: crate::workdir::Where { dir: self.env.home.clone(), ..Default::default() },
            env: vec![(aca_core::agent::LOGIN_LINK.to_string(), file.to_string_lossy().into_owned())],
            run: Some(run),
            link: Some(crate::term::Catch { file, screen: a == Agent::Codex || self.env.remote }),
        }
    }

    /// The form's directory is filled in: record it in config.toml; if the directory was just created, open a terminal to log in
    fn add(&mut self, fx: &mut Fx) {
        let Some(f) = self.form.as_mut() else { return };
        let (a, name) = (f.agent, f.name.clone());
        match accounts::add_in(&self.env.config, &self.env.home, a, &name, f.text.trim()) {
            Err(e) => f.hint = e,
            Ok(added) => {
                self.form = None;
                self.read();
                self.at(a, &name);
                // Claude's quota depends on the status bar: wiring it up is chosen under s, not done for the user (the key bar shows s)
                fx.say(tr!("已添加 {} 账号 {name}", "added the {} account {name}", accounts::who(a)));
                if added.created {
                    fx.term = Some(self.term(&name, a, accounts::login_cmd(a, &name)));
                }
            }
        }
    }

    /// Save the highlighted account's launch args (section header: those every account of the tool gets); if that fails, why goes
    /// under the input box and editing continues
    fn save_args(&mut self, fx: &mut Fx) {
        let (Some(text), Some(item)) = (&self.editing, self.item()) else { return };
        let args = match split(text) {
            Ok(args) => args,
            Err(e) => {
                self.args_err = e;
                return;
            }
        };
        let saved = match item {
            Item::Head(a) => accounts::set_common_args_in(&self.env.config, a, &args),
            Item::Acct(i) => accounts::set_args_in(&self.env.config, self.rows[i].a.agent, &self.rows[i].a.name, &args),
        };
        match saved {
            Ok(changed) => {
                (self.editing, self.args_err) = (None, String::new());
                // Unchanged: don't write, don't say
                if changed {
                    // The highlight stays on the same item
                    self.read();
                    // Open sessions are left alone; only new or restarted ones use this
                    fx.say(t!("已保存,新会话生效", "saved; new sessions use it"));
                }
            }
            Err(e) => self.args_err = e,
        }
    }

    fn remove(&mut self, fx: &mut Fx) {
        let Some(r) = self.cur() else { return };
        let (a, name) = (r.a.agent, r.a.name.clone());
        match accounts::remove_in(&self.env.config, &self.env.home, a, &name) {
            // The directory stays: the confirm before removing said so
            Ok(_) => {
                fx.say(tr!("已删除 {} 账号 {name}", "removed the {} account {name}", accounts::who(a)));
                self.read();
            }
            Err(e) => fx.say(e),
        }
    }

    /// Move one step: the list reorders on the spot, no separate message (kept in `[ui] menu`, the same order as the `n` menu)
    fn shift(&mut self, up: bool, fx: &mut Fx) {
        let Some(r) = self.cur() else { return };
        let (a, name) = (r.a.agent, r.a.name.clone());
        match accounts::shift_in(&self.env.config, &self.env.home, a, &name, up) {
            Ok(true) => {
                self.read();
                self.at(a, &name);
            }
            Ok(false) => {}
            Err(e) => fx.say(e),
        }
    }
}

impl Overlay for Page {
    fn width(&self) -> u16 {
        120
    }

    /// Fixed size like the plugins page: moving the highlight, entering the form, asking whether to remove don't change the frame
    fn fill(&self) -> bool {
        true
    }

    /// Height follows the content (unlike the plugins page filling the screen: there are only a few accounts, so the bottom half
    /// would be empty): title, the longer of the two columns, the rule and keys. The right column is sized for the longest account
    /// or section header page (including its remove confirm), so moving the highlight doesn't change it; only adding / removing an
    /// account does
    fn fill_rows(&self) -> Option<u16> {
        let (lw, rw) = self.split(self.width() as usize - 6);
        let ask = |r: &Row| self.confirm(r, rw).len().max(self.relogin(r, rw).len());
        let right = self.rows.iter().map(|r| self.detail(r, rw).len().max(ask(r))).max().unwrap_or(0);
        let right = self.fams.iter().map(|f| self.head(f.agent, rw).len()).fold(right, usize::max);
        Some((2 + self.list(lw).0.len().max(right).max(FORM_ROWS) + 2).max(22) as u16)
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        self.lines_in(w, 32)
    }

    /// Title, two columns (stacked on narrow screens), rule, keys; exactly h lines
    fn lines_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let busy = self.form.is_some() || self.asking.is_some() || self.editing.is_some();
        let esc = kit::esc_hint(if busy { t!("返回", "back") } else { t!("关闭", "close") });
        let mut out = vec![kit::modal_title(t!("账号", "Accounts"), theme::accent(), esc, w), Line::raw("")];
        // When they don't fit, later keys are dropped first: the most important go first. While filling the form or asking whether to
        // remove, the keys go in the right column
        let out_now = self.cur().is_none_or(|r| r.foreign.is_some() || self.logged_out(r));
        let all = [
            ("⏎", if out_now { t!("登录", "log in") } else { t!("重新登录", "log in again") }),
            ("+", t!("添加账号", "add")),
            ("d", t!("删除", "remove")),
            ("⇧↑↓", t!("移动", "move")),
            ("p", t!("启动参数", "launch args")),
            ("s", t!("状态栏", "status bar")),
            ("r", t!("刷新", "recheck")),
        ];
        // A section header lacks the keys that act on one account; the status bar table is empty when there are no accounts
        let on_head = |k: &str| ["+", "p", "r"].contains(&k) || (k == "s" && !self.rows.is_empty());
        let keys: Vec<(&str, &str)> = match self.item() {
            _ if busy => vec![],
            None => all[1..2].to_vec(),
            Some(Item::Head(_)) => all.into_iter().filter(|(k, _)| on_head(k)).collect(),
            Some(Item::Acct(_)) => all.to_vec(),
        };
        let foot = [Line::styled("─".repeat(w), theme::faint()), kit::keys_line(&keys, vec![], w)];
        let bh = h.saturating_sub(out.len() + foot.len());
        if w >= SPLIT_W {
            let (lw, rw) = self.split(w);
            let (left, at) = self.list(lw);
            let right = self.right(rw);
            let lo = scroll(at, left.len(), bh);
            for i in 0..bh {
                let mut spans = pad(left.get(lo + i).cloned().unwrap_or_default(), lw);
                spans.push(Span::styled(DIVIDER, theme::faint()));
                spans.extend(flat(right.get(i).cloned().unwrap_or_default()));
                out.push(Line::from(spans));
            }
        } else {
            // Phone: list on top (at most half), the highlighted one's page below
            let (left, at) = self.list(w);
            let lh = left.len().min((bh / 2).max(4)).min(bh);
            let lo = scroll(at, left.len(), lh);
            let mut body: Vec<Line<'static>> = left.into_iter().skip(lo).take(lh).collect();
            body.push(Line::styled("┄".repeat(w), theme::faint()));
            let room = bh.saturating_sub(body.len());
            body.extend(self.right(w).into_iter().take(room));
            body.resize(bh, Line::raw(""));
            out.extend(body);
        }
        out.extend(foot);
        out.truncate(h);
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        if let Some(f) = self.form.as_mut() {
            let rows = &self.rows;
            match f.key(k, |a, name| rows.iter().any(|r| r.a.agent == a && r.a.name == name)) {
                Went::Left => self.form = None,
                Went::Filled => self.add(fx),
                Went::Stay => {}
            }
            return Flow::Stay;
        }
        if let Some(text) = self.editing.as_mut() {
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            match k.code {
                KeyCode::Esc => (self.editing, self.args_err) = (None, String::new()),
                KeyCode::Enter => self.save_args(fx),
                KeyCode::Backspace => drop(text.pop()),
                KeyCode::Char('u') if ctrl => text.clear(),
                KeyCode::Char('w') if ctrl => {
                    let kept = text.trim_end().rfind(' ').map_or(0, |i| i + 1);
                    text.truncate(kept);
                }
                KeyCode::Char(c) if !ctrl && text.chars().count() < ARGS_MAX => text.push(c),
                _ => {}
            }
            return Flow::Stay;
        }
        if let Some(what) = self.asking.take() {
            match (what, k.code) {
                (Asking::Remove, KeyCode::Char('y')) => self.remove(fx),
                (Asking::Relogin, KeyCode::Char('y')) => self.open_login(fx),
                _ => {}
            }
            return Flow::Stay;
        }
        let n = self.items().len();
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Close,
            KeyCode::Up if shift => self.shift(true, fx),
            KeyCode::Down if shift => self.shift(false, fx),
            KeyCode::Char('K') => self.shift(true, fx),
            KeyCode::Char('J') => self.shift(false, fx),
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Enter => self.login(fx),
            KeyCode::Char('+') => self.form = Some(Form::new()),
            KeyCode::Char('d') | KeyCode::Delete if self.cur().is_some() => self.asking = Some(Asking::Remove),
            // On an account, edit its own; on a section header, those every account of the tool gets
            KeyCode::Char('p') => {
                self.editing = match self.item() {
                    Some(Item::Head(a)) => Some(join(self.common(a))),
                    Some(Item::Acct(i)) => Some(join(&self.rows[i].args)),
                    None => None,
                }
            }
            // Whether each account's status bar is wired up (`aca setup` opens the same table; accounts are added on this page, not in
            // the table)
            KeyCode::Char('s') => fx.open = Some(Box::new(crate::setup::Page::real(false))),
            KeyCode::Char('r') => {
                self.read();
                self.ask(true);
                self.fetch_quotas();
            }
            _ => {}
        }
        Flow::Stay
    }

    /// config.toml edited in the editor, or the status bar table done: relist
    fn reload(&mut self) {
        self.read();
    }

    fn typing(&self) -> bool {
        self.form.as_ref().is_some_and(Form::typing) || self.editing.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(name: &str, config: &str) -> (PathBuf, Env) {
        let root = std::env::temp_dir().join(format!("aca-acctpage-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("aca")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let env = Env {
            config: root.join("aca/config.toml"),
            home: root.to_string_lossy().into_owned(),
            ask: |a, name| match (a, name) {
                (Agent::Claude, "sub") => Login::In("max · me@example.com".into()),
                (Agent::Codex, _) => Login::In("ChatGPT".into()),
                _ => Login::Out,
            },
            background: false,
            links: root.join("state"),
            remote: false,
            // Only Claude is installed in this home
            installed: |prog| prog == "claude",
            quotas: Vec::new,
        };
        std::fs::write(&env.config, config).unwrap();
        (root, env)
    }

    fn dir(root: &Path, name: &str, files: &[&str]) {
        std::fs::create_dir_all(root.join(name)).unwrap();
        for f in files {
            std::fs::write(root.join(name).join(f), "{}").unwrap();
        }
    }

    fn key(p: &mut Page, k: KeyCode) -> (Flow, Fx) {
        let mut fx = Fx::default();
        (p.on_key(KeyEvent::new(k, KeyModifiers::NONE), &mut fx), fx)
    }

    fn press(p: &mut Page, keys: &[KeyCode]) -> Fx {
        keys.iter().fold(Fx::default(), |_, k| key(p, *k).1)
    }

    fn type_in(p: &mut Page, text: &str) {
        text.chars().for_each(|c| drop(key(p, KeyCode::Char(c))));
    }

    fn retype(p: &mut Page, text: &str) {
        p.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &mut Fx::default());
        type_in(p, text);
    }

    fn shown(p: &Page, w: usize) -> Vec<String> {
        p.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect()
    }

    fn shows(text: &[String], wants: &[&str]) {
        for w in wants {
            assert!(text.iter().any(|l| l.contains(w)), "没有「{w}」:{text:#?}");
        }
    }

    fn names(p: &Page) -> Vec<&str> {
        p.rows.iter().map(|r| r.a.name.as_str()).collect()
    }

    /// The highlighted item: an account's name, or a section header's tool
    fn on(p: &Page) -> &str {
        match p.item() {
            Some(Item::Acct(i)) => &p.rows[i].a.name,
            Some(Item::Head(a)) => accounts::who(a),
            None => "",
        }
    }

    /// This account's row in the left column (the highlighted one starts with ›)
    fn row<'a>(text: &'a [String], name: &str) -> &'a str {
        let found = text.iter().find(|l| l.starts_with(&format!("  {name} ")) || l.starts_with(&format!("› {name} ")));
        found.unwrap_or_else(|| panic!("左边没有 {name}:{text:#?}"))
    }

    const CONFIG: &str = "# 我的\n[tools.claude]\naccount = \"sub\"\n\n[accounts.claude]\nsub = \"~/.claude-sub\"\nfirm = \"~/.claude-firm\"\ncodex = \"~/.codex\"\ngone = \"~/.claude-gone\"\n\n[accounts.codex]\ncx = \"~/.codex-cx\"\n\n[ui]\nhide = [\"firm\"]\n";

    fn page(name: &str) -> (PathBuf, Page) {
        page_with(name, CONFIG)
    }

    fn page_with(name: &str, config: &str) -> (PathBuf, Page) {
        let (root, env) = home(name, config);
        dir(&root, ".claude-sub", &[".claude.json", "settings.json"]);
        dir(&root, ".claude-firm", &[".claude.json"]);
        // A Codex directory registered as Claude's and launched once: it gained a .claude.json
        dir(&root, ".codex", &["auth.json", "config.toml", "version.json", ".claude.json"]);
        dir(&root, ".codex-cx", &["auth.json"]);
        let used = |a, account: &str, title: &str| (a, account.to_string(), title.to_string());
        let uses = vec![used(Agent::Claude, "sub", "写账号页"), used(Agent::Claude, "sub", "查额度"), used(Agent::Codex, "cx", "跑测试")];
        (root, Page::load(env, uses, false))
    }

    #[test]
    fn the_right_side_shows_quotas_and_launch_args_and_p_changes_them() {
        use crate::usage::Win;
        let config = CONFIG.replace("account = \"sub\"\n", "account = \"sub\"\nargs = [\"--x\"]\n");
        let (root, mut p) = page_with("args", &config);
        p.env.quotas = || {
            let win = |label: &str, used| Win { label: label.into(), used, resets: None };
            let quota = |agent, who: &str, wins, note: &str| Quota {
                agent,
                who: who.into(),
                plan: String::new(),
                wins,
                updated: 0,
                note: note.into(),
            };
            vec![
                quota(Agent::Claude, "sub", vec![win("5h", 12.0), win("7d", 61.0)], ""),
                quota(Agent::Codex, "cx", vec![win("5h", 95.0)], "余额 5"),
            ]
        };
        p.fetch_quotas();
        // sub: quota; the only launch args are those every Claude account gets
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "sub");
        shows(&shown(&p, 110), &["额度      5h 12% · 7d 61%", "启动参数  所有 Claude Code 账号都附带:--x"]);
        // p: edit its own, saved to its line in config.toml
        press(&mut p, &[KeyCode::Char('p')]);
        assert!(p.typing());
        type_in(&mut p, "--model 'big one'");
        shows(&shown(&p, 110), &["启动参数  --model 'big one'▏", "⏎ 保存 • ^U 清空"]);
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!((fx.flash.as_deref(), p.editing.is_none()), (Some("已保存,新会话生效"), true));
        let saved = std::fs::read_to_string(&p.env.config).unwrap();
        assert!(saved.contains("sub = { home = \"~/.claude-sub\", args = [\"--model\", \"big one\"] }\n"), "{saved}");
        shows(&shown(&p, 110), &["启动参数  --model 'big one'", "          所有 Claude Code 账号都附带:--x"]);
        // Unmatched quote: say why, keep editing; Esc leaves it unchanged
        press(&mut p, &[KeyCode::Char('p')]);
        type_in(&mut p, " \"oops");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.editing.is_some() && !p.args_err.is_empty());
        press(&mut p, &[KeyCode::Esc]);
        assert!(p.editing.is_none() && std::fs::read_to_string(&p.env.config).unwrap() == saved);
        // Clear: written back as the bare directory line
        press(&mut p, &[KeyCode::Char('p')]);
        p.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &mut Fx::default());
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), config);
        // Codex's quota needs no status bar wiring; the balance follows it
        press(&mut p, &[KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "cx");
        shows(&shown(&p, 110), &["额度      5h 95%  余额 5"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn p_on_a_tools_heading_changes_what_every_account_of_it_gets() {
        let config = CONFIG.replace("account = \"sub\"\n", "account = \"sub\"\nargs = [\"--x\"]\n");
        let (root, mut p) = page_with("common", &config);
        // Section headers can be highlighted too: one up from the first account
        press(&mut p, &[KeyCode::Up]);
        assert_eq!(on(&p), "Claude Code");
        let text = shown(&p, 110);
        assert!(text.iter().any(|l| l.starts_with("Claude Code") && l.contains("│  Claude Code")), "{text:#?}");
        shows(&text, &["启动参数  --x"]);
        // A section header has no keys that act on one account
        assert_eq!(text.last().unwrap(), "+ 添加账号 • p 启动参数 • s 状态栏 • r 刷新");
        assert!(press(&mut p, &[KeyCode::Enter]).term.is_none() && p.asking.is_none());
        press(&mut p, &[KeyCode::Char('d')]);
        assert!(p.asking.is_none());
        // p: edit those every account of this tool gets, saved to the [tools.claude] section, nothing else touched
        press(&mut p, &[KeyCode::Char('p')]);
        assert!(p.typing());
        type_in(&mut p, " --effort high");
        shows(&shown(&p, 110), &["启动参数  --x --effort high▏", "⏎ 保存 • ^U 清空"]);
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!((fx.flash.as_deref(), p.editing.is_none(), on(&p)), (Some("已保存,新会话生效"), true, "Claude Code"));
        let saved = std::fs::read_to_string(&p.env.config).unwrap();
        assert_eq!(saved, config.replace("args = [\"--x\"]", "args = [\"--x\", \"--effort\", \"high\"]"));
        // The account page shows those every account gets
        press(&mut p, &[KeyCode::Up]);
        assert_eq!(on(&p), "cx");
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "sub");
        shows(&shown(&p, 110), &["启动参数  所有 Claude Code 账号都附带:--x --effort high"]);
        // The Codex section: there wasn't one, so it's appended at the end
        press(&mut p, &[KeyCode::Down]);
        assert_eq!(on(&p), "Codex");
        assert!(!shown(&p, 110).iter().any(|l| l.contains("启动参数  ")));
        press(&mut p, &[KeyCode::Char('p')]);
        type_in(&mut p, "--search");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), format!("{saved}\n[tools.codex]\nargs = [\"--search\"]\n"));
        press(&mut p, &[KeyCode::Down]);
        shows(&shown(&p, 110), &["启动参数  所有 Codex 账号都附带:--search"]);
        // Enter with no change: don't write, don't say; unmatched quote: say why, keep editing
        press(&mut p, &[KeyCode::Up, KeyCode::Char('p')]);
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        assert!(press(&mut p, &[KeyCode::Enter]).flash.is_none() && std::fs::read_to_string(&p.env.config).unwrap() == before);
        press(&mut p, &[KeyCode::Char('p')]);
        type_in(&mut p, " 'oops");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.editing.is_some() && p.args_err == "引号不成对");
        shows(&shown(&p, 110), &["引号不成对"]);
        press(&mut p, &[KeyCode::Esc]);
        assert!(p.editing.is_none() && std::fs::read_to_string(&p.env.config).unwrap() == before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_line_splits_like_a_shell_and_joins_back() {
        let v = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(split("  --model opus\t--effort  xhigh ").unwrap(), v(&["--model", "opus", "--effort", "xhigh"]));
        assert_eq!(split(r#"--x='a b' "c \"d\" \\" e\ f '' g"#).unwrap(), v(&["--x=a b", r#"c "d" \"#, "e f", "", "g"]));
        assert_eq!(split("").unwrap(), Vec::<String>::new());
        assert!(split("--append-system-prompt 'oops").is_err());
        assert!(split(r#"--x "oops\""#).is_err());
        for args in [v(&["--model", "opus"]), v(&["it's", "a b", "", r#"q"\"#, "--x=1"]), vec![]] {
            assert_eq!(split(&join(&args)).unwrap(), args, "{}", join(&args));
        }
        assert_eq!(
            join(&v(&["--dangerously-skip-permissions", "--append-system-prompt", "be brief"])),
            "--dangerously-skip-permissions --append-system-prompt 'be brief'"
        );
    }

    #[test]
    fn the_list_is_on_the_left_and_the_highlighted_account_in_full_on_the_right() {
        let (root, mut p) = page("list");
        let text = shown(&p, 110);
        // Left: two sections, each with a header (its own line naming the tool), accounts in their own section, each showing whether
        // it's logged in
        let at = |want: &str| text.iter().position(|l| l.starts_with(want)).unwrap_or_else(|| panic!("没有「{want}」开头的行:{text:#?}"));
        assert!(at("Claude Code") < at("  sub ") && at("  gone ") < at("Codex") && at("Codex") < at("  cx "), "{text:#?}");
        assert!(row(&text, "sub").contains("● max") && !row(&text, "sub").contains("me@example.com"), "{text:#?}");
        assert!(row(&text, "firm").contains("○ 未登录") && row(&text, "cx").contains("● ChatGPT"), "{text:#?}");
        // The other tool's directory, a missing directory: marked, and not asked about login (asking would write into them)
        assert!(row(&text, "codex").contains("⚠ 是 Codex 的目录") && row(&text, "gone").contains("⚠ 目录不存在"), "{text:#?}");
        assert!(!p.logins.lock().unwrap().keys().any(|k| k == "claude:codex" || k == "claude:gone"));
        // Right: the highlighted one (added wrongly) — whose directory it is and what to do; no login field
        assert_eq!(on(&p), "codex");
        shows(&text, &["⚠ 这是 Codex 的目录(其中包含 auth.json、config.toml…)", "d 删除 · + 添加为 Codex 账号", "目录      ~/.codex"]);
        assert!(!text.iter().any(|l| l.contains(" 登录    ")), "{text:#?}");
        press(&mut p, &[KeyCode::Down, KeyCode::Down]);
        shows(&shown(&p, 110), &["登录      ⚠ 目录不存在 · d 删除"]);
        // Logged in: who, default, status bar, which sessions use it; listed in the n menu, so not mentioned
        press(&mut p, &[KeyCode::Down]);
        let text = shown(&p, 110);
        shows(
            &text,
            &[
                "sub  默认",
                "目录      ~/.claude-sub",
                "登录      ● max · me@example.com",
                "状态栏    不接入 · 无额度信息",
                "会话      2 个正在使用",
                "        · 写账号页",
                "        · 查额度",
            ],
        );
        assert!(!text.iter().any(|l| l.contains("n 菜单") || l.contains("aca ")), "{text:#?}");
        // No sessions using it and not default: not mentioned; hidden from the n menu: one line says so
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        let text = shown(&p, 110);
        shows(&text, &["登录      ○ 未登录 · ⏎ 登录", "n 菜单    不显示"]);
        assert!(!text.iter().any(|l| l.contains(" 会话    ") || l.contains("默认")), "{text:#?}");
        // Up wraps past the section header to the last one
        press(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Up]);
        assert_eq!(on(&p), "cx");
        shows(&shown(&p, 110), &["cx", "登录      ● ChatGPT"]);
        // Codex's quota doesn't depend on the status bar: no such field
        assert!(!shown(&p, 110).iter().any(|l| l.contains(" 状态栏  ")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_long_list_of_sessions_is_cut_short() {
        let (root, env) = home("many", "[accounts.claude]\nsub = \"~/.claude-sub\"\n");
        dir(&root, ".claude-sub", &[]);
        let uses = (1..=9).map(|i| (Agent::Claude, "sub".to_string(), format!("会话 {i}"))).collect();
        let text = shown(&Page::load(env, uses, false), 110);
        shows(&text, &["会话      9 个正在使用", "· 会话 4", "· …还有 5 个"]);
        assert!(!text.iter().any(|l| l.contains("会话 5")), "{text:#?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn d_says_what_goes_and_only_y_removes_the_registration() {
        let (root, mut p) = page("rm");
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        assert_eq!(names(&p), ["codex", "firm", "gone", "sub", "cx"]);
        press(&mut p, &[KeyCode::Char('d')]);
        shows(&shown(&p, 110), &["删除 Claude Code 账号 codex?", "仅从 aca 中移除,目录和登录信息保留。", "y 删除 • 其他键 取消"]);
        assert!(!shown(&p, 110).iter().any(|l| l.contains("config.toml")));
        // Any other key: no removal, and the key isn't passed on (j doesn't move the highlight)
        press(&mut p, &[KeyCode::Char('j')]);
        assert_eq!((on(&p), std::fs::read_to_string(&p.env.config).unwrap()), ("codex", before.clone()));
        let fx = press(&mut p, &[KeyCode::Char('d'), KeyCode::Char('y')]);
        assert_eq!(fx.flash.as_deref(), Some("已删除 Claude Code 账号 codex"));
        assert_eq!(names(&p), ["firm", "gone", "sub", "cx"]);
        // Only that line is gone; the directory and its contents remain; the pre-removal copy is kept
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before.replace("codex = \"~/.codex\"\n", ""));
        assert!(root.join(".codex/auth.json").exists() && root.join(".codex/.claude.json").exists());
        assert_eq!(std::fs::read_to_string(root.join("aca/config.toml.bak-aca")).unwrap(), before);
        // In use by sessions, or the default account: spelled out before removing
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Char('d')]);
        shows(&shown(&p, 110), &["删除 Claude Code 账号 sub?", "2 个会话正在使用它,删除后这些会话将无法重启。", "它是 claude 的默认账号"]);
        press(&mut p, &[KeyCode::Esc]);
        assert_eq!(names(&p).len(), 4);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn shift_arrows_reorder_within_a_tool_and_the_n_menu_follows() {
        let (root, mut p) = page("order");
        let shift = |p: &mut Page, k: KeyCode| {
            let mut fx = Fx::default();
            p.on_key(KeyEvent::new(k, KeyModifiers::SHIFT), &mut fx);
            fx
        };
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "sub");
        // Already last for this tool: no move, nothing said
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        assert!(shift(&mut p, KeyCode::Down).flash.is_none());
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before);
        // Moved: the list reorders on the spot, no extra message in the bottom bar
        let fx = shift(&mut p, KeyCode::Up);
        assert!(fx.flash.is_none() && names(&p)[2] == "sub");
        press(&mut p, &[KeyCode::Char('K'), KeyCode::Char('K')]);
        // The highlight follows it; Codex's are unaffected
        assert_eq!((names(&p), on(&p)), (vec!["sub", "codex", "firm", "gone", "cx"], "sub"));
        press(&mut p, &[KeyCode::Char('K')]);
        assert_eq!(names(&p)[0], "sub");
        // Kept in [ui] menu, other lines (comments, hide) untouched; the n menu reads this same order
        let text = std::fs::read_to_string(&p.env.config).unwrap();
        assert_eq!(text, before.replace("[ui]\n", "[ui]\nmenu = [\"sub\", \"codex\", \"firm\", \"gone\", \"cx\"]\n"));
        let menu = crate::tools::menu(
            &aca_core::config::parse(&text),
            &crate::tools::Layout { menu: crate::config::list_in(&text, "ui", "menu"), hide: vec![] },
        );
        assert_eq!(menu.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["shell", "sub", "codex", "firm", "gone", "cx"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adding_picks_the_tool_first_and_refuses_the_other_tools_directory() {
        let (root, mut p) = page_with("add", &CONFIG.replace("codex = \"~/.codex\"\n", ""));
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        press(&mut p, &[KeyCode::Char('+')]);
        // Step one: the two tools laid out to pick from, not typing (j / k pick); the form is in the right column, the left list
        // still there
        assert!(!p.typing());
        let text = shown(&p, 110);
        shows(&text, &["添加账号 · 选择工具", "› Claude Code  默认目录 ~/.claude-<名字>", "  Codex        默认目录 ~/.codex-<名字>"]);
        assert!(row(&text, "sub").contains("● max"), "{text:#?}");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.typing());
        shows(&shown(&p, 110), &["添加 Claude Code 账号 · 名字", "字母、数字、- 和 _"]);
        // Name: empty, malformed, or already taken for this tool is reported on the spot
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号名不能为空");
        type_in(&mut p, "a b");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号名只能包含字母、数字、- 和 _");
        retype(&mut p, "sub");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号已存在:sub");
        // Registering Codex's default directory as a Claude account: refused, saying whose it is
        retype(&mut p, "mine");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().text, "~/.claude-mine");
        std::fs::remove_dir_all(root.join(".codex")).unwrap();
        retype(&mut p, "~/.codex");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "~/.codex 是 Codex 的目录(Codex 的默认目录),不能添加为 Claude Code 账号");
        dir(&root, ".elsewhere", &["auth.json", "config.toml", "version.json"]);
        retype(&mut p, "~/.elsewhere");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.form.as_ref().unwrap().hint.starts_with("~/.elsewhere 是 Codex 的目录(其中包含 auth.json、config.toml…)"));
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before);
        // Esc back step by step to step one, switch to Codex: the same directory can be added; it already exists, so no login
        press(&mut p, &[KeyCode::Esc, KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        shows(&shown(&p, 110), &["加 Codex 账号 · 名字"]);
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.elsewhere");
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert!(p.form.is_none() && fx.term.is_none());
        assert_eq!(fx.flash.as_deref(), Some("已添加 Codex 账号 mine"));
        assert_eq!((names(&p), on(&p)), (vec!["firm", "gone", "sub", "cx", "mine"], "mine"));
        shows(&shown(&p, 110), &["目录      ~/.elsewhere", "登录      ● ChatGPT"]);
        // A newly created directory: once added, a terminal opens in the bottom half running the tool's own login for this account
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "work");
        press(&mut p, &[KeyCode::Enter]);
        shows(&shown(&p, 110), &["加 Claude Code 账号 · work · 目录", "> ~/.claude-work", "不存在时自动创建"]);
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!(fx.flash.as_deref(), Some("已添加 Claude Code 账号 work"));
        let term = fx.term.unwrap();
        assert!(
            root.join(".claude-work").is_dir() && term.run.as_deref().is_some_and(|c| c.ends_with("' run claude:work")),
            "{:?}",
            term.run
        );
        // Esc on step one: not adding, back to the page
        assert_eq!(key(&mut p, KeyCode::Char('+')).0, Flow::Stay);
        assert_eq!(key(&mut p, KeyCode::Esc).0, Flow::Stay);
        assert!(p.form.is_none());
        assert_eq!(key(&mut p, KeyCode::Esc).0, Flow::Close);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn enter_logs_in_with_the_tools_own_command_and_r_asks_again() {
        let (root, mut p) = page("login");
        // The other tool's directory: no login in it
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!((fx.term.is_none(), fx.flash.as_deref()), (true, Some("是 Codex 的目录,不能在此登录")));
        let fx = press(&mut p, &[KeyCode::Down, KeyCode::Enter]);
        let term = fx.term.unwrap();
        assert_eq!(term.title, "登录 Claude Code · firm");
        assert!(term.run.as_deref().is_some_and(|c| c.ends_with("' run claude:firm -- auth login")));
        // Claude isn't allowed to open the browser itself: the link goes to this file and the UI pops up a choice (Codex's is read off
        // the screen)
        let file = root.join("state/login-claude-firm.url");
        assert_eq!(term.env, [("ACA_LOGIN_LINK".to_string(), file.to_string_lossy().into_owned())]);
        assert_eq!(term.link, Some(crate::term::Catch { file, screen: false }));
        // Logged in (up wraps past the section header to the last row, Codex's): ⏎ doesn't go straight to login — Codex's login
        // deletes the old one the moment it starts, so ask first
        let fx = press(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Up, KeyCode::Enter]);
        assert!(fx.term.is_none() && p.asking == Some(Asking::Relogin));
        let text = shown(&p, 110);
        shows(&text, &["重新登录 Codex 账号 cx?", "当前已登录:ChatGPT。", "Codex 开始登录时会清除当前登录", "y 重新登录 • 其他键 取消"]);
        assert!(text.last().unwrap().is_empty(), "在问的时候底下不写其他键:{text:#?}");
        // Any other key: no login, back to the page, and the key isn't passed on
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert!(fx.term.is_none() && p.asking.is_none() && on(&p) == "cx");
        shows(&shown(&p, 110), &["登录      ● ChatGPT"]);
        assert!(shown(&p, 110).last().unwrap().starts_with("⏎ 重新登录 • + 添加账号"));
        let fx = press(&mut p, &[KeyCode::Enter, KeyCode::Char('y')]);
        let term = fx.term.unwrap();
        assert!(term.run.as_deref().is_some_and(|c| c.ends_with("' run codex:cx -- login")));
        assert!(term.link.is_some_and(|c| c.screen && c.file.ends_with("login-codex-cx.url")));
        // A logged-in Claude account asks first too; not asked yet or can't tell is treated as logged in (not asked yet doesn't say
        // whether it's logged in now)
        let fx = press(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Enter]);
        assert!(fx.term.is_none() && on(&p) == "sub");
        shows(&shown(&p, 110), &["重新登录 Claude Code 账号 sub?", "当前已登录:max · me@example.com。", "重新登录将替换当前登录。"]);
        let fx = press(&mut p, &[KeyCode::Char('y')]);
        assert!(fx.term.unwrap().run.as_deref().is_some_and(|c| c.ends_with("' run claude:sub -- auth login")));
        p.logins.lock().unwrap().remove("claude:sub");
        assert!(press(&mut p, &[KeyCode::Enter]).term.is_none() && p.asking == Some(Asking::Relogin));
        let text = shown(&p, 110);
        shows(&text, &["重新登录 Claude Code 账号 sub?", "重新登录将替换当前登录。"]);
        assert!(!text.iter().any(|l| l.contains("已登录")), "{text:#?}");
        press(&mut p, &[KeyCode::Esc]);
        // The logged-out row says 「登录」 (log in); ⏎ goes straight there
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        assert_eq!(on(&p), "firm");
        shows(&shown(&p, 110), &["登录      ○ 未登录 · ⏎ 登录"]);
        assert!(shown(&p, 110).last().unwrap().starts_with("⏎ 登录 • + 添加账号"));
        // r: drop every answer and ask again (press it after coming back from logging in)
        p.logins.lock().unwrap().insert("claude:firm".into(), Login::Unknown("旧的".into()));
        press(&mut p, &[KeyCode::Char('r')]);
        assert_eq!(p.logins.lock().unwrap().get("claude:firm"), Some(&Login::Out));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_box_keeps_its_size_wide_and_narrow_and_says_so_when_empty() {
        let (root, mut p) = page("draw");
        assert!(p.fill());
        for w in [30, 44, 60, 80, 116] {
            for keys in [&[][..], &[KeyCode::Char('d')][..], &[KeyCode::Esc, KeyCode::Char('+')][..], &[KeyCode::Enter][..]] {
                press(&mut p, keys);
                for h in [14, 24, 40] {
                    let lines: Vec<String> =
                        p.lines_in(w, h).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
                    let over: Vec<&String> = lines.iter().filter(|l| l.width() > w).collect();
                    // Exactly h lines, none too wide: moving the highlight, asking whether to remove, filling the form
                    // leave the frame alone
                    assert!(lines.len() == h && over.is_empty(), "{w}x{h}:{} 行,超宽的 {over:#?}", lines.len());
                }
            }
            press(&mut p, &[KeyCode::Esc, KeyCode::Esc]);
            assert!(p.form.is_none() && p.asking.is_none());
        }
        // Frame height follows the content; moving the highlight or asking whether to remove doesn't change it
        let tall = p.fill_rows();
        let downs = [KeyCode::Down; 4];
        for keys in [&[KeyCode::Down][..], &downs[..], &[KeyCode::Char('d')][..]] {
            press(&mut p, keys);
            assert_eq!(p.fill_rows(), tall);
        }
        // ⏎ on cx (logged in): asking whether to log in again, the frame is just as tall
        press(&mut p, &[KeyCode::Esc, KeyCode::Enter]);
        assert_eq!((on(&p), p.asking, p.fill_rows()), ("cx", Some(Asking::Relogin), tall));
        press(&mut p, &[KeyCode::Esc, KeyCode::Down, KeyCode::Down]);
        assert_eq!((on(&p), tall), ("codex", Some(22)));
        // Wide: rule and keys at the bottom; while filling the form the keys go in the right column and the bottom line is empty
        let wide = shown(&p, 116);
        assert_eq!(wide[wide.len() - 1], "⏎ 登录 • + 添加账号 • d 删除 • ⇧↑↓ 移动 • p 启动参数 • s 状态栏 • r 刷新");
        assert!(wide[wide.len() - 2].starts_with("────"), "{wide:#?}");
        press(&mut p, &[KeyCode::Char('+')]);
        let form = shown(&p, 116);
        assert!(form.last().unwrap().is_empty() && form.iter().any(|l| l.ends_with("⏎ 下一步 • ↑↓ 切换 • Esc 返回")), "{form:#?}");
        press(&mut p, &[KeyCode::Esc]);
        // Phone width: stacked, list on top, the highlighted one's page below
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        let narrow = shown(&p, 42);
        let list = narrow.iter().position(|l| l.starts_with("› sub ") && l.contains("● max")).unwrap_or_else(|| panic!("{narrow:#?}"));
        let rule = narrow.iter().position(|l| l.starts_with("┄┄")).unwrap();
        let full = narrow.iter().position(|l| l.contains("登录      ● max · me@example.com")).unwrap_or_else(|| panic!("{narrow:#?}"));
        assert!(list < rule && rule < full, "{narrow:#?}");
        let (root2, env) = home("empty", "");
        let none = Page::load(env, vec![], false);
        let text = shown(&none, 110);
        // The installed tool (only Claude here) gets a section header, highlighted: whether the one used without an account is logged
        // in and how to log in; launch args can be edited on the header; Codex, not installed, isn't mentioned
        assert_eq!(on(&none), "Claude Code");
        assert!(text.iter().any(|l| l.starts_with("Claude Code") && l.contains("│  Claude Code")), "{text:#?}");
        shows(&text, &["  暂无账号", "默认使用 ~/.claude · + 添加账号", "登录      ○ 未登录"]);
        shows(&text, &["          登录:aca run claude -- auth login"]);
        assert!(!text.iter().any(|l| l.contains("Codex") || l.contains("aca run codex")), "{text:#?}");
        assert_eq!(text.last().unwrap(), "+ 添加账号 • p 启动参数 • r 刷新");
        // No tool installed: only adding is possible
        let (root3, mut env) = home("bare", "");
        env.installed = |_| false;
        let bare = Page::load(env, vec![], false);
        let text = shown(&bare, 110);
        shows(&text, &["暂无账号", "默认使用 ~/.claude、~/.codex"]);
        assert_eq!(text.last().unwrap(), "+ 添加账号");
        assert!(bare.item().is_none());
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&root2).unwrap();
        std::fs::remove_dir_all(&root3).unwrap();
    }
}
