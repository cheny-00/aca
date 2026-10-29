//! Three things for writing plugins (`aca plugin new / check / emit`):
//!
//! - **new**: creates a plugin directory you can link and run right away — a manifest (one command, one key, an event hook
//!   written out but commented) and the script it runs
//! - **check**: checks without linking: does the manifest read and is it right, are the programs / scripts its commands run
//!   there, are the programs in `needs` on this machine, does the directory for agents look right; for a linked one, also
//!   whether another plugin took its keys
//! - **emit**: fakes an event; the hooks subscribed to it run in the **foreground** with their output printed (normally hooks
//!   start from the hook, aren't waited for, and their output goes to the log). Testing a hook needs no real agent turn

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};

use super::{Context, Entry, MANIFEST, Manifest, cond_matches, registry};
use crate::paths::{self, Res};
use crate::{t, tr};

/// The manifest's id: the directory name in lowercase, unknown characters turned into -, prefixed with me.
fn id_from(dir: &Path) -> String {
    let base = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let s: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
    let s = s.trim_matches('-');
    format!("me.{}", if s.is_empty() { "plugin" } else { s })
}

fn manifest_text(id: &str, name: &str) -> String {
    let (zh, en) = (
        format!(
            r#"# aca 插件的清单。写法:aca help plugin,以及 https://github.com/cheny-00/aca/blob/main/docs/plugins.zh-CN.md
# 改了脚本马上生效,改了清单 aca plugin link 一次
id = "{id}"
name = "{name}"
version = "0.1.0"
api = 1                          # 照着插件接口第几版写的(上面那份文档的「接口约定」)
description = "一句话说它干什么"
# needs = ["jq"]                 # 要用到的外面的程序:装的时候、aca plugin check、插件页说缺了哪个

# 一条命令:插件菜单(会话列表上按 :)里有它,命令行 aca plugin run {id}.hello 也能跑
[[commands]]
id = "hello"
title = "打个招呼"
input = "reply"                  # 高亮那个会话的最后一条回复从标准输入进来(还有 screen / scrollback)
command = ["sh", "hello.sh"]
# expose = ["menu", "cli", "agent"]   # 加上 agent:给 agent 当工具用(写好 desc,它才知道什么时候该调)
# desc = "…"

# 插件菜单里按 h 直接跑它
[[keys]]
key = "h"
run = "hello"

# 一轮跑完时跑:不等它,输出进 aca plugin log {id};aca plugin emit turn.done --plugin {id} 在前台试
# [[events]]
# on = "turn.done"
# command = ["sh", "hello.sh"]

# 设置:值写在 plugins.toml(插件页里 ⏎ 进去改),交给命令时在环境里(ACA_SETTING_GREETING)
[settings.greeting]
default = "你好"
desc = "打招呼用的那句"
"#
        ),
        format!(
            r#"# An aca plugin manifest. How to write one: aca help plugin, and https://github.com/cheny-00/aca/blob/main/docs/plugins.md
# Script changes apply at once; after changing this, aca plugin link again
id = "{id}"
name = "{name}"
version = "0.1.0"
api = 1                          # the plugin API version it's written for ("The contract" in the docs above)
description = "One line on what it does"
# needs = ["jq"]                 # outside programs it uses: install, aca plugin check and the plugins page say which are missing

# A command: it's in the plugin menu (press : on the session list), and aca plugin run {id}.hello runs it too
[[commands]]
id = "hello"
title = "Say hello"
input = "reply"                  # the highlighted session's last reply comes in on stdin (also screen / scrollback)
command = ["sh", "hello.sh"]
# expose = ["menu", "cli", "agent"]   # add agent to give it to agents as a tool (write a desc so they know when to call it)
# desc = "…"

# h in the plugin menu runs it directly
[[keys]]
key = "h"
run = "hello"

# Runs when a turn finishes: not waited for, output goes to aca plugin log {id}; try it in the foreground with aca plugin emit turn.done --plugin {id}
# [[events]]
# on = "turn.done"
# command = ["sh", "hello.sh"]

# A setting: its value lives in plugins.toml (⏎ on the plugins page to change it) and reaches the command as ACA_SETTING_GREETING
[settings.greeting]
default = "hello"
desc = "What to greet with"
"#
        ),
    );
    if crate::i18n::lang() == crate::i18n::Lang::Zh { zh } else { en }
}

fn script_text() -> &'static str {
    t!(
        r#"#!/bin/sh
# 环境里有会话、设置、参数(ACA_SESSION_ID、ACA_SETTING_<名字>、ACA_ARG_<名字>…),见插件文档的「交给命令的环境」:
# https://github.com/cheny-00/aca/blob/main/docs/plugins.zh-CN.md
# 界面用哪种话在 ACA_LANG(zh / en)。要 aca 做什么就调 "$ACA_BIN"(aca show "$ACA_SESSION_ID" --json、aca send …)
lines=$(wc -l | tr -d ' ')
echo "${ACA_SETTING_GREETING:-你好}!会话 ${ACA_SESSION_ID:-(没有)} 的最后一条回复有 $lines 行"
"#,
        r#"#!/bin/sh
# The environment carries the session, settings and arguments (ACA_SESSION_ID, ACA_SETTING_<NAME>, ACA_ARG_<NAME>…),
# see "The environment a program gets" in https://github.com/cheny-00/aca/blob/main/docs/plugins.md
# The interface language is in ACA_LANG (zh / en). To have aca do something, call "$ACA_BIN"
lines=$(wc -l | tr -d ' ')
echo "${ACA_SETTING_GREETING:-hello}! the last reply of session ${ACA_SESSION_ID:-(none)} has $lines lines"
"#
    )
}

/// `aca plugin new <dir>`: creates the manifest and script, returns the id. Leaves alone what's already in the directory
pub fn scaffold(dir: &str, id: Option<&str>) -> Res<String> {
    let dir = PathBuf::from(paths::expand(dir));
    if dir.read_dir().is_ok_and(|mut d| d.next().is_some()) {
        return Err(tr!("{} 不是空目录:请换一个空目录", "{} isn't empty: pick an empty directory", dir.display()));
    }
    let id = id.map_or_else(|| id_from(&dir), String::from);
    if !super::manifest::id_ok(&id, true) {
        return Err(tr!("id「{id}」无效(只能用字母、数字、. : _ -)", "id “{id}” is invalid (letters, digits, . : _ -)"));
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}:{e}", dir.display()))?;
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).map_err(|e| format!("{}:{e}", dir.join(file).display()));
    write(MANIFEST, &manifest_text(&id, &name))?;
    write("hello.sh", script_text())?;
    Ok(id)
}

/// What `aca plugin check` found
#[derive(Debug, Default)]
pub struct Report {
    pub dir: PathBuf,
    /// The manifest as read (None when unreadable)
    pub manifest: Option<Manifest>,
    /// What makes it unusable
    pub errors: Vec<String>,
    /// Usable, but better fixed
    pub warns: Vec<String>,
}

/// Commands that call an interpreter to run a script: the script is the next argument
const INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "python", "python3", "node", "deno", "bun", "ruby", "perl"];

/// Whether the program / script a command runs is there: `./x` is looked up in the plugin directory, a bare name in PATH;
/// for `sh x.sh` and the like, also whether the script is there
fn program_problem(dir: &Path, argv: &[String], cwd: &str) -> Option<String> {
    let prog = argv.first()?;
    if prog.starts_with("./") || prog.starts_with("../") {
        let p = dir.join(prog);
        return (!p.is_file()).then(|| tr!("插件目录中没有 {prog}", "{prog} isn't in the plugin directory"));
    }
    if prog.contains('/') {
        return (!Path::new(prog).is_file()).then(|| tr!("{prog} 不存在", "{prog} doesn't exist"));
    }
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| d.join(prog).is_file());
    if !on_path {
        return Some(tr!("PATH 中找不到 {prog}", "{prog} isn't on PATH"));
    }
    // Run in the session's directory, so it's unclear what the script is relative to: not checked
    let script = argv.get(1).filter(|a| INTERPRETERS.contains(&prog.as_str()) && !a.starts_with('-') && cwd != "session")?;
    let p = if Path::new(script).is_absolute() { PathBuf::from(script) } else { dir.join(script) };
    (!p.is_file()).then(|| tr!("插件目录中没有 {script}", "{script} isn't in the plugin directory"))
}

/// Checks one plugin: given a directory (or the manifest's path), or a linked id
pub fn check(target: &str) -> Res<Report> {
    let p = PathBuf::from(paths::expand(target));
    let dir = if p.exists() {
        let d = if p.file_name().is_some_and(|n| n == MANIFEST) { p.parent().unwrap_or(Path::new(".")).to_path_buf() } else { p };
        d.canonicalize().map_err(|e| format!("{}:{e}", d.display()))?
    } else {
        let e = registry().into_iter().find(|e| e.id == target);
        PathBuf::from(
            e.ok_or_else(|| tr!("{target} 既不是目录,也不是已链接的插件", "{target} is neither a directory nor a linked plugin"))?.path,
        )
    };
    let mut r = Report { dir: dir.clone(), ..Default::default() };
    if !dir.join(MANIFEST).is_file() {
        r.errors.push(tr!("{} 中没有 {MANIFEST}", "{} has no {MANIFEST}", dir.display()));
        return Ok(r);
    }
    let (m, warn) = match super::read_manifest(&dir) {
        Ok(x) => x,
        Err(e) => {
            r.errors.push(e);
            return Ok(r);
        }
    };
    r.warns.extend(warn);
    if let Err(e) = super::agent_parts(&dir, &m, &mut r.warns) {
        r.errors.push(e);
    }
    let missing = m.missing();
    if !missing.is_empty() {
        r.warns.push(super::missing_note(&missing));
    }
    let cmds = m
        .actions
        .iter()
        .map(|a| (t!("命令", "command"), a.id.clone(), &a.command, a.cwd.as_str()))
        .chain(m.panes.iter().map(|p| (t!("窗格", "pane"), p.id.clone(), &p.command, p.cwd.as_str())))
        .chain(m.views.iter().map(|v| (t!("视图", "view"), v.id.clone(), &v.command, v.cwd.as_str())))
        .chain(m.events.iter().map(|h| (t!("事件钩子", "event hook"), h.on.clone(), &h.command, "")))
        .chain(m.agent.mcp.iter().map(|x| ("MCP", String::new(), &x.command, "")));
    for (what, id, argv, cwd) in cmds {
        if let Some(why) = program_problem(&dir, argv, cwd) {
            r.errors.push(format!("{what} {id}:{why}").replace(" :", ":"));
        }
    }
    // A linked one: whether another plugin took its keys
    if registry().iter().any(|e| e.id == m.id && Path::new(&e.path) == dir) {
        r.warns.extend(super::key_notes().into_iter().filter(|(p, _)| *p == m.id).map(|(_, n)| n));
    }
    r.manifest = Some(m);
    Ok(r)
}

/// A hook emit ran
#[derive(Debug)]
pub struct Ran {
    pub plugin: String,
    pub command: String,
    /// Exit code; None when killed by a signal
    pub code: Option<i32>,
    /// stdout + stderr
    pub out: String,
}

/// `aca plugin emit <event>`: the hooks of (enabled) plugins subscribed to it whose `if` matches run one by one in the
/// foreground, with their output collected. `only` = only this plugin's; `fields` are the event's own fields (`kind=transient`)
pub fn emit(event: &str, ctx: &Context, fields: serde_json::Map<String, Value>, only: Option<&str>) -> Res<Vec<Ran>> {
    if !crate::event::KNOWN.contains(&event) {
        return Err(tr!("没有事件 {event}:{}", "no event {event}; there are: {}", crate::event::KNOWN.join(" ")));
    }
    let reg = registry();
    if let Some(id) = only {
        match reg.iter().find(|e| e.id == id) {
            None => return Err(tr!("未链接插件 {id}", "{id} isn't linked")),
            Some(e) if !e.enabled => {
                return Err(tr!("{id} 已关闭:先运行 aca plugin enable {id}", "{id} is paused: aca plugin enable {id} first"));
            }
            Some(_) => {}
        }
    }
    let mut line = json!({ "ts": paths::now_ms(), "event": event, "session": ctx.session });
    if let Some(o) = line.as_object_mut() {
        o.extend(fields);
    }
    let line = line.to_string();
    let s = crate::store::Store::open().ok().and_then(|st| st.get(&ctx.session).ok());
    let facts = super::facts(&line, s.as_ref());
    let subs: Vec<&Entry> = reg
        .iter()
        .filter(|e| e.enabled && only.is_none_or(|id| e.id == id) && e.events.iter().any(|on| crate::event::matches(on, event)))
        .collect();
    let mut ran = vec![];
    for e in subs {
        let Ok((m, _)) = super::read_manifest(Path::new(&e.path)) else { continue };
        for h in m.events.iter().filter(|h| crate::event::matches(&h.on, event) && cond_matches(&h.cond, &facts)) {
            let o = super::command(e, &m, &h.command, ctx, &[("ACA_PLUGIN_EVENT", event), ("ACA_PLUGIN_EVENT_JSON", &line)], "")
                .stdin(Stdio::null())
                .output()
                .map_err(|err| tr!("无法启动 {}:{err}", "can't start {}: {err}", h.command[0]))?;
            let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            ran.push(Ran { plugin: e.id.clone(), command: h.command.join(" "), code: o.status.code(), out });
        }
    }
    Ok(ran)
}

/// `name=value`: a number, true / false, null, or a JSON array / object keeps its type; anything else is a string
pub fn field_value(v: &str) -> Value {
    match serde_json::from_str::<Value>(v) {
        Ok(x @ (Value::Number(_) | Value::Bool(_) | Value::Null | Value::Array(_) | Value::Object(_))) => x,
        _ => Value::String(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_plugin_gets_an_id_from_its_directory_and_its_manifest_reads() {
        assert_eq!(id_from(Path::new("/x/My Thing")), "me.my-thing");
        assert_eq!(id_from(Path::new("/x/!!")), "me.plugin");
        for lang in [crate::i18n::Lang::Zh, crate::i18n::Lang::En] {
            crate::i18n::with(lang, || {
                let (m, warn) = super::super::parse(&manifest_text("me.hello", "hello")).unwrap();
                assert!(warn.is_empty(), "{warn:?}");
                assert_eq!((m.id.as_str(), m.actions.len(), m.keys.len(), m.settings.len()), ("me.hello", 1, 1, 1));
            });
        }
    }

    #[test]
    fn a_field_keeps_its_json_type_or_is_text() {
        assert_eq!(field_value("3"), json!(3));
        assert_eq!(field_value("true"), json!(true));
        assert_eq!(field_value("transient"), json!("transient"));
        assert_eq!(field_value("[\"a\"]"), json!(["a"]));
        assert_eq!(field_value("a b"), json!("a b"));
    }

    #[test]
    fn a_missing_program_or_script_is_named() {
        let dir = std::env::temp_dir().join(format!("aca-dev-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.sh"), "").unwrap();
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(program_problem(&dir, &v(&["sh", "ok.sh"]), ""), None);
        assert!(program_problem(&dir, &v(&["sh", "gone.sh"]), "").unwrap().contains("gone.sh"));
        assert_eq!(program_problem(&dir, &v(&["sh", "-c", "echo hi"]), ""), None);
        assert_eq!(program_problem(&dir, &v(&["sh", "x.sh"]), "session"), None);
        assert!(program_problem(&dir, &v(&["./nope"]), "").unwrap().contains("./nope"));
        assert!(program_problem(&dir, &v(&["surely-not-a-program-aca"]), "").unwrap().contains("PATH"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
