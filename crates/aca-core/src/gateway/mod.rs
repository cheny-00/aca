//! MCP gateway (the first-party plugin `aca.gateway`): a registry for MCP. Optional to install, and can be switched on and off.
//!
//! When on, aca mounts only one MCP server, `aca`, on each agent; it's a very thin relay (`aca plugin gateway connect`, see
//! [`connect`]): stdio in, a local Unix socket out, connected to the gateway. The gateway is one process with the MCP servers
//! under it:
//! - plugins' MCP servers (`[agent.mcp]` whose `via` isn't direct)
//! - commands plugins expose to agents (`aca plugin tools`)
//! - `[mcps.<name>]` in config.toml that are named in the gateway's `servers` setting
//!
//! Each server starts according to its `scope`: shared (the default) starts once, shared by all sessions — request ids are
//! rewritten before forwarding, so they don't collide; session gives each session its own (stateful ones, such as a browser),
//! cleaned up when that session's last connection closes. All start only when first asked for.
//!
//! Who's calling: the relay says "which session I am" first when it connects; when forwarding a call, the gateway adds
//! `dev.aca/session` to the request's `_meta` (the spec has no standard caller field, and prefixes whose second label is mcp /
//! modelcontextprotocol are reserved, hence dev.aca). Shared plugin MCP servers read the caller from there; per-session ones also
//! have `ACA_SESSION_ID` in their environment. The tools each session sees follow the agents setting in plugins.toml (a plugin
//! that can't reach this session doesn't get its tools offered).
//!
//! This doesn't break "the core has no resident process": the first relay that finds the gateway not running starts it (under a
//! lock, so two arriving at once start only one), and it exits by itself after the set number of minutes with nobody connected;
//! once a plugin is stopped it's no longer mounted, so nobody connects to it. The socket, pid and logs are all in
//! `state/gateway/`, and any aca command can check it (`aca plugin gateway status`) and stop it.

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

/// The gateway plugin's id
pub const PLUGIN: &str = "aca.gateway";

/// The name of the server mounted on agents (the same name as the plugin-commands server without the gateway: the tool name prefix
/// the agent sees is the same, mcp__aca__)
pub const SERVER: &str = "aca";

/// Whether the gateway is on: the `aca.gateway` plugin is installed and enabled
pub fn enabled() -> bool {
    plugin::registry().iter().any(|e| e.enabled && e.id == PLUGIN)
}

pub fn dir() -> PathBuf {
    paths::state_dir().join("gateway")
}

/// The socket's path. A Unix socket path is at most a hundred-odd bytes: when the state directory is too deep (tests, a different
/// ACA_HOME), put it in /tmp instead, named after the state directory
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

/// A server's log (its stderr, and the log notifications it sends)
pub fn log_of(server: &str) -> PathBuf {
    log_dir().join(format!("{}.log", mount::safe(server)))
}

fn calls_log() -> PathBuf {
    dir().join("calls.jsonl")
}

/// How to start a downstream MCP server
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Spec {
    /// Name: the prefix of the tool names (`<name>__<tool>`)
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// One per session
    pub per_session: bool,
    /// Which plugin it belongs to (the agents setting in plugins.toml decides who gets it); None = everyone
    pub plugin: Option<String>,
    /// aca's own plugin-commands server: the tool names already carry the plugin name, so no prefix is added, and the plugin in the tool
    /// name decides who gets it
    pub tools: bool,
}

/// The gateway's settings (`[plugins."aca.gateway"] settings` in plugins.toml; the manifest has the defaults)
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The `[mcps.<name>]` to offer agents through the gateway
    pub servers: Vec<String>,
    /// How long with nobody connected before exiting
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

/// Which servers should be under the gateway now (recomputed every time someone lists tools, so changes to config.toml /
/// plugins.toml need no gateway restart)
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

/// Whether the gateway has anything for this session: only then is the relay mounted on it (by default nothing goes into the agent)
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

/// Servers switched off for this session on the M page (names)
pub fn off_for(session: &str) -> Vec<String> {
    if session.is_empty() {
        return vec![];
    }
    std::fs::read_to_string(off_file(session)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Change the servers switched off for this session; a running gateway tells connected agents to fetch the tool list again (Claude
/// switches on the spot, Codex after a restart)
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

/// Servers this session can see through the gateway (filtered by who each plugin is for; ones switched off on the M page are
/// included): (server, switched off)
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

/// How many times each server was called in the last day, and which sessions use it (counted from the call log)
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

/// Put `[mcps.<name>]` into / take it out of the gateway (changes the gateway's servers setting in plugins.toml). The next one to
/// list tools gets the new set
pub fn set_served(name: &str, on: bool) -> Res<()> {
    if on && !mcps_names().iter().any(|n| n == name) {
        return Err(tr!("config.toml 中没有 [mcps.{name}]", "no [mcps.{name}] in config.toml"));
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

/// Names of `[mcps.*]` in config.toml
pub fn mcps_names() -> Vec<String> {
    let cfg: toml::Table =
        std::fs::read_to_string(paths::config_dir().join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    cfg.get("mcps").and_then(|m| m.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default()
}

/// Send a control command to the running gateway and read the line it replies
pub fn ctl(cmd: Value) -> Res<Value> {
    ctl_within(cmd, Duration::from_secs(10))
}

/// Same as [`ctl`], waiting at most `wait` (things like the status bar asking every few seconds mustn't get held up by it)
pub fn ctl_within(cmd: Value, wait: Duration) -> Res<Value> {
    let s = UnixStream::connect(sock_path()).map_err(|_| t!("gateway 未运行", "the gateway isn't running").to_string())?;
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
    serde_json::from_str(&line).map_err(|_| t!("无法解析 gateway 的响应", "can't read the gateway's answer").to_string())
}

/// `aca plugin gateway status`: whether it's running, who's connected, how each server under it is doing, the last few calls
pub fn status_text() -> String {
    let mut out = vec![];
    if !enabled() {
        out.push(
            t!(
                "gateway 未开启(aca plugin enable aca.gateway;未安装:aca plugin install <aca 仓库>/plugins/gateway)",
                "the gateway is off (aca plugin enable aca.gateway; not installed: aca plugin install <aca repo>/plugins/gateway)"
            )
            .to_string(),
        );
    }
    match ctl(json!({ "cmd": "status" })) {
        Err(e) => out.push(tr!("{e}(第一个 agent 连接时会自动启动)", "{e} (it starts when the first agent connects)")),
        Ok(v) => {
            out.push(tr!(
                "运行中:pid {},已运行 {} 秒,{} 个连接",
                "running: pid {}, up {} s, {} connections",
                v["pid"],
                v["up"],
                v["clients"].as_array().map_or(0, Vec::len)
            ));
            for c in v["clients"].as_array().into_iter().flatten() {
                let s = c["session"].as_str().filter(|s| !s.is_empty()).unwrap_or("-");
                out.push(tr!("  已连接:会话 {s},{} 秒", "  connected: session {s}, {} s", c["secs"]));
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
        out.push(t!("最近调用:", "recent calls:").to_string());
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
