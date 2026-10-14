//! 挂载:起 agent 时把插件给它的东西带进去。只对这一次启动有效,不改账号的配置文件、不往你的仓库里写。
//!
//! - **Claude**:插件目录用 `--plugin-dir`(能给好几个);MCP 服务写进一份 `--mcp-config`(见 agent/claude.rs)。
//!   清单的 `[agent] auto` 指的那个开关设置开着时,给的是 `claude-plugins/<名字>/local/` 里的副本:插件目录照抄,
//!   命令、skill 头上的 `disable-model-invocation` 去掉(agent 自己能用了);关着、没写 auto 的直接给插件目录
//! - **Codex**:它只认装进 `CODEX_HOME` 插件缓存里的插件,没有按启动指定目录的参数(0.162 源码:`-c` 那一层不出 skill 根目录)。
//!   所以 aca 在自己的数据目录里放一份它认得的副本 `codex-plugins/<名字>/local/`:插件目录照抄,斜杠命令转成 skill
//!   (Codex 没有自定义斜杠命令),`.mcp.json` 不放(MCP 走 `-c`);每个账号的 `CODEX_HOME/plugins/cache/aca` 链到
//!   `codex-plugins`(只建一次,不改 config.toml),起的时候 `-c plugins.<名字>@aca.enabled=true` 只对这一次打开。
//!   这靠的是 Codex 内部的缓存目录布局,不是公开的接口:那里已经有别的东西、建不了链接时只挂 MCP,原因记在
//!   `plugin-logs/mount.log`。MCP 是 `-c mcp_servers.<名字>={…}`;它给 MCP 子进程的环境只有白名单,会话 id 得写在 `env` 里
//! - 两家的插件 MCP 都是 `aca plugin mcp <插件 id>`:aca 切到插件目录、带上插件的环境(设置、会话)再换成它的命令
//! - 露给 agent 的能力(`expose = ["agent"]`)由 aca 自带的 `aca plugin tools` 包成工具,这个 MCP 服务叫 `aca`
//! - gateway 开着([`crate::gateway`]):插件的 MCP(via 不是 direct 的)和能力都归 gateway,agent 身上只挂一个 `aca`,
//!   是到 gateway 的转接(`aca plugin gateway connect`);via = direct 的照旧直接挂
//!
//! 默认什么都不带:没有开着的插件给这个会话东西,命令行一个参数都不多。

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use super::{Entry, Manifest, decl, read_manifest, registry};
use crate::paths::{self, Res};
use crate::tr;

/// 起一个会话时要带的
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mounts {
    /// 插件目录(Claude:`--plugin-dir`)
    pub dirs: Vec<String>,
    /// 直接挂的 MCP 服务:(名字, 交给 aca 的参数)。命令都是 aca 自己
    pub mcp: Vec<(String, Vec<String>)>,
    /// Codex:要打开的插件(`-c plugins.<名字>@aca.enabled=true`)
    pub codex_plugins: Vec<String>,
}

/// 插件 id → Codex 插件名、MCP 服务名里能用的样子:字母数字 `_` `-`,别的换成 `-`(`aca.delegate` → `aca-delegate`)
pub fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect()
}

/// aca 自带的那个把能力包成工具的 MCP 服务的名字
pub const TOOLS_SERVER: &str = "aca";

/// 这一家(`kind`)、这个会话(工具名、账号)要带什么。`codex_home` 给了(Codex)就顺手把副本、链接备好
pub fn plan(kind: &str, tool: &str, account: &str, codex_home: Option<&Path>) -> Mounts {
    // 刚升级了 aca:自带的插件先换成新的,再算挂什么
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
                // gateway 开着:不是非得直接挂的,都归 gateway
                (via, true) if via != "direct" => {}
                ("gateway", false) => note(&tr!(
                    "{}:它的 MCP 只挂到 gateway 上,gateway 没开,这回不挂",
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
    out
}

/// 这一家、这个会话现在该带什么,写成一行(不碰 Codex 的副本、链接):起的时候记下来,和后来的一比,
/// 不一样就是开着的会话要重启才带上。只算给 agent 的(目录、它的命令 agent 自己能不能用、MCP、工具、gateway 开没开),
/// 只有事件钩子的插件不算
pub fn signature(kind: &str, tool: &str, account: &str) -> String {
    if !super::AGENTS.contains(&kind) {
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

/// 开着的会话里,插件给的和它起来时带的不一样的(要重启才带上):(id, 标题)
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

/// 挂不上的原因记一行(起 agent 的时候没处说)
fn note(msg: &str) {
    let log = super::log_file("mount");
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        use std::io::Write;
        let _ = writeln!(f, "{} {msg}", paths::now());
    }
}

/// Codex 的副本都放在这:`<数据目录>/codex-plugins`
pub fn codex_root() -> PathBuf {
    paths::data_dir().join("codex-plugins")
}

/// `CODEX_HOME/plugins/cache/aca` 链到 [`codex_root`]。已经链好了不动;那里有别的东西(别的目录、链到别处的)不碰,Err
pub fn codex_link(home: &Path) -> Res<()> {
    let cache = home.join("plugins").join("cache");
    let link = cache.join("aca");
    let target = codex_root();
    if link.symlink_metadata().is_ok() {
        return match std::fs::read_link(&link) {
            Ok(t) if t == target => Ok(()),
            _ => Err(tr!(
                "{} 已经有别的东西,不动它:Codex 这回只挂 MCP",
                "{} already holds something else; left alone: Codex only gets MCP this time",
                link.display()
            )),
        };
    }
    std::fs::create_dir_all(&cache).map_err(|e| tr!("建不了 {}:{e}", "can't create {}: {e}", cache.display()))?;
    std::fs::create_dir_all(&target).map_err(|e| tr!("建不了 {}:{e}", "can't create {}: {e}", target.display()))?;
    std::os::unix::fs::symlink(&target, &link).map_err(|e| tr!("链不上 {}:{e}", "can't link {}: {e}", link.display()))
}

/// 一个目录里的文件变没变:相对路径、大小、修改时间合起来的指纹
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

/// 一份副本 `<root>/local/`:来源没变(`fp` 和上次记的一样)就不动;变了另起一份再换上,正在起的别的会话不会读到半份
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
    // 旧的先挪开再换上新的(rename 不能盖掉非空目录)
    let old = root.join(format!(".old-{}", paths::short_id()));
    let had = local.exists() && std::fs::rename(&local, &old).is_ok();
    std::fs::rename(&tmp, &local).map_err(|err| tr!("放不上 {}:{err}", "can't put {} in place: {err}", local.display()))?;
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(local)
}

/// 给 Codex 的副本:`codex-plugins/<名字>/local/`。插件目录没变就不动。返回插件名
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

/// Claude 的副本都放在这:`<数据目录>/claude-plugins`
pub fn claude_root() -> PathBuf {
    paths::data_dir().join("claude-plugins")
}

/// 清单的 `[agent] auto` 指的那个开关设置现在开没开(没写 auto = 没开)
fn auto_on(e: &Entry, m: &Manifest) -> bool {
    !m.agent.auto.is_empty() && decl::settings(&e.id, m).get(&m.agent.auto).and_then(toml::Value::as_bool).unwrap_or(false)
}

/// Claude 这回拿哪个目录:平时就是插件目录;`[agent] auto` 的设置开着时是去掉了 `disable-model-invocation` 的副本。
/// 副本做不出来照旧给插件目录(原因记在 mount.log)
fn claude_dir(e: &Entry, dir: &str) -> String {
    match read_manifest(Path::new(&e.path)) {
        Ok((m, _)) if auto_on(e, &m) => claude_copy(e, Path::new(dir), &m).unwrap_or_else(|err| {
            note(&format!("{}:{err}", e.id));
            dir.to_string()
        }),
        _ => dir.to_string(),
    }
}

/// 给 Claude 的副本:`claude-plugins/<名字>/local/`,插件目录照抄,命令、skill 头上的 `disable-model-invocation` 去掉。
/// 插件目录没变就不动。返回副本的路径
pub fn claude_copy(e: &Entry, src: &Path, m: &Manifest) -> Res<String> {
    let fp = format!("{} {}", fingerprint(src), m.version);
    let local = rebuilt(&claude_root().join(safe(&e.id)), &fp, |tmp| {
        copy_tree(src, tmp, &[".git"])?;
        ["commands", "skills"].iter().try_for_each(|d| lift_all(&tmp.join(d)))
    })?;
    Ok(local.to_string_lossy().into_owned())
}

/// 一个目录底下的 `.md`(命令、SKILL.md)都过一遍 [`lift`];没有这个目录不算错
fn lift_all(dir: &Path) -> Res<()> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            lift_all(&p)?;
        } else if p.extension().is_some_and(|x| x == "md") {
            let text = std::fs::read_to_string(&p).map_err(|e| tr!("读不了 {}:{e}", "can't read {}: {e}", p.display()))?;
            let lifted = lift(&text);
            if lifted != text {
                write(&p, &lifted)?;
            }
        }
    }
    Ok(())
}

/// 文件头(两行 `---` 之间)里的 `disable-model-invocation` 那一行去掉;没有头、没有这一行的原样
pub(crate) fn lift(text: &str) -> String {
    let Some((front, body)) = text.strip_prefix("---\n").and_then(|r| r.split_once("\n---\n")) else { return text.to_string() };
    let kept: Vec<&str> = front.lines().filter(|l| l.split_once(':').is_none_or(|(k, _)| k.trim() != "disable-model-invocation")).collect();
    format!("---\n{}\n---\n{body}", kept.join("\n"))
}

fn write(p: &Path, text: &str) -> Res<()> {
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(p, text).map_err(|e| tr!("写不了 {}:{e}", "can't write {}: {e}", p.display()))
}

/// 照抄一个目录(链接跟过去抄成真的文件:Codex 不认链接的清单),`skip` 里的名字不抄
fn copy_tree(src: &Path, dst: &Path, skip: &[&str]) -> Res<()> {
    std::fs::create_dir_all(dst).map_err(|e| tr!("建不了 {}:{e}", "can't create {}: {e}", dst.display()))?;
    let rd = std::fs::read_dir(src).map_err(|e| tr!("读不了 {}:{e}", "can't read {}: {e}", src.display()))?;
    for ent in rd.flatten() {
        let name = ent.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let (from, to) = (ent.path(), dst.join(&name));
        match std::fs::metadata(&from) {
            Ok(md) if md.is_dir() => copy_tree(&from, &to, skip)?,
            Ok(_) => {
                std::fs::copy(&from, &to).map_err(|e| tr!("抄不了 {}:{e}", "can't copy {}: {e}", from.display()))?;
            }
            Err(_) => {}
        }
    }
    Ok(())
}

/// Claude 的斜杠命令(`commands/<名字>.md`)变成 Codex 的 skill(`skills/<名字>/SKILL.md`):
/// 头上的 description 照搬,`$ARGUMENTS` 换成一句说明。同名的 skill 已经有了就不转
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

/// Codex 那边一个 MCP 服务的 `-c` 值:`mcp_servers.<名字>={ command = …, args = […], env = {…} }`
pub fn codex_mcp_flag(name: &str, aca: &str, args: &[String], env: &[(String, String)]) -> String {
    let mut t = toml::Table::new();
    t.insert("command".into(), aca.into());
    t.insert("args".into(), toml::Value::Array(args.iter().map(|a| a.clone().into()).collect()));
    let vars: toml::Table = env.iter().map(|(k, v)| (k.clone(), v.clone().into())).collect();
    t.insert("env".into(), toml::Value::Table(vars));
    format!("mcp_servers.{name}={}", toml::Value::Table(t))
}

/// Claude 那边的 `--mcp-config` 内容
pub fn claude_mcp_config(aca: &str, mcp: &[(String, Vec<String>)]) -> String {
    let servers: serde_json::Map<String, serde_json::Value> =
        mcp.iter().map(|(name, args)| (name.clone(), serde_json::json!({ "type": "stdio", "command": aca, "args": args }))).collect();
    serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": servers })).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 没写头的:描述就是它的命令名
        assert!(command_as_skill("x", "hello").contains("description: /x\n"));
    }

    #[test]
    fn lifting_takes_only_the_no_model_invocation_line_out_of_the_head() {
        let cmd = "---\ndescription: 派活给 worker\ndisable-model-invocation: true\nargument-hint: [task]\n---\n\ndisable-model-invocation: 正文里的不动\n";
        assert_eq!(lift(cmd), "---\ndescription: 派活给 worker\nargument-hint: [task]\n---\n\ndisable-model-invocation: 正文里的不动\n");
        // 没有头、头里没有这一行:原样
        for same in ["hello\n", "---\ndescription: x\n---\nhi\n"] {
            assert_eq!(lift(same), same);
        }
        // 副本里命令、skill 都过一遍,别的文件(.mcp.json 也在)照抄;插件目录自己不动
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
        // CODEX_HOME 里那个位置已经有别的东西:不碰
        let home = tmp.join("codex");
        write(&home.join("plugins/cache/aca/keep"), "mine").unwrap();
        assert!(codex_link(&home).is_err());
        assert!(home.join("plugins/cache/aca/keep").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
