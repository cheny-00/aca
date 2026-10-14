//! 账号页:`^S` → `a`;`n` 菜单里按 `+` 也到这(直接进加账号)。
//!
//! 和插件页一样是大小固定的大弹窗:左边一栏是登记的账号,分成 Claude Code、Codex 两段,一行「名字  登录着没有」
//! (问的是工具自己,见 accounts.rs 的 `login`);右边一栏是高亮那个账号的整页 —— 配置目录、登录的是谁、状态栏现在怎么接的、
//! 哪些会话在用、是不是哪个工具的默认账号、`n` 菜单里第几个、要敲的命令;目录不在、目录其实是另一家的在这说怎么办。
//! 窄屏(手机)上两栏上下叠。加账号的表单、删之前的确认也在右边那一栏,框不跟着变。
//!
//! ⏎ 登录(下半屏开终端跑工具自己的登录)· `+` 加 · `d` 删(先说清动什么、谁在用,y 才删;只去掉登记那一行,目录不动)·
//! ⇧↑ ⇧↓ 挪顺序(就是 `n` 菜单的顺序,记在 `[ui] menu`)· `s` 各账号的状态栏(setup.rs 的表)· `r` 重新问一遍登录。
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
                    self.hint = t!("账号名是空的", "the account name is empty").into();
                } else if !accounts::valid_name(&name) {
                    self.hint = t!("账号名只能用字母、数字、- 和 _", "an account name is letters, digits, - and _").into();
                } else if taken(self.agent, &name) {
                    self.hint = tr!("已经有这个账号:{name}", "that account already exists: {name}");
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
            let mut out = vec![Line::styled(t!("加账号 · 哪个工具的", "Add an account · for which tool"), bold)];
            let name_w = accounts::who(Agent::Claude).width();
            for a in [Agent::Claude, Agent::Codex] {
                let on = a == self.agent;
                let note = tr!("配置目录默认 {}", "config directory defaults to {}", accounts::default_dir(a, t!("<名字>", "<NAME>")));
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
            out.push(kit::keys_line(&[("⏎", t!("下一步", "next")), ("↑↓", t!("换", "switch")), ("Esc", t!("退回", "back"))], vec![], w));
            return out;
        }
        let spec = format!("{}:{}", accounts::fam(self.agent).unwrap_or("claude"), t!("名字", "NAME"));
        let (mut title, tip, enter) = match self.step {
            Step::Dir => (
                vec![Span::styled(tr!(" 账号 · {} · 配置目录", " account · {} · config directory", self.name), bold)],
                t!(
                    "这个账号的配置目录;还没有的话回车后建,再用它自己的命令登录",
                    "This account's config directory; if missing it's created after Enter, then log in with the tool's own command"
                )
                .to_string(),
                done,
            ),
            _ => (
                vec![Span::styled(t!(" 账号 · 名字", " account · name"), bold)],
                tr!(
                    "名字是你起的,起会话时写成「{spec}」;用字母、数字、- 和 _",
                    "A name of your choice; sessions use it as {spec}. Letters, digits, - and _"
                ),
                t!("下一步", "next"),
            ),
        };
        title.splice(0..0, [Span::styled(t!("加 ", "Add a "), bold), who_span(self.agent)]);
        let input = vec![Span::styled("> ", bold), Span::raw(tail(&self.text, w.saturating_sub(3))), Span::styled("▏", theme::accent())];
        let mut out = vec![Line::from(title), Line::from(input)];
        if self.hint.is_empty() {
            out.push(Line::styled(kit::clip(&tip, w).trim_end().to_string(), kit::dim()));
        } else {
            out.extend(fold(&self.hint, w).into_iter().map(|l| Line::styled(l, theme::warn())));
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("⏎", enter), ("Esc", t!("退回", "back"))], vec![], w));
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
    /// `n` 菜单里排第几(从 1 数,最上面的 shell 是 1);藏着的(`[ui] hide`)没有
    menu_at: Option<usize>,
    /// Claude 的、目录没毛病的:状态栏现在是什么
    bar: Option<String>,
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

/// 右边那一栏最多列几个会话的标题
const TITLES: usize = 5;
/// 加账号的表单最多几行(提示折成两行时)
const FORM_ROWS: usize = 7;

pub struct Page {
    env: Env,
    rows: Vec<Row>,
    sel: usize,
    uses: Vec<Use>,
    /// 问到的登录,按「工具:账号」记;没有 = 还在问
    logins: Arc<Mutex<HashMap<String, Login>>>,
    form: Option<Form>,
    asking: Option<Asking>,
}

impl Page {
    /// `adding`:一开就进加账号(`n` 菜单里按 `+`)
    pub fn real(adding: bool) -> Page {
        let env =
            Env { config: crate::config::path(), home: std::env::var("HOME").unwrap_or_default(), ask: accounts::login, background: true };
        Page::load(env, crate::setup::session_accounts(), adding)
    }

    /// `uses`:登记着的会话各用的(哪一家, 账号名, 标题)
    pub fn load(env: Env, uses: Vec<Use>, adding: bool) -> Page {
        let mut p = Page { env, rows: vec![], sel: 0, uses, logins: Arc::default(), form: adding.then(Form::new), asking: None };
        p.read();
        p
    }

    /// 照 config.toml 现在写的重列;高亮跟着原来那个账号
    fn read(&mut self) {
        let keep = self.rows.get(self.sel).map(|r| (r.a.agent, r.a.name.clone()));
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
                    menu_at: menu.iter().position(|t| t.tool == fam && t.account == a.name).map(|i| i + 1),
                    bar: wired.then(|| crate::setup::bar_said(Path::new(&abs))),
                    there,
                    foreign,
                    a,
                }
            })
            .collect();
        let at = keep.and_then(|(g, n)| self.rows.iter().position(|r| r.a.agent == g && r.a.name == n));
        self.sel = at.unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
        self.ask(false);
    }

    /// 两栏各多宽(里面一共 w 宽):左边照最长的名字,右边是剩下的
    fn split(&self, w: usize) -> (usize, usize) {
        let names = self.rows.iter().map(|r| r.a.name.width()).max().unwrap_or(0).min(14);
        let lw = (2 + names + 2 + 2 + 16).clamp(26, 38);
        (lw, w.saturating_sub(lw + DIVIDER.width()))
    }

    fn at(&mut self, a: Agent, name: &str) {
        if let Some(i) = self.rows.iter().position(|r| r.a.agent == a && r.a.name == name) {
            self.sel = i;
        }
    }

    /// 问还没问过的账号登录着没有;`again` = 都重问
    fn ask(&self, again: bool) {
        let mut known = self.logins.lock().unwrap_or_else(|e| e.into_inner());
        if again {
            known.clear();
        }
        let todo: Vec<(String, Agent, String)> = self
            .rows
            .iter()
            .filter(|r| r.askable() && !known.contains_key(&r.spec()))
            .map(|r| (r.spec(), r.a.agent, r.a.name.clone()))
            .collect();
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

    fn login_of(&self, r: &Row) -> Option<Login> {
        self.logins.lock().unwrap_or_else(|e| e.into_inner()).get(&r.spec()).cloned()
    }

    /// 这个账号现在怎么样:记号、一句话、颜色。左边列表只写这句话的头一段
    fn state(&self, r: &Row) -> (&'static str, String, Style) {
        let green = Style::new().fg(theme::pal().green);
        if let Some((other, _)) = &r.foreign {
            return ("⚠", tr!("是 {} 的目录", "a {} directory", accounts::who(*other)), theme::warn());
        }
        if !r.there {
            return ("⚠", t!("目录不在", "directory missing").into(), theme::warn());
        }
        match self.login_of(r) {
            None => ("…", t!("在问", "asking").into(), kit::dim()),
            Some(Login::In(who)) if who.is_empty() => ("●", t!("登录着", "logged in").into(), green),
            Some(Login::In(who)) => ("●", who, green),
            Some(Login::Out) => ("○", t!("没登录", "not logged in").into(), theme::warn()),
            Some(Login::Unknown(said)) => ("?", said, kit::dim()),
        }
    }

    /// 左边那一栏:两段(Claude Code、Codex),一行一个账号「名字  记号 头一段」。连同高亮在第几行
    fn list(&self, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let name_w = self.rows.iter().map(|r| r.a.name.width()).max().unwrap_or(0).min(14);
        let (mut out, mut at, mut last) = (vec![], None, None);
        for (i, r) in self.rows.iter().enumerate() {
            if last != Some(r.a.agent) {
                if last.is_some() {
                    out.push(Line::raw(""));
                }
                out.push(Line::from(who_span(r.a.agent)));
                last = Some(r.a.agent);
            }
            let on = i == self.sel;
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
        if self.rows.is_empty() {
            out.push(Line::styled(t!("还没登记账号", "no accounts yet"), kit::dim()));
        }
        (out, at)
    }

    /// 右边那一栏:在加账号就是那张表单,按过 d 是要删什么,不然是高亮那个账号的整页
    fn right(&self, w: usize) -> Vec<Line<'static>> {
        if let Some(f) = &self.form {
            return f.lines(w, t!("加上", "add"));
        }
        let Some(r) = self.rows.get(self.sel) else {
            let none = t!(
                "不登记也能用:就是 ~/.claude、~/.codex。还有别的配置目录(另一个订阅、另一个 key),按 + 登记成一个账号,起会话时写成「工具:名字」。",
                "It works without registering: that's ~/.claude, ~/.codex. For another config directory (another subscription, another key), + registers it as an account; sessions use it as TOOL:NAME."
            );
            return fold(none, w).into_iter().map(|l| Line::styled(l, kit::dim())).collect();
        };
        match self.asking {
            Some(Asking::Remove) => self.confirm(r, w),
            Some(Asking::Relogin) => self.relogin(r, w),
            None => self.detail(r, w),
        }
    }

    /// 高亮那个账号的整页:是哪个工具的、目录、登录的是谁、状态栏、谁在用、要敲的命令
    fn detail(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let labels = [
            t!("配置目录", "directory"),
            t!("登录", "login"),
            t!("状态栏", "status bar"),
            t!("会话", "sessions"),
            t!("默认", "default"),
            t!("n 菜单", "n menu"),
            t!("起会话", "start"),
            t!("重新登录", "log in again"),
            t!("去登录", "log in"),
        ];
        let lw = labels.iter().map(|l| l.width()).max().unwrap_or(0) + 2;
        // 一项:「名字  值」,值太长折到下面、和值对齐;lead 是值前面的记号(● ○ ⚠)
        let field = |out: &mut Vec<Line<'static>>, label: &str, lead: Option<(&str, Style)>, text: &str, st: Style| {
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
        };
        let (me, plain) = (accounts::who(r.a.agent), Style::new());
        let name = Span::styled(r.a.name.clone(), agent_style(r.a.agent).add_modifier(Modifier::BOLD));
        let mut out = vec![kit::spread(vec![name], vec![who_span(r.a.agent)], w), Line::raw("")];
        if let Some((other, why)) = &r.foreign {
            let theirs = accounts::who(*other);
            let said = [
                tr!(
                    "⚠ 这是 {theirs} 的配置目录({why}),登记成了 {me} 的账号。",
                    "⚠ This is a {theirs} config directory ({why}), registered as a {me} account."
                ),
                tr!(
                    "按 d 删掉这条登记(目录不动);要用它,按 + 登记成 {theirs} 的。",
                    "d removes the registration (the directory stays); to use it, + registers it for {theirs}."
                ),
            ];
            out.extend(said.iter().flat_map(|s| fold(s, w)).map(|l| Line::styled(l, theme::warn())));
            out.push(Line::raw(""));
        }
        let dir = if r.a.dir.is_empty() {
            t!("不换配置目录(只带环境变量 / 参数)", "same config directory (only env vars / arguments)")
        } else {
            &r.a.dir
        };
        field(&mut out, labels[0], None, dir, plain);
        if r.foreign.is_none() {
            let (mark, said, st) = self.state(r);
            let said = match (r.there, mark) {
                (false, _) => t!(
                    "配置目录不在(登记之后被删了,或者写错了):按 d 删掉这条登记,或者改 config.toml",
                    "the config directory is missing (removed since, or mistyped): d removes the registration, or edit config.toml"
                )
                .to_string(),
                (_, "…") => tr!("正在问 {me}", "asking {me}"),
                (_, "○") => t!("没登录:按 ⏎ 在下面开个终端登录", "not logged in: ⏎ opens a terminal below to log in").to_string(),
                (_, "?") => tr!("问不出登录着没有:{said}", "can't tell whether it's logged in: {said}"),
                _ => said,
            };
            field(&mut out, labels[1], Some((mark, st)), &said, if mark == "●" { plain } else { st });
        }
        if let Some(bar) = &r.bar {
            field(&mut out, labels[2], None, &tr!("{bar}(按 s 改)", "{bar} (s changes it)"), plain);
        }
        out.push(Line::raw(""));
        match r.used.len() {
            0 => field(&mut out, labels[3], None, t!("没有会话在用", "no sessions use it"), kit::dim()),
            n => {
                field(&mut out, labels[3], None, &tr!("{n} 个在用", "{n} using it"), plain);
                let room = w.saturating_sub(lw + 2);
                for title in r.used.iter().take(if n > TITLES { TITLES - 1 } else { TITLES }) {
                    out.push(Line::styled(format!("{}· {}", " ".repeat(lw), kit::clip(title, room).trim_end()), kit::dim()));
                }
                if n > TITLES {
                    let more = tr!("…还有 {} 个", "…and {} more", n + 1 - TITLES);
                    out.push(Line::styled(format!("{}· {more}", " ".repeat(lw)), kit::dim()));
                }
            }
        }
        match r.default_of.as_slice() {
            [] => field(&mut out, labels[4], None, t!("不是哪个工具的默认账号", "not a tool's default account"), kit::dim()),
            tools => {
                let said: Vec<String> = tools
                    .iter()
                    .map(|tool| tr!("aca new {tool} 不写账号时用它", "aca new {tool} uses it when no account is given"))
                    .collect();
                field(&mut out, labels[4], None, &said.join(t!(";", "; ")), plain);
            }
        }
        match r.menu_at {
            Some(i) => field(&mut out, labels[5], None, &tr!("列着,第 {i} 个", "listed, number {i}"), plain),
            None => field(&mut out, labels[5], None, t!("不列([ui] hide)", "not listed ([ui] hide)"), kit::dim()),
        }
        if r.foreign.is_none() {
            out.push(Line::raw(""));
            field(&mut out, labels[6], None, &tr!("aca new {} <目录>", "aca new {} <DIR>", r.spec()), kit::dim());
            let how = if r.a.agent == Agent::Codex { "login" } else { "auth login" };
            // 登录着的:⏎ 会先问一句(登录一开始原来那份就没了)
            let (label, said) = if self.logged_out(r) {
                (labels[8], tr!("⏎,或者 aca run {} -- {how}", "⏎, or aca run {} -- {how}", r.spec()))
            } else {
                (labels[7], tr!("⏎(会先问);命令 aca run {} -- {how}", "⏎ (asks first); command: aca run {} -- {how}", r.spec()))
            };
            field(&mut out, label, None, &said, kit::dim());
        }
        out
    }

    /// 按过 d:要删的是什么、会动什么、谁在用
    fn confirm(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let me = accounts::who(r.a.agent);
        let mut said = vec![
            t!(
                "只去掉 config.toml 里这一行,删之前的存成 config.toml.bak-aca。",
                "Only this line in config.toml goes; the file as it was is kept as config.toml.bak-aca."
            )
            .to_string(),
        ];
        if !r.a.dir.is_empty() {
            said.push(tr!("目录 {} 和里面的登录都不动。", "The directory {} and the login in it stay.", r.a.dir));
        }
        if !r.used.is_empty() {
            said.push(tr!(
                "有 {} 个会话在用它:删了以后它们再启动会说没有这个账号。",
                "{} sessions use it: once removed, restarting them fails with no such account.",
                r.used.len()
            ));
        }
        for tool in &r.default_of {
            said.push(tr!(
                "aca new {tool} 不写账号时用的是它([tools.{tool}] account):删了那一行要改。",
                "It's the default for aca new {tool} ([tools.{tool}] account): that line needs changing."
            ));
        }
        let title = tr!("删掉 {me} 账号 {}?", "Remove the {me} account {}?", r.a.name);
        let mut out = vec![Line::styled(title, theme::warn().add_modifier(Modifier::BOLD)), Line::raw("")];
        out.extend(said.iter().flat_map(|s| fold(s, w)).map(|l| Line::styled(l, Style::new())));
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("y", t!("删掉", "remove")), (t!("别的键", "any other key"), t!("不删", "keep"))], vec![], w));
        out
    }

    /// 工具说了它没登录(目录不在的也算:里面没有登录可丢)。登录着的、还没问到的、问不出的都不算
    fn logged_out(&self, r: &Row) -> bool {
        !r.there || self.login_of(r) == Some(Login::Out)
    }

    /// 在登录着的账号上按了 ⏎:重新登录会丢掉什么,y 才去
    fn relogin(&self, r: &Row, w: usize) -> Vec<Line<'static>> {
        let me = accounts::who(r.a.agent);
        let now = match self.login_of(r) {
            Some(Login::In(who)) if !who.is_empty() => tr!("它现在登录着:{who}。", "It is logged in now: {who}."),
            Some(Login::In(_)) => t!("它现在登录着。", "It is logged in now.").to_string(),
            Some(Login::Unknown(said)) => {
                tr!("问不出它登录着没有({said}),当它登录着。", "Can't tell whether it's logged in ({said}); assuming it is.")
            }
            _ => tr!("还没问到它登录着没有,当它登录着。", "Not known yet whether it's logged in; assuming it is."),
        };
        let lost = if r.a.agent == Agent::Codex {
            t!(
                "Codex 的登录一开始就把现在这份删掉:浏览器里没登完、中途关了,这个账号就是没登录的,原来的 key / 订阅登录要重新弄。",
                "Codex's login deletes the current one the moment it starts: if you don't finish in the browser, this account is left logged out and the old key / subscription login has to be set up again."
            )
        } else {
            t!("重新登录会换掉现在这份登录。", "Logging in again replaces the current login.")
        };
        let why = t!(
            "要换登录的人、换登录方式才需要;只想看它登录着没有,右边已经写着了。",
            "Only needed to change who or how it logs in; whether it's logged in is already shown on the right."
        );
        let title = tr!("重新登录 {me} 账号 {}?", "Log the {me} account {} in again?", r.a.name);
        let mut out = vec![Line::styled(title, theme::warn().add_modifier(Modifier::BOLD)), Line::raw("")];
        out.extend(fold(&now, w).into_iter().map(Line::raw));
        out.extend(fold(lost, w).into_iter().map(|l| Line::styled(l, theme::warn())));
        out.extend(fold(why, w).into_iter().map(|l| Line::styled(l, kit::dim())));
        out.push(Line::raw(""));
        let keys = [("y", t!("重新登录", "log in again")), (t!("别的键", "any other key"), t!("不登录", "leave it"))];
        out.push(kit::keys_line(&keys, vec![], w));
        out
    }

    /// 高亮那一行上的 ⏎:没登录的直接去登录;登录着的先问(见 [`Asking::Relogin`])
    fn login(&mut self, fx: &mut Fx) {
        let Some(r) = self.rows.get(self.sel) else { return };
        if let Some((other, _)) = &r.foreign {
            return fx.say(tr!(
                "这是 {} 的目录,不在这登录:按 d 删掉这条登记",
                "this is a {} directory, not logging in here: d removes the registration",
                accounts::who(*other)
            ));
        }
        if self.logged_out(r) { self.open_login(fx) } else { self.asking = Some(Asking::Relogin) }
    }

    /// 在下半屏开个终端,带着高亮那个账号跑工具自己的登录
    fn open_login(&self, fx: &mut Fx) {
        let Some(r) = self.rows.get(self.sel) else { return };
        fx.term = Some(self.term(&r.a.name, r.a.agent, accounts::relogin_cmd(r.a.agent, &r.a.name)));
    }

    fn term(&self, name: &str, a: Agent, run: String) -> TermReq {
        TermReq {
            key: format!("account:{name}"),
            family: String::new(),
            title: tr!("登录 {} · {name}", "log in {} · {name}", accounts::who(a)),
            w: crate::workdir::Where { dir: self.env.home.clone(), ..Default::default() },
            env: vec![],
            run: Some(run),
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
                // Claude 的额度靠状态栏:接不接在 s 里挑,不替人接
                let bar = if a == Agent::Claude { t!(",状态栏接不接:按 s", "; status bar wiring: s") } else { "" };
                fx.say(tr!("已加 {} 账号 {name}{bar}", "added the {} account {name}{bar}", accounts::who(a)));
                if added.created {
                    fx.term = Some(self.term(&name, a, accounts::login_cmd(a, &name)));
                }
            }
        }
    }

    fn remove(&mut self, fx: &mut Fx) {
        let Some(r) = self.rows.get(self.sel) else { return };
        let (a, name) = (r.a.agent, r.a.name.clone());
        match accounts::remove_in(&self.env.config, &self.env.home, a, &name) {
            Ok(gone) => {
                let kept = if gone.dir.is_empty() {
                    String::new()
                } else {
                    tr!("(目录 {} 没动)", " (the directory {} is untouched)", gone.dir)
                };
                fx.say(tr!("删掉了 {} 账号 {name} 的登记{kept}", "removed the {} account {name}{kept}", accounts::who(a)));
                self.read();
            }
            Err(e) => fx.say(e),
        }
    }

    fn shift(&mut self, up: bool, fx: &mut Fx) {
        let Some(r) = self.rows.get(self.sel) else { return };
        let (a, name) = (r.a.agent, r.a.name.clone());
        match accounts::shift_in(&self.env.config, &self.env.home, a, &name, up) {
            Ok(true) => {
                self.read();
                self.at(a, &name);
                fx.say(t!("顺序记进了 [ui] menu(n 菜单同一个顺序)", "order saved under [ui] menu (the n menu follows it)"));
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
    /// 右边按最长的那个账号算(连同删它之前的确认),所以挪高亮不变,加了 / 删了账号才变
    fn fill_rows(&self) -> Option<u16> {
        let (lw, rw) = self.split(self.width() as usize - 6);
        let ask = |r: &Row| self.confirm(r, rw).len().max(self.relogin(r, rw).len());
        let right = self.rows.iter().map(|r| self.detail(r, rw).len().max(ask(r))).max().unwrap_or(0);
        Some((2 + self.list(lw).0.len().max(right).max(FORM_ROWS) + 2).max(22) as u16)
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        self.lines_in(w, 32)
    }

    /// 标题、两栏(窄屏上下叠)、细线、键;正好 h 行
    fn lines_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let busy = self.form.is_some() || self.asking.is_some();
        let esc = kit::esc_hint(if busy { t!("退回", "back") } else { t!("关闭", "close") });
        let mut out = vec![kit::modal_title(t!("账号", "Accounts"), theme::accent(), esc, w), Line::raw("")];
        // 放不下时后面的键先不写:最要紧的排前面。在填表单、在问删不删时键写在右边那一栏里
        let out_now = self.rows.get(self.sel).is_none_or(|r| r.foreign.is_some() || self.logged_out(r));
        let keys = [
            ("⏎", if out_now { t!("登录", "log in") } else { t!("重新登录", "log in again") }),
            ("+", t!("加账号", "add")),
            ("d", t!("删", "remove")),
            ("⇧↑↓", t!("挪", "move")),
            ("s", t!("状态栏", "status bar")),
            ("r", t!("重查", "recheck")),
        ];
        let keys = match (busy, self.rows.is_empty()) {
            (true, _) => &keys[..0],
            (_, true) => &keys[1..2],
            _ => &keys[..],
        };
        let foot = [Line::styled("─".repeat(w), theme::faint()), kit::keys_line(keys, vec![], w)];
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
        if let Some(what) = self.asking.take() {
            match (what, k.code) {
                (Asking::Remove, KeyCode::Char('y')) => self.remove(fx),
                (Asking::Relogin, KeyCode::Char('y')) => self.open_login(fx),
                _ => {}
            }
            return Flow::Stay;
        }
        let n = self.rows.len();
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
            KeyCode::Char('d') | KeyCode::Delete if n > 0 => self.asking = Some(Asking::Remove),
            // 各账号的状态栏接不接(`aca setup` 开的也是这张表;加账号在这一页,表里不给加)
            KeyCode::Char('s') => fx.open = Some(Box::new(crate::setup::Page::real(false))),
            KeyCode::Char('r') => {
                self.read();
                self.ask(true);
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
        self.form.as_ref().is_some_and(Form::typing)
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
    fn the_list_is_on_the_left_and_the_highlighted_account_in_full_on_the_right() {
        let (root, mut p) = page("list");
        let text = shown(&p, 110);
        // 左边:两段各有表头(单独一行写哪个工具),账号在自己那一段里,各带登录着没有
        let at = |want: &str| text.iter().position(|l| l.starts_with(want)).unwrap_or_else(|| panic!("没有「{want}」开头的行:{text:#?}"));
        assert!(at("Claude Code") < at("  sub ") && at("  gone ") < at("Codex") && at("Codex") < at("  cx "), "{text:#?}");
        assert!(row(&text, "sub").contains("● max") && !row(&text, "sub").contains("me@example.com"), "{text:#?}");
        assert!(row(&text, "firm").contains("○ 没登录") && row(&text, "cx").contains("● ChatGPT"), "{text:#?}");
        // 另一家的目录、不在的目录:标出来,不去问它登录(问了会在里面写东西)
        assert!(row(&text, "codex").contains("⚠ 是 Codex 的目录") && row(&text, "gone").contains("⚠ 目录不在"), "{text:#?}");
        assert!(!p.logins.lock().unwrap().keys().any(|k| k == "claude:codex" || k == "claude:gone"));
        // 右边:高亮那个(登记错的)—— 是谁的目录、怎么办;不给登录的命令
        assert_eq!(names(&p)[p.sel], "codex");
        shows(&text, &["⚠ 这是 Codex 的配置目录(里面有 auth.json、config.toml…)", "按 d 删掉这条登记(目录不动)", "配置目录  ~/.codex"]);
        assert!(!text.iter().any(|l| l.contains("重新登录") || l.contains("去登录") || l.contains("起会话")), "{text:#?}");
        press(&mut p, &[KeyCode::Down, KeyCode::Down]);
        shows(&shown(&p, 110), &["登录      ⚠ 配置目录不在"]);
        // 登录着的:是谁、状态栏、哪些会话在用、是不是默认、n 菜单里第几个、要敲的命令
        press(&mut p, &[KeyCode::Down]);
        shows(
            &shown(&p, 110),
            &[
                "配置目录  ~/.claude-sub",
                "登录      ● max · me@example.com",
                "状态栏    没有状态栏(按 s 改)",
                "会话      2 个在用",
                "          · 写账号页",
                "          · 查额度",
                "默认      aca new claude 不写账号时用它",
                "n 菜单    列着,第 4 个",
                "起会话    aca new claude:sub <目录>",
                "重新登录  ⏎(会先问);命令 aca run claude:sub -- auth login",
            ],
        );
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        shows(&shown(&p, 110), &["登录      ○ 没登录:按 ⏎", "会话      没有会话在用", "n 菜单    不列([ui] hide)"]);
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        shows(&shown(&p, 110), &["cx", "登录      ● ChatGPT", "重新登录  ⏎(会先问);命令 aca run codex:cx -- login"]);
        // Codex 的额度不靠状态栏:没有那一项
        assert!(!shown(&p, 110).iter().any(|l| l.contains("状态栏    ")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_long_list_of_sessions_is_cut_short() {
        let (root, env) = home("many", "[accounts.claude]\nsub = \"~/.claude-sub\"\n");
        dir(&root, ".claude-sub", &[]);
        let uses = (1..=9).map(|i| (Agent::Claude, "sub".to_string(), format!("会话 {i}"))).collect();
        let text = shown(&Page::load(env, uses, false), 110);
        shows(&text, &["会话      9 个在用", "· 会话 4", "· …还有 5 个"]);
        assert!(!text.iter().any(|l| l.contains("会话 5")), "{text:#?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn d_says_what_goes_and_only_y_removes_the_registration() {
        let (root, mut p) = page("rm");
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        assert_eq!(names(&p), ["codex", "firm", "gone", "sub", "cx"]);
        press(&mut p, &[KeyCode::Char('d')]);
        shows(
            &shown(&p, 110),
            &["删掉 Claude Code 账号 codex?", "只去掉 config.toml 里这一行", "目录 ~/.codex 和里面的登录都不动。", "y 删掉 • 别的键 不删"],
        );
        // 别的键:不删,键也不往下走(j 不挪高亮)
        press(&mut p, &[KeyCode::Char('j')]);
        assert_eq!((names(&p)[p.sel], std::fs::read_to_string(&p.env.config).unwrap()), ("codex", before.clone()));
        let fx = press(&mut p, &[KeyCode::Char('d'), KeyCode::Char('y')]);
        assert_eq!(fx.flash.as_deref(), Some("删掉了 Claude Code 账号 codex 的登记(目录 ~/.codex 没动)"));
        assert_eq!(names(&p), ["firm", "gone", "sub", "cx"]);
        // 只少了那一行;目录和里面的东西都在;删之前的那份留着
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before.replace("codex = \"~/.codex\"\n", ""));
        assert!(root.join(".codex/auth.json").exists() && root.join(".codex/.claude.json").exists());
        assert_eq!(std::fs::read_to_string(root.join("aca/config.toml.bak-aca")).unwrap(), before);
        // 有会话在用的、是默认账号的:删之前说清楚
        press(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Char('d')]);
        shows(
            &shown(&p, 110),
            &["删掉 Claude Code 账号 sub?", "有 2 个会话在用它", "aca new claude 不写账号时用的是它([tools.claude] account)"],
        );
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
        assert_eq!(names(&p)[p.sel], "sub");
        // 已经在这一家的最后:不动,什么都不写
        let before = std::fs::read_to_string(&p.env.config).unwrap();
        assert!(shift(&mut p, KeyCode::Down).flash.is_none());
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before);
        let fx = shift(&mut p, KeyCode::Up);
        assert_eq!(fx.flash.as_deref(), Some("顺序记进了 [ui] menu(n 菜单同一个顺序)"));
        press(&mut p, &[KeyCode::Char('K'), KeyCode::Char('K')]);
        // 高亮跟着它;Codex 的不受影响;右边写的 n 菜单里第几个也跟着变(shell 是第 1 个)
        assert_eq!((names(&p), p.sel), (vec!["sub", "codex", "firm", "gone", "cx"], 0));
        shows(&shown(&p, 110), &["n 菜单    列着,第 2 个"]);
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
        shows(
            &text,
            &["加账号 · 哪个工具的", "› Claude Code  配置目录默认 ~/.claude-<名字>", "  Codex        配置目录默认 ~/.codex-<名字>"],
        );
        assert!(row(&text, "sub").contains("● max"), "{text:#?}");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.typing());
        shows(&shown(&p, 110), &["加 Claude Code 账号 · 名字", "起会话时写成「claude:名字」"]);
        // 名字:空的、写得不对的、这一家里重了的,当场说
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号名是空的");
        type_in(&mut p, "a b");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号名只能用字母、数字、- 和 _");
        retype(&mut p, "sub");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "已经有这个账号:sub");
        // 把 Codex 默认的目录登记成 Claude 的账号:不给,说它是谁的
        retype(&mut p, "mine");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().text, "~/.claude-mine");
        std::fs::remove_dir_all(root.join(".codex")).unwrap();
        retype(&mut p, "~/.codex");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "~/.codex 是 Codex 的配置目录(Codex 默认用它),不能登记成 Claude Code 的账号");
        dir(&root, ".elsewhere", &["auth.json", "config.toml", "version.json"]);
        retype(&mut p, "~/.elsewhere");
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.form.as_ref().unwrap().hint.starts_with("~/.elsewhere 是 Codex 的配置目录(里面有 auth.json、config.toml…)"));
        assert_eq!(std::fs::read_to_string(&p.env.config).unwrap(), before);
        // Esc 一步步退回第一步,换成 Codex:同一个目录就能加了;目录本来就在,不用登录
        press(&mut p, &[KeyCode::Esc, KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        shows(&shown(&p, 110), &["加 Codex 账号 · 名字"]);
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.elsewhere");
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert!(p.form.is_none() && fx.term.is_none());
        assert_eq!(fx.flash.as_deref(), Some("已加 Codex 账号 mine"));
        assert_eq!((names(&p), names(&p)[p.sel]), (vec!["firm", "gone", "sub", "cx", "mine"], "mine"));
        shows(&shown(&p, 110), &["配置目录  ~/.elsewhere", "登录      ● ChatGPT", "起会话    aca new codex:mine <目录>"]);
        // 新建的目录:加完在下半屏开终端,带着这个账号跑它自己的登录
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "work");
        let fx = press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        assert_eq!(fx.flash.as_deref(), Some("已加 Claude Code 账号 work,状态栏接不接:按 s"));
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
        assert!(fx.term.is_none() && fx.flash.as_deref().is_some_and(|m| m.contains("这是 Codex 的目录")));
        let fx = press(&mut p, &[KeyCode::Down, KeyCode::Enter]);
        let term = fx.term.unwrap();
        assert_eq!(term.title, "登录 Claude Code · firm");
        assert!(term.run.as_deref().is_some_and(|c| c.ends_with("' run claude:firm -- auth login")) && term.env.is_empty());
        // 登录着的(往上绕到最后一行,Codex 的):⏎ 不直接去登录 —— Codex 的登录一开始就把原来那份删了,先问
        let fx = press(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Enter]);
        assert!(fx.term.is_none() && p.asking == Some(Asking::Relogin));
        let text = shown(&p, 110);
        shows(
            &text,
            &["重新登录 Codex 账号 cx?", "它现在登录着:ChatGPT。", "Codex 的登录一开始就把现在这份删掉", "y 重新登录 • 别的键 不登录"],
        );
        assert!(text.last().unwrap().is_empty(), "在问的时候底下不写别的键:{text:#?}");
        // 别的键:不登录,回到那一页,键也不往下走
        let fx = press(&mut p, &[KeyCode::Enter]);
        assert!(fx.term.is_none() && p.asking.is_none() && names(&p)[p.sel] == "cx");
        shows(&shown(&p, 110), &["重新登录  ⏎(会先问);命令 aca run codex:cx -- login"]);
        assert!(shown(&p, 110).last().unwrap().starts_with("⏎ 重新登录 • + 加账号"));
        let fx = press(&mut p, &[KeyCode::Enter, KeyCode::Char('y')]);
        assert!(fx.term.unwrap().run.as_deref().is_some_and(|c| c.ends_with("' run codex:cx -- login")));
        // Claude 登录着的也先问;还没问到的、问不出的当它登录着
        let fx = press(&mut p, &[KeyCode::Up, KeyCode::Enter]);
        assert!(fx.term.is_none() && names(&p)[p.sel] == "sub");
        shows(&shown(&p, 110), &["重新登录 Claude Code 账号 sub?", "它现在登录着:max · me@example.com。", "重新登录会换掉现在这份登录。"]);
        let fx = press(&mut p, &[KeyCode::Char('y')]);
        assert!(fx.term.unwrap().run.as_deref().is_some_and(|c| c.ends_with("' run claude:sub -- auth login")));
        p.logins.lock().unwrap().remove("claude:sub");
        assert!(press(&mut p, &[KeyCode::Enter]).term.is_none() && p.asking == Some(Asking::Relogin));
        shows(&shown(&p, 110), &["还没问到它登录着没有,当它登录着。"]);
        press(&mut p, &[KeyCode::Esc]);
        // 没登录的那一行写的是「登录」,⏎ 直接去
        press(&mut p, &[KeyCode::Up, KeyCode::Up]);
        assert_eq!(names(&p)[p.sel], "firm");
        shows(&shown(&p, 110), &["去登录    ⏎,或者 aca run claude:firm -- auth login"]);
        assert!(shown(&p, 110).last().unwrap().starts_with("⏎ 登录 • + 加账号"));
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
        for keys in [&[KeyCode::Down][..], &[KeyCode::Down, KeyCode::Down, KeyCode::Down][..], &[KeyCode::Char('d')][..]] {
            press(&mut p, keys);
            assert_eq!(p.fill_rows(), tall);
        }
        // 停在 cx(登录着)上按 ⏎:在问要不要重新登录,框还是那么高
        press(&mut p, &[KeyCode::Esc, KeyCode::Enter]);
        assert_eq!((names(&p)[p.sel], p.asking, p.fill_rows()), ("cx", Some(Asking::Relogin), tall));
        press(&mut p, &[KeyCode::Esc, KeyCode::Down]);
        assert_eq!((names(&p)[p.sel], tall), ("codex", Some(22)));
        // 宽的:最下面是细线和键;在填表单时键写在右边那一栏里,底下那行空着
        let wide = shown(&p, 116);
        assert_eq!(wide[wide.len() - 1], "⏎ 登录 • + 加账号 • d 删 • ⇧↑↓ 挪 • s 状态栏 • r 重查");
        assert!(wide[wide.len() - 2].starts_with("────"), "{wide:#?}");
        press(&mut p, &[KeyCode::Char('+')]);
        let form = shown(&p, 116);
        assert!(form.last().unwrap().is_empty() && form.iter().any(|l| l.ends_with("⏎ 下一步 • ↑↓ 换 • Esc 退回")), "{form:#?}");
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
        shows(&text, &["还没登记账号", "不登记也能用:就是 ~/.claude、~/.codex。"]);
        assert_eq!(text.last().unwrap(), "+ 加账号");
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&root2).unwrap();
    }
}
