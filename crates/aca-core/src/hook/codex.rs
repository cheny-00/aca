//! Reconcile a local Codex pane's root status with its optional hooks.

use serde_json::{Value, json};

use super::stands_for;
use crate::agent::{Agent, Events, lifecycle};
use crate::runtime::{self, CodexTurn, Program, Run};

pub(super) fn hook(agent: &dyn Agent, r: &mut Run, v: &Value, pid: u32, now: u64) -> Events {
    let ev = v["hook_event_name"].as_str().unwrap_or("");
    let id = v["turn_id"].as_str().unwrap_or("");
    if ev == "Stop" && r.status == "done" && !id.is_empty() && id == r.prompt_id {
        lifecycle::reply(r, v);
        return lifecycle::metadata(r, v);
    }
    let native = r.codex_turn(pid);
    if let Some(turn) = native {
        if !id.is_empty() && r.ps.as_ref().is_some_and(|p| id == p.previous_turn_id) {
            return vec![];
        }
        match ev {
            "SessionStart" if turn == CodexTurn::Working || v["source"] == "compact" => return lifecycle::metadata(r, v),
            "UserPromptSubmit" if turn == CodexTurn::Working || (!id.is_empty() && id == r.prompt_id) => {
                r.prompt = runtime::clip(v["prompt"].as_str().unwrap_or(""), 400);
                r.prompt_id = id.into();
                return lifecycle::metadata(r, v);
            }
            "Stop" if r.ps.as_ref().is_some_and(|p| !p.state.is_empty()) => {
                if !id.is_empty() && !r.prompt_id.is_empty() && id != r.prompt_id {
                    return vec![];
                }
                if !id.is_empty() {
                    r.prompt_id = id.into();
                }
                lifecycle::reply(r, v);
                return lifecycle::metadata(r, v);
            }
            "Interrupt" if !matches!(r.status.as_str(), "running" | "waiting") => return lifecycle::metadata(r, v),
            _ => {}
        }
        if let Some(ps) = &mut r.ps {
            match ev {
                "UserPromptSubmit" => {
                    ps.previous_turn_id.clone_from(&r.prompt_id);
                    ps.codex_turn = Some(CodexTurn::Ready);
                }
                "Interrupt" | "SessionEnd" | "SessionStart" | "StopFailure" => ps.codex_turn = Some(CodexTurn::Ready),
                "Stop" => ps.codex_turn = Some(CodexTurn::Done),
                _ => {}
            }
        }
    } else if ev == "UserPromptSubmit" && pid != 0 && r.ps.as_ref().is_some_and(|p| p.pid != pid) {
        r.ps = None;
    }
    let events = agent.on_hook(r, v, now);
    if ev == "UserPromptSubmit" {
        r.codex_submit_pid = (pid != 0).then_some(pid);
        r.prompt_id = id.into();
        if native.is_some() {
            r.last.clear();
            r.bg = 0;
        }
    }
    events
}

/// Only working opens a terminal turn. In particular, a permission gate at startup can also say blocked.
pub(super) fn report(agent: &dyn Agent, r: &mut Run, ps: &mut Program) -> Option<(Value, Events)> {
    use CodexTurn::{Done, Ready, Working};
    let previous = r.codex_turn(ps.pid);
    let turn = previous.unwrap_or(Ready);
    if previous.is_none() && ps.state.is_empty() {
        return None;
    }
    if previous.is_none() && r.codex_submit_pid == Some(ps.pid) && matches!(r.status.as_str(), "running" | "waiting") {
        r.last.clear();
        r.bg = 0;
    }
    if let Some(old) = r.ps.as_ref().filter(|p| p.pid == ps.pid) {
        ps.previous_turn_id.clone_from(&old.previous_turn_id);
    }
    if r.ps.as_ref().is_some_and(|p| p.pid != ps.pid) {
        r.status = "idle".into();
        r.clear_ask();
        r.clear_error();
    }
    ps.codex_turn = Some(turn);
    let v = match ps.state.as_str() {
        "working" if turn == Working && r.status == "waiting" => json!({ "hook_event_name": "PostToolUse" }),
        "working" if turn != Working => {
            ps.codex_turn = Some(Working);
            // A submit, or even its Stop, can beat the recorder thread on a fast first turn.
            if r.codex_submit_pid == Some(ps.pid) {
                r.codex_submit_pid = None;
                return None;
            }
            ps.previous_turn_id.clone_from(&r.prompt_id);
            r.last.clear();
            r.bg = 0;
            json!({ "hook_event_name": "UserPromptSubmit" })
        }
        "blocked" if turn == Working => stands_for(r, "blocked", &ps.kind, &ps.msg)?,
        "idle" if turn == Working => {
            ps.codex_turn = Some(Ready);
            stands_for(r, "idle", "", "")?
        }
        "done" if r.status == "done" && (turn == Working || r.codex_submit_pid == Some(ps.pid)) => {
            r.codex_submit_pid = None;
            ps.codex_turn = Some(Done);
            return None;
        }
        "done" if turn == Working => {
            ps.codex_turn = Some(Ready);
            let stop = stands_for(r, "done", "", "")?;
            ps.codex_turn = Some(Done);
            stop
        }
        "error" => {
            ps.codex_turn = Some(Ready);
            let mut failure = stands_for(r, "error", "", &ps.msg)?;
            if ps.msg.is_empty() {
                failure["error_kind"] = "".into();
            }
            failure
        }
        _ => return None,
    };
    // Only a record for this turn can supply its reply or error details. The terminal end stands on its own.
    let needs_details = ps.state == "error" || (ps.state == "done" && r.last.is_empty() && r.bg == 0);
    let record = (needs_details && !r.prompt_id.is_empty())
        .then(|| agent.unreported_end(r))
        .flatten()
        .filter(|end| end["turn_id"] == r.prompt_id && end["hook_event_name"] == v["hook_event_name"]);
    let events = if let Some(end) = record { agent.on_hook(r, &end, ps.at) } else { lifecycle::apply_state(r, &v, ps.at) };
    Some((v, events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent;

    fn terminal(r: &mut Run, pid: u32, state: &str, at: u64) -> Vec<&'static str> {
        let mut ps = Program { pid, state: state.into(), at, ..Default::default() };
        let events = report(agent::builtin("codex").unwrap(), r, &mut ps).map(|(_, ev)| ev).unwrap_or_default();
        r.ps = Some(ps);
        events.into_iter().map(|(name, _)| name).collect()
    }

    #[test]
    fn only_the_current_panes_working_turn_can_finish() {
        let mut r = Run::default();
        for state in ["blocked", "done", "idle"] {
            assert!(terminal(&mut r, 1, state, 10).is_empty());
        }
        assert_eq!(terminal(&mut r, 1, "working", 11), ["turn.started"]);
        assert_eq!(terminal(&mut r, 1, "blocked", 12), ["agent.waiting"]);
        assert!(terminal(&mut r, 1, "working", 13).is_empty());
        assert_eq!((r.turn, r.status.as_str()), (11, "running"));
        assert_eq!(terminal(&mut r, 1, "idle", 14), ["turn.interrupted"]);
        assert!(terminal(&mut r, 1, "done", 15).is_empty());
        assert_eq!(terminal(&mut r, 1, "working", 16), ["turn.started"]);
        assert!(terminal(&mut r, 2, "done", 17).is_empty());
        assert_eq!(r.done, 0);
        assert_eq!(terminal(&mut r, 2, "working", 18), ["turn.started"]);
        assert_eq!(terminal(&mut r, 2, "error", 19), ["turn.failed"]);
        assert!(terminal(&mut r, 2, "done", 20).is_empty());
        assert_eq!((r.turn, r.done, r.status.as_str()), (18, 19, "error"));
    }

    #[test]
    fn codex_hooks_fill_in_the_same_terminal_turn_in_either_order() {
        let mut r = Run::default();
        let a = agent::builtin("codex").unwrap();
        let submit = |id: &str| json!({ "hook_event_name": "UserPromptSubmit", "turn_id": id, "prompt": id });
        let stop = |id: &str| json!({ "hook_event_name": "Stop", "turn_id": id, "last_assistant_message": id, "background_tasks": [{}] });
        terminal(&mut r, 9, "idle", 10);
        assert_eq!(hook(a, &mut r, &submit("first"), 9, 11)[0].0, "turn.started");
        assert!(terminal(&mut r, 9, "working", 12).is_empty());
        assert!(hook(a, &mut r, &stop("first"), 9, 13).is_empty());
        assert_eq!(terminal(&mut r, 9, "done", 14), ["turn.done"]);
        assert_eq!((r.turn, r.done, r.last.as_str(), r.bg), (11, 14, "first", 1));
        assert!(terminal(&mut r, 9, "done", 15).is_empty());
        assert_eq!(terminal(&mut r, 9, "working", 16), ["turn.started"]);
        assert!(hook(a, &mut r, &stop("first"), 9, 16).is_empty());
        assert_eq!((r.prompt.as_str(), r.last.as_str(), r.bg), ("", "", 0));
        let started = json!({ "hook_event_name": "SessionStart", "session_id": "thread", "transcript_path": "/rollout" });
        assert!(hook(a, &mut r, &started, 9, 17).is_empty());
        assert!(hook(a, &mut r, &submit("second"), 9, 18).is_empty());
        assert!(hook(a, &mut r, &stop("first"), 9, 19).is_empty());
        assert_eq!(terminal(&mut r, 9, "done", 20), ["turn.done"]);
        assert!(r.last.is_empty());
        assert!(hook(a, &mut r, &stop("second"), 9, 21).is_empty());
        assert_eq!((r.turn, r.done, r.prompt.as_str(), r.last.as_str()), (16, 20, "second", "second"));
        assert_eq!((r.conv.as_str(), r.transcript.as_str()), ("thread", "/rollout"));
    }

    #[test]
    fn a_fast_hook_turn_is_not_replayed_when_the_recorder_catches_up() {
        let mut r = Run::default();
        let a = agent::builtin("codex").unwrap();
        let start = json!({ "hook_event_name": "UserPromptSubmit", "turn_id": "fast", "prompt": "first" });
        let stop = json!({ "hook_event_name": "Stop", "turn_id": "fast", "last_assistant_message": "answer" });
        assert_eq!(hook(a, &mut r, &start, 42, 10)[0].0, "turn.started");
        assert_eq!(hook(a, &mut r, &stop, 42, 11)[0].0, "turn.done");
        for state in ["idle", "working", "done"] {
            assert!(terminal(&mut r, 42, state, 12).is_empty());
        }
        assert_eq!((r.turn, r.done, r.last.as_str(), r.codex_turn(42)), (10, 11, "answer", Some(CodexTurn::Done)));
        assert_eq!(terminal(&mut r, 42, "working", 13), ["turn.started"]);
        assert_eq!(terminal(&mut r, 42, "done", 14), ["turn.done"]);
    }
}
