//! Plugin manager: `P` on the list opens this page (like a vim plugin manager, lazy.nvim's `:Lazy`). The box has a fixed size
//! (most of the screen on a desktop). On the left, one plugin per row (on ● / off ○ / broken ✕, name), with aca's bundled ones not
//! yet installed listed below under "Available"; on the right, details of the highlighted one: name and version, where it's from
//! (a word or two), on or off, description, and below that what can be changed (settings, tools for the agent, keys in the plugin
//! menu, see plugtune.rs).
//! On a narrow screen (phone) they stack: list on top, details below.
//!
//! - ⏎ goes into the right column to change things (Esc back to the list); ⏎ on an available one installs it; `m` on the gateway
//!   row manages the MCP servers under it (gwpage.rs)
//! - `i` installs one: type a git URL, `owner/repo` (GitHub), a local directory (local means linked), or a bundled `aca.<name>`, ⏎
//!   to install
//! - Space turns on / off: an off plugin's hooks don't run and the agent doesn't get its things (the agent changes on its next start)
//! - `u` updates this one, `U` updates all: git installs pull (fast-forward only); pinned and linked ones stay; bundled ones follow aca
//! - `x` twice removes: git and bundled installs are deleted with their directory, linked ones are only unregistered; the plugin's
//!   own config and state stay
//! - `e` opens `plugins.toml` in the editor at the highlighted plugin's section; syncs by itself on return (only if changed).
//!   For your own one (`~/.config/aca/user/`) it opens its manifest
//! - When plugins.toml doesn't match this machine (changed elsewhere, pulled in with dotfiles), the bottom says where it differs,
//!   `S` syncs to it; nothing is said when it matches
//! - `l` shows its log (event hook output)
//!
//! Install, update, remove, on / off run in the background (`Job::Plugins`, see `aca_core::plugin`); the page reloads when done.
//! The bottom flags which open sessions started with something different from now (need a restart to pick it up, see
//! `mount::stale`): "takes effect after a restart" is said only here, not elsewhere (section titles, done messages).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::plugin;
use aca_core::{t, tr};

use crate::ops::{Job, Outcome};
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::plugtune::{Back, Detail, Tune};
use crate::theme;

/// Longest source that can be typed
const INPUT_MAX: usize = 300;
/// Most lines the highlighted setting's description takes
const HINT_MOST: usize = 6;
/// Least height of the box (rows, not counting the frame and the sentence below)
const LEAST_ROWS: usize = 20;

/// Where it's from
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// A linked local directory
    #[default]
    Linked,
    Git,
    /// Bundled with aca
    Bundled,
    /// Your own one (`~/.config/aca/user/`)
    Mine,
}

/// A registered plugin
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub id: String,
    pub enabled: bool,
    /// Name, version, description from the manifest; empty when the manifest can't be read, with the reason in `problem`
    pub name: String,
    pub version: String,
    pub description: String,
    pub problem: Option<String>,
    pub from: Origin,
    pub path: String,
    /// Programs in the manifest's needs that this machine lacks (marked "missing" in the list, the right side says which)
    pub lacks: Vec<String>,
    /// What's failing (a hook or a schedule didn't run last time, the gateway's server stopped), one line each: marked "failing" in the
    /// list, listed on the right
    pub alerts: Vec<String>,
    /// The gateway plugin: whether it's running now, how many connections, how many servers under it
    pub live: Option<String>,
    /// Bundled with aca, not installed yet: listed under "Available", ⏎ installs
    pub available: bool,
    /// Not installed yet: what installing it brings (as the manifest says, see `Manifest::brings`)
    pub brings: Vec<String>,
}

impl Origin {
    /// Where it's from, a word or two (in the right side's title line, after the version). Not written for your own one: its title is
    /// 「你自己的」 ("Your own")
    fn word(self) -> &'static str {
        match self {
            Origin::Linked => t!("本地", "local"),
            Origin::Git => "git",
            Origin::Bundled => t!("自带", "built-in"),
            Origin::Mine => "",
        }
    }
}

/// What the status bar calls it: the manifest's name (your own one is 「你自己的」 ("Your own")); the id when unreadable
/// (manifest broken, just removed).
/// When item is given: the title of one of its items (capability, pane, view)
fn called(id: &str, item: Option<&str>) -> String {
    let e = plugin::registry().into_iter().find(|e| e.id == id);
    if item.is_none() && e.as_ref().is_some_and(|e| e.source == plugin::user::SOURCE) {
        return t!("你自己的", "your own").into();
    }
    let m = e.and_then(|e| plugin::read_manifest(Path::new(&e.path)).ok()).map(|(m, _)| m);
    let found = match (&m, item) {
        (Some(m), None) => Some(m.name.clone()),
        (Some(m), Some(x)) => m
            .actions
            .iter()
            .find(|a| a.id == x)
            .map(|a| a.title.clone())
            .or_else(|| m.panes.iter().find(|p| p.id == x).map(|p| p.title.clone()))
            .or_else(|| m.views.iter().find(|v| v.id == x).map(|v| v.title.clone())),
        (None, _) => None,
    };
    found.filter(|n| !n.is_empty()).unwrap_or_else(|| item.unwrap_or(id).to_string())
}

/// Plugins in the registry, with their manifests read
pub fn load() -> Vec<Row> {
    // aca was just upgraded: swap the bundled ones for the new ones first, so this page lists what's current
    plugin::first_party::refresh();
    let (problems, now) = (plugin::health::problems(), aca_core::paths::now());
    plugin::registry()
        .into_iter()
        .map(|e| {
            let from = match e.source.as_str() {
                "" => Origin::Linked,
                plugin::first_party::SOURCE => Origin::Bundled,
                plugin::user::SOURCE => Origin::Mine,
                _ => Origin::Git,
            };
            let mut r = Row { id: e.id.clone(), enabled: e.enabled, from, path: e.path.clone(), ..Default::default() };
            match plugin::read_manifest(Path::new(&e.path)) {
                Ok((m, _)) => {
                    if e.id == aca_core::gateway::PLUGIN {
                        r.live = gateway_live(e.enabled);
                    }
                    r.lacks = m.missing();
                    r.alerts = problems.iter().filter(|p| p.plugin == e.id).map(|p| p.text(now)).collect();
                    (r.name, r.version, r.description) = (m.name, m.version, m.description);
                    if from == Origin::Mine && r.description.is_empty() {
                        r.description = t!("你自己写的命令、窗格、视图", "commands, panes and views you write yourself").into();
                    }
                }
                Err(err) => r.problem = Some(err),
            }
            r
        })
        .chain(plugin::first_party::available().into_iter().filter_map(|id| {
            let m = plugin::first_party::manifest(id)?;
            Some(Row {
                id: id.to_string(),
                from: Origin::Bundled,
                available: true,
                brings: m.brings(),
                name: m.name,
                version: m.version,
                description: m.description,
                ..Default::default()
            })
        }))
        .collect()
}

/// The gateway's state now (one line): ask the running one; not running with the plugin on is idle (starts itself when the first
/// agent connects), not written when off
fn gateway_live(on: bool) -> Option<String> {
    match aca_core::gateway::ctl(serde_json::json!({ "cmd": "status" })) {
        Ok(v) => Some(tr!(
            "运行中 · {} 个连接 · {} 个服务",
            "running · {} connections · {} servers",
            v["clients"].as_array().map_or(0, Vec::len),
            v["servers"].as_array().map_or(0, Vec::len)
        )),
        Err(_) if on => Some(t!("空闲", "idle").into()),
        Err(_) => None,
    }
}

/// Things to run in the background
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Install from here (git URL, owner/repo, local directory, aca.<name>)
    Install(String),
    /// Update this one; None = all (linked ones excluded)
    Update(Option<String>),
    Remove(String),
    Enable(String, bool),
    /// Bring this machine in line with plugins.toml
    Sync,
    /// Back from editing plugins.toml in the editor: sync to it and say what changed
    AfterEdit,
    /// Give / don't give the agent a capability as a tool: (plugin, capability, give or not)
    Tool(String, String, bool),
    /// A key in the `:` menu: (plugin, capability / pane / view, Some(key) / Some("") no key / None back to the manifest's)
    Key(String, String, Option<String>),
    /// Change a setting: (plugin, name, Some(value) / None back to the default)
    Setting(String, String, Option<String>),
}

impl Act {
    /// What the status bar shows while it runs
    pub fn doing(&self) -> String {
        match self {
            // Ones under "Available" come as an id: use the manifest's name; others (URL, directory) are what you typed, written as is
            Act::Install(src) => {
                tr!("正在安装 {}…", "installing {}…", plugin::first_party::manifest(src).map_or_else(|| src.clone(), |m| m.name))
            }
            Act::Update(Some(id)) => tr!("正在更新 {}…", "updating {}…", called(id, None)),
            Act::Update(None) => t!("正在更新插件…", "updating plugins…").into(),
            Act::Remove(id) => tr!("正在删除 {}…", "removing {}…", called(id, None)),
            Act::Enable(id, true) => tr!("正在开启 {}…", "switching on {}…", called(id, None)),
            Act::Enable(id, false) => tr!("正在关闭 {}…", "switching off {}…", called(id, None)),
            Act::Sync | Act::AfterEdit => t!("正在按 plugins.toml 同步…", "syncing with plugins.toml…").into(),
            Act::Tool(id, tool, true) => tr!("正在将「{}」提供给 agent…", "giving agents “{}”…", called(id, Some(tool))),
            Act::Tool(id, tool, false) => tr!("正在收回「{}」…", "withholding “{}”…", called(id, Some(tool))),
            Act::Key(id, target, _) => tr!("正在修改「{}」的按键…", "changing the key for “{}”…", called(id, Some(target))),
            Act::Setting(_, name, _) => tr!("正在修改 {name}…", "changing {name}…"),
        }
    }
}

/// What the run loop puts in the background (`Job::Plugins`)
pub fn job(a: &Act) -> Outcome {
    match a {
        Act::Install(src) => match plugin::install(src, None) {
            Ok((m, warn)) => {
                let warn = if warn.is_empty() { String::new() } else { tr!(" · 提醒:{}", " · note: {}", warn.join(t!(";", "; "))) };
                Outcome::Done(tr!("已安装 {} {}{warn}", "installed {} {}{warn}", m.name, m.version))
            }
            Err(e) => Outcome::Failed(tr!("安装失败:{e}", "not installed: {e}")),
        },
        Act::Update(one) => {
            let ids: Vec<String> = match one {
                Some(id) => vec![id.clone()],
                None => plugin::registry().into_iter().filter(|e| !e.source.is_empty()).map(|e| e.id).collect(),
            };
            if ids.is_empty() {
                return Outcome::Done(t!("没有要更新的插件", "nothing to update").into());
            }
            let said: Vec<String> = ids.iter().map(|id| plugin::update(id).unwrap_or_else(|e| format!("{id}: {e}"))).collect();
            Outcome::Done(said.join(" · "))
        }
        Act::Remove(id) => plugin::remove(id).map_or_else(|e| Outcome::Failed(tr!("删除失败:{e}", "not removed: {e}")), Outcome::Done),
        Act::Enable(id, on) => match plugin::enable(id, *on) {
            Ok(()) if *on => Outcome::Done(tr!("已开启 {}", "switched on {}", called(id, None))),
            Ok(()) => Outcome::Done(tr!("已关闭 {}", "switched off {}", called(id, None))),
            Err(e) => Outcome::Failed(e),
        },
        Act::Sync => match plugin::sync() {
            Ok(said) if said.is_empty() => Outcome::Done(t!("与 plugins.toml 一致", "matches plugins.toml").into()),
            Ok(said) => Outcome::Done(said.join(" · ")),
            Err(e) => Outcome::Failed(tr!("同步失败:{e}", "not synced: {e}")),
        },
        Act::AfterEdit => match plugin::sync() {
            Ok(said) if said.is_empty() => Outcome::Done(t!("已修改", "saved").into()),
            Ok(said) => Outcome::Done(tr!("已修改:{}", "saved: {}", said.join(" · "))),
            Err(e) => Outcome::Failed(tr!("plugins.toml 有问题:{e} · e 再改", "plugins.toml has a problem: {e} · e to fix it")),
        },
        Act::Tool(id, tool, on) => match plugin::yours::allow(id, tool, *on) {
            Ok(_) if *on => Outcome::Done(tr!("已将「{}」提供给 agent", "agents get “{}”", called(id, Some(tool)))),
            Ok(_) => Outcome::Done(tr!("已停止向 agent 提供「{}」", "agents no longer get “{}”", called(id, Some(tool)))),
            Err(e) => Outcome::Failed(e),
        },
        Act::Key(id, target, key) => match plugin::yours::set_key(id, target, key.as_deref()) {
            Ok(()) => {
                let now = plugin::menu().into_iter().find(|i| &i.plugin == id && &i.id == target).and_then(|i| i.key);
                let what = called(id, Some(target));
                Outcome::Done(match now {
                    Some(k) => tr!("「{what}」的键:{k}", "“{what}”: key {k}"),
                    None => tr!("「{what}」已无按键", "“{what}” has no key now"),
                })
            }
            Err(e) => Outcome::Failed(e),
        },
        Act::Setting(id, name, value) => match plugin::set_setting(id, name, value.as_deref()) {
            Ok(()) if value.is_some() => Outcome::Done(tr!("{name} 已修改", "{name} saved")),
            Ok(()) => Outcome::Done(tr!("{name} 已恢复默认", "{name} is back to its default")),
            Err(e) => Outcome::Failed(e),
        },
    }
}

/// The file e opened and its mtime before opening: sync on return only if changed
#[derive(Debug)]
pub struct Edited {
    path: PathBuf,
    before: Option<SystemTime>,
    /// It's plugins.toml (sync if changed); your own manifest takes effect at once when changed, no sync needed
    sync: bool,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// e: open plugins.toml in the editor at this plugin's section (at the end if there's none); with `mine` (your own one) open its
/// manifest
pub fn edit_file(id: Option<&str>, mine: Option<&str>, fx: &mut Fx) -> Option<Edited> {
    let (path, line, sync) = match mine {
        Some(p) => (PathBuf::from(p), 1, false),
        None => {
            if let Err(e) = plugin::decl::ensure() {
                fx.say(e);
                return None;
            }
            (plugin::decl::decl_file(), id.map_or(1, plugin::decl::line_of), true)
        }
    };
    let before = mtime(&path);
    fx.external = Some(crate::ui::External::Edit { path: path.to_string_lossy().into_owned(), line });
    Some(Edited { path, before, sync })
}

/// Back from the editor: say nothing if unchanged; sync if plugins.toml changed
pub fn back_from(ed: Edited, fx: &mut Fx) {
    match (mtime(&ed.path) != ed.before, ed.sync) {
        (false, _) => {}
        (true, true) => fx.job = Some(Job::Plugins(Act::AfterEdit)),
        (true, false) => fx.say(t!("已修改", "saved")),
    }
}

/// l: show its log (event hook output); name is what the status bar calls it when there's no log
pub fn show_log(id: &str, name: &str, fx: &mut Fx) {
    let log = plugin::log_file(id);
    if log.exists() {
        fx.external = Some(crate::ui::External::View { path: log.to_string_lossy().into_owned(), line: 1, temp: false });
    } else {
        fx.say(tr!("{name} 暂无日志", "{name} has no log yet"));
    }
}

/// Which column the highlight is in
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    List,
    Detail,
}

/// Narrowest width that fits two columns; narrower (phone) stacks them
pub(crate) const SPLIT_W: usize = 76;
/// Divider between the two columns
pub(crate) const DIVIDER: &str = " │  ";

/// The `P` page
pub struct Page {
    load: fn() -> Vec<Row>,
    rows: Vec<Row>,
    sel: usize,
    focus: Focus,
    /// Details of the highlighted plugin (the right column); none for available ones or ones with a broken manifest
    detail: Option<Detail>,
    /// x pressed once, waiting for the second press
    armed: bool,
    /// The source being typed for installing
    typing: Option<String>,
    /// Open sessions that need a restart to pick up plugin changes: titles
    stale: Vec<String>,
    stale_of: fn() -> Vec<String>,
    /// Where plugins.toml doesn't match this machine
    drift: Vec<String>,
    drift_of: fn() -> Vec<String>,
    /// Reads one plugin's details (faked in tests)
    tune: fn(&str) -> Tune,
    /// e opened a file: sync on return if it changed
    edited: Option<Edited>,
    /// Most lines the right column needs (desktop width, the longest of all plugins, see `measure`): for sizing the box height
    tall: usize,
}

/// Open sessions that started with something different from now (titles)
pub fn stale() -> Vec<String> {
    plugin::mount::stale().into_iter().map(|(_, title)| title).collect()
}

/// What the list and the right-side title call it: the manifest's name; your own one is 「你自己的」 ("Your own")
fn title(r: &Row) -> String {
    match r.from {
        Origin::Mine => t!("你自己的", "Your own").to_string(),
        _ if r.name.is_empty() => r.id.clone(),
        _ => r.name.clone(),
    }
}

/// Split a line into spans, putting the whole line's style (background, weight) on each: when two columns join into one line, the
/// left one's background mustn't spread to the right
pub(crate) fn flat(line: Line<'static>) -> Vec<Span<'static>> {
    let st = line.style;
    line.spans.into_iter().map(|s| Span::styled(s.content, st.patch(s.style))).collect()
}

/// Pad with spaces to width w (for joining two columns into one line), carrying the whole line's style
pub(crate) fn pad(line: Line<'static>, w: usize) -> Vec<Span<'static>> {
    let (st, n) = (line.style, w.saturating_sub(line.width()));
    let mut spans = flat(line);
    if n > 0 {
        spans.push(Span::styled(" ".repeat(n), st));
    }
    spans
}

/// Which line to start showing from so line at lands within h lines (placed high, leaving some visible below)
pub(crate) fn scroll(at: Option<usize>, len: usize, h: usize) -> usize {
    match at {
        Some(i) if len > h => i.saturating_sub(h / 2).min(len - h),
        _ => 0,
    }
}

/// Most lines the description takes when the highlight is on a setting (plus a blank line before): the longest one counts when sizing
/// the box height
fn hint_tall(d: &Detail, w: usize) -> usize {
    d.data
        .settings
        .iter()
        .filter(|s| !s.desc.is_empty())
        .map(|s| 1 + crate::ui::wrap_width(&tr!("{}:{}", "{}: {}", s.name, s.desc), w).len().min(HINT_MOST))
        .max()
        .unwrap_or(0)
}

impl Page {
    pub fn new(load: fn() -> Vec<Row>) -> Page {
        Page::with(load, stale, plugin::drift, crate::plugtune::load)
    }

    pub fn with(load: fn() -> Vec<Row>, stale_of: fn() -> Vec<String>, drift_of: fn() -> Vec<String>, tune: fn(&str) -> Tune) -> Page {
        let mut p = Page {
            load,
            rows: vec![],
            sel: 0,
            focus: Focus::List,
            detail: None,
            armed: false,
            typing: None,
            stale: vec![],
            stale_of,
            drift: vec![],
            drift_of,
            tune,
            edited: None,
            tall: 0,
        };
        p.reload();
        p
    }

    fn cur(&self) -> Option<&Row> {
        self.rows.get(self.sel)
    }

    /// Width of each column: the left by the longest name, the rest to the right
    fn split(&self, w: usize) -> (usize, usize) {
        let names = self.rows.iter().map(|r| title(r).width()).max().unwrap_or(0);
        let lw = (names + 4 + 5).clamp(22, 32);
        (lw, w.saturating_sub(lw + DIVIDER.width()))
    }

    /// Most lines the right column needs: computed for every plugin (the highlighted one from its loaded details, others read now), the
    /// longest wins. For sizing the box height, so moving the highlight doesn't change it
    fn measure(&mut self) {
        let (_, rw) = self.split(self.width() as usize - 6);
        let mut tall = 0;
        for (i, r) in self.rows.iter().enumerate() {
            let fresh;
            let d = match (r.available || r.problem.is_some(), i == self.sel) {
                (true, _) => None,
                (false, true) => self.detail.as_ref(),
                (false, false) => {
                    fresh = Detail::new((self.tune)(&r.id));
                    Some(&fresh)
                }
            };
            tall = tall.max(self.right_of(r, d, rw).0.len() + d.map_or(0, |d| hint_tall(d, rw)));
        }
        self.tall = tall;
    }

    /// Which row the "Available" section starts at
    fn first_available(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.available)
    }

    /// Highlight moved or reloaded: the right column switches to the highlighted one; for the same plugin keep the highlight inside it
    fn follow(&mut self) {
        let want = self.cur().filter(|r| !r.available && r.problem.is_none()).map(|r| r.id.clone());
        match want {
            Some(id) => {
                let data = (self.tune)(&id);
                match &mut self.detail {
                    Some(d) if d.data.id == id => d.swap(data),
                    _ => self.detail = Some(Detail::new(data)),
                }
            }
            None => self.detail = None,
        }
        if self.detail.as_ref().is_none_or(|d| d.rows() == 0) {
            self.focus = Focus::List;
        }
    }

    fn edit(&mut self, fx: &mut Fx) {
        let (id, mine) = match self.cur() {
            Some(r) if r.from == Origin::Mine => (Some(r.id.clone()), Some(format!("{}/{}", r.path, plugin::MANIFEST))),
            Some(r) => (Some(r.id.clone()), None),
            None => (None, None),
        };
        self.edited = edit_file(id.as_deref(), mine.as_deref(), fx);
    }

    /// The left list: one per row (on ● / off ○ / broken ✕, name), "Available" ones below. Also returns which row is highlighted
    fn list(&self, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let mut out = vec![];
        let mut at = None;
        if self.first_available() == Some(0) {
            out.push(Line::styled(t!("尚未安装插件", "nothing installed yet"), kit::dim()));
        }
        for (i, r) in self.rows.iter().enumerate() {
            if Some(i) == self.first_available() {
                if !out.is_empty() {
                    out.push(Line::raw(""));
                }
                out.push(Line::styled(t!("可安装", "Available"), kit::dim().add_modifier(Modifier::BOLD)));
            }
            let on = i == self.sel;
            if on {
                at = Some(out.len());
            }
            let (mark, mark_st) = match (r.available, &r.problem, r.enabled) {
                (true, ..) => ("+", theme::accent()),
                (_, Some(_), _) => ("✕", theme::warn()),
                (_, None, true) => ("●", theme::accent()),
                (_, None, false) => ("○", kit::dim()),
            };
            let (tag, tag_st) = match (r.available, &r.problem, r.enabled) {
                (true, ..) => ("", kit::dim()),
                (_, Some(_), _) => (t!("有问题", "broken"), theme::warn()),
                _ if !r.alerts.is_empty() && r.enabled => (t!("出错", "failing"), theme::warn()),
                _ if !r.lacks.is_empty() => (t!("缺少程序", "missing"), theme::warn()),
                _ => ("", kit::dim()),
            };
            let name_st = match (r.available, r.enabled) {
                (true, _) => Style::new(),
                (false, true) => Style::new().add_modifier(Modifier::BOLD),
                (false, false) => kit::dim(),
            };
            let bar = if on { Span::styled("▌", theme::accent()) } else { Span::raw(" ") };
            let room = w.saturating_sub(4 + if tag.is_empty() { 0 } else { tag.width() + 1 });
            let name = kit::clip(&title(r), room).trim_end().to_string();
            let line = kit::spread(
                vec![bar, Span::styled(mark, mark_st), Span::raw(" "), Span::styled(name, name_st)],
                vec![Span::styled(tag.to_string(), tag_st)],
                w,
            );
            out.push(kit::picked(line, w, on && self.focus == Focus::List));
        }
        (out, at)
    }

    /// The right-side title: name (accent bold), the faint version and where it's from (a word or two), on / off at the right
    fn header(r: &Row, w: usize) -> Line<'static> {
        let (state, st) = match (r.available, &r.problem, r.enabled) {
            (true, ..) => (t!("未安装", "not installed"), kit::dim()),
            (_, Some(_), _) => (t!("✕ 清单有问题", "✕ manifest problem"), theme::warn()),
            (_, None, true) => (t!("● 已开启", "● on"), theme::accent()),
            (_, None, false) => (t!("○ 已关闭", "○ off"), kit::dim()),
        };
        let room = w.saturating_sub(state.width() + 2);
        let name = kit::clip(&title(r), room).trim_end().to_string();
        let ver = if r.from == Origin::Mine { "" } else { r.version.as_str() };
        let facts: Vec<&str> = [ver, r.from.word()].into_iter().filter(|x| !x.is_empty()).collect();
        let facts = if facts.is_empty() { String::new() } else { format!("  {}", facts.join(" · ")) };
        let facts = kit::clip(&facts, room.saturating_sub(name.width())).trim_end().to_string();
        kit::spread(
            vec![Span::styled(name, theme::accent().add_modifier(Modifier::BOLD)), Span::styled(facts, kit::dim())],
            vec![Span::styled(state.to_string(), st)],
            w,
        )
    }

    /// The right column: what the highlighted plugin is and what can be changed. Also returns which row is highlighted
    fn right(&self, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let wrap = |text: &str, st: Style, most: usize| {
            crate::ui::wrap_width(text, w).into_iter().take(most).map(move |l| Line::styled(l, st)).collect::<Vec<_>>()
        };
        let mut out = vec![];
        if let Some(text) = &self.typing {
            out.push(Line::styled(t!("从其他来源安装", "Install from elsewhere"), theme::accent().add_modifier(Modifier::BOLD)));
            out.push(Line::raw(""));
            out.push(Line::from(vec![
                Span::styled(t!("安装来源 > ", "install from > "), theme::accent()),
                Span::raw(kit::clip(text, w.saturating_sub(12)).trim_end().to_string()),
                Span::raw("▏"),
            ]));
            out.push(Line::raw(""));
            out.extend(wrap(t!("git 地址、owner/repo 或本地目录", "a git URL, owner/repo or a local directory"), kit::dim(), 2));
            return (out, None);
        }
        let Some(r) = self.cur() else {
            out.extend(wrap(t!("尚未安装插件 · i 安装", "No plugins yet · i installs one"), Style::new(), 2));
            return (out, None);
        };
        self.right_of(r, self.detail.as_ref(), w)
    }

    /// How one plugin looks in the right column: title, problems, description (three lines at most), what can be changed (detail);
    /// computed for every plugin when sizing the box height
    fn right_of(&self, r: &Row, detail: Option<&Detail>, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let wrap = |text: &str, st: Style, most: usize| {
            crate::ui::wrap_width(text, w).into_iter().take(most).map(move |l| Line::styled(l, st)).collect::<Vec<_>>()
        };
        let mut out = vec![Self::header(r, w)];
        out.push(Line::raw(""));
        if let Some(p) = &r.problem {
            out.extend(wrap(&tr!("清单有问题:{p}", "manifest problem: {p}"), theme::warn(), 4));
            out.push(Line::raw(""));
        }
        if !r.alerts.is_empty() {
            out.push(Line::styled(t!("出错", "Failing"), theme::warn().add_modifier(Modifier::BOLD)));
            for a in r.alerts.iter().take(4) {
                out.extend(wrap(a, theme::warn(), 2));
            }
            out.push(Line::raw(""));
        }
        if !r.description.is_empty() {
            out.extend(wrap(&r.description, Style::new(), 3));
            out.push(Line::raw(""));
        }
        if r.available {
            if !r.brings.is_empty() {
                out.push(Line::styled(t!("安装后将添加", "Brings"), Style::new().add_modifier(Modifier::BOLD)));
                let fields: Vec<(&str, &str)> = r.brings.iter().map(|b| b.split_once([':', ':']).unwrap_or(("", b))).collect();
                let label_w = fields.iter().map(|(l, _)| l.width()).max().unwrap_or(0) + 2;
                for (l, v) in fields {
                    let v = kit::clip(v, w.saturating_sub(label_w)).trim_end().to_string();
                    out.push(Line::from(vec![Span::styled(format!("{l}{}", " ".repeat(label_w - l.width())), kit::dim()), Span::raw(v)]));
                }
                out.push(Line::raw(""));
            }
            out.push(Line::from(kit::button(t!("⏎ 安装", "⏎ install"), theme::pal().accent)));
            return (out, None);
        }
        // gateway: whether it's running now, written before what can be changed (its MCP servers are managed with m, shown in the key bar)
        if let Some(l) = &r.live {
            let label = t!("状态", "state");
            out.push(Line::from(vec![
                Span::styled(format!("{label}  "), kit::dim()),
                Span::raw(kit::clip(l, w.saturating_sub(label.width() + 2)).trim_end().to_string()),
            ]));
            out.push(Line::raw(""));
        }
        let mut hl = None;
        if let Some(d) = detail {
            let (body, at) = d.body(w, self.focus == Focus::Detail);
            hl = at.map(|i| i + out.len());
            out.extend(body);
        }
        (out, hl)
    }

    /// Highlight on a setting in the right column: "name: description" wrapped in full, below the details (with a blank line before, see
    /// `right_in`); at most this many lines
    fn hint(&self, w: usize) -> Vec<Line<'static>> {
        let Some((name, desc)) = self.detail.as_ref().filter(|_| self.focus == Focus::Detail).and_then(Detail::hint) else {
            return vec![];
        };
        let mut out = vec![Line::raw("")];
        let text = tr!("{name}:{desc}", "{name}: {desc}");
        out.extend(crate::ui::wrap_width(&text, w).into_iter().take(HINT_MOST).map(|l| Line::styled(l, kit::dim())));
        out
    }

    /// Fit the right column into h lines: the highlighted setting's description follows the details; if it doesn't fit, pin it at the
    /// bottom and scroll the details with the highlight
    fn right_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let hint = self.hint(w);
        let (right, hl) = self.right(w);
        let mut out: Vec<Line<'static>> = if right.len() + hint.len() <= h {
            right
        } else {
            let rh = h.saturating_sub(hint.len());
            let ro = scroll(hl, right.len(), rh);
            let mut body: Vec<Line<'static>> = right.into_iter().skip(ro).take(rh).collect();
            body.resize(rh, Line::raw(""));
            body
        };
        out.extend(hint);
        out.resize(h, Line::raw(""));
        out
    }

    /// Notices at the bottom (confirm before removing, plugins.toml mismatch, sessions needing a restart), each two lines at most
    fn banners(&self, w: usize) -> Vec<Line<'static>> {
        let wrap =
            |text: &str| crate::ui::wrap_width(text, w).into_iter().take(2).map(|l| Line::styled(l, theme::warn())).collect::<Vec<_>>();
        let mut out = vec![];
        if let Some(r) = self.cur().filter(|_| self.armed) {
            let what = match r.from {
                Origin::Linked => t!("仅取消登记,保留目录", "only unregisters it; the directory stays"),
                Origin::Git => t!("同时删除克隆的目录", "the cloned directory goes too"),
                Origin::Bundled | Origin::Mine => "",
            };
            let ask = tr!("删除 {}?", "remove {}?", title(r));
            out.extend(wrap(&if what.is_empty() { ask } else { tr!("{ask}{what}", "{ask} {what}") }));
        }
        if !self.drift.is_empty() {
            out.extend(wrap(&tr!(
                "plugins.toml 与本机不一致:{} · S 同步",
                "plugins.toml doesn't match this machine: {} · S syncs",
                self.drift.join(t!("、", ", "))
            )));
        }
        // Plugins changed (on / off, tools for the agent), open sessions need a restart to pick it up: said only here, once, naming which
        if !self.stale.is_empty() {
            let names: Vec<String> = self.stale.iter().take(3).map(|t| format!("「{t}」")).collect();
            let (n, rest) = (self.stale.len(), self.stale.len().saturating_sub(3));
            let more = if rest > 0 { tr!("等 {n} 个", " and {rest} more") } else { String::new() };
            out.extend(wrap(&tr!("{}{more}重启后启用新插件", "restart {}{more} to pick up the plugin changes", names.join(t!("", ", ")))));
        }
        out
    }

    /// The key row at the bottom: follows which column the highlight is in and what's highlighted
    fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.typing.is_some() {
            return vec![("⏎", t!("安装", "install")), ("^U", t!("清空", "clear")), ("Esc", t!("取消", "cancel"))];
        }
        if self.armed {
            return vec![("x", t!("删除", "remove")), ("Esc", t!("取消", "keep"))];
        }
        if let (Focus::Detail, Some(d)) = (self.focus, &self.detail) {
            let mut keys = d.keys();
            if !d.typing() {
                keys.push(("e", t!("编辑", "edit")));
            }
            return keys;
        }
        match self.cur() {
            None => vec![("i", t!("从其他来源安装", "install other")), ("e", t!("编辑", "edit"))],
            Some(r) if r.available => {
                vec![("⏎", t!("安装", "install")), ("i", t!("从其他来源安装", "install other")), ("e", t!("编辑", "edit"))]
            }
            Some(r) => {
                let mut keys = vec![];
                if self.detail.as_ref().is_some_and(|d| d.rows() > 0) {
                    keys.push(("⏎", t!("进入", "change")));
                }
                if r.id == aca_core::gateway::PLUGIN {
                    keys.push(("m", t!("管理 MCP", "MCPs")));
                }
                keys.extend([
                    (t!("空格", "space"), t!("开/关", "on/off")),
                    ("i", t!("从其他来源安装", "install other")),
                    ("u", t!("更新", "update")),
                    ("x", t!("删除", "remove")),
                    ("e", t!("编辑", "edit")),
                ]);
                if self.detail.as_ref().is_some_and(|d| d.data.log) {
                    keys.push(("l", t!("日志", "log")));
                }
                keys
            }
        }
    }
}

impl Overlay for Page {
    fn width(&self) -> u16 {
        120
    }

    fn fill(&self) -> bool {
        true
    }

    /// Height follows the content: title, the longer of the two columns (the right by the longest of all plugins, see `measure`),
    /// notices, rule, keys. Moving the highlight doesn't change it, only installing / removing plugins does; a bit taller while the
    /// two-line remove question from x is up
    fn fill_rows(&self) -> Option<u16> {
        let w = self.width() as usize - 6;
        let (lw, _) = self.split(w);
        let body = self.list(lw).0.len().max(self.tall);
        Some((2 + body + self.banners(w).len() + 2).max(LEAST_ROWS) as u16)
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        self.lines_in(w, 32)
    }

    /// Title, two columns (stacked on a narrow screen), notices, rule, keys; exactly h lines, the box size doesn't follow the highlight
    fn lines_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let in_detail = self.focus == Focus::Detail;
        let esc = if in_detail { t!("返回列表", "back to the list") } else { t!("关闭", "close") };
        let mut out = vec![kit::modal_title(t!("插件", "Plugins"), theme::accent(), kit::esc_hint(esc), w), Line::raw("")];
        let mut foot = self.banners(w);
        foot.push(Line::styled("─".repeat(w), theme::faint()));
        foot.push(kit::keys_line(&self.keys(), vec![], w));
        let bh = h.saturating_sub(out.len() + foot.len());
        if w >= SPLIT_W {
            let (lw, rw) = self.split(w);
            let (left, at) = self.list(lw);
            let right = self.right_in(rw, bh);
            let lo = scroll(at, left.len(), bh);
            for i in 0..bh {
                let l = left.get(lo + i).cloned().unwrap_or_default();
                let r = right.get(i).cloned().unwrap_or_default();
                let mut spans = pad(l, lw);
                spans.push(Span::styled(DIVIDER, theme::faint()));
                spans.extend(flat(r));
                out.push(Line::from(spans));
            }
        } else {
            // Phone: list on top, the highlighted one's details below; inside the details only the details show
            let mut body = vec![];
            if !in_detail {
                let (left, at) = self.list(w);
                let lh = left.len().min((bh * 2 / 5).max(4)).min(bh);
                let lo = scroll(at, left.len(), lh);
                body.extend(left.into_iter().skip(lo).take(lh));
                body.push(Line::styled("┄".repeat(w), theme::faint()));
            }
            let room = bh.saturating_sub(body.len());
            body.extend(self.right_in(w, room));
            body.resize(bh, Line::raw(""));
            out.extend(body);
        }
        out.extend(foot);
        out.truncate(h);
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        if let Some(text) = &mut self.typing {
            match k.code {
                KeyCode::Esc => self.typing = None,
                KeyCode::Enter => {
                    let src = text.trim().to_string();
                    self.typing = None;
                    if !src.is_empty() {
                        fx.job = Some(Job::Plugins(Act::Install(src)));
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) && text.chars().count() < INPUT_MAX => text.push(c),
                _ => {}
            }
            return Flow::Stay;
        }
        // Highlight in the right column: keys go to it; e and l still work
        if self.focus == Focus::Detail {
            let typing = self.detail.as_ref().is_some_and(Detail::typing);
            match k.code {
                KeyCode::Char('e') if !typing => self.edit(fx),
                KeyCode::Char('l') if !typing => {
                    if let Some(r) = self.cur() {
                        show_log(&r.id, &title(r), fx);
                    }
                }
                _ => {
                    if self.detail.as_mut().is_none_or(|d| d.on_key(k, fx) == Back::Leave) {
                        self.focus = Focus::List;
                    }
                }
            }
            return Flow::Stay;
        }
        let armed = std::mem::take(&mut self.armed);
        let n = self.rows.len();
        // Under "Available": ⏎ / Space installs; update, remove and log don't apply yet
        if let Some(r) = self.cur().filter(|r| r.available) {
            match k.code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    fx.job = Some(Job::Plugins(Act::Install(r.id.clone())));
                    return Flow::Stay;
                }
                KeyCode::Char('u' | 'x' | 'd' | 'l') => {
                    fx.say(tr!("{} 未安装 · ⏎ 安装", "{} isn't installed · ⏎ installs it", title(r)));
                    return Flow::Stay;
                }
                _ => {}
            }
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if armed {
                    fx.say(t!("未删除", "not removed"));
                }
                return Flow::Close;
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.sel = (self.sel + 1) % n;
                self.follow();
            }
            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                self.sel = (self.sel + n - 1) % n;
                self.follow();
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Tab => match (self.cur(), &self.detail) {
                (Some(_), Some(d)) if d.rows() > 0 => self.focus = Focus::Detail,
                (Some(r), _) => fx.say(tr!("{} 没有可修改的设置", "{} has nothing to change", title(r))),
                (None, _) => {}
            },
            KeyCode::Char('m') if self.cur().is_some_and(|r| r.id == aca_core::gateway::PLUGIN) => {
                fx.open = Some(Box::new(crate::gwpage::Page::new(crate::gwpage::load)));
            }
            KeyCode::Char('i') => self.typing = Some(String::new()),
            KeyCode::Char(' ') => {
                if let Some(r) = self.cur() {
                    fx.job = Some(Job::Plugins(Act::Enable(r.id.clone(), !r.enabled)));
                }
            }
            KeyCode::Char('u') => {
                if let Some(r) = self.cur() {
                    fx.job = Some(Job::Plugins(Act::Update(Some(r.id.clone()))));
                }
            }
            KeyCode::Char('U') => fx.job = Some(Job::Plugins(Act::Update(None))),
            KeyCode::Char('S') => fx.job = Some(Job::Plugins(Act::Sync)),
            KeyCode::Char('e') => self.edit(fx),
            KeyCode::Char('l') => {
                if let Some(r) = self.cur() {
                    show_log(&r.id, &title(r), fx);
                }
            }
            KeyCode::Char('x' | 'd') => match self.cur() {
                None => {}
                Some(r) if r.from == Origin::Mine => {
                    fx.say(t!("无法删除你自己的插件 · 空格 关闭", "your own can't be removed · space switches it off"))
                }
                Some(_) if !armed => self.armed = true,
                Some(r) => fx.job = Some(Job::Plugins(Act::Remove(r.id.clone()))),
            },
            KeyCode::Char('r') => self.reload(),
            _ => {}
        }
        Flow::Stay
    }

    fn reload(&mut self) {
        let was = self.cur().map(|r| r.id.clone());
        self.rows = (self.load)();
        self.stale = (self.stale_of)();
        self.drift = (self.drift_of)();
        // The highlight follows the same one; if it's gone (just removed) it stays at the same position
        let same = was.and_then(|id| self.rows.iter().position(|r| r.id == id));
        self.sel = same.unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
        self.armed = false;
        self.follow();
        self.measure();
    }

    fn returned(&mut self, fx: &mut Fx) {
        if let Some(ed) = self.edited.take() {
            back_from(ed, fx);
        }
    }

    fn typing(&self) -> bool {
        self.typing.is_some() || (self.focus == Focus::Detail && self.detail.as_ref().is_some_and(Detail::typing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugtune::Set;
    use aca_core::i18n::{self, Lang};
    use ratatui::crossterm::event::KeyEvent;
    use std::cell::RefCell;

    thread_local! {
        /// Fake registry: the page reads it on every reload
        static REG: RefCell<Vec<Row>> = const { RefCell::new(vec![]) };
        static STALE: RefCell<Vec<String>> = const { RefCell::new(vec![]) };
        static DRIFT: RefCell<Vec<String>> = const { RefCell::new(vec![]) };
    }

    fn fake() -> Vec<Row> {
        REG.with(|r| r.borrow().clone())
    }

    fn fake_stale() -> Vec<String> {
        STALE.with(|r| r.borrow().clone())
    }

    fn fake_drift() -> Vec<String> {
        DRIFT.with(|r| r.borrow().clone())
    }

    /// Fake details: aca.delegate has one setting and subscribes to events, nothing else
    fn fake_tune(id: &str) -> Tune {
        match id {
            "aca.delegate" => Tune {
                id: id.into(),
                enabled: true,
                settings: vec![Set {
                    name: "say".into(),
                    shown: "continue".into(),
                    raw: "continue".into(),
                    desc: "what to say".into(),
                    ..Default::default()
                }],
                info: vec![("agents".into(), "skills".into())],
                log: true,
                ..Default::default()
            },
            _ => Tune { id: id.into(), ..Default::default() },
        }
    }

    fn page() -> Page {
        Page::with(fake, fake_stale, fake_drift, fake_tune)
    }

    fn set(rows: Vec<Row>) {
        REG.with(|r| *r.borrow_mut() = rows);
    }

    fn two() -> Vec<Row> {
        vec![
            Row {
                id: "aca.delegate".into(),
                enabled: true,
                name: "Delegate".into(),
                version: "0.1.0".into(),
                description: "派活给别的 agent".into(),
                from: Origin::Git,
                path: "/d/plugins/aca.delegate".into(),
                ..Default::default()
            },
            Row {
                id: "my.local".into(),
                enabled: false,
                name: "Local".into(),
                version: "0.0.1".into(),
                path: "/w/my".into(),
                ..Default::default()
            },
        ]
    }

    fn text_in(p: &Page, w: usize, h: usize) -> Vec<String> {
        p.lines_in(w, h).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    fn text(p: &Page, w: usize) -> Vec<String> {
        text_in(p, w, 30)
    }

    fn press(p: &mut Page, code: KeyCode) -> (Fx, Flow) {
        let mut fx = Fx::default();
        let f = p.on_key(KeyEvent::from(code), &mut fx);
        (fx, f)
    }

    /// A failing plugin: marked "failing" in the list, the right side lists where and what went wrong; paused ones aren't marked
    #[test]
    fn a_failing_plugin_is_marked_and_says_what_failed() {
        i18n::with(Lang::Zh, || {
            let mut rows = two();
            rows[0].alerts = vec!["钩子 turn.done 退出码 3 · 5m 前".into()];
            rows[1].alerts = vec!["定时 x 退出码 1 · 1h 前".into()];
            set(rows);
            let p = page();
            let t = text_in(&p, 110, 30).join("\n");
            assert!(t.contains("出错") && t.contains("钩子 turn.done 退出码 3 · 5m 前") && t.contains("l 日志"), "{t}");
            // The off one (my.local) is still marked 「关着」 ("off") as before
            assert!(t.lines().any(|l| l.contains(" ○ Local") && !l.contains("已关闭")), "{t}");
        });
    }

    #[test]
    fn the_list_is_on_the_left_the_highlighted_plugin_on_the_right_and_the_box_keeps_its_size() {
        i18n::with(Lang::Zh, || {
            set(two());
            let mut p = page();
            let lines = text_in(&p, 110, 30);
            let t = lines.join("\n");
            assert_eq!(lines.len(), 30);
            assert!(lines.iter().all(|l| l.width() <= 110), "{t}");
            // Left: on or off, name; an off one only draws ○, without writing 「关着」 ("off") again
            assert!(t.contains("▌● Delegate") && t.contains(" ○ Local"), "{t}");
            assert!(!t.lines().any(|l| l.contains(" ○ Local") && l.contains("已关闭")), "{t}");
            // Right: name, version and where it's from (a word or two), on, description, what can be changed; no id or source URL
            assert!(t.contains("Delegate  0.1.0 · git") && t.contains("● 已开启"), "{t}");
            assert!(!t.contains("aca.delegate") && !t.contains("github.com"), "{t}");
            assert!(
                t.contains("派活给别的 agent") && t.contains("设置") && t.contains("say  continue") && t.contains("agents  skills"),
                "{t}"
            );
            // A setting's description isn't squeezed into its row
            assert!(!t.contains("what to say"), "{t}");
            assert!(t.contains("⏎ 进入") && t.contains("e 编辑") && t.contains("l 日志") && t.contains("Esc 关闭"), "{t}");
            // The highlight background covers only the left column, not spilling right
            let row = &p.lines_in(110, 30)[2];
            let div = row.spans.iter().position(|s| s.content == DIVIDER).unwrap();
            assert!(row.style.bg.is_none() && row.spans[0].style.bg.is_some() && row.spans[div..].iter().all(|s| s.style.bg.is_none()));
            // Another highlight: same box height, the right side switches to it
            press(&mut p, KeyCode::Char('j'));
            let lines = text_in(&p, 110, 30);
            let t = lines.join("\n");
            assert_eq!(lines.len(), 30);
            assert!(
                t.contains("Local  0.0.1 · 本地") && t.contains("○ 已关闭") && !t.contains("say  continue") && !t.contains("⏎ 进入"),
                "{t}"
            );
            // Narrow screen (phone): stacked, within bounds, also exactly that tall
            let narrow = text_in(&p, 40, 24);
            assert!(narrow.len() == 24 && narrow.iter().all(|l| l.width() <= 40), "{narrow:?}");
            assert!(narrow.join("\n").contains("○ Local") && narrow.join("\n").contains("○ 已关闭"), "{narrow:?}");
            // A broken manifest is flagged
            set(vec![Row { id: "bad".into(), problem: Some("清单读不懂".into()), ..Default::default() }]);
            let t = text(&page(), 110).join("\n");
            assert!(t.contains("✕ bad") && t.contains("有问题") && t.contains("清单有问题:清单读不懂"), "{t}");
            // A needed program is missing on this machine: flagged in the list
            set(vec![Row { id: "x".into(), name: "X".into(), enabled: true, lacks: vec!["jq".into()], ..Default::default() }]);
            assert!(text(&page(), 110).join("\n").contains("缺少程序"));
            // None at all: say how to install
            set(vec![]);
            let t = text(&page(), 110).join("\n");
            assert!(t.contains("尚未安装插件 · i 安装"), "{t}");
        });
    }

    #[test]
    fn the_box_is_as_tall_as_the_longest_plugin_and_moving_the_highlight_doesnt_change_it() {
        i18n::with(Lang::Zh, || {
            set(two());
            let mut p = page();
            let (_, rw) = p.split(120 - 6);
            // The right side is sized by Delegate: its details plus the two description lines when a setting is highlighted;
            // on Local the right side gets shorter, the box doesn't
            let right = p.right(rw).0.len();
            assert_eq!(p.tall, right + 2, "{right}");
            let h = p.fill_rows().unwrap();
            press(&mut p, KeyCode::Down);
            assert!(p.right(rw).0.len() < right);
            assert_eq!((p.tall, p.fill_rows()), (right + 2, Some(h)));
            assert!(h >= LEAST_ROWS as u16 && h < 32, "{h}");
            // More plugins, taller list
            set((0..30).map(|i| Row { id: format!("p{i}"), name: format!("P{i}"), ..Default::default() }).collect());
            assert!(page().fill_rows().unwrap() >= 2 + 30 + 2);
        });
    }

    #[test]
    fn it_speaks_english_too() {
        i18n::with(Lang::En, || {
            set(two());
            let t = text(&page(), 120).join("\n");
            assert!(t.contains("Plugins") && t.contains("● on") && t.contains("Settings") && t.contains("⏎ change"), "{t}");
            assert!(!t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c) && !"派活给别的".contains(c)), "{t}");
        });
    }

    #[test]
    fn enter_moves_into_the_details_and_esc_comes_back_to_the_list() {
        i18n::with(Lang::Zh, || {
            set(two());
            let mut p = page();
            assert!(!text(&p, 110).join("\n").contains("▌ say"));
            press(&mut p, KeyCode::Enter);
            let t = text(&p, 110).join("\n");
            // Highlight moved right: the setting row is marked, its description at the bottom of this column, its keys below,
            // the left row no longer has a background
            assert!(t.contains("▌ say") && t.contains("⏎ 修改") && t.contains("- 默认") && t.contains("Esc 返回列表"), "{t}");
            assert!(!t.contains("空格 开/关"), "{t}");
            let lines = text(&p, 110);
            let at = lines.iter().position(|l| l.contains("say:what to say")).expect("说明");
            assert!(lines[at - 1].trim_end().ends_with('│') && lines[at - 2].contains("skills"), "{t}");
            // Long description: wrapped in full, not cut
            let desc = "开=你说「用delegate」时它自己接手:规划、派worker、验收、汇报,一路做完再回来告诉你结果".repeat(2);
            let mut long = fake_tune("aca.delegate");
            long.settings[0].desc = desc.clone();
            p.detail = Some(Detail::new(long));
            let right: String = text(&p, 110).iter().filter_map(|l| l.split(DIVIDER).nth(1)).map(str::trim_end).collect();
            assert!(right.contains(&desc), "{right}");
            p.detail = Some(Detail::new(fake_tune("aca.delegate")));
            // Editing on the right: ⏎ types, and while typing q is a letter; Esc first leaves typing, then back to the list, then closes
            press(&mut p, KeyCode::Enter);
            assert!(p.typing());
            assert_eq!(press(&mut p, KeyCode::Char('q')).1, Flow::Stay);
            let (fx, _) = press(&mut p, KeyCode::Enter);
            assert_eq!(fx.job, Some(Job::Plugins(Act::Setting("aca.delegate".into(), "say".into(), Some("continueq".into())))));
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Stay);
            assert!(text(&p, 110).join("\n").contains("Esc 关闭"));
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Close);
            // Nothing to change: ⏎ says so, the highlight stays in the list
            let mut p = page();
            press(&mut p, KeyCode::Char('j'));
            let (fx, _) = press(&mut p, KeyCode::Enter);
            assert!(fx.flash.unwrap().contains("Local 没有可修改的设置"));
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Close);
        });
    }

    #[test]
    fn keys_turn_into_background_jobs_and_removing_asks_twice() {
        i18n::with(Lang::Zh, || {
            set(two());
            let mut p = page();
            assert_eq!(press(&mut p, KeyCode::Char(' ')).0.job, Some(Job::Plugins(Act::Enable("aca.delegate".into(), false))));
            assert_eq!(press(&mut p, KeyCode::Char('u')).0.job, Some(Job::Plugins(Act::Update(Some("aca.delegate".into())))));
            assert_eq!(press(&mut p, KeyCode::Char('U')).0.job, Some(Job::Plugins(Act::Update(None))));
            assert_eq!(press(&mut p, KeyCode::Char('S')).0.job, Some(Job::Plugins(Act::Sync)));
            // On the paused one: Space turns it on
            press(&mut p, KeyCode::Char('j'));
            assert_eq!(press(&mut p, KeyCode::Char(' ')).0.job, Some(Job::Plugins(Act::Enable("my.local".into(), true))));
            // First x only warns, saying what will happen; the second removes; anything else in between cancels
            assert_eq!(press(&mut p, KeyCode::Char('x')).0.job, None);
            let t = text(&p, 110).join("\n");
            assert!(t.contains("删除 Local?仅取消登记,保留目录") && t.contains("x 删除 • Esc 取消"), "{t}");
            assert_eq!(press(&mut p, KeyCode::Char('x')).0.job, Some(Job::Plugins(Act::Remove("my.local".into()))));
            press(&mut p, KeyCode::Char('x'));
            press(&mut p, KeyCode::Char('k'));
            assert_eq!(press(&mut p, KeyCode::Char('x')).0.job, None);
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Close);
        });
    }

    #[test]
    fn plugins_toml_out_of_step_with_this_machine_is_named_and_s_syncs_otherwise_it_isnt_mentioned() {
        i18n::with(Lang::Zh, || {
            set(two());
            assert!(!text(&page(), 110).join("\n").contains("与本机不一致"));
            DRIFT.with(|d| *d.borrow_mut() = vec!["将安装 aca.bark".into(), "将删除 my.local".into()]);
            let t = text(&page(), 110).join("\n");
            DRIFT.with(|d| d.borrow_mut().clear());
            assert!(t.contains("plugins.toml 与本机不一致:将安装 aca.bark、将删除 my.local · S 同步"), "{t}");
        });
    }

    #[test]
    fn back_from_the_editor_syncs_only_if_plugins_toml_changed() {
        i18n::with(Lang::Zh, || {
            let f = std::env::temp_dir().join(format!("aca-plugins-edit-{}.toml", std::process::id()));
            std::fs::write(&f, "a").unwrap();
            let ed = |sync| Edited { path: f.clone(), before: mtime(&f), sync };
            let mut fx = Fx::default();
            back_from(ed(true), &mut fx);
            // Unchanged: do nothing, say nothing
            assert!(fx.job.is_none() && fx.flash.is_none());
            let (e1, e2) = (ed(true), ed(false));
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&f, "b").unwrap();
            let mut fx = Fx::default();
            back_from(e1, &mut fx);
            assert_eq!(fx.job, Some(Job::Plugins(Act::AfterEdit)));
            // Your own manifest: takes effect at once when changed, no sync
            let mut fx = Fx::default();
            back_from(e2, &mut fx);
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("已修改"));
            let _ = std::fs::remove_file(&f);
        });
    }

    #[test]
    fn what_ships_with_aca_and_isnt_installed_is_listed_below_and_enter_installs_it() {
        i18n::with(Lang::Zh, || {
            let mut rows = two();
            rows.push(Row {
                id: "aca.gateway".into(),
                from: Origin::Bundled,
                available: true,
                name: "MCP Gateway".into(),
                version: "0.1.0".into(),
                description: "所有 agent 共用的 MCP".into(),
                brings: vec!["给 agent:一个 MCP 服务".into(), "设置:servers".into()],
                ..Default::default()
            });
            set(rows);
            let mut p = page();
            let t = text(&p, 110).join("\n");
            assert!(t.contains("可安装") && t.contains("+ MCP Gateway"), "{t}");
            // Installed ones first, available ones under the "Available" row
            assert!(t.find("Local").unwrap() < t.find("可安装").unwrap(), "{t}");
            press(&mut p, KeyCode::Char('j'));
            press(&mut p, KeyCode::Char('j'));
            let t = text(&p, 110).join("\n");
            assert!(t.contains("MCP Gateway  0.1.0 · 自带") && t.contains("未安装") && !t.contains("aca.gateway"), "{t}");
            assert!(t.contains("所有 agent 共用的 MCP") && t.contains("安装后将添加") && t.contains("给 agent  一个 MCP 服务"), "{t}");
            assert!(t.contains("设置      servers") && t.contains(" ⏎ 安装 ") && t.contains("i 从其他来源安装"), "{t}");
            // Not installed yet: remove, update and log all say install first; ⏎ / Space installs
            let (fx, _) = press(&mut p, KeyCode::Char('x'));
            assert_eq!(fx.job, None);
            assert!(fx.flash.unwrap().contains("MCP Gateway 未安装"));
            assert_eq!(press(&mut p, KeyCode::Enter).0.job, Some(Job::Plugins(Act::Install("aca.gateway".into()))));
            assert_eq!(press(&mut p, KeyCode::Char(' ')).0.job, Some(Job::Plugins(Act::Install("aca.gateway".into()))));
            // Narrow screen stays within bounds too
            assert!(text_in(&p, 40, 24).iter().all(|l| l.width() <= 40), "{:?}", text_in(&p, 40, 24));
            // Only available ones: the left says no plugins are installed yet
            set(vec![Row { id: "aca.bark".into(), from: Origin::Bundled, available: true, ..Default::default() }]);
            let t = text(&page(), 110).join("\n");
            assert!(t.contains("尚未安装插件") && t.contains("+ aca.bark"), "{t}");
        });
    }

    #[test]
    fn i_asks_where_to_install_from_and_enter_installs_it() {
        i18n::with(Lang::Zh, || {
            set(vec![]);
            let mut p = page();
            press(&mut p, KeyCode::Char('i'));
            assert!(p.typing());
            for c in "x/aca-delegatee".chars() {
                press(&mut p, KeyCode::Char(c));
            }
            press(&mut p, KeyCode::Backspace);
            let t = text(&p, 110).join("\n");
            assert!(t.contains("从其他来源安装") && t.contains("安装来源 > x/aca-delegate▏") && t.contains("⏎ 安装"), "{t}");
            // While typing, q and x are letters, not close / remove
            assert_eq!(press(&mut p, KeyCode::Char('q')).1, Flow::Stay);
            press(&mut p, KeyCode::Backspace);
            assert_eq!(press(&mut p, KeyCode::Enter).0.job, Some(Job::Plugins(Act::Install("x/aca-delegate".into()))));
            assert!(!p.typing());
            // Empty doesn't install; Esc only leaves the input
            press(&mut p, KeyCode::Char('i'));
            assert_eq!(press(&mut p, KeyCode::Enter).0.job, None);
            press(&mut p, KeyCode::Char('i'));
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Stay);
            assert!(!p.typing());
        });
    }

    #[test]
    fn running_sessions_that_need_a_restart_are_named() {
        i18n::with(Lang::Zh, || {
            set(two());
            STALE.with(|s| *s.borrow_mut() = vec!["主线".into(), "读登录".into()]);
            let t = text(&page(), 120).join("\n");
            STALE.with(|s| s.borrow_mut().clear());
            assert!(t.contains("「主线」「读登录」重启后启用新插件"), "{t}");
            // With more, only the first three are named
            STALE.with(|s| *s.borrow_mut() = ["a", "b", "c", "d", "e"].map(String::from).to_vec());
            let t = text(&page(), 120).join("\n");
            STALE.with(|s| s.borrow_mut().clear());
            assert!(t.contains("「a」「b」「c」等 5 个重启后启用新插件"), "{t}");
        });
    }

    #[test]
    fn your_own_plugin_is_called_that_and_cant_be_removed_here() {
        i18n::with(Lang::Zh, || {
            let mut rows = two();
            rows.push(Row {
                id: "user".into(),
                enabled: true,
                name: "user".into(),
                version: "0".into(),
                from: Origin::Mine,
                path: "/c/aca/user".into(),
                ..Default::default()
            });
            set(rows);
            let mut p = page();
            press(&mut p, KeyCode::Char('j'));
            press(&mut p, KeyCode::Char('j'));
            let t = text(&p, 120).join("\n");
            // Named 「你自己的」 ("Your own"), no version or path
            assert!(t.contains("▌● 你自己的") && !t.contains("/c/aca/user") && !t.contains("你自己的  0"), "{t}");
            assert!(t.contains("e 编辑"), "{t}");
            let (fx, _) = press(&mut p, KeyCode::Char('x'));
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("无法删除你自己的插件"));
            assert!(!p.armed);
            // e: your own one opens its manifest
            let (fx, _) = press(&mut p, KeyCode::Char('e'));
            assert_eq!(fx.external, Some(crate::ui::External::Edit { path: "/c/aca/user/aca-plugin.toml".into(), line: 1 }));
        });
    }

    #[test]
    fn after_a_job_the_highlight_stays_on_the_same_plugin_and_in_the_details() {
        set(two());
        let mut p = page();
        press(&mut p, KeyCode::Enter);
        assert_eq!(p.focus, Focus::Detail);
        // Reloaded after a change: still on the right
        p.reload();
        assert_eq!(p.focus, Focus::Detail);
        press(&mut p, KeyCode::Esc);
        press(&mut p, KeyCode::Char('j'));
        // The one before was removed: the highlight stays on my.local
        set(two().into_iter().skip(1).collect());
        p.reload();
        assert_eq!(p.cur().map(|r| r.id.as_str()), Some("my.local"));
    }
}
