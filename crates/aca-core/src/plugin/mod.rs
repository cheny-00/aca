//! 插件:一个目录,里面一份 `aca-plugin.toml` 清单和它要跑的命令(照 herdr 的做法)。
//!
//! 没有 SDK,插件用什么语言写都行。清单里能写的几样见 [`manifest`]:能力(一条命令,界面里绑键 / 进菜单、
//! 命令行调、给 agent 当工具)、事件钩子、定时、窗格、设置、给 agent 带的目录和 MCP。
//! 插件要做什么就回头调 `aca` 命令行 —— 命令行就是插件的 API,`ACA_BIN` 指着现在这个程序。
//!
//! 命令以插件目录为工作目录,不经过 shell。交给它的环境:`ACA_BIN`、`ACA_ENV=1`、`ACA_PLUGIN_ID`、
//! `ACA_PLUGIN_ROOT`、`ACA_PLUGIN_CONFIG_DIR`、`ACA_PLUGIN_STATE_DIR`、`ACA_PLUGIN_CONTEXT_JSON`、`ACA_INVOKED_BY`、`ACA_LANG`(zh / en)、
//! `ACA_PLUGIN_SETTINGS_JSON` 和每个设置一个 `ACA_SETTING_<名字>`,
//! `ACA_SESSION_ID` / `ACA_SESSION_DIR` / `ACA_SESSION_PANE` / `ACA_SESSION_PARENT`(没有会话时是空的);
//! 能力、视图、窗格多 `ACA_PLUGIN_ENTRYPOINT_ID`,能力再多 `ACA_PLUGIN_ACTION_ID`(老名字)、`ACA_PLUGIN_ARGS_JSON`
//! 和每个给了的参数一个 `ACA_ARG_<名字>`,写了 `input` 的窗格多 `ACA_INPUT_FILE`,
//! 事件钩子多 `ACA_PLUGIN_EVENT` / `ACA_PLUGIN_EVENT_JSON`,定时跑的能力多 `ACA_PLUGIN_TIMER`。这是插件接口的一部分(docs/plugins.md「接口约定」,e2e 里一样一样对)。
//!
//! 要哪些插件写在 `plugins.toml`(见 [`decl`]);登记在 `~/.config/aca/plugins.json` 的是算出来的那份:目录、开没开、
//! 订了哪些事件(hook 里靠这个一眼看出没人订,不用读清单)、给哪家 agent 带什么目录(起 agent 时不用读清单)、从哪装的。
//!
//! 怎么来的四种:`link` 一个本地目录(你自己写的,aca 不动它);`install` 一个 git 仓库,克隆到
//! `~/.local/share/aca/plugins/<id>`,之后 `update` 拉新的、`remove` 连目录一起删(插件自己的配置、状态留着);
//! `install aca.<名字>` 装 aca 自带的第一方插件(见 [`first_party`]);`~/.config/aca/user/` 放了清单就有(见 [`user`])。
//!
//! 插件想要什么(给 agent 的工具、`:` 菜单里的键)由你在 plugins.toml 里定,见 [`yours`]。

pub mod decl;
pub mod dev;
pub mod first_party;
pub mod health;
mod manifest;
pub mod mount;
pub mod timer;
pub mod tools;
pub mod user;
pub mod yours;

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;

pub use manifest::{
    AGENTS, ARG_TYPES, Action, AgentPart, AgentSpec, CONTEXTS, EXPOSE, Hook, INPUTS, Key, MANIFEST, MENU_KEYS_TAKEN, Manifest, McpSpec,
    PaneDef, ROW_KEYS_TAKEN, RowKey, Setting, Timer, View, cond_matches, hotkey_ok, missing_note, parse, parse_user, read_manifest,
};

use crate::paths::{self, Res};
use crate::{t, tr};

/// 会认 `[agents.<它>] plugin_dir`(老写法)的那几家
const AGENTS_WITH_PLUGINS: &[&str] = &["claude"];

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub enabled: bool,
    /// 清单里订的事件,链接时抄下来
    pub events: Vec<String>,
    /// 给哪家 agent 带的目录(完整路径),链接时抄下来
    pub agents: BTreeMap<String, String>,
    /// 给 agent 的 MCP 服务(`[agent.mcp]`):怎么挂、几份。命令起的时候再读清单
    pub mcp: Option<McpEntry>,
    /// 露给 agent 当工具的能力(`expose` 里有 agent 的动作 id)
    pub tools: Vec<String>,
    /// 能给 agent、但要你点名的(`expose` 里写 `agent?` 的)
    pub tools_ask: Vec<String>,
    /// 从哪装的(git 地址,`--ref` 钉了版本的带着 `#版本`);空 = 链接的本地目录;[`first_party::SOURCE`] = aca 自带的;
    /// [`user::SOURCE`] = 你自己的那份
    pub source: String,
    /// 你自己的那份:登记时清单的修改时间(变了就重新登记)
    pub stamp: u64,
}

impl Entry {
    /// git 装的(不是链接的、不是自带的)
    pub fn from_git(&self) -> bool {
        !self.source.is_empty() && self.source != first_party::SOURCE
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct McpEntry {
    /// auto / direct / gateway(空 = auto)
    pub via: String,
    /// shared / session(空 = shared)
    pub scope: String,
    /// 给哪几家(空 = 两家都给)
    pub targets: Vec<String>,
}

fn registry_file() -> PathBuf {
    paths::config_dir().join("plugins.json")
}

fn read_registry() -> Vec<Entry> {
    std::fs::read_to_string(registry_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// 登记了的插件。你自己的那份(`~/.config/aca/user/`)跟着文件走:看一眼修改时间,变了先重新登记
pub fn registry() -> Vec<Entry> {
    let reg = read_registry();
    if user::refresh(&reg) { read_registry() } else { reg }
}

/// 登记(或者重新登记)你自己的那份。清单读不懂也留一个空的登记:列出来时看得到原因
fn register_user(dir: &Path, stamp: u64) {
    if register(dir, Some(user::SOURCE.into())).is_err() && !read_registry().iter().any(|e| e.source == user::SOURCE) {
        let mut reg = read_registry();
        let path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()).to_string_lossy().into_owned();
        let enabled = decl::wants().ok().and_then(|w| w.get(user::ID).map(decl::Want::on)).unwrap_or(true);
        reg.push(Entry { id: user::ID.into(), path, enabled, source: user::SOURCE.into(), ..Default::default() });
        let _ = save(&reg);
    }
    let mut reg = read_registry();
    if let Some(e) = reg.iter_mut().find(|e| e.source == user::SOURCE) {
        e.stamp = stamp;
        let _ = save(&reg);
    }
}

/// 你自己的那份没了(清单删了)
fn drop_user() {
    let mut reg = read_registry();
    reg.retain(|e| e.source != user::SOURCE);
    let _ = save(&reg);
    timer::drop_plugin(user::ID);
    health::clear(user::ID);
}

/// 登记表一变(装、删、开、停),tmux 上的热键跟着变:装上就绑,去掉就解
fn save(v: &[Entry]) -> Res<()> {
    paths::write_atomic(&registry_file(), &serde_json::to_string_pretty(v).unwrap())
        .map_err(|e| tr!("写不了 plugins.json:{e}", "can't write plugins.json: {e}"))?;
    bind_hotkeys();
    Ok(())
}

/// 本地目录(或者直接给清单的路径)→ 插件目录的完整路径
fn plugin_dir(path: &str) -> Res<PathBuf> {
    let p = PathBuf::from(paths::expand(path));
    let dir = if p.file_name().is_some_and(|n| n == MANIFEST) { p.parent().unwrap().to_path_buf() } else { p };
    dir.canonicalize().map_err(|e| format!("{}:{e}", dir.display()))
}

/// 登记(已经登记过的就地更新):读清单,抄下要的几样。`source` = 从哪装的(None = 照旧)。不碰 plugins.toml
fn register(dir: &Path, source: Option<String>) -> Res<(Manifest, Vec<String>)> {
    // 登记解开链接的路径(macOS 的 /var 是 /private/var):比给 agent 带的目录在不在插件里面时两边一样
    let dir = &dir.canonicalize().map_err(|e| format!("{}:{e}", dir.display()))?;
    let (m, mut warn) = read_manifest(dir)?;
    let mut reg = read_registry();
    // 给 agent 的工具名、Codex 插件名、MCP 服务名都拿 id 换出来(点换成 -):换出来一样的两个分不开
    let name = mount::safe(&m.id);
    if let Some(other) = reg.iter().find(|e| e.id != m.id && mount::safe(&e.id) == name) {
        return Err(tr!(
            "{} 和已经链接的 {} 在 agent 那边是同一个名字({name}):改一个 id",
            "{} and the linked {} get the same name on the agent side ({name}): change one id",
            m.id,
            other.id
        ));
    }
    let missing = m.missing();
    if !missing.is_empty() {
        warn.push(manifest::missing_note(&missing));
    }
    let agents = agent_parts(dir, &m, &mut warn)?;
    let mcp = m.agent.mcp.as_ref().map(|x| McpEntry { via: x.via.clone(), scope: x.scope.clone(), targets: m.agent.targets.clone() });
    // 开没开:plugins.toml 里写了的照它;没写的,登记过的照旧、新的开着
    let wanted = decl::wants().ok().and_then(|w| w.get(&m.id).map(decl::Want::on));
    let entry = Entry {
        id: m.id.clone(),
        path: dir.to_string_lossy().into_owned(),
        enabled: wanted.unwrap_or(true),
        events: m.events.iter().map(|h| h.on.clone()).collect(),
        agents,
        mcp,
        tools: m.actions.iter().filter(|a| a.exposed("agent")).map(|a| a.id.clone()).collect(),
        tools_ask: m.actions.iter().filter(|a| a.agent_opt_in()).map(|a| a.id.clone()).collect(),
        source: source.clone().unwrap_or_default(),
        stamp: 0,
    };
    match reg.iter_mut().find(|e| e.id == m.id) {
        Some(e) => *e = Entry { enabled: wanted.unwrap_or(e.enabled), source: source.unwrap_or_else(|| e.source.clone()), ..entry },
        None => reg.push(entry),
    }
    save(&reg)?;
    for d in [config_dir(&m.id), state_dir(&m.id)] {
        let _ = std::fs::create_dir_all(d);
    }
    // 钩子的记录按清单里第几个记:清单换了就对不上了
    health::clear(&m.id);
    // 定时照新的清单对一遍(没写 manual 的插件开着就开)
    warn.extend(timer::sync(&m.id).into_iter().map(|p| tr!("定时没办成:{p}", "timer not set up: {p}")));
    Ok((m, warn))
}

/// 清单变了(自带的插件随 aca 升级重写了):照原来的来源再登记一遍
fn reregister(dir: &Path) -> Res<(Manifest, Vec<String>)> {
    register(dir, None)
}

/// 链接一个本地目录(或者直接给清单的路径)。已经链接过的就地更新;plugins.toml 里记下这个目录
pub fn link(path: &str) -> Res<(Manifest, Vec<String>)> {
    let dir = plugin_dir(path)?;
    let (m, warn) = register(&dir, Some(String::new()))?;
    decl::set(&m.id, "source", Some(dir.to_string_lossy().into_owned().into()))?;
    Ok((m, warn))
}

/// 给 agent 带的目录:得是插件目录里真有的一个目录。不认插件目录的那家、Claude 认不出的样子,提醒一声
fn agent_parts(dir: &Path, m: &Manifest, warn: &mut Vec<String>) -> Res<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for agent in AGENTS {
        let Some(rel) = m.agent_dir(agent) else { continue };
        let old = m.agent.dir.is_empty();
        let what = if old { format!("[agents.{agent}] plugin_dir") } else { "[agent] dir".to_string() };
        let d = dir.join(rel).canonicalize().map_err(|_| tr!("{what}「{rel}」不存在", "{what} “{rel}” doesn't exist"))?;
        if !d.starts_with(dir) || !d.is_dir() {
            return Err(tr!("{what} 要是插件目录里的一个目录", "{what} has to be a directory inside the plugin"));
        }
        if !d.join(".claude-plugin").join("plugin.json").is_file() {
            warn.push(tr!(
                "{} 里没有 .claude-plugin/plugin.json:Claude、Codex 可能认不出它是插件",
                "{} has no .claude-plugin/plugin.json: Claude and Codex may not recognize it as a plugin",
                d.display()
            ));
        }
        out.insert(agent.to_string(), d.to_string_lossy().into_owned());
    }
    for agent in m.agents.keys().filter(|a| !AGENTS_WITH_PLUGINS.contains(&a.as_str())) {
        warn.push(tr!(
            "{agent} 不认老写法的插件目录:[agents.{agent}] 不会带进去",
            "{agent} ignores the old [agents.{agent}] form: not brought in"
        ));
    }
    Ok(out)
}

/// 这个目录是不是某个插件给 agent 带的(接管时认出哪些参数是 aca 带进去的):插件自己的目录,或者 aca 给 Claude 做的副本
pub fn is_agent_dir(d: &str) -> bool {
    Path::new(d).starts_with(mount::claude_root()) || registry().iter().any(|e| e.agents.values().any(|x| x == d))
}

/// git 装的插件放在这:`<数据目录>/plugins/<id>`
pub fn installed_root() -> PathBuf {
    paths::data_dir().join("plugins")
}

fn git(dir: Option<&Path>, args: &[&str]) -> Res<String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    let o = c.args(args).stdin(Stdio::null()).output().map_err(|e| tr!("git 起不来:{e}", "can't run git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(format!("git {}:{}", args[0], String::from_utf8_lossy(&o.stderr).trim()))
    }
}

/// `owner/repo` 是 GitHub 上的;别的(https、ssh、file://)照原样交给 git
fn git_url(src: &str) -> String {
    let short = src.split('/').count() == 2 && !src.contains(':') && !src.starts_with(['.', '/', '~']);
    if short { format!("https://github.com/{src}.git") } else { src.to_string() }
}

/// 克隆到 [`installed_root`] 下(`rev` 给了就切过去),登记。`want` = 只认这个 id(sync 照 plugins.toml 装时)
fn clone_in(src: &str, rev: Option<&str>, want: Option<&str>) -> Res<(Manifest, Vec<String>, String)> {
    let local = PathBuf::from(paths::expand(src));
    let url = if local.is_dir() { local.canonicalize().map_err(|e| e.to_string())?.to_string_lossy().into_owned() } else { git_url(src) };
    let root = installed_root();
    std::fs::create_dir_all(&root).map_err(|e| tr!("建不了 {}:{e}", "can't create {}: {e}", root.display()))?;
    let tmp = root.join(format!(".installing-{}", paths::short_id()));
    let placed = (|| {
        git(None, &["clone", "--quiet", &url, &tmp.to_string_lossy()])?;
        if let Some(r) = rev {
            git(Some(&tmp), &["checkout", "--quiet", r])?;
        }
        let (m, _) = read_manifest(&tmp)?;
        if let Some(w) = want.filter(|w| *w != m.id) {
            return Err(tr!("{src} 里的插件是 {},不是 plugins.toml 里写的 {w}", "{src} holds {}, not {w} as plugins.toml says", m.id));
        }
        if let Some(e) = registry().into_iter().find(|e| e.id == m.id) {
            return Err(tr!(
                "已经有 {} 了({}):要换成这个,先 aca plugin remove {}",
                "{} is already there ({}): to replace it, aca plugin remove {} first",
                m.id,
                e.path,
                m.id
            ));
        }
        let dest = root.join(&m.id);
        if dest.exists() {
            return Err(tr!("{} 已经有了", "{} already exists", dest.display()));
        }
        std::fs::rename(&tmp, &dest).map_err(|e| tr!("挪不过去:{e}", "can't move it into place: {e}"))?;
        Ok(dest)
    })();
    let dest = placed.inspect_err(|_| {
        // 克隆了一半、清单不对的:那是 aca 自己刚建的临时目录
        let _ = std::fs::remove_dir_all(&tmp);
    })?;
    let source = match rev {
        Some(r) => format!("{url}#{r}"),
        None => url,
    };
    let (m, warn) = register(&dest, Some(source))?;
    let commit = git(Some(&dest), &["rev-parse", "HEAD"])?;
    decl::lock(&m.id, Some(&commit))?;
    Ok((m, warn, commit))
}

/// `aca plugin install <来源> [--ref 版本]`:本地目录就是 [`link`](你正在写的插件,改了马上生效);
/// git 仓库克隆到 [`installed_root`] 下再链接,plugins.toml 记下从哪来、plugins.lock 记下提交。
/// `--ref` 钉在一个标签 / 分支 / 提交上(本地仓库带 `--ref` 也是克隆一份)
pub fn install(src: &str, rev: Option<&str>) -> Res<(Manifest, Vec<String>)> {
    let local = PathBuf::from(paths::expand(src));
    if first_party::known(src) && !local.is_dir() {
        return install_bundled(src);
    }
    let (m, mut warn) = if local.is_dir() && rev.is_none() {
        link(src)?
    } else {
        let (m, warn, _) = clone_in(src, rev, None)?;
        decl::set(&m.id, "source", Some(src.to_string().into()))?;
        decl::set(&m.id, "ref", rev.map(|r| r.to_string().into()))?;
        (m, warn)
    };
    warn.extend(install_requires(&m));
    Ok((m, warn))
}

/// 装 aca 自带的插件:写出来、登记,plugins.toml 里只写个表头(不写 source:换台机器 sync 也认得)
fn install_bundled(id: &str) -> Res<(Manifest, Vec<String>)> {
    if let Some(e) = registry().into_iter().find(|e| e.id == id && e.source != first_party::SOURCE) {
        return Err(tr!(
            "已经有 {id} 了({}):要换成 aca 自带的,先 aca plugin remove {id}",
            "{id} is already there ({}): to use the one aca ships with, aca plugin remove {id} first",
            e.path
        ));
    }
    let dir = first_party::unpack(id)?;
    let (m, mut warn) = register(&dir, Some(first_party::SOURCE.into()))?;
    decl::declare(id)?;
    warn.extend(install_requires(&m));
    Ok((m, warn))
}

/// 清单里 `requires` 的第一方插件,还没有的装上;返回给人看的提醒
fn install_requires(m: &Manifest) -> Vec<String> {
    let have: Vec<String> = registry().into_iter().map(|e| e.id).collect();
    let mut out = vec![];
    for r in m.requires.iter().filter(|r| !have.contains(r)) {
        out.push(match install_bundled(r) {
            Ok(_) => tr!("它要 {r},一起装上了", "it needs {r}; installed that too"),
            Err(e) => tr!("它要 {r},但没装上:{e}", "it needs {r}, which didn't install: {e}"),
        });
    }
    out
}

/// `aca plugin update <id>`:git 装的拉新的(只快进;钉了版本的不动),再重新读一遍清单、记下新的提交。返回给人看的一句
pub fn update(id: &str) -> Res<String> {
    let e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有 {id} 这个插件", "no plugin {id}"))?;
    if e.source.is_empty() {
        return Ok(tr!("{id}:链接的本地目录,不归 aca 更新", "{id}: a linked local directory; aca doesn't update it"));
    }
    if e.source == user::SOURCE {
        return Ok(tr!("{id}:你自己的(~/.config/aca/user/),不归 aca 更新", "{id}: your own (~/.config/aca/user/); aca doesn't update it"));
    }
    if e.source == first_party::SOURCE {
        first_party::unpack(id)?;
        let (m, _) = reregister(Path::new(&e.path))?;
        return Ok(tr!(
            "{id}:aca 自带的,跟着 aca 走,现在是 {}(升级 aca 就是升级它)",
            "{id}: ships with aca and follows it; now {} (upgrading aca upgrades it)",
            m.version
        ));
    }
    if let Some((_, rev)) = e.source.rsplit_once('#') {
        return Ok(tr!(
            "{id}:钉在 {rev},不更新(要换版本:改 plugins.toml 的 ref 再 aca plugin sync)",
            "{id}: pinned at {rev}, not updated (to change: edit ref in plugins.toml, then aca plugin sync)"
        ));
    }
    let dir = Path::new(&e.path);
    let before = git(Some(dir), &["rev-parse", "--short", "HEAD"])?;
    git(Some(dir), &["pull", "--quiet", "--ff-only"])?;
    let after = git(Some(dir), &["rev-parse", "--short", "HEAD"])?;
    let (m, _) = register(dir, None)?;
    decl::lock(id, Some(&git(Some(dir), &["rev-parse", "HEAD"])?))?;
    Ok(if before == after {
        tr!("{id}:已经是最新的({after})", "{id}: already up to date ({after})")
    } else {
        tr!("{id}:{before} → {after}({} {})", "{id}: {before} → {after} ({} {})", m.name, m.version)
    })
}

/// 不再登记、删掉 git 装的目录(链接的本地目录是你的,不动)。插件自己的配置、状态都留着。不碰 plugins.toml
/// 不再用一个插件时,aca 替它放的东西收走:交给它窗格的那份会话内容(`plugin-input/<插件>.<窗格>.txt`,里面是会话的回复),
/// 和 aca 替它建的配置、状态两个目录 —— 只删空的,里面有东西的是插件自己的,留着
fn sweep(id: &str) {
    for f in std::fs::read_dir(paths::state_dir().join("plugin-input")).into_iter().flatten().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        // 窗格的 id 里没有点:`a.b` 的文件不会被当成 `a` 的
        let pane = name.strip_prefix(id).and_then(|r| r.strip_prefix('.')).and_then(|r| r.strip_suffix(".txt"));
        if pane.is_some_and(|p| !p.contains('.')) {
            let _ = std::fs::remove_file(f.path());
        }
    }
    for dir in [config_dir(id), state_dir(id)] {
        let _ = std::fs::remove_dir(dir);
    }
}

fn drop_entry(e: &Entry) -> Res<String> {
    let mut reg = registry();
    reg.retain(|x| x.id != e.id);
    save(&reg)?;
    timer::drop_plugin(&e.id);
    sweep(&e.id);
    health::clear(&e.id);
    let dir = Path::new(&e.path);
    // 登记的路径是解开链接的(macOS 的 /var 是 /private/var):比的时候两边一样
    let root = if e.source == first_party::SOURCE { first_party::root() } else { installed_root() };
    let root = root.canonicalize().unwrap_or(root);
    let id = &e.id;
    if !e.source.is_empty() && dir.starts_with(&root) {
        std::fs::remove_dir_all(dir)
            .map_err(|err| tr!("取消了登记,但删不了 {}:{err}", "unregistered, but can't delete {}: {err}", dir.display()))?;
        Ok(tr!("删了 {id}({})", "removed {id} ({})", dir.display()))
    } else {
        Ok(tr!("不再用 {id}:目录 {} 是链接来的,没动", "no longer using {id}: {} is a linked directory, left alone", dir.display()))
    }
}

/// `aca plugin remove <id>`:不再用它。git 装的连目录一起删;链接的本地目录是你的,不动。
/// plugins.toml、plugins.lock 里也划掉;插件自己的配置、状态都留着
pub fn remove(id: &str) -> Res<String> {
    let e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有 {id} 这个插件", "no plugin {id}"))?;
    mine_stays(&e)?;
    let said = drop_entry(&e)?;
    decl::forget(id)?;
    decl::lock(id, None)?;
    Ok(said)
}

/// 你自己的那份删不掉(它跟着 ~/.config/aca/user/ 走)
fn mine_stays(e: &Entry) -> Res<()> {
    if e.source != user::SOURCE {
        return Ok(());
    }
    Err(tr!(
        "{} 是你自己的:删掉 {} 就没了;想先停着:aca plugin disable {}",
        "{} is your own: delete {} to drop it; to pause it: aca plugin disable {}",
        e.id,
        user::dir().join(MANIFEST).display(),
        e.id
    ))
}

pub fn unlink(id: &str) -> Res<()> {
    if let Some(e) = registry().into_iter().find(|e| e.id == id) {
        mine_stays(&e)?;
    }
    let mut reg = registry();
    let n = reg.len();
    reg.retain(|e| e.id != id);
    if reg.len() == n {
        return Err(tr!("没有链接过 {id}", "{id} isn't linked"));
    }
    save(&reg)?;
    timer::drop_plugin(id);
    sweep(id);
    health::clear(id);
    decl::forget(id)
}

pub fn enable(id: &str, on: bool) -> Res<()> {
    let mut reg = registry();
    reg.iter_mut().find(|e| e.id == id).ok_or_else(|| tr!("没有链接过 {id}", "{id} isn't linked"))?.enabled = on;
    save(&reg)?;
    let _ = timer::sync(id);
    decl::set(id, "enabled", (!on).then_some(false.into()))
}

/// `aca plugin sync`:让本机跟 plugins.toml 一致。缺的装(git 的照 plugins.lock 的提交)、没写的删、开关照它、
/// 设置和给谁带的下次用到时就是新的。返回一行一件事;一件出错不耽误别的
pub fn sync() -> Res<Vec<String>> {
    decl::ensure()?;
    let wants = decl::wants()?;
    let locked = decl::locked();
    let mut said: Vec<String> =
        first_party::refresh().into_iter().map(|id| tr!("{id}:换成这个 aca 自带的版本", "{id}: now the version this aca ships")).collect();
    for e in registry() {
        // 你自己的那份不写在 plugins.toml 里也在
        if !wants.contains_key(&e.id) && e.source != user::SOURCE {
            said.push(drop_entry(&e).unwrap_or_else(|err| format!("{}:{err}", e.id)));
            let _ = decl::lock(&e.id, None);
        }
    }
    for (id, w) in &wants {
        let had = registry().into_iter().find(|e| &e.id == id);
        if id == user::ID && had.is_none() {
            continue;
        }
        let r = match had {
            None => sync_add(id, w, locked.get(id).map(String::as_str)),
            Some(e) => sync_one(&e, w, locked.get(id).map(String::as_str)),
        };
        match r {
            Ok(Some(s)) => said.push(s),
            Ok(None) => {}
            Err(err) => said.push(format!("{id}:{err}")),
        }
    }
    // 你在 plugins.toml 里手改了热键:登记表没变,tmux 上的也要跟上
    bind_hotkeys();
    Ok(said)
}

/// plugins.toml 里有、本机还没有的:本地目录链接,git 的克隆(钉了 ref 的切到 ref,不然照锁里的提交)
fn sync_add(id: &str, w: &decl::Want, commit: Option<&str>) -> Res<Option<String>> {
    if w.source.is_empty() && first_party::known(id) {
        install_bundled(id)?;
        if !w.on() {
            enable_quiet(id, false)?;
        }
        return Ok(Some(tr!("装了 {id}(aca 自带的)", "installed {id} (ships with aca)")));
    }
    if w.source.is_empty() {
        return Err(t!("plugins.toml 里没写 source,装不了", "no source in plugins.toml; can't install").into());
    }
    let local = PathBuf::from(paths::expand(&w.source));
    let r = if local.is_dir() && w.rev.is_empty() {
        let dir = plugin_dir(&w.source)?;
        let (m, _) = register(&dir, Some(String::new()))?;
        if m.id != id {
            return Err(tr!("{} 里的插件是 {},不是 {id}", "{} holds {}, not {id}", w.source, m.id));
        }
        tr!("链接了 {id}", "linked {id}")
    } else {
        let rev = (!w.rev.is_empty()).then_some(w.rev.as_str()).or(commit);
        let (_, _, c) = clone_in(&w.source, rev, Some(id))?;
        if w.rev.is_empty() {
            // 照锁装的不算钉住:以后 update 照样拉新的
            let mut reg = registry();
            if let Some(e) = reg.iter_mut().find(|e| e.id == id) {
                e.source = e.source.split('#').next().unwrap_or("").to_string();
                let dir = PathBuf::from(&e.path);
                save(&reg)?;
                if commit.is_some() {
                    // 切到锁里的提交时 HEAD 是游离的:回到默认分支、把它放到那个提交上,update 才能快进
                    let branch = git(Some(&dir), &["rev-parse", "--abbrev-ref", "origin/HEAD"]).unwrap_or_default();
                    if let Some(b) = branch.strip_prefix("origin/") {
                        git(Some(&dir), &["checkout", "--quiet", "-B", b, &c])?;
                        let _ = git(Some(&dir), &["branch", "--quiet", "--set-upstream-to", &branch]);
                    }
                }
            }
        }
        tr!("装了 {id}({})", "installed {id} ({})", &c[..c.len().min(7)])
    };
    if !w.on() {
        enable_quiet(id, false)?;
    }
    Ok(Some(r))
}

/// 本机已经有的:开关照 plugins.toml;git 装的、没钉 ref、锁里的提交和现在不一样,切过去
fn sync_one(e: &Entry, w: &decl::Want, commit: Option<&str>) -> Res<Option<String>> {
    let mut said = vec![];
    if e.enabled != w.on() {
        enable_quiet(&e.id, w.on())?;
        said.push(if w.on() { t!("开了", "switched on") } else { t!("停了", "paused") }.to_string());
    }
    let dir = Path::new(&e.path);
    if e.from_git() && w.rev.is_empty() {
        if let Some(c) = commit {
            let head = git(Some(dir), &["rev-parse", "HEAD"])?;
            if head != c {
                if git(Some(dir), &["cat-file", "-e", &format!("{c}^{{commit}}")]).is_err() {
                    git(Some(dir), &["fetch", "--quiet"])?;
                }
                git(Some(dir), &["reset", "--quiet", "--hard", c])?;
                register(dir, None)?;
                said.push(tr!("切到 {}", "now at {}", &c[..c.len().min(7)]));
            }
        } else {
            decl::lock(&e.id, Some(&git(Some(dir), &["rev-parse", "HEAD"])?))?;
        }
    }
    Ok((!said.is_empty()).then(|| format!("{}:{}", e.id, said.join(t!(",", ", ")))))
}

/// 只改登记表里的开关(plugins.toml 已经是这样了)
fn enable_quiet(id: &str, on: bool) -> Res<()> {
    let mut reg = registry();
    if let Some(e) = reg.iter_mut().find(|e| e.id == id) {
        e.enabled = on;
    }
    save(&reg)?;
    let _ = timer::sync(id);
    Ok(())
}

/// 一个设置现在的样子
#[derive(Clone, Debug, PartialEq)]
pub struct SettingNow {
    pub name: String,
    /// 现在的值:清单的默认值,plugins.toml 里改过的盖上去;都没有 = 空字
    pub value: toml::Value,
    pub default: Option<toml::Value>,
    pub desc: String,
    /// plugins.toml 里改过
    pub changed: bool,
    /// 不给人看全(清单的 `secret`)
    pub secret: bool,
}

impl SettingNow {
    /// 给人看的值:字不带引号;secret 的打码(空的照样说空)
    pub fn shown(&self) -> String {
        let v = plain(&self.value);
        if !self.secret || v.is_empty() {
            return v;
        }
        let tail: String = v.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        if v.chars().count() > 12 { format!("••••{tail}") } else { "••••".into() }
    }
}

/// 一个插件现在的设置(清单里的顺序)
pub fn settings_of(id: &str) -> Res<Vec<SettingNow>> {
    let e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有 {id} 这个插件", "no plugin {id}"))?;
    let (m, _) = read_manifest(Path::new(&e.path))?;
    let mine = decl::want(id).settings;
    let all = decl::settings(id, &m);
    Ok(m.settings
        .iter()
        .map(|(k, s)| SettingNow {
            name: k.clone(),
            value: all.get(k).cloned().unwrap_or_else(|| toml::Value::String(String::new())),
            default: s.default.clone(),
            desc: s.desc.clone(),
            changed: mine.contains_key(k),
            secret: s.secret,
        })
        .collect())
}

/// 设置的值给人 / 脚本看:字不带引号,别的照 TOML 写
pub fn plain(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 改一个设置(写进 plugins.toml);None = 回到默认值。给的字照默认值的类型读(见 [`typed`])
pub fn set_setting(id: &str, name: &str, value: Option<&str>) -> Res<()> {
    let all = settings_of(id)?;
    let s = all.iter().find(|s| s.name == name).ok_or_else(|| tr!("{id} 没有 {name} 这个设置", "{id} has no setting {name}"))?;
    let mut table = decl::want(id).settings;
    match value {
        Some(v) => {
            table.insert(name.to_string(), typed(v, s.default.as_ref()).map_err(|e| format!("{name}:{e}"))?);
        }
        None => {
            table.remove(name);
        }
    }
    decl::set(id, "settings", (!table.is_empty()).then_some(toml::Value::Table(table)))
}

/// 照默认值的类型读你给的字:默认是字就当字(不用加引号,加了也认),整数、小数、开关要对得上,
/// 数组、表照 TOML 写;没有默认值的照 TOML 读,读不懂当字
pub fn typed(v: &str, default: Option<&toml::Value>) -> Res<toml::Value> {
    use toml::Value as V;
    let toml_of = |v: &str| format!("v = {v}").parse::<toml::Table>().ok().and_then(|mut t| t.remove("v"));
    let t = v.trim();
    match default {
        Some(V::String(_)) => Ok(match toml_of(t) {
            Some(V::String(s)) if t.starts_with(['"', '\'']) => V::String(s),
            _ => V::String(v.to_string()),
        }),
        Some(V::Integer(_)) => t.parse::<i64>().map(V::from).map_err(|_| tr!("要是整数(写的是 {t})", "takes an integer (got {t})")),
        Some(V::Float(_)) => t.parse::<f64>().map(V::from).map_err(|_| tr!("要是数(写的是 {t})", "takes a number (got {t})")),
        Some(V::Boolean(_)) => match t {
            "true" | "yes" | "on" | "1" => Ok(V::Boolean(true)),
            "false" | "no" | "off" | "0" => Ok(V::Boolean(false)),
            _ => Err(tr!("要是 true / false(写的是 {t})", "takes true / false (got {t})")),
        },
        Some(V::Array(_)) => match toml_of(t) {
            Some(a @ V::Array(_)) => Ok(a),
            _ => Err(tr!("要写成 TOML 的数组:[\"a\", \"b\"](写的是 {t})", "takes a TOML array: [\"a\", \"b\"] (got {t})")),
        },
        Some(V::Table(_)) => match toml_of(t) {
            Some(x @ V::Table(_)) => Ok(x),
            _ => Err(tr!("要写成 TOML 的表:{{ a = 1 }}(写的是 {t})", "takes a TOML table: {{ a = 1 }} (got {t})")),
        },
        _ => Ok(toml_of(t).unwrap_or_else(|| V::String(v.to_string()))),
    }
}

/// plugins.toml 和本机对不上的地方(`sync` 会做的),一样一句。文件读不懂也说一句;还没有这个文件 = 没有对不上的
pub fn drift() -> Vec<String> {
    if !decl::decl_file().exists() {
        return vec![];
    }
    let wants = match decl::wants() {
        Ok(w) => w,
        Err(e) => return vec![e],
    };
    let reg = registry();
    let mut out = vec![];
    for (id, w) in &wants {
        match reg.iter().find(|e| &e.id == id) {
            None if id == user::ID => {}
            None => out.push(tr!("要装 {id}", "install {id}")),
            Some(e) if e.enabled != w.on() => {
                out.push(if w.on() { tr!("要开 {id}", "switch on {id}") } else { tr!("要停 {id}", "pause {id}") })
            }
            Some(_) => {}
        }
    }
    for e in reg.iter().filter(|e| !wants.contains_key(&e.id) && e.source != user::SOURCE) {
        out.push(tr!("要删 {}", "remove {}", e.id));
    }
    out
}

/// 插件放自己配置 / 状态的地方:aca 只管建目录,里面的东西归插件
pub fn config_dir(id: &str) -> PathBuf {
    paths::config_dir().join("plugins").join(id)
}

pub fn state_dir(id: &str) -> PathBuf {
    paths::state_dir().join("plugins").join(id)
}

pub fn log_file(id: &str) -> PathBuf {
    paths::state_dir().join("plugin-logs").join(format!("{id}.log"))
}

/// 调动作 / 开窗格时是冲着哪个会话的
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub session: String,
    pub dir: String,
    /// 会话在 tmux 里的号(`$3`)和 agent 的窗格(`%5`);没在跑是空的
    pub tmux: String,
    pub pane: String,
    /// 派它的那个会话(`aca new` 在某个会话里建的就挂在它下面);自己开的是空的
    pub parent: String,
    /// menu / cli / event / agent / timer
    pub source: String,
}

fn find(id: &str) -> Res<(Entry, Manifest)> {
    let mut e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有链接过 {id}", "{id} isn't linked"))?;
    // 自带的:刚升级了 aca 就先换成新写出来的那份(老的可能还在调已经没了的命令)
    if e.source == first_party::SOURCE && first_party::refresh_one(id) {
        e = registry().into_iter().find(|e| e.id == id).unwrap_or(e);
    }
    if !e.enabled {
        return Err(tr!("{id} 关着(aca plugin enable {id})", "{id} is off (aca plugin enable {id})"));
    }
    let (m, _) = read_manifest(Path::new(&e.path))?;
    Ok((e, m))
}

/// `./x`、`../x` 按插件目录算(不是调用的人所在的目录,也不是 `cwd = "session"` 的会话目录)
fn program(e: &Entry, prog: &str) -> PathBuf {
    if prog.starts_with("./") || prog.starts_with("../") { Path::new(&e.path).join(prog) } else { PathBuf::from(prog) }
}

/// 在哪跑:`cwd = "session"` 且有会话 = 会话的目录,不然插件目录
fn workdir(e: &Entry, cwd: &str, ctx: &Context) -> String {
    if cwd == "session" && !ctx.dir.is_empty() && Path::new(&ctx.dir).is_dir() { ctx.dir.clone() } else { e.path.clone() }
}

fn command(e: &Entry, m: &Manifest, argv: &[String], ctx: &Context, extra: &[(&str, &str)], cwd: &str) -> Command {
    let mut c = Command::new(program(e, &argv[0]));
    c.args(&argv[1..]).current_dir(workdir(e, cwd, ctx));
    for (k, v) in env(e, m, ctx, extra) {
        c.env(k, v);
    }
    c
}

/// 设置名 → 环境变量名:`max_continue` → `ACA_SETTING_MAX_CONTINUE`
fn setting_var(name: &str) -> String {
    manifest::var("ACA_SETTING_", name)
}

fn env(e: &Entry, m: &Manifest, ctx: &Context, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let context = json!({
        "session_id": ctx.session,
        "session_dir": ctx.dir,
        "session_pane": ctx.pane,
        "session_parent": ctx.parent,
        "invocation_source": ctx.source,
    });
    let settings = decl::settings(&e.id, m);
    let mut v: Vec<(String, String)> = vec![
        ("ACA_BIN".into(), crate::agent::self_exe()),
        ("ACA_ENV".into(), "1".into()),
        ("ACA_PLUGIN_ID".into(), e.id.clone()),
        ("ACA_PLUGIN_ROOT".into(), e.path.clone()),
        ("ACA_PLUGIN_CONFIG_DIR".into(), config_dir(&e.id).to_string_lossy().into_owned()),
        ("ACA_PLUGIN_STATE_DIR".into(), state_dir(&e.id).to_string_lossy().into_owned()),
        ("ACA_PLUGIN_CONTEXT_JSON".into(), context.to_string()),
        // 谁调的(menu / cli / agent …),shell 写的插件不用解上面那份 JSON
        ("ACA_INVOKED_BY".into(), ctx.source.clone()),
        // 界面现在用哪种话:插件说的话照它(zh / en)
        ("ACA_LANG".into(), if crate::i18n::lang() == crate::i18n::Lang::Zh { "zh" } else { "en" }.into()),
        ("ACA_PLUGIN_SETTINGS_JSON".into(), serde_json::to_string(&settings).unwrap_or_else(|_| "{}".into())),
        // 没有会话时也设成空的:从某个会话里调过来的,别把那个会话的 id 带给插件
        ("ACA_SESSION_ID".into(), ctx.session.clone()),
        ("ACA_SESSION_DIR".into(), ctx.dir.clone()),
        ("ACA_SESSION_PANE".into(), ctx.pane.clone()),
        // 派它的那个:钩子里要找 worker 的上级不用再问 aca show
        ("ACA_SESSION_PARENT".into(), ctx.parent.clone()),
        // 定时跑的是哪个(`插件.能力`);别的时候空着:定时跑的那一次里又起的插件命令不跟着带
        ("ACA_PLUGIN_TIMER".into(), String::new()),
    ];
    for (k, val) in &settings {
        let s = match val {
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        v.push((setting_var(k), s));
    }
    v.extend(extra.iter().map(|(k, val)| (k.to_string(), val.to_string())));
    // 参数再一个一个放一份(ACA_ARG_<名字>):shell 写的插件不用解 JSON
    if let Some((_, args)) = extra.iter().find(|(k, _)| *k == "ACA_PLUGIN_ARGS_JSON")
        && let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(args)
    {
        for (k, val) in o {
            let s = match val {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            v.push((manifest::var("ACA_ARG_", &k), s));
        }
    }
    v
}

/// `插件 id.动作 id` → (插件 id, 动作 id):插件 id 里可以有点,动作 id 里没有
fn split_qualified(q: &str) -> Res<(&str, &str)> {
    q.rsplit_once('.').ok_or_else(|| tr!("动作要写成 插件.动作:{q}", "an action is written PLUGIN.ACTION: {q}"))
}

/// 调的时候给的参数(`名字=值`)照清单里的类型变成 JSON;少了必须给的、多了没声明的、类型不对的都说
pub fn args_json(a: &Action, given: &[(String, String)]) -> Res<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    let sorted: BTreeMap<&String, &String> = given.iter().map(|(k, v)| (k, v)).collect();
    for (k, v) in sorted {
        let Some((ty, _)) = a.arg(k) else {
            return Err(tr!("{} 没有参数 {k}", "{} has no argument {k}", a.id));
        };
        let val = match ty {
            "int" => v.parse::<i64>().map(serde_json::Value::from).map_err(|_| tr!("{k} 要是整数", "{k} takes an integer"))?,
            "bool" => match v.as_str() {
                "true" | "yes" | "1" => true.into(),
                "false" | "no" | "0" => false.into(),
                _ => return Err(tr!("{k} 要是 true / false", "{k} takes true / false")),
            },
            "path" => paths::expand(v).into(),
            _ => v.clone().into(),
        };
        out.insert(k.clone(), val);
    }
    if let Some(k) = a.args.keys().find(|k| !out.contains_key(*k) && a.arg(k).is_some_and(|(_, opt)| !opt)) {
        return Err(tr!("{} 要给参数 {k}", "{} needs argument {k}", a.id));
    }
    Ok(out)
}

/// 在前台跑一个动作(输出直接给调用的人),返回它的退出码。`given` = 参数(`名字=值`)
pub fn invoke(qualified: &str, ctx: &Context, given: &[(String, String)]) -> Res<i32> {
    let (pid, aid) = split_qualified(qualified)?;
    let (e, m) = find(pid)?;
    let a = m.actions.iter().find(|a| a.id == aid).ok_or_else(|| tr!("{pid} 没有 {aid} 这个动作", "{pid} has no action {aid}"))?;
    let to = match ctx.source.as_str() {
        "cli" => "cli",
        "agent" => "agent",
        _ => "menu",
    };
    if !a.exposed(to) {
        return Err(tr!("{pid}.{aid} 没露给 {to}(清单的 expose)", "{pid}.{aid} isn't exposed to {to} (expose in the manifest)"));
    }
    let args = serde_json::Value::Object(args_json(a, given)?).to_string();
    let mut c = command(&e, &m, &a.command, ctx, &command_ids(aid, &args), &a.cwd);
    let started = |err: std::io::Error| tr!("{} 起不来:{err}", "can't start {}: {err}", a.command[0]);
    let st = if a.input.is_empty() {
        c.status().map_err(started)?
    } else {
        let text = input_text(&a.input, ctx);
        let mut child = c.stdin(Stdio::piped()).spawn().map_err(started)?;
        // 另起一个线程写:命令不读(回滚很长、管道满了)也只卡那个线程,不卡这边等它退出
        if let Some(mut w) = child.stdin.take() {
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = w.write_all(text.as_bytes());
            });
        }
        child.wait().map_err(|err| err.to_string())?
    };
    Ok(st.code().unwrap_or(1))
}

/// 交给插件的会话内容(`input`):最后一条回复 / 屏幕上现在的字 / 连回滚一起。没有会话、会话没在跑 = 空的
pub fn input_text(kind: &str, ctx: &Context) -> String {
    if kind.is_empty() || ctx.session.is_empty() {
        return String::new();
    }
    if kind == "reply" {
        return reply(&ctx.session);
    }
    let pane = if ctx.pane.is_empty() { crate::tmux::find(&ctx.session).map(|p| p.pane).unwrap_or_default() } else { ctx.pane.clone() };
    if pane.is_empty() {
        return String::new();
    }
    crate::tmux::text(&pane, kind == "scrollback")
}

/// 会话的最后一条回复:hook 记的那份(最长 4000 字)。没记到(接管来还没重启的会话没有 hook)、
/// 或者被截短了(结尾是 …),就去它的对话记录里读全的(见 [`crate::agent::Agent::last_reply`])
fn reply(session: &str) -> String {
    let r = crate::runtime::load(session);
    if !r.last.trim().is_empty() && !r.last.ends_with('…') {
        return r.last;
    }
    let full = crate::store::Store::open().ok().and_then(|st| st.get(session).ok()).and_then(|s| {
        let (agent, _) = crate::agent::of(&crate::config::load(), &s).ok()?;
        agent.last_reply(&r)
    });
    full.unwrap_or(r.last)
}

/// 界面 `:` 菜单里的一项:一个露给菜单的能力、一个窗格或一个视图
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuItem {
    pub plugin: String,
    pub plugin_name: String,
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    /// `[[keys]]` 给它绑的键(在 `:` 菜单里按)
    pub key: Option<char>,
    pub contexts: Vec<String>,
    /// 能力的参数:(名字, 类型, 能不能不给)
    pub args: Vec<(String, String, bool)>,
    /// 视图的输出是一份列表([`ViewRows`])
    pub rows: bool,
    /// 列表视图里每行能按的键
    pub acts: Vec<RowAct>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ItemKind {
    #[default]
    Command,
    Pane,
    View,
}

/// 列表视图里的一个键(清单的 `[[views.keys]]`),要跑的能力已经找好
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowAct {
    /// 一个字符,或者 `enter`
    pub key: String,
    pub title: String,
    /// 要跑的能力(露没露给菜单都行);复制的是 None
    pub run: Option<MenuItem>,
    /// 参数:(名字, 模板)
    pub args: Vec<(String, String)>,
    /// 复制的模板
    pub copy: String,
    pub confirm: bool,
    /// 要停在某一行上才能按
    pub row: bool,
}

/// 列表视图的命令输出的那份 JSON:`{"head": "…", "warn": "…", "empty": "…", "fields": {…}, "rows": [{…}]}`,都可以不写
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ViewRows {
    /// 列表上面一行灰字(现在怎样)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub head: String,
    /// 要留意的(标成警告色)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub warn: String,
    /// 一行都没有时写的
    #[serde(skip_serializing_if = "String::is_empty")]
    pub empty: String,
    /// 不停在哪一行上也能用的字段(`row = false` 的键的模板读这里)
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
    pub rows: Vec<ViewRow>,
}

/// 列表里的一行
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ViewRow {
    pub id: String,
    /// 行首的记号(● ✕ ?…)
    pub mark: String,
    /// 记号的颜色:ok / warn / bad / dim(不写 = 平常色)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tone: String,
    pub text: String,
    /// 靠右的那一小段(状态、时间)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub right: String,
    /// 停在这一行时下面写的
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    /// 键的模板读的字段
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
}

impl ViewRows {
    pub fn parse(out: &str) -> Res<ViewRows> {
        serde_json::from_str(out.trim()).map_err(|e| tr!("列表读不懂:{e}", "can't parse the list: {e}"))
    }

    /// 给人看的几行(`aca plugin view`)
    pub fn text(&self) -> String {
        let mut out = vec![];
        out.extend([&self.head, &self.warn].into_iter().filter(|s| !s.is_empty()).cloned());
        if self.rows.is_empty() && !self.empty.is_empty() {
            out.push(self.empty.clone());
        }
        for r in &self.rows {
            let mark = if r.mark.is_empty() { "·" } else { &r.mark };
            let right = if r.right.is_empty() { String::new() } else { format!("  {}", r.right) };
            out.push(format!("{mark} {}{right}", r.text));
        }
        out.join("\n")
    }
}

/// 填模板:`{名字}` 换成字段(`id`、`text` 是行自己的,别的在 `fields` 里;`row` 是 None 时只读列表的 `fields`)。
/// 用到的字段有一个没有、是空的,就是 None(这一行上这个键不能按)
pub fn fill(tpl: &str, list: &ViewRows, row: Option<&ViewRow>) -> Option<String> {
    let get = |name: &str| -> Option<String> {
        let v = match (name, row) {
            ("id", Some(r)) => Some(r.id.clone()),
            ("text", Some(r)) => Some(r.text.clone()),
            (_, Some(r)) => r.fields.get(name).or_else(|| list.fields.get(name)).cloned(),
            (_, None) => list.fields.get(name).cloned(),
        };
        v.filter(|v| !v.is_empty())
    };
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        let Some(j) = rest[i..].find('}') else { break };
        out += &rest[..i];
        out += &get(&rest[i + 1..i + j])?;
        rest = &rest[i + j + 1..];
    }
    out += rest;
    Some(out)
}

impl MenuItem {
    /// `插件.id`
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.plugin, self.id)
    }

    /// 在这种行上(session / group / global)有没有意义
    pub fn fits(&self, on: &str) -> bool {
        self.contexts.is_empty() || self.contexts.iter().any(|c| c == on || c == "global")
    }
}

/// 开着的插件给菜单的全部:露给 menu 的能力、窗格、视图,按插件、清单里的顺序。
/// 键:plugins.toml 的 `keys` 先拿,再是清单的 `[[keys]]`(见 [`yours::settle`])
pub fn menu() -> Vec<MenuItem> {
    menu_and_notes().0
}

/// 键没拿到 / plugins.toml 里写得不对的:(插件 id, 一句话)。`:` 菜单的键和会话里的热键都在这
pub fn key_notes() -> Vec<(String, String)> {
    let mut notes = menu_and_notes().1;
    notes.extend(hotkeys().1);
    notes
}

/// 会话里面的一个热键(键已经分好):按了开哪个插件的哪个窗格
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HotkeyItem {
    pub plugin: String,
    pub pane: String,
    pub title: String,
    /// tmux 的键名
    pub key: String,
}

/// 开着的插件的热键:清单的 `[[hotkeys]]`,你在 plugins.toml 的 `hotkeys` 里写的盖在上面(清单没给热键的窗格你也能给)。
/// 分法和 `:` 菜单的键一样(见 [`yours::settle_hotkeys`])。第二样是没拿到 / 写得不对的:(插件 id, 一句话)
pub fn hotkeys() -> (Vec<HotkeyItem>, Vec<(String, String)>) {
    hotkeys_in(registry())
}

fn hotkeys_in(reg: Vec<Entry>) -> (Vec<HotkeyItem>, Vec<(String, String)>) {
    let decl = decl::wants().unwrap_or_default();
    let mut wants: Vec<yours::KeyWant<String>> = vec![];
    let mut notes = vec![];
    for e in reg.into_iter().filter(|e| e.enabled) {
        let Ok((m, _)) = read_manifest(Path::new(&e.path)) else { continue };
        let mine = decl.get(&e.id).map(|w| w.hotkeys.clone()).unwrap_or_default();
        for p in &m.panes {
            let manifest = m.hotkeys.iter().find(|k| k.run == p.id).map(|k| k.key.clone());
            let mine = mine.get(&p.id).cloned();
            if manifest.is_some() || mine.is_some() {
                let title = if p.title.is_empty() { &p.id } else { &p.title }.clone();
                wants.push(yours::KeyWant { plugin: e.id.clone(), id: p.id.clone(), title, manifest, mine });
            }
        }
        for t in mine.keys().filter(|t| !m.panes.iter().any(|p| &p.id == *t)) {
            notes.push((
                e.id.clone(),
                tr!(
                    "plugins.toml 里 {} 的 hotkeys 写了 {t},它没有这个窗格(热键只开窗格)",
                    "plugins.toml gives {} a hotkey for {t}, which isn't one of its panes (hotkeys open panes only)",
                    e.id
                ),
            ));
        }
    }
    let (keys, more) = yours::settle_hotkeys(&wants);
    notes.extend(more);
    let items = wants
        .into_iter()
        .zip(keys)
        .filter_map(|(w, k)| Some(HotkeyItem { plugin: w.plugin, pane: w.id, title: w.title, key: k? }))
        .collect();
    (items, notes)
}

/// 热键按下去跑的那条命令(交给 tmux 的 run-shell:里面的 `#{…}` 它会换成按键的那个会话、那台终端)
fn hotkey_command(exe: &str, h: &HotkeyItem) -> String {
    format!(
        "{} plugin pane {} {} {} {} --session '#{{@aca_id}}' --client '#{{client_name}}'",
        crate::agent::sh_quote(exe),
        h.plugin,
        h.pane,
        crate::tmux::HOTKEY_MARK,
        h.key
    )
}

/// 没绑上的那几句话里,aca 自己的键(`[keys]`)记在这个名字下(插件的记在插件 id 下)
pub const OWN_KEYS: &str = "aca";

/// aca 自己的那个键按下去跑的命令:进下一个在等你的会话(`aca next`)
fn next_command(exe: &str, key: &str) -> String {
    format!("{} next {} {key} --session '#{{@aca_id}}' --client '#{{client_name}}'", crate::agent::sh_quote(exe), crate::tmux::HOTKEY_MARK)
}

/// 让 tmux 上绑的热键跟现在要的一致:要的绑上;aca 以前绑的、现在没人要的(插件停了、删了、换了键)解掉。
/// 要的 = aca 自己的那个键(config.toml 的 `[keys] next`)和开着的插件的热键;两边要同一个键,给 aca 的。
/// 返回没绑上的:(插件 id 或 [`OWN_KEYS`], 一句话) —— 那个键你自己在 tmux 里绑了别的,不动它。
/// tmux 没在跑就什么都不做:绑定跟着 Ctrl+Q,建会话后、进会话前都会再来一遍(见 [`crate::tmux::bind_detach`])
pub fn bind_hotkeys() -> Vec<(String, String)> {
    // 单元测试没有自己的 tmux 服务:不去碰真的那个
    if cfg!(test) {
        return vec![];
    }
    // 照登记表现在的样子,不去看你自己的那份清单变没变([`registry`] 会看:变了它要重新登记、存登记表,又绕回这里)
    let (items, _) = hotkeys_in(read_registry());
    let (next, mut notes) = match crate::config::load().keys.next_key() {
        Ok(k) => (k, vec![]),
        Err(e) => (None, vec![(OWN_KEYS.to_string(), tr!("config.toml [keys] next:{e}", "config.toml [keys] next: {e}"))]),
    };
    let held = crate::tmux::bind_hotkeys(&want_keys(&items, next.as_ref()));
    if let Some(k) = next.as_ref().filter(|k| held.contains(k)) {
        notes.push((
            OWN_KEYS.to_string(),
            tr!(
                "键 {k} 你在 tmux 里绑了别的,没动它:「进下一个在等你的」没绑上(换一个:aca tmux key next <键>)",
                "key {k} is bound to something else in your tmux and was left alone: “go to the next one waiting” isn't bound (pick another: aca tmux key next <key>)"
            ),
        ));
    }
    notes.extend(items.iter().filter(|h| next.as_ref() == Some(&h.key)).map(|h| {
        (
            h.plugin.clone(),
            tr!(
                "键 {} 是 aca 自己的(进下一个在等你的):{}「{}」没绑上(换一个:aca plugin hotkey {} {} <键>)",
                "key {} is aca's own (go to the next one waiting): {} “{}” isn't bound (pick another: aca plugin hotkey {} {} <key>)",
                h.key,
                h.plugin,
                h.title,
                h.plugin,
                h.pane
            ),
        )
    }));
    let plugins = items
        .iter()
        .filter(|h| held.contains(&h.key) && next.as_ref() != Some(&h.key))
        .map(|h| {
            (
                h.plugin.clone(),
                tr!(
                    "键 {} 你在 tmux 里绑了别的,没动它:{}「{}」没绑上(换一个:aca plugin hotkey {} {} <键>)",
                    "key {} is bound to something else in your tmux and was left alone: {} “{}” isn't bound (pick another: aca plugin hotkey {} {} <key>)",
                    h.key,
                    h.plugin,
                    h.title,
                    h.plugin,
                    h.pane
                ),
            )
        });
    notes.extend(plugins);
    notes
}

/// tmux 上该绑的热键,和 [`bind_hotkeys`] 绑的同一份:(键, 按了跑的命令)。不说哪些没绑上 ——
/// 进会话前补绑定(见 [`crate::tmux::bind_detach`])只要这一份。单元测试里是 None:不去碰真的那个 tmux 服务
pub fn hotkey_binds() -> Option<Vec<(String, String)>> {
    if cfg!(test) {
        return None;
    }
    let (items, _) = hotkeys_in(read_registry());
    let next = crate::config::load().keys.next_key().ok().flatten();
    Some(want_keys(&items, next.as_ref()))
}

/// aca 自己的那个键,和开着的插件的热键;两边要同一个键,给 aca 的
fn want_keys(items: &[HotkeyItem], next: Option<&String>) -> Vec<(String, String)> {
    let exe = crate::agent::self_exe();
    let mut want: Vec<(String, String)> = next.iter().map(|k| (k.to_string(), next_command(&exe, k))).collect();
    want.extend(items.iter().filter(|h| next != Some(&h.key)).map(|h| (h.key.clone(), hotkey_command(&exe, h))));
    want
}

fn menu_and_notes() -> (Vec<MenuItem>, Vec<(String, String)>) {
    let mut out: Vec<MenuItem> = vec![];
    let mut wants: Vec<yours::KeyWant> = vec![];
    let mut notes = vec![];
    let decl = decl::wants().unwrap_or_default();
    for e in registry().into_iter().filter(|e| e.enabled) {
        let Ok((m, _)) = read_manifest(Path::new(&e.path)) else { continue };
        let mine = decl.get(&e.id).map(|w| w.keys.clone()).unwrap_or_default();
        let key = |id: &str| m.keys.iter().find(|k| k.run == id).and_then(|k| k.key.chars().next());
        let item = |id: &str, kind, title: &str, contexts: &[String]| MenuItem {
            plugin: e.id.clone(),
            plugin_name: m.name.clone(),
            id: id.to_string(),
            kind,
            title: if title.is_empty() { id.to_string() } else { title.to_string() },
            key: key(id),
            contexts: contexts.to_vec(),
            ..Default::default()
        };
        let from = out.len();
        for a in m.actions.iter().filter(|a| a.exposed("menu")) {
            let mut it = item(&a.id, ItemKind::Command, &a.title, &a.contexts);
            it.args = command_args(a);
            out.push(it);
        }
        out.extend(m.panes.iter().map(|p| item(&p.id, ItemKind::Pane, &p.title, &p.contexts)));
        for v in &m.views {
            let mut it = item(&v.id, ItemKind::View, &v.title, &v.contexts);
            it.rows = v.rows;
            it.acts = v
                .keys
                .iter()
                .map(|k| RowAct {
                    key: k.key.clone(),
                    title: k.title.clone(),
                    run: m.actions.iter().find(|a| !k.run.is_empty() && a.id == k.run).map(|a| {
                        let mut r = item(&a.id, ItemKind::Command, &a.title, &a.contexts);
                        r.key = None;
                        r.args = command_args(a);
                        r
                    }),
                    args: k.args.iter().map(|(n, t)| (n.clone(), t.clone())).collect(),
                    copy: k.copy.clone(),
                    confirm: k.confirm,
                    row: k.row,
                })
                .collect();
            out.push(it);
        }
        for it in &out[from..] {
            wants.push(yours::KeyWant {
                plugin: it.plugin.clone(),
                id: it.id.clone(),
                title: it.title.clone(),
                manifest: it.key,
                mine: mine.get(&it.id).cloned(),
            });
        }
        for t in mine.keys().filter(|t| !out[from..].iter().any(|it| &it.id == *t)) {
            notes.push((
                e.id.clone(),
                tr!(
                    "plugins.toml 里 {} 的 keys 写了 {t},它没有这个(: 菜单里的能力、窗格、视图才有键)",
                    "plugins.toml gives {} a key for {t}, which it doesn't have (only : menu commands, panes and views take keys)",
                    e.id
                ),
            ));
        }
    }
    let (keys, more) = yours::settle(&wants);
    for (it, k) in out.iter_mut().zip(keys) {
        it.key = k;
    }
    notes.extend(more);
    (out, notes)
}

/// 能力的参数,表单用:(名字, 类型, 能不能不给),必填的排前面
fn command_args(a: &Action) -> Vec<(String, String, bool)> {
    let mut args: Vec<(String, String, bool)> =
        a.args.keys().filter_map(|k| a.arg(k).map(|(t, opt)| (k.clone(), t.to_string(), opt))).collect();
    args.sort_by_key(|(_, _, opt)| *opt);
    args
}

/// 界面里跑的能力、视图最多等这么久
pub const CAPTURE_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// 跑一个能力(露给 menu 的,或者列表视图的键要跑的)或一个视图,收下输出:(成没成, 标准输出 + 标准错误)。界面在后台调它。
/// 列表视图跑成了只交标准输出(那是一份 JSON;标准错误写进插件的日志),没跑成照样两样都交
pub fn capture(qualified: &str, ctx: &Context, given: &[(String, String)]) -> Res<(bool, String)> {
    let (pid, xid) = split_qualified(qualified)?;
    let (e, m) = find(pid)?;
    let rows = m.views.iter().any(|v| v.id == xid && v.rows);
    let (mut c, input) = if let Some(v) = m.views.iter().find(|v| v.id == xid) {
        (command(&e, &m, &v.command, ctx, &[("ACA_PLUGIN_ENTRYPOINT_ID", xid)], &v.cwd), &v.input)
    } else {
        let a = m.actions.iter().find(|a| a.id == xid).ok_or_else(|| tr!("{pid} 没有 {xid}", "{pid} has no {xid}"))?;
        let from_view = m.views.iter().any(|v| v.keys.iter().any(|k| k.run == xid));
        if !a.exposed("menu") && !from_view {
            return Err(tr!("{pid}.{xid} 没露给界面(清单的 expose)", "{pid}.{xid} isn't exposed to the menu (expose in the manifest)"));
        }
        let args = serde_json::Value::Object(args_json(a, given)?).to_string();
        (command(&e, &m, &a.command, ctx, &command_ids(xid, &args), &a.cwd), &a.input)
    };
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let fed = (!input.is_empty()).then(|| input_text(input, ctx));
    let f = tools::run_fed(c, CAPTURE_LIMIT, fed)?;
    if !(rows && f.ok) {
        return Ok((f.ok, f.text()));
    }
    if !f.err.trim().is_empty() {
        log_line(&e.id, &format!("--- {} {qualified}({})\n{}", paths::now(), t!("标准错误", "stderr"), f.err.trim_end()));
    }
    Ok((true, f.out))
}

/// 跑一个能力时交给它的:哪个(新旧两个名字)、参数
pub(crate) fn command_ids<'a>(id: &'a str, args: &'a str) -> [(&'static str, &'a str); 3] {
    [("ACA_PLUGIN_ENTRYPOINT_ID", id), ("ACA_PLUGIN_ACTION_ID", id), ("ACA_PLUGIN_ARGS_JSON", args)]
}

/// 往插件的日志里记一段(钩子的输出也在那;`aca plugin log` 看)
fn log_line(id: &str, text: &str) {
    use std::io::Write;
    let log = log_file(id);
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        let _ = writeln!(f, "{text}");
    }
}

/// `aca plugin mcp <插件 id>`:agent 起这个插件的 MCP 服务时跑的就是这个(见 [`mount`])。
/// 切到插件目录、带上插件的环境(设置;会话 id 是 agent 给的环境里那个),换成清单里 `[agent.mcp]` 的命令
pub fn exec_mcp(id: &str) -> Res<i32> {
    let (e, m) = find(id)?;
    let spec = m.agent.mcp.as_ref().ok_or_else(|| tr!("{id} 没有 [agent.mcp]", "{id} has no [agent.mcp]"))?;
    let ctx = agent_context();
    let err = command(&e, &m, &spec.command, &ctx, &[], "").exec();
    Err(tr!("{} 起不来:{err}", "can't start {}: {err}", spec.command[0]))
}

/// agent 那头调过来的(MCP):会话是它环境里的 `ACA_SESSION_ID`
pub(crate) fn agent_context() -> Context {
    agent_context_for(&std::env::var("ACA_SESSION_ID").unwrap_or_default())
}

/// 冲着这个会话(gateway 转来的请求在 `_meta` 里说是谁)
pub(crate) fn agent_context_for(session: &str) -> Context {
    let session = session.to_string();
    let s = (!session.is_empty()).then(|| crate::store::Store::open().ok()?.get(&session).ok()).flatten();
    let (dir, parent) = s.map(|s| (s.dir, s.parent)).unwrap_or_default();
    Context { dir, parent, session, source: "agent".into(), ..Default::default() }
}

fn size(v: &Option<toml::Value>, default: &str) -> String {
    match v {
        Some(toml::Value::Integer(n)) => n.to_string(),
        Some(toml::Value::String(s)) => s.clone(),
        _ => default.into(),
    }
}

/// 开一个插件窗格。弹窗开在 `client` 那个终端上(不给 = tmux 自己挑现在这个);
/// 人不在 tmux 里(手机上直接 ssh 进来跑的界面)就在当前终端前台跑,效果一样是「盖一层、关了回来」
pub fn open_pane(plugin: &str, pane: &str, placement: Option<&str>, client: Option<&str>, ctx: &Context) -> Res<i32> {
    let (e, m) = find(plugin)?;
    let p = m.panes.iter().find(|p| p.id == pane).ok_or_else(|| tr!("{plugin} 没有 {pane} 这个窗格", "{plugin} has no pane {pane}"))?;
    let placement = placement.filter(|s| !s.is_empty()).unwrap_or(if p.placement.is_empty() { "popup" } else { &p.placement });
    // 窗格是交互的程序,标准输入是终端:会话的内容写进一个文件(一个窗格一个,下次开时盖掉)
    let input_file = if p.input.is_empty() {
        String::new()
    } else {
        let f = paths::state_dir().join("plugin-input").join(format!("{plugin}.{pane}.txt"));
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        std::fs::write(&f, input_text(&p.input, ctx)).map_err(|err| tr!("写不了 {}:{err}", "can't write {}: {err}", f.display()))?;
        f.to_string_lossy().into_owned()
    };
    let mut extra = vec![("ACA_PLUGIN_ENTRYPOINT_ID", pane)];
    if !input_file.is_empty() {
        extra.push(("ACA_INPUT_FILE", &input_file));
    }
    let in_tmux = std::env::var_os("TMUX").is_some() || client.is_some();
    if placement == "popup" && !in_tmux {
        let st = command(&e, &m, &p.command, ctx, &extra, &p.cwd)
            .status()
            .map_err(|err| tr!("{} 起不来:{err}", "can't start {}: {err}", p.command[0]))?;
        return Ok(st.code().unwrap_or(1));
    }
    let title = if p.title.is_empty() { &m.name } else { &p.title };
    let mut args: Vec<String> = match placement {
        "popup" => {
            let mut a = vec!["display-popup".to_string(), "-E".into(), "-T".into(), format!(" {title} ")];
            a.extend(["-w".into(), size(&p.width, "80%"), "-h".into(), size(&p.height, "80%")]);
            if let Some(c) = client {
                a.extend(["-c".into(), c.into()]);
            }
            a
        }
        // 清单里的宽高是给弹窗的;分屏固定占右边四成,agent 那格留大头
        "split" if !ctx.pane.is_empty() => {
            vec!["split-window".into(), "-h".into(), "-l".into(), "40%".into(), "-t".into(), ctx.pane.clone()]
        }
        "window" if !ctx.tmux.is_empty() => vec!["new-window".into(), "-n".into(), title.clone(), "-t".into(), format!("{}:", ctx.tmux)],
        "split" | "window" => {
            return Err(t!("这个窗格要开在一个在跑的会话里(--session)", "this pane has to open in a running session (--session)").into());
        }
        other => return Err(tr!("不认识的 placement:{other}", "unknown placement: {other}")),
    };
    args.extend([if placement == "popup" { "-d" } else { "-c" }.to_string(), workdir(&e, &p.cwd, ctx)]);
    for (k, v) in env(&e, &m, ctx, &extra) {
        args.extend(["-e".into(), format!("{k}={v}")]);
    }
    args.push(crate::tmux::arg(&program(&e, &p.command[0]).to_string_lossy()));
    args.extend(p.command[1..].iter().map(|a| crate::tmux::arg(a)));
    let st =
        crate::tmux::cmd().args(&args).stdin(Stdio::inherit()).status().map_err(|err| tr!("tmux 起不来:{err}", "can't run tmux: {err}"))?;
    Ok(st.code().unwrap_or(1))
}

/// 事件那一行加上会话的几样(`if` 里能写的):`parent`、`tool`、`account`、`group`(事件里有同名字段的以事件为准)
fn facts(line: &str, s: Option<&crate::store::Session>) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|_| json!({}));
    if let (Some(o), Some(s)) = (v.as_object_mut(), s) {
        for (k, val) in [("parent", &s.parent), ("tool", &s.tool), ("account", &s.account), ("group", &s.group)] {
            o.entry(k).or_insert_with(|| val.clone().into());
        }
    }
    v
}

/// 事件来了:订了它的每个插件各跑一条(`if` 对不上的不跑),**不等**(hook 里调的,不能拖住 agent),输出进插件自己的日志。
/// 外面包一层 sh,跑完把退出码记下来(见 [`health`]):没跑成的状态栏、插件页标出来
pub fn dispatch(event: &str, session: &str, line: &str) {
    let reg = registry();
    let subs: Vec<&Entry> = reg.iter().filter(|e| e.enabled && e.events.iter().any(|on| crate::event::matches(on, event))).collect();
    if subs.is_empty() {
        return;
    }
    // 有人订才去查会话;窗格只有 hook 里知道(它就跑在那一格里)
    let s = crate::store::Store::open().ok().and_then(|st| st.get(session).ok());
    let (dir, parent) = s.as_ref().map(|s| (s.dir.clone(), s.parent.clone())).unwrap_or_default();
    let in_hook = std::env::var("ACA_SESSION_ID").is_ok_and(|id| id == session);
    let pane = std::env::var("TMUX_PANE").ok().filter(|_| in_hook).unwrap_or_default();
    let ctx = Context { session: session.into(), dir, pane, parent, source: "event".into(), ..Default::default() };
    let facts = facts(line, s.as_ref());
    for e in subs {
        // 自带的:刚升级了 aca 就先换成新写出来的那份(只读它自己那几个小文件)
        if e.source == first_party::SOURCE {
            first_party::refresh_one(&e.id);
        }
        let Ok((m, _)) = read_manifest(Path::new(&e.path)) else { continue };
        for (i, h) in m.events.iter().enumerate().filter(|(_, h)| crate::event::matches(&h.on, event) && cond_matches(&h.cond, &facts)) {
            let log = log_file(&e.id);
            let _ = std::fs::create_dir_all(log.parent().unwrap());
            let Ok(out) = std::fs::OpenOptions::new().create(true).append(true).open(&log) else { continue };
            let Ok(err) = out.try_clone() else { continue };
            {
                use std::io::Write;
                let _ = writeln!(&out, "--- {} {event} {}", paths::now(), h.command.join(" "));
            }
            let rec = health::hook_file(&e.id, i);
            let _ = std::fs::create_dir_all(rec.parent().unwrap());
            let mut argv = vec![
                "/bin/sh".to_string(),
                "-c".into(),
                health::WRAP.into(),
                "sh".into(),
                rec.to_string_lossy().into_owned(),
                program(e, &h.command[0]).to_string_lossy().into_owned(),
            ];
            argv.extend(h.command[1..].iter().cloned());
            // 三个口都接走、自成一组:调用它的 hook 退出后它接着跑,也不会让 agent 那头的管道一直开着
            let _ = command(e, &m, &argv, &ctx, &[("ACA_PLUGIN_EVENT", event), ("ACA_PLUGIN_EVENT_JSON", line)], "")
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err)
                .process_group(0)
                .spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::manifest::newer;
    use super::*;

    #[test]
    fn a_setting_you_type_is_read_as_the_defaults_type() {
        use toml::Value as V;
        let s = V::String("x".into());
        assert_eq!(typed("codex:labs", Some(&s)).unwrap(), V::String("codex:labs".into()));
        assert_eq!(typed("true", Some(&s)).unwrap(), V::String("true".into()));
        assert_eq!(typed("\"a b\"", Some(&s)).unwrap(), V::String("a b".into()));
        assert_eq!(typed(" 5 ", Some(&V::Integer(1))).unwrap(), V::Integer(5));
        assert!(typed("five", Some(&V::Integer(1))).is_err());
        assert_eq!(typed("on", Some(&V::Boolean(false))).unwrap(), V::Boolean(true));
        assert!(typed("maybe", Some(&V::Boolean(false))).is_err());
        assert_eq!(typed("[\"a\"]", Some(&V::Array(vec![]))).unwrap(), V::Array(vec!["a".into()]));
        assert!(typed("a", Some(&V::Array(vec![]))).is_err());
        assert_eq!(typed("3", None).unwrap(), V::Integer(3));
        assert_eq!(typed("hi there", None).unwrap(), V::String("hi there".into()));
        // secret 的打码
        let mut n = SettingNow {
            name: "url".into(),
            value: V::String("https://discord.com/api/webhooks/1/abcd".into()),
            default: None,
            desc: String::new(),
            changed: true,
            secret: true,
        };
        assert_eq!(n.shown(), "••••abcd");
        n.value = V::String(String::new());
        assert_eq!(n.shown(), "");
    }

    #[test]
    fn a_list_views_output_reads_and_templates_fill_from_the_row_or_say_the_key_cant_be_used() {
        let out = r#"{"head":"h","fields":{"next":"on"},"rows":[
            {"id":"a1","mark":"●","tone":"ok","text":"https://m/ → :3000","right":"在听","fields":{"url":"https://m/","log":""}},
            {"id":"b2","text":"tcp"}]}"#;
        let l = ViewRows::parse(out).unwrap();
        assert_eq!((l.rows.len(), l.rows[0].tone.as_str(), l.rows[1].mark.as_str()), (2, "ok", ""));
        assert_eq!(fill("{url}", &l, Some(&l.rows[0])).as_deref(), Some("https://m/"));
        assert_eq!(fill("rm {id} --to {next}", &l, Some(&l.rows[0])).as_deref(), Some("rm a1 --to on"));
        // 空的、没有的字段:这一行上按不了
        assert_eq!(fill("{log}", &l, Some(&l.rows[0])), None);
        assert_eq!(fill("{url}", &l, Some(&l.rows[1])), None);
        // 不停在行上:只读列表的字段
        assert_eq!(fill("{next}", &l, None).as_deref(), Some("on"));
        assert_eq!(fill("{id}", &l, None), None);
        assert_eq!(fill("plain", &l, None).as_deref(), Some("plain"));
        assert_eq!(l.text(), "h\n● https://m/ → :3000  在听\n· tcp");
        assert!(ViewRows::parse("not json").is_err());
        let empty = ViewRows::parse(r#"{"empty":"没有挂载"}"#).unwrap();
        assert_eq!(empty.text(), "没有挂载");
    }

    const OK: &str = r#"
id = "example.notify"
name = "Notify"
version = "0.1.0"
[[events]]
on = "turn.done"
command = ["sh", "notify.sh"]
[[events]]
on = "turn.finished"
command = ["true"]
[[actions]]
id = "review"
title = "看改动"
contexts = ["session"]
command = ["sh", "review.sh"]
[[panes]]
id = "board"
width = "80%"
height = 20
command = ["sh", "board.sh"]
"#;

    #[test]
    fn a_manifest_declares_actions_events_and_panes() {
        let (m, warn) = parse(OK).unwrap();
        assert_eq!((m.id.as_str(), m.actions.len(), m.events.len(), m.panes.len()), ("example.notify", 1, 2, 1));
        assert_eq!(warn, ["没有 turn.finished 这个事件"]);
        assert_eq!(
            (size(&m.panes[0].width, "50%"), size(&m.panes[0].height, "50%"), size(&None, "50%")),
            ("80%".into(), "20".into(), "50%".into())
        );
    }

    #[test]
    fn a_broken_manifest_says_what_is_wrong() {
        let bad = |t: &str| parse(t).unwrap_err();
        assert!(bad("name = \"x\"\nversion = \"1\"").contains("id"));
        assert!(bad("id = \"a b\"\nname = \"x\"\nversion = \"1\"").contains("id"));
        assert!(bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[actions]]\nid = \"x.y\"\ncommand = [\"true\"]").contains("不能有点"));
        assert!(bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[actions]]\nid = \"x\"").contains("command"));
        assert!(
            bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[panes]]\nid = \"x\"\nplacement = \"tab\"\ncommand = [\"true\"]")
                .contains("placement")
        );
        assert!(bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\nmin_aca_version = \"99.0.0\"").contains("99.0.0"));
        assert!(bad("id = ").contains("读不懂"));
    }

    #[test]
    fn a_qualified_action_splits_at_the_last_dot() {
        assert_eq!(split_qualified("example.notify.review").unwrap(), ("example.notify", "review"));
        assert!(split_qualified("review").is_err());
        assert!(newer("0.2.0", "0.1.9") && !newer("0.1.0", "0.1.0") && newer("0.10.0", "0.9.0"));
    }

    #[test]
    fn arguments_are_typed_by_the_manifest_and_missing_ones_are_named() {
        let (m, _) = parse(
            "id = \"p\"\nname = \"x\"\nversion = \"1\"\n[[commands]]\nid = \"go\"\nargs = { task = \"string\", n = \"int?\", dry = \"bool?\" }\ncommand = [\"true\"]",
        )
        .unwrap();
        let a = &m.actions[0];
        let kv = |xs: &[(&str, &str)]| xs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let got = args_json(a, &kv(&[("task", "修登录"), ("n", "3"), ("dry", "yes")])).unwrap();
        assert_eq!(serde_json::Value::Object(got), json!({ "task": "修登录", "n": 3, "dry": true }));
        assert!(args_json(a, &kv(&[("n", "3")])).unwrap_err().contains("task"));
        assert!(args_json(a, &kv(&[("task", "x"), ("n", "three")])).unwrap_err().contains("n"));
        assert!(args_json(a, &kv(&[("task", "x"), ("who", "y")])).unwrap_err().contains("who"));
    }

    #[test]
    fn a_setting_becomes_an_environment_variable_name() {
        assert_eq!(setting_var("max_continue"), "ACA_SETTING_MAX_CONTINUE");
        assert_eq!(setting_var("a-b:c"), "ACA_SETTING_A_B_C");
    }

    #[test]
    fn event_facts_carry_the_sessions_parent_unless_the_event_has_its_own() {
        let s = crate::store::Session { parent: "p1".into(), tool: "codex".into(), ..Default::default() };
        let f = facts(r#"{"event":"turn.failed","kind":"transient"}"#, Some(&s));
        assert_eq!((f["parent"].as_str(), f["tool"].as_str(), f["kind"].as_str()), (Some("p1"), Some("codex"), Some("transient")));
        let f = facts(r#"{"event":"session.created","parent":"p2"}"#, Some(&s));
        assert_eq!(f["parent"].as_str(), Some("p2"));
    }
}
