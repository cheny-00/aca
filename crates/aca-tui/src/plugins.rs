//! 插件管理:列表上按 `P` 开这一页(像 vim 的插件管理器,lazy.nvim 的 `:Lazy`)。框的大小固定(桌面上占大半屏),
//! 左边一行一个插件(开着 ● / 停着 ○ / 坏了 ✕、名字),aca 自带还没装的列在下面「可装」里;右边是高亮那个的详情:
//! 名字版本、开没开、从哪来、说明,再往下是能改的(设置、给 agent 的工具、插件菜单里的键,见 plugtune.rs)。
//! 窄屏(手机)上下叠着:列表在上,详情在下。
//!
//! - ⏎ 进到右边那一栏改(Esc 回列表);可装的 ⏎ 装上;gateway 那一行 `m` 管它下面的 MCP(gwpage.rs)
//! - `i` 装一个:输入 git 地址、`owner/repo`(GitHub)、本地目录(本地的就是链接),或者 aca 自带的 `aca.<名字>`,⏎ 装
//! - 空格 开 / 停:停着的不跑它的钩子、不给 agent 带它的东西(agent 下次启动才变)
//! - `u` 更新这一个,`U` 更新全部:git 装的拉新的(只快进);钉了版本的、链接的不动;自带的跟着 aca 走
//! - `x` 按两次删:git 装的、自带的连目录删,链接的只取消登记;插件自己的配置、状态都留着
//! - `e` 用编辑器打开 `plugins.toml`,停在高亮那个插件那一段;改完回来自己同步(改了才同步)。
//!   你自己的那份(`~/.config/aca/user/`)打开它的清单
//! - plugins.toml 跟本机对不上(别处改的、dotfiles 拉下来的)时底下写出差在哪,`S` 照它同步;对得上就不提
//! - `l` 看它的日志(事件钩子的输出)
//!
//! 装、更新、删、开停都放到后台跑(`Job::Plugins`,见 `aca_core::plugin`),做完这一页重读。
//! 底下提醒哪些开着的会话起来时带的和现在不一样(要重启才带上,见 `mount::stale`)。

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

/// 来源最多输这么长
const INPUT_MAX: usize = 300;

/// 从哪来的
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// 链接的本地目录
    #[default]
    Linked,
    Git,
    /// aca 自带的
    Bundled,
    /// 你自己的那份(`~/.config/aca/user/`)
    Mine,
}

/// 一个登记了的插件
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub id: String,
    pub enabled: bool,
    /// 清单里的名字、版本、说明;清单读不了时空着,`problem` 写原因
    pub name: String,
    pub version: String,
    pub description: String,
    pub problem: Option<String>,
    pub from: Origin,
    /// 从哪来,一句话(右边标题下面那行)
    pub origin: String,
    pub path: String,
    /// 清单 needs 里这台机器上没有的程序(列表里标「缺程序」,右边写缺哪个)
    pub lacks: Vec<String>,
    /// 出错了的(钩子、定时上次没跑成,gateway 的服务停下了),一处一句:列表里标「出错了」,右边列出来
    pub alerts: Vec<String>,
    /// gateway 这个插件:它现在在不在跑、几个连接、下面几个服务
    pub live: Option<String>,
    /// aca 自带、还没装的:列在「可装」里,⏎ 装
    pub available: bool,
    /// 还没装的:装上会带来什么(清单说的,见 `Manifest::brings`)
    pub brings: Vec<String>,
}

/// 从哪来的,一句话(右边标题下面那行)
pub fn origin_text(e: &plugin::Entry) -> String {
    let path = crate::create::tilde(&e.path);
    match e.source.as_str() {
        "" => tr!("链接 {path}", "linked, {path}"),
        plugin::first_party::SOURCE => t!("aca 自带,跟着 aca 升级", "ships with aca, upgrades with it").into(),
        // 标题已经写着「你自己的」
        plugin::user::SOURCE => format!("{path}/{}", plugin::MANIFEST),
        src => tr!("git {src}", "git {src}"),
    }
}

/// 登记表里的插件,清单读进来
pub fn load() -> Vec<Row> {
    // 刚升级了 aca:自带的先换成新的,这一页列的就是现在的
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
            let mut r =
                Row { id: e.id.clone(), enabled: e.enabled, from, origin: origin_text(&e), path: e.path.clone(), ..Default::default() };
            match plugin::read_manifest(Path::new(&e.path)) {
                Ok((m, _)) => {
                    if e.id == aca_core::gateway::PLUGIN {
                        r.live = Some(gateway_live());
                    }
                    r.lacks = m.missing();
                    r.alerts = problems.iter().filter(|p| p.plugin == e.id).map(|p| p.text(now)).collect();
                    (r.name, r.version, r.description) = (m.name, m.version, m.description);
                    if from == Origin::Mine && r.description.is_empty() {
                        r.description = t!(
                            "你自己写的命令、窗格、视图:不用装,存了马上生效(e 打开它)",
                            "commands, panes and views you write yourself: nothing to install, saving applies at once (e opens it)"
                        )
                        .into();
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

/// gateway 现在的样子(一句话):问在跑的那个,没在跑就说什么时候会起
fn gateway_live() -> String {
    match aca_core::gateway::ctl(serde_json::json!({ "cmd": "status" })) {
        Ok(v) => tr!(
            "在跑:{} 个连接 · {} 个服务",
            "running: {} connections · {} servers",
            v["clients"].as_array().map_or(0, Vec::len),
            v["servers"].as_array().map_or(0, Vec::len)
        ),
        Err(_) => t!("没在跑(第一个 agent 连上来时自己起)", "not running (starts when the first agent connects)").into(),
    }
}

/// 要放到后台做的
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// 从这里装(git 地址、owner/repo、本地目录、aca.<名字>)
    Install(String),
    /// 更新这一个;None = 全部(链接的不算)
    Update(Option<String>),
    Remove(String),
    Enable(String, bool),
    /// 让本机跟 plugins.toml 一致
    Sync,
    /// 编辑器里改过 plugins.toml 回来了:照它同步,说改了什么
    AfterEdit,
    /// 给 / 不给 agent 一个能力当工具:(插件, 能力, 给不给)
    Tool(String, String, bool),
    /// `:` 菜单里一个的键:(插件, 能力 / 窗格 / 视图, Some(键) / Some("") 不要键 / None 回到清单的)
    Key(String, String, Option<String>),
    /// 改一个设置:(插件, 名字, Some(值) / None 回到默认)
    Setting(String, String, Option<String>),
}

impl Act {
    /// 跑的时候底栏显示的
    pub fn doing(&self) -> String {
        match self {
            Act::Install(src) => tr!("正在装 {src}…", "installing {src}…"),
            Act::Update(Some(id)) => tr!("正在更新 {id}…", "updating {id}…"),
            Act::Update(None) => t!("正在更新插件…", "updating plugins…").into(),
            Act::Remove(id) => tr!("正在删 {id}…", "removing {id}…"),
            Act::Enable(id, true) => tr!("正在开 {id}…", "switching on {id}…"),
            Act::Enable(id, false) => tr!("正在停 {id}…", "pausing {id}…"),
            Act::Sync | Act::AfterEdit => t!("正在照 plugins.toml 同步…", "syncing with plugins.toml…").into(),
            Act::Tool(_, tool, true) => tr!("正在把 {tool} 给 agent…", "giving agents {tool}…"),
            Act::Tool(_, tool, false) => tr!("正在收回 {tool}…", "withholding {tool}…"),
            Act::Key(_, target, _) => tr!("正在改 {target} 的键…", "changing the key for {target}…"),
            Act::Setting(_, name, _) => tr!("正在改 {name}…", "changing {name}…"),
        }
    }
}

/// run 循环放进后台跑的(`Job::Plugins`)
pub fn job(a: &Act) -> Outcome {
    match a {
        Act::Install(src) => match plugin::install(src, None) {
            Ok((m, warn)) => {
                let warn = if warn.is_empty() { String::new() } else { tr!(" · 提醒:{}", " · note: {}", warn.join(t!(";", "; "))) };
                Outcome::Done(tr!("装好了 {}({} {}){warn}", "installed {} ({} {}){warn}", m.id, m.name, m.version))
            }
            Err(e) => Outcome::Failed(tr!("没装上:{e}", "not installed: {e}")),
        },
        Act::Update(one) => {
            let ids: Vec<String> = match one {
                Some(id) => vec![id.clone()],
                None => plugin::registry().into_iter().filter(|e| !e.source.is_empty()).map(|e| e.id).collect(),
            };
            if ids.is_empty() {
                return Outcome::Done(t!("没有要更新的插件(链接的不归 aca 更新)", "nothing to update (linked plugins are yours)").into());
            }
            let said: Vec<String> = ids.iter().map(|id| plugin::update(id).unwrap_or_else(|e| format!("{id}: {e}"))).collect();
            Outcome::Done(said.join(" · "))
        }
        Act::Remove(id) => plugin::remove(id).map_or_else(|e| Outcome::Failed(tr!("没删:{e}", "not removed: {e}")), Outcome::Done),
        Act::Enable(id, on) => match plugin::enable(id, *on) {
            Ok(()) if *on => Outcome::Done(tr!("开了 {id}(agent 下次启动带上)", "switched on {id} (agents get it from their next start)")),
            Ok(()) => Outcome::Done(tr!("停了 {id}(agent 下次启动不带)", "paused {id} (agents drop it from their next start)")),
            Err(e) => Outcome::Failed(e),
        },
        Act::Sync => match plugin::sync() {
            Ok(said) if said.is_empty() => {
                Outcome::Done(t!("跟 plugins.toml 一致,没什么要做的", "matches plugins.toml; nothing to do").into())
            }
            Ok(said) => Outcome::Done(said.join(" · ")),
            Err(e) => Outcome::Failed(tr!("没同步:{e}", "not synced: {e}")),
        },
        Act::AfterEdit => match plugin::sync() {
            Ok(said) if said.is_empty() => Outcome::Done(
                t!(
                    "改好了:设置、键马上就是新的;给 agent 的,开着的会话重启才变",
                    "saved: settings and keys apply now; what agents get changes for running sessions after a restart"
                )
                .into(),
            ),
            Ok(said) => Outcome::Done(tr!("改好了:{}", "saved: {}", said.join(" · "))),
            Err(e) => Outcome::Failed(tr!("plugins.toml 有问题:{e}(e 再改)", "plugins.toml has a problem: {e} (e to fix it)")),
        },
        Act::Tool(id, tool, on) => match plugin::yours::allow(id, tool, *on) {
            Ok(_) if *on => {
                Outcome::Done(tr!("{tool} 给 agent 了(开着的会话重启才带上)", "agents get {tool} (running sessions after a restart)"))
            }
            Ok(_) => Outcome::Done(tr!(
                "{tool} 不给 agent 了(开着的会话重启才变)",
                "agents no longer get {tool} (running sessions after a restart)"
            )),
            Err(e) => Outcome::Failed(e),
        },
        Act::Key(id, target, key) => match plugin::yours::set_key(id, target, key.as_deref()) {
            Ok(()) => {
                let now = plugin::menu().into_iter().find(|i| &i.plugin == id && &i.id == target).and_then(|i| i.key);
                Outcome::Done(match now {
                    Some(k) => tr!("{target}:: 菜单里按 {k}", "{target}: press {k} in the : menu"),
                    None => tr!("{target}:没有键", "{target}: no key"),
                })
            }
            Err(e) => Outcome::Failed(e),
        },
        Act::Setting(id, name, value) => match plugin::set_setting(id, name, value.as_deref()) {
            Ok(()) if value.is_some() => Outcome::Done(tr!("{name} 改好了", "{name} saved")),
            Ok(()) => Outcome::Done(tr!("{name} 回到默认的了", "{name} is back to its default")),
            Err(e) => Outcome::Failed(e),
        },
    }
}

/// e 打开的文件和打开前的修改时间:回来时改了才同步
#[derive(Debug)]
pub struct Edited {
    path: PathBuf,
    before: Option<SystemTime>,
    /// 是 plugins.toml(改了要同步);你自己的清单改了马上生效,不用
    sync: bool,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

/// e:用编辑器打开 plugins.toml,停在这个插件那一段(没有这一段停在最后);`mine` 给了(你自己的那份)打开它的清单
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

/// 从编辑器回来了:没改就说没改;改了 plugins.toml 就同步
pub fn back_from(ed: Edited, fx: &mut Fx) {
    match (mtime(&ed.path) != ed.before, ed.sync) {
        (false, _) => fx.say(t!("没改", "unchanged")),
        (true, true) => fx.job = Some(Job::Plugins(Act::AfterEdit)),
        (true, false) => fx.say(t!("改好了,马上生效", "saved; takes effect now")),
    }
}

/// l:看它的日志(事件钩子的输出)
pub fn show_log(id: &str, fx: &mut Fx) {
    let log = plugin::log_file(id);
    if log.exists() {
        fx.external = Some(crate::ui::External::View { path: log.to_string_lossy().into_owned(), line: 1, temp: false });
    } else {
        fx.say(tr!("{id} 还没有日志(事件钩子的输出记在这里)", "{id} has no log yet (event hook output goes there)"));
    }
}

/// 高亮在哪一栏
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    List,
    Detail,
}

/// 两栏放得下的最窄宽度;再窄(手机)上下叠着
pub(crate) const SPLIT_W: usize = 76;
/// 两栏中间的分隔
pub(crate) const DIVIDER: &str = " │  ";

/// `P` 那一页
pub struct Page {
    load: fn() -> Vec<Row>,
    rows: Vec<Row>,
    sel: usize,
    focus: Focus,
    /// 高亮那个插件的详情(右边那一栏);可装的、清单坏了的没有
    detail: Option<Detail>,
    /// 按了一次 x,等再按一次
    armed: bool,
    /// 正在输要装的来源
    typing: Option<String>,
    /// 开着的会话里要重启才带上插件改动的:标题
    stale: Vec<String>,
    stale_of: fn() -> Vec<String>,
    /// plugins.toml 跟本机对不上的地方
    drift: Vec<String>,
    drift_of: fn() -> Vec<String>,
    /// 读一个插件的详情(测试换成假的)
    tune: fn(&str) -> Tune,
    /// e 打开了文件:回来时改了就同步
    edited: Option<Edited>,
}

/// 开着的会话里,起来时带的和现在不一样的(标题)
pub fn stale() -> Vec<String> {
    plugin::mount::stale().into_iter().map(|(_, title)| title).collect()
}

/// 列表里、右边标题上叫它什么:清单里的名字;你自己的那份叫「你自己的」
fn title(r: &Row) -> String {
    match r.from {
        Origin::Mine => t!("你自己的", "Your own").to_string(),
        _ if r.name.is_empty() => r.id.clone(),
        _ => r.name.clone(),
    }
}

/// 一行拆成片段,整行的样式(底色、粗细)落到每一段上:两栏拼成一行时,左边的底色不能铺到右边去
pub(crate) fn flat(line: Line<'static>) -> Vec<Span<'static>> {
    let st = line.style;
    line.spans.into_iter().map(|s| Span::styled(s.content, st.patch(s.style))).collect()
}

/// 补空格到 w 宽(两栏拼成一行用),整行的样式带上
pub(crate) fn pad(line: Line<'static>, w: usize) -> Vec<Span<'static>> {
    let (st, n) = (line.style, w.saturating_sub(line.width()));
    let mut spans = flat(line);
    if n > 0 {
        spans.push(Span::styled(" ".repeat(n), st));
    }
    spans
}

/// 从第几行开始显示,让第 at 行落在 h 行里(放在偏上的位置,下面留点看得见的)
pub(crate) fn scroll(at: Option<usize>, len: usize, h: usize) -> usize {
    match at {
        Some(i) if len > h => i.saturating_sub(h / 2).min(len - h),
        _ => 0,
    }
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
        };
        p.reload();
        p
    }

    fn cur(&self) -> Option<&Row> {
        self.rows.get(self.sel)
    }

    /// 「可装」那一段从第几行开始
    fn first_available(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.available)
    }

    /// 高亮换了、重读了:右边那一栏换成高亮那个的;还是同一个插件就留着它里面的高亮
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

    /// 左边的列表:一行一个(开着 ● / 停着 ○ / 坏了 ✕、名字),「可装」的在下面。也给出高亮那行是第几行
    fn list(&self, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let mut out = vec![];
        let mut at = None;
        if self.first_available() == Some(0) {
            out.push(Line::styled(t!("还没装插件", "nothing installed yet"), kit::dim()));
        }
        for (i, r) in self.rows.iter().enumerate() {
            if Some(i) == self.first_available() {
                if !out.is_empty() {
                    out.push(Line::raw(""));
                }
                out.push(Line::styled(t!("可装", "Available"), kit::dim().add_modifier(Modifier::BOLD)));
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
                _ if !r.alerts.is_empty() && r.enabled => (t!("出错了", "failing"), theme::warn()),
                _ if !r.lacks.is_empty() => (t!("缺程序", "missing"), theme::warn()),
                (_, None, false) => (t!("停着", "paused"), kit::dim()),
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

    /// 右边的标题:名字(强调色粗体)、版本,右边写开着 / 停着;下一行淡的 id 和从哪来
    fn header(r: &Row, w: usize) -> Vec<Line<'static>> {
        let (state, st) = match (r.available, &r.problem, r.enabled) {
            (true, ..) => (t!("还没装", "not installed"), kit::dim()),
            (_, Some(_), _) => (t!("✕ 清单有问题", "✕ manifest problem"), theme::warn()),
            (_, None, true) => (t!("● 开着", "● on"), theme::accent()),
            (_, None, false) => (t!("○ 停着", "○ paused"), kit::dim()),
        };
        let room = w.saturating_sub(state.width() + 2);
        let name = kit::clip(&title(r), room).trim_end().to_string();
        let ver = if r.version.is_empty() || r.from == Origin::Mine { String::new() } else { format!("  {}", r.version) };
        let ver = kit::clip(&ver, room.saturating_sub(name.width())).trim_end().to_string();
        let top = kit::spread(
            vec![Span::styled(name, theme::accent().add_modifier(Modifier::BOLD)), Span::styled(ver, kit::dim())],
            vec![Span::styled(state.to_string(), st)],
            w,
        );
        let from = if r.available { t!("aca 自带", "ships with aca").to_string() } else { r.origin.clone() };
        let sub = kit::clip(&format!("{} · {from}", r.id), w).trim_end().to_string();
        vec![top, Line::styled(sub, kit::dim())]
    }

    /// 右边那一栏:高亮的插件是什么、能改什么。也给出高亮那行是第几行
    fn right(&self, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
        let wrap = |text: &str, st: Style, most: usize| {
            crate::ui::wrap_width(text, w).into_iter().take(most).map(move |l| Line::styled(l, st)).collect::<Vec<_>>()
        };
        let mut out = vec![];
        if let Some(text) = &self.typing {
            out.push(Line::styled(t!("从别处装", "Install from elsewhere"), theme::accent().add_modifier(Modifier::BOLD)));
            out.push(Line::raw(""));
            out.push(Line::from(vec![
                Span::styled(t!("从哪装 > ", "install from > "), theme::accent()),
                Span::raw(kit::clip(text, w.saturating_sub(12)).trim_end().to_string()),
                Span::raw("▏"),
            ]));
            out.push(Line::raw(""));
            out.extend(wrap(
                t!(
                    "git 地址、owner/repo(GitHub 上的)、本地目录(就是链接,改了马上生效),或者 aca.<名字>(aca 自带的)",
                    "a git URL, owner/repo (on GitHub), a local directory (linked: changes apply at once), or aca.<name> (ships with aca)"
                ),
                kit::dim(),
                4,
            ));
            return (out, None);
        }
        let Some(r) = self.cur() else {
            out.extend(wrap(
                t!(
                    "还没装插件。别处的按 i(git 地址、owner/repo、本地目录)",
                    "No plugins yet. For others press i (a git URL, owner/repo, a local directory)"
                ),
                Style::new(),
                3,
            ));
            return (out, None);
        };
        out.extend(Self::header(r, w));
        out.push(Line::raw(""));
        if let Some(p) = &r.problem {
            out.extend(wrap(&tr!("清单有问题:{p}", "manifest problem: {p}"), theme::warn(), 4));
            out.push(Line::raw(""));
        }
        if !r.alerts.is_empty() {
            out.push(Line::styled(t!("出错了", "Failing"), theme::warn().add_modifier(Modifier::BOLD)));
            for a in r.alerts.iter().take(4) {
                out.extend(wrap(a, theme::warn(), 2));
            }
            out.extend(wrap(
                t!("l 看它的日志;下次跑成了、过了一天就不再提", "l shows its log; it stops showing once a run succeeds, or after a day"),
                kit::dim(),
                2,
            ));
            out.push(Line::raw(""));
        }
        if !r.description.is_empty() {
            out.extend(wrap(&r.description, Style::new(), 4));
            out.push(Line::raw(""));
        }
        if r.available {
            if !r.brings.is_empty() {
                out.push(Line::styled(t!("装上会带来", "Brings"), Style::new().add_modifier(Modifier::BOLD)));
                let fields: Vec<(&str, &str)> = r.brings.iter().map(|b| b.split_once([':', ':']).unwrap_or(("", b))).collect();
                let label_w = fields.iter().map(|(l, _)| l.width()).max().unwrap_or(0) + 2;
                for (l, v) in fields {
                    let v = kit::clip(v, w.saturating_sub(label_w)).trim_end().to_string();
                    out.push(Line::from(vec![Span::styled(format!("{l}{}", " ".repeat(label_w - l.width())), kit::dim()), Span::raw(v)]));
                }
                out.push(Line::raw(""));
            }
            out.push(Line::from(vec![
                kit::button(t!("⏎ 装上", "⏎ install"), theme::pal().accent),
                Span::styled(t!("  装了随时能停、能删", "  pause or remove it any time"), kit::dim()),
            ]));
            return (out, None);
        }
        // gateway:现在在不在跑,写在能改的前面;它下面的 MCP 按 m 进去管
        if let Some(l) = &r.live {
            let label = t!("状态", "state");
            let hint = t!("  m 管它下面的 MCP", "  m manages its MCPs");
            out.push(Line::from(vec![
                Span::styled(format!("{label}  "), kit::dim()),
                Span::raw(kit::clip(l, w.saturating_sub(label.width() + 2 + hint.width())).trim_end().to_string()),
                Span::styled(hint, kit::dim()),
            ]));
            out.push(Line::raw(""));
        }
        let mut hl = None;
        if let Some(d) = &self.detail {
            let (body, at) = d.body(w, self.focus == Focus::Detail);
            hl = at.map(|i| i + out.len());
            out.extend(body);
        }
        (out, hl)
    }

    /// 底下的提醒(删前确认、plugins.toml 对不上、要重启的会话),一条最多两行
    fn banners(&self, w: usize) -> Vec<Line<'static>> {
        let wrap =
            |text: &str| crate::ui::wrap_width(text, w).into_iter().take(2).map(|l| Line::styled(l, theme::warn())).collect::<Vec<_>>();
        let mut out = vec![];
        if let Some(r) = self.cur().filter(|_| self.armed) {
            let what = match r.from {
                Origin::Linked => t!("取消登记(目录是你的,不动)", "unregister it (the directory is yours, left alone)"),
                Origin::Git => t!("连克隆的目录一起删", "delete the cloned directory too"),
                Origin::Bundled => {
                    t!("连 aca 写出来的那份一起删(以后还能再装)", "delete the copy aca wrote out (you can install it again)")
                }
                Origin::Mine => "",
            };
            out.extend(wrap(&tr!(
                "再按 x 删掉 {}:{what};它的配置、状态留着",
                "press x again to remove {}: {what}; its config and state stay",
                r.id
            )));
        }
        if !self.drift.is_empty() {
            out.extend(wrap(&tr!(
                "plugins.toml 跟本机对不上:{}(S 照它同步)",
                "plugins.toml doesn't match this machine: {} (S to sync)",
                self.drift.join(t!("、", ", "))
            )));
        }
        if !self.stale.is_empty() {
            let names: Vec<String> = self.stale.iter().take(3).map(|t| format!("「{t}」")).collect();
            let more = if self.stale.len() > 3 { tr!(" 等 {} 个", " and {} more", self.stale.len() - 3) } else { String::new() };
            out.extend(wrap(&tr!(
                "{}{more} 开着,起来时带的插件和现在不一样:重启它们才带上",
                "{}{more} still run with what plugins gave them at start: restart them to pick up the change",
                names.join(t!("、", ", "))
            )));
        }
        out
    }

    /// 底下那行键:跟着高亮在哪一栏、高亮的是什么
    fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.typing.is_some() {
            return vec![("⏎", t!("装", "install")), ("^U", t!("清空", "clear")), ("Esc", t!("不装了", "cancel"))];
        }
        if self.armed {
            return vec![("x", t!("删掉", "remove")), ("Esc", t!("不删", "keep"))];
        }
        if let (Focus::Detail, Some(d)) = (self.focus, &self.detail) {
            let mut keys = d.keys();
            if !d.typing() {
                keys.push(("e", if d.data.mine.is_some() { t!("改清单", "edit manifest") } else { t!("改文件", "edit file") }));
            }
            return keys;
        }
        match self.cur() {
            None => vec![("i", t!("装", "install")), ("e", t!("改 plugins.toml", "edit plugins.toml"))],
            Some(r) if r.available => {
                vec![
                    ("⏎", t!("装上", "install")),
                    ("i", t!("从别处装", "install other")),
                    ("e", t!("改 plugins.toml", "edit plugins.toml")),
                ]
            }
            Some(r) => {
                let mut keys = vec![];
                if self.detail.as_ref().is_some_and(|d| d.rows() > 0) {
                    keys.push(("⏎", t!("进去改", "change")));
                }
                if r.id == aca_core::gateway::PLUGIN {
                    keys.push(("m", t!("管 MCP", "MCPs")));
                }
                keys.extend([
                    (t!("空格", "space"), t!("开/停", "on/off")),
                    ("i", t!("装", "install")),
                    ("u", t!("更新", "update")),
                    ("x", t!("删", "remove")),
                    ("e", if r.from == Origin::Mine { t!("改清单", "edit manifest") } else { t!("改文件", "edit file") }),
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

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        self.lines_in(w, 32)
    }

    /// 标题、两栏(窄屏上下叠)、提醒、细线、键;正好 h 行,框的大小不跟着高亮变
    fn lines_in(&self, w: usize, h: usize) -> Vec<Line<'static>> {
        let in_detail = self.focus == Focus::Detail;
        let esc = if in_detail { t!("回列表", "back to the list") } else { t!("关闭", "close") };
        let mut out = vec![kit::modal_title(t!("插件", "Plugins"), theme::accent(), kit::esc_hint(esc), w), Line::raw("")];
        let mut foot = self.banners(w);
        foot.push(Line::styled("─".repeat(w), theme::faint()));
        foot.push(kit::keys_line(&self.keys(), vec![], w));
        let bh = h.saturating_sub(out.len() + foot.len());
        if w >= SPLIT_W {
            let names = self.rows.iter().map(|r| title(r).width()).max().unwrap_or(0);
            let lw = (names + 4 + 5).clamp(22, 32);
            let rw = w.saturating_sub(lw + DIVIDER.width());
            let (left, at) = self.list(lw);
            let (right, hl) = self.right(rw);
            let (lo, ro) = (scroll(at, left.len(), bh), scroll(hl, right.len(), bh));
            for i in 0..bh {
                let l = left.get(lo + i).cloned().unwrap_or_default();
                let r = right.get(ro + i).cloned().unwrap_or_default();
                let mut spans = pad(l, lw);
                spans.push(Span::styled(DIVIDER, theme::faint()));
                spans.extend(flat(r));
                out.push(Line::from(spans));
            }
        } else {
            // 手机:列表在上、高亮那个的详情在下;进了详情只显示详情
            let (right, hl) = self.right(w);
            let mut body = vec![];
            if !in_detail {
                let (left, at) = self.list(w);
                let lh = left.len().min((bh * 2 / 5).max(4)).min(bh);
                let lo = scroll(at, left.len(), lh);
                body.extend(left.into_iter().skip(lo).take(lh));
                body.push(Line::styled("┄".repeat(w), theme::faint()));
            }
            let room = bh.saturating_sub(body.len());
            let ro = scroll(hl, right.len(), room);
            body.extend(right.into_iter().skip(ro).take(room));
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
        // 高亮在右边那一栏:键归它;e、l 照样能按
        if self.focus == Focus::Detail {
            let typing = self.detail.as_ref().is_some_and(Detail::typing);
            match k.code {
                KeyCode::Char('e') if !typing => self.edit(fx),
                KeyCode::Char('l') if !typing => {
                    if let Some(r) = self.cur() {
                        show_log(&r.id, fx);
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
        // 「可装」里的:⏎ / 空格装上;更新、删、看日志都还谈不上
        if let Some(r) = self.cur().filter(|r| r.available) {
            match k.code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    fx.job = Some(Job::Plugins(Act::Install(r.id.clone())));
                    return Flow::Stay;
                }
                KeyCode::Char('u' | 'x' | 'd' | 'l') => {
                    fx.say(tr!("{} 还没装:⏎ 装上", "{} isn't installed: ⏎ installs it", r.id));
                    return Flow::Stay;
                }
                _ => {}
            }
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if armed {
                    fx.say(t!("没删", "not removed"));
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
                (Some(r), _) => {
                    fx.say(tr!("{} 没有能改的(设置、工具、菜单键)", "{} has nothing to change (settings, tools, menu keys)", title(r)))
                }
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
                    show_log(&r.id, fx);
                }
            }
            KeyCode::Char('x' | 'd') => match self.cur() {
                None => {}
                Some(r) if r.from == Origin::Mine => fx.say(tr!(
                    "{} 是你自己的:删掉 {}/aca-plugin.toml 就没了;空格先停着",
                    "{} is your own: delete {}/aca-plugin.toml to drop it; space pauses it",
                    r.id,
                    crate::create::tilde(&r.path)
                )),
                Some(_) if !armed => self.armed = true,
                Some(r) => fx.job = Some(Job::Plugins(Act::Remove(r.id.clone()))),
            },
            KeyCode::Char('r') => {
                self.reload();
                fx.say(t!("重读了", "reloaded"));
            }
            _ => {}
        }
        Flow::Stay
    }

    fn reload(&mut self) {
        let was = self.cur().map(|r| r.id.clone());
        self.rows = (self.load)();
        self.stale = (self.stale_of)();
        self.drift = (self.drift_of)();
        // 高亮跟着原来那个;它没了(刚删掉)就留在原来的位置上
        let same = was.and_then(|id| self.rows.iter().position(|r| r.id == id));
        self.sel = same.unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
        self.armed = false;
        self.follow();
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
        /// 假的登记表:页面每次重读都读它
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

    /// 假的详情:aca.delegate 有一个设置、订了事件,别的什么都没有
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
                info: vec![("events".into(), "turn.failed".into())],
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
                origin: "git https://github.com/x/aca-delegate.git".into(),
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

    /// 出错了的插件:列表里标「出错了」,右边列出哪一处、怎么了;停着的不标
    #[test]
    fn a_failing_plugin_is_marked_and_says_what_failed() {
        i18n::with(Lang::Zh, || {
            let mut rows = two();
            rows[0].alerts = vec!["钩子 turn.done 退出码 3 · 5m 前".into()];
            rows[1].alerts = vec!["定时 x 退出码 1 · 1h 前".into()];
            set(rows);
            let p = page();
            let t = text_in(&p, 110, 30).join("\n");
            assert!(t.contains("出错了") && t.contains("钩子 turn.done 退出码 3 · 5m 前") && t.contains("l 看它的日志"), "{t}");
            // 停着的(my.local)照旧标「停着」
            assert!(t.lines().any(|l| l.contains(" ○ Local") && l.contains("停着")), "{t}");
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
            // 左边:开没开、名字;停着的写出来
            assert!(t.contains("▌● Delegate") && t.contains(" ○ Local") && t.contains("停着"), "{t}");
            // 右边:名字版本、开着,id 和从哪来,说明,能改的
            assert!(t.contains("Delegate  0.1.0") && t.contains("● 开着"), "{t}");
            assert!(t.contains("aca.delegate · git https://github.com/x/aca-delegate.git"), "{t}");
            assert!(
                t.contains("派活给别的 agent") && t.contains("设置") && t.contains("say  continue") && t.contains("events  turn.failed"),
                "{t}"
            );
            assert!(t.contains("⏎ 进去改") && t.contains("e 改文件") && t.contains("l 日志") && t.contains("Esc 关闭"), "{t}");
            // 高亮的底色只铺左边那一栏,不漫到右边
            let row = &p.lines_in(110, 30)[2];
            let div = row.spans.iter().position(|s| s.content == DIVIDER).unwrap();
            assert!(row.style.bg.is_none() && row.spans[0].style.bg.is_some() && row.spans[div..].iter().all(|s| s.style.bg.is_none()));
            // 换一个高亮:框一样高,右边换成它的
            press(&mut p, KeyCode::Char('j'));
            let lines = text_in(&p, 110, 30);
            let t = lines.join("\n");
            assert_eq!(lines.len(), 30);
            assert!(t.contains("○ 停着") && t.contains("my.local") && !t.contains("say  continue") && !t.contains("⏎ 进去改"), "{t}");
            // 窄屏(手机):上下叠着,不出界,也是正好那么高
            let narrow = text_in(&p, 40, 24);
            assert!(narrow.len() == 24 && narrow.iter().all(|l| l.width() <= 40), "{narrow:?}");
            assert!(narrow.join("\n").contains("○ Local") && narrow.join("\n").contains("○ 停着"), "{narrow:?}");
            // 清单坏了的标出来
            set(vec![Row { id: "bad".into(), problem: Some("清单读不懂".into()), ..Default::default() }]);
            let t = text(&page(), 110).join("\n");
            assert!(t.contains("✕ bad") && t.contains("有问题") && t.contains("清单有问题:清单读不懂"), "{t}");
            // 要用的程序这台机器上没有:列表里标出来
            set(vec![Row { id: "x".into(), name: "X".into(), enabled: true, lacks: vec!["jq".into()], ..Default::default() }]);
            assert!(text(&page(), 110).join("\n").contains("缺程序"));
            // 一个都没有:告诉怎么装
            set(vec![]);
            let t = text(&page(), 110).join("\n");
            assert!(t.contains("还没装插件") && t.contains("别处的按 i"), "{t}");
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
            // 高亮到了右边:设置那行标出来,底下是它的键,左边那行不再铺底色
            assert!(t.contains("▌ say") && t.contains("⏎ 改") && t.contains("- 默认") && t.contains("Esc 回列表"), "{t}");
            assert!(!t.contains("空格 开/停"), "{t}");
            // 在右边改:⏎ 打字,打字时 q 是字;Esc 先退出打字,再回列表,再关
            press(&mut p, KeyCode::Enter);
            assert!(p.typing());
            assert_eq!(press(&mut p, KeyCode::Char('q')).1, Flow::Stay);
            let (fx, _) = press(&mut p, KeyCode::Enter);
            assert_eq!(fx.job, Some(Job::Plugins(Act::Setting("aca.delegate".into(), "say".into(), Some("continueq".into())))));
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Stay);
            assert!(text(&p, 110).join("\n").contains("Esc 关闭"));
            assert_eq!(press(&mut p, KeyCode::Esc).1, Flow::Close);
            // 没有能改的:⏎ 说一声,高亮留在列表
            let mut p = page();
            press(&mut p, KeyCode::Char('j'));
            let (fx, _) = press(&mut p, KeyCode::Enter);
            assert!(fx.flash.unwrap().contains("Local 没有能改的"));
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
            // 停着的那个:空格是开
            press(&mut p, KeyCode::Char('j'));
            assert_eq!(press(&mut p, KeyCode::Char(' ')).0.job, Some(Job::Plugins(Act::Enable("my.local".into(), true))));
            // x 第一次只提醒,写清楚会怎样;第二次才删;中间按别的就算了
            assert_eq!(press(&mut p, KeyCode::Char('x')).0.job, None);
            let t = text(&p, 110).join("\n");
            assert!(t.contains("再按 x 删掉 my.local:取消登记(目录是你的,不动)") && t.contains("x 删掉 • Esc 不删"), "{t}");
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
            assert!(!text(&page(), 110).join("\n").contains("对不上"));
            DRIFT.with(|d| *d.borrow_mut() = vec!["要装 aca.bark".into(), "要删 my.local".into()]);
            let t = text(&page(), 110).join("\n");
            DRIFT.with(|d| d.borrow_mut().clear());
            assert!(t.contains("plugins.toml 跟本机对不上:要装 aca.bark、要删 my.local(S 照它同步)"), "{t}");
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
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("没改"));
            let (e1, e2) = (ed(true), ed(false));
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&f, "b").unwrap();
            let mut fx = Fx::default();
            back_from(e1, &mut fx);
            assert_eq!(fx.job, Some(Job::Plugins(Act::AfterEdit)));
            // 你自己的清单:改了马上生效,不用同步
            let mut fx = Fx::default();
            back_from(e2, &mut fx);
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("马上生效"));
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
            assert!(t.contains("可装") && t.contains("+ MCP Gateway"), "{t}");
            // 装了的排前面,可装的在「可装」那一行下面
            assert!(t.find("Local").unwrap() < t.find("可装").unwrap(), "{t}");
            press(&mut p, KeyCode::Char('j'));
            press(&mut p, KeyCode::Char('j'));
            let t = text(&p, 110).join("\n");
            assert!(t.contains("MCP Gateway  0.1.0") && t.contains("还没装") && t.contains("aca.gateway · aca 自带"), "{t}");
            assert!(t.contains("所有 agent 共用的 MCP") && t.contains("装上会带来") && t.contains("给 agent  一个 MCP 服务"), "{t}");
            assert!(t.contains("设置      servers") && t.contains(" ⏎ 装上 ") && t.contains("i 从别处装"), "{t}");
            // 还没装:删、更新、日志都说先装;⏎ / 空格装
            let (fx, _) = press(&mut p, KeyCode::Char('x'));
            assert_eq!(fx.job, None);
            assert!(fx.flash.unwrap().contains("aca.gateway 还没装"));
            assert_eq!(press(&mut p, KeyCode::Enter).0.job, Some(Job::Plugins(Act::Install("aca.gateway".into()))));
            assert_eq!(press(&mut p, KeyCode::Char(' ')).0.job, Some(Job::Plugins(Act::Install("aca.gateway".into()))));
            // 窄屏也不出界
            assert!(text_in(&p, 40, 24).iter().all(|l| l.width() <= 40), "{:?}", text_in(&p, 40, 24));
            // 只有可装的:左边说还没装插件
            set(vec![Row { id: "aca.bark".into(), from: Origin::Bundled, available: true, ..Default::default() }]);
            let t = text(&page(), 110).join("\n");
            assert!(t.contains("还没装插件") && t.contains("+ aca.bark"), "{t}");
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
            assert!(t.contains("从别处装") && t.contains("从哪装 > x/aca-delegate▏") && t.contains("⏎ 装"), "{t}");
            // 打字时 q、x 是字,不是关掉 / 删除
            assert_eq!(press(&mut p, KeyCode::Char('q')).1, Flow::Stay);
            press(&mut p, KeyCode::Backspace);
            assert_eq!(press(&mut p, KeyCode::Enter).0.job, Some(Job::Plugins(Act::Install("x/aca-delegate".into()))));
            assert!(!p.typing());
            // 空的不装;Esc 只退出输入
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
            assert!(t.contains("「主线」、「读登录」 开着,起来时带的插件和现在不一样:重启它们才带上"), "{t}");
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
                origin: "/c/aca/user/aca-plugin.toml".into(),
                path: "/c/aca/user".into(),
                ..Default::default()
            });
            set(rows);
            let mut p = page();
            press(&mut p, KeyCode::Char('j'));
            press(&mut p, KeyCode::Char('j'));
            let t = text(&p, 120).join("\n");
            // 名字叫「你自己的」,不写版本
            assert!(t.contains("▌● 你自己的") && t.contains("user · /c/aca/user/aca-plugin.toml") && !t.contains("你自己的  0"), "{t}");
            assert!(t.contains("e 改清单"), "{t}");
            let (fx, _) = press(&mut p, KeyCode::Char('x'));
            assert!(fx.job.is_none() && fx.flash.unwrap().contains("删掉 /c/aca/user/aca-plugin.toml 就没了"));
            assert!(!p.armed);
            // e:你自己的那份打开它的清单
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
        // 改完重读:还在右边
        p.reload();
        assert_eq!(p.focus, Focus::Detail);
        press(&mut p, KeyCode::Esc);
        press(&mut p, KeyCode::Char('j'));
        // 前面那个删掉了:高亮还在 my.local 上
        set(two().into_iter().skip(1).collect());
        p.reload();
        assert_eq!(p.cur().map(|r| r.id.as_str()), Some("my.local"));
    }
}
