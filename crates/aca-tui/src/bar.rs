//! `aca tmux-status`: one segment of the tmux status bar: how many sessions need you, which plugin has errors (`⚠ bark`,
//! see `plugin::health`).
//! `aca next`: go into the next one that needs you (the ones those two numbers count). Bound to a key the same way as Ctrl+Q
//! (config.toml's `[keys] next`, see `plugin::bind_hotkeys`): see the number, press the key, you're there.
//!
//! The status bar is caught out of the corner of your eye, so it shows only kinds and counts, with color for urgency, and no
//! names: a name cut to a dozen-odd cells can't be recognized, and seeing it you still can't click it.
//! Who it is and what it's asking, the list spells out in full.
//!
//! tmux runs the status bar every `status-interval` seconds, so it only reads: the registry, one `tmux list-panes`, one small
//! status file per session, the seen marks the interface keeps.
//! It doesn't read screens, doesn't fill in events for a turn whose hook didn't report, doesn't follow renames (those are left
//! to the list refresh), so now and then it's a beat behind the list
//! (e.g. a turn where Codex missed the Stop hook is filled in only when the list refreshes).
//! `aca next` runs once per press: it reads screens like the list, so it also reaches ones only the screen shows need you
//! (Codex without trusted hooks, a confirmation prompt at startup).
//!
//! `aca tmux install` hooks it up for you (see tmuxconf.rs); by hand:
//! `set -ag status-right "#($HOME/.local/bin/aca tmux-status #{@aca_id})"`.
//! The argument at the end is the session you're looking at, which isn't counted; with nothing needing you and every plugin
//! fine, it prints nothing and the segment is gone.

use aca_core::plugin::health::{self, Problem};
use aca_core::status::{self, Row, State};
use aca_core::{t, tr};
use unicode_width::UnicodeWidthChar;

use crate::reads::Reads;
use crate::ui::SEEN_SLACK;

/// How many names of failing plugins to list; the rest as +N
const NAMES: usize = 2;
/// Max width of one name, in cells
const NAME_W: usize = 14;
/// The two kinds that need you: wants you to confirm / choose; finished and not seen yet
const ASK: u8 = 0;
const DONE: u8 = 1;

pub fn run(args: &[String]) -> i32 {
    let here = args.first().map(String::as_str).unwrap_or("");
    // Whatever goes wrong, this segment just isn't there: no errors on the status bar
    let Ok(rows) = aca_core::store::Store::open().and_then(|s| status::peek(&s)) else { return 0 };
    let parts = [text(&rows, &Reads::open(Reads::default_path()), here), alerts(&health::problems())];
    let line = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  ");
    if !line.is_empty() {
        println!("{line}");
    }
    0
}

/// `⚠ bark remote`: plugins with errors (aca's own without `aca.`), at most two listed, the rest as +N.
/// Details on the plugins page, `aca plugin ls`
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
    // tmux reads anything starting with # as a format: double any # in names
    out.replace('#', "##")
}

/// The ones that need you: those wanting your confirmation first, within a kind the most recently active first. `!` wants you
/// to confirm / choose (delegated workers count too), `●` finished and not seen yet (only ones you started yourself: a finished
/// worker is collected by the one that delegated it). The one you're looking at doesn't count
fn waiting<'a>(rows: &'a [Row], reads: &Reads, here: &str) -> Vec<(u8, &'a Row)> {
    // Layer on the seen / unread marks the interface keeps, so it matches what the list shows (see reads.rs)
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

/// ` ◆ 2  ◐ 3`: how many of each, symbols as in the list. `◆` needs your answer: the agent is stopped waiting for you, a red block;
/// `◐` unread: nothing's lost if left, secondary text color. A kind with none isn't shown.
/// Colors are named from the terminal palette: the background is set by your tmux theme, so they follow it
fn text(rows: &[Row], reads: &Reads, here: &str) -> String {
    let w = waiting(rows, reads, here);
    let asks = w.iter().filter(|(kind, _)| *kind == ASK).count();
    let done = w.len() - asks;
    let mut parts = vec![];
    if asks > 0 {
        parts.push(format!("#[fg=red reverse bold] ◆ {asks} #[default]"));
    }
    if done > 0 {
        parts.push(format!("#[fg=brightblack]◐ {done}#[default]"));
    }
    parts.join(" ")
}

/// Which one to go into next: unseen ones first (order as in [`waiting`]); once all are seen and some still wait for your
/// confirmation, the one longest unvisited, round and round.
/// Seen = gone into, with no new activity since (same as `i` in the interface). Ones not running can't be gone into, so don't count
fn pick<'a>(rows: &'a [Row], reads: &Reads, here: &str) -> Option<&'a Row> {
    let w: Vec<(u8, &Row)> = waiting(rows, reads, here).into_iter().filter(|(_, r)| !r.pane.is_empty()).collect();
    let fresh = |r: &Row| reads.read_at(&r.id).is_none_or(|t| r.active > t + SEEN_SLACK);
    w.iter().find(|(_, r)| fresh(r)).or_else(|| w.iter().min_by_key(|(_, r)| reads.read_at(&r.id))).map(|(_, r)| *r)
}

/// `aca next [--session <the one you're in>] [--client <tmux client>]`: go into the next one that needs you. It's what the key runs
/// (`--session '#{@aca_id}' --client '#{client_name}'`, tmux fills in the session and terminal the key was pressed in); typing it
/// yourself, neither is needed.
/// Both the one left and the one entered count as seen, so pressing again goes to the next one
pub fn next(args: &[String]) -> i32 {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).filter(|v| !v.is_empty()).cloned();
    let client = opt("--client");
    let in_tmux = client.is_some() || std::env::var_os("TMUX").is_some();
    let tmux = |args: &[&str]| aca_core::tmux::run(args);
    // The key was pressed in tmux, with no terminal to print to: messages go on the status bar
    let say = |m: &str| match (&client, in_tmux) {
        (Some(c), _) => drop(tmux(&["display-message", "-c", c, m])),
        (None, true) => drop(tmux(&["display-message", m])),
        (None, false) => println!("{m}"),
    };
    // Read screens (as the list does): with peek alone, ones the list shows as ◐ but whose hook didn't report couldn't be reached
    let cfg = aca_core::config::load();
    let rows = match aca_core::store::Store::open().and_then(|s| Ok((status::collect(&s, &cfg, true)?, s))) {
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
        say(t!("暂无待处理会话", "nothing waits for you"));
        return 0;
    };
    // The Ctrl+Q and hotkey bindings are gone after a server restart: redo them before going into a session (as `aca attach` does)
    aca_core::tmux::bind_detach();
    let went = if in_tmux {
        let mut a = vec!["switch-client"];
        if let Some(c) = &client {
            a.extend(["-c", c.as_str()]);
        }
        a.extend(["-t", r.pane.as_str()]);
        tmux(&a).map(|_| ())
    } else {
        // Not in tmux: attach, and come back only on detach
        let sid = aca_core::tmux::find(&r.id).map(|p| p.sid).unwrap_or_default();
        match aca_core::tmux::cmd().args(["attach-session", "-t", &sid]).status() {
            Ok(st) if st.success() => Ok(()),
            Ok(_) => Err(tr!("无法进入「{}」", "couldn't go into “{}”", r.title)),
            Err(e) => Err(tr!("无法启动 tmux:{e}", "couldn't run tmux: {e}")),
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

/// Cut to NAME_W cells wide (a Chinese character is two cells); a cut one ends with …
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

    const ASKS: &str = "#[fg=red reverse bold] ◆ ";
    const DONES: &str = "#[fg=brightblack]◐ ";

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
        // A finished worker doesn't count (the one that delegated it collects it), an asking worker does; names stay off the status bar
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
        // Ones wanting your confirmation first, most recently active first; the one you're in is skipped
        assert_eq!(id(pick(&rows, &reads, "")), Some("ask-new".into()));
        assert_eq!(id(pick(&rows, &reads, "ask-new")), Some("ask-old".into()));
        // Ones gone into and quiet since move back: both asking ones seen, so it's the finished unseen one's turn
        reads.mark_read("ask-new", 100, &|_| true).unwrap();
        reads.mark_read("ask-old", 101, &|_| true).unwrap();
        assert_eq!(id(pick(&rows, &reads, "ask-old")), Some("done-new".into()));
        // A finished one, once seen, no longer needs you; two still asking remain: press from one, go to the other, back and forth
        reads.mark_read("done-new", 102, &|_| true).unwrap();
        reads.mark_read("done-old", 103, &|_| true).unwrap();
        assert_eq!(id(pick(&rows, &reads, "done-old")), Some("ask-new".into()));
        reads.mark_read("ask-new", 104, &|_| true).unwrap();
        assert_eq!(id(pick(&rows, &reads, "ask-new")), Some("ask-old".into()));
        // One active again after being seen (a new question): back to the front
        rows[3].active = 200;
        assert_eq!(id(pick(&rows, &reads, "")), Some("ask-new".into()));
        // Ones not running can't be gone into; none waiting at all = None
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
