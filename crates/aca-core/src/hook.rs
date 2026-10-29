//! `aca hook <built-in tool name | tool name>`: runs once for every event from the agent, with the event's JSON on stdin
//! (the hook protocol).
//!
//! What it does: updates the session's state file; renames the tmux session when the title changes; sends the event to
//! the interface and plugins; and both ends of passing messages between sessions ([`inbox`]): if this session was
//! delegated, it leaves a notice for the session that delegated it when it finishes, errors, exits or waits for a
//! confirmation; if its own inbox has undelivered messages, it starts a detached delivery process when the turn ends.
//! Which session it is comes from `ACA_SESSION_ID` in the environment (set when the session starts); without it, nothing
//! happens. How events become states and where titles come from is each agent's business (`agent/`); this only runs the
//! flow. A declared tool's forwarding script uses `"$ACA_BIN" hook "$ACA_TOOL"`: both are put in the environment when the
//! session starts.
//!
//! Rules: **always exit 0 and never write a byte to stdout** (output from permission hooks is taken as a decision), read
//! stdin to the end (otherwise the agent gets a broken pipe), and never wait on anything slow.

use std::io::Read;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::agent::{self, Agent, Delivery, Events};
use crate::runtime::Run;
use crate::store::{Session, Store};
use crate::{config, event, inbox, paths, proc, runtime, tmux};

/// The turn is over (finished / interrupted): the title should be there by now, and check whether anyone is attached
fn turn_ended(v: &Value) -> bool {
    matches!(v["hook_event_name"].as_str(), Some("Stop" | "Interrupt"))
}

/// Records the title the agent made up (`pick` says which one it is now); if the name isn't locked, renames the tmux
/// session to match. Returns the events to send
pub(crate) fn retitle(id: &str, pick: impl FnOnce(&Run) -> Option<String>) -> Events {
    let mut title = None;
    runtime::update(id, |r| {
        // A name set by a person is still recorded, just not used to rename the session (so it's ready the moment the lock is
        // released)
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
    // Read it all, used or not
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    let cfg = config::load();
    let Ok((agent, _)) = agent::resolve(&cfg, tool) else { return 0 };
    let agent: &dyn Agent = &*agent;
    // Claude's background sessions inherit the original pane's environment; they aren't this session itself
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    // To see what payloads look like: after `touch ~/.local/state/aca/hook-debug`, every event is logged verbatim to
    // hook-debug.log
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
    // One tmux command asks two things: is anyone attached to this session, and the pane title. Only asked on events that
    // need it
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
        // Someone is attached to the session the moment it finishes: counts as read
        if ended && attached {
            r.seen = now;
        }
    });
    events.extend(retitle(&id, |r| agent.title(r, &pane_title)));
    // The title often arrives a second or two after "finished" (especially on the first turn): if it isn't there yet, leave
    // a detached follow-up that looks again in a few seconds
    if ended && runtime::load(&id).auto_title.is_empty() {
        let _ = proc::detach(
            Command::new(agent::self_exe())
                .args(["retitle", &id, tool, "3"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
    // Codex has no status bar to go through: after a turn, leave a detached follow-up that checks whether its limits moved
    // (if they did, it writes limits.changed to the event stream)
    if ended && agent.name() == "codex" && !cfg!(test) {
        let _ = proc::detach(
            Command::new(agent::self_exe()).args(["limits", "--codex"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()),
        );
    }
    settle(&cfg, &id, agent, &v, events);
    0
}

/// The second half of an event: inbox, event stream, notices
fn settle(cfg: &config::Config, id: &str, agent: &dyn Agent, v: &Value, events: Events) {
    // The receiving end: the text aca typed in is marked delivered; if the turn finished and there's still undelivered mail,
    // deliver it again
    let ev = v["hook_event_name"].as_str().unwrap_or("");
    inbox::on_hook(id, ev, v["prompt"].as_str().unwrap_or(""), agent.delivery() == Delivery::Bell);
    for (name, data) in events {
        event::emit(name, id, data);
        // Delegated: tell the session that delegated it
        inbox::notify(id, name, cfg);
    }
}

/// An event aca fills in for the agent (one it doesn't report itself, e.g. a Codex turn that errored; see
/// [`Agent::unreported_end`]): it goes the same way as a real hook — state, event stream, the notice to the session that
/// delegated it, and inbox redelivery all the same. One with `at` is recorded as happening at that moment: when someone
/// looks only much later, "was it looked at after finishing" compares against when it really finished
pub fn feed(cfg: &config::Config, id: &str, agent: &dyn Agent, v: &Value) {
    let mut events: Events = vec![];
    let at = v["at"].as_u64().unwrap_or_else(paths::now);
    runtime::update(id, |r| events = agent.on_hook(r, v, at));
    settle(cfg, id, agent, v, events);
}

/// `aca retitle <session> <tool> <secs>`: the follow-up a hook leaves. Waits a few seconds, then looks at the title again
pub fn retitle_later(id: &str, tool: &str, secs: u64) -> i32 {
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let (Ok((agent, _)), Some(p)) = (agent::resolve(&config::load(), tool), tmux::find(id)) else { return 0 };
    // Ask with the state exactly as it was after that event: Claude's title checks whether the last event was "finished"
    for (name, data) in retitle(id, |r| agent.title(r, &p.title)) {
        event::emit(name, id, data);
    }
    0
}

/// `aca hook-name`: runs alongside `aca hook` when Claude starts (SessionStart) and on each prompt you send
/// (UserPromptSubmit), handing the session's name to Claude as its own name (`sessionTitle`): the input box, the terminal
/// title, and ListAgents / SendMessage finding sessions by name all use it; without it Claude calls the session
/// `<dir name>-<number>`. Only this one writes to stdout (that one line of JSON), which is why it's separate from
/// `aca hook`.
///
/// Which name and when: see [`name_for`]. If the session can't be read or anything goes wrong, nothing is handed over;
/// the next prompt tries again
pub fn name_cmd() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    // Claude's background sessions inherit the original pane's environment; they aren't this session itself
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

/// The name handed to Claude; `has` is its current name (`session_title` in the hook payload, empty if there is none).
/// - Which: a name a person gave or changed (locked) is used as is; if not locked, Claude's own title is handed over
///   (only there after the first turn finishes; it keeps that name after /clear or a restart)
/// - When: it has no name yet; or it still has the name aca handed it last time and you renamed it in aca. A name set in
///   Claude with /rename is left alone
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
        // A name you gave: handed over right at start
        assert_eq!(name_for(&s("修登录", true), &r("", ""), "").as_deref(), Some("修登录"));
        // No name given: wait until it makes up a title (after the first turn) before handing one over; the dir name from
        // registration doesn't count
        assert_eq!(name_for(&s("aca", false), &r("", ""), ""), None);
        assert_eq!(name_for(&s("aca", false), &r("Fix login\n", ""), "").as_deref(), Some("Fix login"));
        // Already called that: don't hand it over again
        assert_eq!(name_for(&s("修登录", true), &r("", "修登录"), "修登录"), None);
        // Renamed in aca: it still has the name handed over last time, so follow along
        assert_eq!(name_for(&s("修登录 2", true), &r("", "修登录"), "修登录").as_deref(), Some("修登录 2"));
        // Renamed with /rename in Claude: leave it alone
        assert_eq!(name_for(&s("修登录 2", true), &r("", "修登录"), "我起的"), None);
        assert_eq!(name_for(&s("aca", false), &r("Fix login", "Fix login"), "我起的"), None);
        // After /clear the new conversation has no name yet: it keeps the old one
        assert_eq!(name_for(&s("aca", false), &r("Fix login", "Fix login"), "").as_deref(), Some("Fix login"));
    }
}
