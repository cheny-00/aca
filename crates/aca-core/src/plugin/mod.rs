//! Plugins: a directory with an `aca-plugin.toml` manifest and the commands it runs (modeled on herdr).
//!
//! There's no SDK; a plugin can be written in any language. What a manifest can declare is in [`manifest`]: commands (one
//! command each, bound to a key / put in the menu in the interface, called from the command line, given to agents as
//! tools), event hooks, timers, panes, settings, and directories and MCP servers to bring to agents. When a plugin wants
//! something done it calls back into the `aca` command line — the command line is the plugin API, and `ACA_BIN` points to
//! the running program.
//!
//! Commands run with the plugin directory as their working directory, not through a shell. Their environment:
//! `ACA_BIN`, `ACA_ENV=1`, `ACA_PLUGIN_ID`, `ACA_PLUGIN_ROOT`, `ACA_PLUGIN_CONFIG_DIR`, `ACA_PLUGIN_STATE_DIR`,
//! `ACA_PLUGIN_CONTEXT_JSON`, `ACA_INVOKED_BY`, `ACA_LANG` (zh / en), `ACA_PLUGIN_SETTINGS_JSON` plus one
//! `ACA_SETTING_<name>` per setting, `ACA_SESSION_ID` / `ACA_SESSION_DIR` / `ACA_SESSION_PANE` / `ACA_SESSION_PARENT`
//! (empty when there's no session); commands, views and panes also get `ACA_PLUGIN_ENTRYPOINT_ID`, commands also
//! `ACA_PLUGIN_ACTION_ID` (the old name), `ACA_PLUGIN_ARGS_JSON` and one `ACA_ARG_<name>` per argument given, panes with
//! `input` also `ACA_INPUT_FILE`, event hooks also `ACA_PLUGIN_EVENT` / `ACA_PLUGIN_EVENT_JSON`, and commands run by a
//! timer also `ACA_PLUGIN_TIMER`. This is part of the plugin API (docs/plugins.md, "The contract"; e2e checks each one).
//!
//! Which plugins you want is written in `plugins.toml` (see [`decl`]); what's registered in `~/.config/aca/plugins.json`
//! is the computed version: the directory, enabled or not, which events it subscribes to (so the hook sees at a glance
//! that nobody subscribes, without reading manifests), which directory goes to which agent (so starting an agent needs no
//! manifest read), and where it was installed from.
//!
//! Four ways in: `link` a local directory (one you write yourself; aca doesn't touch it); `install` a git repository,
//! cloned to `~/.local/share/aca/plugins/<id>`, after which `update` pulls and `remove` deletes it along with the directory
//! (the plugin's own config and state stay); `install aca.<name>` installs a first-party plugin shipped with aca (see
//! [`first_party`]); and `~/.config/aca/user/` is there as soon as it holds a manifest (see [`user`]).
//!
//! What plugins would like (tools for agents, keys in the `:` menu) you decide in plugins.toml, see [`yours`].

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
    AGENTS, ARG_TYPES, Action, AgentPart, AgentSpec, CONTEXTS, EXPOSE, Hook, INPUTS, Key, MANIFEST, MCP_AGENTS, MENU_KEYS_TAKEN, Manifest,
    McpSpec, PaneDef, ROW_KEYS_TAKEN, RowKey, Setting, Timer, View, cond_matches, hotkey_ok, missing_note, parse, parse_user,
    read_manifest,
};

use crate::paths::{self, Res};
use crate::{t, tr};

/// The agents that accept `[agents.<it>] plugin_dir` (the old form)
const AGENTS_WITH_PLUGINS: &[&str] = &["claude"];

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub enabled: bool,
    /// The events subscribed to in the manifest, copied when linking
    pub events: Vec<String>,
    /// The directory for each agent (full path), copied when linking
    pub agents: BTreeMap<String, String>,
    /// The MCP server for agents (`[agent.mcp]`): how it's mounted and how many copies. The command is read from the manifest
    /// when it starts
    pub mcp: Option<McpEntry>,
    /// Commands exposed to agents as tools (action ids with agent in `expose`)
    pub tools: Vec<String>,
    /// Ones that can go to agents but only when you name them (`agent?` in `expose`)
    pub tools_ask: Vec<String>,
    /// Where it was installed from (a git URL, with `#version` when pinned with `--ref`); empty = a linked local directory;
    /// [`first_party::SOURCE`] = shipped with aca; [`user::SOURCE`] = your own one
    pub source: String,
    /// Your own one: the manifest's modification time when registered (re-registered when it changes)
    pub stamp: u64,
}

impl Entry {
    /// Installed from git (not linked, not built in)
    pub fn from_git(&self) -> bool {
        !self.source.is_empty() && self.source != first_party::SOURCE
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct McpEntry {
    /// auto / direct / gateway (empty = auto)
    pub via: String,
    /// shared / session (empty = shared)
    pub scope: String,
    /// Which agents (empty = both)
    pub targets: Vec<String>,
}

fn registry_file() -> PathBuf {
    paths::config_dir().join("plugins.json")
}

fn read_registry() -> Vec<Entry> {
    std::fs::read_to_string(registry_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Registered plugins. Your own one (`~/.config/aca/user/`) follows its file: one look at the modification time,
/// re-registered first if it changed
pub fn registry() -> Vec<Entry> {
    let reg = read_registry();
    if user::refresh(&reg) { read_registry() } else { reg }
}

/// Registers (or re-registers) your own one. An unreadable manifest still leaves an empty registration, so listings show
/// the reason
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

/// Your own one is gone (the manifest was deleted)
fn drop_user() {
    let mut reg = read_registry();
    reg.retain(|e| e.source != user::SOURCE);
    let _ = save(&reg);
    timer::drop_plugin(user::ID);
    health::clear(user::ID);
}

/// Whenever the registry changes (install, remove, enable, disable), the hotkeys in tmux follow: bound when added, unbound
/// when gone
fn save(v: &[Entry]) -> Res<()> {
    paths::write_atomic(&registry_file(), &serde_json::to_string_pretty(v).unwrap())
        .map_err(|e| tr!("无法写入 plugins.json:{e}", "can't write plugins.json: {e}"))?;
    bind_hotkeys();
    Ok(())
}

/// A local directory (or a manifest's path given directly) → the plugin directory's full path
fn plugin_dir(path: &str) -> Res<PathBuf> {
    let p = PathBuf::from(paths::expand(path));
    let dir = if p.file_name().is_some_and(|n| n == MANIFEST) { p.parent().unwrap().to_path_buf() } else { p };
    dir.canonicalize().map_err(|e| format!("{}:{e}", dir.display()))
}

/// Registers (updates in place if already registered): reads the manifest and copies what's needed. `source` = where it
/// was installed from (None = unchanged). Doesn't touch plugins.toml
fn register(dir: &Path, source: Option<String>) -> Res<(Manifest, Vec<String>)> {
    // Register the path with links resolved (macOS's /var is /private/var): both sides match when checking whether the
    // directories for agents are inside the plugin
    let dir = &dir.canonicalize().map_err(|e| format!("{}:{e}", dir.display()))?;
    let (m, mut warn) = read_manifest(dir)?;
    let mut reg = read_registry();
    // Tool names for agents, Codex plugin names and MCP server names are all derived from the id (dots become -): two that
    // come out the same can't be told apart
    let name = mount::safe(&m.id);
    if let Some(other) = reg.iter().find(|e| e.id != m.id && mount::safe(&e.id) == name) {
        return Err(tr!(
            "{} 和已链接的 {} 在 agent 中同名({name}):请修改其中一个的 id",
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
    // Enabled or not: as plugins.toml says if written there; otherwise registered ones stay as they were and new ones are on
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
    // Hook records are kept by index in the manifest: a new manifest makes them stale
    health::clear(&m.id);
    // Check the timers against the new manifest (ones without manual are on while the plugin is enabled)
    warn.extend(timer::sync(&m.id).into_iter().map(|p| tr!("定时任务设置失败:{p}", "timer not set up: {p}")));
    Ok((m, warn))
}

/// The manifest changed (a built-in plugin was rewritten by an aca upgrade): register it again with its old source
fn reregister(dir: &Path) -> Res<(Manifest, Vec<String>)> {
    register(dir, None)
}

/// Links a local directory (or a manifest's path given directly). Updates in place if already linked; records the
/// directory in plugins.toml
pub fn link(path: &str) -> Res<(Manifest, Vec<String>)> {
    let dir = plugin_dir(path)?;
    let (m, warn) = register(&dir, Some(String::new()))?;
    decl::set(&m.id, "source", Some(dir.to_string_lossy().into_owned().into()))?;
    Ok((m, warn))
}

/// The directory for agents: must be a real directory inside the plugin directory. For an agent that doesn't accept
/// plugin directories, or a layout Claude won't recognize, a warning
fn agent_parts(dir: &Path, m: &Manifest, warn: &mut Vec<String>) -> Res<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for agent in AGENTS {
        let Some(rel) = m.agent_dir(agent) else { continue };
        let old = m.agent.dir.is_empty();
        let what = if old { format!("[agents.{agent}] plugin_dir") } else { "[agent] dir".to_string() };
        let d = dir.join(rel).canonicalize().map_err(|_| tr!("{what}「{rel}」不存在", "{what} “{rel}” doesn't exist"))?;
        if !d.starts_with(dir) || !d.is_dir() {
            return Err(tr!("{what} 应为插件目录中的一个目录", "{what} has to be a directory inside the plugin"));
        }
        if !d.join(".claude-plugin").join("plugin.json").is_file() {
            warn.push(tr!(
                "{} 中没有 .claude-plugin/plugin.json:Claude、Codex 可能无法识别它是插件",
                "{} has no .claude-plugin/plugin.json: Claude and Codex may not recognize it as a plugin",
                d.display()
            ));
        }
        out.insert(agent.to_string(), d.to_string_lossy().into_owned());
    }
    for agent in m.agents.keys().filter(|a| !AGENTS_WITH_PLUGINS.contains(&a.as_str())) {
        warn.push(tr!(
            "{agent} 不支持旧写法的插件目录:不会加载 [agents.{agent}]",
            "{agent} ignores the old [agents.{agent}] form: not brought in"
        ));
    }
    Ok(out)
}

/// Whether this directory is one some plugin brings to agents (so adopting can tell which arguments aca added): the
/// plugin's own directory, or the copy aca made for Claude
pub fn is_agent_dir(d: &str) -> bool {
    Path::new(d).starts_with(mount::claude_root()) || registry().iter().any(|e| e.agents.values().any(|x| x == d))
}

/// Plugins installed from git live here: `<data dir>/plugins/<id>`
pub fn installed_root() -> PathBuf {
    paths::data_dir().join("plugins")
}

fn git(dir: Option<&Path>, args: &[&str]) -> Res<String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    let o = c.args(args).stdin(Stdio::null()).output().map_err(|e| tr!("无法启动 git:{e}", "can't run git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(format!("git {}:{}", args[0], String::from_utf8_lossy(&o.stderr).trim()))
    }
}

/// `owner/repo` is on GitHub; anything else (https, ssh, file://) goes to git as it is
fn git_url(src: &str) -> String {
    let short = src.split('/').count() == 2 && !src.contains(':') && !src.starts_with(['.', '/', '~']);
    if short { format!("https://github.com/{src}.git") } else { src.to_string() }
}

/// Clones under [`installed_root`] (checking out `rev` if given) and registers. `want` = only accept this id (when sync
/// installs from plugins.toml)
fn clone_in(src: &str, rev: Option<&str>, want: Option<&str>) -> Res<(Manifest, Vec<String>, String)> {
    let local = PathBuf::from(paths::expand(src));
    let url = if local.is_dir() { local.canonicalize().map_err(|e| e.to_string())?.to_string_lossy().into_owned() } else { git_url(src) };
    let root = installed_root();
    std::fs::create_dir_all(&root).map_err(|e| tr!("无法创建 {}:{e}", "can't create {}: {e}", root.display()))?;
    let tmp = root.join(format!(".installing-{}", paths::short_id()));
    let placed = (|| {
        git(None, &["clone", "--quiet", &url, &tmp.to_string_lossy()])?;
        if let Some(r) = rev {
            git(Some(&tmp), &["checkout", "--quiet", r])?;
        }
        let (m, _) = read_manifest(&tmp)?;
        if let Some(w) = want.filter(|w| *w != m.id) {
            return Err(tr!("{src} 中的插件是 {},不是 plugins.toml 中写的 {w}", "{src} holds {}, not {w} as plugins.toml says", m.id));
        }
        if let Some(e) = registry().into_iter().find(|e| e.id == m.id) {
            return Err(tr!(
                "已存在 {}({}):如需替换,先运行 aca plugin remove {}",
                "{} is already there ({}): to replace it, aca plugin remove {} first",
                m.id,
                e.path,
                m.id
            ));
        }
        let dest = root.join(&m.id);
        if dest.exists() {
            return Err(tr!("{} 已存在", "{} already exists", dest.display()));
        }
        std::fs::rename(&tmp, &dest).map_err(|e| tr!("无法移动:{e}", "can't move it into place: {e}"))?;
        Ok(dest)
    })();
    let dest = placed.inspect_err(|_| {
        // Half cloned, or a bad manifest: that's a temp directory aca just made itself
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

/// `aca plugin install <source> [--ref version]`: a local directory is just [`link`] (a plugin you're writing; changes
/// apply at once); a git repository is cloned under [`installed_root`] and then linked, with plugins.toml recording where
/// it came from and plugins.lock the commit. `--ref` pins a tag / branch / commit (a local repository with `--ref` is
/// cloned too)
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

/// Installs a plugin shipped with aca: written out and registered, with only a header in plugins.toml (no source: sync
/// on another machine knows it too)
fn install_bundled(id: &str) -> Res<(Manifest, Vec<String>)> {
    if let Some(e) = registry().into_iter().find(|e| e.id == id && e.source != first_party::SOURCE) {
        return Err(tr!(
            "已存在 {id}({}):如需换成 aca 自带的版本,先运行 aca plugin remove {id}",
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

/// Installs the first-party plugins in the manifest's `requires` that aren't there yet; returns notes for people
fn install_requires(m: &Manifest) -> Vec<String> {
    let have: Vec<String> = registry().into_iter().map(|e| e.id).collect();
    let mut out = vec![];
    for r in m.requires.iter().filter(|r| !have.contains(r)) {
        out.push(match install_bundled(r) {
            Ok(_) => tr!("它依赖 {r},已一并安装", "it needs {r}; installed that too"),
            Err(e) => tr!("它依赖 {r},但安装失败:{e}", "it needs {r}, which didn't install: {e}"),
        });
    }
    out
}

/// `aca plugin update <id>`: pulls a git-installed plugin (fast-forward only; pinned ones are left alone), re-reads the
/// manifest and records the new commit. Returns one sentence for people
pub fn update(id: &str) -> Res<String> {
    let e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有插件 {id}", "no plugin {id}"))?;
    if e.source.is_empty() {
        return Ok(tr!("{id}:链接的本地目录,aca 不负责更新", "{id}: a linked local directory; aca doesn't update it"));
    }
    if e.source == user::SOURCE {
        return Ok(tr!(
            "{id}:你自己的插件(~/.config/aca/user/),aca 不负责更新",
            "{id}: your own (~/.config/aca/user/); aca doesn't update it"
        ));
    }
    if e.source == first_party::SOURCE {
        first_party::unpack(id)?;
        let (m, _) = reregister(Path::new(&e.path))?;
        return Ok(tr!(
            "{id}:aca 自带,版本随 aca,当前为 {}(升级 aca 即可升级它)",
            "{id}: ships with aca and follows it; now {} (upgrading aca upgrades it)",
            m.version
        ));
    }
    if let Some((_, rev)) = e.source.rsplit_once('#') {
        return Ok(tr!(
            "{id}:固定在 {rev},不更新(如需换版本:修改 plugins.toml 的 ref 后运行 aca plugin sync)",
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
        tr!("{id}:已是最新版本({after})", "{id}: already up to date ({after})")
    } else {
        tr!("{id}:{before} → {after}({} {})", "{id}: {before} → {after} ({} {})", m.name, m.version)
    })
}

/// Unregisters and deletes the directory of a git-installed plugin (a linked local directory is yours and left alone).
/// The plugin's own config and state stay. Doesn't touch plugins.toml. When a plugin is no longer used, what aca put
/// there for it is cleaned up: the session content handed to its panes (`plugin-input/<plugin>.<pane>.txt`, which holds
/// the session's replies), and the config and state directories aca created for it — only if empty; anything in them
/// belongs to the plugin and stays
fn sweep(id: &str) {
    for f in std::fs::read_dir(paths::state_dir().join("plugin-input")).into_iter().flatten().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        // Pane ids have no dots: a file of `a.b` is never taken for one of `a`
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
    // Registered paths have links resolved (macOS's /var is /private/var): both sides match when comparing
    let root = if e.source == first_party::SOURCE { first_party::root() } else { installed_root() };
    let root = root.canonicalize().unwrap_or(root);
    let id = &e.id;
    if !e.source.is_empty() && dir.starts_with(&root) {
        std::fs::remove_dir_all(dir)
            .map_err(|err| tr!("已取消登记,但无法删除 {}:{err}", "unregistered, but can't delete {}: {err}", dir.display()))?;
        Ok(tr!("已删除 {id}({})", "removed {id} ({})", dir.display()))
    } else {
        Ok(tr!("已移除 {id}:目录 {} 是链接的,未删除", "no longer using {id}: {} is a linked directory, left alone", dir.display()))
    }
}

/// `aca plugin remove <id>`: stop using it. A git-installed one is deleted with its directory; a linked local directory
/// is yours and left alone. Also struck from plugins.toml and plugins.lock; the plugin's own config and state stay
pub fn remove(id: &str) -> Res<String> {
    let e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有插件 {id}", "no plugin {id}"))?;
    mine_stays(&e)?;
    let said = drop_entry(&e)?;
    decl::forget(id)?;
    decl::lock(id, None)?;
    Ok(said)
}

/// Your own one can't be removed (it follows ~/.config/aca/user/)
fn mine_stays(e: &Entry) -> Res<()> {
    if e.source != user::SOURCE {
        return Ok(());
    }
    Err(tr!(
        "{} 是你自己的插件:删除 {} 后无法恢复;如只想关闭:aca plugin disable {}",
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
        return Err(tr!("未链接插件 {id}", "{id} isn't linked"));
    }
    save(&reg)?;
    timer::drop_plugin(id);
    sweep(id);
    health::clear(id);
    decl::forget(id)
}

pub fn enable(id: &str, on: bool) -> Res<()> {
    let mut reg = registry();
    reg.iter_mut().find(|e| e.id == id).ok_or_else(|| tr!("未链接插件 {id}", "{id} isn't linked"))?.enabled = on;
    save(&reg)?;
    let _ = timer::sync(id);
    decl::set(id, "enabled", (!on).then_some(false.into()))
}

/// `aca plugin sync`: makes this machine match plugins.toml. Installs what's missing (git ones at the commit in
/// plugins.lock), removes what isn't listed, enables / disables as written; settings and who gets what take effect the
/// next time they're used. Returns one line per thing done; one failure doesn't hold up the rest
pub fn sync() -> Res<Vec<String>> {
    decl::ensure()?;
    let wants = decl::wants()?;
    let locked = decl::locked();
    let mut said: Vec<String> = first_party::refresh()
        .into_iter()
        .map(|id| tr!("{id}:已换成当前 aca 自带的版本", "{id}: now the version this aca ships"))
        .collect();
    for e in registry() {
        // Your own one is there even though it isn't in plugins.toml
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
    // You changed hotkeys by hand in plugins.toml: the registry didn't change, but tmux has to follow too
    bind_hotkeys();
    Ok(said)
}

/// In plugins.toml but not on this machine yet: local directories are linked, git ones cloned (checked out at ref when
/// pinned, otherwise at the commit in the lock)
fn sync_add(id: &str, w: &decl::Want, commit: Option<&str>) -> Res<Option<String>> {
    if w.source.is_empty() && first_party::known(id) {
        install_bundled(id)?;
        if !w.on() {
            enable_quiet(id, false)?;
        }
        return Ok(Some(tr!("已安装 {id}(aca 自带)", "installed {id} (ships with aca)")));
    }
    if w.source.is_empty() {
        return Err(t!("plugins.toml 中没有 source,无法安装", "no source in plugins.toml; can't install").into());
    }
    let local = PathBuf::from(paths::expand(&w.source));
    let r = if local.is_dir() && w.rev.is_empty() {
        let dir = plugin_dir(&w.source)?;
        let (m, _) = register(&dir, Some(String::new()))?;
        if m.id != id {
            return Err(tr!("{} 中的插件是 {},不是 {id}", "{} holds {}, not {id}", w.source, m.id));
        }
        tr!("已链接 {id}", "linked {id}")
    } else {
        let rev = (!w.rev.is_empty()).then_some(w.rev.as_str()).or(commit);
        let (_, _, c) = clone_in(&w.source, rev, Some(id))?;
        if w.rev.is_empty() {
            // Installing from the lock doesn't count as pinned: update still pulls later
            let mut reg = registry();
            if let Some(e) = reg.iter_mut().find(|e| e.id == id) {
                e.source = e.source.split('#').next().unwrap_or("").to_string();
                let dir = PathBuf::from(&e.path);
                save(&reg)?;
                if commit.is_some() {
                    // Checking out the locked commit leaves HEAD detached: go back to the default branch and put it at that commit, so
                    // update can fast-forward
                    let branch = git(Some(&dir), &["rev-parse", "--abbrev-ref", "origin/HEAD"]).unwrap_or_default();
                    if let Some(b) = branch.strip_prefix("origin/") {
                        git(Some(&dir), &["checkout", "--quiet", "-B", b, &c])?;
                        let _ = git(Some(&dir), &["branch", "--quiet", "--set-upstream-to", &branch]);
                    }
                }
            }
        }
        tr!("已安装 {id}({})", "installed {id} ({})", &c[..c.len().min(7)])
    };
    if !w.on() {
        enable_quiet(id, false)?;
    }
    Ok(Some(r))
}

/// Already on this machine: enabled / disabled as plugins.toml says; for a git-installed one that isn't pinned and whose
/// locked commit differs from the current one, check that out
fn sync_one(e: &Entry, w: &decl::Want, commit: Option<&str>) -> Res<Option<String>> {
    let mut said = vec![];
    if e.enabled != w.on() {
        enable_quiet(&e.id, w.on())?;
        said.push(if w.on() { t!("已开启", "switched on") } else { t!("已关闭", "paused") }.to_string());
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
                said.push(tr!("已切换到 {}", "now at {}", &c[..c.len().min(7)]));
            }
        } else {
            decl::lock(&e.id, Some(&git(Some(dir), &["rev-parse", "HEAD"])?))?;
        }
    }
    Ok((!said.is_empty()).then(|| format!("{}:{}", e.id, said.join(t!(",", ", ")))))
}

/// Only changes the on/off in the registry (plugins.toml already says so)
fn enable_quiet(id: &str, on: bool) -> Res<()> {
    let mut reg = registry();
    if let Some(e) = reg.iter_mut().find(|e| e.id == id) {
        e.enabled = on;
    }
    save(&reg)?;
    let _ = timer::sync(id);
    Ok(())
}

/// A setting as it is now
#[derive(Clone, Debug, PartialEq)]
pub struct SettingNow {
    pub name: String,
    /// The current value: the manifest's default, with what plugins.toml changed on top; neither = an empty string
    pub value: toml::Value,
    pub default: Option<toml::Value>,
    pub desc: String,
    /// Changed in plugins.toml
    pub changed: bool,
    /// Not shown in full (the manifest's `secret`)
    pub secret: bool,
}

impl SettingNow {
    /// The value for people: strings without quotes; secret ones masked (an empty one is still shown as empty)
    pub fn shown(&self) -> String {
        let v = plain(&self.value);
        if !self.secret || v.is_empty() {
            return v;
        }
        let tail: String = v.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        if v.chars().count() > 12 { format!("••••{tail}") } else { "••••".into() }
    }
}

/// A plugin's current settings (in manifest order)
pub fn settings_of(id: &str) -> Res<Vec<SettingNow>> {
    let e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("没有插件 {id}", "no plugin {id}"))?;
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

/// A setting's value for people / scripts: strings without quotes, everything else as TOML
pub fn plain(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Changes a setting (written to plugins.toml); None = back to the default. The given text is read as the default's type
/// (see [`typed`])
pub fn set_setting(id: &str, name: &str, value: Option<&str>) -> Res<()> {
    let all = settings_of(id)?;
    let s = all.iter().find(|s| s.name == name).ok_or_else(|| tr!("{id} 没有设置项 {name}", "{id} has no setting {name}"))?;
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

/// Reads the text you give as the default's type: a string default takes it as a string (no quotes needed, quotes also
/// accepted), integers, floats and on/off must match, arrays and tables are written as TOML; without a default it's read
/// as TOML, and as a string if that fails
pub fn typed(v: &str, default: Option<&toml::Value>) -> Res<toml::Value> {
    use toml::Value as V;
    let toml_of = |v: &str| format!("v = {v}").parse::<toml::Table>().ok().and_then(|mut t| t.remove("v"));
    let t = v.trim();
    match default {
        Some(V::String(_)) => Ok(match toml_of(t) {
            Some(V::String(s)) if t.starts_with(['"', '\'']) => V::String(s),
            _ => V::String(v.to_string()),
        }),
        Some(V::Integer(_)) => t.parse::<i64>().map(V::from).map_err(|_| tr!("应为整数(当前为 {t})", "takes an integer (got {t})")),
        Some(V::Float(_)) => t.parse::<f64>().map(V::from).map_err(|_| tr!("应为数字(当前为 {t})", "takes a number (got {t})")),
        Some(V::Boolean(_)) => match t {
            "true" | "yes" | "on" | "1" => Ok(V::Boolean(true)),
            "false" | "no" | "off" | "0" => Ok(V::Boolean(false)),
            _ => Err(tr!("应为 true / false(当前为 {t})", "takes true / false (got {t})")),
        },
        Some(V::Array(_)) => match toml_of(t) {
            Some(a @ V::Array(_)) => Ok(a),
            _ => Err(tr!("应为 TOML 数组:[\"a\", \"b\"](当前为 {t})", "takes a TOML array: [\"a\", \"b\"] (got {t})")),
        },
        Some(V::Table(_)) => match toml_of(t) {
            Some(x @ V::Table(_)) => Ok(x),
            _ => Err(tr!("应为 TOML 表:{{ a = 1 }}(当前为 {t})", "takes a TOML table: {{ a = 1 }} (got {t})")),
        },
        _ => Ok(toml_of(t).unwrap_or_else(|| V::String(v.to_string()))),
    }
}

/// Where plugins.toml and this machine differ (what `sync` would do), one sentence each. An unreadable file gets a
/// sentence too; no file yet = no differences
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
            None => out.push(tr!("将安装 {id}", "install {id}")),
            Some(e) if e.enabled != w.on() => {
                out.push(if w.on() { tr!("将开启 {id}", "switch on {id}") } else { tr!("将关闭 {id}", "pause {id}") })
            }
            Some(_) => {}
        }
    }
    for e in reg.iter().filter(|e| !wants.contains_key(&e.id) && e.source != user::SOURCE) {
        out.push(tr!("将删除 {}", "remove {}", e.id));
    }
    out
}

/// Where a plugin keeps its own config / state: aca only creates the directory, its content belongs to the plugin
pub fn config_dir(id: &str) -> PathBuf {
    paths::config_dir().join("plugins").join(id)
}

pub fn state_dir(id: &str) -> PathBuf {
    paths::state_dir().join("plugins").join(id)
}

pub fn log_file(id: &str) -> PathBuf {
    paths::state_dir().join("plugin-logs").join(format!("{id}.log"))
}

/// Which session an action call / pane opening is aimed at
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub session: String,
    pub dir: String,
    /// The session's id in tmux (`$3`) and the agent's pane (`%5`); empty when not running
    pub tmux: String,
    pub pane: String,
    /// The session that delegated it (one created by `aca new` inside some session hangs under it); empty for ones you started
    pub parent: String,
    /// menu / cli / event / agent / timer
    pub source: String,
}

fn find(id: &str) -> Res<(Entry, Manifest)> {
    let mut e = registry().into_iter().find(|e| e.id == id).ok_or_else(|| tr!("未链接插件 {id}", "{id} isn't linked"))?;
    // Built-in: right after an aca upgrade, switch to the freshly written copy first (the old one may still call commands
    // that are gone)
    if e.source == first_party::SOURCE && first_party::refresh_one(id) {
        e = registry().into_iter().find(|e| e.id == id).unwrap_or(e);
    }
    if !e.enabled {
        return Err(tr!("{id} 已关闭(aca plugin enable {id})", "{id} is off (aca plugin enable {id})"));
    }
    let (m, _) = read_manifest(Path::new(&e.path))?;
    Ok((e, m))
}

/// `./x` and `../x` are relative to the plugin directory (not the caller's directory, nor the session's directory of
/// `cwd = "session"`)
fn program(e: &Entry, prog: &str) -> PathBuf {
    if prog.starts_with("./") || prog.starts_with("../") { Path::new(&e.path).join(prog) } else { PathBuf::from(prog) }
}

/// Where it runs: the session's directory if `cwd = "session"` and there is a session, otherwise the plugin directory
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

/// Setting name → environment variable name: `max_continue` → `ACA_SETTING_MAX_CONTINUE`
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
        // Who called it (menu / cli / agent …), so plugins written in shell don't have to parse the JSON above
        ("ACA_INVOKED_BY".into(), ctx.source.clone()),
        // Which language the interface speaks now: plugins speak the same (zh / en)
        ("ACA_LANG".into(), if crate::i18n::lang() == crate::i18n::Lang::Zh { "zh" } else { "en" }.into()),
        ("ACA_PLUGIN_SETTINGS_JSON".into(), serde_json::to_string(&settings).unwrap_or_else(|_| "{}".into())),
        // Set to empty even with no session: when called from inside some session, don't hand that session's id to the plugin
        ("ACA_SESSION_ID".into(), ctx.session.clone()),
        ("ACA_SESSION_DIR".into(), ctx.dir.clone()),
        ("ACA_SESSION_PANE".into(), ctx.pane.clone()),
        // The session that delegated it: hooks looking for a worker's parent don't have to ask aca show
        ("ACA_SESSION_PARENT".into(), ctx.parent.clone()),
        // Which timer is running (`plugin.command`); empty otherwise: plugin commands started again inside a timer's run don't
        // inherit it
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
    // The arguments once more, one by one (ACA_ARG_<name>): plugins written in shell don't have to parse JSON
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

/// `plugin id.action id` → (plugin id, action id): plugin ids may contain dots, action ids don't
fn split_qualified(q: &str) -> Res<(&str, &str)> {
    q.rsplit_once('.').ok_or_else(|| tr!("能力应写为 插件.能力:{q}", "an action is written PLUGIN.ACTION: {q}"))
}

/// Turns the arguments given on a call (`name=value`) into JSON by the manifest's types; missing required ones,
/// undeclared extras and wrong types are all reported
pub fn args_json(a: &Action, given: &[(String, String)]) -> Res<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    let sorted: BTreeMap<&String, &String> = given.iter().map(|(k, v)| (k, v)).collect();
    for (k, v) in sorted {
        let Some((ty, _)) = a.arg(k) else {
            return Err(tr!("{} 没有参数 {k}", "{} has no argument {k}", a.id));
        };
        let val = match ty {
            "int" => v.parse::<i64>().map(serde_json::Value::from).map_err(|_| tr!("{k} 应为整数", "{k} takes an integer"))?,
            "bool" => match v.as_str() {
                "true" | "yes" | "1" => true.into(),
                "false" | "no" | "0" => false.into(),
                _ => return Err(tr!("{k} 应为 true / false", "{k} takes true / false")),
            },
            "path" => paths::expand(v).into(),
            _ => v.clone().into(),
        };
        out.insert(k.clone(), val);
    }
    if let Some(k) = a.args.keys().find(|k| !out.contains_key(*k) && a.arg(k).is_some_and(|(_, opt)| !opt)) {
        return Err(tr!("{} 需要参数 {k}", "{} needs argument {k}", a.id));
    }
    Ok(out)
}

/// Runs an action in the foreground (output goes straight to the caller) and returns its exit code. `given` = the
/// arguments (`name=value`)
pub fn invoke(qualified: &str, ctx: &Context, given: &[(String, String)]) -> Res<i32> {
    let (pid, aid) = split_qualified(qualified)?;
    let (e, m) = find(pid)?;
    let a = m.actions.iter().find(|a| a.id == aid).ok_or_else(|| tr!("{pid} 没有动作 {aid}", "{pid} has no action {aid}"))?;
    let to = match ctx.source.as_str() {
        "cli" => "cli",
        "agent" => "agent",
        _ => "menu",
    };
    if !a.exposed(to) {
        return Err(tr!("{pid}.{aid} 未提供给 {to}(见清单的 expose)", "{pid}.{aid} isn't exposed to {to} (expose in the manifest)"));
    }
    let args = serde_json::Value::Object(args_json(a, given)?).to_string();
    let mut c = command(&e, &m, &a.command, ctx, &command_ids(aid, &args), &a.cwd);
    let started = |err: std::io::Error| tr!("无法启动 {}:{err}", "can't start {}: {err}", a.command[0]);
    let st = if a.input.is_empty() {
        c.status().map_err(started)?
    } else {
        let text = input_text(&a.input, ctx);
        let mut child = c.stdin(Stdio::piped()).spawn().map_err(started)?;
        // Written from another thread: if the command doesn't read (long scrollback, full pipe) only that thread blocks, not
        // this side waiting for it to exit
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

/// Session content handed to a plugin (`input`): the last reply / the text on screen now / the screen plus scrollback.
/// No session, or the session isn't running = empty
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

/// The session's last reply: the copy the hook recorded (at most 4000 characters). If none was recorded (an adopted
/// session not restarted yet has no hooks) or it was cut short (ends with …), read the full one from its conversation
/// record (see [`crate::agent::Agent::last_reply`])
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

/// An entry in the interface's `:` menu: a command exposed to the menu, a pane or a view
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuItem {
    pub plugin: String,
    pub plugin_name: String,
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    /// The key `[[keys]]` binds to it (pressed in the `:` menu)
    pub key: Option<char>,
    pub contexts: Vec<String>,
    /// The command's arguments: (name, type, optional)
    pub args: Vec<(String, String, bool)>,
    /// The view's output is a list ([`ViewRows`])
    pub rows: bool,
    /// The keys each row of a list view takes
    pub acts: Vec<RowAct>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ItemKind {
    #[default]
    Command,
    Pane,
    View,
}

/// A key in a list view (the manifest's `[[views.keys]]`), with the command it runs already looked up
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowAct {
    /// One character, or `enter`
    pub key: String,
    pub title: String,
    /// The command to run (whether or not it's in the menu); None for copying
    pub run: Option<MenuItem>,
    /// Arguments: (name, template)
    pub args: Vec<(String, String)>,
    /// The template to copy
    pub copy: String,
    pub confirm: bool,
    /// Only works on a row
    pub row: bool,
}

/// The JSON a list view's command prints: `{"head": "…", "warn": "…", "empty": "…", "fields": {…}, "rows": [{…}]}`, all
/// optional
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ViewRows {
    /// A gray line above the list (how things stand)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub head: String,
    /// What needs attention (shown in the warning color)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub warn: String,
    /// What's shown when there are no rows
    #[serde(skip_serializing_if = "String::is_empty")]
    pub empty: String,
    /// Fields usable without being on a row (templates of `row = false` keys read these)
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
    pub rows: Vec<ViewRow>,
}

/// A row in the list
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ViewRow {
    pub id: String,
    /// The mark at the start of the row (● ✕ ?…)
    pub mark: String,
    /// The mark's color: ok / warn / bad / dim (not written = the normal color)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tone: String,
    pub text: String,
    /// The short bit on the right (state, time)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub right: String,
    /// What's shown below while on this row
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    /// Fields the key templates read
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
}

impl ViewRows {
    pub fn parse(out: &str) -> Res<ViewRows> {
        serde_json::from_str(out.trim()).map_err(|e| tr!("无法解析列表:{e}", "can't parse the list: {e}"))
    }

    /// Lines for people (`aca plugin view`)
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

/// Fills a template: `{name}` becomes the field (`id` and `text` are the row's own, the rest are in `fields`; when `row`
/// is None only the list's `fields` are read). If any field used is missing or empty, None (the key can't be pressed on
/// this row)
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
    /// `plugin.id`
    pub fn qualified(&self) -> String {
        format!("{}.{}", self.plugin, self.id)
    }

    /// Whether it makes sense on this kind of row (session / group / global)
    pub fn fits(&self, on: &str) -> bool {
        self.contexts.is_empty() || self.contexts.iter().any(|c| c == on || c == "global")
    }
}

/// Everything enabled plugins give the menu: commands exposed to menu, panes and views, by plugin and in manifest order.
/// Keys: `keys` in plugins.toml first, then the manifest's `[[keys]]` (see [`yours::settle`])
pub fn menu() -> Vec<MenuItem> {
    menu_and_notes().0
}

/// Keys not gotten / written wrong in plugins.toml: (plugin id, one sentence). Both `:` menu keys and hotkeys inside
/// sessions are here
pub fn key_notes() -> Vec<(String, String)> {
    let mut notes = menu_and_notes().1;
    notes.extend(hotkeys().1);
    notes
}

/// A hotkey inside sessions (keys already handed out): which plugin's which pane it opens
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HotkeyItem {
    pub plugin: String,
    pub pane: String,
    pub title: String,
    /// A tmux key name
    pub key: String,
}

/// Hotkeys of enabled plugins: the manifest's `[[hotkeys]]`, with what you wrote in `hotkeys` in plugins.toml on top (you
/// can give one to a pane the manifest gave none). Handed out the same way as `:` menu keys (see
/// [`yours::settle_hotkeys`]). The second value is the ones not gotten / written wrong: (plugin id, one sentence)
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
                    "plugins.toml 中 {} 的 hotkeys 写了 {t},但它没有这个窗格(热键只能打开窗格)",
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

/// The command a hotkey runs (given to tmux's run-shell, which replaces the `#{…}` in it with the session and terminal
/// where the key was pressed)
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

/// In the notes about what didn't get bound, aca's own keys (`[keys]`) are listed under this name (plugins' under the
/// plugin id)
pub const OWN_KEYS: &str = "aca";

/// The command aca's own key runs: go to the next session that needs you (`aca next`)
fn next_command(exe: &str, key: &str) -> String {
    format!("{} next {} {key} --session '#{{@aca_id}}' --client '#{{client_name}}'", crate::agent::sh_quote(exe), crate::tmux::HOTKEY_MARK)
}

/// Makes the hotkeys bound in tmux match what's wanted now: binds the wanted ones; unbinds ones aca bound before that
/// nobody wants now (plugin disabled, removed, key changed). Wanted = aca's own key (`[keys] next` in config.toml) and the
/// hotkeys of enabled plugins; when both want the same key, aca gets it. Returns what didn't get bound: (plugin id or
/// [`OWN_KEYS`], one sentence) — you bound that key to something else in tmux yourself, and it's left alone. Does nothing
/// when tmux isn't running: bindings go along with Ctrl+Q and are redone after creating a session and before entering one
/// (see [`crate::tmux::bind_detach`])
pub fn bind_hotkeys() -> Vec<(String, String)> {
    // Unit tests have no tmux server of their own: don't touch the real one
    if cfg!(test) {
        return vec![];
    }
    // Go by the registry as it is, without checking whether your own manifest changed ([`registry`] would: a change makes it
    // re-register and save the registry, which loops back here)
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
                "键 {k} 已在 tmux 中绑定了其他操作,未修改:「下一个待处理」未绑定(换一个键:aca tmux key next <键>)",
                "key {k} is bound to something else in your tmux and was left alone: “go to the next one waiting” isn't bound (pick another: aca tmux key next <key>)"
            ),
        ));
    }
    notes.extend(items.iter().filter(|h| next.as_ref() == Some(&h.key)).map(|h| {
        (
            h.plugin.clone(),
            tr!(
                "键 {} 已被 aca 占用(下一个待处理):{}「{}」未绑定(换一个键:aca plugin hotkey {} {} <键>)",
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
                    "键 {} 已在 tmux 中绑定了其他操作,未修改:{}「{}」未绑定(换一个键:aca plugin hotkey {} {} <键>)",
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

/// The hotkeys that should be bound in tmux, the same list [`bind_hotkeys`] binds: (key, command it runs). Doesn't say
/// which ones didn't get bound — re-binding before entering a session (see [`crate::tmux::bind_detach`]) only needs this
/// list. None in unit tests: don't touch the real tmux server
pub fn hotkey_binds() -> Option<Vec<(String, String)>> {
    if cfg!(test) {
        return None;
    }
    let (items, _) = hotkeys_in(read_registry());
    let next = crate::config::load().keys.next_key().ok().flatten();
    Some(want_keys(&items, next.as_ref()))
}

/// aca's own key, and the hotkeys of enabled plugins; when both want the same key, aca gets it
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
                    "plugins.toml 中 {} 的 keys 写了 {t},但它没有这一项(只有 : 菜单中的能力、窗格、视图能设键)",
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

/// A command's arguments, for the form: (name, type, optional), required ones first
fn command_args(a: &Action) -> Vec<(String, String, bool)> {
    let mut args: Vec<(String, String, bool)> =
        a.args.keys().filter_map(|k| a.arg(k).map(|(t, opt)| (k.clone(), t.to_string(), opt))).collect();
    args.sort_by_key(|(_, _, opt)| *opt);
    args
}

/// How long commands and views run from the interface may take at most
pub const CAPTURE_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs a command (exposed to menu, or run by a list view's key) or a view and collects the output: (succeeded, stdout +
/// stderr). The interface calls it in the background. A list view that succeeded hands over only stdout (that's the JSON;
/// stderr goes to the plugin's log); one that failed hands over both
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
            return Err(tr!("{pid}.{xid} 未提供给界面(见清单的 expose)", "{pid}.{xid} isn't exposed to the menu (expose in the manifest)"));
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

/// What a command gets when run: which one (under the new and the old name), and the arguments
pub(crate) fn command_ids<'a>(id: &'a str, args: &'a str) -> [(&'static str, &'a str); 3] {
    [("ACA_PLUGIN_ENTRYPOINT_ID", id), ("ACA_PLUGIN_ACTION_ID", id), ("ACA_PLUGIN_ARGS_JSON", args)]
}

/// Writes a section to the plugin's log (hook output goes there too; see it with `aca plugin log`)
fn log_line(id: &str, text: &str) {
    use std::io::Write;
    let log = log_file(id);
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        let _ = writeln!(f, "{text}");
    }
}

/// `aca plugin mcp <plugin id>`: what runs when an agent starts this plugin's MCP server (see [`mount`]). Changes to the
/// plugin directory, adds the plugin's environment (settings; the session id is the one in the environment the agent
/// gave), and replaces itself with the `[agent.mcp]` command from the manifest
pub fn exec_mcp(id: &str) -> Res<i32> {
    let (e, m) = find(id)?;
    let spec = m.agent.mcp.as_ref().ok_or_else(|| tr!("{id} 没有 [agent.mcp]", "{id} has no [agent.mcp]"))?;
    let ctx = agent_context();
    let err = command(&e, &m, &spec.command, &ctx, &[], "").exec();
    Err(tr!("无法启动 {}:{err}", "can't start {}: {err}", spec.command[0]))
}

/// Called from the agent side (MCP): the session is `ACA_SESSION_ID` in its environment
pub(crate) fn agent_context() -> Context {
    agent_context_for(&std::env::var("ACA_SESSION_ID").unwrap_or_default())
}

/// Aimed at this session (requests forwarded by the gateway say who in `_meta`)
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

/// Opens a plugin pane. A popup opens on terminal `client` (not given = tmux picks the current one); when the person isn't
/// in tmux (the interface run over a direct ssh from a phone) it runs in the foreground of the current terminal, with the
/// same effect: "covers the screen, and closing it brings you back"
pub fn open_pane(plugin: &str, pane: &str, placement: Option<&str>, client: Option<&str>, ctx: &Context) -> Res<i32> {
    let (e, m) = find(plugin)?;
    let p = m.panes.iter().find(|p| p.id == pane).ok_or_else(|| tr!("{plugin} 没有窗格 {pane}", "{plugin} has no pane {pane}"))?;
    let placement = placement.filter(|s| !s.is_empty()).unwrap_or(if p.placement.is_empty() { "popup" } else { &p.placement });
    // A pane is an interactive program whose stdin is the terminal: the session content is written to a file (one per pane,
    // overwritten the next time it opens)
    let input_file = if p.input.is_empty() {
        String::new()
    } else {
        let f = paths::state_dir().join("plugin-input").join(format!("{plugin}.{pane}.txt"));
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        std::fs::write(&f, input_text(&p.input, ctx)).map_err(|err| tr!("无法写入 {}:{err}", "can't write {}: {err}", f.display()))?;
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
            .map_err(|err| tr!("无法启动 {}:{err}", "can't start {}: {err}", p.command[0]))?;
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
        // The manifest's width and height are for popups; a split always takes the right 40%, leaving the agent's pane the bulk
        "split" if !ctx.pane.is_empty() => {
            vec!["split-window".into(), "-h".into(), "-l".into(), "40%".into(), "-t".into(), ctx.pane.clone()]
        }
        "window" if !ctx.tmux.is_empty() => vec!["new-window".into(), "-n".into(), title.clone(), "-t".into(), format!("{}:", ctx.tmux)],
        "split" | "window" => {
            return Err(t!("此窗格需要在运行中的会话里打开(--session)", "this pane has to open in a running session (--session)").into());
        }
        other => return Err(tr!("无法识别的 placement:{other}", "unknown placement: {other}")),
    };
    args.extend([if placement == "popup" { "-d" } else { "-c" }.to_string(), workdir(&e, &p.cwd, ctx)]);
    for (k, v) in env(&e, &m, ctx, &extra) {
        args.extend(["-e".into(), format!("{k}={v}")]);
    }
    args.push(crate::tmux::arg(&program(&e, &p.command[0]).to_string_lossy()));
    args.extend(p.command[1..].iter().map(|a| crate::tmux::arg(a)));
    let st = crate::tmux::cmd()
        .args(&args)
        .stdin(Stdio::inherit())
        .status()
        .map_err(|err| tr!("无法启动 tmux:{err}", "can't run tmux: {err}"))?;
    Ok(st.code().unwrap_or(1))
}

/// The event's line plus a few things about the session (usable in `if`): `parent`, `tool`, `account`, `group` (a field of
/// the same name in the event wins)
fn facts(line: &str, s: Option<&crate::store::Session>) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|_| json!({}));
    if let (Some(o), Some(s)) = (v.as_object_mut(), s) {
        for (k, val) in [("parent", &s.parent), ("tool", &s.tool), ("account", &s.account), ("group", &s.group)] {
            o.entry(k).or_insert_with(|| val.clone().into());
        }
    }
    v
}

/// An event arrived: each plugin subscribed to it runs one command (skipped when `if` doesn't match), **not waited for**
/// (called from the hook, it mustn't hold up the agent), with output going to the plugin's own log. Wrapped in an sh that
/// records the exit code when done (see [`health`]): failed runs are flagged in the status bar and on the plugins page
pub fn dispatch(event: &str, session: &str, line: &str) {
    let reg = registry();
    let subs: Vec<&Entry> = reg.iter().filter(|e| e.enabled && e.events.iter().any(|on| crate::event::matches(on, event))).collect();
    if subs.is_empty() {
        return;
    }
    // Only look up the session when someone subscribes; the pane is known only inside the hook (it runs in that very pane)
    let s = crate::store::Store::open().ok().and_then(|st| st.get(session).ok());
    let (dir, parent) = s.as_ref().map(|s| (s.dir.clone(), s.parent.clone())).unwrap_or_default();
    let in_hook = std::env::var("ACA_SESSION_ID").is_ok_and(|id| id == session);
    let pane = std::env::var("TMUX_PANE").ok().filter(|_| in_hook).unwrap_or_default();
    let ctx = Context { session: session.into(), dir, pane, parent, source: "event".into(), ..Default::default() };
    let facts = facts(line, s.as_ref());
    for e in subs {
        // Built-in: right after an aca upgrade, switch to the freshly written copy first (only reads its own few small files)
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
            // All three streams taken and in a process group of its own: it keeps running after the hook that called it exits, and
            // doesn't keep the agent's pipe open
            let _ = crate::proc::detach(
                command(e, &m, &argv, &ctx, &[("ACA_PLUGIN_EVENT", event), ("ACA_PLUGIN_EVENT_JSON", line)], "")
                    .stdin(Stdio::null())
                    .stdout(out)
                    .stderr(err),
            );
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
        // Secret ones are masked
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
        // Empty or missing fields: can't be pressed on this row
        assert_eq!(fill("{log}", &l, Some(&l.rows[0])), None);
        assert_eq!(fill("{url}", &l, Some(&l.rows[1])), None);
        // Not on a row: only the list's fields are read
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
        assert_eq!(warn, ["没有事件 turn.finished"]);
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
        assert!(bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[actions]]\nid = \"x.y\"\ncommand = [\"true\"]").contains("不能含点"));
        assert!(bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[actions]]\nid = \"x\"").contains("command"));
        assert!(
            bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\n[[panes]]\nid = \"x\"\nplacement = \"tab\"\ncommand = [\"true\"]")
                .contains("placement")
        );
        assert!(bad("id = \"a\"\nname = \"x\"\nversion = \"1\"\nmin_aca_version = \"99.0.0\"").contains("99.0.0"));
        assert!(bad("id = ").contains("无法解析"));
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
