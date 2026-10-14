//! 写插件时用的三样(`aca plugin new / check / emit`):
//!
//! - **new**:建一个能直接链接、直接跑的插件目录 —— 清单(一条命令、一个键,事件钩子写好了注释着)和它跑的脚本
//! - **check**:不链接也能查:清单读不读得懂、写得对不对,命令要跑的程序 / 脚本在不在,`needs` 里的程序这台机器上有没有,
//!   给 agent 的目录像不像样;链接过的再看它的键有没有被别的插件拿走
//! - **emit**:假装来了一个事件,订了它的钩子在**前台**跑、输出直接打出来(平时钩子在 hook 里起、不等它、输出进日志)。
//!   测钩子不用真跑一轮 agent

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};

use super::{Context, Entry, MANIFEST, Manifest, cond_matches, registry};
use crate::paths::{self, Res};
use crate::{t, tr};

/// 清单里的 id:目录名小写,不认的字换成 -,前面加 me.
fn id_from(dir: &Path) -> String {
    let base = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let s: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect();
    let s = s.trim_matches('-');
    format!("me.{}", if s.is_empty() { "plugin" } else { s })
}

fn manifest_text(id: &str, name: &str) -> String {
    let (zh, en) = (
        format!(
            r#"# aca 插件的清单(写法:aca help plugin、docs/plugins.zh-CN.md)。改了脚本马上生效,改了清单 aca plugin link 一次
id = "{id}"
name = "{name}"
version = "0.1.0"
api = 1                          # 照着插件接口第几版写的(docs/plugins.zh-CN.md「接口约定」)
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
            r#"# An aca plugin manifest (see aca help plugin, docs/plugins.md). Script changes apply at once; after changing this, aca plugin link again
id = "{id}"
name = "{name}"
version = "0.1.0"
api = 1                          # the plugin API version it's written for (docs/plugins.md, "The contract")
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
# 环境里有会话、设置、参数(ACA_SESSION_ID、ACA_SETTING_<名字>、ACA_ARG_<名字>…,见 docs/plugins.zh-CN.md「交给命令的环境」);
# 界面用哪种话在 ACA_LANG(zh / en)。要 aca 做什么就调 "$ACA_BIN"(aca show "$ACA_SESSION_ID" --json、aca send …)
lines=$(wc -l | tr -d ' ')
echo "${ACA_SETTING_GREETING:-你好}!会话 ${ACA_SESSION_ID:-(没有)} 的最后一条回复有 $lines 行"
"#,
        r#"#!/bin/sh
# The environment carries the session, settings and arguments (ACA_SESSION_ID, ACA_SETTING_<NAME>, ACA_ARG_<NAME>…, see
# docs/plugins.md); the interface language is in ACA_LANG (zh / en). To have aca do something, call "$ACA_BIN"
lines=$(wc -l | tr -d ' ')
echo "${ACA_SETTING_GREETING:-hello}! the last reply of session ${ACA_SESSION_ID:-(none)} has $lines lines"
"#
    )
}

/// `aca plugin new <目录>`:建好清单和脚本,返回 id。目录里已经有东西的不动
pub fn scaffold(dir: &str, id: Option<&str>) -> Res<String> {
    let dir = PathBuf::from(paths::expand(dir));
    if dir.read_dir().is_ok_and(|mut d| d.next().is_some()) {
        return Err(tr!("{} 里已经有东西了:换一个空目录", "{} isn't empty: pick an empty directory", dir.display()));
    }
    let id = id.map_or_else(|| id_from(&dir), String::from);
    if !super::manifest::id_ok(&id, true) {
        return Err(tr!("id「{id}」不行(字母、数字、. : _ -)", "id “{id}” is invalid (letters, digits, . : _ -)"));
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}:{e}", dir.display()))?;
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).map_err(|e| format!("{}:{e}", dir.join(file).display()));
    write(MANIFEST, &manifest_text(&id, &name))?;
    write("hello.sh", script_text())?;
    Ok(id)
}

/// `aca plugin check` 查出来的
#[derive(Debug, Default)]
pub struct Report {
    pub dir: PathBuf,
    /// 读出来的清单(读不懂是 None)
    pub manifest: Option<Manifest>,
    /// 用不了的
    pub errors: Vec<String>,
    /// 能用,但最好改的
    pub warns: Vec<String>,
}

/// 叫解释器跑脚本的:脚本是下一个参数
const INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "python", "python3", "node", "deno", "bun", "ruby", "perl"];

/// 一条命令要跑的程序 / 脚本在不在:`./x` 照插件目录找,光名字在 PATH 里找;`sh x.sh` 这种再看脚本在不在
fn program_problem(dir: &Path, argv: &[String], cwd: &str) -> Option<String> {
    let prog = argv.first()?;
    if prog.starts_with("./") || prog.starts_with("../") {
        let p = dir.join(prog);
        return (!p.is_file()).then(|| tr!("{prog} 在插件目录里没有", "{prog} isn't in the plugin directory"));
    }
    if prog.contains('/') {
        return (!Path::new(prog).is_file()).then(|| tr!("{prog} 不存在", "{prog} doesn't exist"));
    }
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| d.join(prog).is_file());
    if !on_path {
        return Some(tr!("{prog} 不在 PATH 里", "{prog} isn't on PATH"));
    }
    // 在会话的目录里跑的,脚本相对哪儿说不准:不查
    let script = argv.get(1).filter(|a| INTERPRETERS.contains(&prog.as_str()) && !a.starts_with('-') && cwd != "session")?;
    let p = if Path::new(script).is_absolute() { PathBuf::from(script) } else { dir.join(script) };
    (!p.is_file()).then(|| tr!("{script} 在插件目录里没有", "{script} isn't in the plugin directory"))
}

/// 查一个插件:给目录(或者清单的路径),或者链接过的 id
pub fn check(target: &str) -> Res<Report> {
    let p = PathBuf::from(paths::expand(target));
    let dir = if p.exists() {
        let d = if p.file_name().is_some_and(|n| n == MANIFEST) { p.parent().unwrap_or(Path::new(".")).to_path_buf() } else { p };
        d.canonicalize().map_err(|e| format!("{}:{e}", d.display()))?
    } else {
        let e = registry().into_iter().find(|e| e.id == target);
        PathBuf::from(
            e.ok_or_else(|| tr!("{target} 既不是目录,也不是链接过的插件", "{target} is neither a directory nor a linked plugin"))?.path,
        )
    };
    let mut r = Report { dir: dir.clone(), ..Default::default() };
    if !dir.join(MANIFEST).is_file() {
        r.errors.push(tr!("{} 里没有 {MANIFEST}", "{} has no {MANIFEST}", dir.display()));
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
    // 链接过的:它的键有没有被别的插件拿走
    if registry().iter().any(|e| e.id == m.id && Path::new(&e.path) == dir) {
        r.warns.extend(super::key_notes().into_iter().filter(|(p, _)| *p == m.id).map(|(_, n)| n));
    }
    r.manifest = Some(m);
    Ok(r)
}

/// emit 跑了的一个钩子
#[derive(Debug)]
pub struct Ran {
    pub plugin: String,
    pub command: String,
    /// 退出码;被信号打断的是 None
    pub code: Option<i32>,
    /// 标准输出 + 标准错误
    pub out: String,
}

/// `aca plugin emit <事件>`:订了它的(开着的)插件、`if` 对得上的钩子,一个个在前台跑完,输出收下来。
/// `only` = 只跑这个插件的;`fields` 是事件自己的字段(`kind=transient`)
pub fn emit(event: &str, ctx: &Context, fields: serde_json::Map<String, Value>, only: Option<&str>) -> Res<Vec<Ran>> {
    if !crate::event::KNOWN.contains(&event) {
        return Err(tr!("没有 {event} 这个事件:{}", "no event {event}; there are: {}", crate::event::KNOWN.join(" ")));
    }
    let reg = registry();
    if let Some(id) = only {
        match reg.iter().find(|e| e.id == id) {
            None => return Err(tr!("没有链接过 {id}", "{id} isn't linked")),
            Some(e) if !e.enabled => {
                return Err(tr!("{id} 停着:先 aca plugin enable {id}", "{id} is paused: aca plugin enable {id} first"));
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
                .map_err(|err| tr!("{} 起不来:{err}", "can't start {}: {err}", h.command[0]))?;
            let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            ran.push(Ran { plugin: e.id.clone(), command: h.command.join(" "), code: o.status.code(), out });
        }
    }
    Ok(ran)
}

/// `名字=值`:值是数、true / false、null、JSON 数组 / 对象的照它的类型,别的当字
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
