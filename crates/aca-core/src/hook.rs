//! `aca hook <内置那家的名字 | 工具名>`:agent 那边每来一个事件跑一次,事件的 JSON 在标准输入里(hook 协议)。
//!
//! 做这几件事:改这个会话的状态文件;标题变了就把 tmux 会话名改过去;把事件发给界面和插件;
//! 会话之间递话的两头([`inbox`]):它是被派出来的,就在跑完、出错、退出、在等确认时给派它的那个放一条通知;
//! 它自己收件箱里有没送的,这一轮跑完时起一个不等的送信进程。
//! 是哪个会话看环境里的 `ACA_SESSION_ID`(起会话时放进去的),没有就什么也不做。
//! 事件怎么变成状态、标题从哪来,是各家 agent 自己的事(`agent/`),这里只管流程。
//! 声明式工具的转发脚本用 `"$ACA_BIN" hook "$ACA_TOOL"`:两个都是起会话时放进环境的。
//!
//! 规矩:**永远退出 0、标准输出一个字不写**(权限类 hook 的输出会被当成决定),
//! 标准输入读完(不读完 agent 那头会 broken pipe),不等任何慢的东西。

use std::io::Read;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::agent::{self, Agent, Delivery, Events};
use crate::runtime::Run;
use crate::store::{Session, Store};
use crate::{config, event, inbox, paths, proc, runtime, tmux};

/// 这一轮结束了(跑完 / 被打断):标题该有了,也要看一眼有没有人连着
fn turn_ended(v: &Value) -> bool {
    matches!(v["hook_event_name"].as_str(), Some("Stop" | "Interrupt"))
}

/// 记下 agent 自己起的标题(`pick` 说现在是哪个);没锁名字的话把 tmux 会话名也改过去。返回要发的事件
pub(crate) fn retitle(id: &str, pick: impl FnOnce(&Run) -> Option<String>) -> Events {
    let mut title = None;
    runtime::update(id, |r| {
        // 人改过名的也一直记着,只是不拿它去改会话名(放开锁时马上能用)
        if let Some(t) = pick(r).filter(|t| *t != r.auto_title) {
            r.auto_title = t.clone();
            title = (!r.locked).then_some(t);
        }
    });
    match title {
        Some(t) => {
            let name = tmux::sync_name(id, &t).unwrap_or_default();
            vec![("session.titled", json!({ "title": t, "tmux": name }))]
        }
        None => vec![],
    }
}

pub fn run(tool: &str) -> i32 {
    // 不管用不用都读完
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    let cfg = config::load();
    let Ok((agent, _)) = agent::resolve(&cfg, tool) else { return 0 };
    let agent: &dyn Agent = &*agent;
    // Claude 的后台会话继承了原窗格的环境,不是这个会话本人
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    // 查载荷长什么样:`touch ~/.local/state/aca/hook-debug` 之后,每个事件原样记进 hook-debug.log
    let debug = paths::state_dir().join("hook-debug");
    if debug.exists() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(debug.with_extension("log")) {
            let _ = writeln!(f, "{id}\t{tool}\t{}", input.trim());
        }
    }
    let Ok(v) = serde_json::from_str::<Value>(&input) else { return 0 };
    let now = paths::now();
    let ended = turn_ended(&v);
    // 一条 tmux 命令问两样:有没有人连着这个会话、窗格标题。只在用得上的事件上问
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let (attached, pane_title) = match !pane.is_empty() && (ended || agent.title_in_pane()) {
        true => tmux::run(&["display-message", "-p", "-t", &pane, "#{session_attached}\t#{pane_title}"])
            .ok()
            .and_then(|o| o.trim_end_matches('\n').split_once('\t').map(|(a, t)| (a != "0", t.to_string())))
            .unwrap_or_default(),
        false => Default::default(),
    };
    let mut events: Events = vec![];
    runtime::update(&id, |r| {
        events = agent.on_hook(r, &v, now);
        // 跑完的那一刻有人正连着这个会话:算看过了
        if ended && attached {
            r.seen = now;
        }
    });
    events.extend(retitle(&id, |r| agent.title(r, &pane_title)));
    // 标题常常比「跑完」晚一两秒才有(第一轮尤其):还没有的话,留一个不等它的后手过几秒再看一次
    if ended && runtime::load(&id).auto_title.is_empty() {
        let _ = proc::detach(
            Command::new(agent::self_exe())
                .args(["retitle", &id, tool, "3"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
    // Codex 不过状态栏:一轮跑完,留一个不等它的后手看看额度动了没有(动了它往事件流写 limits.changed)
    if ended && agent.name() == "codex" && !cfg!(test) {
        let _ = proc::detach(
            Command::new(agent::self_exe()).args(["limits", "--codex"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()),
        );
    }
    settle(&cfg, &id, agent, &v, events);
    0
}

/// 一个事件的后半截:收件箱、事件流、通知
fn settle(cfg: &config::Config, id: &str, agent: &dyn Agent, v: &Value, events: Events) {
    // 收信这一头:aca 送进去的那段话记成送到了;这一轮跑完了还有没送的,补送一次
    let ev = v["hook_event_name"].as_str().unwrap_or("");
    inbox::on_hook(id, ev, v["prompt"].as_str().unwrap_or(""), agent.delivery() == Delivery::Bell);
    for (name, data) in events {
        event::emit(name, id, data);
        // 被派出来的:这件事告诉派它的那个
        inbox::notify(id, name, cfg);
    }
}

/// aca 替 agent 补的一个事件(它自己不报的那种,比如 Codex 一轮出错,见 [`Agent::unreported_end`]):
/// 和真的 hook 走同一条路,状态、事件流、给派它的那个的通知、收件箱补送都一样。
/// 带着 `at` 的记成那个时刻的事:隔了很久才有人看时,「跑完之后进去看过没有」比的是它真跑完的时刻
pub fn feed(cfg: &config::Config, id: &str, agent: &dyn Agent, v: &Value) {
    let mut events: Events = vec![];
    let at = v["at"].as_u64().unwrap_or_else(paths::now);
    runtime::update(id, |r| events = agent.on_hook(r, v, at));
    settle(cfg, id, agent, v, events);
}

/// `aca retitle <会话> <工具> <秒>`:hook 留的后手。等几秒,再看一次标题
pub fn retitle_later(id: &str, tool: &str, secs: u64) -> i32 {
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let (Ok((agent, _)), Some(p)) = (agent::resolve(&config::load(), tool), tmux::find(id)) else { return 0 };
    // 那个事件之后的状态原样拿来问:Claude 的标题看的是最后一个事件是不是「跑完」
    for (name, data) in retitle(id, |r| agent.title(r, &p.title)) {
        event::emit(name, id, data);
    }
    0
}

/// `aca hook-name`:Claude 起来(SessionStart)、你每发一句(UserPromptSubmit)时和 `aca hook` 并排跑的一条,
/// 把会话的名字交给 Claude 当它自己的名字(`sessionTitle`):输入框上、终端标题、ListAgents / SendMessage 按名字找会话都用它,
/// 不交的话它叫「目录名-编号」。只有这条往标准输出写(那一行 JSON),所以和 `aca hook` 分开。
///
/// 交哪个、什么时候交见 [`name_for`]。读不到会话、出什么错都是不交,下一句再试
pub fn name_cmd() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    // Claude 的后台会话继承了原窗格的环境,不是这个会话本人
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    let Ok(v) = serde_json::from_str::<Value>(&input) else { return 0 };
    let Some(ev) = v["hook_event_name"].as_str().filter(|e| matches!(*e, "SessionStart" | "UserPromptSubmit")) else { return 0 };
    let Ok(s) = Store::open().and_then(|st| st.get(&id)) else { return 0 };
    let has = v["session_title"].as_str().unwrap_or("").trim();
    let mut give = None;
    runtime::update(&id, |r| {
        give = name_for(&s, r, has);
        if let Some(n) = &give {
            r.named = n.clone();
        }
    });
    if let Some(n) = give {
        println!("{}", json!({ "hookSpecificOutput": { "hookEventName": ev, "sessionTitle": n } }));
    }
    0
}

/// 交给 Claude 的名字;`has` 是它现在的名字(hook 载荷里的 `session_title`,没有是空的)。
/// - 交哪个:人起的、改过的名(锁着的)就是它;没锁的交 Claude 自己起的标题(第一轮跑完才有;之后 /clear、重启都还叫这个)
/// - 什么时候交:它还没有名字;或者还叫着 aca 上次交的那个,而你在 aca 里改了名。名字是在 Claude 里 /rename 起的就不动
fn name_for(s: &Session, r: &Run, has: &str) -> Option<String> {
    let want: String = if s.title_locked { &s.title } else { &r.auto_title }.chars().filter(|c| !c.is_control()).collect();
    let want = want.trim();
    let follow = has.is_empty() || (s.title_locked && has == r.named);
    (!want.is_empty() && want != has && follow).then(|| want.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_is_named_after_the_session_once_and_follows_renames_made_in_aca_only() {
        let s = |title: &str, locked: bool| Session { title: title.into(), title_locked: locked, ..Default::default() };
        let r = |auto: &str, named: &str| Run { auto_title: auto.into(), named: named.into(), ..Default::default() };
        // 你起的名:一起来就交
        assert_eq!(name_for(&s("修登录", true), &r("", ""), "").as_deref(), Some("修登录"));
        // 没起名:等它自己起了标题(第一轮跑完)再交;登记时那个目录名不算
        assert_eq!(name_for(&s("aca", false), &r("", ""), ""), None);
        assert_eq!(name_for(&s("aca", false), &r("Fix login\n", ""), "").as_deref(), Some("Fix login"));
        // 已经叫这个了:不再交
        assert_eq!(name_for(&s("修登录", true), &r("", "修登录"), "修登录"), None);
        // 在 aca 里改了名:它还叫着上次交的那个,跟过去
        assert_eq!(name_for(&s("修登录 2", true), &r("", "修登录"), "修登录").as_deref(), Some("修登录 2"));
        // 在 Claude 里 /rename 过:不动
        assert_eq!(name_for(&s("修登录 2", true), &r("", "修登录"), "我起的"), None);
        assert_eq!(name_for(&s("aca", false), &r("Fix login", "Fix login"), "我起的"), None);
        // /clear 之后新的那段对话还没名字:还叫原来那个
        assert_eq!(name_for(&s("aca", false), &r("Fix login", "Fix login"), "").as_deref(), Some("Fix login"));
    }
}
