//! Subagents in the list: the ones Claude Code / Codex send out themselves (Claude's Agent tool, Codex's spawn_agent).
//!
//! They run inside the parent session's process with no pane of their own, only record files. So what's read here is the
//! `Z` tree (`trace`): "is it running" comes from the same record and the same judgement as the `Z` page; records are only
//! read, no hook is added.
//! - Which sessions get read: for Claude, whether `subagents/` next to the record has anything touched recently; for Codex,
//!   whether the rollout ever started a child thread. Those without aren't parsed (a record can be tens of MB); those with
//!   are read once, and after that each refresh reads only the newly appended part.
//! - Which are listed: those still running (whichever turn started them), plus those started in the last turn (kept even
//!   when finished); earlier ones are seen in `Z`.
//! - The main conversation stopped but a background subagent is still running: the session counts as running, not
//!   "finished, not seen", otherwise you'd go in and find it still waiting on the subagent.
//!
//! How they're drawn in the list and how keys work is in ui.rs (`l` unfolds them into rows, `⏎` goes to its own record in
//! the `Z` page).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use aca_core::runtime::Run;
use aca_core::t;
use memchr::memmem;
use ratatui::style::{Color, Modifier, Style};

use crate::data::{Agent, Session, State};
use crate::theme;
use crate::trace::{Kind, St, Trace, clock, turn_of};

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

/// Subagent records untouched for this long (seconds) are no longer looked at: while the session is idle, finished
/// subagents stay at most this long
const QUIET: u64 = 30 * 60;
/// The preview lists at most this many steps, and at most this many lines each of the instruction / the report
const STEPS: usize = 40;
const TASK_LINES: usize = 6;
const REPLY_LINES: usize = 30;

/// The subagents to list from one record, in the order they started, with the ones each starts right after it. `busy` =
/// the session's current turn is still running (only then does an Agent tool with no result yet count), `alive` = the agent
/// process is still there (background subagents live on it)
pub(crate) fn list(t: &Trace, busy: bool, alive: bool, now: i64) -> Vec<Subagent> {
    let mut out = vec![];
    walk(t, busy, alive, now, None, 0, &mut out);
    out
}

/// Every subagent ever started in a conversation, whichever turn; taken as one second after its last activity, with the
/// process still there. Used by the tests that look at this machine's real records
#[cfg(test)]
pub(crate) fn every(t: &Trace) -> (Vec<Subagent>, i64) {
    fn last(t: &Trace) -> i64 {
        t.subs.iter().map(|s| last(&s.t)).fold(t.last_active(), i64::max)
    }
    let now = last(t) + 1000;
    let mut out = vec![];
    walk(t, false, true, now, Some(true), 0, &mut out);
    (out, now)
}

/// `recent`: the level above has already decided whether it counts as in the last turn (nested ones follow the one that
/// started them)
fn walk(t: &Trace, busy: bool, alive: bool, now: i64, recent: Option<bool>, depth: u8, out: &mut Vec<Subagent>) {
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
        walk(&s.t, run, alive && run, now, Some(rec), depth.saturating_add(1), out);
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

/// Style -> SGR prefix (the preview parses it like a screen, see `ansi::parse_lines`)
fn paint(text: &str, st: Style) -> String {
    let mut codes: Vec<String> = vec![];
    if st.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".into());
    }
    match st.fg {
        Some(Color::Rgb(r, g, b)) => codes.push(format!("38;2;{r};{g};{b}")),
        Some(Color::Indexed(n)) => codes.push(format!("38;5;{n}")),
        Some(c) => {
            const NAMED: [Color; 16] = [
                Color::Black,
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::Gray,
                Color::DarkGray,
                Color::LightRed,
                Color::LightGreen,
                Color::LightYellow,
                Color::LightBlue,
                Color::LightMagenta,
                Color::LightCyan,
                Color::White,
            ];
            if let Some(k) = NAMED.iter().position(|&x| x == c) {
                codes.push(format!("38;5;{k}"));
            }
        }
        None => {}
    }
    if codes.is_empty() { text.to_string() } else { format!("\x1b[{}m{text}\x1b[0m", codes.join(";")) }
}

/// The first max non-empty lines; the rest becomes one "… N more lines" line
fn head_lines(text: &str, max: usize, out: &mut Vec<String>) {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    out.extend(lines.iter().take(max).map(|l| format!("  {l}")));
    if lines.len() > max {
        let more = lines.len() - max;
        let w = aca_core::i18n::noun(more as u64, "line", "lines");
        out.push(paint(&aca_core::tr!("  … 还有 {more} 行", "  … {more} more {w}"), crate::ui::dim()));
    }
}

/// A subagent has no screen: write a few lines from its record as one, the instruction given to it, then one line per step
/// (newest at the bottom). One that has reported back gets that report put first (the returned bool; see
/// `Subagent::screen`). When the instruction can't be read from the record (in Codex 0.144+ messages between threads are
/// encrypted), `task` (the task name) is written instead
fn screen(t: &Trace, phase: Phase, task: &str) -> (Vec<String>, bool) {
    let p = theme::pal();
    let dim = crate::ui::dim();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut head = vec![];
    if phase != Phase::Running {
        let reply = t
            .last_turn()
            .and_then(|r| crate::trace_page::last_said(t, r))
            .map(|c| crate::trace::said(t, c).1.join("\n\n"))
            .unwrap_or_default();
        if !reply.trim().is_empty() {
            head.push(paint(t!("返回的结果", "Its report"), bold));
            head_lines(&reply, REPLY_LINES, &mut head);
            head.push(String::new());
        }
    }
    let reported = !head.is_empty();
    let mut out = head;
    if let Some(&turn) = t.roots.iter().find(|&&r| t.nodes[r].kind == Kind::Turn) {
        // Nobody (an empty speaker) = this turn has no prompt that can be read
        let (who, said) = crate::trace::asked(t, turn);
        let said = if who.is_empty() { task } else { said.as_str() };
        if !said.trim().is_empty() {
            out.push(paint(t!("分配的任务", "Its task"), bold));
            head_lines(said, TASK_LINES, &mut out);
            out.push(String::new());
        }
    }
    let tools: Vec<&crate::trace::Node> = t.nodes.iter().filter(|n| n.kind == Kind::Tool).collect();
    if tools.len() > STEPS {
        let more = tools.len() - STEPS;
        let w = aca_core::i18n::noun(more as u64, "step", "steps");
        out.push(paint(&aca_core::tr!("… 前面还有 {more} 步", "… {more} earlier {w}"), dim));
    }
    let name_w = tools.iter().rev().take(STEPS).map(|n| crate::trace::tool_name(&n.name).chars().count()).max().unwrap_or(0).min(10);
    for n in &tools[tools.len().saturating_sub(STEPS)..] {
        let open = n.t1 == 0 && phase == Phase::Running;
        let mark = match n.state {
            _ if open => paint("●", Style::new().fg(p.green).add_modifier(Modifier::BOLD)),
            St::Err => paint("✕", Style::new().fg(p.red)),
            St::Denied | St::Stopped => paint("■", dim),
            St::Ok => " ".to_string(),
        };
        let name = format!("{:<name_w$}", crate::trace::tool_name(&n.name));
        out.push(format!("{mark} {} {}  {}", paint(&clock(n.t0, t.tz, false), dim), paint(&name, bold), n.label));
    }
    (out, reported)
}

// ---------- Collection: which sessions get read, and remembering what was read ----------

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Claude: `subagents/` next to the record has a record touched since `since`
fn claude_stirred(record: &Path, since: u64) -> bool {
    let Ok(dir) = std::fs::read_dir(record.with_extension("").join("subagents")) else { return false };
    dir.flatten().map(|e| e.path()).any(|p| p.extension().is_some_and(|x| x == "jsonl") && mtime(&p) >= since)
}

/// Whether a Codex rollout ever started a child thread: only looks for those few words, continuing from where the last
/// search got to
#[derive(Default)]
struct Probe {
    path: PathBuf,
    off: u64,
    hit: bool,
}

impl Probe {
    fn spawned(&mut self, record: &Path) -> bool {
        if self.path != record {
            *self = Probe { path: record.to_path_buf(), ..Probe::default() };
        }
        if self.hit {
            return true;
        }
        let Ok(mut f) = std::fs::File::open(record) else { return false };
        let len = f.metadata().map_or(0, |m| m.len());
        // A word may straddle the seam between two reads: read a little further back
        let from = self.off.min(len).saturating_sub(32);
        let mut buf = vec![];
        if f.seek(SeekFrom::Start(from)).is_err() || f.read_to_end(&mut buf).is_err() {
            return false;
        }
        self.off = from + buf.len() as u64;
        self.hit = ["spawn_agent", "SubAgentActivity", "sub_agent_activity"].iter().any(|w| memmem::find(&buf, w.as_bytes()).is_some());
        self.hit
    }
}

/// What was read for one session: the tree its record was read into, plus for Codex how far the probe got; where the
/// record was found when the path the hook reported can't be used (see `record`)
#[derive(Default)]
struct Seen {
    t: Option<Trace>,
    probe: Probe,
    found: Option<(u32, Instant, Option<PathBuf>)>,
}

static SEEN: LazyLock<Mutex<HashMap<String, Seen>>> = LazyLock::new(Default::default);
const FIND_EVERY: Duration = Duration::from_secs(30);

/// The record of the session's current conversation: the one the hook reported, if it's still there; otherwise found the
/// way `Z` does (identify the conversation by process, look in each project folder).
/// The second path isn't only for sessions without aca's hook (adopted and not restarted yet): when Claude enters / leaves
/// a worktree it moves the record along with `subagents/` to the project folder matching the new directory, and the path the hook
/// reported is gone (two sessions on this machine on 2026-10-10).
/// Finding once means checking processes, so what's found is remembered for 30 seconds; if the remembered one is gone
/// (moved again) it's searched for right away
fn record(s: &Session, r: &Run, seen: &mut Seen) -> Option<PathBuf> {
    let p = PathBuf::from(&r.transcript);
    if !r.transcript.is_empty() && p.is_file() {
        return Some(p);
    }
    match &seen.found {
        Some((pid, at, got)) if *pid == s.pid && at.elapsed() < FIND_EVERY && got.as_ref().is_none_or(|p| p.is_file()) => got.clone(),
        _ => {
            let got = crate::trace::find(s).ok();
            seen.found = Some((s.pid, Instant::now(), got.clone()));
            got
        }
    }
}

/// Add subagents to the collected list: registered Claude / Codex sessions whose agent is still there. `run` = the
/// session's runtime state (written by the hook)
pub fn attach(sessions: &mut [Session], run: &dyn Fn(&str) -> Run) {
    let now = now_ms();
    let now_s = (now / 1000) as u64;
    let mut all = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept: HashSet<String> = HashSet::new();
    for s in sessions.iter_mut() {
        let alive = !matches!(s.state, State::Stopped | State::Crashed | State::Dead | State::Exited);
        if s.sub.is_some() || s.deck_id.is_empty() || !alive || !matches!(s.agent, Agent::Claude | Agent::Codex) {
            continue;
        }
        let seen = all.entry(s.name.clone()).or_default();
        kept.insert(s.name.clone());
        let r = run(&s.deck_id);
        let Some(path) = record(s, &r, seen) else { continue };
        let busy = matches!(s.state, State::Running | State::Ask);
        // The current turn is still running: count from its start; an idle one only looks at what was touched recently
        let since = now_s.saturating_sub(QUIET).min(if busy && r.turn > 0 { r.turn } else { u64::MAX });
        let stirred = match s.agent {
            Agent::Codex => seen.probe.spawned(&path) && (busy || mtime(&path) >= since),
            _ => claude_stirred(&path, since),
        };
        if !stirred {
            seen.t = None;
            continue;
        }
        if seen.t.as_ref().is_none_or(|t| t.path != path) {
            seen.t = Trace::open(path, s.agent).ok();
        } else if let Some(t) = seen.t.as_mut() {
            let _ = t.tail();
        }
        let Some(t) = &seen.t else { continue };
        s.subagents = list(t, busy, true, now);
        settle(s, now);
    }
    all.retain(|k, _| kept.contains(k));
}

/// The main conversation stopped but a subagent is still running: counts as running, with the running time counted from
/// the earliest one still running
pub(crate) fn settle(s: &mut Session, now: i64) {
    let first = s.subagents.iter().filter(|a| a.running()).map(|a| a.t0).min();
    if let (Some(t0), State::Idle | State::Unread) = (first, s.state) {
        s.state = State::Running;
        s.ran = ((now - t0).max(0) / 1000) as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::ts_ms;
    use crate::trace_claude::tests::with_subs;

    fn at(ms: u32) -> i64 {
        ts_ms(&crate::trace_claude::tests::at(ms))
    }

    fn names(v: &[Subagent]) -> Vec<(String, Phase)> {
        v.iter().map(|a| (a.label(), a.phase)).collect()
    }

    #[test]
    fn background_ones_run_until_their_last_word_and_the_last_turn_keeps_the_finished() {
        let (p, _, later) = with_subs("list-subs");
        // a1 hasn't finished writing (stopped while running a command)
        let a1 = p.with_extension("").join("subagents/agent-a1.jsonl");
        let full = std::fs::read_to_string(&a1).unwrap();
        std::fs::write(&a1, full.lines().take(4).map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let mut t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        // The main conversation's turn ended long ago (busy = false), the process is still there: a1 runs in the background
        let got = list(&t, false, true, at(6000));
        assert_eq!(got.iter().filter(|a| a.running()).map(|a| a.label()).collect::<Vec<_>>(), ["Explore · 找 width"]);
        assert!(got.iter().any(|a| a.label() == "fork · 接着想" && a.phase == Phase::Done), "{:?}", names(&got));
        // The process is gone: it will never finish writing, so not running
        assert!(list(&t, false, false, at(6000)).iter().all(|a| !a.running()));
        // Preview: the instruction given to it, one line per step, the running step marked
        let a = got.iter().find(|a| a.running()).unwrap();
        let plain: Vec<String> = a.screen.iter().map(|l| crate::ansi::strip(l)).collect();
        assert_eq!(plain[..2], ["分配的任务", "  找出所有 width( 的调用"], "{plain:#?}");
        assert!(plain.iter().any(|l| l.contains("Bash")), "{plain:#?}");
        assert!(!a.reported && !plain.iter().any(|l| l == "返回的结果"));
        // Finished writing, notified, a new turn started: no subagent started in that turn, so the list is empty
        std::fs::write(&a1, &full).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            use std::io::Write;
            writeln!(f, "{l}").unwrap();
        }
        assert!(t.tail().unwrap());
        assert_eq!(names(&list(&t, false, true, at(30_000))), []);
    }

    #[test]
    fn finished_ones_carry_their_report_and_nested_ones_follow_their_parent() {
        let (p, _, _) = with_subs("list-done");
        let t = Trace::open_born(p, Agent::Claude, 0).unwrap();
        let got = list(&t, false, true, at(6000));
        // The first turn is the last turn: both are listed, the one Explore started right after it
        assert_eq!(
            names(&got),
            [
                ("Explore · 找 width".to_string(), Phase::Done),
                ("细看 ui.rs".to_string(), Phase::Done),
                ("fork · 接着想".to_string(), Phase::Done)
            ]
        );
        assert_eq!(got.iter().map(|a| a.depth).collect::<Vec<_>>(), [0, 1, 0]);
        // A finished one: its report comes first (the preview shows from the top), then the instruction and the steps
        let plain: Vec<String> = got[0].screen.iter().map(|l| crate::ansi::strip(l)).collect();
        assert_eq!(plain[..2], ["返回的结果", "  找到 4 处:"], "{plain:#?}");
        let k = plain.iter().position(|l| l == "分配的任务").expect("后面还有派给它的话");
        assert!(got[0].reported && k > 2 && plain[k + 1].contains("找出所有 width( 的调用"), "{plain:#?}");
        assert!(plain[k..].iter().any(|l| l.contains("Bash")), "{plain:#?}");
        assert!(got[0].tools >= 1 && got[0].dur > 0);
    }

    #[test]
    fn codex_child_threads_are_listed_the_same_way() {
        // The parent thread's turn ended (spawn_agent returned long ago), kid1 is still stopped while running a command: counted
        // like a background one, still running; kid2's turn ended
        let (_h, parent) = crate::trace_codex::tests::family("list-kids", false);
        let t = Trace::open_born(parent, Agent::Codex, 0).unwrap();
        let now = ts_ms(&crate::trace_codex::tests::at(9500));
        let got = list(&t, false, true, now);
        // The row is "name · task given to it": older versions use the spawn_agent summary (type · first line of the instruction)
        assert_eq!(
            names(&got),
            [("Euclid · explorer · 看看 b.rs 里的断言".to_string(), Phase::Running), ("Noether · 查 git log".to_string(), Phase::Done)]
        );
        let plain = |a: &Subagent| a.screen.iter().map(|l| crate::ansi::strip(l)).collect::<Vec<_>>();
        let (k1, k2) = (plain(&got[0]), plain(&got[1]));
        assert_eq!(k1[..2], ["分配的任务", "  看看 b.rs 里的断言"], "{k1:#?}");
        assert!(k1.iter().any(|l| l.starts_with("● ") && l.contains("rg -n assert src/b.rs")), "{k1:#?}");
        let at = k2.iter().position(|l| l == "返回的结果").expect("跑完的写它交回来的话");
        assert_eq!(k2[at + 1], "  最近一次提交改了 b.rs", "{k2:#?}");
        // The Codex process is gone: child threads will never finish writing
        assert!(list(&t, false, false, now).iter().all(|a| !a.running()));
    }

    #[test]
    fn an_idle_session_with_a_running_subagent_counts_as_running() {
        let sub = |phase, t0| Subagent {
            reported: false,
            id: "a".into(),
            kind: "Explore".into(),
            desc: "x".into(),
            phase,
            t0,
            dur: 0,
            tools: 0,
            depth: 0,
            screen: vec![],
        };
        let mut s = crate::data::tests::mk_session();
        s.state = State::Unread;
        s.subagents = vec![sub(Phase::Done, 1000), sub(Phase::Running, 5000), sub(Phase::Running, 9000)];
        settle(&mut s, 65_000);
        assert_eq!((s.state, s.ran), (State::Running, 60));
        // One that needs you is left alone; so is one with nothing running
        s.state = State::Ask;
        settle(&mut s, 65_000);
        assert_eq!(s.state, State::Ask);
        s.state = State::Idle;
        s.subagents.retain(|a| !a.running());
        settle(&mut s, 65_000);
        assert_eq!(s.state, State::Idle);
    }

    /// Look at this machine's real records (not run in CI):
    /// `ACA_SUBAGENT_PROBE=<session record>[,<another>…] cargo test -p aca-tui probe_real_records -- --ignored --nocapture`.
    /// Prints both: which the list would show at its last moment (one second after the last activity, process still there);
    /// and all of them including those started in earlier turns (the first few preview lines of each)
    #[test]
    #[ignore]
    fn probe_real_records() {
        let Ok(paths) = std::env::var("ACA_SUBAGENT_PROBE") else { return };
        for p in paths.split(',') {
            let agent = if p.contains("rollout-") { Agent::Codex } else { Agent::Claude };
            let t0 = Instant::now();
            let t = Trace::open(p.into(), agent).unwrap();
            let took = t0.elapsed();
            let stirred = match agent {
                Agent::Codex => Probe::default().spawned(Path::new(p)),
                _ => claude_stirred(Path::new(p), 0),
            };
            let (all, now) = every(&t);
            let t1 = Instant::now();
            let listed = list(&t, false, true, now);
            println!(
                "{p}\n  读 {took:?} · 列 {:?} · 探到起过子代理 {stirred} · 最后一刻列 {} 个 · 一共 {} 个",
                t1.elapsed(),
                listed.len(),
                all.len()
            );
            for a in &all {
                let on = if listed.iter().any(|x| x.id == a.id) { "列" } else { "  " };
                println!("  {on} {:?} {}{} · {}ms · {} 步", a.phase, "↳ ".repeat(a.depth as usize), a.label(), a.dur, a.tools);
            }
            if let Some(a) = all.iter().max_by_key(|a| a.tools) {
                println!("  ---- {} 的预览({} 行)", a.label(), a.screen.len());
                for l in a.screen.iter().take(8).chain(a.screen.iter().rev().take(6).collect::<Vec<_>>().into_iter().rev()) {
                    println!("  | {}", crate::ansi::strip(l).chars().take(150).collect::<String>());
                }
            }
            assert!(all.iter().all(|a| !a.label().is_empty() && !a.id.is_empty()));
        }
    }

    /// Look at this machine's real sessions (not run in CI):
    /// `ACA_SUBAGENT_LIVE=1 cargo test -p aca-tui probe_live_sessions -- --ignored --nocapture`.
    /// When the path the hook reported can't be used, the record is found with `trace::find` (see `record`): sessions with
    /// the hook have both paths, and this checks they find the same one. Those whose hook-recorded one is gone (moved by
    /// Claude entering / leaving a worktree) are listed separately, since that's exactly what finding is for; only both
    /// existing but differing is an error.
    /// Then, pretending nobody has the hook (`Run::default()`), subagents are added again, and the result must match the
    /// one with the hook
    #[test]
    #[ignore]
    fn probe_live_sessions() {
        if std::env::var("ACA_SUBAGENT_LIVE").is_err() {
            return;
        }
        let mut v = crate::data::collect();
        let with_hook: Vec<(String, usize)> =
            v.iter().filter(|s| !s.subagents.is_empty()).map(|s| (s.name.clone(), s.subagents.len())).collect();
        let (mut same, mut moved, mut differ, mut bare) = (0, 0, vec![], 0);
        for s in
            v.iter().filter(|s| s.sub.is_none() && !s.deck_id.is_empty() && s.pid > 0 && matches!(s.agent, Agent::Claude | Agent::Codex))
        {
            let r = aca_core::status::load(&s.deck_id);
            let found = crate::trace::find(s).ok();
            match (r.transcript.is_empty(), found) {
                (true, f) => {
                    bare += 1;
                    println!("  没带 hook:{} → {}", s.title, f.map_or("找不到".into(), |p| p.display().to_string()));
                }
                (false, Some(p)) if p == Path::new(&r.transcript) => same += 1,
                (false, Some(p)) if !Path::new(&r.transcript).is_file() => {
                    moved += 1;
                    println!("  hook 记的那份不在了:{} → 找到 {}", s.title, p.display());
                }
                (false, f) => differ.push((s.title.clone(), r.transcript.clone(), f)),
            }
        }
        println!(
            "在跑的 Claude / Codex 会话:两条路找到同一份记录的 {same} 个,hook 记的不在了、靠找的 {moved} 个,没带 hook 的 {bare} 个,不一样的 {} 个",
            differ.len()
        );
        for d in &differ {
            println!("  不一样:{d:?}");
        }
        for s in &mut v {
            s.subagents.clear();
        }
        attach(&mut v, &|_| Run::default());
        let bare_way: Vec<(String, usize)> =
            v.iter().filter(|s| !s.subagents.is_empty()).map(|s| (s.name.clone(), s.subagents.len())).collect();
        println!("带 hook 时补上子代理的会话 {with_hook:?}\n当成没带 hook 再补一遍 {bare_way:?}");
        assert!(differ.is_empty());
        assert_eq!(with_hook, bare_way);
    }

    #[test]
    fn codex_probe_finds_a_spawn_word_across_two_reads() {
        let dir = std::env::temp_dir().join(format!("aca-subagent-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rollout.jsonl");
        std::fs::write(&p, "{\"type\":\"x\"}\n{\"name\":\"spawn_").unwrap();
        let mut probe = Probe::default();
        assert!(!probe.spawned(&p));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        writeln!(f, "agent\"}}").unwrap();
        assert!(probe.spawned(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
