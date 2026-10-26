//! 账号页:`^S` → `a`;`n` 菜单里按 `+` 也到这(直接进加账号)。
//!
//! 和插件页一样是大小固定的大弹窗:左边一栏是登记的账号,分成 Claude Code、Codex 两段,一行「名字  登录着没有」
//! (问的是工具自己,见 accounts.rs 的 `login`);右边一栏是高亮那个账号的整页 —— 配置目录、登录的是谁、额度(同 `n` 菜单)、
//! 状态栏现在用谁的、启动参数;有会话在用、是默认账号、`n` 菜单里不显示的才写;目录不在、目录其实是另一家的在这说怎么办。
//! 段头(Claude Code、Codex)也能高亮:右边是这一家每个账号都带的启动参数(`[tools.<家>] args`);
//! 这一家没登记账号的,还有不登记时用的那个目录登录着没有。没登记账号、装着的那家也列段头。
//! 窄屏(手机)上两栏上下叠。加账号的表单、删之前的确认也在右边那一栏,框不跟着变。
//!
//! ⏎ 登录(下半屏开终端跑工具自己的登录)· `+` 加 · `d` 删(先说清动什么、谁在用,y 才删;只去掉登记那一行,目录不动)·
//! ⇧↑ ⇧↓ 挪顺序(就是 `n` 菜单的顺序,记在 `[ui] menu`)· `p` 改启动参数(就在右边那一栏里改):账号上改它自己的,
//! 存进它在 config.toml 的那一行;段头上改这一家每个账号都带的 · `s` 各账号的状态栏(setup.rs 的表)· `r` 重新问一遍登录、额度。
//!
//! [`Form`] 是加账号填的三步(哪个工具 → 名字 → 配置目录),`aca setup` 的表也用它。

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

/// 表头、表单里写哪一家:它的颜色、粗体(一眼分得出是 Claude Code 还是 Codex)
pub(crate) fn who_span(a: Agent) -> Span<'static> {
    Span::styled(accounts::who(a), agent_style(a).add_modifier(Modifier::BOLD))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Tool,
    Name,
    Dir,
}

/// 加账号填的三步。哪个工具单独一步、摆成两行来挑:原来是名字那一步按 Tab 换、只差标题上一个词,
/// 容易把 Codex 的目录登记成 Claude 的账号
#[derive(Clone, Debug)]
pub(crate) struct Form {
    step: Step,
    pub agent: Agent,
    /// 名字那一步过了才有
    pub name: String,
    /// 输入框里的字:名字那一步是名字,目录那一步是目录
    pub text: String,
    pub hint: String,
}

/// 表单按完一个键
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Went {
    Stay,
    /// 第一步上按了 Esc:不加了
    Left,
    /// 目录填完按了回车:`agent` / `name` / `text` 交给开它的那一页核对、去办;不行就写 `hint`,表单留着
    Filled,
}

impl Form {
    pub fn new() -> Form {
        Form { step: Step::Tool, agent: Agent::Claude, name: String::new(), text: String::new(), hint: String::new() }
    }

    /// 在打字(挑工具那一步不算:那时 j / k 是挑)
    pub fn typing(&self) -> bool {
        self.step != Step::Tool
    }

    /// `taken`:这一家里已经有这个名字了
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
            // 退一步:目录 → 名字 → 哪个工具
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

    /// `done`:目录那一步回车写的是什么(「加上」/「列进表里」)
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

/// 读写的那几处(测试里换成临时目录、假的登录)
#[derive(Clone)]
pub struct Env {
    pub config: PathBuf,
    pub home: String,
    /// 问工具这个账号登录着没有
    pub ask: fn(Agent, &str) -> Login,
    /// 放到后台问、问到了下一帧就显示(界面里);测试里当场问
    pub background: bool,
    /// 登录链接交到这个目录里的文件(见 `aca_core::agent::LOGIN_LINK`)
    pub links: PathBuf,
    /// 人不在这台机器跟前(`aca_core::agent::remote`):Claude 也不交链接,从屏幕上认
    pub remote: bool,
    /// 这个程序装了没有(一个都没登记时,列装着的那家自己的)
    pub installed: fn(&str) -> bool,
    /// 各账号的订阅额度(Claude 的是状态栏 / mod 记下的,Codex 的读它最近的对话记录);和登录一样放到后台拿
    pub quotas: fn() -> Vec<Quota>,
}

/// 登记着的一个会话:哪一家、用的账号、标题
pub(crate) type Use = (Agent, String, String);

struct Row {
    a: Account,
    /// 配置目录在(不换目录的也算在)
    there: bool,
    /// 目录是另一家的:哪一家、凭什么这么说
    foreign: Option<(Agent, String)>,
    /// 在用它的会话的标题
    used: Vec<String>,
    /// 哪些工具不写账号时用它
    default_of: Vec<String>,
    /// `n` 菜单里列着(藏着的 `[ui] hide` 不算)
    in_menu: bool,
    /// Claude 的、目录没毛病的:状态栏现在用谁的,aca 记不记得到额度
    bar: Option<(&'static str, bool)>,
    /// 账号自己的启动参数(`[accounts.<家>] 名 = { args = […] }`)
    args: Vec<String>,
}

/// 列着段头的一家
struct Fam {
    agent: Agent,
    /// 这一家每个账号都带的启动参数(`[tools.<家>] args`),起的时候排在账号自己的前面
    args: Vec<String>,
}

/// 左边那一栏里能高亮的一项:一家的段头,或一个账号(`rows` 里第几个)
#[derive(Clone, Copy, Debug, PartialEq)]
enum Item {
    Head(Agent),
    Acct(usize),
}

impl Row {
    fn spec(&self) -> String {
        format!("{}:{}", accounts::fam(self.a.agent).unwrap_or("claude"), self.a.name)
    }

    /// 问得了登录:目录在、是它自己这一家的(不在的目录一问会被建出来,另一家的目录里不该跑它)
    fn askable(&self) -> bool {
        self.there && self.foreign.is_none()
    }
}

/// 右边那一栏在问的事,等 y
#[derive(Clone, Copy, Debug, PartialEq)]
enum Asking {
    /// 按过 d:删这条登记
    Remove,
    /// 在登录着的账号上按了 ⏎:重新登录。工具的登录一开始,原来那份就没了(Codex 0.162 实测:`codex login` 一启动就删
    /// auth.json,浏览器里没登完就是没登录),所以先问
    Relogin,
}

/// 不登记时那一家自己的,登录记在这个名下(`aca run claude` 不带账号)
fn own_spec(a: Agent) -> String {
    accounts::fam(a).unwrap_or("claude").to_string()
}

/// 问到的登录怎么写:记号、一句话、颜色。None = 还在问
fn login_said(l: Option<Login>) -> (&'static str, String, Style) {
    match l {
        None => ("…", t!("查询中", "asking").into(), kit::dim()),
        Some(Login::In(who)) if who.is_empty() => ("●", t!("已登录", "logged in").into(), Style::new().fg(theme::pal().green)),
        Some(Login::In(who)) => ("●", who, Style::new().fg(theme::pal().green)),
        Some(Login::Out) => ("○", t!("未登录", "not logged in").into(), theme::warn()),
        Some(Login::Unknown(said)) => ("?", said, kit::dim()),
    }
}

/// 右边那一栏最多列几个会话的标题
const TITLES: usize = 5;
/// 加账号的表单最多几行(提示折成两行时)
const FORM_ROWS: usize = 7;
/// 启动参数一行最多输这么长
const ARGS_MAX: usize = 2000;

/// 右边那一栏里各项的名字
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

/// 名字那一列多宽
fn label_w() -> usize {
    labels().iter().map(|l| l.width()).max().unwrap_or(0) + 2
}

/// 右边那一栏里的一项:「名字  值」,值太长折到下面、和值对齐;lead 是值前面的记号(● ○ ⚠)
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

/// 启动参数改的时候是一行字,照 shell 的写法分:空格隔开,带空格的用引号包上(`'…'` 原样,`"…"` 里 `\"` `\\` 转义)。
/// 引号没配上 = Err
fn split(s: &str) -> Result<Vec<String>, String> {
    let unclosed = || t!("引号不成对", "a quote isn't closed").to_string();
    let mut out = vec![];
    // None = 不在一个词里(连着的引号、字算同一个词:`--x='a b'`)
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

/// 参数拼回一行:空的、带空格引号的用单引号包上,`split` 分回来还是这几个
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
    /// 列着段头的那几家:有账号的、装着的、写了启动参数的
    fams: Vec<Fam>,
    /// 不登记时各家用的那个(`accounts::defaults`):这一家没登记账号时,段头那页说它登录着没有
    own: Vec<Account>,
    /// 高亮的是 `items()` 里第几项
    sel: usize,
    uses: Vec<Use>,
    /// 问到的登录,按「工具:账号」记;没有 = 还在问
    logins: Arc<Mutex<HashMap<String, Login>>>,
    form: Option<Form>,
    asking: Option<Asking>,
    /// 拿到的额度;还在拿 = None
    quotas: Arc<Mutex<Option<Vec<Quota>>>>,
    /// 拿额度的时刻(算重置、「几小时前的数」)和本地时区偏移
    at: i64,
    off: i64,
    /// 开了 Claude mod:额度不靠状态栏
    mods: bool,
    /// 按了 p,在改高亮那个账号(段头:那一家每个账号都带)的启动参数:输入框里的字
    editing: Option<String>,
    /// 参数没存成:为什么
    args_err: String,
}

impl Page {
    /// `adding`:一开就进加账号(`n` 菜单里按 `+`)
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

    /// `uses`:登记着的会话各用的(哪一家, 账号名, 标题)
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
        // 一开高亮第一个账号(没有账号的是第一个段头)
        p.sel = p.items().iter().position(|i| matches!(i, Item::Acct(_))).unwrap_or(0);
        p.fetch_quotas();
        p
    }

    /// 左边那一栏从上到下能高亮的:每一家先是段头,再是它的账号
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

    /// 高亮的那个账号;高亮的是段头 = None
    fn cur(&self) -> Option<&Row> {
        match self.item()? {
            Item::Acct(i) => self.rows.get(i),
            Item::Head(_) => None,
        }
    }

    /// 这一家每个账号都带的启动参数
    fn common(&self, a: Agent) -> &[String] {
        self.fams.iter().find(|f| f.agent == a).map_or(&[], |f| &f.args)
    }

    /// 照 config.toml 现在写的重列;高亮跟着原来那一项
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

    /// 两栏各多宽(里面一共 w 宽):左边照最长的名字,右边是剩下的
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

    /// 问还没问过的账号登录着没有;`again` = 都重问
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
        // 这一家一个都没登记:问它自己的那个(段头那页要说)
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

    /// 重新拿各账号的额度
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

    /// 这个账号现在怎么样:记号、一句话、颜色。左边列表只写这句话的头一段
    fn state(&self, r: &Row) -> (&'static str, String, Style) {
        if let Some((other, _)) = &r.foreign {
            return ("⚠", tr!("是 {} 的目录", "a {} directory", accounts::who(*other)), theme::warn());
        }
        if !r.there {
            return ("⚠", t!("目录不存在", "directory missing").into(), theme::warn());
        }
        login_said(self.login_of(r))
    }

    /// 左边那一栏:两段(Claude Code、Codex),段头单独一行,一行一个账号「名字  记号 头一段」。连同高亮在第几行
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

    /// 右边那一栏:在加账号就是那张表单,按过 d 是要删什么,高亮的是段头就是那一家的,不然是高亮那个账号的整页
    fn right(&self, w: usize) -> Vec<Line<'static>> {
        if let Some(f) = &self.form {
            return f.lines(w, t!("添加", "add"));
        }
        match self.item() {
            // 哪一家都没装、没登记(键栏有 +)
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

    /// 段头那一页:这一家每个账号都带的启动参数。这一家没登记账号的,先说不登记时用的那个目录登录着没有、怎么登
    /// (没登录的新用户在这看得出来)
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

    /// 启动参数那一项(`now` 是现在的,空的不写);在改时是输入框
    fn args_field(&self, out: &mut Vec<Line<'static>>, w: usize, now: &str) {
        let (label, lw) = (labels()[4], label_w());
        match &self.editing {
            Some(text) => field(out, w, lw, label, None, &format!("{text}▏"), theme::accent()),
            None if !now.is_empty() => field(out, w, lw, label, None, now, Style::new()),
            None => {}
        }
    }

    /// 在改启动参数时输入框下面的:没存成的为什么、键
    fn args_keys(&self, out: &mut Vec<Line<'static>>, w: usize) {
        if !self.args_err.is_empty() {
            field(out, w, label_w(), "", None, &self.args_err, theme::warn());
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("⏎", t!("保存", "save")), ("^U", t!("清空", "clear"))], vec![], w));
    }

    /// 名字后面那个「默认」:哪些工具不写账号时用它。只有它自己那一家的就不写是哪个
    fn default_tag(&self, r: &Row) -> Option<String> {
        let fam = accounts::fam(r.a.agent).unwrap_or_default();
        match r.default_of.as_slice() {
            [] => None,
            [only] if only == fam => Some(t!("默认", "default").into()),
            tools => Some(tr!("默认:{}", "default: {}", tools.join(t!("、", ", ")))),
        }
    }

    /// 高亮那个账号的整页:是哪个工具的、是不是默认、目录、登录的是谁、额度、状态栏、启动参数;
    /// 有会话在用、n 菜单里不显示的才写。在改启动参数时,输入框和键写在启动参数那一项下面,后面的不写
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
        // 额度记到了才写;Claude 的状态栏没接(也没开 mod)记不到,写在状态栏那一项
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
        // 启动参数:账号自己的,和这一家每个账号都带的(排在前面,在段头上改)
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
        // 有内容才写的几项,和上面空一行
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

    /// 按过 d:会动什么、谁在用
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

    /// 工具说了它没登录(目录不在的也算:里面没有登录可丢)。登录着的、还没问到的、问不出的都不算
    fn logged_out(&self, r: &Row) -> bool {
        !r.there || self.login_of(r) == Some(Login::Out)
    }

    /// 在登录着的账号上按了 ⏎:重新登录会丢掉什么,y 才去
    fn relogin(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let me = accounts::who(r.a.agent);
        // 还没问到的不写:当它登录着问一句
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

    /// 高亮那一行上的 ⏎:没登录的直接去登录;登录着的先问(见 [`Asking::Relogin`])
    fn login(&mut self, fx: &mut Fx) {
        let Some(r) = self.cur() else { return };
        if let Some((other, _)) = &r.foreign {
            return fx.say(tr!("是 {} 的目录,不能在此登录", "a {} directory, not logging in here", accounts::who(*other)));
        }
        if self.logged_out(r) { self.open_login(fx) } else { self.asking = Some(Asking::Relogin) }
    }

    /// 在下半屏开个终端,带着高亮那个账号跑工具自己的登录
    fn open_login(&self, fx: &mut Fx) {
        let Some(r) = self.cur() else { return };
        fx.term = Some(self.term(&r.a.name, r.a.agent, accounts::relogin_cmd(r.a.agent, &r.a.name)));
    }

    /// 登录不让工具自己开默认浏览器:链接交给界面(文件),弹出来挑用哪个开(见 browser.rs)。
    /// Codex 不交、远程时 Claude 也不交(给的是要贴码的那条),界面从屏幕上认;上次留下的文件先删掉,免得一开就弹旧的
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

    /// 表单的目录填完了:记进 config.toml;目录是新建的就开终端登录
    fn add(&mut self, fx: &mut Fx) {
        let Some(f) = self.form.as_mut() else { return };
        let (a, name) = (f.agent, f.name.clone());
        match accounts::add_in(&self.env.config, &self.env.home, a, &name, f.text.trim()) {
            Err(e) => f.hint = e,
            Ok(added) => {
                self.form = None;
                self.read();
                self.at(a, &name);
                // Claude 的额度靠状态栏:接不接在 s 里挑,不替人接(键栏写着 s)
                fx.say(tr!("已添加 {} 账号 {name}", "added the {} account {name}", accounts::who(a)));
                if added.created {
                    fx.term = Some(self.term(&name, a, accounts::login_cmd(a, &name)));
                }
            }
        }
    }

    /// 存高亮那个账号的启动参数(段头:那一家每个账号都带的);没存成的为什么写在输入框下面,接着改
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
                // 和原来一样:不写、不说
                if changed {
                    // 高亮跟着原来那一项
                    self.read();
                    // 开着的会话不动,新起、重启的才用这份
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
            // 目录留着:删之前的确认里说过了
            Ok(_) => {
                fx.say(tr!("已删除 {} 账号 {name}", "removed the {} account {name}", accounts::who(a)));
                self.read();
            }
            Err(e) => fx.say(e),
        }
    }

    /// 挪一格:列表当场换了顺序,不另外说(记在 `[ui] menu`,`n` 菜单同一个顺序)
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

    /// 和插件页一样大小固定:挪高亮、进表单、问删不删,框都不跟着变
    fn fill(&self) -> bool {
        true
    }

    /// 高度照内容定(不像插件页占满屏幕:账号就那么几个,占满了下半截全是空的):标题、两栏里长的那一栏、细线和键。
    /// 右边按最长的那个账号、段头算(连同删它之前的确认),所以挪高亮不变,加了 / 删了账号才变
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

    /// 标题、两栏(窄屏上下叠)、细线、键;正好 h 行
    fn lines_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let busy = self.form.is_some() || self.asking.is_some() || self.editing.is_some();
        let esc = kit::esc_hint(if busy { t!("返回", "back") } else { t!("关闭", "close") });
        let mut out = vec![kit::modal_title(t!("账号", "Accounts"), theme::accent(), esc, w), Line::raw("")];
        // 放不下时后面的键先不写:最要紧的排前面。在填表单、在问删不删时键写在右边那一栏里
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
        // 段头上没有只对一个账号的那几个;状态栏那张表一个账号都没有时是空的
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
            // 手机:列表在上(最多占一半),高亮那个的整页在下
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
            // 账号上改它自己的,段头上改这一家每个账号都带的
            KeyCode::Char('p') => {
                self.editing = match self.item() {
                    Some(Item::Head(a)) => Some(join(self.common(a))),
                    Some(Item::Acct(i)) => Some(join(&self.rows[i].args)),
                    None => None,
                }
            }
            // 各账号的状态栏接不接(`aca setup` 开的也是这张表;加账号在这一页,表里不给加)
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

    /// config.toml 在编辑器里改了、状态栏那张表办完了:重列
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
            // 这个家里只装了 Claude
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

    /// 高亮的那一项:账号是它的名字,段头是那一家
    fn on(p: &Page) -> &str {
        match p.item() {
            Some(Item::Acct(i)) => &p.rows[i].a.name,
            Some(Item::Head(a)) => accounts::who(a),
            None => "",
        }
    }

    /// 左边那一栏里这个账号的那一行(高亮的前面是 ›)
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
        // Codex 的目录,被登记成 Claude 的起过一次:多了个 .claude.json
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
        // sub:额度;启动参数只有每个 Claude 账号都带的那份
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "sub");
        shows(&shown(&p, 110), &["额度      5h 12% · 7d 61%", "启动参数  所有 Claude Code 账号都附带:--x"]);
        // p:改它自己的,存进 config.toml 那一行
        press(&mut p, &[KeyCode::Char('p')]);
        assert!(p.typing());
        type_in(&mut p, "--model 'big one'");
        shows(&shown(&p, 110), &["启动参数  --model 'big one'▏", "⏎ 保存 • ^U 清空"]);
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!((fx.flash.as_deref(), p.editing.is_none()), (Some("已保存,新会话生效"), true));
        let saved = std::fs::read_to_string(&p.env.config).unwrap();
        assert!(saved.contains("sub = { home = \"~/.claude-sub\", args = [\"--model\", \"big one\"] }\n"), "{saved}");
        shows(&shown(&p, 110), &["启动参数  --model 'big one'", "          所有 Claude Code 账号都附带:--x"]);
        // 引号没配上:说为什么,接着改;Esc 不改
        press(&mut p, &[KeyCode::Char('p')]);
        type_in(&mut p, " \"oops");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.editing.is_some() && !p.args_err.is_empty());
        press(&mut p, &[KeyCode::Esc]);
        assert!(p.editing.is_none() && std::fs::read_to_string(&p.env.config).unwrap() == saved);
        // 清空:写回一行目录
        press(&mut p, &[KeyCode::Char('p')]);
        p.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &mut Fx::default());
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), config);
        // Codex 的额度不用接状态栏;余额写在后面
        press(&mut p, &[KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "cx");
        shows(&shown(&p, 110), &["额度      5h 95%  余额 5"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn p_on_a_tools_heading_changes_what_every_account_of_it_gets() {
        let config = CONFIG.replace("account = \"sub\"\n", "account = \"sub\"\nargs = [\"--x\"]\n");
        let (root, mut p) = page_with("common", &config);
        // 段头也能高亮:在第一个账号上往上一格
        press(&mut p, &[KeyCode::Up]);
        assert_eq!(on(&p), "Claude Code");
        let text = shown(&p, 110);
        assert!(text.iter().any(|l| l.starts_with("Claude Code") && l.contains("│  Claude Code")), "{text:#?}");
        shows(&text, &["启动参数  --x"]);
        // 段头上没有只对一个账号的键
        assert_eq!(text.last().unwrap(), "+ 添加账号 • p 启动参数 • s 状态栏 • r 刷新");
        assert!(press(&mut p, &[KeyCode::Enter]).term.is_none() && p.asking.is_none());
        press(&mut p, &[KeyCode::Char('d')]);
        assert!(p.asking.is_none());
        // p:改这一家每个账号都带的,存进 [tools.claude] 那一段,别的一个字不动
        press(&mut p, &[KeyCode::Char('p')]);
        assert!(p.typing());
        type_in(&mut p, " --effort high");
        shows(&shown(&p, 110), &["启动参数  --x --effort high▏", "⏎ 保存 • ^U 清空"]);
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!((fx.flash.as_deref(), p.editing.is_none(), on(&p)), (Some("已保存,新会话生效"), true, "Claude Code"));
        let saved = std::fs::read_to_string(&p.env.config).unwrap();
        assert_eq!(saved, config.replace("args = [\"--x\"]", "args = [\"--x\", \"--effort\", \"high\"]"));
        // 账号那页写着每个账号都带的
        press(&mut p, &[KeyCode::Up]);
        assert_eq!(on(&p), "cx");
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(on(&p), "sub");
        shows(&shown(&p, 110), &["启动参数  所有 Claude Code 账号都附带:--x --effort high"]);
        // Codex 那一段:原来没有这一段,加在最后
        press(&mut p, &[KeyCode::Down]);
        assert_eq!(on(&p), "Codex");
        assert!(!shown(&p, 110).iter().any(|l| l.contains("启动参数  ")));
        press(&mut p, &[KeyCode::Char('p')]);
        type_in(&mut p, "--search");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), format!("{saved}\n[tools.codex]\nargs = [\"--search\"]\n"));
        press(&mut p, &[KeyCode::Down]);
        shows(&shown(&p, 110), &["启动参数  所有 Codex 账号都附带:--search"]);
        // 没改就回车:不写、不说;引号没配上:说为什么,接着改
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
        // 左边:两段各有表头(单独一行写哪个工具),账号在自己那一段里,各带登录着没有
        let at = |want: &str| text.iter().position(|l| l.starts_with(want)).unwrap_or_else(|| panic!("没有「{want}」开头的行:{text:#?}"));
        assert!(at("Claude Code") < at("  sub ") && at("  gone ") < at("Codex") && at("Codex") < at("  cx "), "{text:#?}");
        assert!(row(&text, "sub").contains("● max") && !row(&text, "sub").contains("me@example.com"), "{text:#?}");
        assert!(row(&text, "firm").contains("○ 未登录") && row(&text, "cx").contains("● ChatGPT"), "{text:#?}");
        // 另一家的目录、不在的目录:标出来,不去问它登录(问了会在里面写东西)
        assert!(row(&text, "codex").contains("⚠ 是 Codex 的目录") && row(&text, "gone").contains("⚠ 目录不存在"), "{text:#?}");
        assert!(!p.logins.lock().unwrap().keys().any(|k| k == "claude:codex" || k == "claude:gone"));
        // 右边:高亮那个(加错的)—— 是谁的目录、怎么办;没有登录那一项
        assert_eq!(on(&p), "codex");
        shows(&text, &["⚠ 这是 Codex 的目录(其中包含 auth.json、config.toml…)", "d 删除 · + 添加为 Codex 账号", "目录      ~/.codex"]);
        assert!(!text.iter().any(|l| l.contains(" 登录    ")), "{text:#?}");
        press(&mut p, &[KeyCode::Down, KeyCode::Down]);
        shows(&shown(&p, 110), &["登录      ⚠ 目录不存在 · d 删除"]);
        // 登录着的:是谁、是默认、状态栏、哪些会话在用;n 菜单里列着就不写
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
        // 没会话在用、不是默认的不写;n 菜单里藏着的写一句
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        let text = shown(&p, 110);
        shows(&text, &["登录      ○ 未登录 · ⏎ 登录", "n 菜单    不显示"]);
        assert!(!text.iter().any(|l| l.contains(" 会话    ") || l.contains("默认")), "{text:#?}");
        // 往上绕过段头到最后一个
        press(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Up]);
        assert_eq!(on(&p), "cx");
        shows(&shown(&p, 110), &["cx", "登录      ● ChatGPT"]);
        // Codex 的额度不靠状态栏:没有那一项
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
        // 别的键:不删,键也不往下走(j 不挪高亮)
        press(&mut p, &[KeyCode::Char('j')]);
        assert_eq!((on(&p), std::fs::read_to_string(&p.env.config).unwrap()), ("codex", before.clone()));
        let fx = press(&mut p, &[KeyCode::Char('d'), KeyCode::Char('y')]);
        assert_eq!(fx.flash.as_deref(), Some("已删除 Claude Code 账号 codex"));
        assert_eq!(names(&p), ["firm", "gone", "sub", "cx"]);
        // 只少了那一行;目录和里面的东西都在;删之前的那份留着
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before.replace("codex = \"~/.codex\"\n", ""));
        assert!(root.join(".codex/auth.json").exists() && root.join(".codex/.claude.json").exists());
        assert_eq!(std::fs::read_to_string(root.join("aca/config.toml.bak-aca")).unwrap(), before);
        // 有会话在用的、是默认账号的:删之前说清楚
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
        // 已经在这一家的最后:不动,什么都不写
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        assert!(shift(&mut p, KeyCode::Down).flash.is_none());
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before);
        // 挪了:列表当场换顺序,底栏不另外说
        let fx = shift(&mut p, KeyCode::Up);
        assert!(fx.flash.is_none() && names(&p)[2] == "sub");
        press(&mut p, &[KeyCode::Char('K'), KeyCode::Char('K')]);
        // 高亮跟着它;Codex 的不受影响
        assert_eq!((names(&p), on(&p)), (vec!["sub", "codex", "firm", "gone", "cx"], "sub"));
        press(&mut p, &[KeyCode::Char('K')]);
        assert_eq!(names(&p)[0], "sub");
        // 记在 [ui] menu,别的行(注释、hide)原样;n 菜单读到的就是这个顺序
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
        // 第一步:两个工具摆出来挑,不是在打字(j / k 是挑);表单在右边那一栏,左边的列表还在
        assert!(!p.typing());
        let text = shown(&p, 110);
        shows(&text, &["添加账号 · 选择工具", "› Claude Code  默认目录 ~/.claude-<名字>", "  Codex        默认目录 ~/.codex-<名字>"]);
        assert!(row(&text, "sub").contains("● max"), "{text:#?}");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.typing());
        shows(&shown(&p, 110), &["添加 Claude Code 账号 · 名字", "字母、数字、- 和 _"]);
        // 名字:空的、写得不对的、这一家里重了的,当场说
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号名不能为空");
        type_in(&mut p, "a b");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号名只能包含字母、数字、- 和 _");
        retype(&mut p, "sub");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号已存在:sub");
        // 把 Codex 默认的目录登记成 Claude 的账号:不给,说它是谁的
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
        // Esc 一步步退回第一步,换成 Codex:同一个目录就能加了;目录本来就在,不用登录
        press(&mut p, &[KeyCode::Esc, KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        shows(&shown(&p, 110), &["加 Codex 账号 · 名字"]);
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.elsewhere");
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert!(p.form.is_none() && fx.term.is_none());
        assert_eq!(fx.flash.as_deref(), Some("已添加 Codex 账号 mine"));
        assert_eq!((names(&p), on(&p)), (vec!["firm", "gone", "sub", "cx", "mine"], "mine"));
        shows(&shown(&p, 110), &["目录      ~/.elsewhere", "登录      ● ChatGPT"]);
        // 新建的目录:加完在下半屏开终端,带着这个账号跑它自己的登录
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
        // 第一步上 Esc:不加了,回到表
        assert_eq!(key(&mut p, KeyCode::Char('+')).0, Flow::Stay);
        assert_eq!(key(&mut p, KeyCode::Esc).0, Flow::Stay);
        assert!(p.form.is_none());
        assert_eq!(key(&mut p, KeyCode::Esc).0, Flow::Close);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn enter_logs_in_with_the_tools_own_command_and_r_asks_again() {
        let (root, mut p) = page("login");
        // 另一家的目录:不在里面登录
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert_eq!((fx.term.is_none(), fx.flash.as_deref()), (true, Some("是 Codex 的目录,不能在此登录")));
        let fx = press(&mut p, &[KeyCode::Down, KeyCode::Enter]);
        let term = fx.term.unwrap();
        assert_eq!(term.title, "登录 Claude Code · firm");
        assert!(term.run.as_deref().is_some_and(|c| c.ends_with("' run claude:firm -- auth login")));
        // 不让 Claude 自己开浏览器:链接交到这个文件,界面弹出来挑(Codex 的从屏幕上认)
        let file = root.join("state/login-claude-firm.url");
        assert_eq!(term.env, [("ACA_LOGIN_LINK".to_string(), file.to_string_lossy().into_owned())]);
        assert_eq!(term.link, Some(crate::term::Catch { file, screen: false }));
        // 登录着的(往上绕过段头到最后一行,Codex 的):⏎ 不直接去登录 —— Codex 的登录一开始就把原来那份删了,先问
        let fx = press(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Up, KeyCode::Enter]);
        assert!(fx.term.is_none() && p.asking == Some(Asking::Relogin));
        let text = shown(&p, 110);
        shows(&text, &["重新登录 Codex 账号 cx?", "当前已登录:ChatGPT。", "Codex 开始登录时会清除当前登录", "y 重新登录 • 其他键 取消"]);
        assert!(text.last().unwrap().is_empty(), "在问的时候底下不写其他键:{text:#?}");
        // 别的键:不登录,回到那一页,键也不往下走
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert!(fx.term.is_none() && p.asking.is_none() && on(&p) == "cx");
        shows(&shown(&p, 110), &["登录      ● ChatGPT"]);
        assert!(shown(&p, 110).last().unwrap().starts_with("⏎ 重新登录 • + 添加账号"));
        let fx = press(&mut p, &[KeyCode::Enter, KeyCode::Char('y')]);
        let term = fx.term.unwrap();
        assert!(term.run.as_deref().is_some_and(|c| c.ends_with("' run codex:cx -- login")));
        assert!(term.link.is_some_and(|c| c.screen && c.file.ends_with("login-codex-cx.url")));
        // Claude 登录着的也先问;还没问到的、问不出的当它登录着(还没问到的不写现在登录着没有)
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
        // 没登录的那一行写的是「登录」,⏎ 直接去
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        assert_eq!(on(&p), "firm");
        shows(&shown(&p, 110), &["登录      ○ 未登录 · ⏎ 登录"]);
        assert!(shown(&p, 110).last().unwrap().starts_with("⏎ 登录 • + 添加账号"));
        // r:问到的都丢掉重问(登录完回来按一下)
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
                    // 正好 h 行、不超宽:挪高亮、问删不删、填表单,框都不变
                    assert!(lines.len() == h && over.is_empty(), "{w}x{h}:{} 行,超宽的 {over:#?}", lines.len());
                }
            }
            press(&mut p, &[KeyCode::Esc, KeyCode::Esc]);
            assert!(p.form.is_none() && p.asking.is_none());
        }
        // 框多高照内容定,挪高亮、问删不删都不变
        let tall = p.fill_rows();
        let downs = [KeyCode::Down; 4];
        for keys in [&[KeyCode::Down][..], &downs[..], &[KeyCode::Char('d')][..]] {
            press(&mut p, keys);
            assert_eq!(p.fill_rows(), tall);
        }
        // 停在 cx(登录着)上按 ⏎:在问要不要重新登录,框还是那么高
        press(&mut p, &[KeyCode::Esc, KeyCode::Enter]);
        assert_eq!((on(&p), p.asking, p.fill_rows()), ("cx", Some(Asking::Relogin), tall));
        press(&mut p, &[KeyCode::Esc, KeyCode::Down, KeyCode::Down]);
        assert_eq!((on(&p), tall), ("codex", Some(22)));
        // 宽的:最下面是细线和键;在填表单时键写在右边那一栏里,底下那行空着
        let wide = shown(&p, 116);
        assert_eq!(wide[wide.len() - 1], "⏎ 登录 • + 添加账号 • d 删除 • ⇧↑↓ 移动 • p 启动参数 • s 状态栏 • r 刷新");
        assert!(wide[wide.len() - 2].starts_with("────"), "{wide:#?}");
        press(&mut p, &[KeyCode::Char('+')]);
        let form = shown(&p, 116);
        assert!(form.last().unwrap().is_empty() && form.iter().any(|l| l.ends_with("⏎ 下一步 • ↑↓ 切换 • Esc 返回")), "{form:#?}");
        press(&mut p, &[KeyCode::Esc]);
        // 手机那么宽:上下叠,列表在上、高亮那个的整页在下
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        let narrow = shown(&p, 42);
        let list = narrow.iter().position(|l| l.starts_with("› sub ") && l.contains("● max")).unwrap_or_else(|| panic!("{narrow:#?}"));
        let rule = narrow.iter().position(|l| l.starts_with("┄┄")).unwrap();
        let full = narrow.iter().position(|l| l.contains("登录      ● max · me@example.com")).unwrap_or_else(|| panic!("{narrow:#?}"));
        assert!(list < rule && rule < full, "{narrow:#?}");
        let (root2, env) = home("empty", "");
        let none = Page::load(env, vec![], false);
        let text = shown(&none, 110);
        // 装着的那家(这里只装了 Claude)列段头,高亮着:不加账号时用的那个登录着没有、怎么登,段头上能改启动参数;
        // 没装的 Codex 不提
        assert_eq!(on(&none), "Claude Code");
        assert!(text.iter().any(|l| l.starts_with("Claude Code") && l.contains("│  Claude Code")), "{text:#?}");
        shows(&text, &["  暂无账号", "默认使用 ~/.claude · + 添加账号", "登录      ○ 未登录"]);
        shows(&text, &["          登录:aca run claude -- auth login"]);
        assert!(!text.iter().any(|l| l.contains("Codex") || l.contains("aca run codex")), "{text:#?}");
        assert_eq!(text.last().unwrap(), "+ 添加账号 • p 启动参数 • r 刷新");
        // 哪一家都没装:只能加
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
