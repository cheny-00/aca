//! 清单 `aca-plugin.toml`:插件里有什么。读进来、查一遍,链接、调用时都从这里拿。
//!
//! 一个插件能带的几样(都可以不写):
//! - **能力** `[[commands]]`(老写法 `[[actions]]` 照认):一条命令,带参数说明;`expose` 说露给谁 ——
//!   `menu` 界面(菜单、键)、`cli` 命令行(`aca plugin run`)、`agent` 给 agent 当工具用、
//!   `agent?` 能给 agent 但默认不给(你在 plugins.toml 的 `tools` 里点了名才给,见 [`super::yours`])
//! - 能力、窗格、视图都能写 `input`(把会话的最后一条回复 / 屏幕 / 回滚交给它:命令从标准输入读,
//!   窗格读 `ACA_INPUT_FILE`)和 `cwd = "session"`(在会话的目录里跑,不在插件目录里)
//! - **事件钩子** `[[events]]`:某件事发生时跑一条命令;`if` 写过滤条件,不匹配不起进程
//! - **窗格** `[[panes]]`:一个终端程序,开在弹窗 / 分屏 / 新窗口里
//! - **视图** `[[views]]`:一条命令的输出,放进 aca 自己的阅读页(翻页、搜索、复制;手机上好用)。
//!   `rows = true` 的是列表:命令输出一份 JSON(一行一条,见 [`super::ViewRows`]),`[[views.keys]]` 说每行能按哪些键
//!   (复制一个字段、带着这一行跑这个插件的某个能力),按完列表重读
//! - **键** `[[keys]]`:界面 `:` 菜单里按一个键直接跑某个能力 / 窗格 / 视图
//! - **定时** `[[timers]]`:每隔一阵跑这个插件的一个能力;`manual` 的平时关着,插件自己开关(见 [`super::timer`])
//! - **热键** `[[hotkeys]]`:在会话里面(不在界面里)不按前缀直接按的键,开这个插件的一个窗格。aca 把它绑在 tmux 上,
//!   和 Ctrl+Q 一个办法(见 [`crate::tmux::bind_hotkeys`]):只在 aca 的会话里拦,别的会话里键照样给里面的程序
//! - **依赖** `requires`:只能是第一方插件(`aca.core` 这种),装它时一起装上
//! - **要用的程序** `needs`:外面的命令(`jq`、`tailscale`;`a|b` = 有一个就行)。aca 不替它装,只在装的时候、
//!   `aca plugin check`、插件页里说这台机器上缺了哪个
//! - **设置** `[settings.<名字>]`:默认值和说明;值写在 `plugins.toml`,交给命令时在环境里
//! - **给 agent 的** `[agent]`:一个 Claude 格式的插件目录(skills/、commands/),两家共用;`[agent.mcp]` 一个 MCP 服务;
//!   `auto` 指一个开关设置,开着时命令头上的 `disable-model-invocation` 不作数(见 [`super::mount`])
//!   (老写法 `[agents.claude] plugin_dir` 照认,只给 Claude)
//!
//! `api` 是插件照着写的那一版插件接口(不写 = 1,见 [`API`]);写得比这个 aca 新的读不进来。
//! 不认识的字段不拦着,提醒一声(多半是拼错了,或者写到了别的表底下)

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::paths::Res;
use crate::{t, tr};

pub const MANIFEST: &str = "aca-plugin.toml";

/// 这个 aca 认的插件接口版本(清单的 `api`)。同一版里只加不改:新的可选字段、新的环境变量、新的事件和字段;
/// 要删要改名就是下一版。约定写在 docs/plugins.md「接口约定」
pub const API: u32 = 1;

/// 参数认的类型;后面带 `?` 的可以不给
pub const ARG_TYPES: &[&str] = &["string", "path", "int", "bool"];
/// 能力能露给的几处(`agent?`:能给 agent,默认不给)
pub const EXPOSE: &[&str] = &["menu", "cli", "agent", "agent?"];
/// `input` 认的几样:最后一条回复、屏幕上现在的字、连回滚一起
pub const INPUTS: &[&str] = &["reply", "screen", "scrollback"];
/// `:` 菜单里键留给界面自己的:`/` 筛
pub const MENU_KEYS_TAKEN: &[&str] = &["/"];
/// 热键不给的:Ctrl+Q 是 aca 自己的(回来);剩下几个终端里另有意思(中断、文件尾、挂起、回车、Tab、换行、退格)。
/// 再加上 Alt 的(`C-M-c`)也不给:绑定还没补上的时候(tmux 刚重启),这个键发给 agent 的是 Esc 加 Ctrl+C
pub const HOTKEYS_TAKEN: &[&str] = &["C-q", "C-c", "C-d", "C-z", "C-m", "C-i", "C-j", "C-h"];
/// `[agent] for` 认的几家
pub const AGENTS: &[&str] = &["claude", "codex"];
/// 起的时候带得进插件 MCP 服务的几家(插件目录只有 [`AGENTS`] 那几家认)
pub const MCP_AGENTS: &[&str] = &["claude", "codex", "pi", "opencode"];
/// `contexts` 认的几处:在会话上、在分组上、哪儿都行
pub const CONTEXTS: &[&str] = &["session", "group", "global"];

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Action {
    pub id: String,
    pub title: String,
    /// 给 agent 看的那段说明(它决定什么时候调);不写用 title
    pub desc: String,
    /// 在什么上面才有意义:session / group / global;不写 = 哪儿都行(界面 `:` 菜单按高亮的那行挑)
    pub contexts: Vec<String>,
    /// 参数:名字 → 类型(`string` / `path` / `int` / `bool`,后面带 `?` = 可以不给)。
    /// 界面拿它生成表单,MCP 拿它生成参数说明;调的时候以 JSON 放在 `ACA_PLUGIN_ARGS_JSON` 里
    pub args: BTreeMap<String, String>,
    /// 露给谁:menu / cli / agent / agent?。不写 = menu + cli(不写 agent 就不给 agent 用)
    pub expose: Vec<String>,
    /// 交给它会话的什么:reply / screen / scrollback(空 = 什么都不给)
    pub input: String,
    /// 在哪跑:空 / plugin = 插件目录,session = 会话的目录(没有会话还是插件目录)
    pub cwd: String,
    pub command: Vec<String>,
}

impl Action {
    pub fn exposed(&self, to: &str) -> bool {
        self.expose.iter().any(|e| e == to)
    }

    /// `agent?`:能给 agent,但要你点名(plugins.toml 的 `tools`)
    pub fn agent_opt_in(&self) -> bool {
        self.exposed("agent?")
    }

    /// 参数的类型,和它能不能不给
    pub fn arg(&self, name: &str) -> Option<(&str, bool)> {
        self.args.get(name).map(|t| match t.strip_suffix('?') {
            Some(base) => (base, true),
            None => (t.as_str(), false),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hook {
    pub on: String,
    /// 过滤条件:键是事件里的字段(`kind`、`data.x` 这种点号写法),加上会话的几样(`parent` 有没有派它的、
    /// `tool`、`account`、`group`);值相等才跑。值写数组 = 其中一个就行;`true` / `false` = 有没有(非空)
    #[serde(rename = "if")]
    pub cond: BTreeMap<String, toml::Value>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct PaneDef {
    pub id: String,
    pub title: String,
    pub contexts: Vec<String>,
    pub placement: String,
    /// 格数(`20`)或者占终端的百分比(`"80%"`)
    pub width: Option<toml::Value>,
    pub height: Option<toml::Value>,
    /// 交给它会话的什么(写进一个文件,路径在 `ACA_INPUT_FILE`):reply / screen / scrollback
    pub input: String,
    /// 空 / plugin = 插件目录,session = 会话的目录
    pub cwd: String,
    pub command: Vec<String>,
}

/// 一条命令的输出,放进阅读页;`r` 重跑。`rows = true`:输出是一份列表(JSON),每行能按 `keys` 里的键
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct View {
    pub id: String,
    pub title: String,
    pub contexts: Vec<String>,
    pub command: Vec<String>,
    pub rows: bool,
    pub keys: Vec<RowKey>,
    /// 交给它会话的什么(标准输入):reply / screen / scrollback
    pub input: String,
    /// 空 / plugin = 插件目录,session = 会话的目录
    pub cwd: String,
}

/// 列表视图里的一个键:复制(`copy`)或者跑这个插件的一个能力(`run`,参数从这一行填)
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct RowKey {
    /// 一个字符,或者 `enter`
    pub key: String,
    /// 键旁边写的
    pub title: String,
    /// 跑这个插件里的哪个能力(露没露给菜单都行)
    pub run: String,
    /// 能力的参数:名字 → 模板(`{id}`、`{url}`:这一行的字段);能力要的别的参数先弹表单问
    pub args: BTreeMap<String, String>,
    /// 复制这段(模板)
    pub copy: String,
    /// 按两次才算(撤掉、删除这种)
    pub confirm: bool,
    /// 要停在某一行上才有意义;false = 列表空着也能按(新加一条、现在去取)
    pub row: bool,
}

impl Default for RowKey {
    fn default() -> Self {
        RowKey {
            key: String::new(),
            title: String::new(),
            run: String::new(),
            args: BTreeMap::new(),
            copy: String::new(),
            confirm: false,
            row: true,
        }
    }
}

/// 列表视图里键留给界面自己的:上下、关掉
pub const ROW_KEYS_TAKEN: &[&str] = &["j", "k", "q"];

/// 定时:每隔 `every` 跑这个插件的一个能力(`run`)
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Timer {
    /// 能力的 id:不能有必填的参数,到点跑的时候没有会话
    pub run: String,
    /// 30s / 10m / 2h / 1d,最短 1m
    pub every: String,
    /// 平时关着,插件自己 `aca plugin timer on / off`(有活才开);不写 = 插件开着就开着
    pub manual: bool,
}

impl Timer {
    /// 隔几秒(读清单时查过,读不懂的进不来)
    pub fn secs(&self) -> u64 {
        super::timer::parse_every(&self.every).unwrap_or(0)
    }
}

/// `:` 菜单里的一个键
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Key {
    /// 一个字符
    pub key: String,
    /// 同一个插件里的能力 / 窗格 / 视图的 id
    pub run: String,
}

/// 会话里面的一个热键
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Hotkey {
    /// tmux 的键名,要带 Ctrl / Alt:`C-]`、`M-y`、`C-M-y`;或者 `F1`–`F12`
    pub key: String,
    /// 同一个插件里的窗格的 id
    pub run: String,
}

/// 能当热键的键(tmux 的键名)。光一个字符的不行:它会把你在会话里打的字截走。
/// 后面那个字符只认小写字母、数字和 `, . / = -`(光 Ctrl 的还有 `]` `^`):别的在 tmux 的键表里要转义,
/// Alt 加大写字母、`[` `]` 发出来的又是终端里别的序列的开头(ESC O、ESC [、ESC ])
pub fn hotkey_ok(k: &str) -> Res<()> {
    let f = k
        .strip_prefix('F')
        .is_some_and(|n| !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()) && matches!(n.parse(), Ok(1..=12u8)));
    let (last, ctrl_only) = match ["C-M-", "M-"].iter().find_map(|p| k.strip_prefix(p)) {
        Some(c) => (c, false),
        None => (k.strip_prefix("C-").unwrap_or_default(), true),
    };
    let fits = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || ",./=-".contains(c) || (ctrl_only && "]^".contains(c));
    let chord = last.len() == 1 && last.chars().all(fits);
    if !f && !chord {
        return Err(tr!(
            "热键应写为带 Ctrl / Alt 的 tmux 键名(C-]、M-y、C-M-y)或 F1–F12:「{k}」",
            "a hotkey is a tmux key name with Ctrl / Alt (C-], M-y, C-M-y) or F1–F12: “{k}”"
        ));
    }
    if HOTKEYS_TAKEN.contains(&k.replacen("C-M-", "C-", 1).as_str()) {
        return Err(tr!(
            "{k} 不能用作热键(已被 aca 占用,或在终端中另有含义)",
            "{k} can't be a hotkey (aca's own, or it means something to the terminal)"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Setting {
    pub default: Option<toml::Value>,
    pub desc: String,
    /// 不给人看全(webhook 地址、key 这种):界面、`aca plugin setting` 列的时候打码
    pub secret: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpSpec {
    /// 起这个 MCP 服务的命令(stdio),相对插件目录
    pub command: Vec<String>,
    /// 怎么挂:auto(默认:gateway 开着就登记到 gateway,不然直接挂到 agent 上)/ direct / gateway
    pub via: String,
    /// 在 gateway 里:shared(默认,大家共用一份)/ session(每个会话一份,有状态的用这个)
    pub scope: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentSpec {
    /// 给哪几家:claude / codex。不写 = 两家都给
    #[serde(rename = "for")]
    pub targets: Vec<String>,
    /// Claude 格式的插件目录(`.claude-plugin/plugin.json`、`skills/`、`commands/`),相对插件目录
    pub dir: String,
    /// 一个开关设置的名字:它开着时,`dir` 里命令、skill 头上的 `disable-model-invocation` 不作数(agent 自己能用);
    /// 关着照文件写的来。不写 = 一直照文件写的来。只管 Claude:Codex 的 skill 本来就都在它的列表里
    pub auto: String,
    pub mcp: Option<McpSpec>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentPart {
    /// 这一家自己的插件目录(相对插件目录):起它时带上(Claude:`--plugin-dir`)
    pub plugin_dir: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// 照着哪一版插件接口写的;不写 = 1
    pub api: Option<u32>,
    pub min_aca_version: String,
    pub description: String,
    /// `[[actions]]` 和 `[[commands]]` 合在一起(读的时候合)
    pub actions: Vec<Action>,
    commands: Vec<Action>,
    pub events: Vec<Hook>,
    pub panes: Vec<PaneDef>,
    pub views: Vec<View>,
    pub keys: Vec<Key>,
    pub timers: Vec<Timer>,
    pub hotkeys: Vec<Hotkey>,
    /// 要先有的第一方插件
    pub requires: Vec<String>,
    /// 要用到的外面的程序(PATH 里找;写了 / 的看那个文件;`a|b` = 有一个就行)
    pub needs: Vec<String>,
    pub settings: BTreeMap<String, Setting>,
    pub agent: AgentSpec,
    /// 老写法:给哪家 agent 带什么(`[agents.claude]`)
    pub agents: BTreeMap<String, AgentPart>,
}

impl Manifest {
    /// 给这一家带东西的那个目录(相对插件目录):`[agent] dir`,或者老写法 `[agents.<它>] plugin_dir`
    pub fn agent_dir(&self, agent: &str) -> Option<&str> {
        if !self.agent.dir.is_empty() && self.gives(agent) {
            return Some(&self.agent.dir);
        }
        self.agents.get(agent).map(|p| p.plugin_dir.as_str()).filter(|d| !d.is_empty())
    }

    /// `[agent]` 给不给这一家(没写 for = 都给)
    pub fn gives(&self, agent: &str) -> bool {
        self.agent.targets.is_empty() || self.agent.targets.iter().any(|t| t == agent)
    }

    /// `needs` 里这台机器上找不到的
    pub fn missing(&self) -> Vec<String> {
        self.needs.iter().filter(|n| !n.split('|').any(|p| found(p.trim()))).cloned().collect()
    }

    /// 设置的默认值(没写默认值的不在里面)
    pub fn defaults(&self) -> toml::Table {
        self.settings.iter().filter_map(|(k, s)| Some((k.clone(), s.default.clone()?))).collect()
    }

    /// 它带了什么,一样一行(装的时候、插件页给人看):给 agent 的、给人的、订的事件、设置
    pub fn brings(&self) -> Vec<String> {
        let mut out = vec![];
        let mut agent = vec![];
        let dirs: Vec<&str> = AGENTS.iter().copied().filter(|a| self.agent_dir(a).is_some()).collect();
        if !dirs.is_empty() {
            agent.push(tr!(
                "{} 的插件目录(skill、斜杠命令)",
                "a plugin directory for {} (skills, slash commands)",
                dirs.join(t!("、", ", "))
            ));
        }
        if self.agent.mcp.is_some() {
            agent.push(t!("一个 MCP 服务", "an MCP server").to_string());
        }
        let tools = self.actions.iter().filter(|a| a.exposed("agent")).count();
        let ask = self.actions.iter().filter(|a| a.agent_opt_in()).count();
        match (tools, ask) {
            (0, 0) => {}
            (n, 0) => agent.push(tr!("{n} 个工具", "{n} tools")),
            (0, k) => agent.push(tr!("{k} 个工具(需明确允许)", "{k} tools (only if you allow them)")),
            (n, k) => agent.push(tr!("{} 个工具({k} 个需明确允许)", "{} tools ({k} only if you allow them)", n + k)),
        }
        if !agent.is_empty() {
            out.push(tr!("提供给 agent:{}", "for agents: {}", agent.join(" · ")));
        }
        let mut human = vec![];
        let menu = self.actions.iter().filter(|a| a.exposed("menu")).count();
        if menu > 0 {
            human.push(tr!("{menu} 个命令", "{menu} commands"));
        }
        if !self.panes.is_empty() {
            human.push(tr!("{} 个窗格", "{} panes", self.panes.len()));
        }
        if !self.views.is_empty() {
            human.push(tr!("{} 个视图", "{} views", self.views.len()));
        }
        if !human.is_empty() {
            out.push(tr!("插件菜单:{}(在会话列表按 :)", "plugin menu: {} (press : on the session list)", human.join(" · ")));
        }
        if !self.timers.is_empty() {
            let each: Vec<String> = self
                .timers
                .iter()
                .map(|x| {
                    let every = super::timer::span(x.secs());
                    if x.manual {
                        tr!("{} 每 {every}(有任务时开启)", "{} every {every} (on when there's work)", x.run)
                    } else {
                        tr!("{} 每 {every}", "{} every {every}", x.run)
                    }
                })
                .collect();
            out.push(tr!("定时:{}", "timers: {}", each.join(" · ")));
        }
        if !self.hotkeys.is_empty() {
            let keys: Vec<String> = self
                .hotkeys
                .iter()
                .map(|k| {
                    let title = self.panes.iter().find(|p| p.id == k.run).map(|p| p.title.as_str()).filter(|t| !t.is_empty());
                    format!("{} {}", k.key, title.unwrap_or(&k.run))
                })
                .collect();
            out.push(tr!(
                "热键:{}(在会话中直接按;更改:aca plugin hotkey)",
                "hotkeys: {} (press inside a session; to change: aca plugin hotkey)",
                keys.join(" · ")
            ));
        }
        if !self.events.is_empty() {
            let mut on: Vec<&str> = self.events.iter().map(|h| h.on.as_str()).collect();
            on.dedup();
            out.push(tr!("订阅事件:{}", "subscribes to: {}", on.join(" ")));
        }
        if !self.settings.is_empty() {
            let names: Vec<&str> = self.settings.keys().map(String::as_str).collect();
            out.push(tr!("设置:{}(安装后在插件页修改)", "settings: {} (change them on the plugins page)", names.join(" ")));
        }
        out
    }
}

/// 这个程序在不在:写了 / 的看那个文件,不然在 PATH 里找
fn found(prog: &str) -> bool {
    if prog.is_empty() {
        return false;
    }
    if prog.contains('/') {
        return Path::new(&crate::paths::expand(prog)).is_file();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(prog).is_file())
}

/// 缺了的程序,一句话(装的时候、插件页、`aca plugin check` 说)
pub fn missing_note(missing: &[String]) -> String {
    tr!(
        "需要 {},但本机未安装(PATH 中找不到):安装后才能使用",
        "needs {}, which isn't on this machine (not on PATH): install it first",
        missing.join(t!("、", ", "))
    )
}

pub(crate) fn id_ok(id: &str, dots: bool) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-') || (dots && c == '.'))
}

/// 读清单并检查;第二个是提醒(不拦着链接的小问题)
pub fn parse(text: &str) -> Res<(Manifest, Vec<String>)> {
    let unreadable = |e: toml::de::Error| tr!("无法解析清单:{}", "can't parse the manifest: {}", e.message());
    let mut t: toml::Table = text.parse().map_err(unreadable)?;
    localize(&mut t);
    let mut m: Manifest = toml::Value::Table(t).try_into().map_err(unreadable)?;
    if !id_ok(&m.id, true) {
        return Err(t!("清单需要 id(字母、数字、. : _ -)", "the manifest needs an id (letters, digits, . : _ -)").into());
    }
    if m.name.is_empty() || m.version.is_empty() {
        return Err(t!("清单需要 name 和 version", "the manifest needs name and version").into());
    }
    match m.api {
        Some(0) => return Err(t!("api 从 1 开始", "api starts at 1").into()),
        Some(n) if n > API => {
            return Err(tr!(
                "此插件使用插件接口第 {n} 版,当前 aca 只支持到第 {API} 版:请先升级 aca",
                "this plugin is written for plugin API {n}; this aca supports up to {API}: upgrade aca"
            ));
        }
        _ => {}
    }
    if !m.min_aca_version.is_empty() && newer(&m.min_aca_version, env!("CARGO_PKG_VERSION")) {
        return Err(tr!(
            "此插件需要 aca {} 或更高版本,当前为 {}",
            "this plugin needs aca {} or later; this is {}",
            m.min_aca_version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    let commands = std::mem::take(&mut m.commands);
    m.actions.extend(commands);
    for a in &mut m.actions {
        if a.expose.is_empty() {
            a.expose = vec!["menu".into(), "cli".into()];
        }
    }
    let mut seen = std::collections::HashMap::new();
    for (what, id, command) in m
        .actions
        .iter()
        .map(|a| (t!("动作", "action"), &a.id, &a.command))
        .chain(m.panes.iter().map(|p| (t!("窗格", "pane"), &p.id, &p.command)))
        .chain(m.views.iter().map(|v| (t!("视图", "view"), &v.id, &v.command)))
    {
        if !id_ok(id, false) {
            return Err(tr!(
                "{what}的 id「{id}」无效(只能用字母、数字、: _ -,不能含点)",
                "{what} id “{id}” is invalid (letters, digits, : _ -, no dots)"
            ));
        }
        // 能力、窗格、视图共用一套 id:键、命令行都只拿 id 找
        if let Some(first) = seen.insert(id.clone(), what) {
            return Err(if first == what {
                tr!("{what} {id} 重复", "{what} {id} is defined twice")
            } else {
                tr!(
                    "{first}和{what}都叫 {id}:能力、窗格、视图的 id 不能重复",
                    "a {first} and a {what} are both called {id}: ids are shared"
                )
            });
        }
        if command.is_empty() {
            return Err(tr!("{what} {id} 缺少 command", "{what} {id} has no command"));
        }
    }
    let mut warn: Vec<String> = text
        .parse::<toml::Table>()
        .map(|t| unknown_keys(&t))
        .unwrap_or_default()
        .into_iter()
        .map(|k| {
            tr!("清单中有无法识别的 {k}:是否拼错,或写到了其他表下?", "unknown key {k} in the manifest: a typo, or under the wrong table?")
        })
        .collect();
    for a in &m.actions {
        if let Some(e) = a.expose.iter().find(|e| !EXPOSE.contains(&e.as_str())) {
            return Err(tr!(
                "动作 {} 的 expose 值 {e} 无效,只能为 menu / cli / agent / agent?",
                "action {} exposes {e}; only menu / cli / agent / agent?",
                a.id
            ));
        }
        if a.exposed("agent") && a.agent_opt_in() {
            return Err(tr!("动作 {} 的 expose 中 agent 和 agent? 只能写一个", "action {}: expose takes agent or agent?, not both", a.id));
        }
        if (a.exposed("agent") || a.agent_opt_in()) && (a.id.contains(':') || a.id.contains("__")) {
            return Err(tr!(
                "动作 {} 提供给 agent:id 中不能含 : 或 __(会用它组成 MCP 工具名)",
                "action {} is for agents: its id can't contain : or __ (it becomes part of an MCP tool name)",
                a.id
            ));
        }
        let mut vars = std::collections::HashMap::new();
        for (name, ty) in &a.args {
            if !id_ok(name, false) {
                return Err(tr!("动作 {} 的参数名「{name}」无效", "action {} has an invalid argument name “{name}”", a.id));
            }
            if let Some(other) = vars.insert(var("ACA_ARG_", name), name) {
                return Err(tr!(
                    "动作 {} 的参数 {other} 和 {name} 传给命令时都叫 {}",
                    "action {}: arguments {other} and {name} both become {}",
                    a.id,
                    var("ACA_ARG_", name)
                ));
            }
            if !ARG_TYPES.contains(&ty.trim_end_matches('?')) {
                return Err(tr!(
                    "动作 {} 的参数 {name} 类型为 {ty},只能为 string / path / int / bool(可加 ?)",
                    "action {} argument {name} is {ty}; only string / path / int / bool (optionally with ?)",
                    a.id
                ));
            }
        }
        if (a.exposed("agent") || a.agent_opt_in()) && a.title.is_empty() && a.desc.is_empty() {
            warn.push(tr!(
                "动作 {} 提供给 agent,但缺少 title / desc:agent 无法判断何时调用它",
                "action {} is exposed to agents without a title / desc: they won't know when to use it",
                a.id
            ));
        }
    }
    for h in &m.events {
        if h.command.is_empty() {
            return Err(tr!("事件 {} 缺少 command", "event {} has no command", h.on));
        }
        if !crate::event::KNOWN.iter().any(|k| crate::event::matches(&h.on, k)) {
            warn.push(tr!("没有事件 {}", "no event {}", h.on));
        }
        if let Some((k, _)) = h.cond.iter().find(|(_, v)| !plain(v)) {
            return Err(tr!(
                "事件 {} 的 if 中 {k} 只能为字符串、数字、true / false 或它们的数组",
                "event {}: if.{k} takes text, a number, true / false, or an array of those",
                h.on
            ));
        }
    }
    for p in &m.panes {
        if !matches!(p.placement.as_str(), "" | "popup" | "split" | "window") {
            return Err(tr!(
                "窗格 {} 的 placement 值 {} 无效,只能为 popup / split / window",
                "pane {} has placement {}; only popup / split / window",
                p.id,
                p.placement
            ));
        }
    }
    let contexts = m.actions.iter().map(|a| (&a.id, &a.contexts)).chain(m.panes.iter().map(|p| (&p.id, &p.contexts)));
    for (id, cs) in contexts.chain(m.views.iter().map(|v| (&v.id, &v.contexts))) {
        if let Some(c) = cs.iter().find(|c| !CONTEXTS.contains(&c.as_str())) {
            return Err(tr!(
                "{id} 的 contexts 值 {c} 无效,只能为 session / group / global",
                "{id} has context {c}; only session / group / global"
            ));
        }
    }
    let io = m
        .actions
        .iter()
        .map(|a| (&a.id, &a.input, &a.cwd))
        .chain(m.panes.iter().map(|p| (&p.id, &p.input, &p.cwd)))
        .chain(m.views.iter().map(|v| (&v.id, &v.input, &v.cwd)));
    for (id, input, cwd) in io {
        if !input.is_empty() && !INPUTS.contains(&input.as_str()) {
            return Err(tr!(
                "{id} 的 input 值 {input} 无效,只能为 reply / screen / scrollback",
                "{id} has input {input}; only reply / screen / scrollback"
            ));
        }
        if !matches!(cwd.as_str(), "" | "plugin" | "session") {
            return Err(tr!("{id} 的 cwd 值 {cwd} 无效,只能为 plugin / session", "{id} has cwd {cwd}; only plugin / session"));
        }
    }
    for v in &m.views {
        if !v.rows && !v.keys.is_empty() {
            return Err(tr!("视图 {} 写了 keys,但它不是列表(需 rows = true)", "view {} has keys but isn't a list (rows = true)", v.id));
        }
        let mut taken = std::collections::HashSet::new();
        for k in &v.keys {
            let one = k.key.chars().count() == 1 && !k.key.trim().is_empty();
            if !(one || k.key == "enter") {
                return Err(tr!("视图 {} 的键应为单个字符或 enter:「{}」", "view {}: a key is one character or enter: “{}”", v.id, k.key));
            }
            if ROW_KEYS_TAKEN.contains(&k.key.as_str()) {
                return Err(tr!("视图 {} 的键 {} 已被界面占用", "view {}: key {} is taken by the interface", v.id, k.key));
            }
            if !taken.insert(&k.key) {
                return Err(tr!("视图 {} 的键 {} 重复", "view {}: key {} is bound twice", v.id, k.key));
            }
            match (k.run.is_empty(), k.copy.is_empty()) {
                (true, true) | (false, false) => {
                    return Err(tr!(
                        "视图 {} 的键 {} 需要写 run / copy 其中之一",
                        "view {}: key {} needs exactly one of run / copy",
                        v.id,
                        k.key
                    ));
                }
                (false, true) => {
                    let Some(a) = m.actions.iter().find(|a| a.id == k.run) else {
                        return Err(tr!(
                            "视图 {} 的键 {} 要运行的 {} 不是此插件的能力",
                            "view {}: key {} runs {}, which isn't a command of this plugin",
                            v.id,
                            k.key,
                            k.run
                        ));
                    };
                    if let Some(n) = k.args.keys().find(|n| !a.args.contains_key(*n)) {
                        return Err(tr!("视图 {} 的键 {}:{} 没有参数 {n}", "view {}: key {}: {} has no argument {n}", v.id, k.key, k.run));
                    }
                }
                (true, false) => {}
            }
        }
    }
    let mut keys = std::collections::HashSet::new();
    for k in &m.keys {
        if k.key.chars().count() != 1 || k.key.trim().is_empty() {
            return Err(tr!("[[keys]] 的 key 应为单个字符:「{}」", "a [[keys]] key is one character: “{}”", k.key));
        }
        if MENU_KEYS_TAKEN.contains(&k.key.as_str()) {
            return Err(tr!("键 {} 已被 : 菜单占用(用于筛选)", "key {} is taken by the : menu (filter)", k.key));
        }
        if !keys.insert(&k.key) {
            return Err(tr!("键 {} 重复", "key {} is bound twice", k.key));
        }
        let menu = m.actions.iter().any(|a| a.id == k.run && a.exposed("menu"));
        if !menu && !m.panes.iter().any(|p| p.id == k.run) && !m.views.iter().any(|v| v.id == k.run) {
            return Err(tr!(
                "键 {} 要运行的 {} 不是此插件在菜单中提供的能力、窗格或视图",
                "key {} runs {}, which isn't a menu command, pane or view of this plugin",
                k.key,
                k.run
            ));
        }
    }
    let mut runs = std::collections::HashSet::new();
    for t in &m.timers {
        let Some(a) = m.actions.iter().find(|a| a.id == t.run) else {
            return Err(tr!("定时任务要运行的 {} 不是此插件的能力", "a timer runs {}, which isn't a command of this plugin", t.run));
        };
        if let Some(k) = a.args.keys().find(|k| a.arg(k).is_some_and(|(_, opt)| !opt)) {
            return Err(tr!(
                "定时运行 {} 时无法提供参数,但它的参数 {k} 是必填的",
                "a timer runs {} with no arguments, but its argument {k} is required",
                t.run
            ));
        }
        if !a.input.is_empty() || (!a.contexts.is_empty() && !a.contexts.iter().any(|c| c == "global")) {
            return Err(tr!(
                "定时运行 {} 时没有会话:它不能写 input,contexts 中需包含 global(或省略 contexts)",
                "a timer runs {} with no session: it can't have input, and its contexts need global (or none)",
                t.run
            ));
        }
        match super::timer::parse_every(&t.every) {
            None => {
                return Err(tr!(
                    "无法识别定时任务 {} 的 every:「{}」(应写为 30s / 10m / 2h / 1d)",
                    "timer {}: can't read every “{}” (write 30s / 10m / 2h / 1d)",
                    t.run,
                    t.every
                ));
            }
            Some(s) if s < super::timer::MIN_EVERY => {
                return Err(tr!("定时任务 {} 的间隔至少为 1m", "timer {}: every is at least 1m", t.run));
            }
            Some(_) => {}
        }
        if !runs.insert(&t.run) {
            return Err(tr!("定时任务 {} 重复", "timer {} is defined twice", t.run));
        }
    }
    let mut hot = std::collections::HashSet::new();
    for k in &m.hotkeys {
        hotkey_ok(&k.key)?;
        if !hot.insert(&k.key) {
            return Err(tr!("热键 {} 重复", "hotkey {} is bound twice", k.key));
        }
        if m.hotkeys.iter().any(|o| o.run == k.run && o.key != k.key) {
            return Err(tr!("窗格 {} 设了两个热键(每个窗格只能有一个)", "pane {} has two hotkeys (one per pane)", k.run));
        }
        if !m.panes.iter().any(|p| p.id == k.run) {
            return Err(tr!(
                "热键 {} 要打开的 {} 不是此插件的窗格(热键只能打开窗格)",
                "hotkey {} opens {}, which isn't a pane of this plugin (hotkeys open panes only)",
                k.key,
                k.run
            ));
        }
    }
    if let Some(r) = m.requires.iter().find(|r| !super::first_party::known(r)) {
        return Err(tr!(
            "requires 只能写第一方插件(如 aca.core),{r} 不是",
            "requires takes first-party plugins only (like aca.core); {r} isn't one"
        ));
    }
    if m.needs.iter().any(|n| n.split('|').any(|p| p.trim().is_empty())) {
        return Err(t!("needs 中有空的程序名", "needs has an empty program name").into());
    }
    if let Some(k) = m.settings.keys().find(|k| !id_ok(k, false)) {
        return Err(tr!("设置名「{k}」无效(只能用字母、数字、: _ -)", "setting name “{k}” is invalid (letters, digits, : _ -)"));
    }
    let mut vars = std::collections::HashMap::new();
    for k in m.settings.keys() {
        if let Some(other) = vars.insert(var("ACA_SETTING_", k), k) {
            return Err(tr!("设置 {other} 和 {k} 传给命令时都叫 {}", "settings {other} and {k} both become {}", var("ACA_SETTING_", k)));
        }
    }
    if let Some(a) = m.agent.targets.iter().find(|a| !AGENTS.contains(&a.as_str())) {
        return Err(tr!("[agent] for 值 {a} 无效,只能为 claude / codex", "[agent] for has {a}; only claude / codex"));
    }
    if !m.agent.auto.is_empty() {
        let auto = &m.agent.auto;
        if m.agent.dir.is_empty() {
            return Err(
                t!("[agent] auto 控制的是 dir 中的命令:缺少 dir", "[agent] auto is about the commands in dir: there is no dir").into()
            );
        }
        if !m.settings.get(auto).is_some_and(|s| matches!(s.default, Some(toml::Value::Boolean(_)))) {
            return Err(tr!(
                "[agent] auto 值 {auto} 无效:应为 [settings] 中默认值为 true / false 的设置",
                "[agent] auto is {auto}: it has to name a setting in [settings] whose default is true / false"
            ));
        }
    }
    if let Some(mcp) = &m.agent.mcp {
        if mcp.command.is_empty() {
            return Err(t!("[agent.mcp] 缺少 command", "[agent.mcp] has no command").into());
        }
        if !matches!(mcp.via.as_str(), "" | "auto" | "direct" | "gateway") {
            return Err(tr!(
                "[agent.mcp] via 值 {} 无效,只能为 auto / direct / gateway",
                "[agent.mcp] via is {}; only auto / direct / gateway",
                mcp.via
            ));
        }
        if !matches!(mcp.scope.as_str(), "" | "shared" | "session") {
            return Err(tr!(
                "[agent.mcp] scope 值 {} 无效,只能为 shared / session",
                "[agent.mcp] scope is {}; only shared / session",
                mcp.scope
            ));
        }
    }
    Ok((m, warn))
}

/// 名字 → 交给命令的环境变量名:大写,`-` `:` 换成 `_`(`max-lines` → `ACA_SETTING_MAX_LINES`)
pub(crate) fn var(prefix: &str, name: &str) -> String {
    format!("{prefix}{}", name.to_ascii_uppercase().replace(['-', ':'], "_"))
}

// 每张表认的字段。加了字段这里也要加(docs/plugins.md 里那份全的清单读出来不能有提醒,测试里查)
const TOP_KEYS: &[&str] = &[
    "id",
    "name",
    "version",
    "api",
    "min_aca_version",
    "description",
    "actions",
    "commands",
    "events",
    "panes",
    "views",
    "keys",
    "timers",
    "hotkeys",
    "requires",
    "needs",
    "settings",
    "agent",
    "agents",
];
const ACTION_KEYS: &[&str] = &["id", "title", "desc", "contexts", "args", "expose", "input", "cwd", "command"];
const HOOK_KEYS: &[&str] = &["on", "if", "command"];
const PANE_KEYS: &[&str] = &["id", "title", "contexts", "placement", "width", "height", "input", "cwd", "command"];
const VIEW_KEYS: &[&str] = &["id", "title", "contexts", "command", "rows", "keys", "input", "cwd"];
const ROW_KEY_KEYS: &[&str] = &["key", "title", "run", "args", "copy", "confirm", "row"];
const KEY_KEYS: &[&str] = &["key", "run"];
const TIMER_KEYS: &[&str] = &["run", "every", "manual"];
const SETTING_KEYS: &[&str] = &["default", "desc", "secret"];
const AGENT_KEYS: &[&str] = &["for", "dir", "auto", "mcp"];
const MCP_KEYS: &[&str] = &["command", "via", "scope"];
const AGENT_PART_KEYS: &[&str] = &["plugin_dir"];

/// 清单里不认识的字段,写成 `keys[1].requires` 这样(第几条从 1 数)。`args`、`if`、设置名这些自己起名的不算
fn unknown_keys(t: &toml::Table) -> Vec<String> {
    fn check(t: &toml::Table, at: &str, known: &[&str], out: &mut Vec<String>) {
        // 给人看的字另写一种话的那份(`desc_en`):这张表认 desc,就认它(见 [`localize`])
        let other_lang =
            |k: &str| ["_en", "_zh"].iter().any(|s| k.strip_suffix(s).is_some_and(|base| TEXTS.contains(&base) && known.contains(&base)));
        out.extend(
            t.keys()
                .filter(|k| !known.contains(&k.as_str()) && !other_lang(k))
                .map(|k| if at.is_empty() { k.clone() } else { format!("{at}.{k}") }),
        );
    }
    fn list<'a>(t: &'a toml::Table, name: &str) -> impl Iterator<Item = (usize, &'a toml::Table)> {
        t.get(name).and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_table).enumerate()
    }
    fn named<'a>(t: &'a toml::Table, name: &str) -> impl Iterator<Item = (&'a String, &'a toml::Table)> {
        t.get(name).and_then(toml::Value::as_table).into_iter().flatten().filter_map(|(k, v)| Some((k, v.as_table()?)))
    }
    let mut out = vec![];
    check(t, "", TOP_KEYS, &mut out);
    for (name, known) in [
        ("actions", ACTION_KEYS),
        ("commands", ACTION_KEYS),
        ("events", HOOK_KEYS),
        ("panes", PANE_KEYS),
        ("keys", KEY_KEYS),
        ("timers", TIMER_KEYS),
        ("hotkeys", KEY_KEYS),
    ] {
        for (i, x) in list(t, name) {
            check(x, &format!("{name}[{}]", i + 1), known, &mut out);
        }
    }
    for (i, v) in list(t, "views") {
        let at = format!("views[{}]", i + 1);
        check(v, &at, VIEW_KEYS, &mut out);
        for (j, k) in list(v, "keys") {
            check(k, &format!("{at}.keys[{}]", j + 1), ROW_KEY_KEYS, &mut out);
        }
    }
    for (name, s) in named(t, "settings") {
        check(s, &format!("settings.{name}"), SETTING_KEYS, &mut out);
    }
    if let Some(a) = t.get("agent").and_then(toml::Value::as_table) {
        check(a, "agent", AGENT_KEYS, &mut out);
        if let Some(m) = a.get("mcp").and_then(toml::Value::as_table) {
            check(m, "agent.mcp", MCP_KEYS, &mut out);
        }
    }
    for (name, p) in named(t, "agents") {
        check(p, &format!("agents.{name}"), AGENT_PART_KEYS, &mut out);
    }
    out
}

/// `if` 里能写的值:字、数、布尔,或者它们的数组
fn plain(v: &toml::Value) -> bool {
    match v {
        toml::Value::String(_) | toml::Value::Integer(_) | toml::Value::Boolean(_) => true,
        toml::Value::Array(xs) => xs.iter().all(|x| !matches!(x, toml::Value::Array(_) | toml::Value::Table(_)) && plain(x)),
        _ => false,
    }
}

/// a 比 b 新(按点分开的数字比)
pub(crate) fn newer(a: &str, b: &str) -> bool {
    let n = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    n(a) > n(b)
}

pub fn read_manifest(dir: &Path) -> Res<(Manifest, Vec<String>)> {
    let f = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&f).map_err(|e| tr!("无法读取 {}:{e}", "can't read {}: {e}", f.display()))?;
    if super::user::is_dir(dir) { parse_user(&text) } else { parse(&text) }
}

/// 给人看的几样字(名字、说明、标题)可以按语言另写一份:`description_en`、`title_zh` 这样带后缀的,
/// 界面是那种话时顶替不带后缀的那份;不带后缀的照旧是默认的、两种话都没另写时用的。
/// 旧的 aca 不认后缀,照旧用不带后缀的 —— 只加不改(见 [`API`])
const TEXTS: [&str; 4] = ["name", "description", "title", "desc"];

/// 照现在的语言把带后缀的那份换上来,带后缀的都拿掉。每一层的表、表数组里的每一张都看
fn localize(t: &mut toml::Table) {
    let mine = match crate::i18n::lang() {
        crate::i18n::Lang::Zh => "zh",
        crate::i18n::Lang::En => "en",
    };
    for key in TEXTS {
        for lang in ["en", "zh"] {
            if let Some(v) = t.remove(&format!("{key}_{lang}"))
                && lang == mine
                && v.as_str().is_some_and(|s| !s.trim().is_empty())
            {
                t.insert(key.into(), v);
            }
        }
    }
    for (_, v) in t.iter_mut() {
        match v {
            toml::Value::Table(sub) => localize(sub),
            toml::Value::Array(items) => items.iter_mut().filter_map(toml::Value::as_table_mut).for_each(localize),
            _ => {}
        }
    }
}

/// 你自己的那份(`~/.config/aca/user/`,见 [`super::user`]):id 总是 `user`,name、version 可以不写
pub fn parse_user(text: &str) -> Res<(Manifest, Vec<String>)> {
    let mut t: toml::Table =
        text.parse().map_err(|e: toml::de::Error| tr!("无法解析清单:{}", "can't parse the manifest: {}", e.message()))?;
    let mut warn = vec![];
    if let Some(id) = t.get("id").and_then(toml::Value::as_str).filter(|id| *id != super::user::ID) {
        warn.push(tr!("你的插件 id 固定为 {},写的 {id} 无效", "your own plugin's id is always {}; {id} is ignored", super::user::ID));
    }
    t.insert("id".into(), super::user::ID.into());
    t.entry("name").or_insert_with(|| t!("你的", "yours").into());
    t.entry("version").or_insert_with(|| "0".into());
    let text = toml::to_string(&t).map_err(|e| e.to_string())?;
    let (m, more) = parse(&text)?;
    warn.extend(more);
    Ok((m, warn))
}

/// 事件钩子的 `if` 对不对得上:`facts` 是事件那一行加上会话的几样(见 [`Hook::cond`])
pub fn cond_matches(cond: &BTreeMap<String, toml::Value>, facts: &serde_json::Value) -> bool {
    cond.iter().all(|(k, want)| {
        let got = facts.pointer(&format!("/{}", k.replace('.', "/")));
        match want {
            toml::Value::Array(xs) => xs.iter().any(|x| one(x, got)),
            _ => one(want, got),
        }
    })
}

fn one(want: &toml::Value, got: Option<&serde_json::Value>) -> bool {
    use serde_json::Value as J;
    let truthy = |g: Option<&J>| match g {
        None | Some(J::Null) | Some(J::Bool(false)) => false,
        Some(J::String(s)) => !s.is_empty(),
        Some(J::Number(n)) => n.as_f64() != Some(0.0),
        Some(J::Array(a)) => !a.is_empty(),
        Some(_) => true,
    };
    match want {
        toml::Value::Boolean(b) => truthy(got) == *b,
        toml::Value::String(s) => got.and_then(J::as_str) == Some(s.as_str()),
        toml::Value::Integer(n) => got.and_then(J::as_i64) == Some(*n),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V2: &str = r#"
id = "aca.delegate"
name = "Delegate"
version = "0.2.0"
[settings.max_continue]
default = 5
desc = "最多续几次"
[settings.note]
desc = "没写默认值"
[[commands]]
id = "dispatch"
title = "派一个子任务"
args = { task = "string", repo = "path?" }
expose = ["menu", "cli", "agent"]
command = ["sh", "dispatch.sh"]
[[actions]]
id = "old"
command = ["true"]
[[events]]
on = "turn.failed"
if = { kind = "transient", parent = true }
command = ["sh", "continue.sh"]
[agent]
for = ["claude", "codex"]
dir = "agent"
[agent.mcp]
command = ["./mcp"]
scope = "session"
"#;

    #[test]
    fn commands_join_actions_and_say_who_they_are_for() {
        let (m, warn) = parse(V2).unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        let ids: Vec<&str> = m.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["old", "dispatch"]);
        let (old, dispatch) = (&m.actions[0], &m.actions[1]);
        assert!(old.exposed("menu") && old.exposed("cli") && !old.exposed("agent"));
        assert!(dispatch.exposed("agent"));
        assert_eq!(
            (dispatch.arg("task"), dispatch.arg("repo"), dispatch.arg("nope")),
            (Some(("string", false)), Some(("path", true)), None)
        );
        assert_eq!(m.defaults().get("max_continue"), Some(&toml::Value::Integer(5)));
        assert!(!m.defaults().contains_key("note"));
        assert_eq!((m.agent_dir("claude"), m.agent_dir("codex"), m.agent_dir("shell")), (Some("agent"), Some("agent"), None));
        assert_eq!(m.agent.mcp.as_ref().map(|x| x.scope.as_str()), Some("session"));
    }

    #[test]
    fn the_old_agents_table_still_brings_a_directory_to_claude_only() {
        let (m, _) = parse("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[agents.claude]\nplugin_dir = \"claude\"").unwrap();
        assert_eq!((m.agent_dir("claude"), m.agent_dir("codex")), (Some("claude"), None));
    }

    #[test]
    fn manifest_mistakes_in_the_new_parts_are_named() {
        let bad = |extra: &str| parse(&format!("id = \"a\"\nname = \"x\"\nversion = \"1\"\n{extra}")).unwrap_err();
        assert!(bad("[[commands]]\nid = \"c\"\nexpose = [\"tui\"]\ncommand = [\"true\"]").contains("tui"));
        assert!(bad("[[commands]]\nid = \"c\"\nargs = { n = \"float\" }\ncommand = [\"true\"]").contains("float"));
        assert!(bad("[agent]\nfor = [\"gemini\"]").contains("gemini"));
        assert!(bad("[agent.mcp]\ncommand = []").contains("command"));
        // auto 得指一个开关设置,还得有 dir
        assert!(bad("[agent]\ndir = \"a\"\nauto = \"nope\"").contains("nope"));
        assert!(bad("[settings.n]\ndefault = 1\n[agent]\ndir = \"a\"\nauto = \"n\"").contains("true / false"));
        assert!(bad("[settings.on]\ndefault = true\n[agent]\nauto = \"on\"").contains("dir"));
        let ok = "id = \"a\"\nname = \"x\"\nversion = \"1\"\n[settings.on]\ndefault = false\n[agent]\ndir = \"a\"\nauto = \"on\"";
        let (m, warn) = parse(ok).unwrap();
        assert!(m.agent.auto == "on" && warn.is_empty(), "{warn:?}");
        assert!(bad("[agent.mcp]\ncommand = [\"x\"]\nvia = \"socket\"").contains("socket"));
        assert!(bad("[[events]]\non = \"turn.done\"\nif = { x = { y = 1 } }\ncommand = [\"true\"]").contains("if"));
        // 列表视图的键
        let view = |keys: &str| {
            bad(&format!(
                "[[commands]]\nid = \"rm\"\nargs = {{ id = \"string\" }}\ncommand = [\"true\"]\n[[views]]\nid = \"v\"\nrows = true\ncommand = [\"true\"]\n{keys}"
            ))
        };
        assert!(view("[[views.keys]]\nkey = \"x\"\nrun = \"nope\"").contains("nope"));
        assert!(view("[[views.keys]]\nkey = \"x\"\nrun = \"rm\"\nargs = { who = \"{id}\" }").contains("who"));
        assert!(view("[[views.keys]]\nkey = \"x\"").contains("run / copy"));
        assert!(view("[[views.keys]]\nkey = \"j\"\ncopy = \"{id}\"").contains("已被界面占用"));
        assert!(view("[[views.keys]]\nkey = \"xy\"\ncopy = \"{id}\"").contains("enter"));
        assert!(bad("[[views]]\nid = \"v\"\ncommand = [\"true\"]\n[[views.keys]]\nkey = \"x\"\ncopy = \"{id}\"").contains("rows"));
        let (m, _) = parse("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[commands]]\nid = \"rm\"\nargs = { id = \"string\" }\ncommand = [\"true\"]\n[[views]]\nid = \"v\"\nrows = true\ncommand = [\"true\"]\n[[views.keys]]\nkey = \"enter\"\ncopy = \"{url}\"\n[[views.keys]]\nkey = \"a\"\nrun = \"rm\"\nrow = false").unwrap();
        assert_eq!((m.views[0].keys[0].row, m.views[0].keys[1].row), (true, false));
        let (_, warn) =
            parse("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[commands]]\nid = \"c\"\nexpose = [\"agent\"]\ncommand = [\"true\"]")
                .unwrap();
        assert_eq!(warn.len(), 1, "{warn:?}");
        // input、cwd、agent?、: 菜单留给自己的键
        assert!(bad("[[commands]]\nid = \"c\"\ninput = \"clipboard\"\ncommand = [\"true\"]").contains("clipboard"));
        assert!(bad("[[panes]]\nid = \"p\"\ncwd = \"home\"\ncommand = [\"true\"]").contains("home"));
        assert!(bad("[[commands]]\nid = \"c\"\nexpose = [\"agent\", \"agent?\"]\ncommand = [\"true\"]").contains("agent?"));
        assert!(bad("[[commands]]\nid = \"c\"\ncommand = [\"true\"]\n[[keys]]\nkey = \"/\"\nrun = \"c\"").contains("/"));
    }

    #[test]
    fn a_command_can_be_offered_to_agents_only_if_you_allow_it_and_take_the_sessions_text() {
        let (m, warn) = parse(
            "id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[commands]]\nid = \"ls\"\ntitle = \"列\"\nexpose = [\"cli\", \"agent\"]\ncommand = [\"true\"]\n[[commands]]\nid = \"add\"\ntitle = \"加\"\nexpose = [\"cli\", \"agent?\"]\ninput = \"reply\"\ncwd = \"session\"\ncommand = [\"true\"]",
        )
        .unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        let (ls, add) = (&m.actions[0], &m.actions[1]);
        assert!(ls.exposed("agent") && !ls.agent_opt_in());
        assert!(!add.exposed("agent") && add.agent_opt_in());
        assert_eq!((add.input.as_str(), add.cwd.as_str()), ("reply", "session"));
        crate::i18n::with(crate::i18n::Lang::Zh, || {
            assert!(m.brings().iter().any(|l| l.contains("2 个工具(1 个需明确允许)")), "{:?}", m.brings());
        });
    }

    const BASE: &str = "id = \"t.x\"\nname = \"x\"\nversion = \"1\"\n";

    #[test]
    fn text_can_be_written_per_language_and_the_plain_one_is_the_fallback() {
        let m = format!(
            "{BASE}description = \"中文说明\"\ndescription_en = \"In English\"\n[settings.a]\ndesc = \"甲\"\ndesc_en = \"A\"\n\
             [settings.b]\ndesc = \"只有一种\"\n[[commands]]\nid = \"go\"\ntitle = \"走\"\ntitle_en = \"Go\"\ncommand = [\"true\"]\n\
             [[panes]]\nid = \"p\"\ntitle = \"Pad\"\ntitle_zh = \"便签\"\ncommand = [\"true\"]\n"
        );
        let texts = |lang| {
            let m = crate::i18n::with(lang, || parse(&m).unwrap().0);
            [
                m.description,
                m.settings["a"].desc.clone(),
                m.settings["b"].desc.clone(),
                m.actions[0].title.clone(),
                m.panes[0].title.clone(),
            ]
        };
        assert_eq!(texts(crate::i18n::Lang::En), ["In English", "A", "只有一种", "Go", "Pad"]);
        assert_eq!(texts(crate::i18n::Lang::Zh), ["中文说明", "甲", "只有一种", "走", "便签"]);
        // 带后缀的那几样不算不认识;别的后缀、这张表本来就没有的字段带后缀,照样点名
        let (_, warn) = crate::i18n::with(crate::i18n::Lang::Zh, || parse(&m).unwrap());
        assert!(warn.is_empty(), "{warn:?}");
        let odd = format!("{BASE}description_fr = \"x\"\n[settings.a]\ntitle_en = \"x\"\n");
        let (_, warn) = crate::i18n::with(crate::i18n::Lang::Zh, || parse(&odd).unwrap());
        assert!(warn.iter().any(|w| w.contains("description_fr")) && warn.iter().any(|w| w.contains("settings.a.title_en")), "{warn:?}");
    }

    #[test]
    fn a_hotkey_is_a_chord_tmux_can_bind_and_opens_a_pane() {
        let pane = "[[panes]]\nid = \"pick\"\ncommand = [\"true\"]\n";
        let with = |key: &str, run: &str| parse(&format!("{BASE}{pane}[[hotkeys]]\nkey = \"{key}\"\nrun = \"{run}\"\n"));
        let (m, warn) = with("C-]", "pick").unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!((m.hotkeys[0].key.as_str(), m.hotkeys[0].run.as_str()), ("C-]", "pick"));
        for ok in ["M-y", "C-M-y", "F5", "F12", "C-/", "M-9", "C-^"] {
            assert!(with(ok, "pick").is_ok(), "{ok}");
        }
        // 光一个字符会把打的字截走;要转义的、tmux 不认的、发出来是别的序列开头的都不行
        for no in ["y", "", "C-", "C-yy", "M-[", "M-]", "M-O", "C-M-]", "C-\\\\", "M-;", "F13", "F0", "F05", "S-y", "c-y"] {
            assert!(with(no, "pick").unwrap_err().contains("tmux"), "{no}");
        }
        assert!(with("C-q", "pick").unwrap_err().contains("C-q"));
        assert!(with("C-c", "pick").unwrap_err().contains("C-c"));
        assert!(with("C-M-c", "pick").unwrap_err().contains("C-M-c"));
        // 只开窗格;一个键、一个窗格都只能写一遍
        let cmd = format!("{BASE}[[commands]]\nid = \"go\"\ncommand = [\"true\"]\n[[hotkeys]]\nkey = \"C-]\"\nrun = \"go\"\n");
        assert!(parse(&cmd).unwrap_err().contains("只能打开窗格"));
        let twice = |a: &str, b: &str| {
            parse(&format!("{BASE}{pane}[[hotkeys]]\nkey = \"{a}\"\nrun = \"pick\"\n[[hotkeys]]\nkey = \"{b}\"\nrun = \"pick\"\n"))
        };
        assert!(twice("C-]", "C-]").unwrap_err().contains("重复"));
        assert!(twice("C-]", "M-y").unwrap_err().contains("两个热键"));
    }

    #[test]
    fn the_api_version_is_one_unless_written_and_a_newer_one_wont_read() {
        assert_eq!(parse(BASE).unwrap().0.api, None);
        assert!(parse(&format!("{BASE}api = 1\n")).is_ok());
        assert!(parse(&format!("{BASE}api = {}\n", API + 1)).unwrap_err().contains("先升级 aca"));
        assert!(parse(&format!("{BASE}api = 0\n")).unwrap_err().contains("从 1 开始"));
    }

    #[test]
    fn unknown_keys_are_named_with_where_they_landed() {
        let text = format!(
            "{BASE}descripton = \"x\"\n[[commands]]\nid = \"go\"\ntitel = \"y\"\ncommand = [\"true\"]\nargs = {{ anything = \"string\" }}\n\
             [[views]]\nid = \"v\"\nrows = true\ncommand = [\"true\"]\n[[views.keys]]\nkey = \"x\"\nrun = \"go\"\nconfrim = true\n\
             [settings.n]\ndefault = 1\nsecrt = true\n[agent]\ndir = \"a\"\n[agent.mcp]\ncommand = [\"m\"]\nvai = \"auto\"\n\
             [[keys]]\nkey = \"g\"\nrun = \"go\"\nrequires = [\"aca.core\"]\n"
        );
        let (_, warn) = parse(&text).unwrap();
        let named: Vec<&str> = warn.iter().filter_map(|w| w.strip_prefix("清单中有无法识别的 ")?.split(':').next()).collect();
        assert_eq!(
            named,
            ["descripton", "commands[1].titel", "keys[1].requires", "views[1].keys[1].confrim", "settings.n.secrt", "agent.mcp.vai"],
            "{warn:?}"
        );
        // 老写法、自己起名的(参数、if、设置名)不算
        assert!(parse(V2).unwrap().1.is_empty());
    }

    #[test]
    fn names_that_would_collide_once_turned_into_variables_or_tool_names_are_refused() {
        let args = format!("{BASE}[[commands]]\nid = \"go\"\nargs = {{ a-b = \"string\", a_b = \"int\" }}\ncommand = [\"true\"]\n");
        assert!(parse(&args).unwrap_err().contains("ACA_ARG_A_B"));
        let settings = format!("{BASE}[settings.max-lines]\ndefault = 1\n[settings.max_lines]\ndefault = 2\n");
        assert!(parse(&settings).unwrap_err().contains("ACA_SETTING_MAX_LINES"));
        let tool = format!("{BASE}[[commands]]\nid = \"a__b\"\ntitle = \"t\"\nexpose = [\"agent\"]\ncommand = [\"true\"]\n");
        assert!(parse(&tool).unwrap_err().contains("MCP"));
        // 不给 agent 的照样能用 :
        assert!(parse(&format!("{BASE}[[commands]]\nid = \"a:b\"\ncommand = [\"true\"]\n")).is_ok());
        let shared = format!("{BASE}[[commands]]\nid = \"go\"\ncommand = [\"true\"]\n[[views]]\nid = \"go\"\ncommand = [\"true\"]\n");
        assert!(parse(&shared).unwrap_err().contains("id 不能重"), "{:?}", parse(&shared));
    }

    /// 定时跑这个插件的一个能力:到点时没有会话、没人给参数,所以能力不能要这些;every 要读得懂、不短于 1m
    #[test]
    fn a_timer_runs_a_command_that_needs_no_session_or_arguments_at_least_a_minute_apart() {
        let with = |cmd: &str, timer: &str| {
            format!("{BASE}[[commands]]\nid = \"go\"\n{cmd}command = [\"true\"]\n[[timers]]\nrun = \"go\"\n{timer}")
        };
        let (m, warn) = parse(&with("", "every = \"5m\"\nmanual = true\n")).unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!((m.timers[0].secs(), m.timers[0].manual), (300, true));
        crate::i18n::with(crate::i18n::Lang::Zh, || {
            assert!(m.brings().iter().any(|l| l == "定时:go 每 5m(有任务时开启)"), "{:?}", m.brings());
            let err = |cmd: &str, timer: &str| parse(&with(cmd, timer)).unwrap_err();
            assert!(err("args = { n = \"int\" }\n", "every = \"5m\"\n").contains("参数 n 是必填的"));
            assert!(parse(&with("args = { n = \"int?\" }\n", "every = \"5m\"\n")).is_ok());
            assert!(err("contexts = [\"session\"]\n", "every = \"5m\"\n").contains("没有会话"));
            assert!(parse(&with("contexts = [\"session\", \"global\"]\n", "every = \"5m\"\n")).is_ok());
            assert!(err("input = \"reply\"\n", "every = \"5m\"\n").contains("没有会话"));
            assert!(err("", "every = \"30s\"\n").contains("至少为 1m"));
            assert!(err("", "every = \"often\"\n").contains("无法识别"));
            assert!(err("", "every = \"5m\"\n[[timers]]\nrun = \"go\"\nevery = \"1h\"\n").contains("重复"));
            let nothing = format!("{BASE}[[timers]]\nrun = \"gone\"\nevery = \"5m\"\n");
            assert!(parse(&nothing).unwrap_err().contains("不是此插件的能力"));
            // 写错的字段照样点名
            assert!(parse(&with("", "every = \"5m\"\nevry = 1\n")).unwrap().1.iter().any(|w| w.contains("timers[1].evry")));
        });
    }

    /// docs/plugins.md(英文)和 plugins.zh-CN.md「清单」那份写全了的例子:读得进来,一条提醒都没有(字段表和文档对得上)
    #[test]
    fn the_full_example_in_the_docs_reads_without_a_single_note() {
        for (docs, head) in [
            (include_str!("../../../../docs/plugins.md"), "## The manifest\n\n```toml\n"),
            (include_str!("../../../../docs/plugins.zh-CN.md"), "## 清单\n\n```toml\n"),
        ] {
            let start = docs.find(head).unwrap_or_else(|| panic!("文档里没有 {head:?} 那一节")) + head.len();
            let text = &docs[start..start + docs[start..].find("```").unwrap()];
            let (m, warn) = parse(text).unwrap();
            assert!(warn.is_empty(), "{head:?}: {warn:?}");
            assert_eq!((m.api, m.requires.as_slice()), (Some(API), ["aca.core".to_string()].as_slice()));
        }
    }

    #[test]
    fn your_own_plugin_needs_no_id_name_or_version() {
        let (m, warn) =
            parse_user("[[panes]]\nid = \"lg\"\ncwd = \"session\"\ncommand = [\"lazygit\"]\n[[keys]]\nkey = \"g\"\nrun = \"lg\"").unwrap();
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!((m.id.as_str(), m.version.as_str(), m.keys.len()), ("user", "0", 1));
        assert!(!m.name.is_empty());
        let (m, warn) = parse_user("id = \"mine\"\nname = \"我的\"").unwrap();
        assert_eq!((m.id.as_str(), m.name.as_str(), warn.len()), ("user", "我的", 1));
        assert!(parse_user("[[panes]]\nid = \"p\"").unwrap_err().contains("command"));
    }

    #[test]
    fn an_event_filter_matches_fields_session_facts_and_lists() {
        let (m, _) = parse(V2).unwrap();
        let cond = &m.events[0].cond;
        let ev = |kind: &str, parent: &str| json!({ "event": "turn.failed", "kind": kind, "parent": parent });
        assert!(cond_matches(cond, &ev("transient", "ab12")));
        assert!(!cond_matches(cond, &ev("transient", "")));
        assert!(!cond_matches(cond, &ev("fatal", "ab12")));
        let t = |s: &str| toml::from_str::<BTreeMap<String, toml::Value>>(s).unwrap();
        assert!(cond_matches(&t("kind = [\"transient\", \"account\"]"), &json!({ "kind": "account" })));
        assert!(cond_matches(&t("\"data.n\" = 3"), &json!({ "data": { "n": 3 } })));
        assert!(cond_matches(&t("missing = false"), &json!({})));
        assert!(!cond_matches(&t("missing = \"x\""), &json!({})));
        assert!(cond_matches(&BTreeMap::new(), &json!({})));
    }
}
