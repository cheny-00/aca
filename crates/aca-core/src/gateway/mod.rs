//! MCP gateway(第一方插件 `aca.gateway`):MCP 的注册中心。可装可不装、可开可关。
//!
//! 开着时,aca 给每个 agent 只挂一个 MCP 服务 `aca`,它是一个很薄的转接(`aca plugin gateway connect`,见 [`connect`]):
//! stdio 进,本机 Unix socket 出,连到 gateway。gateway 是一个进程,下面挂着各个 MCP 服务:
//! - 插件的 MCP(`[agent.mcp]`,`via` 不是 direct 的)
//! - 插件露给 agent 的能力(`aca plugin tools`)
//! - config.toml 里 `[mcps.<名字>]` 写的、在 gateway 的设置 `servers` 里点了名的
//!
//! 每个服务按 `scope` 起:shared(默认)只起一份,所有会话共用 —— 请求编号改写了再转,不会撞;
//! session 每个会话一份(有状态的,比如一个浏览器),这个会话最后一个连接断了就收掉。都是第一次有人要时才起。
//!
//! 谁在调:转接连上先报「我是哪个会话」;gateway 转调用时在请求的 `_meta` 里加 `dev.aca/session`(规范没有标准的调用者字段,
//! 第二段是 mcp / modelcontextprotocol 的前缀是保留的,所以用 dev.aca)。共用的插件 MCP 从这读调用者;每会话一份的
//! 环境里还有 `ACA_SESSION_ID`。每个会话看到的工具按 plugins.toml 的 agents 算(插件够不着这个会话的,它的工具不给)。
//!
//! 不违背「核心没有常驻进程」:第一个转接发现 gateway 没在跑就把它拉起来(加锁,两个同时来只起一个),
//! 没人连着过了设置的分钟数自己退出;插件停了就不再挂,它也就没人连了。socket、pid、日志都在 `state/gateway/`,
//! 任何一个 aca 命令都能查(`aca plugin gateway status`)、能停。

pub mod connect;
mod down;
pub mod import;
mod serve;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::paths::{self, Res};
use crate::plugin::{self, decl, mount};
use crate::{t, tr};

pub use serve::serve;

/// gateway 这个插件的 id
pub const PLUGIN: &str = "aca.gateway";

/// 挂到 agent 上的那个服务的名字(和不开 gateway 时能力的那个服务同名:agent 那头看到的工具名前缀一样,都是 mcp__aca__)
pub const SERVER: &str = "aca";

/// gateway 开没开:装了 `aca.gateway` 这个插件、开着
pub fn enabled() -> bool {
    plugin::registry().iter().any(|e| e.enabled && e.id == PLUGIN)
}

pub fn dir() -> PathBuf {
    paths::state_dir().join("gateway")
}

/// socket 的路径。Unix socket 的路径最长一百来个字节:状态目录太深(测试、换了 ACA_HOME)时改放 /tmp,名字按状态目录算
pub fn sock_path() -> PathBuf {
    let p = dir().join("gw.sock");
    if p.as_os_str().len() < 100 {
        return p;
    }
    let mut h = DefaultHasher::new();
    p.hash(&mut h);
    PathBuf::from(format!("/tmp/aca-gw-{:012x}.sock", h.finish() & 0xffff_ffff_ffff))
}

fn lock_path() -> PathBuf {
    dir().join("gw.lock")
}

fn pid_path() -> PathBuf {
    dir().join("gw.pid")
}

pub fn log_dir() -> PathBuf {
    dir().join("logs")
}

/// 一个服务的日志(它的 stderr、它发来的日志通知)
pub fn log_of(server: &str) -> PathBuf {
    log_dir().join(format!("{}.log", mount::safe(server)))
}

fn calls_log() -> PathBuf {
    dir().join("calls.jsonl")
}

/// 一个下游 MCP 服务怎么起
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Spec {
    /// 名字:工具名的前缀(`<名字>__<工具>`)
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// 每个会话一份
    pub per_session: bool,
    /// 哪个插件的(按 plugins.toml 的 agents 决定给谁);None = 谁都给
    pub plugin: Option<String>,
    /// aca 自己的能力服务:工具名本来就带着插件名,不再加前缀,按工具名里的插件决定给谁
    pub tools: bool,
}

/// gateway 的设置(plugins.toml 里 `[plugins."aca.gateway"] settings`,清单里有默认值)
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// 要经 gateway 给 agent 的 `[mcps.<名字>]`
    pub servers: Vec<String>,
    /// 没人连着多久退出
    pub idle: Duration,
}

pub fn settings() -> Settings {
    let manifest = plugin::registry().into_iter().find(|e| e.id == PLUGIN).and_then(|e| plugin::read_manifest(Path::new(&e.path)).ok());
    let all = match manifest {
        Some((m, _)) => decl::settings(PLUGIN, &m),
        None => decl::want(PLUGIN).settings,
    };
    let servers = all
        .get("servers")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mins = all.get("idle_minutes").and_then(|v| v.as_integer()).unwrap_or(10).max(1) as u64;
    Settings { servers, idle: Duration::from_secs(mins * 60) }
}

/// 现在 gateway 下面该挂哪些服务(每次有人来列工具时重算,改了 config.toml / plugins.toml 不用重启 gateway)
pub fn catalog() -> Vec<Spec> {
    let aca = crate::agent::self_exe();
    let mut out = vec![];
    let reg = plugin::registry();
    for e in reg.iter().filter(|e| e.enabled && e.id != PLUGIN) {
        if let Some(m) = e.mcp.as_ref().filter(|m| m.via != "direct") {
            out.push(Spec {
                name: mount::safe(&e.id),
                command: aca.clone(),
                args: vec!["plugin".into(), "mcp".into(), e.id.clone()],
                env: vec![],
                per_session: m.scope == "session",
                plugin: Some(e.id.clone()),
                tools: false,
            });
        }
    }
    let wants = decl::wants().unwrap_or_default();
    if reg.iter().any(|e| e.enabled && !plugin::yours::agent_tools(e, wants.get(&e.id)).is_empty()) {
        out.push(Spec {
            name: "tools".into(),
            command: aca.clone(),
            args: vec!["plugin".into(), "tools".into()],
            env: vec![],
            per_session: false,
            plugin: None,
            tools: true,
        });
    }
    let cfg: toml::Table =
        std::fs::read_to_string(paths::config_dir().join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    for name in settings().servers {
        let Some(def) = cfg.get("mcps").and_then(|m| m.get(&name)).and_then(|d| d.as_table()) else { continue };
        let Some(command) = def.get("command").and_then(|c| c.as_str()) else { continue };
        let strs = |k: &str| {
            def.get(k)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        let env = def
            .get("env")
            .and_then(|v| v.as_table())
            .map(|t| t.iter().filter_map(|(k, v)| Some((k.clone(), paths::expand(v.as_str()?)))).collect())
            .unwrap_or_default();
        out.push(Spec {
            name: mount::safe(&name),
            command: paths::expand(command),
            args: strs("args"),
            env,
            per_session: def.get("scope").and_then(|s| s.as_str()) == Some("session"),
            plugin: None,
            tools: false,
        });
    }
    out
}

/// gateway 有没有东西给这个会话:有,才给它挂转接(默认什么都不进 agent)
pub fn offers(kind: &str, tool: &str, account: &str) -> bool {
    let wants = decl::wants().unwrap_or_default();
    let reaches = |id: &str| wants.get(id).is_none_or(|w| w.reaches(tool, kind, account));
    catalog().iter().any(|s| match (&s.plugin, s.tools) {
        (Some(id), _) => reaches(id),
        (None, true) => {
            plugin::registry().iter().any(|e| e.enabled && !plugin::yours::agent_tools(e, wants.get(&e.id)).is_empty() && reaches(&e.id))
        }
        (None, false) => true,
    })
}

fn off_file(session: &str) -> PathBuf {
    dir().join("off").join(format!("{session}.json"))
}

/// 这个会话在 M 页里关掉的服务(名字)
pub fn off_for(session: &str) -> Vec<String> {
    if session.is_empty() {
        return vec![];
    }
    std::fs::read_to_string(off_file(session)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// 改这个会话关掉的服务;在跑的 gateway 通知连着的 agent 重新拉工具列表(Claude 当场换,Codex 重启才换)
pub fn set_off(session: &str, names: &[String]) -> Res<()> {
    let f = off_file(session);
    if names.is_empty() {
        let _ = std::fs::remove_file(&f);
    } else {
        std::fs::create_dir_all(f.parent().unwrap()).map_err(|e| e.to_string())?;
        paths::write_atomic(&f, &serde_json::to_string(names).unwrap()).map_err(|e| e.to_string())?;
    }
    let _ = ctl(json!({ "cmd": "changed" }));
    Ok(())
}

/// 这个会话从 gateway 能看到的服务(按插件给谁筛过,M 页里关掉的也在):(服务, 关没关)
pub fn seen_by(session: &str) -> Vec<(Spec, bool)> {
    let (tool, kind, account) = serve::facts(session);
    let wants = decl::wants().unwrap_or_default();
    let reaches = |id: &str| wants.get(id).is_none_or(|w| w.reaches(&tool, &kind, &account));
    let off = off_for(session);
    catalog()
        .into_iter()
        .filter(|s| match (&s.plugin, s.tools) {
            (Some(id), _) => reaches(id),
            (None, true) => plugin::registry()
                .iter()
                .any(|e| e.enabled && !plugin::yours::agent_tools(e, wants.get(&e.id)).is_empty() && reaches(&e.id)),
            (None, false) => true,
        })
        .map(|s| {
            let o = off.contains(&s.name);
            (s, o)
        })
        .collect()
}

/// 每个服务最近一天被调了几次、哪几个会话在用(从调用记录里数)
pub fn usage() -> std::collections::HashMap<String, (usize, Vec<String>)> {
    let since = paths::now().saturating_sub(86_400);
    let mut out: std::collections::HashMap<String, (usize, Vec<String>)> = Default::default();
    for l in std::fs::read_to_string(calls_log()).unwrap_or_default().lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        if v["ts"].as_u64().unwrap_or(0) < since {
            continue;
        }
        let e = out.entry(v["server"].as_str().unwrap_or("").to_string()).or_default();
        e.0 += 1;
        let who = v["session"].as_str().unwrap_or("").to_string();
        if !who.is_empty() && !e.1.contains(&who) {
            e.1.push(who);
        }
    }
    out
}

/// 把 `[mcps.<名字>]` 收进 / 移出 gateway(改 plugins.toml 里 gateway 的设置 servers)。下次有人来列工具时就是新的
pub fn set_served(name: &str, on: bool) -> Res<()> {
    if on && !mcps_names().iter().any(|n| n == name) {
        return Err(tr!("config.toml 里没有 [mcps.{name}]", "no [mcps.{name}] in config.toml"));
    }
    let mut servers = settings().servers;
    servers.retain(|s| s != name);
    if on {
        servers.push(name.to_string());
    }
    let mut table = decl::want(PLUGIN).settings;
    table.insert("servers".into(), toml::Value::Array(servers.into_iter().map(toml::Value::String).collect()));
    decl::set(PLUGIN, "settings", Some(toml::Value::Table(table)))?;
    let _ = ctl(json!({ "cmd": "changed" }));
    Ok(())
}

/// config.toml 里 `[mcps.*]` 的名字
pub fn mcps_names() -> Vec<String> {
    let cfg: toml::Table =
        std::fs::read_to_string(paths::config_dir().join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    cfg.get("mcps").and_then(|m| m.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default()
}

/// 发一条控制命令给在跑的 gateway,读它回的一行
pub fn ctl(cmd: Value) -> Res<Value> {
    ctl_within(cmd, Duration::from_secs(10))
}

/// 同 [`ctl`],最多等 `wait`(状态栏这种每隔几秒问一次的,不能被它拖住)
pub fn ctl_within(cmd: Value, wait: Duration) -> Res<Value> {
    let s = UnixStream::connect(sock_path()).map_err(|_| t!("gateway 没在跑", "the gateway isn't running").to_string())?;
    s.set_read_timeout(Some(wait)).ok();
    s.set_write_timeout(Some(wait)).ok();
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let mut msg = json!({ "aca": "ctl" });
    if let (Some(o), Value::Object(c)) = (msg.as_object_mut(), cmd) {
        o.extend(c);
    }
    writeln!(w, "{msg}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|_| t!("gateway 回的看不懂", "can't read the gateway's answer").to_string())
}

/// `aca plugin gateway status`:在不在跑、谁连着、下面每个服务的样子、最近几次调用
pub fn status_text() -> String {
    let mut out = vec![];
    if !enabled() {
        out.push(
            t!(
                "gateway 没开(aca plugin enable aca.gateway;没装:aca plugin install <aca 仓库>/plugins/gateway)",
                "the gateway is off (aca plugin enable aca.gateway; not installed: aca plugin install <aca repo>/plugins/gateway)"
            )
            .to_string(),
        );
    }
    match ctl(json!({ "cmd": "status" })) {
        Err(e) => out.push(tr!("{e}(第一个 agent 连上来时会自己起)", "{e} (it starts when the first agent connects)")),
        Ok(v) => {
            out.push(tr!(
                "在跑:pid {},起来 {} 秒,{} 个连接",
                "running: pid {}, up {} s, {} connections",
                v["pid"],
                v["up"],
                v["clients"].as_array().map_or(0, Vec::len)
            ));
            for c in v["clients"].as_array().into_iter().flatten() {
                let s = c["session"].as_str().filter(|s| !s.is_empty()).unwrap_or("-");
                out.push(tr!("  连着:会话 {s},{} 秒", "  connected: session {s}, {} s", c["secs"]));
            }
            for s in v["servers"].as_array().into_iter().flatten() {
                let who = s["session"].as_str().filter(|x| !x.is_empty()).map(|x| tr!("(会话 {x})", " (session {x})")).unwrap_or_default();
                let err = s["error"].as_str().filter(|x| !x.is_empty()).map(|x| format!(" · {x}")).unwrap_or_default();
                out.push(tr!(
                    "  服务 {}{who}:{} · {} 个工具{err}",
                    "  server {}{who}: {} · {} tools{err}",
                    s["name"].as_str().unwrap_or(""),
                    s["state"].as_str().unwrap_or(""),
                    s["tools"]
                ));
            }
        }
    }
    let calls = std::fs::read_to_string(calls_log()).unwrap_or_default();
    let recent: Vec<&str> = calls.lines().rev().take(5).collect();
    if !recent.is_empty() {
        out.push(t!("最近的调用:", "recent calls:").to_string());
        for l in recent.into_iter().rev() {
            let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
            let ok = if v["ok"].as_bool() == Some(true) { "✓" } else { "✕" };
            out.push(format!(
                "  {ok} {} {} {} ms({})",
                v["server"].as_str().unwrap_or(""),
                v["tool"].as_str().unwrap_or(""),
                v["ms"],
                v["session"].as_str().unwrap_or("-")
            ));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deep_state_directory_gets_a_short_socket_path() {
        let p = sock_path();
        assert!(p.as_os_str().len() < 104, "{}", p.display());
    }
}
