//! The `:` command panel: aca's own actions (the table in acts.rs; picking one = pressing that key on the main list) and the
//! commands enabled plugins expose to the interface (the manifest's `[[commands]]`, called commands in the interface; the
//! ones for agents are called "tools for agents"), panes and views (see `aca_core::plugin::menu`), narrowed by the
//! highlighted row to the ones that make sense here. Diagnostic ones (try it, test connectivity) don't go here; the manifest
//! exposes them to the cli only.
//!
//! - ↑↓ to pick, ⏎ to run; or press the key at the start of a row directly (aca's is the same key as on the main list; keys
//!   the manifest's `[[keys]]` binds for a plugin work only here, and a clash with aca's goes to the plugin); `/` to type a
//!   filter
//! - A command with parameters first pops up a form (`Form`); once it has run, short output goes to the bottom bar, long
//!   output into the full-screen reading page
//! - Views: output goes into the full-screen reading page, `r` reruns. A list view (`rows = true`) opens as a [`List`]
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

/// At most this many per page; beyond that, the list scrolls with the highlight
const SHOWN: usize = 14;
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
fn start(req: Req, fx: &mut Fx) {
    if req.item.kind == ItemKind::Pane && std::env::var_os("TMUX").is_none() {
        fx.external = Some(crate::ui::External::Plugin(req));
        return;
    }
    fx.say(tr!("正在运行 {}…", "running {}…", req.title()));
    fx.plugin = Some(req);
}

/// One row of the panel: an aca action (acts.rs), or one a plugin provides. When a key a plugin binds in `:` clashes with
/// aca's, the key goes to the plugin and the aca row shows no key (shadowed)
enum Entry {
    Act(&'static crate::acts::Act, bool),
    Plugin(MenuItem),
}

impl Entry {
    /// The key at the start of the row; blank if none
    fn key(&self) -> String {
        match self {
            Entry::Act(a, false) => a.key.to_string(),
            Entry::Act(_, true) => String::new(),
            Entry::Plugin(it) => it.key.map_or(String::new(), |k| k.to_string()),
        }
    }

    /// Its name (a plugin one with parameters gets …)
    fn title(&self) -> String {
        match self {
            Entry::Act(a, _) => a.name().to_string(),
            Entry::Plugin(it) => format!("{}{}", it.title, if it.args.is_empty() { "" } else { "…" }),
        }
    }

    /// Right side: whose it is
    fn from(&self) -> String {
        match self {
            Entry::Act(..) => "aca".to_string(),
            Entry::Plugin(it) => it.plugin_name.clone(),
        }
    }

    fn matches(&self, f: &str) -> bool {
        match self {
            Entry::Act(a, _) => [a.key, a.name()].iter().any(|s| s.to_lowercase().contains(f)),
            Entry::Plugin(it) => [&it.title, &it.plugin_name, &it.id, &it.plugin].iter().any(|s| s.to_lowercase().contains(f)),
        }
    }

    /// The key pressed is its direct key
    fn hit(&self, k: KeyEvent) -> bool {
        match self {
            Entry::Act(a, shadowed) => !shadowed && a.hit(k),
            Entry::Plugin(it) => !k.modifiers.contains(KeyModifiers::CONTROL) && it.key.is_some_and(|c| k.code == KeyCode::Char(c)),
        }
    }
}

/// The `:` layer
pub struct Menu {
    items: Vec<Entry>,
    target: Target,
    sel: usize,
    /// The filter text being typed after `/`
    filter: Option<String>,
}

impl Menu {
    /// `all` = everything enabled plugins give the menu; keep only what makes sense on this kind of row, followed by what
    /// aca itself can do on it
    pub fn new(all: Vec<MenuItem>, target: Target) -> Menu {
        let mut items: Vec<Entry> = all.into_iter().filter(|i| i.fits(target.on)).map(Entry::Plugin).collect();
        let bound: Vec<char> = items.iter().filter_map(|e| if let Entry::Plugin(it) = e { it.key } else { None }).collect();
        items.extend(
            crate::acts::ACTS
                .iter()
                .filter(|a| a.fits(target.on))
                .map(|a| Entry::Act(a, a.key.chars().count() == 1 && a.key.chars().any(|c| bound.contains(&c)))),
        );
        Menu { items, target, sel: 0, filter: None }
    }

    #[cfg(test)]
    fn plugin_ids(&self) -> Vec<&str> {
        self.items.iter().filter_map(|e| if let Entry::Plugin(it) = e { Some(it.id.as_str()) } else { None }).collect()
    }

    /// The filtered ones (indices)
    fn shown(&self) -> Vec<usize> {
        let f = self.filter.as_deref().unwrap_or("").to_lowercase();
        (0..self.items.len()).filter(|&i| f.is_empty() || self.items[i].matches(&f)).collect()
    }

    fn pick(&self, i: usize, fx: &mut Fx) -> Flow {
        match &self.items[i] {
            Entry::Act(a, _) => fx.key = a.event(),
            Entry::Plugin(it) if it.kind == ItemKind::Command && !it.args.is_empty() => {
                fx.open = Some(Box::new(Form::new(it.clone(), self.target.clone())));
            }
            Entry::Plugin(it) => start(Req { item: it.clone(), session: self.target.session.clone(), ..Default::default() }, fx),
        }
        Flow::Close
    }

    fn row(&self, it: &Entry, w: usize) -> Line<'static> {
        let key = format!("{:<3}", it.key());
        // The right side only says whose it is: aca's own, or which plugin's (pane / view / command is an implementation
        // split; pressing any of them just "runs it")
        let right = it.from();
        let right_w = right.width();
        let title = kit::clip(&it.title(), w.saturating_sub(right_w + 7)).trim_end().to_string();
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
            "session" => tr!("命令 · 对「{}」", "Commands · for “{}”", self.target.label),
            "group" => tr!("命令 · 对分组 {}", "Commands · for group {}", self.target.label),
            _ => t!("命令", "Commands").to_string(),
        };
        let mut out = vec![kit::modal_title(&on, theme::accent(), kit::esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let shown = self.shown();
        if shown.is_empty() {
            out.extend(wrap(t!("无匹配项", "nothing matches"), kit::dim()));
        }
        let at = shown.iter().position(|&i| i == self.sel).unwrap_or(0);
        let from = at.saturating_sub(SHOWN / 2).min(shown.len().saturating_sub(SHOWN));
        for &i in shown.iter().skip(from).take(SHOWN) {
            out.push(kit::picked(self.row(&self.items[i], w), w, i == self.sel));
        }
        out.push(Line::raw(""));
        if let Some(f) = &self.filter {
            out.push(Line::from(vec![Span::styled(t!("筛选 > ", "filter > "), theme::accent()), Span::raw(f.clone()), Span::raw("▏")]));
            out.push(kit::keys_line(&[("⏎", t!("执行", "run")), ("Esc", t!("清除筛选", "stop filtering"))], vec![], w));
        } else {
            out.push(kit::keys_line(&[("↑↓", t!("选择", "pick")), ("⏎", t!("执行", "run")), ("/", t!("筛选", "filter"))], vec![], w));
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
            // After filtering, the highlight lands on the first match
            let shown = self.shown();
            if !shown.contains(&self.sel) {
                self.sel = shown.first().copied().unwrap_or(0);
            }
            return Flow::Stay;
        }
        match k.code {
            KeyCode::Esc => return Flow::Close,
            KeyCode::Char('q') if !self.items.iter().any(|i| i.hit(k)) => return Flow::Close,
            KeyCode::Enter => return at.map_or(Flow::Stay, |_| self.pick(self.sel, fx)),
            KeyCode::Up => self.sel = step(-1).unwrap_or(0),
            KeyCode::Down => self.sel = step(1).unwrap_or(0),
            KeyCode::Char('/') if !k.modifiers.contains(KeyModifiers::CONTROL) => self.filter = Some(String::new()),
            _ => {
                // Direct keys: plugin-bound ones, aca's; j k, when nobody takes them, move the highlight
                if let Some(i) = self.items.iter().position(|i| i.hit(k)) {
                    return self.pick(i, fx);
                }
                match k.code {
                    KeyCode::Char('j') => self.sel = step(1).unwrap_or(0),
                    KeyCode::Char('k') => self.sel = step(-1).unwrap_or(0),
                    _ => {}
                }
            }
        }
        Flow::Stay
    }

    fn typing(&self) -> bool {
        self.filter.is_some()
    }
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
            keys.push(("r".into(), t!("重新读取", "reload").into()));
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
                fx.say(tr!("正在重新读取 {}…", "reloading {}…", self.req.title()));
                fx.plugin = Some(self.reload_req());
            }
            return Flow::Stay;
        };
        let Some(given) = self.usable(&a) else {
            if self.cur().is_some() || !a.row {
                fx.say(tr!("此行不支持:{}", "not on this row: {}", a.title));
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
    fn the_menu_keeps_what_fits_the_row_and_a_bound_key_runs_at_once() {
        let all = vec![
            item("stat", ItemKind::Command, None, &["session"], &[]),
            item("tidy", ItemKind::Command, None, &["group"], &[]),
            item("ci", ItemKind::View, Some('c'), &[], &[]),
        ];
        let mut m = Menu::new(all, on_session());
        assert_eq!(m.plugin_ids(), ["stat", "ci"]);
        // aca's own actions follow; the plugin binds c, so aca's 「复制回复」 (copy the reply) row shows no key, c goes to the
        // plugin
        let t = text(&m, 70);
        assert!(t.contains("n  新建") && t.contains("aca"), "{t}");
        let shadowed = m.items.iter().find(|e| matches!(e, Entry::Act(a, _) if a.key == "c")).unwrap();
        assert!(shadowed.key().is_empty() && shadowed.title() == "复制回复" && !shadowed.hit(key(KeyCode::Char('c'))));
        let mut fx = Fx::default();
        assert_eq!(m.on_key(key(KeyCode::Char('f')), &mut fx), Flow::Close);
        assert_eq!(fx.key, Some(key(KeyCode::Char('f'))));
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

    #[test]
    fn without_plugins_the_panel_still_lists_acas_own_actions_for_the_row() {
        let t = text(&Menu::new(vec![], on_session()), 70);
        assert!(t.contains("命令 · 对「主线」") && t.contains("n  新建") && t.contains("⏎ 执行"), "{t}");
        // A group row keeps only what can be done on a group: delete is there, fork isn't
        let t = text(&Menu::new(vec![], Target { on: "group", session: String::new(), label: "组".into() }), 70);
        assert!(t.contains("命令 · 对分组 组") && t.contains("d  删除") && !t.contains("分叉"), "{t}");
        // / filter: typing the key finds it too; picking it = pressing that key on the main list
        let mut m = Menu::new(vec![], on_session());
        let mut fx = Fx::default();
        m.on_key(key(KeyCode::Char('/')), &mut fx);
        for c in "^n".chars() {
            m.on_key(key(KeyCode::Char(c)), &mut fx);
        }
        assert_eq!(m.shown().len(), 1);
        assert_eq!(m.on_key(key(KeyCode::Enter), &mut fx), Flow::Close);
        assert_eq!(fx.key, Some(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)));
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
        assert!(t.contains("r 重新读取"), "{t}");
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
    fn row_keys_copy_run_with_the_row_ask_twice_and_hand_the_result_back_to_the_list() {
        let mut l = List::new(mounts(), true, OUT);
        assert_eq!(press(&mut l, KeyCode::Enter).0.copy.as_deref(), Some("https://mac.ts.net/"));
        // x: the first press only warns, the second runs drop with this row; the result goes back to this list (reload
        // only, no new page)
        let (fx, _) = press(&mut l, KeyCode::Char('x'));
        assert!(fx.plugin.is_none() && text(&l, 80).contains("再按 x:撤掉"));
        let req = press(&mut l, KeyCode::Char('x')).0.plugin.expect("跑了");
        assert_eq!(
            (req.item.id.as_str(), req.args.clone(), req.session.as_str()),
            ("drop", vec![("target".into(), "https:443/".into())], "s1")
        );
        let then = req.then.expect("交回列表");
        assert!(then.refresh && l.takes(&then) && then.item.id == "mounts");
        // Pressing something else in between doesn't count
        press(&mut l, KeyCode::Char('x'));
        press(&mut l, KeyCode::Char('k'));
        assert!(press(&mut l, KeyCode::Char('x')).0.plugin.is_none());
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
        // r (not bound by the plugin): reloads this list
        let req = press(&mut l, KeyCode::Char('r')).0.plugin.unwrap();
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
