//! Mounting: what plugins give an agent, brought in when it starts. Holds for that launch only; doesn't change the
//! account's config files or write into your repository.
//!
//! - **Claude**: plugin directories via `--plugin-dir` (several allowed); MCP servers written into an `--mcp-config` file
//!   (see agent/claude.rs). While the on/off setting named by the manifest's `[agent] auto` is on, what's given is a copy
//!   in `claude-plugins/<name>/local/`: the plugin directory copied as is, with `disable-model-invocation` removed from the
//!   headers of commands and skills (so the agent can use them on its own); when it's off or there's no auto, the plugin
//!   directory itself
//! - **Codex**: it only knows plugins installed in the plugin cache under `CODEX_HOME`, and has no flag to name a directory
//!   per launch (0.162 source: the `-c` layer yields no skill roots). So aca keeps a copy Codex understands in its own data
//!   directory, `codex-plugins/<name>/local/`: the plugin directory copied as is, slash commands turned into skills (Codex
//!   has no custom slash commands), no `.mcp.json` (MCP goes through `-c`); each account's `CODEX_HOME/plugins/cache/aca`
//!   links to `codex-plugins` (created once, config.toml untouched), and launching with
//!   `-c plugins.<name>@aca.enabled=true` turns it on for that launch only. This relies on Codex's internal cache layout,
//!   not a public interface: when something else is already there and the link can't be made, only MCP is mounted, with
//!   the reason in `plugin-logs/mount.log`. MCP is made into copies too: one plugin holding only a `.mcp.json` per server,
//!   `codex-plugins/mcp-<name>/local/`, also turned on per session (see [`codex_mcp_copy`], which also says why not
//!   `-c mcp_servers`). Only when the link can't be made is it `-c mcp_servers.<name>={…}`; that passes only allowlisted
//!   variables to the MCP child process, so the session id has to be written in `env`
//! - **pi / opencode**: MCP only, no plugin directory (the skill and command formats haven't been checked yet). For pi,
//!   aca's extension calls `registerMcpServer` for each one when it starts (that list's path is in `ACA_PI_MCP`, see
//!   agent/pi.rs); for opencode they're written into `mcp` of the config layered on at launch (see agent/opencode.rs)
//! - For every agent a plugin's MCP server is `aca plugin mcp <plugin id>`: aca changes to the plugin directory, adds the
//!   plugin's environment (settings, session), then replaces itself with its command
//! - Commands exposed to agents (`expose = ["agent"]`) are wrapped as tools by aca's own `aca plugin tools`; this MCP
//!   server is called `aca`
//! - With the gateway on ([`crate::gateway`]): plugins' MCP servers (those whose via isn't direct) and commands all
//!   belong to the gateway, and the agent gets just one `aca`, a bridge to the gateway (`aca plugin gateway connect`);
//!   via = direct ones are still attached directly
//!
//! By default nothing is brought: when no enabled plugin gives this session anything, its command line has not one extra
//! argument.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use super::{Entry, Manifest, decl, read_manifest, registry};
use crate::paths::{self, Res};
use crate::tr;

/// What to bring when starting a session
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mounts {
    /// Plugin directories (Claude: `--plugin-dir`)
    pub dirs: Vec<String>,
    /// MCP servers attached directly: (name, arguments for aca). The command is always aca itself
    pub mcp: Vec<(String, Vec<String>)>,
    /// Codex: plugins to turn on (`-c plugins.<name>@aca.enabled=true`)
    pub codex_plugins: Vec<String>,
}

/// Plugin id → a form usable in Codex plugin names and MCP server names: letters, digits, `_`, `-`; anything else becomes
/// `-` (`aca.delegate` → `aca-delegate`)
pub fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect()
}

/// The name of aca's own MCP server that wraps commands as tools
pub const TOOLS_SERVER: &str = "aca";

/// What this tool (`kind`) and this session (tool name, account) get. With `codex_home` given (Codex), the copies and
/// link are prepared along the way
pub fn plan(kind: &str, tool: &str, account: &str, codex_home: Option<&Path>) -> Mounts {
    // aca was just upgraded: refresh the built-in plugins first, then work out what to mount
    if super::AGENTS.contains(&kind) {
        super::first_party::refresh();
    }
    let wants = decl::wants().unwrap_or_default();
    let reaches = |e: &Entry| e.enabled && wants.get(&e.id).is_none_or(|w| w.reaches(tool, kind, account));
    let gateway = crate::gateway::enabled();
    let mut out = Mounts::default();
    let mut tools = false;
    let mut linked = None;
    for e in registry().into_iter().filter(|e| reaches(e)) {
        if let Some(dir) = e.agents.get(kind) {
            match codex_home {
                None => out.dirs.push(claude_dir(&e, dir)),
                Some(home) => {
                    let ok = *linked.get_or_insert_with(|| codex_link(home).map_err(|err| note(&err)).is_ok());
                    if ok {
                        match read_manifest(Path::new(&e.path)).and_then(|(m, _)| codex_copy(&e, Path::new(dir), &m)) {
                            Ok(name) => out.codex_plugins.push(name),
                            Err(err) => note(&format!("{}:{err}", e.id)),
                        }
                    }
                }
            }
        }
        if let Some(mcp) = e.mcp.as_ref().filter(|m| m.targets.is_empty() || m.targets.iter().any(|t| t == kind)) {
            match (mcp.via.as_str(), gateway) {
                // Gateway on: anything that doesn't have to be attached directly goes to the gateway
                (via, true) if via != "direct" => {}
                ("gateway", false) => note(&tr!(
                    "{}:它的 MCP 只通过 gateway 提供,gateway 未开启,本次不挂载",
                    "{}: its MCP only goes through the gateway, which is off; not mounted",
                    e.id
                )),
                _ => out.mcp.push((format!("aca-{}", safe(&e.id)), vec!["plugin".into(), "mcp".into(), e.id.clone()])),
            }
        }
        tools |= !super::yours::agent_tools(&e, wants.get(&e.id)).is_empty();
    }
    if gateway {
        if crate::gateway::offers(kind, tool, account) {
            out.mcp.push((crate::gateway::SERVER.into(), vec!["plugin".into(), "gateway".into(), "connect".into()]));
        }
    } else if tools {
        out.mcp.push((TOOLS_SERVER.into(), vec!["plugin".into(), "tools".into()]));
    }
    // Codex: MCP servers are made into copies too and turned on per session; if the link or copy can't be made, still `-c`
    if let Some(home) = codex_home
        && !out.mcp.is_empty()
        && *linked.get_or_insert_with(|| codex_link(home).map_err(|err| note(&err)).is_ok())
    {
        let aca = crate::agent::self_exe();
        out.mcp.retain(|(name, args)| match codex_mcp_copy(name, &aca, args) {
            Ok(p) => {
                out.codex_plugins.push(p);
                false
            }
            Err(err) => {
                note(&format!("{name}:{err}"));
                true
            }
        });
    }
    out
}

/// What this tool and this session should get now, as one line (without touching Codex's copies or link): recorded at
/// start and compared later; a difference means the running session needs a restart to get it. Only what goes to the
/// agent counts (directories, whether the agent may use their commands on its own, MCP, tools, gateway on or off);
/// plugins with only event hooks don't
pub fn signature(kind: &str, tool: &str, account: &str) -> String {
    if !super::MCP_AGENTS.contains(&kind) {
        return String::new();
    }
    let wants = decl::wants().unwrap_or_default();
    let mut parts = vec![];
    for e in registry().into_iter().filter(|e| e.enabled && wants.get(&e.id).is_none_or(|w| w.reaches(tool, kind, account))) {
        let mut p = vec![];
        if e.agents.contains_key(kind) {
            p.push("dir".to_string());
            if kind == "claude" && read_manifest(Path::new(&e.path)).is_ok_and(|(m, _)| auto_on(&e, &m)) {
                p.push("auto".to_string());
            }
        }
        if let Some(m) = e.mcp.as_ref().filter(|m| m.targets.is_empty() || m.targets.iter().any(|t| t == kind)) {
            p.push(format!("mcp-{}", if m.via.is_empty() { "auto" } else { &m.via }));
        }
        let given = super::yours::agent_tools(&e, wants.get(&e.id));
        if !given.is_empty() {
            p.push(format!("tools-{}", given.join("+")));
        }
        if !p.is_empty() {
            parts.push(format!("{}:{}", e.id, p.join(":")));
        }
    }
    if !parts.is_empty() && crate::gateway::enabled() {
        parts.push("gateway".into());
    }
    parts.join(" ")
}

/// Running sessions whose plugin offerings differ from what they started with (a restart is needed to get them): (id, title)
pub fn stale() -> Vec<(String, String)> {
    let (Ok(store), cfg) = (crate::store::Store::open(), crate::config::load()) else { return vec![] };
    let panes = crate::tmux::panes();
    let mut out = vec![];
    for s in store.sessions(false).unwrap_or_default() {
        if crate::tmux::agent_pane(&panes, &s.id).is_none() {
            continue;
        }
        let kind = cfg.tool(&s.tool).map(|t| t.kind).unwrap_or_default();
        if signature(&kind, &s.tool, &s.account) != crate::runtime::load(&s.id).mounts {
            out.push((s.id.clone(), s.title.clone()));
        }
    }
    out
}

/// Logs one line on why something couldn't be mounted (there's nowhere to say it while starting an agent)
fn note(msg: &str) {
    let log = super::log_file("mount");
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        use std::io::Write;
        let _ = writeln!(f, "{} {msg}", paths::now());
    }
}

/// All of Codex's copies live here: `<data dir>/codex-plugins`
pub fn codex_root() -> PathBuf {
    paths::data_dir().join("codex-plugins")
}

/// Links `CODEX_HOME/plugins/cache/aca` to [`codex_root`]. Leaves it if already linked; anything else there (another
/// directory, a link elsewhere) is left alone, Err. A link whose target is gone gets replaced: tests that pair a sandbox
/// `ACA_HOME` with the real `CODEX_HOME` leave those behind, and without replacing it we'd keep falling back to
/// `-c mcp_servers` and Codex couldn't make titles
pub fn codex_link(home: &Path) -> Res<()> {
    link_to(home, &codex_root())
}

fn link_to(home: &Path, target: &Path) -> Res<()> {
    let cache = home.join("plugins").join("cache");
    let link = cache.join("aca");
    if link.symlink_metadata().is_ok() {
        return match std::fs::read_link(&link) {
            Ok(t) if t == target => Ok(()),
            Ok(_) if !link.exists() => {
                std::fs::remove_file(&link).map_err(|e| tr!("无法删除 {}:{e}", "can't remove {}: {e}", link.display()))?;
                link_to(home, target)
            }
            _ => Err(tr!(
                "{} 已被占用,未修改:Codex 本次只挂载 MCP",
                "{} already holds something else; left alone: Codex only gets MCP this time",
                link.display()
            )),
        };
    }
    std::fs::create_dir_all(&cache).map_err(|e| tr!("无法创建 {}:{e}", "can't create {}: {e}", cache.display()))?;
    std::fs::create_dir_all(target).map_err(|e| tr!("无法创建 {}:{e}", "can't create {}: {e}", target.display()))?;
    std::os::unix::fs::symlink(target, &link).map_err(|e| tr!("无法链接 {}:{e}", "can't link {}: {e}", link.display()))
}

/// Whether a directory's files changed: a fingerprint of relative paths, sizes and modification times combined
fn fingerprint(dir: &Path) -> String {
    let mut files = vec![];
    walk(dir, dir, &mut files);
    files.sort();
    let mut h = DefaultHasher::new();
    files.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        let Ok(md) = std::fs::metadata(&p) else { continue };
        if md.is_dir() {
            walk(root, &p, out);
        } else {
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().into_owned();
            let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            out.push((rel, md.len(), mtime));
        }
    }
}

/// One copy at `<root>/local/`: left alone if the source didn't change (`fp` same as last recorded); if it did, a new one
/// is built and swapped in, so other sessions starting at that moment never read half a copy
fn rebuilt(root: &Path, fp: &str, build: impl FnOnce(&Path) -> Res<()>) -> Res<PathBuf> {
    let local = root.join("local");
    if std::fs::read_to_string(local.join(".aca-src")).ok().as_deref() == Some(fp) {
        return Ok(local);
    }
    let tmp = root.join(format!(".build-{}", paths::short_id()));
    if let Err(err) = build(&tmp).and_then(|()| write(&tmp.join(".aca-src"), fp)) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err);
    }
    // Move the old one aside before swapping in the new one (rename can't overwrite a non-empty directory)
    let old = root.join(format!(".old-{}", paths::short_id()));
    let had = local.exists() && std::fs::rename(&local, &old).is_ok();
    std::fs::rename(&tmp, &local).map_err(|err| tr!("无法放置 {}:{err}", "can't put {} in place: {err}", local.display()))?;
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(local)
}

/// The copy for Codex: `codex-plugins/<name>/local/`. Left alone if the plugin directory didn't change. Returns the plugin name
pub fn codex_copy(e: &Entry, src: &Path, m: &Manifest) -> Res<String> {
    let name = safe(&e.id);
    let fp = format!("{} {}", fingerprint(src), m.version);
    rebuilt(&codex_root().join(&name), &fp, |tmp| {
        copy_tree(src, tmp, &[".git", ".mcp.json"])?;
        commands_to_skills(tmp)?;
        let manifest = serde_json::json!({ "name": name, "version": m.version, "description": m.description });
        write(&tmp.join(".codex-plugin").join("plugin.json"), &serde_json::to_string_pretty(&manifest).unwrap())
    })?;
    Ok(name)
}

/// All of Claude's copies live here: `<data dir>/claude-plugins`
pub fn claude_root() -> PathBuf {
    paths::data_dir().join("claude-plugins")
}

/// Whether the on/off setting named by the manifest's `[agent] auto` is on now (no auto = off)
fn auto_on(e: &Entry, m: &Manifest) -> bool {
    !m.agent.auto.is_empty() && decl::settings(&e.id, m).get(&m.agent.auto).and_then(toml::Value::as_bool).unwrap_or(false)
}

/// Which directory Claude gets this time: normally the plugin directory; while the `[agent] auto` setting is on, the copy
/// with `disable-model-invocation` removed. If the copy can't be made, the plugin directory as before (reason in mount.log)
fn claude_dir(e: &Entry, dir: &str) -> String {
    match read_manifest(Path::new(&e.path)) {
        Ok((m, _)) if auto_on(e, &m) => claude_copy(e, Path::new(dir), &m).unwrap_or_else(|err| {
            note(&format!("{}:{err}", e.id));
            dir.to_string()
        }),
        _ => dir.to_string(),
    }
}

/// The copy for Claude: `claude-plugins/<name>/local/`, the plugin directory copied as is, with `disable-model-invocation`
/// removed from the headers of commands and skills. Left alone if the plugin directory didn't change. Returns the copy's path
pub fn claude_copy(e: &Entry, src: &Path, m: &Manifest) -> Res<String> {
    let fp = format!("{} {}", fingerprint(src), m.version);
    let local = rebuilt(&claude_root().join(safe(&e.id)), &fp, |tmp| {
        copy_tree(src, tmp, &[".git"])?;
        ["commands", "skills"].iter().try_for_each(|d| lift_all(&tmp.join(d)))
    })?;
    Ok(local.to_string_lossy().into_owned())
}

/// Runs [`lift`] over every `.md` under a directory (commands, SKILL.md); a missing directory isn't an error
fn lift_all(dir: &Path) -> Res<()> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            lift_all(&p)?;
        } else if p.extension().is_some_and(|x| x == "md") {
            let text = std::fs::read_to_string(&p).map_err(|e| tr!("无法读取 {}:{e}", "can't read {}: {e}", p.display()))?;
            let lifted = lift(&text);
            if lifted != text {
                write(&p, &lifted)?;
            }
        }
    }
    Ok(())
}

/// Removes the `disable-model-invocation` line from the header (between the two `---` lines); without a header or that
/// line, unchanged
pub(crate) fn lift(text: &str) -> String {
    let Some((front, body)) = text.strip_prefix("---\n").and_then(|r| r.split_once("\n---\n")) else { return text.to_string() };
    let kept: Vec<&str> = front.lines().filter(|l| l.split_once(':').is_none_or(|(k, _)| k.trim() != "disable-model-invocation")).collect();
    format!("---\n{}\n---\n{body}", kept.join("\n"))
}

fn write(p: &Path, text: &str) -> Res<()> {
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(p, text).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", p.display()))
}

/// Copies a directory as is (links are followed and copied as real files: Codex doesn't accept a manifest that's a link),
/// skipping the names in `skip`
fn copy_tree(src: &Path, dst: &Path, skip: &[&str]) -> Res<()> {
    std::fs::create_dir_all(dst).map_err(|e| tr!("无法创建 {}:{e}", "can't create {}: {e}", dst.display()))?;
    let rd = std::fs::read_dir(src).map_err(|e| tr!("无法读取 {}:{e}", "can't read {}: {e}", src.display()))?;
    for ent in rd.flatten() {
        let name = ent.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let (from, to) = (ent.path(), dst.join(&name));
        match std::fs::metadata(&from) {
            Ok(md) if md.is_dir() => copy_tree(&from, &to, skip)?,
            Ok(_) => {
                std::fs::copy(&from, &to).map_err(|e| tr!("无法复制 {}:{e}", "can't copy {}: {e}", from.display()))?;
            }
            Err(_) => {}
        }
    }
    Ok(())
}

/// Claude's slash commands (`commands/<name>.md`) become Codex skills (`skills/<name>/SKILL.md`): the header's description
/// is carried over, and `$ARGUMENTS` is replaced with a sentence explaining it. Not converted if a skill with that name
/// already exists
fn commands_to_skills(dir: &Path) -> Res<()> {
    let cmds = dir.join("commands");
    let Ok(rd) = std::fs::read_dir(&cmds) else { return Ok(()) };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        let skill = dir.join("skills").join(&stem).join("SKILL.md");
        if skill.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
        write(&skill, &command_as_skill(&stem, &text))?;
    }
    Ok(())
}

pub(crate) fn command_as_skill(name: &str, text: &str) -> String {
    let (front, body) = match text.strip_prefix("---\n").and_then(|r| r.split_once("\n---\n")) {
        Some((f, b)) => (f, b),
        None => ("", text),
    };
    let fields: BTreeMap<&str, &str> = front.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect();
    let desc = fields.get("description").copied().filter(|d| !d.is_empty()).map_or_else(|| format!("/{name}"), str::to_string);
    let body = body.replace("$ARGUMENTS", "(what the user asked for when invoking this skill)");
    format!("---\nname: {name}\ndescription: {desc}\n---\n{}", body.trim_start_matches('\n'))
}

/// Variables aca puts in the agent's environment when starting a session that MCP servers need (Codex's plugin MCP servers
/// pass them along by name from its own environment)
pub const MCP_ENV: &[&str] = &["ACA_SESSION_ID", "ACA_TOOL", "ACA_BIN", "ACA_HOME", "ACA_TMUX_SOCKET"];

/// An MCP server for Codex: a plugin holding only `.mcp.json`, `codex-plugins/mcp-<name>/local/`, left alone if its content
/// didn't change. Returns the plugin name.
///
/// Why not `-c mcp_servers.<name>={…}`: to title a conversation, Codex (0.162, 0.163) starts another thread, and turns off
/// every MCP server by putting `mcp_servers = { <name> = { enabled = false }, … }` in the request; that merges into the
/// same layer as the command line's `-c` and replaces the table by key, so the command given with `-c` is lost, the thread
/// fails with `invalid transport in mcp_servers.<name>`, and the conversation never gets a title. MCP servers brought by
/// plugins aren't in `mcp_servers`, and that thread turns off plugins without touching them. Things that differ per
/// session, like the session id, aren't written into the file: `env_vars` has Codex pass them from its own environment
pub fn codex_mcp_copy(name: &str, aca: &str, args: &[String]) -> Res<String> {
    let plugin = format!("mcp-{name}");
    let mcp = codex_mcp_json(name, aca, args);
    let mut h = DefaultHasher::new();
    mcp.hash(&mut h);
    rebuilt(&codex_root().join(&plugin), &format!("{:016x}", h.finish()), |tmp| {
        write(&tmp.join(".mcp.json"), &mcp)?;
        let manifest = serde_json::json!({ "name": plugin, "version": "0.0.0", "description": format!("aca: MCP {name}") });
        write(&tmp.join(".codex-plugin").join("plugin.json"), &serde_json::to_string_pretty(&manifest).unwrap())
    })?;
    Ok(plugin)
}

/// The `.mcp.json` of [`codex_mcp_copy`]
fn codex_mcp_json(name: &str, aca: &str, args: &[String]) -> String {
    let server = serde_json::json!({ "type": "stdio", "command": aca, "args": args, "env_vars": MCP_ENV });
    serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": { name: server } })).unwrap()
}

/// The `-c` value for one MCP server on the Codex side: `mcp_servers.<name>={ command = …, args = […], env = {…} }`.
/// Used only when the copy can't be made (conversation titles will fail, see [`codex_mcp_copy`])
pub fn codex_mcp_flag(name: &str, aca: &str, args: &[String], env: &[(String, String)]) -> String {
    let mut t = toml::Table::new();
    t.insert("command".into(), aca.into());
    t.insert("args".into(), toml::Value::Array(args.iter().map(|a| a.clone().into()).collect()));
    let vars: toml::Table = env.iter().map(|(k, v)| (k.clone(), v.clone().into())).collect();
    t.insert("env".into(), toml::Value::Table(vars));
    format!("mcp_servers.{name}={}", toml::Value::Table(t))
}

/// The pi side: `{ name: { command, args } }`, handed one by one to `pi.registerMcpServer` when aca's extension starts.
/// pi hides MCP tools behind codemode by default (reachable only by the model writing a script); aca's are only a few, so
/// they're declared to the model directly, as for Claude and Codex
pub fn pi_mcp_config(aca: &str, mcp: &[(String, Vec<String>)]) -> String {
    let servers: serde_json::Map<String, serde_json::Value> = mcp
        .iter()
        .map(|(name, args)| (name.clone(), serde_json::json!({ "command": aca, "args": args, "exposure": "direct" })))
        .collect();
    serde_json::to_string_pretty(&servers).unwrap()
}

/// The opencode side: the entries under `mcp` in the layered config (local, command and arguments as one array)
pub fn opencode_mcp(aca: &str, mcp: &[(String, Vec<String>)]) -> serde_json::Map<String, serde_json::Value> {
    mcp.iter()
        .map(|(name, args)| {
            let cmd: Vec<&str> = std::iter::once(aca).chain(args.iter().map(String::as_str)).collect();
            (name.clone(), serde_json::json!({ "type": "local", "command": cmd, "enabled": true }))
        })
        .collect()
}

/// The content of Claude's `--mcp-config`
pub fn claude_mcp_config(aca: &str, mcp: &[(String, Vec<String>)]) -> String {
    let servers: serde_json::Map<String, serde_json::Value> =
        mcp.iter().map(|(name, args)| (name.clone(), serde_json::json!({ "type": "stdio", "command": aca, "args": args }))).collect();
    serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": servers })).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_and_opencode_get_the_same_servers_in_their_own_shapes() {
        let mcp = vec![("aca".to_string(), vec!["plugin".to_string(), "tools".to_string()])];
        let pi: serde_json::Value = serde_json::from_str(&pi_mcp_config("/bin/aca", &mcp)).unwrap();
        assert_eq!(pi, serde_json::json!({ "aca": { "command": "/bin/aca", "args": ["plugin", "tools"], "exposure": "direct" } }));
        assert_eq!(
            serde_json::Value::Object(opencode_mcp("/bin/aca", &mcp)),
            serde_json::json!({ "aca": { "type": "local", "command": ["/bin/aca", "plugin", "tools"], "enabled": true } })
        );
    }

    #[test]
    fn ids_become_names_codex_and_mcp_accept() {
        assert_eq!(safe("aca.delegate"), "aca-delegate");
        assert_eq!(safe("owner/gh:glance_1"), "owner-gh-glance_1");
    }

    #[test]
    fn a_claude_command_turns_into_a_codex_skill() {
        let cmd = "---\ndescription: 派活给 worker\ndisable-model-invocation: true\n---\n\n做这件事:$ARGUMENTS\n";
        let s = command_as_skill("delegate", cmd);
        assert!(s.starts_with("---\nname: delegate\ndescription: 派活给 worker\n---\n做这件事:(what the user"), "{s}");
        assert!(!s.contains("disable-model-invocation"));
        // Without a header: the description is its command name
        assert!(command_as_skill("x", "hello").contains("description: /x\n"));
    }

    #[test]
    fn lifting_takes_only_the_no_model_invocation_line_out_of_the_head() {
        let cmd = "---\ndescription: 派活给 worker\ndisable-model-invocation: true\nargument-hint: [task]\n---\n\ndisable-model-invocation: 正文里的不动\n";
        assert_eq!(lift(cmd), "---\ndescription: 派活给 worker\nargument-hint: [task]\n---\n\ndisable-model-invocation: 正文里的不动\n");
        // No header, or no such line in it: unchanged
        for same in ["hello\n", "---\ndescription: x\n---\nhi\n"] {
            assert_eq!(lift(same), same);
        }
        // In the copy, commands and skills are processed, other files (.mcp.json too) copied as is; the plugin directory itself
        // is untouched
        let tmp = std::env::temp_dir().join(format!("aca-lift-{}", paths::short_id()));
        let (src, dst) = (tmp.join("src"), tmp.join("dst"));
        write(&src.join("commands/go.md"), cmd).unwrap();
        write(&src.join("skills/s/SKILL.md"), "---\nname: s\ndisable-model-invocation: true\n---\nx\n").unwrap();
        write(&src.join(".mcp.json"), "{}").unwrap();
        copy_tree(&src, &dst, &[".git"]).unwrap();
        ["commands", "skills", "nope"].iter().try_for_each(|d| lift_all(&dst.join(d))).unwrap();
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert!(!read(dst.join("commands/go.md")).contains("disable-model-invocation: true"));
        assert_eq!(read(dst.join("skills/s/SKILL.md")), "---\nname: s\n---\nx\n");
        assert!(dst.join(".mcp.json").is_file() && read(src.join("commands/go.md")) == cmd);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_flags_for_codex_and_the_file_for_claude_carry_the_session() {
        let args = vec!["plugin".to_string(), "mcp".into(), "aca.delegate".into()];
        let f = codex_mcp_flag("aca-delegate", "/bin/aca", &args, &[("ACA_SESSION_ID".into(), "ab12".into())]);
        let (k, v) = f.split_once('=').unwrap();
        assert_eq!(k, "mcp_servers.aca-delegate");
        let t: toml::Table = toml::from_str(&format!("x = {v}")).unwrap();
        assert_eq!(t["x"]["command"].as_str(), Some("/bin/aca"));
        assert_eq!(t["x"]["args"].as_array().unwrap().len(), 3);
        assert_eq!(t["x"]["env"]["ACA_SESSION_ID"].as_str(), Some("ab12"));
        // The copy doesn't write the session id, only which variables to pass from Codex's environment
        let j: serde_json::Value = serde_json::from_str(&codex_mcp_json("aca-delegate", "/bin/aca", &args)).unwrap();
        let s = &j["mcpServers"]["aca-delegate"];
        assert_eq!((s["command"].as_str(), s["args"][2].as_str()), (Some("/bin/aca"), Some("aca.delegate")));
        assert!(s["env_vars"].as_array().unwrap().iter().any(|v| v == "ACA_SESSION_ID") && s.get("env").is_none());
        let c: serde_json::Value =
            serde_json::from_str(&claude_mcp_config("/bin/aca", &[("aca".into(), vec!["plugin".into(), "tools".into()])])).unwrap();
        assert_eq!(c["mcpServers"]["aca"]["args"][1], "tools");
    }

    #[test]
    fn the_codex_copy_is_rebuilt_only_when_the_plugin_changes_and_the_link_never_clobbers() {
        let tmp = std::env::temp_dir().join(format!("aca-mount-{}", paths::short_id()));
        let src = tmp.join("src");
        write(&src.join(".claude-plugin/plugin.json"), "{\"name\":\"x\"}").unwrap();
        write(&src.join("commands/hi.md"), "---\ndescription: say hi\n---\nhi\n").unwrap();
        write(&src.join(".mcp.json"), "{}").unwrap();
        let dst = tmp.join("dst");
        copy_tree(&src, &dst, &[".git", ".mcp.json"]).unwrap();
        commands_to_skills(&dst).unwrap();
        assert!(dst.join("skills/hi/SKILL.md").is_file() && dst.join("commands/hi.md").is_file() && !dst.join(".mcp.json").exists());
        assert_eq!(fingerprint(&src), fingerprint(&src));
        write(&src.join("commands/more.md"), "x").unwrap();
        assert_ne!(fingerprint(&src), fingerprint(&dst));
        // Something else is already at that spot in CODEX_HOME: left alone
        let home = tmp.join("codex");
        write(&home.join("plugins/cache/aca/keep"), "mine").unwrap();
        assert!(codex_link(&home).is_err());
        assert!(home.join("plugins/cache/aca/keep").is_file());
        // A link elsewhere whose target still exists: also left alone; one whose target is gone: replaced with ours
        let other = tmp.join("codex2");
        let link = other.join("plugins/cache/aca");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let mine = tmp.join("mine");
        std::os::unix::fs::symlink(&src, &link).unwrap();
        assert!(link_to(&other, &mine).is_err());
        assert_eq!(std::fs::read_link(&link).unwrap(), src);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(tmp.join("gone"), &link).unwrap();
        link_to(&other, &mine).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), mine);
        assert!(mine.is_dir());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
