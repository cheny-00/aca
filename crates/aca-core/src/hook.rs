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

mod codex;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent::{self, Agent, Delivery, Events};
use crate::runtime::{CodexTurn, Program, Run};
use crate::store::{Session, Store};
use crate::{config, event, inbox, paths, proc, runtime, tmux};

/// The turn is over (finished / interrupted): the title should be there by now, and check whether anyone is attached
fn turn_ended(v: &Value) -> bool {
    matches!(v["hook_event_name"].as_str(), Some("Stop" | "Interrupt"))
}

/// Records the title the agent made up (`pick` says which one it is now); if the name isn't locked, renames the tmux
/// session to match. Returns the updated run and the events to send
pub(crate) fn retitle(id: &str, pick: impl FnOnce(&Run) -> Option<String>) -> (Run, Events) {
    let mut title = None;
    let (_, after) = runtime::update(id, |r| {
        // A name set by a person is still recorded, just not used to rename the session (so it's ready the moment the lock is
        // released)
        if let Some(t) = pick(r).filter(|t| *t != r.auto_title) {
            r.auto_title = t.clone();
            title = (!r.locked).then_some(t);
        }
    });
    let events = match title {
        Some(t) => {
            let name = tmux::sync_name(id, &t).unwrap_or_default();
            vec![("session.titled", json!({ "title": t, "tmux": name }))]
        }
        None => vec![],
    };
    (after, events)
}

pub fn run(tool: &str) -> i32 {
    handle(tool, None);
    0
}

type RecordLimits = fn(&str, &agent::AgyLimits) -> bool;

/// agy's title command must print one line, even when the input cannot be used.
pub fn title_cmd(tool: &str, record: RecordLimits) -> i32 {
    println!("{}", handle(tool, Some(record)).unwrap_or_default());
    0
}

/// Check the independent quota and lifecycle parts of a redraw before loading config or asking tmux.
fn redraw(input: &str, r: &Run, record: impl FnOnce(u64, &agent::AgyLimits)) -> Option<(runtime::AgyTitle, bool)> {
    let v: Value = serde_json::from_str(input).ok()?;
    let limits = agent::AgyLimits::from_title(&v);
    let snapshot: runtime::AgyTitle = serde_json::from_value(v).ok()?;
    if let Some(limits) = limits {
        let fingerprint = limits.fingerprint();
        if r.agy_quota != Some(fingerprint) {
            record(fingerprint, &limits);
        }
    }
    let changed = r.agy.as_ref() != Some(&snapshot);
    Some((snapshot, changed))
}

fn handle(tool: &str, record: Option<RecordLimits>) -> Option<String> {
    // Read it all, used or not
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let id = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty())?;
    // Claude's background sessions inherit the original pane's environment; they aren't this session itself
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return None;
    }
    let title = record.is_some();
    let snapshot = if let Some(record) = record {
        if tool != "agy" {
            return None;
        }
        let r = runtime::load(&id);
        let (snapshot, changed) = redraw(&input, &r, |fingerprint, limits| {
            runtime::update(&id, |r| {
                if r.agy_quota != Some(fingerprint) && record(&id, limits) {
                    r.agy_quota = Some(fingerprint);
                }
            });
        })?;
        if !changed {
            return Some(snapshot.pane_title(&r));
        }
        Some(snapshot)
    } else {
        None
    };
    let cfg = config::load();
    let (agent, _) = agent::resolve(&cfg, tool).ok()?;
    let agent: &dyn Agent = &*agent;
    // To see what payloads look like: after `touch ~/.local/state/aca/hook-debug`, every event is logged verbatim to
    // hook-debug.log
    let debug = paths::state_dir().join("hook-debug");
    if debug.exists() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(debug.with_extension("log")) {
            let _ = writeln!(f, "{id}\t{tool}\t{}", input.trim());
        }
    }
    let v = if title { Value::Null } else { serde_json::from_str::<Value>(&input).ok()? };
    let now = paths::now();
    let may_end = turn_ended(&v) || snapshot.as_ref().is_some_and(|s| s.agent_state == "idle");
    // One tmux query gets the current relay PID, attachment and title.
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let (pid, attached, pane_title) = match !pane.is_empty() && (may_end || agent.title_in_pane()) {
        true => tmux::run(&["display-message", "-p", "-t", &pane, "#{pane_pid}\t#{session_attached}\t#{pane_title}"])
            .ok()
            .and_then(|o| {
                let mut fields = o.trim_end_matches('\n').splitn(3, '\t');
                Some((fields.next()?.parse().ok()?, fields.next()? != "0", fields.next()?.to_string()))
            })
            .unwrap_or_default(),
        false if agent.name() == "codex" => tmux::find(&id).map(|p| (p.pid, p.attached > 0, p.title)).unwrap_or_default(),
        false => Default::default(),
    };
    let mut batch = vec![];
    // Claude hands every hook its own inbox socket (each session its own, never a parent's): kept so other sessions can be
    // told where to send to (`aca show --json`'s `peer`)
    let peer = std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").ok().filter(|p| !p.is_empty() && agent.name() == "claude");
    let (before, after) = runtime::update(&id, |r| {
        if !r.current_shift() {
            return;
        }
        batch = match snapshot {
            Some(s) => {
                if r.agy.is_none() {
                    r.named = Store::open().and_then(|st| st.get(&id)).map(|s| s.title).unwrap_or_default();
                }
                s.apply(r, &paths::home(), now)
            }
            None => {
                let events = if agent.name() == "codex" { codex::hook(agent, r, &v, pid, now) } else { agent.on_hook(r, &v, now) };
                vec![(v, events)]
            }
        };
        if attached && batch.iter().any(|(v, _)| turn_ended(v)) {
            r.seen = now;
        }
        if let Some(p) = &peer {
            r.peer.clone_from(p);
        }
    });
    if !after.current_shift() {
        return None;
    }
    let (titled, title_events) = retitle(&id, |r| agent.title(r, &pane_title));
    if let Some((_, events)) = batch.last_mut() {
        events.extend(title_events);
    } else {
        for (name, data) in title_events {
            event::emit(name, &id, data);
        }
    }
    if batch.iter().any(|(v, _)| matches!(v["hook_event_name"].as_str(), Some("Stop" | "StopFailure"))) {
        take_in(&id, agent, now);
    }
    let ended = batch.iter().any(|(v, _)| turn_ended(v));
    // agy generates a title during the first turn, which can run for minutes without another redraw.
    let delay = if title && batch.iter().any(|(v, _)| v["hook_event_name"] == "UserPromptSubmit") {
        Some("8")
    } else if ended && !title {
        Some("3")
    } else {
        None
    };
    if let Some(secs) = delay.filter(|_| titled.auto_title.is_empty()) {
        let _ = proc::detach(
            Command::new(agent::self_exe())
                .args(["retitle", &id, tool, secs])
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
    let confirmation = after.agy_confirmation != before.agy_confirmation;
    for (v, mut events) in batch {
        if confirmation {
            events.retain(|(name, data)| !(*name == "agent.waiting" && data["kind"] == "permission"));
        }
        settle(&cfg, &id, agent, &v, events);
    }
    if confirmation {
        let token = after.agy_confirmation;
        if pane.is_empty()
            || proc::detach(
                Command::new(agent::self_exe())
                    .args(["agy-question", &id, &pane, &token.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .is_err()
        {
            finish_agy_question(&id, token, None);
        }
    }
    titled.agy.as_ref().map(|s| s.pane_title(&titled))
}

fn pending_agy_question(r: &Run, token: u64) -> bool {
    token != 0
        && r.agy_confirmation == token
        && r.status == "waiting"
        && r.ask_kind == "permission"
        && r.ask.is_empty()
        && r.agy.as_ref().is_some_and(|s| s.tool_confirmation_pending == Some(true))
}

fn command_hash(command: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    command.hash(&mut hasher);
    hasher.finish()
}

fn agy_question(r: &mut Run, token: u64, command: Option<&str>) -> Option<Value> {
    pending_agy_question(r, token).then(|| {
        if let Some(cmd) = command {
            r.agy_previous_command = command_hash(cmd);
        }
        json!({
            "hook_event_name":"Notification", "notification_type":"permission_prompt",
            "message":command.unwrap_or_else(|| crate::t!("Antigravity 请求操作许可", "Antigravity is requesting permission"))
        })
    })
}

fn finish_agy_question(id: &str, token: u64, command: Option<&str>) {
    // Recheck under the run lock after capture: this box may already be answered or replaced.
    feed_if(&config::load(), id, &agent::agy::Agy, |r| agy_question(r, token, command));
}

/// agy can finish drawing the box after its title command exits. Only the detached reader waits for the screen.
pub fn agy_question_later(id: &str, pane: &str, token: u64) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut cleared = false;
    loop {
        let r = runtime::load(id);
        if !pending_agy_question(&r, token) {
            break;
        }
        if let Some(cmd) = drawn_agy_command(agent::agy::Agy.screen(&tmux::capture(pane)), r.agy_previous_command, &mut cleared) {
            finish_agy_question(id, token, Some(&cmd));
            break;
        }
        if Instant::now() >= deadline {
            finish_agy_question(id, token, None);
            break;
        }
        std::thread::sleep(Duration::from_millis(75));
    }
    0
}

fn drawn_agy_command(seen: crate::screen::Seen, previous: u64, cleared: &mut bool) -> Option<String> {
    match seen {
        crate::screen::Seen::Ask(command) if !command.is_empty() => (*cleared || previous != command_hash(&command)).then_some(command),
        _ => {
            *cleared = true;
            None
        }
    }
}

/// A turn ended: what the conversation record says about it ([`Agent::turn_record`]). Messages other sessions passed it
/// through the agent's own channel go into its inbox as delivered; a turn opened by one of them or by a background task's
/// notice gets what you last said back as `prompt`. The first look starts from the current turn (a little before it, for the
/// message that opened it)
fn take_in(id: &str, agent: &dyn Agent, now: u64) {
    let r = runtime::load(id);
    let since = match (r.native_seen, r.turn) {
        (0, 0) => now,
        (0, turn) => turn.saturating_sub(5),
        (seen, _) => seen,
    };
    let turn = agent.turn_record(&r, since);
    runtime::update(id, |r| {
        if let Some(said) = &turn.said {
            r.prompt = runtime::clip(said, 400);
        }
        r.native_seen = now;
    });
    inbox::native(id, turn.native);
}

/// The second half of an event: inbox, event stream, notices
fn settle(cfg: &config::Config, id: &str, agent: &dyn Agent, v: &Value, events: Events) {
    // The receiving end: the text aca typed in is marked delivered; if the turn finished and there's still undelivered mail,
    // deliver it again
    let ev = v["hook_event_name"].as_str().unwrap_or("");
    if !matches!(ev, "Stop" | "StopFailure") || events.iter().any(|(name, _)| matches!(*name, "turn.done" | "turn.failed")) {
        inbox::on_hook(id, ev, v["prompt"].as_str().unwrap_or(""), agent.delivery() == Delivery::Bell);
    }
    for (name, mut data) in events {
        if matches!(name, "turn.done" | "turn.failed" | "agent.waiting")
            && let Ok(routers) = Store::open().and_then(|st| st.task_routers(id, None))
            && !routers.is_empty()
        {
            data["routers"] = json!(routers);
        }
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
    feed_if(cfg, id, agent, |_| Some(v.clone()));
}

pub(crate) fn feed_if(cfg: &config::Config, id: &str, agent: &dyn Agent, pick: impl FnOnce(&mut Run) -> Option<Value>) -> Run {
    let mut turn = None;
    let (_, after) = runtime::update(id, |r| {
        if let Some(v) = pick(r) {
            let events = agent.on_hook(r, &v, v["at"].as_u64().unwrap_or_else(paths::now));
            if let Some(ps) = r.ps.as_mut().filter(|p| p.state.is_empty() && p.codex_turn.is_some()) {
                ps.codex_turn = Some(if r.status == "done" { CodexTurn::Done } else { CodexTurn::Ready });
            }
            turn = Some((v, events));
        }
    });
    if let Some((v, events)) = turn {
        settle(cfg, id, agent, &v, events);
    }
    after
}

/// Records a root report and its local Codex turn under the same lock as hooks.
pub(crate) fn record(id: &str, mut ps: Program) {
    let cfg = config::load();
    let s = Store::open().and_then(|st| st.get(id)).ok();
    let agent = s.as_ref().and_then(|s| agent::of(&cfg, s).ok().map(|(a, _)| a));
    let caged = s.as_ref().is_some_and(|s| crate::sandbox::cage_of_session(&cfg, s).is_some());
    let local = agent.as_deref().filter(|a| !caged && a.name() == "codex");
    let mut turn = None;
    let (before, after) = runtime::update(id, |r| {
        if let Some(a) = local {
            let Some(pane) = tmux::find(id).filter(|p| !p.dead && p.pid == ps.pid) else { return };
            turn = codex::report(a, r, &mut ps);
            if pane.attached > 0 && turn.as_ref().is_some_and(|(v, _)| turn_ended(v)) {
                r.seen = ps.at;
            }
        }
        r.ps = Some(ps);
    });
    if before.ps == after.ps {
        return;
    }
    let Some(ps) = after.ps else { return };
    event::emit("agent.status", id, json!({ "state": ps.state, "kind": ps.kind, "msg": ps.msg }));
    if let (Some(a), Some((v, events))) = (local, turn) {
        settle(&cfg, id, a, &v, events);
    } else if caged && let (Some(s), Some(a)) = (s, agent) {
        sandbox_report(&cfg, &s, &*a, &ps.state, &ps.kind, &ps.msg);
    }
}

/// Inside a sandbox, the relay also supplies the hook events that cannot leave it.
pub fn reported(id: &str, state: &str, kind: &str, msg: &str) {
    let cfg = config::load();
    let Ok(s) = Store::open().and_then(|st| st.get(id)) else { return };
    if crate::sandbox::cage_of_session(&cfg, &s).is_none() {
        return;
    }
    let Ok((agent, _)) = agent::of(&cfg, &s) else { return };
    sandbox_report(&cfg, &s, &*agent, state, kind, msg);
}

fn sandbox_report(cfg: &config::Config, s: &Session, agent: &dyn Agent, state: &str, kind: &str, msg: &str) {
    let id = &s.id;
    let r = runtime::load(id);
    let Some(v) = stands_for(&r, state, kind, msg) else { return };
    let v = match v["hook_event_name"].as_str() {
        // The reply is in the conversation record, which only it writes: found in the sandbox's config directory
        Some("Stop") => {
            let r = crate::session::inside(cfg, s, &r);
            json!({ "hook_event_name": "Stop", "last_assistant_message": agent.last_reply(&r).unwrap_or_default() })
        }
        _ => v,
    };
    feed_if(cfg, id, agent, |r| stands_for(r, state, kind, msg).map(|_| v));
}

/// The hook event a report stands for, given the state the hook side has recorded so far ([`reported`]). Going by that state
/// rather than the previous report, a turn is opened and closed once even when the screen filled the end in first
fn stands_for(r: &Run, state: &str, kind: &str, msg: &str) -> Option<Value> {
    let open = matches!(r.status.as_str(), "running" | "waiting");
    Some(match state {
        // Approved: on with the same turn
        "working" if r.status == "waiting" => json!({ "hook_event_name": "PostToolUse" }),
        "working" if r.status != "running" => json!({ "hook_event_name": "UserPromptSubmit", "prompt": r.prompt }),
        "done" if open => json!({ "hook_event_name": "Stop" }),
        // Esc, or a permission refused (Codex's "idle" after a finished turn is only you typing)
        "idle" if open => json!({ "hook_event_name": "Interrupt" }),
        "blocked" if r.status != "waiting" => json!({
            "hook_event_name": "Notification",
            "notification_type": if kind == "permission" { "permission_prompt" } else { "elicitation_dialog" },
            "message": msg,
        }),
        "error" if r.status != "error" && msg.is_empty() => json!({ "hook_event_name": "StopFailure" }),
        "error" if r.status != "error" => json!({ "hook_event_name": "StopFailure", "error": msg }),
        "exited" if r.status != "ended" => json!({ "hook_event_name": "SessionEnd", "reason": "exit" }),
        _ => return None,
    })
}

/// `aca retitle <session> <tool> <secs>`: the follow-up a hook leaves. Waits a few seconds, then looks at the title again
pub fn retitle_later(id: &str, tool: &str, secs: u64) -> i32 {
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let (Ok((agent, _)), Some(p)) = (agent::resolve(&config::load(), tool), tmux::find(id)) else { return 0 };
    // Ask with the state exactly as it was after that event: Claude's title checks whether the last event was "finished"
    let (_, events) = retitle(id, |r| agent.title(r, &p.title));
    for (name, data) in events {
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
    fn agy_redraw_checks_quota_and_lifecycle_independently() {
        let mut r = Run::default();
        let mut v = json!({"agent_state":"working","quota":{"gemini-5h":{
            "remaining_fraction":0.9871378, "reset_time":"2026-11-25T08:26:00Z", "reset_in_seconds":17716}}});
        let mut check = |v: &Value, expected| {
            let mut saved = None;
            let (title, lifecycle) = redraw(&v.to_string(), &r, |hash, _| saved = Some(hash)).unwrap();
            assert_eq!((lifecycle, saved.is_some()), expected);
            r.agy = Some(title);
            r.agy_quota = saved.or(r.agy_quota);
        };
        check(&v, (true, true));
        v["quota"]["gemini-5h"]["reset_in_seconds"] = json!(17000);
        check(&v, (false, false));
        v["quota"]["gemini-5h"]["remaining_fraction"] = json!(0.85);
        check(&v, (false, true));
        v["agent_state"] = json!("idle");
        v["quota"]["gemini-5h"]["remaining_fraction"] = json!(0.75);
        check(&v, (true, true));
        check(&v, (false, false));
    }

    #[test]
    fn a_delayed_command_belongs_to_one_permission_box_even_in_the_same_second() {
        let mut r = Run::default();
        let agy = agent::agy::Agy;
        let waiting = json!({"agent_state":"tool_use", "tool_confirmation_pending":true});
        let working = json!({"agent_state":"working"});
        agy.on_hook(&mut r, &waiting, 123);
        let first = r.agy_confirmation;
        let question = agy_question(&mut r, first, Some("pwd")).unwrap();
        assert_eq!(agy.on_hook(&mut r, &question, 123), [("agent.waiting", json!({"ask":"pwd", "kind":"permission", "options":[]}))]);
        assert_eq!(r.ask, "pwd");
        assert!(agy_question(&mut r, first, Some("pwd")).is_none());
        agy.on_hook(&mut r, &working, 123);
        assert!(agy_question(&mut r, first, None).is_none());
        agy.on_hook(&mut r, &waiting, 123);
        assert!(agy_question(&mut r, first, Some("pwd")).is_none());
        let second = r.agy_confirmation;
        let box_for = |cmd: &str| crate::screen::Seen::Ask(cmd.to_owned());
        let mut cleared = false;
        assert!(drawn_agy_command(box_for("pwd"), r.agy_previous_command, &mut cleared).is_none());
        let cmd = drawn_agy_command(box_for("rm notes.txt"), r.agy_previous_command, &mut cleared).unwrap();
        assert!(drawn_agy_command(crate::screen::Seen::Unsure, r.agy_previous_command, &mut cleared).is_none());
        assert_eq!(drawn_agy_command(box_for("pwd"), r.agy_previous_command, &mut cleared).as_deref(), Some("pwd"));
        let question = agy_question(&mut r, second, Some(&cmd)).unwrap();
        assert_eq!(agy.on_hook(&mut r, &question, 123)[0].1["ask"], "rm notes.txt");
        assert_eq!(r.ask, "rm notes.txt");
        agy.on_hook(&mut r, &working, 123);
        agy.on_hook(&mut r, &waiting, 123);
        let token = r.agy_confirmation;
        let timeout = agy_question(&mut r, token, None).unwrap();
        assert!(!agy.on_hook(&mut r, &timeout, 125)[0].1["ask"].as_str().unwrap().is_empty());
    }

    #[test]
    fn a_sandboxed_agents_own_reports_open_and_close_each_turn_once() {
        let at = |status: &str| Run { status: status.into(), prompt: "修登录".into(), ..Default::default() };
        let ev = |status: &str, state: &str, kind: &str, msg: &str| {
            stands_for(&at(status), state, kind, msg).map(|v| v["hook_event_name"].as_str().unwrap_or("").to_string())
        };
        let ev = |status: &str, state: &str| ev(status, state, "", "");
        // Started, idle: nothing; a turn opens once, an approval carries it on
        assert_eq!((ev("", "idle"), ev("", "done")), (None, None));
        assert_eq!(ev("idle", "working").as_deref(), Some("UserPromptSubmit"));
        assert_eq!(ev("done", "working").as_deref(), Some("UserPromptSubmit"));
        assert_eq!(ev("running", "working"), None);
        assert_eq!(ev("waiting", "working").as_deref(), Some("PostToolUse"));
        // It closes once: a turn the screen already closed isn't closed again; Codex's idle after done is you typing
        assert_eq!(ev("running", "done").as_deref(), Some("Stop"));
        assert_eq!(ev("waiting", "done").as_deref(), Some("Stop"));
        assert_eq!((ev("done", "done"), ev("done", "idle")), (None, None));
        assert_eq!(ev("running", "idle").as_deref(), Some("Interrupt"));
        assert_eq!((ev("running", "error").as_deref(), ev("error", "error")), (Some("StopFailure"), None));
        assert_eq!((ev("done", "exited").as_deref(), ev("ended", "exited")), (Some("SessionEnd"), None));
        // Asking: a permission prompt or a question to you, with its own line
        let v = stands_for(&at("running"), "blocked", "permission", "approve Bash: make").unwrap();
        assert_eq!((v["notification_type"].as_str(), v["message"].as_str()), (Some("permission_prompt"), Some("approve Bash: make")));
        let v = stands_for(&at("running"), "blocked", "question", "").unwrap();
        assert_eq!(v["notification_type"], "elicitation_dialog");
        assert_eq!(ev("waiting", "blocked"), None);
        // The prompt it was given stays the prompt
        assert_eq!(stands_for(&at("idle"), "working", "", "").unwrap()["prompt"], "修登录");
    }

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
