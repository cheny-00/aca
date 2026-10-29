//! The list: merges the registry, the panes actually in tmux, the state hooks write and what's on the screen into one row
//! per session.
//!
//! One refresh = one `tmux list-panes -a` + one database read + one small file per session + (when reading screens) one
//! tmux command that captures the screens of all running sessions together (`tmux::capture_many`). Without screens
//! (`--no-screen`) only the first three remain: a few milliseconds.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use serde::Serialize;

use crate::agent::{self, Agent, Handle};
use crate::config::Config;
use crate::import;
use crate::paths::{self, Res};
use crate::runtime::{self, Run};
use crate::screen::{self, Seen};
use crate::store::{Session, Store};
use crate::tmux::{self, Pane};
use crate::{event, hook};

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Running
    Running,
    /// Needs you (a choice prompt / confirmation prompt)
    Ask,
    /// Finished and not read yet
    Unread,
    Idle,
    Error,
    /// The agent exited but the pane is still there (`aca start` resumes the conversation in place)
    Exited,
    /// Registered but not in tmux (stopped, or the computer restarted): `aca start` creates it again
    Stopped,
}

impl State {
    pub fn glyph(self) -> &'static str {
        match self {
            State::Running => "●",
            State::Ask => "◆",
            State::Unread => "◐",
            State::Idle => "○",
            State::Error => "✕",
            State::Exited | State::Stopped => "■",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub state: State,
    pub tool: String,
    pub account: String,
    pub group: String,
    pub dir: String,
    pub parent: String,
    /// Forked: which session it was forked from (not `parent`: nobody delegated it)
    pub fork_of: String,
    /// The tmux session name (= the title) and the agent's pane; empty when not running
    pub tmux: String,
    pub pane: String,
    pub attached: u32,
    pub conv: String,
    /// The question it asks when it needs you
    pub ask: String,
    /// How many seconds it has been running, when running
    pub ran: u64,
    /// The last time anything happened: when a registered agent isn't running, what the hook reported (see [`Run::hook_at`]);
    /// otherwise the last time the window had output
    pub active: u64,
    pub bg: u32,
    pub locked: bool,
    pub created: u64,
    pub order: i64,
    /// Short snippets of text plugins attach
    pub tokens: BTreeMap<String, String>,
    /// When aca stopped it for sitting idle (see [`crate::session::stop_idle`]); 0 otherwise (running, stopped by a person)
    pub idle_stopped: u64,
}

impl Row {
    /// The state icon: ☾ if aca stopped it for sitting idle, otherwise by state
    pub fn glyph(&self) -> &'static str {
        if self.idle_stopped > 0 { IDLE_STOPPED } else { self.state.glyph() }
    }
}

/// The icon for sessions aca stopped for sitting idle (stopped by a person or exited by the agent itself is ■)
pub const IDLE_STOPPED: &str = "☾";

/// The title shown: a person's rename wins, otherwise the agent's own title, and until there is one, the title from
/// registration
pub fn title(s: &Session, r: &Run) -> String {
    if s.title_locked || r.auto_title.is_empty() { s.title.clone() } else { r.auto_title.clone() }
}

/// When the title in the pane title differs from the recorded one: record it and rename the tmux session to match (the same
/// thing the hook does). Returns true if anything was renamed. Checked on every list refresh, without reading files or
/// asking tmux again: for sessions without hooks (adopted, not restarted yet) this is the only way titles follow, and
/// sessions with hooks don't have to wait for the turn to finish
pub fn follow_titles(cfg: &Config, sessions: &[Session], panes: &[Pane]) -> bool {
    let mut renamed = false;
    for s in sessions {
        let Some(p) = tmux::agent_pane(panes, &s.id).filter(|p| !p.dead) else { continue };
        let Some(agent) = agent_of(cfg, s) else { continue };
        let r = runtime::load(&s.id);
        if agent.live_title(&r, &p.title).is_none_or(|t| t == r.auto_title) {
            continue;
        }
        // Only now take the lock and ask again: the hook may be changing it at the same moment
        for (name, data) in hook::retitle(&s.id, |r| agent.live_title(r, &p.title)) {
            event::emit(name, &s.id, data);
            renamed = true;
        }
    }
    renamed
}

/// The runtime state the list reads: the one aca's hooks write ([`runtime::load`]). An adopted process not restarted yet
/// doesn't carry aca's hooks, so that one is empty; those agent-deck started still carry its hooks and keep reporting, so
/// borrow the last thing they recorded: "running" and "unread" don't have to wait for a restart
pub fn load(id: &str) -> Run {
    let mut r = runtime::load(id);
    if r.status.is_empty()
        && let Some((event, ts)) = import::agent_deck_hook(id)
    {
        borrow(&mut r, &event, ts);
    }
    r
}

/// An event recorded by agent-deck's hooks → aca's state. It records finished, just started and waiting for your approval
/// all as waiting, which can't be told apart, so look at the event name
fn borrow(r: &mut Run, event: &str, ts: u64) {
    let status = match event {
        "UserPromptSubmit" => {
            r.turn = ts;
            "running"
        }
        "Stop" => {
            r.done = ts;
            "done"
        }
        "PermissionRequest" | "Notification" => "waiting",
        "SessionStart" => "idle",
        _ => return,
    };
    (r.status, r.event, r.ts) = (status.into(), event.into(), ts);
}

/// The screen decides the state (when it was read); hooks fill in what the screen can't show
pub fn merge(pane: Option<&Pane>, r: &Run, seen: Option<&Seen>, now: u64) -> (State, String, u64) {
    let Some(p) = pane else { return (State::Stopped, String::new(), 0) };
    if p.dead {
        return (State::Exited, String::new(), 0);
    }
    let since_turn = if r.status == "running" && r.turn > 0 { now.saturating_sub(r.turn) } else { 0 };
    // Tools whose screen can't tell: treat as if the screen wasn't read, and go by whatever the hook says
    let seen = seen.filter(|s| **s != Seen::Unsure);
    match seen {
        // The question: the hook's report can name the tool and the command, which is more useful than the screen's "Do you want
        // to proceed?"
        Some(Seen::Ask(q)) => {
            return (State::Ask, if q.is_empty() || (r.status == "waiting" && !r.ask.is_empty()) { r.ask.clone() } else { q.clone() }, 0);
        }
        // In the first seconds of a turn there's no timer on the screen yet: use the turn start the hook recorded
        Some(Seen::Running(n)) => return (State::Running, String::new(), if *n > 0 { *n } else { since_turn }),
        _ => {}
    }
    if screen::spinning(&p.title) {
        return (State::Running, String::new(), since_turn);
    }
    let state = match r.status.as_str() {
        // The screen was read but shows no prompt: either the prompt isn't drawn yet (just reported), or it was already
        // interrupted with Esc (interrupting sends no event)
        "waiting" if seen.is_none() || now.saturating_sub(r.ts) < 5 => State::Ask,
        // A turn just reported as started but the screen isn't spinning yet: the reread an event triggers often runs ahead of the
        // screen, and not trusting the hook would mean waiting for the next 2-second reread. If the screen is still quiet after
        // these few seconds, take it as interrupted with Esc. Text still streaming out (only those lines changing on screen)
        // doesn't count as quiet
        "running" if seen.is_none() || now.saturating_sub(r.turn) < 3 || seen == Some(&Seen::Moving) => State::Running,
        "error" => State::Error,
        // Read: marked by hand / someone attached the moment it finished (recorded by the hook), attached after it finished, or
        // pressed a key inside after it finished (stayed attached watching it finish, then switched away). A key press in the
        // same second as finishing doesn't count: the order can't be told, and tmux also records "someone touched it" when a
        // session is created
        "done" if p.attached == 0 && r.done > r.seen.max(p.last_attached) && r.done >= p.activity => State::Unread,
        _ => State::Idle,
    };
    match state {
        State::Ask => (state, r.ask.clone(), 0),
        State::Running => (state, String::new(), since_turn),
        _ => (state, String::new(), 0),
    }
}

fn row(s: &Session, pane: Option<&Pane>, r: &Run, seen: Option<&Seen>, now: u64) -> Row {
    let (state, ask, ran) = merge(pane, r, seen, now);
    Row {
        id: s.id.clone(),
        title: title(s, r),
        state,
        tool: s.tool.clone(),
        account: s.account.clone(),
        group: s.group.clone(),
        dir: s.dir.clone(),
        parent: s.parent.clone(),
        fork_of: s.fork_of.clone(),
        tmux: pane.map(|p| p.session.clone()).unwrap_or_default(),
        pane: pane.map(|p| p.pane.clone()).unwrap_or_default(),
        attached: pane.map_or(0, |p| p.attached),
        conv: if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() },
        ask,
        ran,
        active: match r.hook_at() {
            // An agent that isn't running but is still open goes by what the hook reported (see [`Run::hook_at`]): an idle one
            // redrawing would make the window's activity time "just now"
            at if at > 0 && matches!(state, State::Idle | State::Unread | State::Ask | State::Error) => at,
            _ => pane.map_or(0, |p| p.activity).max(r.ts).max(r.restored).max(s.created),
        },
        bg: r.bg,
        locked: s.title_locked,
        created: s.created,
        order: s.order,
        tokens: r.live_tokens(now),
        idle_stopped: if matches!(state, State::Exited | State::Stopped) { r.idle_stopped } else { 0 },
    }
}

/// What the tail of the text (see [`Agent::body`]) looked like last time and when it last changed (ms): session id →
/// (fingerprint, time). Only kept in this process: the interface and `aca wait`, which keep watching, remember it;
/// `aca ls` looks only once, has nothing to compare, and takes it as not moving
static BODIES: LazyLock<Mutex<HashMap<String, (u64, u64)>>> = LazyLock::new(Default::default);
/// For this long (ms) after the text last changed, it still counts as moving. After an Esc interrupt, ● stays this much
/// longer
const MOVING_MS: u64 = 3000;

/// Reading the screen: this tool's reader; if that reads as quiet, check whether the text is still moving
/// ([`Seen::Moving`]). Every look records what the text looks like
pub fn look(id: &str, agent: &dyn Agent, lines: &[String]) -> Seen {
    let seen = agent.screen(lines);
    let Some(fp) = agent.body(lines) else { return seen };
    let mut bodies = BODIES.lock().unwrap_or_else(|e| e.into_inner());
    let (next, moving) = moved(bodies.get(id).copied(), fp, paths::now_ms());
    bodies.insert(id.to_string(), next);
    if seen == Seen::Quiet && moving { Seen::Moving } else { seen }
}

/// Compares the fingerprint `fp` seen now with the last recorded one (fingerprint, time of last change): returns what to
/// record and whether it counts as moving right now. The first look (no previous one) doesn't count as moving
fn moved(last: Option<(u64, u64)>, fp: u64, now: u64) -> ((u64, u64), bool) {
    let at = match last {
        Some((was, at)) if was == fp => at,
        Some(_) => now,
        None => 0,
    };
    ((fp, at), at > 0 && now.saturating_sub(at) < MOVING_MS)
}

/// Which tool this session is (it decides how to read the screen); a tool deleted from the config can't be recognized,
/// so the screen isn't read
fn agent_of(cfg: &Config, s: &Session) -> Option<Handle> {
    agent::of(cfg, s).ok().map(|x| x.0)
}

/// All registered sessions, in group order, then their order within the group
pub fn collect(store: &Store, cfg: &Config, read_screen: bool) -> Res<Vec<Row>> {
    let sessions = store.sessions(false)?;
    let mut panes = tmux::panes();
    // If session names changed, list again (the rows carry the session name)
    if follow_titles(cfg, &sessions, &panes) {
        panes = tmux::panes();
    }
    let now = paths::now();
    let picked: Vec<Option<&Pane>> = sessions.iter().map(|s| tmux::agent_pane(&panes, &s.id)).collect();
    let agents: Vec<Option<Handle>> = sessions.iter().map(|s| if read_screen { agent_of(cfg, s) } else { None }).collect();
    // Capture the panes whose screens are needed with one tmux command: one tmux process per pane would mean dozens of
    // processes per refresh with dozens of sessions
    let want: Vec<Option<&Pane>> = agents.iter().zip(&picked).map(|(a, p)| a.as_ref().and_then(|_| p.filter(|p| !p.dead))).collect();
    let ids: Vec<&str> = want.iter().flatten().map(|p| p.pane.as_str()).collect();
    let mut screens = tmux::capture_many(&ids, false).into_iter();
    let seen: Vec<Option<Seen>> = sessions
        .iter()
        .zip(&agents)
        .zip(&want)
        .map(|((s, a), w)| {
            let agent: &dyn Agent = a.as_deref()?;
            w.and_then(|_| screens.next()).map(|out| look(&s.id, agent, &tmux::plain_lines(&out)))
        })
        .collect();
    let rank: HashMap<String, i64> = store.groups()?.into_iter().map(|g| (g.path, g.order)).collect();
    let mut rows: Vec<Row> = sessions
        .iter()
        .zip(&picked)
        .zip(&seen)
        .zip(&agents)
        .map(|(((s, p), sn), a)| row(s, *p, &settled(cfg, s, a.as_deref(), sn.as_ref()), sn.as_ref(), now))
        .collect();
    rows.sort_by(|a, b| {
        let key = |r: &Row| (rank.get(&r.group).copied().unwrap_or(i64::MAX), r.group.clone(), r.order, r.created);
        key(a).cmp(&key(b))
    });
    Ok(rows)
}

/// A read-only look at all sessions: no screens, no filling in unreported turn ends, no following renames. For callers that
/// keep asking, like the tmux status bar (`aca tmux-status`), which shouldn't change anything each time they ask
pub fn peek(store: &Store) -> Res<Vec<Row>> {
    let panes = tmux::panes();
    let now = paths::now();
    Ok(store.sessions(false)?.iter().map(|s| row(s, tmux::agent_pane(&panes, &s.id), &load(&s.id), None, now)).collect())
}

/// A session as it is now (`show`, waiting for it to go idle before sending a message)
pub fn one(cfg: &Config, s: &Session, read_screen: bool) -> Row {
    let pane = tmux::find(&s.id);
    let agent = agent_of(cfg, s);
    let seen = pane.as_ref().filter(|p| read_screen && !p.dead).and_then(|p| Some(look(&s.id, agent.as_deref()?, &tmux::capture(&p.pane))));
    row(s, pane.as_ref(), &settled(cfg, s, agent.as_deref(), seen.as_ref()), seen.as_ref(), paths::now())
}

/// The runtime state, after first filling in a turn end it didn't report: if the hook says running but the screen has
/// stopped, ask this tool whether the turn already ended without a hook (a Codex error, an untrusted Stop hook; see
/// [`Agent::unreported_end`]). If so, fill in that event for it, recording the state, emitting `turn.done` /
/// `turn.failed` and notifying the session that delegated it, just like a real hook. There's no resident process: it's
/// only noticed when someone looks (`aca ls`, an interface refresh, `aca wait`)
fn settled(cfg: &Config, s: &Session, agent: Option<&dyn Agent>, seen: Option<&Seen>) -> Run {
    let r = load(&s.id);
    if r.status != "running" || seen != Some(&Seen::Quiet) {
        return r;
    }
    match agent.and_then(|a| Some((a, a.unreported_end(&r)?))) {
        Some((a, v)) => {
            hook::feed(cfg, &s.id, a, &v);
            load(&s.id)
        }
        None => r,
    }
}

/// [`settled`] for the side that reads screens and builds the list itself (the interface): asks each session the hook says
/// is running, and anything filled in shows up in the state file it reads next. `screen` = this pane's current screen
/// (text without colors)
pub fn settle(cfg: &Config, sessions: &[Session], panes: &[Pane], screen: &dyn Fn(&Pane) -> Vec<String>) {
    for s in sessions {
        if runtime::load(&s.id).status != "running" {
            continue;
        }
        let Some(p) = tmux::agent_pane(panes, &s.id).filter(|p| !p.dead) else { continue };
        let Some(agent) = agent_of(cfg, s) else { continue };
        settled(cfg, s, Some(&*agent), Some(&look(&s.id, &*agent, &screen(p))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outer layer of `aca ls --json` / `aca show --json` is plugin interface: every field is written up in "The contract"
    /// in docs/plugins.md (English) and its section in plugins.zh-CN.md. Adding a field to Row fails here — add it, then
    /// write a line in the docs
    #[test]
    fn every_field_of_a_listed_session_is_in_the_plugin_contract() {
        let r = Row {
            id: String::new(),
            title: String::new(),
            state: State::Idle,
            tool: String::new(),
            account: String::new(),
            group: String::new(),
            dir: String::new(),
            parent: String::new(),
            fork_of: String::new(),
            tmux: String::new(),
            pane: String::new(),
            attached: 0,
            conv: String::new(),
            ask: String::new(),
            ran: 0,
            active: 0,
            bg: 0,
            locked: false,
            created: 0,
            order: 0,
            tokens: BTreeMap::new(),
            idle_stopped: 0,
        };
        for (file, docs, from, to) in [
            ("plugins.md", include_str!("../../../docs/plugins.md"), "**JSON that aca prints**", "**Names**"),
            ("plugins.zh-CN.md", include_str!("../../../docs/plugins.zh-CN.md"), "**aca 输出的 JSON**", "**命名**"),
        ] {
            let at = docs.find(from).unwrap_or_else(|| panic!("{file} 里没有 {from}"));
            let part = &docs[at..at + docs[at..].find(to).unwrap()];
            for k in serde_json::to_value(&r).unwrap().as_object().unwrap().keys() {
                assert!(part.contains(&format!("`{k}`")), "{file} 没写 aca ls --json 的 {k}");
            }
        }
    }

    fn pane() -> Pane {
        Pane { id: "ab12".into(), agent: true, pane: "%1".into(), ..Default::default() }
    }

    fn run(status: &str) -> Run {
        Run { status: status.into(), ..Default::default() }
    }

    #[test]
    fn the_screen_decides_and_hooks_fill_in_what_it_cannot_show() {
        let p = pane();
        let st = |r: &Run, seen: Option<Seen>| merge(Some(&p), r, seen.as_ref(), 100).0;
        // A prompt on the screen / spinning: go by the screen, whatever the hook says
        assert_eq!(merge(Some(&p), &run("done"), Some(&Seen::Ask("要删吗?".into())), 100), (State::Ask, "要删吗?".into(), 0));
        let asked = Run { ask: "Bash:rm -rf build".into(), ..run("waiting") };
        assert_eq!(merge(Some(&p), &asked, Some(&Seen::Ask("Do you want to proceed?".into())), 100).1, "Bash:rm -rf build");
        assert_eq!(merge(Some(&p), &run("idle"), Some(&Seen::Running(72)), 100), (State::Running, String::new(), 72));
        // Running on screen but no timer yet (a turn just started): time run counts from the turn start the hook recorded
        assert_eq!(merge(Some(&p), &Run { turn: 91, ..run("running") }, Some(&Seen::Running(0)), 100).2, 9);
        // An Esc interrupt sends no event: the hook still says running, but the screen is already quiet
        assert_eq!(st(&run("running"), Some(Seen::Quiet)), State::Idle);
        // A turn just reported as started and the screen isn't spinning yet: trust the hook first; only quiet after a few
        // seconds counts as interrupted
        assert_eq!(merge(Some(&p), &Run { turn: 99, ..run("running") }, Some(&Seen::Quiet), 100), (State::Running, String::new(), 1));
        assert_eq!(st(&Run { turn: 97, ..run("running") }, Some(Seen::Quiet)), State::Idle);
        // Screen not read: trust the hook
        assert_eq!(merge(Some(&p), &Run { turn: 40, ..run("running") }, None, 100), (State::Running, String::new(), 60));
        assert_eq!(merge(Some(&p), &Run { ask: "Bash:ls".into(), ..run("waiting") }, None, 100), (State::Ask, "Bash:ls".into(), 0));
        // Just reported as needing you, and the prompt isn't drawn yet
        assert_eq!(st(&Run { ts: 98, ..run("waiting") }, Some(Seen::Quiet)), State::Ask);
        assert_eq!(st(&Run { ts: 10, ..run("waiting") }, Some(Seen::Quiet)), State::Idle);
        assert_eq!(st(&run("error"), Some(Seen::Quiet)), State::Error);
        // Tools whose screen can't tell (a declared tool without busy): running if the hook says running, waiting if it says
        // waiting
        assert_eq!(st(&run("running"), Some(Seen::Unsure)), State::Running);
        assert_eq!(st(&Run { ts: 10, ..run("waiting") }, Some(Seen::Unsure)), State::Ask);
        assert_eq!(st(&run("idle"), Some(Seen::Unsure)), State::Idle);
        // A spinner in the pane title also counts as running
        assert_eq!(merge(Some(&Pane { title: "⠋ 修登录".into(), ..pane() }), &run("idle"), Some(&Seen::Quiet), 100).0, State::Running);
        // Text still streaming out (only those lines changing on screen): running if the hook says running. The last lines are
        // also redrawn right when a turn finishes, so once the hook says finished it doesn't count
        assert_eq!(merge(Some(&p), &Run { turn: 40, ..run("running") }, Some(&Seen::Moving), 100), (State::Running, String::new(), 60));
        assert_eq!(st(&Run { done: 99, ..run("done") }, Some(Seen::Moving)), State::Unread);
        assert_eq!(st(&run("idle"), Some(Seen::Moving)), State::Idle);
    }

    #[test]
    fn the_body_counts_as_moving_for_a_few_seconds_after_it_last_changed() {
        // First look: nothing to compare
        assert_eq!(moved(None, 7, 10_000), ((7, 0), false));
        assert_eq!(moved(Some((7, 0)), 7, 12_000), ((7, 0), false));
        // Changed: count from this moment
        assert_eq!(moved(Some((7, 0)), 8, 14_000), ((8, 14_000), true));
        // No change since (done streaming, interrupted with Esc): still counts within 3 seconds, not after
        assert_eq!(moved(Some((8, 14_000)), 8, 16_999), ((8, 14_000), true));
        assert_eq!(moved(Some((8, 14_000)), 8, 17_000), ((8, 14_000), false));
    }

    #[test]
    fn looking_again_tells_text_still_coming_out_from_a_quiet_screen() {
        let claude = agent::builtin("claude").unwrap();
        let bar = "─".repeat(40);
        let frame = |text: &str| -> Vec<String> {
            format!("⏺ 两条路的差别在于:\n{text}\n{bar}\n❯ \n{bar}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)")
                .lines()
                .map(String::from)
                .collect()
        };
        let (a, b) = (frame("  - 第一条"), frame("  - 第一条\n  - 第二条"));
        // The first look is quiet; the next one sees the text grew, which counts as moving, and an unchanged look right after
        // still counts
        assert_eq!(look("look-1", claude, &a), Seen::Quiet);
        assert_eq!(look("look-1", claude, &b), Seen::Moving);
        assert_eq!(look("look-1", claude, &b), Seen::Moving);
        // Each session is tracked separately
        assert_eq!(look("look-2", claude, &b), Seen::Quiet);
        // What the screen itself recognizes stands
        assert_eq!(look("look-1", claude, &frame("✻ Pondering… (12s)")), Seen::Running(12));
    }

    #[test]
    fn done_is_unread_until_someone_has_looked() {
        let done = Run { done: 50, ..run("done") };
        let st = |p: &Pane, r: &Run| merge(Some(p), r, None, 100).0;
        assert_eq!(st(&pane(), &done), State::Unread);
        // Attached after it finished
        assert_eq!(st(&Pane { last_attached: 60, ..pane() }, &done), State::Idle);
        // Attached right now
        assert_eq!(st(&Pane { attached: 1, ..pane() }, &done), State::Idle);
        // Someone was attached the moment it finished (recorded by the hook), or it was marked read by hand
        assert_eq!(st(&pane(), &Run { seen: 50, ..done.clone() }), State::Idle);
        // Stayed attached watching it finish (attached before it finished), and pressed a key inside before switching away
        assert_eq!(st(&Pane { last_attached: 40, activity: 55, ..pane() }, &done), State::Idle);
        // Key press before it finished (sent a prompt and switched away), or in the same second it finished: not read
        assert_eq!(st(&Pane { last_attached: 40, activity: 45, ..pane() }, &done), State::Unread);
        assert_eq!(st(&Pane { activity: 50, ..pane() }, &done), State::Unread);
        // Finished another turn after being read
        assert_eq!(st(&Pane { last_attached: 60, ..pane() }, &Run { done: 70, ..done }), State::Unread);
    }

    #[test]
    fn a_session_without_acas_hook_borrows_what_agent_decks_hook_last_said() {
        let at = |event: &str| {
            let mut r = Run { conv: "c-1".into(), seen: 7, ..Default::default() };
            borrow(&mut r, event, 50);
            r
        };
        let st = |r: &Run| merge(Some(&pane()), r, None, 100).0;
        assert_eq!((at("Stop").status.as_str(), at("Stop").done, st(&at("Stop"))), ("done", 50, State::Unread));
        // A manual "read" mark (`aca seen`, u in the interface) is recorded in aca's own copy and counts all the same
        assert_eq!(st(&Run { seen: 60, ..at("Stop") }), State::Idle);
        assert_eq!(merge(Some(&pane()), &at("UserPromptSubmit"), None, 100), (State::Running, String::new(), 50));
        assert_eq!((st(&at("PermissionRequest")), st(&at("SessionStart"))), (State::Ask, State::Idle));
        // Unrecognized events (SessionEnd, ones added later) change nothing: still go by the screen only; the recorded
        // conversation id and the like aren't lost
        assert_eq!((at("SessionEnd").status.as_str(), at("Stop").conv.as_str(), at("Stop").seen), ("", "c-1", 7));
    }

    #[test]
    fn no_pane_is_stopped_and_a_dead_pane_is_exited() {
        assert_eq!(merge(None, &run("running"), None, 1).0, State::Stopped);
        assert_eq!(merge(Some(&Pane { dead: true, ..pane() }), &run("running"), None, 1).0, State::Exited);
    }

    #[test]
    fn a_renamed_session_keeps_the_name_a_person_gave_it() {
        let s = Session { title: "aca".into(), ..Default::default() };
        let r = Run { auto_title: "修登录 bug".into(), ..Default::default() };
        assert_eq!(title(&s, &r), "修登录 bug");
        assert_eq!(title(&s, &Run::default()), "aca");
        assert_eq!(title(&Session { title_locked: true, ..s }, &r), "aca");
    }
}
