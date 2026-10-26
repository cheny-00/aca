//! 替别的会话按键:`aca answer`(回答确认框)和 `aca interrupt`(中止正在跑的这一轮,见 [`interrupt`])。
//!
//! `aca answer`:替一个在等确认的会话回答。普通消息和 `aca send` 都不往确认框里打字(回车可能直接选中默认那一项),
//! 要回答只走这里:先核对它还在问同一句、是哪种框,再按键,按完记一条 `ask.answered`。
//!
//! - 选择题(AskUserQuestion):按选项的数字,一按就选上(2.1.294 实测,一次只问一题时)。一次问好几题、能多选的不替它答
//! - 权限框:选项个数和顺序会变,只做同意和拒绝,按哪两个键看各家([`Agent::permission_keys`])。
//!   Claude 按 Esc 拒绝后不发任何 hook(连 Stop 都没有):这里自己把它改成空着
//! - 只在屏幕上认得出的启动关卡(信任这个目录、有更新、hook 要审):屏幕上有一串带编号的选项才认([`gate`]),
//!   用上下键走到那一项再回车(这类框按数字不一定算选上);按之前再抓一次屏,还是同一个框才按
//! - 别的看不出是哪种的不替它按
//!
//! 谁能按:你在外面随便按;agent(在某个会话里调的)只能按自己派出去的那些(parent 是它,或者再往下几层)。

use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

use crate::agent::{self, Agent};
use crate::config::Config;
use crate::i18n::Tr;
use crate::paths::Res;
use crate::runtime::{self, Run};
use crate::status::{self, State};
use crate::store::{Session, Store};
use crate::{event, t, tmux, tr};

/// 只在屏幕上认得出的框(启动时那几道关)
#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    /// 选项上面那几行(「Folder access · /p/api · Trust this folder? …」)
    pub title: String,
    pub opts: Vec<String>,
    /// 现在高亮的是第几个(0 起)
    pub at: usize,
}

/// 「› 1. Trust and continue」「  2. Quit」:前面可以有个高亮记号
static OPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*([›❯>▶]\s*)?(\d)\.\s+(\S.*?)\s*$").unwrap());

/// 屏幕最下面一串连着的、从 1 起顺着编号的选项(至少两个),和它上面的几行。认不出就是 None
pub fn gate(lines: &[String]) -> Option<Gate> {
    let rows: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let tail = &rows[rows.len().saturating_sub(16)..];
    let end = tail.iter().rposition(|l| OPT.is_match(l))? + 1;
    let mut start = end - 1;
    while start > 0 && OPT.is_match(tail[start - 1]) {
        start -= 1;
    }
    let (mut opts, mut at) = (vec![], 0);
    for (i, l) in tail[start..end].iter().enumerate() {
        let c = OPT.captures(l)?;
        if c[2].parse::<usize>().ok()? != i + 1 {
            return None;
        }
        if c.get(1).is_some() {
            at = i;
        }
        opts.push(c[3].to_string());
    }
    if opts.len() < 2 {
        return None;
    }
    let title: Vec<&str> = tail[start.saturating_sub(3)..start].iter().map(|l| l.trim()).collect();
    Some(Gate { title: title.join(" · "), opts, at })
}

/// 走到第 n 个(1 起)再回车
pub(crate) fn gate_keys(g: &Gate, n: usize) -> Vec<&'static str> {
    let to = n - 1;
    let mut keys = if to > g.at { vec!["Down"; to - g.at] } else { vec!["Up"; g.at - to] };
    keys.push("Enter");
    keys
}

/// 它在问什么(`aca answer <会话>` 不带回答时打出来)。`screen` = 它现在的屏幕(只在屏幕上认得出的框要看)
pub fn show(s: &Session, r: &Run, screen: &[String]) -> String {
    if r.ask_kind.is_empty()
        && let Some(g) = gate(screen)
    {
        let mut out = tr!("{} 正在询问:{}", "{} is asking: {}", s.id, g.title);
        for (i, o) in g.opts.iter().enumerate() {
            out += &format!("\n  {} {o}", i + 1);
        }
        return out
            + &tr!(
                "\n回答:aca answer {} <选项号>(启动时的确认框,只能从屏幕识别)",
                "\nanswer: aca answer {} <NUMBER> (a startup gate, seen only on screen)",
                s.id
            );
    }
    let mut out = tr!(
        "{} 正在询问:{}",
        "{} is asking: {}",
        s.id,
        if r.ask.is_empty() { t!("(未提供问题内容)", "(no question given)") } else { &r.ask }
    );
    match r.ask_kind.as_str() {
        "choice" if r.ask_multi => {
            out += t!(
                "\n包含多个问题(或可多选),无法用一个数字回答:请在它的界面中回答",
                "\nseveral questions (or multiple choice): one number can't answer it; answer in its own screen"
            )
        }
        "choice" => {
            for (i, o) in r.ask_opts.iter().enumerate() {
                out += &format!("\n  {} {o}", i + 1);
            }
            out += &tr!("\n回答:aca answer {} <选项号>", "\nanswer: aca answer {} <NUMBER>", s.id);
        }
        "permission" => out += &tr!("\n回答:aca answer {} yes|no", "\nanswer: aca answer {} yes|no", s.id),
        _ => out += t!("\n无法识别确认框类型:请在它的界面中回答", "\ncan't tell what kind of prompt this is; answer in its own screen"),
    }
    out
}

/// 在某个会话里调的:只能答它派出去的那些(往上数 parent,数到它为止)
fn may_answer(store: &Store, me: &str, target: &Session) -> bool {
    let mut up = target.parent.clone();
    for _ in 0..32 {
        if up.is_empty() {
            return false;
        }
        if up == me {
            return true;
        }
        up = store.get(&up).map(|s| s.parent).unwrap_or_default();
    }
    false
}

/// 要按的键和给人看的那一句
fn keys_for(agent: &dyn Agent, r: &Run, reply: &str) -> Res<(&'static str, String)> {
    match r.ask_kind.as_str() {
        "choice" if r.ask_multi => Err(t!(
            "包含多个问题(或可多选),无法用一个数字回答:请在它的界面中回答",
            "several questions (or multiple choice): one number can't answer it; answer in its own screen"
        )
        .into()),
        "choice" => {
            let n: usize =
                reply.parse().map_err(|_| t!("选择题请输入选项号:1、2 …", "a choice takes an option number: 1, 2 …").to_string())?;
            let most = if r.ask_opts.is_empty() { 9 } else { r.ask_opts.len().min(9) };
            if n == 0 || n > most {
                return Err(tr!("只有 {most} 个选项", "there are only {most} options"));
            }
            const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
            let label = r.ask_opts.get(n - 1).map(|o| format!("{n} {o}")).unwrap_or_else(|| n.to_string());
            Ok((DIGITS[n - 1], label))
        }
        "permission" => {
            let (yes, no) = agent.permission_keys().ok_or(t!(
                "尚不支持该工具的确认框:请在它的界面中回答",
                "this tool's prompts can't be answered yet; answer in its own screen"
            ))?;
            match reply {
                "yes" | "y" => Ok((yes, "yes".into())),
                "no" | "n" => Ok((no, "no".into())),
                _ => Err(t!("权限确认只能回答 yes 或 no", "a permission prompt takes only yes or no").into()),
            }
        }
        _ => Err(t!(
            "无法识别询问类型,未代为回答:请在它的界面中回答",
            "can't tell what it's asking; nothing answered: answer in its own screen"
        )
        .into()),
    }
}

/// 谁在按:在外面是 None;在某个会话里调的是它的 id,而且得是自己派出去的那些(`what` 是「回答」「中止」)
fn caller(store: &Store, s: &Session, what: &str) -> Res<Option<String>> {
    let me = std::env::var("ACA_SESSION_ID").ok().filter(|x| !x.is_empty());
    if let Some(me) = &me {
        if *me == s.id {
            return Err(tr!("不能为自己{what}", "can't {what} yourself"));
        }
        if !may_answer(store, me, s) {
            return Err(tr!(
                "{} 不是你派出的会话:只能为自己派出的会话{what}",
                "{} isn't a session you dispatched: you can only {what} the ones you dispatched",
                s.id
            ));
        }
    }
    Ok(me)
}

/// `aca interrupt`:中止它正在跑的这一轮(按 Esc),比如它往错的方向跑了。只在真在跑时按;
/// 在等确认的不按(Esc 在确认框里是「拒绝」,要拒绝用 `aca answer … no`)。
/// 停下以后它空着:要换个说法让它接着干用 `aca send`(打字进去马上开一轮)。Codex 被打断后排着的消息不自己开跑
pub fn interrupt(store: &Store, cfg: &Config, key: &str) -> Res<String> {
    let s = store.get(key)?;
    let me = caller(store, &s, t!("中止", "interrupt"))?;
    let (agent, _) = agent::of(cfg, &s)?;
    let keys = agent
        .interrupt_keys()
        .ok_or(t!("尚不支持中止该工具:请在它的界面中操作", "this tool can't be interrupted yet; do it in its own screen"))?;
    let pane = tmux::find(&s.id).filter(|p| !p.dead).ok_or(t!("会话未运行", "the session isn't running"))?;
    match status::one(cfg, &s, true).state {
        State::Running => {}
        State::Ask => {
            return Err(t!(
                "它正在等待确认,未在运行:如需拒绝,用 aca answer <会话> no",
                "it's waiting for a confirmation, not running: to refuse, aca answer <id> no"
            )
            .into());
        }
        _ => return Err(t!("它当前没有在运行的一轮,无需中止", "it has no running turn to interrupt").into()),
    }
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        tmux::keys(&pane.pane, &[k])?;
    }
    if !agent.reports_interrupt() {
        // Claude 被 Esc 停下不发 hook:自己记成空着
        runtime::update(&s.id, |r| {
            r.status = "idle".into();
            r.clear_ask();
        });
        event::emit("turn.interrupted", &s.id, json!({ "by": me.unwrap_or_else(|| "you".into()) }));
    }
    Ok(tr!("已中止 {} 正在运行的这一轮", "interrupted the running turn of {}", s.id))
}

/// 按之前再看一眼,问的已经不是那一句了
const CHANGED: Tr = Tr::new(
    "询问内容已变化,未回答:请重新查看 aca answer <会话>",
    "the question changed; nothing answered: look again with aca answer <id>",
);

/// 回答;返回给人看的那一句
pub fn answer(store: &Store, cfg: &Config, key: &str, reply: &str) -> Res<String> {
    let s = store.get(key)?;
    let me = caller(store, &s, t!("回答", "answer for"))?;
    let (agent, _) = agent::of(cfg, &s)?;
    let pane = tmux::find(&s.id).filter(|p| !p.dead).ok_or(t!("会话未运行", "the session isn't running"))?;
    if status::one(cfg, &s, true).state != State::Ask {
        return Err(t!("它当前未在等待确认", "it isn't waiting for a confirmation").into());
    }
    let r = runtime::load(&s.id);
    if r.ask_kind.is_empty()
        && let Some(g) = gate(&tmux::capture(&pane.pane))
    {
        return answer_gate(&s, &pane.pane, &g, reply.trim(), me);
    }
    let (key, said) = keys_for(&*agent, &r, reply.trim())?;
    // 按之前再看一眼:还在问同一句(和你同时按、框已经换了的,不按)
    let now = runtime::load(&s.id);
    if now.status != "waiting" || now.ask != r.ask || now.ask_kind != r.ask_kind {
        return Err(CHANGED.get().into());
    }
    tmux::keys(&pane.pane, &[key])?;
    box_gone(&*agent, &pane.pane);
    let refused = r.ask_kind == "permission" && reply.starts_with('n');
    runtime::update(&s.id, |r| {
        // 拒绝了的,Claude 停下来等你说别的,不发 hook;别的接着跑,它自己的 hook 会再报
        r.status = if refused { "idle".into() } else { "running".into() };
        r.clear_ask();
    });
    let by = me.unwrap_or_else(|| "you".into());
    event::emit("ask.answered", &s.id, json!({ "by": by, "answer": said, "ask": r.ask, "kind": r.ask_kind }));
    Ok(tr!("已代 {} 回答:{said}", "answered for {}: {said}", s.id))
}

/// 按完之后等屏幕上那个框收起来,最多 1.5 秒:列表、`aca wait` 以屏幕为准,框还没重画掉的那一下还当它在问
/// (opencode 重画得慢,答完马上 `aca wait` 会读成又在等确认)。屏幕上认不出框的那几家不等
fn box_gone(agent: &dyn Agent, pane: &str) {
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_millis(1500) && matches!(agent.screen(&tmux::capture(pane)), crate::screen::Seen::Ask(_))
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// 启动时的关卡:状态看屏幕(没有 hook 报),按完不改 runtime,它过了关自己会变
fn answer_gate(s: &Session, pane: &str, g: &Gate, reply: &str, me: Option<String>) -> Res<String> {
    let n: usize = reply
        .parse()
        .map_err(|_| t!("这是启动时的确认框,请输入选项号:1、2 …", "this is a startup gate: give an option number: 1, 2 …").to_string())?;
    if n == 0 || n > g.opts.len() {
        return Err(tr!("只有 {} 个选项", "there are only {} options", g.opts.len()));
    }
    // 按之前再抓一次:框换了(和你同时按了)就不按
    if gate(&tmux::capture(pane)).as_ref() != Some(g) {
        return Err(CHANGED.get().into());
    }
    tmux::keys(pane, &gate_keys(g, n))?;
    let said = format!("{n} {}", g.opts[n - 1]);
    let by = me.unwrap_or_else(|| "you".into());
    event::emit("ask.answered", &s.id, json!({ "by": by, "answer": said, "ask": g.title, "kind": "gate" }));
    Ok(tr!("已代 {} 回答:{said}", "answered for {}: {said}", s.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_choice_or_a_known_permission_box_is_answered() {
        let claude = agent::builtin("claude").unwrap();
        let codex = agent::builtin("codex").unwrap();
        let q =
            Run { ask: "选 A 还是 B?".into(), ask_kind: "choice".into(), ask_opts: vec!["A".into(), "B".into()], ..Default::default() };
        assert_eq!(keys_for(claude, &q, "2").unwrap(), ("2", "2 B".to_string()));
        assert!(keys_for(claude, &q, "3").unwrap_err().contains("只有 2 个"));
        assert!(keys_for(claude, &q, "B").is_err() && keys_for(claude, &q, "0").is_err());
        assert!(keys_for(claude, &Run { ask_multi: true, ..q.clone() }, "1").unwrap_err().contains("界面中回答"));
        // 权限框:Claude 同意按 1、拒绝按 Esc;Codex 同意按 y、拒绝按 Esc(0.163 上实测)
        let p = Run { ask: "Bash:touch x".into(), ask_kind: "permission".into(), ..Default::default() };
        assert_eq!(keys_for(claude, &p, "yes").unwrap().0, "1");
        assert_eq!(keys_for(claude, &p, "no").unwrap().0, "Escape");
        assert!(keys_for(claude, &p, "2").is_err());
        assert_eq!((keys_for(codex, &p, "yes").unwrap().0, keys_for(codex, &p, "no").unwrap().0), ("y", "Escape"));
        // 只在屏幕上认出来的:不归这里(走 gate)
        assert!(keys_for(claude, &Run { ask: "Hooks need review".into(), ..Default::default() }, "1").is_err());
    }

    #[test]
    fn what_it_says_back_follows_the_interface_language() {
        use crate::i18n::{self, Lang};
        let claude = agent::builtin("claude").unwrap();
        let p = Run { ask: "Bash:touch x".into(), ask_kind: "permission".into(), ..Default::default() };
        let q = Run { ask: "A or B?".into(), ask_kind: "choice".into(), ask_opts: vec!["A".into(), "B".into()], ..Default::default() };
        let s = Session { id: "w1".into(), ..Default::default() };
        assert_eq!(show(&s, &q, &[]), "w1 正在询问:A or B?\n  1 A\n  2 B\n回答:aca answer w1 <选项号>");
        i18n::with(Lang::En, || {
            assert_eq!(keys_for(claude, &p, "2").unwrap_err(), "a permission prompt takes only yes or no");
            assert_eq!(keys_for(claude, &q, "3").unwrap_err(), "there are only 2 options");
            assert_eq!(show(&s, &q, &[]), "w1 is asking: A or B?\n  1 A\n  2 B\nanswer: aca answer w1 <NUMBER>");
        });
    }

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn a_startup_gate_with_numbered_options_is_read_off_the_screen_and_walked_to_with_arrows() {
        // Codex 0.159 的信任框(实际抓的),上面还有别的字
        let trust = "OpenAI Codex\n\n  Folder access\n  /p/api\n  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit";
        let g = gate(&lines(trust)).unwrap();
        assert_eq!(g.opts, ["Trust and continue", "Quit"]);
        assert_eq!(g.at, 0);
        assert_eq!(g.title, "Folder access · /p/api · Trust this folder? Codex can read, edit, and run files here.");
        assert_eq!(gate_keys(&g, 1), ["Enter"]);
        assert_eq!(gate_keys(&g, 2), ["Down", "Enter"]);
        // 高亮在第三个:往上走
        let review = "  Hooks need review\n  1. Review hooks\n  2. Trust all and continue\n› 3. Continue without trusting\n  enter confirm · esc skip";
        let g = gate(&lines(review)).unwrap();
        assert_eq!((g.at, gate_keys(&g, 1)), (2, vec!["Up", "Up", "Enter"]));
        let s = Session { id: "w1".into(), ..Default::default() };
        let said = show(&s, &Run::default(), &lines(review));
        assert!(
            said.contains("w1 正在询问:Hooks need review")
                && said.contains("  2 Trust all and continue")
                && said.contains("aca answer w1 <选项号>")
        );
        // 编号不连着、只有一个、没有编号的:不认
        assert_eq!(gate(&lines("› 1. a\n  3. b")), None);
        assert_eq!(gate(&lines("› 1. only")), None);
        assert_eq!(gate(&lines("❯ No, exit\n  Yes, I trust this folder")), None);
        // hook 报了是哪种的(选择题、权限框)照旧,不看屏幕
        let q = Run { ask: "选?".into(), ask_kind: "choice".into(), ask_opts: vec!["A".into(), "B".into()], ..Default::default() };
        assert!(show(&s, &q, &lines(review)).contains("选?"));
    }

    #[test]
    fn an_agent_may_only_answer_the_sessions_it_sent_out() {
        let store = Store::memory();
        let add = |id: &str, parent: &str| store.insert(&Session { id: id.into(), parent: parent.into(), ..Default::default() }).unwrap();
        add("lead", "");
        add("w1", "lead");
        add("w1a", "w1");
        add("other", "");
        let get = |id: &str| store.get(id).unwrap();
        assert!(may_answer(&store, "lead", &get("w1")) && may_answer(&store, "lead", &get("w1a")));
        assert!(!may_answer(&store, "w1", &get("lead")) && !may_answer(&store, "lead", &get("other")));
    }
}
