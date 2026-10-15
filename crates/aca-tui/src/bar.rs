//! `aca tmux-status`:tmux 状态栏上的一段 —— 有几个会话在等你,哪个插件出错了(`⚠ bark`,见 `plugin::health`)。
//! `aca next`:进下一个在等你的(那两个数指的就是它们)。绑在一个键上,和 Ctrl+Q 一个办法(config.toml 的 `[keys] next`,
//! 见 `plugin::bind_hotkeys`):看到数、按键、到了。
//!
//! 状态栏是余光扫的地方,所以只打种类和个数、用颜色分轻重,不打名字:名字截到十几格认不出来,看到了也点不了。
//! 是谁、在问什么,列表里写得全。
//!
//! tmux 每隔 `status-interval` 秒跑一次,所以只读:登记表、一条 `tmux list-panes`、每个会话一个小状态文件、界面记的已读。
//! 不读屏幕、不替没报 hook 的那一轮补事件、不跟改名(这些留给列表刷新),所以偶尔比列表慢一拍
//! (比如 Codex 漏了 Stop hook 的那一轮,列表刷新时才补上)。
//!
//! `aca tmux install` 替你接上(见 tmuxconf.rs);自己接:`set -ag status-right "#($HOME/.local/bin/aca tmux-status #{@aca_id})"`。
//! 后面那个是你正看着的会话,不算它;没有在等你的、插件也都好好的,就什么都不打,这一段就没了。

use aca_core::plugin::health::{self, Problem};
use aca_core::status::{self, Row, State};
use aca_core::{t, tr};
use unicode_width::UnicodeWidthChar;

use crate::reads::Reads;
use crate::ui::SEEN_SLACK;

/// 出错的插件列几个名字,多的写 +N
const NAMES: usize = 2;
/// 一个名字最多几格宽
const NAME_W: usize = 14;
/// 在等你的两种:要你确认 / 选择,跑完了还没看
const ASK: u8 = 0;
const DONE: u8 = 1;

pub fn run(args: &[String]) -> i32 {
    let here = args.first().map(String::as_str).unwrap_or("");
    // 出什么错都只是这一段没了:状态栏上不打错误
    let Ok(rows) = aca_core::store::Store::open().and_then(|s| status::peek(&s)) else { return 0 };
    let parts = [text(&rows, &Reads::open(Reads::default_path()), here), alerts(&health::problems())];
    let line = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  ");
    if !line.is_empty() {
        println!("{line}");
    }
    0
}

/// `⚠ bark remote`:出错了的插件(aca 自带的去掉 `aca.`),最多列两个,多的写 +N。细节在插件页、`aca plugin ls`
fn alerts(problems: &[Problem]) -> String {
    let mut names: Vec<&str> = vec![];
    for p in problems {
        let n = p.plugin.strip_prefix("aca.").unwrap_or(&p.plugin);
        if !names.contains(&n) {
            names.push(n);
        }
    }
    if names.is_empty() {
        return String::new();
    }
    let mut out = format!("⚠ {}", names.iter().take(NAMES).map(|n| clip(n)).collect::<Vec<_>>().join(" "));
    if names.len() > NAMES {
        out.push_str(&format!(" +{}", names.len() - NAMES));
    }
    // tmux 拿 # 起头的当格式:名字里的 # 写两个
    out.replace('#', "##")
}

/// 在等你的,先要你确认的,同一种里最近有动静的在前。`!` 要你确认 / 选择(派出去的 worker 也算),
/// `●` 跑完了还没看(只算你自己开的:worker 跑完是派它的那个收)。你正看着的那个不算
fn waiting<'a>(rows: &'a [Row], reads: &Reads, here: &str) -> Vec<(u8, &'a Row)> {
    // 叠上界面记的已读 / 未读,跟列表里看到的一样(见 reads.rs)
    let state = |r: &Row| match r.state {
        State::Unread if reads.read_at(&r.id).is_some_and(|t| r.active <= t + SEEN_SLACK) => State::Idle,
        State::Idle if reads.unread(&r.id) => State::Unread,
        st => st,
    };
    let mut waiting: Vec<(u8, &Row)> = rows
        .iter()
        .filter(|r| r.id != here)
        .filter_map(|r| match state(r) {
            State::Ask => Some((ASK, r)),
            State::Unread if r.parent.is_empty() => Some((DONE, r)),
            _ => None,
        })
        .collect();
    waiting.sort_by_key(|(kind, r)| (*kind, std::cmp::Reverse(r.active)));
    waiting
}

/// ` ! 2  ● 3`:各有几个。`!` 是 agent 停着等你,红底的一块;`●` 放着也不丢,次要的字色。没有的那一样不打。
/// 颜色用终端调色板里的名字:底色是你的 tmux 主题定的,跟着它走
fn text(rows: &[Row], reads: &Reads, here: &str) -> String {
    let w = waiting(rows, reads, here);
    let asks = w.iter().filter(|(kind, _)| *kind == ASK).count();
    let done = w.len() - asks;
    let mut parts = vec![];
    if asks > 0 {
        parts.push(format!("#[fg=red reverse bold] ! {asks} #[default]"));
    }
    if done > 0 {
        parts.push(format!("#[fg=brightblack]● {done}#[default]"));
    }
    parts.join(" ")
}

/// 下一个进哪个:先没看过的(次序同 [`waiting`]);都看过了、还有在等你确认的,挑最久没去的那个,一圈圈轮。
/// 看过 = 进去过、之后没有新动静(同界面里的 `i`)。没在跑的进不去,不算
fn pick<'a>(rows: &'a [Row], reads: &Reads, here: &str) -> Option<&'a Row> {
    let w: Vec<(u8, &Row)> = waiting(rows, reads, here).into_iter().filter(|(_, r)| !r.pane.is_empty()).collect();
    let fresh = |r: &Row| reads.read_at(&r.id).is_none_or(|t| r.active > t + SEEN_SLACK);
    w.iter().find(|(_, r)| fresh(r)).or_else(|| w.iter().min_by_key(|(_, r)| reads.read_at(&r.id))).map(|(_, r)| *r)
}

/// `aca next [--session <你正在的那个>] [--client <tmux 的客户端>]`:进下一个在等你的。键按下去跑的就是它
/// (`--session '#{@aca_id}' --client '#{client_name}'`,tmux 换成按键的那个会话、那台终端);自己敲的话两样都不用写。
/// 离开的这个、进去的那个都算看过了,所以再按一次是下一个
pub fn next(args: &[String]) -> i32 {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).filter(|v| !v.is_empty()).cloned();
    let client = opt("--client");
    let in_tmux = client.is_some() || std::env::var_os("TMUX").is_some();
    let tmux = |args: &[&str]| aca_core::tmux::run(args);
    // 键是在 tmux 里按的,没有终端可打字:话说在状态栏上
    let say = |m: &str| match (&client, in_tmux) {
        (Some(c), _) => drop(tmux(&["display-message", "-c", c, m])),
        (None, true) => drop(tmux(&["display-message", m])),
        (None, false) => println!("{m}"),
    };
    let rows = match aca_core::store::Store::open().and_then(|s| Ok((status::peek(&s)?, s))) {
        Ok(v) => v,
        Err(e) => {
            say(&e);
            return 1;
        }
    };
    let (rows, store) = rows;
    let here = opt("--session")
        .or_else(|| in_tmux.then(|| tmux(&["display-message", "-p", "#{@aca_id}"]).unwrap_or_default().trim().to_string()))
        .unwrap_or_default();
    let mut reads = Reads::open(Reads::default_path());
    let now = aca_core::paths::now();
    if !here.is_empty() {
        let _ = reads.mark_read(&here, now, &|_| true);
    }
    let Some(r) = pick(&rows, &reads, &here) else {
        say(t!("没有在等你的", "nothing waits for you"));
        return 0;
    };
    // Ctrl+Q、热键的绑定服务重启就没了:进会话前补一遍(同 `aca attach`)
    aca_core::tmux::bind_detach();
    let went = if in_tmux {
        let mut a = vec!["switch-client"];
        if let Some(c) = &client {
            a.extend(["-c", c.as_str()]);
        }
        a.extend(["-t", r.pane.as_str()]);
        tmux(&a).map(|_| ())
    } else {
        // 不在 tmux 里:连上去,detach 才回来
        let sid = aca_core::tmux::find(&r.id).map(|p| p.sid).unwrap_or_default();
        match aca_core::tmux::cmd().args(["attach-session", "-t", &sid]).status() {
            Ok(st) if st.success() => Ok(()),
            Ok(_) => Err(tr!("进不去「{}」", "couldn't go into “{}”", r.title)),
            Err(e) => Err(tr!("tmux 起不来:{e}", "couldn't run tmux: {e}")),
        }
    };
    match went {
        Ok(()) => {
            let _ = reads.mark_read(&r.id, now, &|_| true);
            let _ = aca_core::session::seen(&store, &r.id);
            0
        }
        Err(e) => {
            say(&e);
            1
        }
    }
}

/// 截到 NAME_W 格宽(中文一个字两格),截了的末尾放 …
fn clip(s: &str) -> String {
    let w = |c: char| c.width().unwrap_or(0);
    if s.chars().map(w).sum::<usize>() <= NAME_W {
        return s.to_string();
    }
    let (mut out, mut used) = (String::new(), 0);
    for c in s.chars().take_while(|c| {
        used += w(*c);
        used < NAME_W
    }) {
        out.push(c);
    }
    out + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(id: &str, title: &str, state: State, parent: &str, active: u64) -> Row {
        Row {
            id: id.into(),
            title: title.into(),
            state,
            tool: String::new(),
            account: String::new(),
            group: String::new(),
            dir: String::new(),
            parent: parent.into(),
            fork_of: String::new(),
            tmux: String::new(),
            pane: format!("%{id}"),
            attached: 0,
            conv: String::new(),
            ask: String::new(),
            ran: 0,
            active,
            bg: 0,
            locked: false,
            created: 0,
            order: 0,
            tokens: BTreeMap::new(),
            idle_stopped: 0,
        }
    }

    const ASKS: &str = "#[fg=red reverse bold] ! ";
    const DONES: &str = "#[fg=brightblack]● ";

    #[test]
    fn the_bar_counts_what_waits_asks_loud_finished_quiet_and_not_the_one_you_are_in() {
        let reads = Reads::open(None);
        let rows = [
            row("a", "写文档", State::Unread, "", 10),
            row("b", "修登录", State::Ask, "", 5),
            row("c", "worker 跑完", State::Unread, "a", 20),
            row("d", "worker 问权限", State::Ask, "a", 30),
            row("e", "在跑", State::Running, "", 40),
            row("f", "我正看着", State::Ask, "", 50),
        ];
        // worker 跑完的不算(派它的那个收),worker 在问的算;名字不上状态栏
        assert_eq!(text(&rows, &reads, "f"), format!("{ASKS}2 #[default] {DONES}1#[default]"));
        assert_eq!(text(&rows, &reads, ""), format!("{ASKS}3 #[default] {DONES}1#[default]"));
        assert_eq!(text(&rows[..1], &reads, ""), format!("{DONES}1#[default]"));
        assert_eq!(text(&rows[1..2], &reads, ""), format!("{ASKS}1 #[default]"));
        assert_eq!(text(&rows[4..5], &reads, ""), "");
        assert_eq!(text(&rows[..1], &reads, "a"), "");
    }

    #[test]
    fn marks_from_the_list_count_in_the_bar() {
        let mut reads = Reads::open(None);
        reads.mark_read("a", 100, &|_| true).unwrap();
        reads.mark_unread("b", &|_| true).unwrap();
        let rows = [row("a", "看过了", State::Unread, "", 100), row("b", "标成未读", State::Idle, "", 1)];
        assert_eq!(text(&rows, &reads, ""), format!("{DONES}1#[default]"));
        assert_eq!(waiting(&rows, &reads, "")[0].1.id, "b");
    }

    #[test]
    fn next_goes_to_asks_first_then_finished_and_once_all_are_seen_cycles_through_the_asks() {
        let mut reads = Reads::open(None);
        let id = |r: Option<&Row>| r.map(|r| r.id.clone());
        let mut rows = vec![
            row("done-old", "", State::Unread, "", 10),
            row("done-new", "", State::Unread, "", 20),
            row("ask-old", "", State::Ask, "", 5),
            row("ask-new", "", State::Ask, "", 8),
            row("busy", "", State::Running, "", 99),
        ];
        // 先要你确认的,最近有动静的在前;你正在的那个跳过
        assert_eq!(id(pick(&rows, &reads, "")), Some("ask-new".into()));
        assert_eq!(id(pick(&rows, &reads, "ask-new")), Some("ask-old".into()));
        // 进去看过、没再动的往后排:两个在问的都看过了,轮到跑完没看的
        reads.mark_read("ask-new", 100, &|_| true).unwrap();
        reads.mark_read("ask-old", 101, &|_| true).unwrap();
        assert_eq!(id(pick(&rows, &reads, "ask-old")), Some("done-new".into()));
        // 跑完的看过就不算在等你了;剩下两个还在问:从一个按,去另一个,来回轮
        reads.mark_read("done-new", 102, &|_| true).unwrap();
        reads.mark_read("done-old", 103, &|_| true).unwrap();
        assert_eq!(id(pick(&rows, &reads, "done-old")), Some("ask-new".into()));
        reads.mark_read("ask-new", 104, &|_| true).unwrap();
        assert_eq!(id(pick(&rows, &reads, "ask-new")), Some("ask-old".into()));
        // 看过之后又有动静的(换了一个问题):又排到前面
        rows[3].active = 200;
        assert_eq!(id(pick(&rows, &reads, "")), Some("ask-new".into()));
        // 没在跑的进不去;一个在等的都没有 = None
        rows[2].pane.clear();
        rows[3].pane.clear();
        assert_eq!(id(pick(&rows, &reads, "")), None);
        assert_eq!(id(pick(&rows[4..], &reads, "")), None);
    }

    #[test]
    fn broken_plugins_are_named_once_each_at_most_two_and_long_or_odd_names_are_made_safe() {
        let p = |plugin: &str, what: &str| Problem { plugin: plugin.into(), what: what.into(), detail: String::new(), at: 0 };
        assert_eq!(alerts(&[]), "");
        assert_eq!(alerts(&[p("aca.bark", "钩子 turn.done"), p("aca.bark", "钩子 agent.waiting")]), "⚠ bark");
        assert_eq!(alerts(&[p("aca.bark", "a"), p("me.todo#1", "b"), p("aca.remote", "c")]), "⚠ bark me.todo##1 +1");
        assert_eq!(clip("一个特别特别长的会话名字"), "一个特别特别…");
        assert_eq!(clip("short"), "short");
    }
}
