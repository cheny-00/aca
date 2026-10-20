//! `:` 插件菜单:开着的插件露给界面的命令(清单的 `[[commands]]`,界面上就叫命令;给 agent 的叫「给 agent 的工具」)、
//! 窗格、视图(见 `aca_core::plugin::menu`),按高亮的那行挑在这里有意义的。诊断用的(试一下、测连通)不进这里,
//! 清单里只露给 cli。
//!
//! - ↑↓ 挑,⏎ 跑;清单 `[[keys]]` 给它绑了键的,直接按那个键;`/` 打字筛
//! - 能力有参数的先弹一张表单(`Form`);跑完输出短的写在底栏,长的放进全屏阅读页
//! - 视图:输出放进全屏阅读页,`r` 重跑。列表视图(`rows = true`)开成一页 [`List`]:一行一条,
//!   停在哪行按清单 `[[views.keys]]` 的键(复制一个字段、带着这一行跑这个插件的一个能力),跑完写在下面、列表重读
//! - 窗格:tmux 弹窗 / 分屏 / 新窗口(界面不在 tmux 里就让出终端,在前台跑)
//!
//! 能力、视图都在后台跑(run 循环收 `Fx::plugin`,一个请求一个线程),不占界面那个一次只做一件的后台活。

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::plugin::{self, ItemKind, MenuItem};
use aca_core::{t, tr};

use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

/// 一页最多列这么多个,再多跟着高亮滚
const SHOWN: usize = 14;
/// 一个参数最多输这么长
const INPUT_MAX: usize = 2000;

/// 冲着什么跑
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    /// session / group / global
    pub on: &'static str,
    /// 会话 id(登记的会话);分组、别处是空的
    pub session: String,
    /// 菜单标题里写的:会话标题 / 分组名
    pub label: String,
}

/// 要跑的一个
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Req {
    pub item: MenuItem,
    pub session: String,
    pub args: Vec<(String, String)>,
    /// 列表视图的键跑的:跑完结果交给那个列表,再重读它
    pub then: Option<Box<Req>>,
    /// 列表重读:只换开着的那一页,它已经关了就算了(不再开一页)
    pub refresh: bool,
}

impl Req {
    /// 底栏、阅读页标题里叫它
    pub fn title(&self) -> String {
        format!("{} · {}", self.item.plugin_name, self.item.title)
    }
}

/// 冲着这个会话的上下文(目录、tmux 里的号和窗格);没有会话就是空的
pub fn context(session: &str) -> plugin::Context {
    let mut ctx = plugin::Context { session: session.to_string(), source: "menu".into(), ..Default::default() };
    if session.is_empty() {
        return ctx;
    }
    if let Ok(s) = aca_core::store::Store::open().and_then(|st| st.get(session)) {
        (ctx.dir, ctx.parent) = (s.dir, s.parent);
    }
    if let Some(p) = aca_core::tmux::find(session) {
        (ctx.tmux, ctx.pane) = (p.sid, p.pane);
    }
    ctx
}

/// 跑一个:能力、视图收下输出;窗格开出来(弹窗关了才回来)。后台线程里调;窗格在界面不在 tmux 里时由让出终端那里调
pub fn run(req: &Req) -> (bool, String) {
    let ctx = context(&req.session);
    match req.item.kind {
        ItemKind::Pane => match plugin::open_pane(&req.item.plugin, &req.item.id, None, None, &ctx) {
            Ok(0) => (true, String::new()),
            Ok(code) => (false, tr!("窗格退出码 {code}", "the pane exited with {code}")),
            Err(e) => (false, e),
        },
        _ => plugin::capture(&req.item.qualified(), &ctx, &req.args).unwrap_or_else(|e| (false, e)),
    }
}

/// 跑起来:窗格在界面不在 tmux 里时要让出终端,别的放后台
fn start(req: Req, fx: &mut Fx) {
    if req.item.kind == ItemKind::Pane && std::env::var_os("TMUX").is_none() {
        fx.external = Some(crate::ui::External::Plugin(req));
        return;
    }
    fx.say(tr!("正在跑 {}…", "running {}…", req.title()));
    fx.plugin = Some(req);
}

fn kind_word(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Command => "",
        ItemKind::Pane => t!("窗格", "pane"),
        ItemKind::View => t!("视图", "view"),
    }
}

/// `:` 那一层
pub struct Menu {
    items: Vec<MenuItem>,
    target: Target,
    sel: usize,
    /// `/` 之后在输的筛选词
    filter: Option<String>,
}

impl Menu {
    /// `all` = 开着的插件给菜单的全部;只留在这种行上有意义的
    pub fn new(all: Vec<MenuItem>, target: Target) -> Menu {
        let items = all.into_iter().filter(|i| i.fits(target.on)).collect();
        Menu { items, target, sel: 0, filter: None }
    }

    /// 筛过的(下标)
    fn shown(&self) -> Vec<usize> {
        let f = self.filter.as_deref().unwrap_or("").to_lowercase();
        (0..self.items.len())
            .filter(|&i| {
                let it = &self.items[i];
                f.is_empty() || [&it.title, &it.plugin_name, &it.id, &it.plugin].iter().any(|s| s.to_lowercase().contains(&f))
            })
            .collect()
    }

    fn pick(&self, i: usize, fx: &mut Fx) -> Flow {
        let it = self.items[i].clone();
        if it.kind == ItemKind::Command && !it.args.is_empty() {
            fx.open = Some(Box::new(Form::new(it, self.target.clone())));
        } else {
            start(Req { item: it, session: self.target.session.clone(), ..Default::default() }, fx);
        }
        Flow::Close
    }

    fn row(&self, it: &MenuItem, w: usize) -> Line<'static> {
        let key = it.key.map_or("  ".to_string(), |k| format!("{k} "));
        let kind = kind_word(it.kind);
        let right = if kind.is_empty() { it.plugin_name.clone() } else { format!("{kind} · {}", it.plugin_name) };
        let right_w = right.width();
        let dots = if it.args.is_empty() { "" } else { "…" };
        let title = kit::clip(&format!("{}{dots}", it.title), w.saturating_sub(right_w + 6)).trim_end().to_string();
        kit::spread(
            vec![Span::raw(" "), Span::styled(key, kit::key_style()), Span::styled(title, Style::new().add_modifier(Modifier::BOLD))],
            vec![Span::styled(right, kit::dim())],
            w,
        )
    }
}

impl Overlay for Menu {
    fn width(&self) -> u16 {
        72
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let wrap = |text: &str, st: Style| crate::ui::wrap_width(text, w).into_iter().map(move |l| Line::styled(l, st)).collect::<Vec<_>>();
        let on = match self.target.on {
            "session" => tr!("插件菜单 · 对「{}」", "Plugin menu · for “{}”", self.target.label),
            "group" => tr!("插件菜单 · 对分组 {}", "Plugin menu · for group {}", self.target.label),
            _ => t!("插件菜单", "Plugin menu").to_string(),
        };
        let mut out = vec![kit::modal_title(&on, theme::accent(), kit::esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let shown = self.shown();
        if self.items.is_empty() {
            out.extend(wrap(t!("还没有插件命令 · P 装插件", "No plugin commands yet · P installs plugins"), kit::dim()));
        } else if shown.is_empty() {
            out.extend(wrap(t!("没有对得上的", "nothing matches"), kit::dim()));
        }
        let at = shown.iter().position(|&i| i == self.sel).unwrap_or(0);
        let from = at.saturating_sub(SHOWN / 2).min(shown.len().saturating_sub(SHOWN));
        for &i in shown.iter().skip(from).take(SHOWN) {
            out.push(kit::picked(self.row(&self.items[i], w), w, i == self.sel));
        }
        out.push(Line::raw(""));
        if let Some(f) = &self.filter {
            out.push(Line::from(vec![Span::styled(t!("筛 > ", "filter > "), theme::accent()), Span::raw(f.clone()), Span::raw("▏")]));
            out.push(kit::keys_line(&[("⏎", t!("跑", "run")), ("Esc", t!("不筛了", "stop filtering"))], vec![], w));
        } else {
            out.push(kit::keys_line(&[("↑↓", t!("挑", "pick")), ("⏎", t!("跑", "run")), ("/", t!("筛", "filter"))], vec![], w));
        }
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let shown = self.shown();
        let at = shown.iter().position(|&i| i == self.sel);
        let step = |d: isize| -> Option<usize> {
            let n = shown.len() as isize;
            (n > 0).then(|| shown[((at.map_or(-1, |a| a as isize) + d).rem_euclid(n)) as usize])
        };
        if let Some(f) = &mut self.filter {
            match k.code {
                KeyCode::Esc => self.filter = None,
                KeyCode::Enter => return at.map_or(Flow::Stay, |_| self.pick(self.sel, fx)),
                KeyCode::Up => self.sel = step(-1).unwrap_or(0),
                KeyCode::Down => self.sel = step(1).unwrap_or(0),
                KeyCode::Backspace => {
                    f.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => f.push(c),
                _ => {}
            }
            // 筛完高亮落在第一个对得上的上
            let shown = self.shown();
            if !shown.contains(&self.sel) {
                self.sel = shown.first().copied().unwrap_or(0);
            }
            return Flow::Stay;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') if !self.items.iter().any(|i| i.key == Some('q')) => return Flow::Close,
            KeyCode::Esc => return Flow::Close,
            KeyCode::Enter => return at.map_or(Flow::Stay, |_| self.pick(self.sel, fx)),
            KeyCode::Up => self.sel = step(-1).unwrap_or(0),
            KeyCode::Down => self.sel = step(1).unwrap_or(0),
            KeyCode::Char('/') => self.filter = Some(String::new()),
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(i) = self.items.iter().position(|i| i.key == Some(c)) {
                    return self.pick(i, fx);
                }
                match c {
                    'j' => self.sel = step(1).unwrap_or(0),
                    'k' => self.sel = step(-1).unwrap_or(0),
                    _ => {}
                }
            }
            _ => {}
        }
        Flow::Stay
    }

    fn typing(&self) -> bool {
        self.filter.is_some()
    }
}

/// 有参数的能力:一个参数一格,填完跑
pub struct Form {
    item: MenuItem,
    target: Target,
    vals: Vec<String>,
    at: usize,
    err: Option<String>,
    /// 从列表视图的键来的:跑完交回那个列表
    then: Option<Box<Req>>,
}

impl Form {
    pub fn new(item: MenuItem, target: Target) -> Form {
        let vals = item.args.iter().map(|(_, ty, opt)| if ty == "bool" && !opt { "false".into() } else { String::new() }).collect();
        Form { item, target, vals, at: 0, err: None, then: None }
    }

    /// 列表视图的键开的:这一行已经给了的先填上,高亮落在第一个空着的;跑完交回列表
    fn from_list(item: MenuItem, session: &str, given: &[(String, String)], then: Req) -> Form {
        let mut f = Form::new(item, Target { on: "session", session: session.to_string(), label: String::new() });
        for (i, (name, ..)) in f.item.args.iter().enumerate() {
            if let Some((_, v)) = given.iter().find(|(n, _)| n == name) {
                f.vals[i] = v.clone();
            }
        }
        f.at = f.vals.iter().position(String::is_empty).unwrap_or(0);
        f.then = Some(Box::new(then));
        f
    }

    /// 照类型查一遍;不填的可选参数不给
    fn given(&self) -> Result<Vec<(String, String)>, String> {
        let mut out = vec![];
        for ((name, ty, opt), v) in self.item.args.iter().zip(&self.vals) {
            let v = v.trim();
            if v.is_empty() {
                if !opt {
                    return Err(tr!("{name} 要填", "{name} is required"));
                }
                continue;
            }
            if ty == "int" && v.parse::<i64>().is_err() {
                return Err(tr!("{name} 要是整数", "{name} takes an integer"));
            }
            out.push((name.clone(), v.to_string()));
        }
        Ok(out)
    }

    fn is_bool(&self, i: usize) -> bool {
        self.item.args.get(i).is_some_and(|(_, ty, _)| ty == "bool")
    }
}

impl Overlay for Form {
    fn width(&self) -> u16 {
        72
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let title = format!("{} · {}", self.item.plugin_name, self.item.title);
        let mut out = vec![kit::modal_title(&title, theme::accent(), kit::esc_hint(t!("不跑了", "cancel")), w), Line::raw("")];
        for (i, ((name, ty, opt), v)) in self.item.args.iter().zip(&self.vals).enumerate() {
            let on = i == self.at;
            // 类型写人话(字符串不写),可以不填的标出来
            let kind = match ty.as_str() {
                "int" => t!("整数", "integer"),
                "path" => t!("路径", "path"),
                "bool" => t!("空格 开/关", "space: on/off"),
                _ => "",
            };
            let hint: Vec<&str> = [kind, if *opt { t!("可不填", "optional") } else { "" }].into_iter().filter(|x| !x.is_empty()).collect();
            let hint = if hint.is_empty() { String::new() } else { format!("({})", hint.join(t!(",", ", "))) };
            out.push(Line::from(vec![
                Span::styled(format!(" {name} "), if on { theme::accent() } else { Style::new().add_modifier(Modifier::BOLD) }),
                Span::styled(hint, kit::dim()),
            ]));
            let shown = match (ty.as_str(), v.as_str()) {
                ("bool", "") => t!("不给", "not given").to_string(),
                ("bool", "true") => t!("开", "on").to_string(),
                ("bool", "false") => t!("关", "off").to_string(),
                _ => v.clone(),
            };
            let cursor = if on && ty != "bool" { "▏" } else { "" };
            let line = Line::from(vec![Span::raw("   "), Span::raw(shown), Span::raw(cursor)]);
            out.push(kit::picked(line, w, on));
        }
        out.push(Line::raw(""));
        if let Some(e) = &self.err {
            out.push(Line::styled(format!(" {e}"), theme::warn()));
        }
        let last = self.at + 1 >= self.vals.len();
        let enter = if last { t!("跑", "run") } else { t!("下一项", "next") };
        out.push(kit::keys_line(&[("⏎", enter), ("Tab ⇧Tab", t!("换一项", "move")), ("^U", t!("清空", "clear"))], vec![], w));
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let n = self.vals.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.err = None;
        match k.code {
            KeyCode::Esc => return Flow::Close,
            KeyCode::Tab | KeyCode::Down => self.at = (self.at + 1) % n,
            KeyCode::BackTab | KeyCode::Up => self.at = (self.at + n - 1) % n,
            KeyCode::Enter if self.at + 1 < n => self.at += 1,
            KeyCode::Enter => match self.given() {
                Ok(args) => {
                    let then = self.then.clone();
                    start(Req { item: self.item.clone(), session: self.target.session.clone(), args, then, ..Default::default() }, fx);
                    return Flow::Close;
                }
                Err(e) => self.err = Some(e),
            },
            KeyCode::Char('u') if ctrl => self.vals[self.at].clear(),
            KeyCode::Char(' ') if self.is_bool(self.at) => {
                let opt = self.item.args[self.at].2;
                let v = &mut self.vals[self.at];
                *v = match v.as_str() {
                    "false" => "true".into(),
                    "true" if opt => String::new(),
                    _ => "false".into(),
                };
            }
            KeyCode::Backspace if !self.is_bool(self.at) => {
                self.vals[self.at].pop();
            }
            KeyCode::Char(c) if !ctrl && !self.is_bool(self.at) && self.vals[self.at].chars().count() < INPUT_MAX => {
                self.vals[self.at].push(c)
            }
            _ => {}
        }
        Flow::Stay
    }

    fn typing(&self) -> bool {
        true
    }
}

/// 列表里一页最多列这么多行,再多跟着高亮滚
const LIST_SHOWN: usize = 14;
/// 键跑出来的最多留这么多行
const SAID_MAX: usize = 6;

/// 插件的列表视图:命令输出的一行行(见 `aca_core::plugin::ViewRows`),停在哪行按清单 `[[views.keys]]` 的键
pub struct List {
    /// 跑出这份列表的那个请求:`r` 重读、键跑完了再跑它
    req: Req,
    list: plugin::ViewRows,
    /// 跑失败、读不懂时说的
    err: Option<String>,
    sel: usize,
    /// 按了一次要按两次的键(它的名字),等再按一次
    armed: Option<String>,
    /// 上一个键跑出来的(撤掉之后怎么加回去这类)
    said: Vec<String>,
    said_ok: bool,
}

/// 键的名字(清单里写的):⏎ 是 `enter`,别的就是那个字符
fn key_name(k: &KeyEvent) -> Option<String> {
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match k.code {
        KeyCode::Enter => Some("enter".into()),
        KeyCode::Char(c) => Some(c.to_string()),
        _ => None,
    }
}

impl List {
    pub fn new(req: Req, ok: bool, out: &str) -> List {
        let mut l = List { req, list: Default::default(), err: None, sel: 0, armed: None, said: vec![], said_ok: true };
        l.feed(ok, out);
        l
    }

    fn cur(&self) -> Option<&plugin::ViewRow> {
        self.list.rows.get(self.sel)
    }

    fn act(&self, name: &str) -> Option<&plugin::RowAct> {
        self.req.item.acts.iter().find(|a| a.key == name)
    }

    /// 这个键在这一行上能不能按:要停在行上的得有行,模板里用到的字段都得有
    fn usable(&self, a: &plugin::RowAct) -> Option<Vec<(String, String)>> {
        let row = if a.row { Some(self.cur()?) } else { None };
        if a.run.is_none() {
            plugin::fill(&a.copy, &self.list, row)?;
            return Some(vec![]);
        }
        a.args.iter().map(|(n, t)| Some((n.clone(), plugin::fill(t, &self.list, row)?))).collect()
    }

    fn reload_req(&self) -> Req {
        Req { refresh: true, then: None, ..self.req.clone() }
    }

    /// 高亮往下挪几行(往上挪 = 挪 n - 1 行),绕回头
    fn step(&mut self, by: usize) -> Flow {
        let n = self.list.rows.len();
        self.sel = (self.sel + by) % n;
        Flow::Stay
    }

    fn row_line(&self, r: &plugin::ViewRow, w: usize) -> Line<'static> {
        let pal = theme::pal();
        let mark_st = match r.tone.as_str() {
            "ok" => Style::new().fg(pal.green),
            "warn" => theme::warn(),
            "bad" => Style::new().fg(pal.red),
            "dim" => kit::dim(),
            _ => Style::new(),
        };
        let mark = if r.mark.is_empty() { "·".to_string() } else { r.mark.clone() };
        // 右边那段最多占一半(手机上),剩下的给正文
        let right = kit::clip(&r.right, (w / 2).max(8)).trim_end().to_string();
        let room = w.saturating_sub(mark.width() + 1 + right.width() + 3);
        kit::spread(
            vec![Span::styled(mark, mark_st), Span::raw(" "), Span::raw(kit::clip(&r.text, room).trim_end().to_string())],
            vec![Span::styled(right, if r.tone == "warn" { theme::warn() } else { kit::dim() })],
            w,
        )
    }
}

impl Overlay for List {
    fn width(&self) -> u16 {
        84
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let wrap = |text: &str, st: Style| crate::ui::wrap_width(text, w).into_iter().map(move |l| Line::styled(l, st)).collect::<Vec<_>>();
        let n = self.list.rows.len();
        let at = if n > LIST_SHOWN { vec![Span::styled(format!("{}/{n}  ", self.sel + 1), kit::dim())] } else { vec![] };
        let mut out = vec![kit::modal_title(&self.req.title(), theme::accent(), [at, kit::esc_hint(t!("返回", "back"))].concat(), w)];
        for h in self.list.head.lines().filter(|h| !h.is_empty()) {
            out.push(Line::styled(kit::clip(h, w).trim_end().to_string(), kit::dim()));
        }
        for x in self.list.warn.lines().chain(self.err.iter().flat_map(|e| e.lines())).filter(|x| !x.is_empty()) {
            out.extend(wrap(x, theme::warn()));
        }
        out.push(Line::raw(""));
        if self.list.rows.is_empty() && self.err.is_none() {
            out.extend(wrap(if self.list.empty.is_empty() { t!("空的", "empty") } else { &self.list.empty }, kit::dim()));
        }
        let from = self.sel.saturating_sub(LIST_SHOWN / 2).min(n.saturating_sub(LIST_SHOWN));
        for (i, r) in self.list.rows.iter().enumerate().skip(from).take(LIST_SHOWN) {
            out.push(kit::picked(self.row_line(r, w), w, i == self.sel));
        }
        if let Some(r) = self.cur().filter(|r| !r.detail.is_empty()) {
            out.push(Line::raw(""));
            for d in &r.detail {
                out.extend(wrap(d, kit::dim()));
            }
        }
        if let Some(a) = self.armed.as_deref().and_then(|k| self.act(k)) {
            let key = if a.key == "enter" { "⏎" } else { &a.key };
            out.extend(wrap(&tr!("再按 {key}:{}", "press {key} again: {}", a.title), theme::warn()));
        }
        if !self.said.is_empty() {
            out.push(Line::raw(""));
            for l in &self.said {
                out.extend(wrap(l, if self.said_ok { kit::dim() } else { theme::warn() }));
            }
        }
        out.push(Line::raw(""));
        let mut keys: Vec<(String, String)> = self
            .req
            .item
            .acts
            .iter()
            .filter(|a| self.usable(a).is_some())
            .map(|a| (if a.key == "enter" { "⏎".to_string() } else { a.key.clone() }, a.title.clone()))
            .collect();
        if self.act("r").is_none() {
            keys.push(("r".into(), t!("重读", "reload").into()));
        }
        let keys: Vec<(&str, &str)> = keys.iter().map(|(k, t)| (k.as_str(), t.as_str())).collect();
        out.push(kit::keys_line(&keys, vec![], w));
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let armed = self.armed.take();
        let n = self.list.rows.len();
        match k.code {
            KeyCode::Esc => return Flow::Close,
            KeyCode::Down if n > 0 => return self.step(1),
            KeyCode::Up if n > 0 => return self.step(n - 1),
            _ => {}
        }
        let Some(name) = key_name(&k) else { return Flow::Stay };
        match name.as_str() {
            "q" => return Flow::Close,
            "j" if n > 0 => return self.step(1),
            "k" if n > 0 => return self.step(n - 1),
            _ => {}
        }
        let Some(a) = self.act(&name).cloned() else {
            if name == "r" {
                fx.say(tr!("正在重读 {}…", "reloading {}…", self.req.title()));
                fx.plugin = Some(self.reload_req());
            }
            return Flow::Stay;
        };
        let Some(given) = self.usable(&a) else {
            if self.cur().is_some() || !a.row {
                fx.say(tr!("这一行上按不了:{}", "not on this row: {}", a.title));
            }
            return Flow::Stay;
        };
        if a.confirm && armed.as_deref() != Some(name.as_str()) {
            self.armed = Some(name);
            return Flow::Stay;
        }
        let row = if a.row { self.cur() } else { None };
        match &a.run {
            None => {
                fx.copy = plugin::fill(&a.copy, &self.list, row);
            }
            Some(item) => {
                let then = self.reload_req();
                if item.args.iter().any(|(n, _, opt)| !opt && !given.iter().any(|(g, _)| g == n)) {
                    fx.open = Some(Box::new(Form::from_list(item.clone(), &self.req.session, &given, then)));
                } else {
                    let req = Req {
                        item: item.clone(),
                        session: self.req.session.clone(),
                        args: given,
                        then: Some(Box::new(then)),
                        refresh: false,
                    };
                    start(req, fx);
                }
            }
        }
        Flow::Stay
    }

    fn takes(&self, req: &Req) -> bool {
        req.item.qualified() == self.req.item.qualified() && req.session == self.req.session
    }

    fn feed(&mut self, ok: bool, out: &str) {
        let parsed = if ok { plugin::ViewRows::parse(out) } else { Err(out.trim().to_string()) };
        match parsed {
            Ok(list) => {
                // 高亮跟着原来那一行(按 id);它没了就留在原来的位置上
                let was = self.cur().map(|r| r.id.clone());
                self.list = list;
                self.err = None;
                let same = was.and_then(|id| self.list.rows.iter().position(|r| r.id == id));
                self.sel = same.unwrap_or(self.sel).min(self.list.rows.len().saturating_sub(1));
            }
            Err(e) => self.err = Some(if e.is_empty() { t!("出错了,没说为什么", "failed without saying why").into() } else { e }),
        }
    }

    fn said(&mut self, ok: bool, out: &str) {
        self.said = out.trim().lines().take(SAID_MAX).map(String::from).collect();
        if self.said.is_empty() {
            self.said = vec![if ok { t!("跑完了", "done") } else { t!("出错了", "failed") }.into()];
        }
        self.said_ok = ok;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn item(id: &str, kind: ItemKind, key: Option<char>, contexts: &[&str], args: &[(&str, &str, bool)]) -> MenuItem {
        MenuItem {
            plugin: "p.x".into(),
            plugin_name: "PX".into(),
            id: id.into(),
            kind,
            title: format!("{id} 标题"),
            key,
            contexts: contexts.iter().map(|c| c.to_string()).collect(),
            args: args.iter().map(|(n, t, o)| (n.to_string(), t.to_string(), *o)).collect(),
            ..Default::default()
        }
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    fn on_session() -> Target {
        Target { on: "session", session: "s1".into(), label: "主线".into() }
    }

    #[test]
    fn the_menu_keeps_what_fits_the_row_and_a_bound_key_runs_at_once() {
        let all = vec![
            item("stat", ItemKind::Command, None, &["session"], &[]),
            item("tidy", ItemKind::Command, None, &["group"], &[]),
            item("ci", ItemKind::View, Some('c'), &[], &[]),
        ];
        let mut m = Menu::new(all, on_session());
        assert_eq!(m.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["stat", "ci"]);
        let mut fx = Fx::default();
        assert_eq!(m.on_key(key(KeyCode::Char('c')), &mut fx), Flow::Close);
        let req = fx.plugin.expect("跑起来了");
        assert_eq!((req.item.id.as_str(), req.session.as_str()), ("ci", "s1"));
    }

    #[test]
    fn filtering_narrows_the_list_and_enter_runs_the_one_left() {
        let all = vec![item("stat", ItemKind::Command, None, &[], &[]), item("deploy", ItemKind::Command, None, &[], &[])];
        let mut m = Menu::new(all, on_session());
        let mut fx = Fx::default();
        m.on_key(key(KeyCode::Char('/')), &mut fx);
        for c in "dep".chars() {
            m.on_key(key(KeyCode::Char(c)), &mut fx);
        }
        assert_eq!(m.shown(), vec![1]);
        assert_eq!(m.on_key(key(KeyCode::Enter), &mut fx), Flow::Close);
        assert_eq!(fx.plugin.unwrap().item.id, "deploy");
    }

    #[test]
    fn a_command_with_arguments_asks_in_a_form_and_checks_them() {
        let it = item("run", ItemKind::Command, None, &[], &[("host", "string", false), ("n", "int", true), ("dry", "bool", true)]);
        let m = Menu::new(vec![it.clone()], on_session());
        let mut fx = Fx::default();
        assert_eq!(m.pick(0, &mut fx), Flow::Close);
        assert!(fx.open.is_some() && fx.plugin.is_none());
        let mut f = Form::new(it, on_session());
        // 类型写人话,字符串不写;bool 写开 / 关
        let t = text(&f, 70);
        assert!(t.contains(" host\n") && t.contains(" n (整数,可不填)") && t.contains(" dry (空格 开/关,可不填)"), "{t}");
        assert!(!t.contains("string") && !t.contains("int") && !t.contains("bool"), "{t}");
        let mut fx = Fx::default();
        // 必填的没填:不跑
        for _ in 0..3 {
            f.on_key(key(KeyCode::Enter), &mut fx);
        }
        assert!(fx.plugin.is_none() && f.err.as_deref().is_some_and(|e| e.contains("host")));
        f.at = 0;
        for c in "nas".chars() {
            f.on_key(key(KeyCode::Char(c)), &mut fx);
        }
        f.on_key(key(KeyCode::Tab), &mut fx);
        f.on_key(key(KeyCode::Char('x')), &mut fx);
        f.on_key(key(KeyCode::Tab), &mut fx);
        f.on_key(key(KeyCode::Enter), &mut fx);
        assert!(fx.plugin.is_none() && f.err.as_deref().is_some_and(|e| e.contains('n')), "{:?}", f.err);
        f.vals[1] = "3".into();
        f.at = 2;
        f.on_key(key(KeyCode::Char(' ')), &mut fx);
        assert_eq!(f.on_key(key(KeyCode::Enter), &mut fx), Flow::Close);
        let req = fx.plugin.unwrap();
        assert_eq!(req.args, vec![("host".into(), "nas".into()), ("n".into(), "3".into()), ("dry".into(), "false".into())]);
        assert!(text(&f, 70).contains("   关"), "{}", text(&f, 70));
    }

    #[test]
    fn an_empty_menu_says_where_plugins_come_from() {
        let t = text(&Menu::new(vec![], on_session()), 70);
        assert!(t.contains("还没有插件命令 · P 装插件") && !t.contains("[["), "{t}");
    }

    /// 挂载那样的一个列表视图:⏎ 复制地址、x 撤(按两次)、a 挂端口(要填)、p 开关发布(列表上的字段)
    fn mounts() -> Req {
        let cmd = |id: &str, args: &[(&str, &str, bool)]| Some(item(id, ItemKind::Command, None, &[], args));
        let act =
            |key: &str, title: &str, run: Option<MenuItem>, args: &[(&str, &str)], copy: &str, confirm: bool, row: bool| plugin::RowAct {
                key: key.into(),
                title: title.into(),
                run,
                args: args.iter().map(|(n, t)| (n.to_string(), t.to_string())).collect(),
                copy: copy.into(),
                confirm,
                row,
            };
        let mut view = item("mounts", ItemKind::View, Some('m'), &[], &[]);
        view.rows = true;
        view.acts = vec![
            act("enter", "复制地址", None, &[], "{url}", false, true),
            act("x", "撤掉", cmd("drop", &[("target", "string", false)]), &[("target", "{drop}")], "", true, true),
            act("a", "挂一个本机端口", cmd("add", &[("port", "int", false)]), &[], "", false, false),
            act("p", "开关发布", cmd("publish", &[("to", "string", false)]), &[("to", "{publish_to}")], "", false, false),
        ];
        Req { item: view, session: "s1".into(), ..Default::default() }
    }

    const OUT: &str = r#"{"head":"mac.ts.net\n发布 关着","warn":"","empty":"没有挂载","fields":{"publish_to":"on"},"rows":[
        {"id":"https:443/","mark":"✕","tone":"bad","text":":443 / → 127.0.0.1:8420","right":"没在听","detail":["https://mac.ts.net/"],"fields":{"url":"https://mac.ts.net/","drop":"https:443/"}},
        {"id":"fg:https:9000/","mark":"✕","tone":"bad","text":":9000 / → 127.0.0.1:9000","right":"前台","detail":["前台挂的撤不了"],"fields":{"url":"https://mac.ts.net:9000/"}}]}"#;

    fn text(o: &dyn Overlay, w: usize) -> String {
        o.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string() + "\n").collect()
    }

    fn press(o: &mut dyn Overlay, code: KeyCode) -> (Fx, Flow) {
        let mut fx = Fx::default();
        let f = o.on_key(key(code), &mut fx);
        (fx, f)
    }

    #[test]
    fn a_list_view_shows_its_rows_and_the_highlighted_ones_detail_and_only_the_keys_that_work_there() {
        let l = List::new(mounts(), true, OUT);
        let t = text(&l, 80);
        assert!(t.starts_with("PX · mounts 标题") && t.contains("mac.ts.net\n发布 关着\n"), "{t}");
        assert!(t.contains("✕ :443 / → 127.0.0.1:8420") && t.contains("没在听") && t.contains("https://mac.ts.net/"), "{t}");
        assert!(t.contains("⏎ 复制地址") && t.contains("x 撤掉") && t.contains("a 挂一个本机端口") && t.contains("p 开关发布"), "{t}");
        assert!(t.contains("r 重读"), "{t}");
        // 前台那行:没有 drop,x 不列;手机宽度不出界
        let mut l = l;
        press(&mut l, KeyCode::Char('j'));
        let t = text(&l, 80);
        assert!(t.contains("前台挂的撤不了") && !t.contains("x 撤掉"), "{t}");
        assert!(l.lines(40).iter().all(|x| x.width() <= 40));
        let (fx, _) = press(&mut l, KeyCode::Char('x'));
        assert!(fx.plugin.is_none() && fx.flash.unwrap().contains("这一行上按不了:撤掉"));
        // 没装 tpub(列表上没有 publish_to):p 不列
        let bare = List::new(mounts(), true, &OUT.replace(r#""fields":{"publish_to":"on"},"#, ""));
        assert!(!text(&bare, 80).contains("p 开关发布"));
        // 空的:写它说的那句;跑失败 / 读不懂:写出来
        assert!(text(&List::new(mounts(), true, r#"{"empty":"没有挂载"}"#), 80).contains("没有挂载"));
        assert!(text(&List::new(mounts(), false, "tailscale isn't installed"), 80).contains("tailscale isn't installed"));
        assert!(text(&List::new(mounts(), true, "oops"), 80).contains("列表读不懂"));
    }

    #[test]
    fn row_keys_copy_run_with_the_row_ask_twice_and_hand_the_result_back_to_the_list() {
        let mut l = List::new(mounts(), true, OUT);
        assert_eq!(press(&mut l, KeyCode::Enter).0.copy.as_deref(), Some("https://mac.ts.net/"));
        // x:第一次只提醒,第二次带着这一行跑 drop;跑完交回这个列表(只重读,不另开)
        let (fx, _) = press(&mut l, KeyCode::Char('x'));
        assert!(fx.plugin.is_none() && text(&l, 80).contains("再按 x:撤掉"));
        let req = press(&mut l, KeyCode::Char('x')).0.plugin.expect("跑了");
        assert_eq!(
            (req.item.id.as_str(), req.args.clone(), req.session.as_str()),
            ("drop", vec![("target".into(), "https:443/".into())], "s1")
        );
        let then = req.then.expect("交回列表");
        assert!(then.refresh && l.takes(&then) && then.item.id == "mounts");
        // 中间按了别的就不算
        press(&mut l, KeyCode::Char('x'));
        press(&mut l, KeyCode::Char('k'));
        assert!(press(&mut l, KeyCode::Char('x')).0.plugin.is_none());
        // p:读列表上的字段,不用停在哪一行
        let req = press(&mut l, KeyCode::Char('p')).0.plugin.unwrap();
        assert_eq!(req.args, vec![("to".into(), "on".into())]);
        // a:端口要填,先弹表单;填完跑,也交回列表
        let (mut fx, _) = press(&mut l, KeyCode::Char('a'));
        let mut form = fx.open.take().expect("表单");
        assert!(fx.plugin.is_none());
        for c in "3000".chars() {
            press(form.as_mut(), KeyCode::Char(c));
        }
        let req = press(form.as_mut(), KeyCode::Enter).0.plugin.unwrap();
        assert_eq!((req.item.id.as_str(), req.args.clone()), ("add", vec![("port".into(), "3000".into())]));
        assert!(req.then.is_some_and(|t| l.takes(&t)));
        // r(插件没绑):重读这个列表
        let req = press(&mut l, KeyCode::Char('r')).0.plugin.unwrap();
        assert!(req.refresh && l.takes(&req));
        assert_eq!(press(&mut l, KeyCode::Char('q')).1, Flow::Close);
    }

    #[test]
    fn a_reloaded_list_keeps_the_highlight_on_the_same_row_and_shows_what_the_last_key_said() {
        let mut l = List::new(mounts(), true, OUT);
        press(&mut l, KeyCode::Char('j'));
        // 上面那行撤掉了:高亮还在前台那行上
        let after = OUT.replace(r#"{"id":"https:443/","mark":"✕","tone":"bad","text":":443 / → 127.0.0.1:8420","right":"没在听","detail":["https://mac.ts.net/"],"fields":{"url":"https://mac.ts.net/","drop":"https:443/"}},"#, "");
        l.said(true, "已撤 https://mac.ts.net/\n要加回去:tailscale serve --bg --https=443 http://127.0.0.1:8420");
        l.feed(true, &after);
        assert_eq!(l.cur().map(|r| r.id.as_str()), Some("fg:https:9000/"));
        let t = text(&l, 80);
        assert!(t.contains("已撤 https://mac.ts.net/\n要加回去:tailscale serve --bg --https=443 http://127.0.0.1:8420"), "{t}");
        // 别的会话、别的视图的不归它
        let mut other = mounts();
        other.session = "s2".into();
        assert!(!l.takes(&other) && l.takes(&mounts()));
        // 跑失败的:警告色写着,列表留着上一份
        l.feed(false, "tailscale didn't answer");
        assert!(text(&l, 80).contains("tailscale didn't answer") && l.list.rows.len() == 1);
    }
}
