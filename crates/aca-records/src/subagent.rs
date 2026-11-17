//! The subagents Claude Code / Codex send out themselves (Claude's Agent tool, Codex's spawn_agent), read from the session's
//! record (`trace`): "is it running" comes from the same record and the same judgement as the `Z` page. Which sessions get
//! read, and how a subagent's preview lines look, are the interface's (aca-tui's `subagent`).

use crate::agent::Agent;
use crate::trace::{Kind, St, Trace, turn_of};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Done,
    Failed,
    /// Interrupted / stopped, or it hadn't finished writing when the session closed
    Stopped,
}

/// One subagent: what a row in the list needs
#[derive(Clone, Debug, PartialEq)]
pub struct Subagent {
    /// Claude's agentId / Codex's thread id
    pub id: String,
    /// Claude: the type (Explore, general-purpose). Codex: its name (Cicero, which is what Codex's own interface calls it too)
    pub kind: String,
    /// The task given to it. Claude: description. Codex: the task name (task_name in 0.144+); older versions use
    /// "type · first line of the instruction that handed it the task"
    pub desc: String,
    pub phase: Phase,
    /// When it started (ms)
    pub t0: i64,
    /// How long from start to end (ms); a running one counts up to the moment of collection
    pub dur: i64,
    /// How many tools it used
    pub tools: usize,
    /// Started by another subagent: how many levels down (0 = started by the session itself)
    pub depth: u8,
    /// The lines drawn in the preview (colored, drawn the same way as a session's screen). Still running: the instruction
    /// given to it, then one line per step, newest at the bottom; the preview shows the bottom part like a screen.
    /// Reported back (`reported`): its report comes first, then the instruction and the steps; the preview shows from the
    /// top, since what you want to see is the start of the conclusion
    pub screen: Vec<String>,
    pub reported: bool,
}

impl Subagent {
    /// "type · description". general-purpose is what you get with no type given and isn't written: with a dozen in a row it
    /// crowds out the descriptions that actually tell them apart
    pub fn label(&self) -> String {
        match (self.kind.as_str(), self.desc.as_str()) {
            ("" | "general-purpose", d) if !d.is_empty() => d.to_string(),
            ("", d) => d.to_string(),
            (k, "") => k.to_string(),
            (k, d) => format!("{k} · {d}"),
        }
    }

    pub fn running(&self) -> bool {
        self.phase == Phase::Running
    }
}

/// How a subagent's preview lines are written (the interface colors them): its record, its phase, the task given to it → the
/// lines, and whether its report comes first (`Subagent::reported`)
pub type Screen<'a> = &'a dyn Fn(&Trace, Phase, &str) -> (Vec<String>, bool);

/// The subagents to list from one record, in the order they started, with the ones each starts right after it. `busy` =
/// the session's current turn is still running (only then does an Agent tool with no result yet count), `alive` = the agent
/// process is still there (background subagents live on it)
pub fn list(t: &Trace, busy: bool, alive: bool, now: i64, screen: Screen) -> Vec<Subagent> {
    let mut out = vec![];
    walk(t, busy, alive, now, None, 0, screen, &mut out);
    out
}

/// Every subagent ever started in a conversation, whichever turn; taken as one second after its last activity, with the
/// process still there. Used by the tests that look at this machine's real records
#[cfg(any(test, feature = "fixtures"))]
pub fn every(t: &Trace, screen: Screen) -> (Vec<Subagent>, i64) {
    fn last(t: &Trace) -> i64 {
        t.subs.iter().map(|s| last(&s.t)).fold(t.last_active(), i64::max)
    }
    let now = last(t) + 1000;
    let mut out = vec![];
    walk(t, false, true, now, Some(true), 0, screen, &mut out);
    (out, now)
}

/// `recent`: the level above has already decided whether it counts as in the last turn (nested ones follow the one that
/// started them)
fn walk(t: &Trace, busy: bool, alive: bool, now: i64, recent: Option<bool>, depth: u8, screen: Screen, out: &mut Vec<Subagent>) {
    let last = t.last_turn();
    for i in 0..t.nodes.len() {
        let Some(s) = t.sub_of(i) else { continue };
        let run = running(t, i, busy, alive, now);
        let rec = recent.unwrap_or_else(|| Some(turn_of(t, i)) == last);
        if run || rec {
            let phase = match (run, t.sub_state(i)) {
                (true, _) => Phase::Running,
                (_, St::Err) => Phase::Failed,
                (_, St::Stopped | St::Denied) => Phase::Stopped,
                (_, St::Ok) if !s.t.finished() => Phase::Stopped,
                (_, St::Ok) => Phase::Done,
            };
            let first = s.t.roots.iter().map(|&r| s.t.nodes[r].t0).find(|&x| x > 0).unwrap_or(t.nodes[i].t0);
            let (kind, desc) = named(t, i, s);
            let (screen, reported) = screen(&s.t, phase, &desc);
            out.push(Subagent {
                id: s.id.clone(),
                screen,
                reported,
                kind,
                desc,
                phase,
                t0: first,
                dur: t.sub_dur(i, run, now),
                tools: s.t.nodes.iter().filter(|n| n.kind == Kind::Tool).count(),
                depth,
            });
        }
        // The ones it starts: it must itself still be running for those without a result to count as running
        walk(&s.t, run, alive && run, now, Some(rec), depth.saturating_add(1), screen, out);
    }
}

/// The two parts written on a row. Claude's record has them ready (type, description). A Codex child thread itself only
/// records its name and type (`Sub`'s desc / kind), so the task given to it must be found elsewhere: the last segment of
/// its `agent_path`, failing that (older versions) the summary of the spawn_agent that started it, failing that just the type
fn named(t: &Trace, tool: usize, s: &crate::trace::Sub) -> (String, String) {
    if t.agent != Agent::Codex {
        return (s.kind.clone(), s.desc.clone());
    }
    let n = &t.nodes[tool];
    let task = match crate::trace_codex::task_of(&s.t) {
        "" if n.name == "Agent" && !n.label.is_empty() => n.label.clone(),
        "" => s.kind.clone(),
        x => x.to_string(),
    };
    (s.desc.clone(), task)
}

/// Whether the subagent of the tool i that started it is still running.
/// The tool has no result yet (running in the foreground): its turn must still be running; after an Esc interrupt the
/// subagent's record stops halfway and must no longer count as running.
/// The tool returned long ago (running in the background, a Codex child thread): look at whether it finished writing
/// itself (`Trace::sub_live`)
fn running(t: &Trace, i: usize, busy: bool, alive: bool, now: i64) -> bool {
    let n = &t.nodes[i];
    if n.t1 == 0 { busy && t.running(i, true) && t.sub_live(i, true, now) } else { n.state == St::Ok && t.sub_live(i, alive, now) }
}
