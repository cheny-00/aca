//! A session's record: assembles a conversation's record file into a tree — turn → each model call in that turn → the
//! tools the call started, like a Langfuse / Phoenix trace. Reads only local record files; no network, no file changes. The
//! interface's `Z` page lays it out (aca-tui's `trace` and `trace_page`).
//!
//! Understands Claude (`projects/<dir>/<session id>.jsonl`, parsed in `trace_claude`) and Codex (`sessions/…/rollout-*.jsonl`,
//! `trace_codex`). Nodes hold only what drawing one line needs; bodies (the full prompt, replies, tool input and output) aren't
//! kept around but read back from the file at the recorded offsets when viewed; each tool reads them back into the same shape
//! (prompt, thinking and text, the tool's `Io`).
//! A running session: when the file grows only the new part is read (a half-written last line is read next time).
//!
//! Subagents (Claude's `subagents/agent-*.jsonl`, Codex's child threads) are separate records, each read into its own tree
//! (`Sub`) and hung under the tool that started it (Agent / spawn_agent); its cost counts toward that turn.
//!
//! Cost is measured as in `$`: official API prices in US dollars (`usage::call_cost`); history copied in at the start of a
//! resume / fork / child thread isn't counted, rolled-back turns still are (the money was spent), they just get no number.
//! Durations: a turn's is the one the record states (Claude's turn_duration, Codex's duration_ms), a tool's is its result line
//! minus the line that started it (a Codex command uses its stated one when there is one),
//! a call's is estimated from timestamps (the record has no first token or retries).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aca_core::i18n::Tr;
use aca_core::{t, tr};

use crate::agent::Agent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A turn: from one prompt (or slash command, or a background task's completion notice) to the end of that turn
    Turn,
    /// One model call
    Call,
    /// A tool the call started
    Tool,
    /// API error (Claude records it as a fake reply): not counted as a call
    Fail,
    /// A one-line marker: compaction
    Mark,
    /// The turns copied from the previous conversation on resume / fork, folded under this line, not counted
    Copied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum St {
    #[default]
    Ok,
    Err,
    /// Permission denied
    Denied,
    /// Interrupted
    Stopped,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<usize>,
    pub kids: Vec<usize>,
    /// Milliseconds; t1 = 0: not finished yet (or the record doesn't say it finished)
    pub t0: i64,
    pub t1: i64,
    /// The duration is stated in the record, not estimated by subtracting timestamps
    pub exact: bool,
    /// A tool's tool_use id / a call's message.id (used to find the original text for details)
    pub id: String,
    /// Tool name / slash command name
    pub name: String,
    /// One-line summary (call: the first sentence of its text)
    pub label: String,
    /// Call: the first sentence of its thinking (only if the record kept the thinking)
    pub thought: String,
    /// Input (excluding cache), 5-minute cache write, 1-hour cache write, cache read, output (as in usage.rs)
    pub tok: [u64; 5],
    /// Thinking within the output
    pub think: u64,
    pub searches: u64,
    /// Fast mode, US-only inference (both cost extra)
    pub fast: bool,
    pub us: bool,
    /// Index into Trace.models
    pub model: usize,
    pub effort: String,
    pub stop: String,
    pub state: St,
    pub copied: bool,
    /// A turn: later rolled back (Codex's thread_rolled_back): drawn gray, unnumbered, cost still counted
    pub rolled: bool,
    /// A tool: the subagent it started (index into Trace.subs)
    pub sub: Option<usize>,
    /// Time spent in hooks (milliseconds)
    pub hook: u64,
    /// The lines it came from: (offset, length). Tool: the first is the reply that started it, the second the result
    pub src: Vec<(u64, u32)>,
}

impl Node {
    fn new(kind: Kind, parent: Option<usize>, t0: i64) -> Node {
        Node {
            kind,
            parent,
            kids: vec![],
            t0,
            t1: 0,
            exact: false,
            id: String::new(),
            name: String::new(),
            label: String::new(),
            thought: String::new(),
            tok: [0; 5],
            think: 0,
            searches: 0,
            fast: false,
            us: false,
            model: 0,
            effort: String::new(),
            stop: String::new(),
            state: St::Ok,
            copied: false,
            rolled: false,
            sub: None,
            hook: 0,
            src: vec![],
        }
    }

    /// Context: how much this request sent in (input + cache write + cache read)
    pub fn context(&self) -> u64 {
        self.tok[0] + self.tok[1] + self.tok[2] + self.tok[3]
    }
}

/// The tree of one conversation
pub struct Trace {
    pub path: PathBuf,
    pub agent: Agent,
    pub nodes: Vec<Node>,
    /// Top level: turns, markers, the copied block, in order
    pub roots: Vec<usize>,
    pub models: Vec<String>,
    /// The directory it last recorded (paths in tool summaries are written relative to it)
    pub cwd: String,
    /// Local UTC offset (seconds), for writing times
    pub tz: i64,
    /// How far it has read (up to a whole line)
    pub off: u64,
    /// When the file was created (ms, 0 = unknown): conversation lines at the start older than this were copied in on resume
    pub born: i64,
    /// Still inside the copied part at the start of the file
    pub lead: bool,
    // ---- Mid-parse state (carried across reads of the new part) ----
    /// The current turn
    pub turn: Option<usize>,
    /// message.id → call
    pub calls: HashMap<String, usize>,
    /// tool_use id → tool
    pub tools: HashMap<String, usize>,
    /// Each line's uuid → time: a call is timed from the line before its first block
    pub at: HashMap<u64, i64>,
    /// Codex mid-parse state
    pub cx: crate::trace_codex::State,
    /// Subagents started in this record
    pub subs: Vec<Sub>,
    /// This is a subagent's record (a Claude subagent doesn't look further for a subagents directory: nested ones are split out by
    /// the top record via parentAgentId)
    pub is_sub: bool,
    /// Subagent ids already looked up (Claude: the top record keeps every level's; Codex: each keeps its own)
    pub sub_ids: std::collections::HashSet<String>,
    /// Subagents whose record isn't found yet: (tool, subagent id, tries). The file may not be written yet when it starts; look again
    /// on the next read of the new part
    pub want: Vec<(usize, String, u8)>,
    /// Background tasks' completion notices: task id (= subagent id) → outcome (done / failed / stopped)
    pub done: HashMap<String, St>,
}

/// A subagent: the tree read from its own record
pub struct Sub {
    /// Claude's agentId / Codex's thread id
    pub id: String,
    /// Type (Explore, general-purpose / Codex's explorer, worker)
    pub kind: String,
    /// Description (Claude's description / Codex's name)
    pub desc: String,
    pub t: Trace,
}

impl Trace {
    pub fn new(path: PathBuf, agent: Agent) -> Trace {
        Trace {
            path,
            agent,
            nodes: vec![],
            roots: vec![],
            models: vec![String::new()],
            cwd: String::new(),
            tz: 0,
            off: 0,
            born: 0,
            lead: true,
            turn: None,
            calls: HashMap::new(),
            tools: HashMap::new(),
            at: HashMap::new(),
            cx: crate::trace_codex::State::default(),
            subs: vec![],
            is_sub: false,
            sub_ids: std::collections::HashSet::new(),
            want: vec![],
            done: HashMap::new(),
        }
    }

    /// Read a subagent's record. For Codex, pass the parent thread's turns (the parent history copied in at the start is recognized
    /// by them)
    pub fn open_sub(path: PathBuf, agent: Agent, parent_turns: Option<std::collections::HashSet<String>>) -> std::io::Result<Trace> {
        let mut t = Trace::new(path, agent);
        t.tz = crate::usage::local_offset();
        // A subagent's record never starts with copied history (none of the 175 on this machine do): don't use the file's creation
        // time to find copied lines
        t.is_sub = true;
        t.cx.parent_turns = parent_turns;
        t.tail()?;
        Ok(t)
    }

    /// Read a whole record
    pub fn open(path: PathBuf, agent: Agent) -> std::io::Result<Trace> {
        let born = born(&path);
        Trace::open_born(path, agent, born)
    }

    pub fn open_born(path: PathBuf, agent: Agent, born: i64) -> std::io::Result<Trace> {
        let mut t = Trace::new(path, agent);
        t.tz = crate::usage::local_offset();
        t.born = born;
        t.tail()?;
        Ok(t)
    }

    /// Read the new part (subagents' too); returns true if anything is new. A half-written last line waits for next time
    pub fn tail(&mut self) -> std::io::Result<bool> {
        let mut grew = self.tail_file()?;
        for s in &mut self.subs {
            grew |= s.t.tail().unwrap_or(false);
        }
        grew |= match self.agent {
            Agent::Claude => crate::trace_claude::find_subs(self),
            Agent::Codex => crate::trace_codex::find_subs(self),
            _ => false,
        };
        Ok(grew)
    }

    fn tail_file(&mut self) -> std::io::Result<bool> {
        let mut f = File::open(&self.path)?;
        let len = f.metadata()?.len();
        if len < self.off {
            // The file got shorter (replaced): read from the start
            let (path, agent, tz, is_sub) = (self.path.clone(), self.agent, self.tz, self.is_sub);
            let parent_turns = self.cx.parent_turns.take();
            *self = Trace::new(path, agent);
            self.tz = tz;
            self.born = if is_sub { 0 } else { born(&self.path) };
            self.is_sub = is_sub;
            self.cx.parent_turns = parent_turns;
        }
        if len == self.off {
            return Ok(false);
        }
        f.seek(SeekFrom::Start(self.off))?;
        let mut r = BufReader::with_capacity(1 << 16, f);
        let mut buf = Vec::with_capacity(1 << 16);
        let start = self.off;
        loop {
            buf.clear();
            let n = r.read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() != Some(&b'\n') {
                break;
            }
            let at = self.off;
            self.off += n as u64;
            let line = &buf[..n - 1];
            match self.agent {
                _ if line.is_empty() => {}
                Agent::Claude => crate::trace_claude::line(self, line, at),
                Agent::Codex => crate::trace_codex::line(self, line, at),
                _ => {}
            }
        }
        Ok(self.off != start)
    }

    pub fn add(&mut self, kind: Kind, parent: Option<usize>, t0: i64, copied: bool) -> usize {
        let i = self.nodes.len();
        let mut n = Node::new(kind, parent, t0);
        n.copied = copied;
        self.nodes.push(n);
        match parent {
            Some(p) => self.nodes[p].kids.push(i),
            None => self.roots.push(i),
        }
        i
    }

    /// Add a top-level node (turn / marker): copied ones go into the last 「上一段」 ("earlier") block
    pub fn add_top(&mut self, kind: Kind, t0: i64, copied: bool) -> usize {
        if !copied {
            return self.add(kind, None, t0, false);
        }
        let group = match self.roots.last() {
            Some(&g) if self.nodes[g].kind == Kind::Copied => g,
            _ => self.add(Kind::Copied, None, t0, true),
        };
        self.add(kind, Some(group), t0, true)
    }

    /// Tools with no result yet when interrupted: count them as stopped at the moment of interruption
    pub fn stop_open_tools(&mut self, turn: usize, ts: i64) {
        for c in self.nodes[turn].kids.clone() {
            for k in self.nodes[c].kids.clone() {
                let n = &mut self.nodes[k];
                if n.kind == Kind::Tool && n.t1 == 0 {
                    n.t1 = ts.max(n.t0);
                    n.state = St::Stopped;
                }
            }
        }
    }

    pub fn model_idx(&mut self, name: &str) -> usize {
        match self.models.iter().position(|m| m == name) {
            Some(i) => i,
            None => {
                self.models.push(name.to_string());
                self.models.len() - 1
            }
        }
    }

    /// The most frequent model (call lines name the model only when it's a different one)
    pub fn main_model(&self) -> usize {
        let mut n = vec![0usize; self.models.len()];
        for x in self.nodes.iter().filter(|x| x.kind == Kind::Call) {
            n[x.model] += 1;
        }
        n.iter().enumerate().max_by_key(|(_, c)| **c).map_or(0, |(i, _)| i)
    }

    /// This call's own cost; model missing from the price table = None
    pub fn cost(&self, i: usize) -> Option<f64> {
        let n = &self.nodes[i];
        crate::usage::call_cost(&self.models[n.model], n.tok, n.fast, n.us, n.searches)
    }

    /// Totals of a node and everything under it (copied ones not counted)
    pub fn sum(&self, i: usize) -> Sum {
        let mut s = Sum::default();
        self.add_up(i, &mut s);
        s
    }

    fn add_up(&self, i: usize, s: &mut Sum) {
        let n = &self.nodes[i];
        if n.copied {
            return;
        }
        match n.kind {
            Kind::Call => {
                s.calls += 1;
                for k in 0..5 {
                    s.tok[k] += n.tok[k];
                }
                s.out += n.tok[4];
                s.ctx = s.ctx.max(n.context());
                match self.cost(i) {
                    Some(c) => s.cost += c,
                    None => s.unpriced = true,
                }
            }
            Kind::Tool => {
                s.tools += 1;
                // Subagent: its cost and tokens count; calls, tools and context are its own business (shown on that step on the right)
                if let Some(sub) = n.sub.and_then(|k| self.subs.get(k)) {
                    let x = sub.t.total(false, 0).2;
                    s.subs += 1 + x.subs;
                    s.cost += x.cost;
                    s.sub_cost += x.cost;
                    s.unpriced |= x.unpriced;
                    for k in 0..5 {
                        s.tok[k] += x.tok[k];
                    }
                }
            }
            _ => {}
        }
        for &k in &n.kids {
            self.add_up(k, s);
        }
    }

    /// The subagent a tool started
    pub fn sub_of(&self, i: usize) -> Option<&Sub> {
        self.nodes.get(i)?.sub.and_then(|k| self.subs.get(k))
    }

    /// Descend level by level through the tools that started subagents
    pub fn at(&self, path: &[usize]) -> &Trace {
        match path.split_first() {
            Some((&i, rest)) => self.sub_of(i).map_or(self, |s| s.t.at(rest)),
            None => self,
        }
    }

    /// Whether that level is still running (every level's subagent still working)
    pub fn live_at(&self, path: &[usize], live: bool, now: i64) -> bool {
        match path.split_first() {
            Some((&i, rest)) => self.sub_of(i).is_some_and(|s| s.t.live_at(rest, self.sub_live(i, live, now), now)),
            None => live,
        }
    }

    /// How far the records have been read, subagents' included (when a subagent grows, the block holding it needs a relayout)
    pub fn grown(&self) -> u64 {
        self.off + self.subs.iter().map(|s| s.t.grown()).sum::<u64>()
    }

    /// Subagents started in a turn: (tool, subagent)
    pub fn subs_in(&self, turn: usize) -> Vec<usize> {
        let mut out = vec![];
        for &c in &self.nodes[turn].kids {
            out.extend(self.nodes[c].kids.iter().copied().filter(|&k| self.nodes[k].sub.is_some()));
        }
        out
    }

    /// Whether a subagent is still working: the session is running, no completion notice arrived, it hasn't finished writing itself
    /// (Claude's last call stopped at end_turn, every Codex turn ended), and there's been activity within half an hour — a subagent
    /// running when its session was closed never finishes writing and gets no notice (4 on this machine stopped after a tool call),
    /// so once the session is reopened it can't count as running forever
    pub fn sub_live(&self, i: usize, live: bool, now: i64) -> bool {
        let Some(s) = self.sub_of(i) else { return false };
        live && !self.done.contains_key(&s.id) && !s.t.finished() && now - s.t.last_active() < SUB_QUIET
    }

    /// Time of the last activity
    pub fn last_active(&self) -> i64 {
        self.nodes.iter().map(|n| n.t0.max(n.t1)).max().unwrap_or(0)
    }

    /// This record is finished (for subagents): the last call finished speaking and no tool awaits a result; for Codex, every turn
    /// ended
    pub fn finished(&self) -> bool {
        let Some(turn) = self.last_turn() else { return false };
        let n = &self.nodes[turn];
        if self.agent == Agent::Codex {
            return n.t1 > 0;
        }
        n.t1 > 0
            || n.kids
                .iter()
                .rev()
                .find(|&&k| self.nodes[k].kind == Kind::Call)
                .is_some_and(|&c| self.nodes[c].stop == "end_turn" && self.nodes[c].kids.is_empty())
    }

    /// A subagent's outcome: what the notice says (failed / stopped), otherwise that of its own last turn
    pub fn sub_state(&self, i: usize) -> St {
        let Some(s) = self.sub_of(i) else { return St::Ok };
        match self.done.get(&s.id) {
            Some(&st) => st,
            None => s.t.last_turn().map_or(St::Ok, |r| s.t.nodes[r].state),
        }
    }

    /// A subagent's duration from start to finish (for one run in the background the Agent tool returned long ago; the real time
    /// is here)
    pub fn sub_dur(&self, i: usize, live: bool, now: i64) -> i64 {
        let Some(s) = self.sub_of(i) else { return 0 };
        let live = self.sub_live(i, live, now);
        let first = s.t.roots.iter().map(|&r| s.t.nodes[r].t0).find(|&x| x > 0).unwrap_or(0);
        let last = if live { now } else { s.t.roots.iter().map(|&r| s.t.last_at(r)).max().unwrap_or(first) };
        (last - first).max(0)
    }

    /// Time of the last activity (its own and below)
    pub fn last_at(&self, i: usize) -> i64 {
        let n = &self.nodes[i];
        n.kids.iter().map(|&k| self.last_at(k)).fold(n.t0.max(n.t1), i64::max)
    }

    /// Duration (ms). Unfinished: up to now if running, else up to the last activity
    pub fn dur(&self, i: usize, live: bool, now: i64) -> i64 {
        let n = &self.nodes[i];
        let end = match n.t1 {
            0 if live => now,
            0 => self.last_at(i),
            t => t,
        };
        (end - n.t0).max(0)
    }

    /// Turn number at the top level (copied ones are unnumbered)
    pub fn turn_no(&self, i: usize) -> Option<usize> {
        let n = &self.nodes[i];
        if n.kind != Kind::Turn || n.copied || n.rolled {
            return None;
        }
        self.roots.iter().filter(|&&r| self.nodes[r].kind == Kind::Turn && !self.nodes[r].rolled).position(|&r| r == i).map(|p| p + 1)
    }

    /// The last turn (a numbered one of its own)
    pub fn last_turn(&self) -> Option<usize> {
        self.roots.iter().rev().copied().find(|&r| self.nodes[r].kind == Kind::Turn)
    }

    /// Whether this node is running: the session is running, it's the last turn (or inside it), and it hasn't ended
    pub fn running(&self, i: usize, live: bool) -> bool {
        if !live || self.nodes[i].t1 != 0 || self.nodes[i].copied {
            return false;
        }
        let mut top = i;
        while let Some(p) = self.nodes[top].parent {
            top = p;
        }
        Some(top) == self.last_turn()
    }

    /// Overall: turn count, working time (the turns' durations added up), totals
    pub fn total(&self, live: bool, now: i64) -> (usize, i64, Sum) {
        let mut s = Sum::default();
        let mut turns = 0;
        let mut dur = 0;
        for &r in &self.roots {
            if self.nodes[r].kind == Kind::Turn {
                turns += usize::from(!self.nodes[r].rolled);
                dur += self.dur(r, self.running(r, live), now);
            }
            self.add_up(r, &mut s);
        }
        (turns, dur, s)
    }

    /// Read one original line back
    pub fn read_at(&self, (off, len): (u64, u32)) -> Option<serde_json::Value> {
        let mut f = File::open(&self.path).ok()?;
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut buf = vec![0; len as usize];
        f.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }
}

/// The file's creation time (ms). Not trusted when the modification time is earlier than creation (copied with `cp -p`):
/// returns 0, no search for copied lines
fn born(path: &Path) -> i64 {
    let ms = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let Ok(m) = std::fs::metadata(path) else { return 0 };
    match (m.created(), m.modified()) {
        (Ok(c), Ok(w)) if ms(w) >= ms(c) => ms(c),
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sum {
    pub calls: usize,
    pub tools: usize,
    pub tok: [u64; 5],
    pub out: u64,
    /// The largest context
    pub ctx: u64,
    pub cost: f64,
    /// Some model isn't in the price table: the cost covers only what can be priced
    pub unpriced: bool,
    /// How many subagents were started below (including the ones they started), and their cost (already in cost)
    pub subs: usize,
    pub sub_cost: f64,
}

/// Max characters in a one-line summary
pub const LABEL: usize = 200;

/// A subagent with no activity for this long (ms) no longer counts as running (see Trace::sub_live)
const SUB_QUIET: i64 = 30 * 60_000;

/// 2026-09-30T01:02:03.456Z → ms
pub fn ts_ms(s: &str) -> i64 {
    let ms = s.get(19..).and_then(|r| r.strip_prefix('.')).map_or(0, |r| {
        let d: String = r.chars().take_while(char::is_ascii_digit).take(3).collect();
        format!("{d:0<3}").parse::<i64>().unwrap_or(0)
    });
    crate::usage::parse_ts(s) * 1000 + ms
}

/// A sentence for a list, without Markdown marks: **, `, and a leading # or -
pub fn unmark(s: &str) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let l = l.trim_start_matches('#').trim_start();
    let l = l.strip_prefix("- ").unwrap_or(l);
    l.replace("**", "").replace('`', "")
}

/// Flatten Markdown into one line: strip each line's marks and join them. A line with no closing punctuation (a heading) is
/// followed by ·
pub fn flat(s: &str) -> String {
    let mut out = String::new();
    for l in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match out.chars().last() {
            None => {}
            Some('。' | '！' | '？' | '：' | '；' | '，' | '、' | '…' | '）' | '」' | '”') => {}
            Some('.' | '!' | '?' | ':' | ';' | ',' | ')' | '"') => out.push(' '),
            Some(_) => out.push_str(" · "),
        }
        out.push_str(&unmark(l));
    }
    out
}

// ---------- Numbers ----------

pub fn fmt_dur(ms: i64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60)
    } else {
        format!("{}h{:02}m", ms / 3_600_000, ms / 60_000 % 60)
    }
}

pub fn fmt_tok(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else {
        format!("{:.1}M", n as f64 / 1e6)
    }
}

pub fn fmt_cost(c: f64) -> String {
    if c >= 100.0 { format!("${c:.0}") } else { format!("${c:.2}") }
}

/// Thousands separators: 1,204
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Local time HH:MM (secs = true adds seconds)
pub fn clock(ms: i64, tz: i64, secs: bool) -> String {
    let t = (ms / 1000 + tz).rem_euclid(86400);
    if secs { format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{:02}:{:02}", t / 3600, t / 60 % 60) }
}

// ---------- One-line rows ----------

/// The turn a node is in (a turn inside the copied block counts as a turn)
pub fn turn_of(t: &Trace, mut i: usize) -> usize {
    while let Some(p) = t.nodes[i].parent.filter(|&p| t.nodes[p].kind != Kind::Copied) {
        i = p;
    }
    i
}

/// Short tool name: MCP tools show as MCP (server · tool go in the summary)
pub fn tool_name(name: &str) -> &str {
    if name.starts_with("mcp__") { "MCP" } else { name }
}

/// Short model name: claude-haiku-4-5-20251001 → haiku-4-5
pub fn short_model(m: &str) -> String {
    let k = crate::usage::model_key(m);
    k.strip_prefix("claude-").unwrap_or(&k).to_string()
}

// ---------- Content read back: each tool reads its own; trace_page lays it out as text ----------

/// One line of an edit. c: '-' removed / '+' added / ' ' context / '~' between two hunks; no: the original line number for
/// removed lines, the edited one otherwise (none if unknown)
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub c: char,
    pub no: Option<u64>,
    pub text: String,
}

/// A tool's content, read back by either tool into the same shape: the right side, details and e all lay out from it
#[derive(Debug, Default)]
pub struct Io {
    /// The command(s) run and their note (the description Claude's Bash writes)
    pub cmds: Vec<String>,
    pub note: Option<String>,
    /// Edited files: (path, changes). A file-editing tool has an entry even when the changes are empty
    pub edits: Vec<(String, Vec<Change>)>,
    /// File content read: (path, each line's number and text)
    pub read: Option<(String, Vec<(u64, String)>)>,
    /// Other input: (language, formatted text)
    pub input: Option<(&'static str, String)>,
    pub out: Option<String>,
    pub exit: Option<i64>,
    /// The real file e opens: (path, line)
    pub file: Option<(String, u64)>,
}

/// A call's thinking and text (one paragraph per block)
pub fn said(t: &Trace, i: usize) -> (Vec<String>, Vec<String>) {
    match t.agent {
        Agent::Codex => crate::trace_codex::said(t, &t.nodes[i]),
        _ => crate::trace_claude::said(t, &t.nodes[i]),
    }
}

/// Who asked a turn. Logic uses this; it becomes text only when shown (the text follows the language)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asker {
    You,
    /// Claude's `↩` kind: telling it that background work finished
    Notice,
    /// The task a subagent receives, assigned to it
    Task,
    /// The record starts mid-turn: nobody
    Nobody,
}

impl Asker {
    fn label(self) -> &'static str {
        match self {
            Asker::You => Tr::new("你", "You").get(),
            Asker::Notice => Tr::new("通知", "Notice").get(),
            Asker::Task => Tr::new("任务", "Task").get(),
            Asker::Nobody => "",
        }
    }
}

/// Who asked a turn and what (nobody when the record starts mid-turn; a subagent's is the task assigned to it)
pub fn asked(t: &Trace, i: usize) -> (&'static str, String) {
    let (who, text) = match t.agent {
        Agent::Codex => crate::trace_codex::asked(t, &t.nodes[i]),
        _ => crate::trace_claude::asked(t, &t.nodes[i]),
    };
    (if t.is_sub && who == Asker::You { Asker::Task } else { who }.label(), text)
}

pub fn io(t: &Trace, i: usize) -> Io {
    match t.agent {
        Agent::Codex => crate::trace_codex::io(t, &t.nodes[i]),
        _ => crate::trace_claude::io(t, &t.nodes[i]),
    }
}

/// What the error entry says
pub fn fail_text(t: &Trace, i: usize) -> String {
    match t.agent {
        Agent::Codex => crate::trace_codex::fail_text(t, &t.nodes[i]),
        _ => crate::trace_claude::fail_text(t, &t.nodes[i]),
    }
}

/// An extra line in a call's details (Claude: why the cache missed)
pub fn call_note(t: &Trace, i: usize) -> Option<String> {
    match t.agent {
        Agent::Codex => None,
        _ => crate::trace_claude::call_note(t, &t.nodes[i]),
    }
}

/// The line atop the details: turn N · call K
pub fn where_of(t: &Trace, i: usize) -> String {
    let n = &t.nodes[i];
    let turn = turn_of(t, i);
    let no = match t.turn_no(turn) {
        Some(x) => tr!("第 {x} 轮", "turn {x}"),
        None if t.nodes[turn].rolled => t!("已回退", "rolled back").into(),
        None => t!("上一段", "earlier").into(),
    };
    match n.kind {
        Kind::Turn => no,
        Kind::Call | Kind::Fail => {
            let k = n.parent.map_or(0, |p| {
                t.nodes[p].kids.iter().filter(|&&x| t.nodes[x].kind == Kind::Call).position(|&x| x == i).map_or(0, |x| x + 1)
            });
            if n.kind == Kind::Call { tr!("{no} · 第 {k} 次调用", "{no} · call {k}") } else { tr!("{no} · 出错", "{no} · error") }
        }
        Kind::Tool => format!("{} · {no}", tool_name(&n.name)),
        Kind::Mark => n.label.clone(),
        Kind::Copied => t!("上一段", "earlier").into(),
    }
}

/// Paths relative to its directory at the time; the home directory as ~
pub fn short_path(p: &str, cwd: &str) -> String {
    if !cwd.is_empty()
        && let Some(rest) = p.strip_prefix(cwd).and_then(|r| r.strip_prefix('/'))
    {
        return rest.to_string();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// The first non-empty line (trimmed), at most limit characters
pub fn first_line(s: &str, limit: usize) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    l.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_short() {
        assert_eq!(fmt_dur(8_400), "8.4s");
        assert_eq!(fmt_dur(12_300), "12.3s");
        assert_eq!(fmt_dur(108_000), "1m48s");
        assert_eq!(fmt_dur(9_660_000), "2h41m");
        assert_eq!(fmt_tok(640), "640");
        assert_eq!(fmt_tok(1_204), "1.2k");
        assert_eq!(fmt_tok(92_345), "92k");
        assert_eq!(fmt_tok(1_250_000), "1.2M");
        assert_eq!(fmt_cost(0.052), "$0.05");
        assert_eq!(fmt_cost(123.4), "$123");
        assert_eq!(thousands(88_102), "88,102");
        assert_eq!(thousands(12), "12");
        assert_eq!(clock(0, 8 * 3600, false), "08:00");
        assert_eq!(short_path("/p/app/src/a.rs", "/p/app"), "src/a.rs");
        assert_eq!(first_line("\n  hi there\nnext", 5), "hi th");
    }
}
