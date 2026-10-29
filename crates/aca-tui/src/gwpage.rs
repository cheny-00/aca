//! The gateway page (`m` on the gateway row of the `P` plugins page): managing the gateway is managing MCPs. One row per
//! server under it — MCPs that plugins bring, the server for plugin commands, `[mcps.*]` from config.toml that are served;
//! `[mcps.*]` not served yet are listed too (dimmed).
//! Each row: running / not started / crashed, how many tools, calls in the last day, how many sessions use it.
//!
//! - Space: serve / unserve an `[mcps.*]` through the gateway (edits the gateway's `servers` setting in plugins.toml;
//!   connected agents re-fetch the tool list)
//! - `r` restarts this one, `R` restarts all (started again when next needed); `l` shows its log; `s` stops the gateway
//!   (started again when the next agent connects)
//! - `o` turns the gateway on / off (i.e. enables / disables the `aca.gateway` plugin): while off, newly started agents
//!   don't go through it and mount MCPs directly
//!
//! These are all small local actions (control socket, editing one file), done on the spot and reloaded, not in the background.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

use aca_core::gateway;
use aca_core::{t, tr};

use crate::ops::Job;
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::plugins::Act;
use crate::theme;

const SHOWN: usize = 12;

/// One server
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Srv {
    pub name: String,
    /// What it is: from a plugin / plugin tools for agents / the `[mcps.*]` description
    pub what: String,
    /// From `[mcps.*]`: can be served / unserved
    pub catalog: bool,
    /// Under the gateway
    pub served: bool,
    pub per_session: bool,
    /// ready / idle / failed; empty when the gateway isn't running
    pub state: String,
    pub tools: u64,
    pub error: String,
    /// Calls in the last day, how many sessions use it
    pub calls: usize,
    pub users: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    /// On (`aca.gateway` installed and enabled): newly started agents mount MCPs through it
    pub on: bool,
    /// Running: (how many connected, seconds up); None = not running
    pub running: Option<(usize, u64)>,
    pub rows: Vec<Srv>,
}

/// Read: the catalog (what should be mounted), the status the running one reports, call records, config.toml's `[mcps.*]`
pub fn load() -> Info {
    let status = gateway::ctl(json!({ "cmd": "status" })).ok();
    let usage = gateway::usage();
    let cfg: toml::Table =
        std::fs::read_to_string(aca_core::paths::config_dir().join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let mcps: Vec<(String, String, bool)> = cfg
        .get("mcps")
        .and_then(|m| m.as_table())
        .into_iter()
        .flatten()
        .map(|(n, d)| {
            let desc = d.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string();
            (n.clone(), desc, d.get("scope").and_then(|s| s.as_str()) == Some("session"))
        })
        .collect();
    let info = build(gateway::catalog(), &mcps, &gateway::settings().servers, status.as_ref(), &usage);
    Info { on: gateway::enabled(), ..info }
}

/// Build one row each (kept separate so it's easy to test)
pub fn build(
    catalog: Vec<gateway::Spec>,
    mcps: &[(String, String, bool)],
    served: &[String],
    status: Option<&Value>,
    usage: &HashMap<String, (usize, Vec<String>)>,
) -> Info {
    let live = |name: &str| -> (String, u64, String) {
        let Some(v) = status else { return Default::default() };
        // A per-session one has several entries: running if any copy runs, and say so if any copy crashed
        let mine: Vec<&Value> = v["servers"].as_array().into_iter().flatten().filter(|s| s["name"] == json!(name)).collect();
        let st = |x: &str| mine.iter().any(|s| s["state"] == json!(x));
        let state = if st("failed") {
            "failed"
        } else if st("ready") {
            "ready"
        } else {
            "idle"
        };
        let tools = mine.iter().filter_map(|s| s["tools"].as_u64()).max().unwrap_or(0);
        let error = mine.iter().filter_map(|s| s["error"].as_str()).find(|e| !e.is_empty()).unwrap_or("").to_string();
        (state.to_string(), tools, error)
    };
    let mut rows = vec![];
    for s in &catalog {
        let (what, cat) = match (&s.plugin, s.tools) {
            // The name already comes from the plugin id, so it isn't written again
            (Some(_), _) => (t!("来自插件", "from a plugin").to_string(), false),
            (None, true) => (t!("插件提供给 agent 的工具", "plugin tools for agents").to_string(), false),
            (None, false) => {
                (mcps.iter().find(|m| aca_core::plugin::mount::safe(&m.0) == s.name).map(|m| m.1.clone()).unwrap_or_default(), true)
            }
        };
        let (state, tools, error) = live(&s.name);
        let (calls, users) = usage.get(&s.name).map(|(n, who)| (*n, who.len())).unwrap_or_default();
        rows.push(Srv {
            name: s.name.clone(),
            what,
            catalog: cat,
            served: true,
            per_session: s.per_session,
            state,
            tools,
            error,
            calls,
            users,
        });
    }
    // A served [mcps.*] uses its original name; the catalog's name has characters replaced, so compare against both
    for (name, desc, per_session) in mcps {
        let safe = aca_core::plugin::mount::safe(name);
        if let Some(r) = rows.iter_mut().find(|r| r.catalog && r.name == safe) {
            r.name = name.clone();
            continue;
        }
        if !served.contains(name) {
            rows.push(Srv { name: name.clone(), what: desc.clone(), catalog: true, per_session: *per_session, ..Default::default() });
        }
    }
    let running = status.map(|v| (v["clients"].as_array().map_or(0, Vec::len), v["up"].as_u64().unwrap_or(0)));
    Info { on: true, running, rows }
}

pub struct Page {
    load: fn() -> Info,
    info: Info,
    sel: usize,
}

impl Page {
    pub fn new(load: fn() -> Info) -> Page {
        let info = load();
        Page { load, info, sel: 0 }
    }

    fn cur(&self) -> Option<&Srv> {
        self.info.rows.get(self.sel)
    }

    fn row(&self, r: &Srv, w: usize) -> Line<'static> {
        let (mark, st) = match (r.served, r.state.as_str()) {
            (false, _) => ("○", kit::dim()),
            (true, "ready") => ("●", theme::accent()),
            (true, "failed") => ("✕", theme::warn()),
            (true, _) => ("◌", Style::new()),
        };
        let mut facts = vec![];
        if !r.served {
            facts.push(t!("未加入", "not served").to_string());
        } else {
            facts.push(match r.state.as_str() {
                "ready" => tr!("运行中 · {} 个工具", "running · {} tools", r.tools),
                "failed" => t!("已崩溃", "crashed").to_string(),
                _ => t!("空闲", "idle").to_string(),
            });
            if r.calls > 0 {
                facts.push(tr!("每天 {} 次 · {} 个会话", "{} calls/day · {} sessions", r.calls, r.users));
            }
        }
        if r.per_session {
            facts.push(t!("每个会话独立", "per session").to_string());
        }
        let right = facts.join(" · ");
        let room = w.saturating_sub(right.width() + 6);
        let name_st = if r.served { Style::new().add_modifier(Modifier::BOLD) } else { kit::dim() };
        kit::spread(
            vec![
                Span::raw(" "),
                Span::styled(mark, st),
                Span::raw(" "),
                Span::styled(kit::clip(&r.name, room).trim_end().to_string(), name_st),
            ],
            vec![Span::styled(right, kit::dim())],
            w,
        )
    }
}

impl Overlay for Page {
    fn width(&self) -> u16 {
        80
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let wrap = |text: &str, st: Style| crate::ui::wrap_width(text, w).into_iter().map(move |l| Line::styled(l, st)).collect::<Vec<_>>();
        // While off, new agents bypass it and mount MCPs themselves; not running means idle, it starts itself when the first
        // agent connects — none of this goes on screen
        let head = match (self.info.on, self.info.running) {
            (false, Some((n, _))) => tr!("已关闭 · 仍有 {n} 个连接", "off · {n} still connected"),
            (false, None) => t!("已关闭", "off").to_string(),
            (true, Some((n, _))) => tr!("已开启 · 运行中 · {n} 个连接", "on · running · {n} connections"),
            (true, None) => t!("已开启 · 空闲", "on · idle").to_string(),
        };
        let head_st = if self.info.on { kit::dim() } else { theme::warn() };
        let mut out = vec![
            kit::modal_title("MCP Gateway", theme::accent(), kit::esc_hint(t!("返回", "back")), w),
            Line::styled(format!(" {head}"), head_st),
            Line::raw(""),
        ];
        if self.info.rows.is_empty() {
            out.extend(wrap(
                t!("暂无 MCP · 在 config.toml 中添加 [mcps.<名字>]", "No MCPs yet · add [mcps.<name>] to config.toml"),
                kit::dim(),
            ));
        }
        let n = self.info.rows.len();
        let from = self.sel.saturating_sub(SHOWN / 2).min(n.saturating_sub(SHOWN));
        for (i, r) in self.info.rows.iter().enumerate().skip(from).take(SHOWN) {
            out.push(kit::picked(self.row(r, w), w, i == self.sel));
        }
        out.push(Line::raw(""));
        if let Some(r) = self.cur() {
            if !r.what.is_empty() {
                out.extend(wrap(&r.what, kit::dim()));
            }
            if !r.error.is_empty() {
                out.extend(wrap(&r.error, theme::warn()));
            }
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(
            &[
                ("o", if self.info.on { t!("关闭", "turn off") } else { t!("开启", "turn on") }),
                (t!("空格", "space"), t!("加入/移出", "serve/unserve")),
                ("r R", t!("重启/全部", "restart/all")),
                ("l", t!("日志", "log")),
                ("s", t!("停止进程", "stop process")),
            ],
            vec![],
            w,
        ));
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        let n = self.info.rows.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Close,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Char(' ') => match self.cur().cloned() {
                Some(r) if r.catalog => {
                    match gateway::set_served(&r.name, !r.served) {
                        Ok(()) if r.served => fx.say(tr!("已将 {} 移出 gateway", "took {} out of the gateway", r.name)),
                        Ok(()) => fx.say(tr!("已将 {} 加入 gateway", "{} is served through the gateway now", r.name)),
                        Err(e) => fx.say(e),
                    }
                    self.reload();
                }
                Some(_) => fx.say(t!("来自插件的项随插件开关", "a plugin's own goes on and off with the plugin")),
                None => {}
            },
            KeyCode::Char(c @ ('r' | 'R')) => {
                let name =
                    if c == 'r' { self.cur().map(|r| aca_core::plugin::mount::safe(&r.name)).unwrap_or_default() } else { String::new() };
                match gateway::ctl(json!({ "cmd": "restart", "server": name })) {
                    Ok(v) => fx.say(tr!("已重启 {} 个", "restarted {}", v["restarted"])),
                    Err(e) => fx.say(e),
                }
                self.reload();
            }
            KeyCode::Char('l') => {
                if let Some(r) = self.cur() {
                    let log = gateway::log_of(&aca_core::plugin::mount::safe(&r.name));
                    if log.exists() {
                        fx.external = Some(crate::ui::External::View { path: log.to_string_lossy().into_owned(), line: 1, temp: false });
                    } else {
                        fx.say(tr!("{} 暂无日志", "{} has no log yet", r.name));
                    }
                }
            }
            // On / off = enable / disable this plugin: in the background (like space on the plugins page), this page reloads
            // when done
            KeyCode::Char('o') => fx.job = Some(Job::Plugins(Act::Enable(gateway::PLUGIN.into(), !self.info.on))),
            KeyCode::Char('s') => {
                match gateway::ctl(json!({ "cmd": "stop" })) {
                    Ok(_) => fx.say(t!("已停止 gateway 进程", "stopped the gateway process")),
                    Err(e) => fx.say(e),
                }
                self.reload();
            }
            _ => {}
        }
        Flow::Stay
    }

    fn reload(&mut self) {
        let was = self.cur().map(|r| r.name.clone());
        self.info = (self.load)();
        let same = was.and_then(|n| self.info.rows.iter().position(|r| r.name == n));
        self.sel = same.unwrap_or(self.sel).min(self.info.rows.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aca_core::i18n::{self, Lang};

    fn spec(name: &str, plugin: Option<&str>, tools: bool, per_session: bool) -> gateway::Spec {
        gateway::Spec {
            name: name.into(),
            command: "x".into(),
            args: vec![],
            env: vec![],
            per_session,
            plugin: plugin.map(String::from),
            tools,
        }
    }

    fn info_what(p: &Page, i: usize) -> &str {
        &p.info.rows[i].what
    }

    #[test]
    fn every_server_under_the_gateway_and_the_catalog_ones_left_out_are_listed() {
        i18n::with(Lang::Zh, || {
            let catalog = vec![
                spec("aca-delegate", Some("aca.delegate"), false, false),
                spec("tools", None, true, false),
                spec("web", None, false, true),
            ];
            let mcps = vec![("web".to_string(), "网页搜索".to_string(), true), ("github".to_string(), "GitHub".to_string(), false)];
            let status = json!({ "clients": [{}, {}], "up": 70, "servers": [
                { "name": "web", "session": "a", "state": "ready", "tools": 3, "error": "" },
                { "name": "web", "session": "b", "state": "idle", "tools": 0, "error": "" },
                { "name": "aca-delegate", "session": "", "state": "failed", "tools": 0, "error": "exit 1 (×5)" },
            ]});
            let usage = HashMap::from([("web".to_string(), (7, vec!["a".to_string(), "b".to_string()]))]);
            let info = build(catalog, &mcps, &["web".into()], Some(&status), &usage);
            assert_eq!(info.running, Some((2, 70)));
            let names: Vec<&str> = info.rows.iter().map(|r| r.name.as_str()).collect();
            assert_eq!(names, ["aca-delegate", "tools", "web", "github"]);
            let web = &info.rows[2];
            assert!(
                web.served && web.catalog && web.per_session && web.state == "ready" && web.tools == 3 && web.calls == 7 && web.users == 2
            );
            assert!(info.rows[0].state == "failed" && info.rows[0].error.contains("×5") && !info.rows[0].catalog);
            assert!(!info.rows[3].served && info.rows[3].catalog);
            let p = Page { load: Info::default, info, sel: 2 };
            let t: String = p.lines(100).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>() + "\n").collect();
            assert!(
                t.contains("已开启 · 运行中 · 2 个连接\n")
                    && t.contains("● web")
                    && t.contains("运行中 · 3 个工具 · 每天 7 次 · 2 个会话 · 每个会话独立"),
                "{t}"
            );
            // What space does is in the key bar, not repeated in the body
            assert!(t.contains("✕ aca-delegate") && t.contains("○ github") && t.contains("未加入") && !t.contains("空格:"), "{t}");
            // A plugin's own doesn't show the plugin id; the server of plugin tools for agents is called just that
            assert_eq!((info_what(&p, 0), info_what(&p, 1)), ("来自插件", "插件提供给 agent 的工具"));
        });
    }

    #[test]
    fn o_switches_the_gateway_on_and_off_and_the_top_line_says_which() {
        i18n::with(Lang::Zh, || {
            let text = |p: &Page| -> String {
                p.lines(100).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>() + "\n").collect()
            };
            let mut p = Page { load: Info::default, info: Info { on: true, ..Info::default() }, sel: 0 };
            assert!(text(&p).contains("已开启 · 空闲\n") && text(&p).contains("o 关闭"), "{}", text(&p));
            let mut fx = Fx::default();
            p.on_key(KeyEvent::from(KeyCode::Char('o')), &mut fx);
            assert_eq!(fx.job, Some(Job::Plugins(Act::Enable("aca.gateway".into(), false))));
            // Off with connections left: says how many are still connected
            p.info = Info { on: false, running: Some((2, 5)), rows: vec![] };
            assert!(text(&p).contains("已关闭 · 仍有 2 个连接\n") && text(&p).contains("o 开"), "{}", text(&p));
            p.info = Info { on: false, running: None, rows: vec![] };
            assert!(text(&p).contains(" 已关闭\n"), "{}", text(&p));
            let mut fx = Fx::default();
            p.on_key(KeyEvent::from(KeyCode::Char('o')), &mut fx);
            assert_eq!(fx.job, Some(Job::Plugins(Act::Enable("aca.gateway".into(), true))));
        });
    }
}
