//! Running what enabled plugins expose to the interface: commands (the manifest's `[[commands]]`, called commands in the
//! interface; the ones for agents are called "tools for agents"), panes and views (see `aca_core::plugin::menu`). They're listed
//! first in the main list's `?` sheet (sheet.rs), narrowed by the highlighted row to the ones that make sense here; keys the
//! manifest's `[[keys]]` binds work only there. Diagnostic ones (try it, test connectivity) don't go there; the manifest
//! exposes them to the cli only.
//!
//! - A command with parameters first pops up a form (`Form`); once it has run, short output goes to the bottom bar, long
//!   output into the full-screen reading page
//! - Views: output goes into the full-screen reading page, `^R` reruns. A list view (`rows = true`) opens as a [`List`]
//!   page: one item per row, the keys from the manifest's `[[views.keys]]` act on the highlighted row (copy a field, run one
//!   of this plugin's commands with that row), the result is written below and the list reloads
//! - Panes: tmux popup / split / new window (when the interface isn't in tmux, it gives up the terminal and runs in the
//!   foreground)
//!
//! Commands and views all run in the background (the run loop picks up `Fx::plugin`, one thread per request), not taking
//! the interface's one-at-a-time background job.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::plugin::{self, ItemKind, MenuItem};
use aca_core::{t, tr};

use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

/// The longest input for one parameter
const INPUT_MAX: usize = 2000;

/// What it runs against
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    /// session / group / global
    pub on: &'static str,
    /// Session id (a registered session); empty for a group or elsewhere
    pub session: String,
    /// What the menu title says: session title / group name
    pub label: String,
}

/// One thing to run
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Req {
    pub item: MenuItem,
    pub session: String,
    pub args: Vec<(String, String)>,
    /// Run by a list view's key: the result goes to that list, which then reloads
    pub then: Option<Box<Req>>,
    /// A list reload: replaces only the open page; if it's already closed, never mind (no new page)
    pub refresh: bool,
}

impl Req {
    /// What the bottom bar and the reading page title call it
    pub fn title(&self) -> String {
        format!("{} · {}", self.item.plugin_name, self.item.title)
    }
}

/// The context for this session (dir, its tmux session id and pane); empty without a session
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

/// Run one: commands and views capture the output; a pane is opened (returns once the popup closes). Called on a background
/// thread; for a pane while the interface isn't in tmux, called from where the terminal is given up
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

/// Start it: a pane while the interface isn't in tmux has to give up the terminal; everything else goes to the background
pub(crate) fn start(req: Req, fx: &mut Fx) {
    if req.item.kind == ItemKind::Pane && std::env::var_os("TMUX").is_none() {
        fx.external = Some(crate::view::External::Plugin(req));
        return;
    }
    fx.say(tr!("正在运行 {}…", "running {}…", req.title()));
    fx.plugin = Some(req);
}

/// A command with parameters: one field per parameter, runs once filled in
pub struct Form {
    item: MenuItem,
    target: Target,
    vals: Vec<String>,
    at: usize,
    err: Option<String>,
    /// Came from a list view's key: hand the result back to that list
    then: Option<Box<Req>>,
}

impl Form {
    pub fn new(item: MenuItem, target: Target) -> Form {
        let vals = item.args.iter().map(|(_, ty, opt)| if ty == "bool" && !opt { "false".into() } else { String::new() }).collect();
        Form { item, target, vals, at: 0, err: None, then: None }
    }

    /// Opened by a list view's key: what this row already gives is filled in, the highlight lands on the first empty one;
    /// the result goes back to the list
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

    /// Check each against its type; optional ones left empty aren't passed
    fn given(&self) -> Result<Vec<(String, String)>, String> {
        let mut out = vec![];
        for ((name, ty, opt), v) in self.item.args.iter().zip(&self.vals) {
            let v = v.trim();
            if v.is_empty() {
                if !opt {
                    return Err(tr!("{name} 为必填项", "{name} is required"));
                }
                continue;
            }
            if ty == "int" && v.parse::<i64>().is_err() {
                return Err(tr!("{name} 必须是整数", "{name} takes an integer"));
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
    /// Always typing: ? is a character here
    fn sheet(&self) -> Option<Vec<crate::sheet::Key>> {
        None
    }

    fn width(&self) -> u16 {
        72
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let title = format!("{} · {}", self.item.plugin_name, self.item.title);
        let mut out = vec![kit::modal_title(&title, theme::accent(), kit::esc_hint(t!("取消", "cancel")), w), Line::raw("")];
        for (i, ((name, ty, opt), v)) in self.item.args.iter().zip(&self.vals).enumerate() {
            let on = i == self.at;
            // Types in plain words (none for strings), optional ones marked
            let kind = match ty.as_str() {
                "int" => t!("整数", "integer"),
                "path" => t!("路径", "path"),
                "bool" => t!("空格 开/关", "space: on/off"),
                _ => "",
            };
            let hint: Vec<&str> = [kind, if *opt { t!("选填", "optional") } else { "" }].into_iter().filter(|x| !x.is_empty()).collect();
            let hint = if hint.is_empty() { String::new() } else { format!("({})", hint.join(t!(",", ", "))) };
            out.push(Line::from(vec![
                Span::styled(format!(" {name} "), if on { theme::accent() } else { Style::new().add_modifier(Modifier::BOLD) }),
                Span::styled(hint, kit::dim()),
            ]));
            let shown = match (ty.as_str(), v.as_str()) {
                ("bool", "") => t!("未设置", "not given").to_string(),
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
        let enter = if last { t!("运行", "run") } else { t!("下一项", "next") };
        out.push(kit::keys_line(&[("⏎", enter), ("Tab ⇧Tab", t!("切换项", "move")), ("^U", t!("清空", "clear"))], vec![], w));
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

/// At most this many rows per list page; beyond that, the list scrolls with the highlight
const LIST_SHOWN: usize = 14;
/// Keep at most this many lines of what a key's run printed
const SAID_MAX: usize = 6;

/// A plugin's list view: the rows of a command's output (see `aca_core::plugin::ViewRows`), with the manifest's
/// `[[views.keys]]` keys acting on the highlighted row
pub struct List {
    /// The request that produced this list: `r` reloads with it, and it runs again after a key's run
    req: Req,
    list: plugin::ViewRows,
    /// What to say when the run failed or the output couldn't be understood
    err: Option<String>,
    sel: usize,
    /// A press-twice key that was pressed once (its name), waiting for the second press
    armed: Option<String>,
    /// What the last key's run printed (such as how to add back what was removed)
    said: Vec<String>,
    said_ok: bool,
}

/// A key's name (as the manifest writes it): ⏎ is `enter`, anything else is the character itself
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

    /// Its keys: the plugin's that work on this row, then ^R; for the bottom bar and `?`
    fn keys(&self) -> Vec<crate::sheet::Key> {
        use crate::sheet::{key, more};
        let mut keys: Vec<crate::sheet::Key> = self
            .req
            .item
            .acts
            .iter()
            .filter(|a| self.usable(a).is_some())
            .map(|a| key(if a.key == "enter" { "⏎" } else { a.key.as_str() }, a.title.as_str()))
            .collect();
        keys.push(key("^R", t!("重新读取", "reload")));
        keys.extend([more("↑↓", t!("选择", "select")), more("Esc", t!("返回", "back"))]);
        keys
    }

    /// Run one of its keys on the highlighted row (copy, or the plugin's command; one missing an argument opens the form first)
    fn run(&mut self, a: &plugin::RowAct, fx: &mut Fx) -> Flow {
        let Some(given) = self.usable(a) else { return Flow::Stay };
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

    fn act(&self, name: &str) -> Option<&plugin::RowAct> {
        self.req.item.acts.iter().find(|a| a.key == name)
    }

    /// Whether this key works on this row: one that acts on a row needs a row, and every field its template uses must be
    /// there
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

    /// Move the highlight down by some rows (up = by n - 1 rows), wrapping around
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
        // The right part takes at most half (on a phone), the rest goes to the text
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
        let wrap =
            |text: &str, st: Style| crate::kit::wrap_width(text, w).into_iter().map(move |l| Line::styled(l, st)).collect::<Vec<_>>();
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
            out.extend(wrap(if self.list.empty.is_empty() { t!("无内容", "empty") } else { &self.list.empty }, kit::dim()));
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
            out.push(Line::raw(""));
            out.extend(wrap(&tr!("{}?", "{}?", a.title), theme::warn()));
            out.push(Line::raw(""));
            out.push(kit::keys_line(&[("y", t!("确定", "do it")), ("Esc", t!("取消", "keep"))], vec![], w));
            return out;
        }
        if !self.said.is_empty() {
            out.push(Line::raw(""));
            for l in &self.said {
                out.extend(wrap(l, if self.said_ok { kit::dim() } else { theme::warn() }));
            }
        }
        out.push(Line::raw(""));
        out.push(crate::kit::keys_line_help(&crate::sheet::foot(&self.keys()), vec![], w));
        out
    }

    fn sheet(&self) -> Option<Vec<crate::sheet::Key>> {
        self.armed.is_none().then(|| self.keys())
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        // Asked about a key the plugin marked confirm: only y goes ahead; Esc says nothing, any other key (the same key again
        // included) says it wasn't done
        if let Some(name) = self.armed.take() {
            let Some(a) = self.act(&name).cloned() else { return Flow::Stay };
            match k.code {
                KeyCode::Char('y') if !k.modifiers.contains(KeyModifiers::CONTROL) => return self.run(&a, fx),
                KeyCode::Esc => {}
                _ => fx.say(tr!("未执行:{}", "not done: {}", a.title)),
            }
            return Flow::Stay;
        }
        let n = self.list.rows.len();
        match k.code {
            KeyCode::Esc => return Flow::Close,
            KeyCode::Down if n > 0 => return self.step(1),
            KeyCode::Up if n > 0 => return self.step(n - 1),
            KeyCode::Char('r') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                fx.say(tr!("正在重新读取 {}…", "reloading {}…", self.req.title()));
                fx.plugin = Some(self.reload_req());
                return Flow::Stay;
            }
            _ => {}
        }
        let Some(name) = key_name(&k) else { return Flow::Stay };
        match name.as_str() {
            "q" => return Flow::Close,
            "j" if n > 0 => return self.step(1),
            "k" if n > 0 => return self.step(n - 1),
            _ => {}
        }
        let Some(a) = self.act(&name).cloned() else { return Flow::Stay };
        if self.usable(&a).is_none() {
            if self.cur().is_some() || !a.row {
                fx.say(tr!("此行不支持:{}", "not on this row: {}", a.title));
            }
            return Flow::Stay;
        }
        if a.confirm {
            self.armed = Some(name);
            return Flow::Stay;
        }
        self.run(&a, fx)
    }

    fn takes(&self, req: &Req) -> bool {
        req.item.qualified() == self.req.item.qualified() && req.session == self.req.session
    }

    fn feed(&mut self, ok: bool, out: &str) {
        let parsed = if ok { plugin::ViewRows::parse(out) } else { Err(out.trim().to_string()) };
        match parsed {
            Ok(list) => {
                // The highlight follows the same row (by id); if it's gone, it stays at the same position
                let was = self.cur().map(|r| r.id.clone());
                self.list = list;
                self.err = None;
                let same = was.and_then(|id| self.list.rows.iter().position(|r| r.id == id));
                self.sel = same.unwrap_or(self.sel).min(self.list.rows.len().saturating_sub(1));
            }
            Err(e) => self.err = Some(if e.is_empty() { t!("出错,原因未知", "failed without saying why").into() } else { e }),
        }
    }

    fn said(&mut self, ok: bool, out: &str) {
        self.said = out.trim().lines().take(SAID_MAX).map(String::from).collect();
        if self.said.is_empty() {
            self.said = vec![if ok { t!("已完成", "done") } else { t!("出错", "failed") }.into()];
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
    fn a_command_with_arguments_asks_in_a_form_and_checks_them() {
        let it = item("run", ItemKind::Command, None, &[], &[("host", "string", false), ("n", "int", true), ("dry", "bool", true)]);
        let mut f = Form::new(it, on_session());
        // Types in plain words, none for strings; bool says on / off
        let t = text(&f, 70);
        assert!(t.contains(" host\n") && t.contains(" n (整数,选填)") && t.contains(" dry (空格 开/关,选填)"), "{t}");
        assert!(!t.contains("string") && !t.contains("int") && !t.contains("bool"), "{t}");
        let mut fx = Fx::default();
        // A required one left empty: doesn't run
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

    /// A list view like the mounts one: ⏎ copies the URL, x takes one down (press twice), a mounts a port (needs input), p
    /// toggles publishing (a list-level field)
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
        assert!(t.contains("^R 重新读取"), "{t}");
        // The foreground row: no drop, so x isn't listed; phone width doesn't overflow
        let mut l = l;
        press(&mut l, KeyCode::Char('j'));
        let t = text(&l, 80);
        assert!(t.contains("前台挂的撤不了") && !t.contains("x 撤掉"), "{t}");
        assert!(l.lines(40).iter().all(|x| x.width() <= 40));
        let (fx, _) = press(&mut l, KeyCode::Char('x'));
        assert!(fx.plugin.is_none() && fx.flash.unwrap().contains("此行不支持:撤掉"));
        // tpub not installed (no publish_to on the list): p isn't listed
        let bare = List::new(mounts(), true, &OUT.replace(r#""fields":{"publish_to":"on"},"#, ""));
        assert!(!text(&bare, 80).contains("p 开关发布"));
        // Empty: shows the line it gives; a failed run / unreadable output: shown
        assert!(text(&List::new(mounts(), true, r#"{"empty":"没有挂载"}"#), 80).contains("没有挂载"));
        assert!(text(&List::new(mounts(), false, "tailscale isn't installed"), 80).contains("tailscale isn't installed"));
        assert!(text(&List::new(mounts(), true, "oops"), 80).contains("无法解析列表"));
    }

    #[test]
    fn row_keys_copy_run_with_the_row_ask_first_and_hand_the_result_back_to_the_list() {
        let mut l = List::new(mounts(), true, OUT);
        assert_eq!(press(&mut l, KeyCode::Enter).0.copy.as_deref(), Some("https://mac.ts.net/"));
        // x (marked confirm): asks first, only y runs drop with this row; the result goes back to this list (reload
        // only, no new page)
        let (fx, _) = press(&mut l, KeyCode::Char('x'));
        let t = text(&l, 80);
        assert!(fx.plugin.is_none() && t.contains("撤掉?") && t.contains("y 确定 • Esc 取消"), "{t}");
        let req = press(&mut l, KeyCode::Char('y')).0.plugin.expect("跑了");
        assert_eq!(
            (req.item.id.as_str(), req.args.clone(), req.session.as_str()),
            ("drop", vec![("target".into(), "https:443/".into())], "s1")
        );
        let then = req.then.expect("交回列表");
        assert!(then.refresh && l.takes(&then) && then.item.id == "mounts");
        // The same key again, or anything else, keeps it and says so (and does nothing else); Esc only cancels, the page stays
        press(&mut l, KeyCode::Char('x'));
        let (fx, _) = press(&mut l, KeyCode::Char('x'));
        assert_eq!((fx.plugin.is_none(), fx.flash.as_deref()), (true, Some("未执行:撤掉")));
        press(&mut l, KeyCode::Char('x'));
        let (fx, flow) = press(&mut l, KeyCode::Esc);
        assert_eq!((fx.plugin.is_none(), fx.flash, flow), (true, None, Flow::Stay));
        assert!(press(&mut l, KeyCode::Char('y')).0.plugin.is_none());
        // p: reads the list-level field, no row needed
        let req = press(&mut l, KeyCode::Char('p')).0.plugin.unwrap();
        assert_eq!(req.args, vec![("to".into(), "on".into())]);
        // a: the port needs input, so a form pops up first; runs once filled in, also handed back to the list
        let (mut fx, _) = press(&mut l, KeyCode::Char('a'));
        let mut form = fx.open.take().expect("表单");
        assert!(fx.plugin.is_none());
        for c in "3000".chars() {
            press(form.as_mut(), KeyCode::Char(c));
        }
        let req = press(form.as_mut(), KeyCode::Enter).0.plugin.unwrap();
        assert_eq!((req.item.id.as_str(), req.args.clone()), ("add", vec![("port".into(), "3000".into())]));
        assert!(req.then.is_some_and(|t| l.takes(&t)));
        // ^R reloads this list; a bare r is the plugin's to bind
        assert!(press(&mut l, KeyCode::Char('r')).0.plugin.is_none());
        let mut fx = Fx::default();
        l.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL), &mut fx);
        let req = fx.plugin.unwrap();
        assert!(req.refresh && l.takes(&req));
        assert_eq!(press(&mut l, KeyCode::Char('q')).1, Flow::Close);
    }

    #[test]
    fn a_reloaded_list_keeps_the_highlight_on_the_same_row_and_shows_what_the_last_key_said() {
        let mut l = List::new(mounts(), true, OUT);
        press(&mut l, KeyCode::Char('j'));
        // The row above was taken down: the highlight stays on the foreground row
        let after = OUT.replace(r#"{"id":"https:443/","mark":"✕","tone":"bad","text":":443 / → 127.0.0.1:8420","right":"没在听","detail":["https://mac.ts.net/"],"fields":{"url":"https://mac.ts.net/","drop":"https:443/"}},"#, "");
        l.said(true, "已撤 https://mac.ts.net/\n要加回去:tailscale serve --bg --https=443 http://127.0.0.1:8420");
        l.feed(true, &after);
        assert_eq!(l.cur().map(|r| r.id.as_str()), Some("fg:https:9000/"));
        let t = text(&l, 80);
        assert!(t.contains("已撤 https://mac.ts.net/\n要加回去:tailscale serve --bg --https=443 http://127.0.0.1:8420"), "{t}");
        // Results for other sessions or other views aren't its
        let mut other = mounts();
        other.session = "s2".into();
        assert!(!l.takes(&other) && l.takes(&mounts()));
        // A failed run: shown in the warning color, the list keeps the previous copy
        l.feed(false, "tailscale didn't answer");
        assert!(text(&l, 80).contains("tailscale didn't answer") && l.list.rows.len() == 1);
    }
}
