//! Operations on sessions: create, start, stop, restart, fork, rename, move to a group, delete, send messages. Both the
//! CLI and the interface call into here.
//!
//! A session = one row in the registry + one tmux session (whose name is the title). The agent is in the pane marked
//! `@aca_agent`, and that window has remain-on-exit on: after the agent exits, the pane stays with its last screen, so
//! **stopping and restarting both happen in place**, and other windows you opened in this tmux session aren't affected.
//! If the whole thing is gone from tmux (the computer restarted), that's fine too: the registration remains, and `start`
//! recreates it and picks up the conversation.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::agent::{self, How, Launch};
use crate::config::Config;
use crate::event;
use crate::paths::{self, Res};
use crate::proc;
use crate::runtime::{self, Run};
use crate::status::{self, State};
use crate::store::{Session, Store};
use crate::tmux::{self, Pane};
use crate::{t, tr};

#[derive(Clone, Debug, Default)]
pub struct New {
    /// Tool name, or "tool:account"
    pub tool: String,
    pub dir: String,
    pub title: String,
    pub group: String,
    /// Parent session; empty = if created from inside a session, it goes under that one (see `ACA_SESSION_ID`); `-` = no parent
    pub parent: String,
    pub args: Vec<String>,
    /// Only register, don't start
    pub hold: bool,
    /// Which events notify the parent (see [`Run::notify`]); empty = per the config
    pub notify: String,
    /// Start in a git worktree on this branch (see [`crate::worktree`]); empty = right in `dir`
    pub worktree: String,
}

pub(crate) fn fresh_id(store: &Store) -> String {
    loop {
        let id = paths::short_id();
        if store.get(&id).is_err() {
            return id;
        }
    }
}

/// Writing the group as this (`-g .`) picks the group from the session's directory — the group whose default directory it
/// is, otherwise a group named after the directory, numbered when the name is taken (by another directory): aca-2,
/// aca-3… (see `Store::dir_group`)
pub const BY_DIR: &str = ".";

pub fn create(store: &Store, cfg: &Config, n: New) -> Res<Session> {
    make(store, cfg, n, None).map(|(s, _)| s)
}

/// Creates a session and says its opening prompt (`aca new --say`). The second value = why the prompt wasn't said; the
/// session is created all the same. Tools that take it as a launch argument (Claude, Codex, see
/// [`agent::Agent::opening`]) count as said once it's passed in: they carry it past the startup gates and send it
/// themselves once up, and nothing waits here; other tools are waited for and then typed into ([`send_fresh`], waiting
/// up to a minute)
pub fn create_saying(store: &Store, cfg: &Config, n: New, text: &str) -> Res<(Session, Option<String>)> {
    if text.len() > agent::OPENING_MAX {
        return Err(tr!(
            "开场消息过长({} 字节,上限 {})",
            "the opening message is too long ({} bytes, at most {})",
            text.len(),
            agent::OPENING_MAX
        ));
    }
    let hold = n.hold;
    let (s, said) = make(store, cfg, n, Some(text))?;
    let unsaid = match (hold, said) {
        (true, _) => Some(t!("仅登记,未启动", "only registered, not started; not carried").to_string()),
        (false, true) => None,
        (false, false) => send_fresh(store, cfg, &s.id, text, Duration::from_secs(60)).err(),
    };
    Ok((s, unsaid))
}

/// The second value = the opening prompt was passed as a launch argument
fn make(store: &Store, cfg: &Config, n: New, say: Option<&str>) -> Res<(Session, bool)> {
    let agent::Pick { tool, account, agent, .. } = agent::pick(cfg, &n.tool)?;
    let dir = Path::new(&paths::expand(&n.dir)).canonicalize().map_err(|e| format!("{}:{e}", n.dir))?;
    if !dir.is_dir() {
        return Err(tr!("{} 不是目录", "{} is not a directory", dir.display()));
    }
    let dir = if n.worktree.is_empty() { dir } else { crate::worktree::make(&dir, n.worktree.trim())? };
    // Created from inside a session: becomes its child and joins the same group
    let parent = match n.parent.as_str() {
        "" => std::env::var("ACA_SESSION_ID").ok().and_then(|p| store.get(&p).ok()),
        // Created from the interface: even if the interface runs in some session's pane, it isn't that session's child
        "-" => None,
        p => Some(store.get(p)?),
    };
    let group = match n.group.trim_matches('/') {
        _ if n.group.is_empty() => parent.as_ref().map(|p| p.group.clone()).unwrap_or_default(),
        BY_DIR => store.group_claim(&dir.to_string_lossy())?.0,
        g => g.to_string(),
    };
    let title = n.title.trim();
    let s = Session {
        id: fresh_id(store),
        title: if title.is_empty() {
            dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "aca".into())
        } else {
            title.to_string()
        },
        title_locked: !title.is_empty(),
        order: store.next_order(&group),
        group,
        tool,
        account,
        dir: dir.to_string_lossy().into_owned(),
        args: n.args,
        conv: agent.new_conv(),
        parent: parent.map(|p| p.id).unwrap_or_default(),
        fork_of: String::new(),
        created: paths::now(),
        archived: 0,
    };
    store.insert(&s)?;
    runtime::update(&s.id, |r| {
        r.locked = s.title_locked;
        r.notify = n.notify.clone();
    });
    event::emit(
        "session.created",
        &s.id,
        json!({ "title": s.title, "tool": s.tool, "account": s.account, "dir": s.dir, "group": s.group, "parent": s.parent }),
    );
    if n.hold {
        return Ok((s, false));
    }
    match launch_saying(cfg, &s, None, How::Fresh, say) {
        Ok(said) => Ok((s, said)),
        Err(e) => {
            // Never started at all (the program isn't on PATH and the like): take back the registration, clean up the just-created
            // clean worktree too, and leave no row in the list that can't start
            if tmux::find(&s.id).is_none() {
                let _ = remove(store, &s.id);
            }
            Err(e)
        }
    }
}

/// Whether to resume the old conversation or start a new one (each tool decides); the second value is the current
/// conversation's id
fn how_to_resume(cfg: &Config, s: &Session, r: &Run) -> (How, String) {
    let conv = if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() };
    let how = agent::of(cfg, s).map_or(How::Fresh, |(agent, _)| agent.resume(&conv, r));
    (how, conv)
}

/// Starts the agent in the target pane. The account's environment goes only to this one process via `respawn-pane -e`, not
/// into the tmux session's environment: `new-session -e` writes the session's environment, which every window opened in
/// the session later inherits, and switching accounts only overrides the variables it writes itself, so things like keys
/// from env_file still leak through (tested on tmux 3.7c)
fn respawn(target: &str, dir: &str, l: &Launch, argv: Vec<String>) -> Vec<String> {
    let mut a: Vec<String> = ["respawn-pane", "-k", "-t", target, "-c", dir].iter().map(|x| x.to_string()).collect();
    a.extend(l.env.iter().flat_map(|(k, v)| ["-e".to_string(), format!("{k}={v}")]));
    a.extend(argv);
    a
}

/// Starts the agent: in place if there's a pane (`respawn-pane`), otherwise creates a tmux session named after the title
fn launch(cfg: &Config, s: &Session, pane: Option<&Pane>, how: How) -> Res<()> {
    launch_saying(cfg, s, pane, how, None).map(drop)
}

/// [`launch`], with the opening prompt. Returns whether it was passed as a launch argument (see [`agent::launch`])
fn launch_saying(cfg: &Config, s: &Session, pane: Option<&Pane>, how: How, say: Option<&str>) -> Res<bool> {
    let l = agent::launch(cfg, s, &how, say)?;
    let argv: Vec<String> = l.argv.iter().map(|a| tmux::arg(a)).collect();
    match pane {
        Some(p) => {
            tmux::run(&respawn(&p.pane, &s.dir, &l, argv))?;
        }
        None => {
            let r = runtime::load(&s.id);
            let mut taken: Vec<String> = tmux::panes().into_iter().map(|p| p.session).collect();
            taken.dedup();
            let name = tmux::unique_name(&tmux::safe_name(&status::title(s, &r)), &taken);
            let target = format!("={name}:");
            // The session is created without -e (so its environment is clean), first running a placeholder, and at the end of the
            // same command the pane is replaced with the agent (see `respawn`). The placeholder exits by itself: if that last step
            // doesn't run, the pane is left "not running", same as an agent that exits right away, and `start` can start it again in
            // place
            let mut a: Vec<String> = ["new-session", "-d", "-s", &name, "-n", &name, "-c", &s.dir, "/usr/bin/env", "sleep", "5"]
                .iter()
                .map(|x| x.to_string())
                .collect();
            // Set in the same command: if the agent exits right after starting (bad arguments), the pane is already kept, so the
            // error can be seen
            for opt in [
                vec!["set-option", "-t", &target, "@aca_id", &s.id],
                vec!["set-option", "-p", "-t", &target, "@aca_agent", "1"],
                vec!["set-option", "-w", "-t", &target, "remain-on-exit", "on"],
                vec!["set-option", "-w", "-t", &target, "automatic-rename", "off"],
            ] {
                a.push(";".into());
                a.extend(opt.iter().map(|x| x.to_string()));
            }
            a.push(";".into());
            a.extend(respawn(&target, &s.dir, &l, argv));
            tmux::run(&a)?;
            tmux::bind_detach();
        }
    }
    runtime::update(&s.id, |r| {
        r.clear_error();
        r.clear_ask();
        // The doorbell waits for the agent started this time to connect it itself (Claude's SessionStart)
        r.bell = false;
        r.started = paths::now();
        // Record how long the record is now: anything scheduled this time only gets written after it (see schedule::due)
        r.started_file = r.transcript.clone();
        r.started_len = std::fs::metadata(&r.transcript).map_or(0, |m| m.len());
        r.idle_stopped = 0;
        if r.status == "ended" {
            r.status = "idle".into();
        }
    });
    crate::inbox::launched(cfg, s);
    // The first time in a directory it asks whether to trust it first: if the setting is on, answer for you
    crate::trust::spawn(cfg, &s.id);
    let how = match how {
        How::Fresh => "fresh",
        How::Resume(_) => "resume",
        How::Fork(_) => "fork",
    };
    event::emit("session.started", &s.id, json!({ "how": how }));
    Ok(l.said)
}

/// Starts it if not running (resuming the old conversation); returns what was done
pub fn start(store: &Store, cfg: &Config, key: &str) -> Res<&'static str> {
    let s = store.get(key)?;
    let pane = tmux::find(&s.id);
    if pane.as_ref().is_some_and(|p| !p.dead) {
        return Ok(t!("已在运行", "already running"));
    }
    let (how, conv) = how_to_resume(cfg, &s, &runtime::load(&s.id));
    let said = match how {
        How::Resume(_) => t!("已启动,继续原对话", "started, resuming the conversation"),
        How::Fork(_) => t!(
            "已启动,从原对话重新分叉(分叉后尚未对话)",
            "started, forked again from the original conversation (the fork hadn't talked yet)"
        ),
        How::Fresh => t!("已启动,开始新对话", "started, with a new conversation"),
    };
    launch(cfg, &Session { conv, ..s }, pane.as_ref(), how)?;
    Ok(said)
}

/// Waits for `gone` to become true, at most 3 seconds; still not true by then = false
fn wait_gone(gone: impl Fn() -> bool) -> bool {
    let t = Instant::now();
    while !gone() {
        if t.elapsed() > Duration::from_secs(3) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Has the agent exit by itself (politely first, forcefully if it hasn't gone in 3 seconds). The pane stays
fn halt(p: &Pane) {
    if p.dead || p.pid == 0 {
        return;
    }
    let kill = |pid: u32, sig: libc::c_int| unsafe { libc::kill(pid as i32, sig) };
    let dead = || !tmux::run(&["display-message", "-p", "-t", &p.pane, "#{pane_dead}"]).is_ok_and(|o| o.trim() == "0");
    // In an adopted session the agent isn't the pane's root process (wrapped in a shell, or started by hand in a shell):
    // first have the agent exit cleanly, then reap the outer layer. Killing only the outer layer frees the pane, but the
    // agent may still be wrapping up, and one started in place right after would collide with it on the same conversation
    let inner = proc::agent_at(&proc::fg_kids(), p.pid).map(|a| a.1).filter(|pid| *pid != p.pid);
    if let Some(pid) = inner {
        kill(pid, libc::SIGTERM);
        if !wait_gone(|| !proc::alive(pid) || dead()) {
            kill(pid, libc::SIGKILL);
        }
        if dead() {
            return;
        }
    }
    // An interactive shell ignores SIGTERM and only goes away on hangup (the terminal is gone)
    kill(p.pid, if inner.is_some() { libc::SIGHUP } else { libc::SIGTERM });
    if !wait_gone(dead) {
        kill(p.pid, libc::SIGKILL);
    }
}

/// The conversation id goes by what the hook reported, written back to the registry (the interface and exports read the
/// registry)
fn sync_conv(store: &Store, s: &Session) {
    let r = runtime::load(&s.id);
    if !r.conv.is_empty() && r.conv != s.conv {
        let _ = store.set_conv(&s.id, &r.conv);
    }
}

pub fn stop(store: &Store, key: &str) -> Res<()> {
    let s = store.get(key)?;
    let p = tmux::find(&s.id).ok_or(t!("未运行", "not running"))?;
    halt(&p);
    sync_conv(store, &s);
    runtime::update(&s.id, |r| {
        r.status = "ended".into();
        r.idle_stopped = 0;
    });
    event::emit("session.stopped", &s.id, json!({}));
    Ok(())
}

/// One session stopped for sitting idle
#[derive(Clone, Debug, PartialEq)]
pub struct Idled {
    pub id: String,
    pub title: String,
    /// How many seconds it was idle before being stopped
    pub idle: u64,
}

/// Stops sessions idle for `after` seconds or more (`[idle] stop_after`: run on a timer while the interface is open;
/// `aca stop --idle`). Like [`stop`], the agent exits and the pane and registration remain; it's also recorded as stopped
/// for sitting idle (the list draws ☾; stopped by a person draws ■), and starting it again resumes the conversation as
/// usual. Better not to stop than to stop halfway: sessions that are unread, need you, have an error, have someone
/// attached, have background tasks, have delegated sessions still open (stopped, it would miss their notices), are
/// compacting the conversation, or couldn't resume it on restart (shell, tools that can't resume, ones never spoken to)
/// are not stopped; each one is looked at again from scratch before stopping (see [`idle_for`]). `spare`: ones the
/// caller also wants kept
pub fn stop_idle(store: &Store, cfg: &Config, after: u64, spare: &dyn Fn(&str) -> bool) -> Res<Vec<Idled>> {
    let rows = status::collect(store, cfg, true)?;
    let mut out = vec![];
    for row in candidates(&rows, paths::now(), after).into_iter().filter(|r| !spare(&r.id)) {
        let Ok(s) = store.get(&row.id) else { continue };
        let Some(idle) = idle_for(cfg, &s, after) else { continue };
        if how_to_resume(cfg, &s, &runtime::load(&s.id)).0 == How::Fresh {
            continue;
        }
        let Some(p) = tmux::find(&s.id).filter(|p| !p.dead) else { continue };
        halt(&p);
        sync_conv(store, &s);
        runtime::update(&s.id, |r| {
            r.status = "ended".into();
            r.idle_stopped = paths::now();
        });
        event::emit("session.stopped", &s.id, json!({ "idle": idle }));
        out.push(Idled { id: s.id.clone(), title: row.title.clone(), idle });
    }
    Ok(out)
}

/// Rows in the list that look stoppable (they still have to pass [`idle_for`]): idle, nobody attached, no background
/// tasks, no delegated sessions open, and last touched / last hook event `after` seconds or more ago (the real "how long
/// nothing has happened" also counts screen output and the conversation record, in [`idle_for`])
fn candidates(rows: &[status::Row], now: u64, after: u64) -> Vec<&status::Row> {
    let open_kids: HashSet<&str> =
        rows.iter().filter(|r| !matches!(r.state, State::Exited | State::Stopped)).map(|r| r.parent.as_str()).collect();
    rows.iter().filter(|r| quiet(r) && now.saturating_sub(r.active) >= after && !open_kids.contains(r.id.as_str())).collect()
}

/// The state read from the screen is idle (not running, not asking, no error), nobody is attached, no background tasks
fn quiet(r: &status::Row) -> bool {
    r.state == State::Idle && r.attached == 0 && r.bg == 0
}

/// How many seconds it has been idle; None if under `after`, or if any one thing makes it unclear it's really idle. Read
/// fresh just before stopping, and all of these must hold:
/// - The state read from the screen is idle ([`quiet`])
/// - The hook also says the turn is finished. Only the start reported (running), or waiting for a confirmation, doesn't
///   count — even if the screen is misread, it won't stop halfway. The cost: a Claude interrupted with Esc reports no
///   event and counts as running, so it's only stopped after its next turn finishes
/// - The input box is recognized and holds no unsent text of yours
/// - The last time anything happened (see [`stirred`]) is `after` seconds or more ago
/// - It has nothing scheduled for itself that's still due (Claude's CronCreate, the next `/loop`, see
///   [`crate::schedule`]): those live only in its process and would be lost on stop
/// - Its inbox has no scheduled message that isn't due yet ([`crate::inbox::scheduled`]): once stopped, it couldn't be
///   delivered when due
/// - It isn't compacting the conversation ([`compacting`]). An idle Claude compacts by itself after nearly an hour (before
///   the prompt cache expires), and then the hook state is still "finished"
fn idle_for(cfg: &Config, s: &Session, after: u64) -> Option<u64> {
    let p = tmux::find(&s.id).filter(|p| !p.dead)?;
    let r = runtime::load(&s.id);
    if !matches!(r.status.as_str(), "" | "idle" | "done") || compacting(&r, paths::now()) || !quiet(&status::one(cfg, s, true)) {
        return None;
    }
    let (agent, _) = agent::of(cfg, s).ok()?;
    if let Some(m) = agent.prompt_mark()
        && crate::inbox::draft(m, &tmux::capture_ansi(&p.pane)) != Some(false)
    {
        return None;
    }
    let now = paths::now();
    let idle = now.saturating_sub(stirred(&p, &r, s.created, now));
    (idle >= after && !crate::schedule::pending(&r, now) && crate::inbox::scheduled(&s.id).is_empty()).then_some(idle)
}

/// After a PreCompact with no finish ever seen (compaction failed, the process is gone), stop treating it as compacting
/// after this long. A real compaction mostly takes one to four minutes
const COMPACT_LONGEST: u64 = 30 * 60;

/// Compacting the conversation: start reported (PreCompact), finish not reported yet, started less than
/// [`COMPACT_LONGEST`] ago
fn compacting(r: &Run, now: u64) -> bool {
    r.compacting != 0 && now.saturating_sub(r.compacting) < COMPACT_LONGEST
}

/// For sessions whose hooks have reported events, screen output and record writes only count as activity within this long
/// of now (see [`stirred`])
const FRESH: u64 = 60;

/// The last time anything happened, the latest of: someone attached / pressed a key, this start, registration, the turn
/// start / finish the hook reported ([`Run::hook_at`]), the last write to a subagent's record (Claude; background
/// subagents keep writing after the main thread finishes). Screen output and record writes: for sessions whose hooks never
/// reported anything (shell, tools that don't report) these two are all there is; for those that did, only within
/// [`FRESH`] counts — constant refreshing most likely means working (a turn the hook didn't report, a spinner reading the
/// screen didn't recognize), while an older blip is most likely an idle Claude changing settings, a redraw after a window
/// resize, or a few lines appended to the record, and counting those would mean it never stops. Only used when the hook
/// says the turn is finished (see [`idle_for`])
fn stirred(p: &Pane, r: &Run, created: u64, now: u64) -> u64 {
    let (main, subs) = written(r);
    let screen = p.win_activity.max(main);
    let screen = if r.hook_at() == 0 || now.saturating_sub(screen) < FRESH { screen } else { 0 };
    [r.hook_at(), screen, p.activity, p.last_attached, r.started, created, subs].into_iter().max().unwrap_or(0)
}

/// When the conversation record, and the subagents' records in `subagents/` next to it (Claude), were each last written;
/// 0 if missing
fn written(r: &Run) -> (u64, u64) {
    if r.transcript.is_empty() {
        return (0, 0);
    }
    let t = Path::new(&r.transcript);
    let at = |p: &Path| {
        p.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
    };
    let subs = std::fs::read_dir(t.with_extension("").join("subagents")).into_iter().flatten().flatten().map(|e| at(&e.path()));
    (at(t), subs.max().unwrap_or(0))
}

/// Restarts in place, resuming the old conversation; `fresh` = start a new one
pub fn restart(store: &Store, cfg: &Config, key: &str, fresh: bool) -> Res<()> {
    let s = store.get(key)?;
    let pane = tmux::find(&s.id);
    if let Some(p) = &pane {
        halt(p);
    }
    let (how, conv) = how_to_resume(cfg, &s, &runtime::load(&s.id));
    let (how, conv) = if fresh { (How::Fresh, agent::of(cfg, &s)?.0.new_conv()) } else { (how, conv) };
    if fresh {
        store.set_conv(&s.id, &conv)?;
        runtime::update(&s.id, |r| {
            r.conv.clear();
            r.transcript.clear();
        });
    }
    launch(cfg, &Session { conv, ..s }, pane.as_ref(), how)
}

/// What switching accounts and carrying on ([`rehome`]) did
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rehomed {
    /// The "tool:account" now
    pub to: String,
    /// How many files were copied over (conversation record, subagents' records…); 0 = nothing to copy (never spoken to, or
    /// both accounts use the same directory)
    pub copied: usize,
    /// What changed in the arguments (for Codex, carrying the old model over), for people to read; empty = nothing changed
    pub note: String,
}

/// Turns what `--as` says into "tool:account": with a colon, as written; a single word is first taken as an account of
/// this session's tool (registered, or written as a directory), and otherwise as a tool name (with its default account)
fn rehome_spec(cfg: &Config, s: &Session, to: &str) -> String {
    let kind = agent::of(cfg, s).map(|(a, _)| a.name().to_string()).unwrap_or_default();
    match to.contains(':') || (cfg.account(&kind, to).is_none() && !agent::is_dir(to)) {
        true => to.to_string(),
        false => format!("{}:{to}", s.tool),
    }
}

/// Switching accounts and carrying on: switches to another account of the same tool (`to` is "tool:account", or just the
/// account), restarts in place and resumes this conversation. For when one account's limits run out with the work
/// unfinished: the context comes along as is, and the row, tmux session, title and note stay. The conversation record is
/// copied from the old account's config directory to the same place in the new one (what's copied: see
/// [`agent::Agent::conv_files`]), and the old account keeps its copy (billing for the earlier turns still counts against
/// it, see [`Run::moves`]). Only within the same tool: two tools' conversation records don't mix, so switching tools
/// means handing off with `H` in the interface. The turn running now is interrupted (as with a restart); a session never
/// spoken to is just restarted on the other account
pub fn rehome(store: &Store, cfg: &Config, key: &str, to: &str) -> Res<Rehomed> {
    let s = store.get(key)?;
    let (agent, _) = agent::of(cfg, &s)?;
    let p = agent::pick(cfg, &rehome_spec(cfg, &s, to))?;
    if p.agent.name() != agent.name() {
        return Err(tr!(
            "{} 不属于 {}:只能在同一工具的账号之间切换(不同工具的对话记录不互通),跨工具请用界面中的 H 交接",
            "{} isn't a {} tool: accounts can only be switched within one family (their conversations don't mix); use H in the UI to hand off",
            p.tool,
            agent.name()
        ));
    }
    let label = |tool: &str, account: &str| if account.is_empty() { tool.to_string() } else { format!("{tool}:{account}") };
    let target = label(&p.tool, &p.account);
    if p.tool == s.tool && p.account == s.account {
        return Err(tr!("已是 {target}", "it's already on {target}"));
    }
    let (from, dest) = (agent::home(cfg, &s.tool, &s.account)?, agent::home(cfg, &p.tool, &p.account)?);
    let r = runtime::load(&s.id);
    let (how, conv) = how_to_resume(cfg, &s, &r);
    let carry = match &how {
        How::Resume(c) | How::Fork(c) => c.clone(),
        How::Fresh => String::new(),
    };
    // What has to move: the two accounts' config directories differ, and this conversation has a record
    let moving = match (&from, &dest) {
        (Some(a), Some(b)) if !carry.is_empty() => (!same_dir(a, b)).then(|| (a.clone(), b.clone())),
        _ => None,
    };
    let files = match &moving {
        Some((a, _)) => {
            let f = agent.conv_files(a, &carry);
            if f.is_empty() {
                return Err(tr!(
                    "{} 中找不到对话 {carry} 的记录,无法继续",
                    "can't find the record of conversation {carry} under {}, so it can't move",
                    a.display()
                ));
            }
            f
        }
        None => vec![],
    };
    let fixed: Vec<String> = p.def.args.iter().chain(&agent::account(cfg, &*p.agent, &p.account)?.args).cloned().collect();
    let (args, note) = match &moving {
        Some((a, b)) => agent.rehome_args(&s.args, &a.join(&files[0]), a, b, &fixed),
        None => (s.args.clone(), String::new()),
    };
    // Stop first (so the record stops changing), then copy; if copying fails, stop here: the registration is still on the
    // old account, and ⏎ starts it as before
    let pane = tmux::find(&s.id);
    if let Some(x) = &pane {
        halt(x);
    }
    let mut copied = 0;
    if let Some((a, b)) = &moving {
        for rel in &files {
            copied += copy_over(&a.join(rel), &b.join(rel))
                .map_err(|e| tr!("对话记录复制失败({}):{e}", "couldn't copy the conversation ({}): {e}", rel.display()))?;
        }
    }
    store.set_account(&s.id, &p.tool, &p.account)?;
    if args != s.args {
        store.set_args(&s.id, &args)?;
    }
    if let Some((a, b)) = &moving {
        let move_ = runtime::Move { at: paths::now(), conv: carry.clone(), from: a.display().to_string(), to: b.display().to_string() };
        runtime::update(&s.id, |r| {
            // Point the record at the new account's copy (same content): until the hook reports, the interface and handoffs read it
            // too
            if let Ok(rest) = Path::new(&r.transcript).strip_prefix(a) {
                r.transcript = b.join(rest).display().to_string();
            }
            r.moves.push(move_);
        });
    }
    event::emit("session.rehomed", &s.id, json!({ "tool": p.tool, "account": p.account, "from": label(&s.tool, &s.account) }));
    launch(cfg, &Session { tool: p.tool.clone(), account: p.account.clone(), args, conv, ..s }, pane.as_ref(), how)?;
    Ok(Rehomed { to: target, copied, note })
}

/// Whether two directories are the same (written differently or reached through a link counts too)
fn same_dir(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

/// Copies `from` (a file or directory) to `to`, directories recursively, overwriting same names; returns how many files
/// were copied. For append-only files like conversation records: if the other side already has one that isn't the
/// beginning of this side's (not copied over from here earlier), keep a `.bak-<time>` first, then overwrite
fn copy_over(from: &Path, to: &Path) -> std::io::Result<usize> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        let mut n = 0;
        for e in std::fs::read_dir(from)?.flatten() {
            n += copy_over(&e.path(), &to.join(e.file_name()))?;
        }
        return Ok(n);
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if to.is_file() && !is_prefix(to, from)? {
        let mut bak = to.as_os_str().to_owned();
        bak.push(format!(".bak-{}", paths::now()));
        std::fs::rename(to, bak)?;
    }
    // Copy next to it first, then swap it in: copying halfway never leaves half a file
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".aca-tmp");
    std::fs::copy(from, &tmp)?;
    std::fs::rename(&tmp, to)?;
    Ok(1)
}

/// Whether `a`'s content is the beginning of `b` (identical counts too)
fn is_prefix(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let (la, lb) = (std::fs::metadata(a)?.len(), std::fs::metadata(b)?.len());
    if la > lb {
        return Ok(false);
    }
    let (mut fa, mut fb) = (std::io::BufReader::new(std::fs::File::open(a)?), std::io::BufReader::new(std::fs::File::open(b)?).take(la));
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = fa.read(&mut x)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

/// Branches a new session off this session's current conversation. It's another path you continue yourself, not delegated
/// by the original: no `parent` (notices, answering for it and the status bar all identify workers by `parent`); where it
/// was forked from goes in `fork_of`; it's a sibling of the original, placed right after it
pub fn fork(store: &Store, cfg: &Config, key: &str, title: &str) -> Res<Session> {
    let from = store.get(key)?;
    // The original sits under another session: place it after the topmost one (a family stays together in the list, see the
    // interface's `link_parents`)
    let mut after = from.clone();
    for _ in 0..32 {
        match store.get(&after.parent) {
            Ok(up) if up.id != after.id && up.group == after.group => after = up,
            _ => break,
        }
    }
    let r = runtime::load(&from.id);
    let (agent, _) = agent::of(cfg, &from)?;
    let conv = if r.conv.is_empty() { from.conv.clone() } else { r.conv.clone() };
    if !agent.can_fork() || conv.is_empty() {
        return Err(t!("该会话没有可分叉的对话", "this session has no conversation to fork").into());
    }
    let s = Session {
        id: fresh_id(store),
        title: if title.trim().is_empty() { tr!("{} (分叉)", "{} (fork)", status::title(&from, &r)) } else { title.trim().to_string() },
        title_locked: !title.trim().is_empty(),
        order: store.next_order(&from.group),
        conv: agent.new_conv(),
        parent: String::new(),
        fork_of: from.id.clone(),
        created: paths::now(),
        archived: 0,
        ..from
    };
    store.insert(&s)?;
    // If placing fails it doesn't matter: it stays at the end of the group and starts all the same
    let _ = store.place_after(&s.id, &after.id);
    runtime::update(&s.id, |r| {
        r.locked = s.title_locked;
        r.fork_from = conv.clone();
    });
    event::emit(
        "session.created",
        &s.id,
        json!({
            "title": s.title, "tool": s.tool, "account": s.account, "dir": s.dir, "group": s.group,
            "parent": s.parent, "fork": true, "fork_of": s.fork_of
        }),
    );
    launch(cfg, &s, None, How::Fork(conv))?;
    Ok(s)
}

/// Rename: the registry, the tmux session name and the window name change together; it no longer follows the agent's own
/// title
pub fn rename(store: &Store, key: &str, title: &str) -> Res<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(t!("名称不能为空", "the name can't be empty").into());
    }
    let s = store.get(key)?;
    store.set_title(&s.id, title, true)?;
    runtime::update(&s.id, |r| r.locked = true);
    let name = tmux::sync_name(&s.id, title).unwrap_or_default();
    event::emit("session.renamed", &s.id, json!({ "title": title, "tmux": name }));
    Ok(name)
}

/// Releases the rename lock: the title follows the agent's own again
pub fn unlock(store: &Store, key: &str) -> Res<()> {
    let s = store.get(key)?;
    store.set_title(&s.id, &s.title, false)?;
    let (_, r) = runtime::update(&s.id, |r| r.locked = false);
    if !r.auto_title.is_empty() {
        tmux::sync_name(&s.id, &r.auto_title);
    }
    Ok(())
}

/// Moves it to another group (created if missing); `.` means the group picked from its directory (see [`BY_DIR`]).
/// Returns which group it went into
pub fn move_to(store: &Store, key: &str, group: &str) -> Res<String> {
    let s = store.get(key)?;
    let group = match group.trim_matches('/') {
        BY_DIR => store.group_claim(&s.dir)?.0,
        g => g.to_string(),
    };
    store.set_group(&s.id, &group)?;
    event::emit("session.moved", &s.id, json!({ "group": group }));
    Ok(group)
}

fn kill_tmux(s: &Session) {
    if let Some(p) = tmux::find(&s.id) {
        halt(&p);
        let _ = tmux::run(&["kill-session", "-t", &p.sid]);
    }
}

/// Archive: closes the tmux session, keeps the registration (`unarchive` + `start` brings it back, with the conversation
/// intact)
pub fn archive(store: &Store, key: &str, on: bool) -> Res<()> {
    let s = store.get(key)?;
    if on {
        sync_conv(store, &s);
        kill_tmux(&s);
    }
    store.set_archived(&s.id, if on { paths::now() } else { 0 })?;
    if on {
        event::emit("session.archived", &s.id, json!({}));
    } else {
        runtime::update(&s.id, |r| r.restored = paths::now());
    }
    Ok(())
}

/// Returns the displayed title of the deleted session
/// Deletes it; returns the displayed title, and what happened to the worktree if it was started in one aca made (a clean
/// one is deleted too, the branch is kept)
pub fn remove(store: &Store, key: &str) -> Res<(String, Option<String>)> {
    let s = store.get(key)?;
    let title = status::title(&s, &runtime::load(&s.id));
    kill_tmux(&s);
    store.remove(&s.id)?;
    runtime::remove(&s.id);
    crate::inbox::remove(&s.id);
    event::emit("session.removed", &s.id, json!({ "title": title }));
    // Archived ones count as in use too: they'll carry on there once brought back
    let others: Vec<String> = store.sessions(true).unwrap_or_default().into_iter().map(|o| o.dir).collect();
    let worktree = crate::worktree::release(Path::new(&s.dir), |root| others.iter().any(|d| Path::new(d).starts_with(root)));
    Ok((title, worktree))
}

/// Lets go (the reverse of [`crate::adopt`]): the registration is deleted, the tmux session stays as is with its processes
/// running, and becomes an ordinary tmux session again. The session name isn't changed back. Returns (displayed title,
/// tmux session name; empty if not running)
pub fn release(store: &Store, key: &str) -> Res<(String, String)> {
    let s = store.get(key)?;
    let title = status::title(&s, &runtime::load(&s.id));
    let mut name = String::new();
    if let Some(p) = tmux::find(&s.id) {
        // remain-on-exit was turned on by aca: take it back, or the pane would stay around forever after it exits
        tmux::run(&[
            "set-option",
            "-u",
            "-t",
            &p.sid,
            "@aca_id",
            ";",
            "set-option",
            "-p",
            "-u",
            "-t",
            &p.pane,
            "@aca_agent",
            ";",
            "set-option",
            "-w",
            "-u",
            "-t",
            &p.pane,
            "remain-on-exit",
        ])?;
        name = p.session;
    }
    store.remove(&s.id)?;
    runtime::remove(&s.id);
    event::emit("session.removed", &s.id, json!({ "title": title, "released": true }));
    Ok((title, name))
}

/// Sends text into the session's input box and presses Enter. `wait`: if it's still running / needs you, wait for it to
/// go idle first, up to this long. Without `wait`, if it's waiting for a confirmation: don't type (Enter might pick the
/// default option right away), return an error; to answer for it, use `aca answer`
pub fn send(store: &Store, cfg: &Config, key: &str, text: &str, wait: Option<Duration>) -> Res<()> {
    let s = store.get(key)?;
    let t = Instant::now();
    let pane = loop {
        let row = status::one(cfg, &s, true);
        match (row.state, wait) {
            (State::Stopped | State::Exited, _) => return Err(t!("会话未运行", "the session isn't running").into()),
            (State::Running | State::Ask, Some(w)) if t.elapsed() < w => std::thread::sleep(Duration::from_millis(500)),
            (State::Running | State::Ask, Some(_)) => return Err(t!("会话一直未空闲,未发送", "it never went idle; not sent").into()),
            (State::Ask, None) => {
                return Err(tr!(
                    "会话正在等待确认,未发送(输入后回车可能会直接选中默认选项);如需代为回答:aca answer {}",
                    "it's waiting for a confirmation; not sent (Enter might pick the default option); to answer for it: aca answer {}",
                    s.id
                ));
            }
            _ => break row.pane,
        }
    };
    type_in(&pane, text)
}

/// Pastes into the input box, then presses Enter
fn type_in(pane: &str, text: &str) -> Res<()> {
    tmux::paste(pane, text)?;
    // Enter needs a short pause: Claude swallows an Enter right after a bracketed paste as part of the paste
    std::thread::sleep(Duration::from_millis(150));
    tmux::run(&["send-keys", "-t", pane, "Enter"]).map(|_| ())
}

/// Types into a freshly started session (handoffs, remote jobs; `aca new --say` only comes here when the tool can't take
/// the prompt as a launch argument): waits until it has drawn a stable interface (text on the screen, unchanged half a
/// second later) **and** is idle, then pastes and presses Enter. Waits at most `wait` in all. Both must hold together: the
/// startup gates (trust the folder, review hooks) count as "needs you", and right after someone answers one it's no
/// longer asking but the interface isn't up yet, so anything typed then reaches no one
pub fn send_fresh(store: &Store, cfg: &Config, key: &str, text: &str, wait: Duration) -> Res<()> {
    let s = store.get(key)?;
    let t = Instant::now();
    let mut last: Vec<String> = vec![];
    let pane = loop {
        let p = tmux::find(&s.id).filter(|p| !p.dead).ok_or(t!("会话未运行", "the session isn't running"))?;
        let now = tmux::capture(&p.pane);
        let settled = now.iter().any(|l| !l.trim().is_empty()) && now == last;
        let busy = matches!(status::one(cfg, &s, true).state, State::Running | State::Ask);
        if settled && !busy {
            break p.pane;
        }
        if t.elapsed() > wait {
            return Err(if settled {
                t!("会话一直未空闲,未发送", "it never went idle; not sent").into()
            } else {
                t!("会话一直未启动完成,未发送", "it never came up; not sent").into()
            });
        }
        last = now;
        std::thread::sleep(Duration::from_millis(500));
    };
    type_in(&pane, text)
}

/// Marks it as read
pub fn seen(store: &Store, key: &str) -> Res<()> {
    let s = store.get(key)?;
    runtime::update(&s.id, |r| r.seen = paths::now());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conversation_to_resume_is_the_one_the_hook_last_reported() {
        let cfg = Config::default();
        let cx = Session { tool: "codex".into(), conv: String::new(), ..Default::default() };
        assert_eq!(how_to_resume(&cfg, &cx, &Run { conv: "th".into(), ..Default::default() }), (How::Resume("th".into()), "th".into()));
        assert_eq!(how_to_resume(&cfg, &cx, &Run::default()), (How::Fresh, String::new()));
        // The tool was deleted from the config: can only start fresh
        let gone = Session { tool: "ccx".into(), conv: "c1".into(), ..Default::default() };
        assert_eq!(how_to_resume(&cfg, &gone, &Run::default()), (How::Fresh, "c1".into()));
    }

    #[test]
    fn only_quiet_sessions_nobody_watches_and_nothing_waits_on_are_candidates() {
        let row = |id: &str, state: State, active: u64| status::Row {
            id: id.into(),
            title: id.into(),
            state,
            tool: "claude".into(),
            account: String::new(),
            group: String::new(),
            dir: String::new(),
            parent: String::new(),
            fork_of: String::new(),
            tmux: id.into(),
            pane: String::new(),
            attached: 0,
            conv: String::new(),
            ask: String::new(),
            ran: 0,
            active,
            bg: 0,
            locked: false,
            created: 0,
            order: 0,
            tokens: Default::default(),
            idle_stopped: 0,
        };
        let now = 100_000;
        let mut rows = vec![
            row("old", State::Idle, now - 4000),
            row("new", State::Idle, now - 100),
            row("unread", State::Unread, now - 9000),
            row("asks", State::Ask, now - 9000),
            row("runs", State::Running, now - 9000),
            row("err", State::Error, now - 9000),
            row("off", State::Exited, now - 9000),
            row("watched", State::Idle, now - 9000),
            row("bg", State::Idle, now - 9000),
            // Delegated one that's still running and one that's stopped
            row("boss", State::Idle, now - 9000),
            row("worker", State::Running, now),
            row("boss2", State::Idle, now - 9000),
            row("done", State::Exited, now - 9000),
        ];
        rows[7].attached = 1;
        rows[8].bg = 2;
        rows[10].parent = "boss".into();
        rows[12].parent = "boss2".into();
        let ids = |after| candidates(&rows, now, after).iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(3600), ["old", "boss2"]);
        assert_eq!(ids(60), ["old", "new", "boss2"]);
    }

    #[test]
    fn an_agent_that_reported_counts_from_its_hooks_not_from_an_old_redraw() {
        // Started at 700, finished at 900, 960 is the reminder after idling a while; nobody touched it since (attached at 800,
        // key at 850), redrawn once at 15000
        let p = Pane { activity: 850, last_attached: 800, win_activity: 15_000, ..Default::default() };
        let done = Run { status: "done".into(), ts: 960, turn: 700, done: 900, started: 100, ..Default::default() };
        assert_eq!(stirred(&p, &done, 500, 20_000), 900);
        // The screen was refreshing just now: it may be working (a turn the hook didn't report), so count it for now
        assert_eq!(stirred(&p, &done, 500, 15_030), 15_000);
        // Someone attached and pressed a key: counts
        assert_eq!(stirred(&Pane { activity: 2000, ..p.clone() }, &done, 500, 20_000), 2000);
        // The hook never reported anything (shell, tools that don't report): only the screen to go by, however long ago
        assert_eq!(stirred(&p, &Run { started: 100, ..Default::default() }, 500, 20_000), 15_000);
    }

    #[test]
    fn a_compaction_counts_until_it_reports_done_or_half_an_hour_has_gone() {
        let at = |compacting| Run { compacting, ..Default::default() };
        assert!(!compacting(&at(0), 10_000));
        assert!(compacting(&at(10_000), 10_000));
        assert!(compacting(&at(10_000), 10_000 + 29 * 60));
        // Never saw compaction finish (failed, the process is gone): stops counting after half an hour
        assert!(!compacting(&at(10_000), 10_000 + 30 * 60));
    }

    #[test]
    fn a_transcript_counts_only_fresh_once_hooks_report_and_its_subagents_always() {
        let dir = std::env::temp_dir().join(format!("aca-written-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("c1/subagents")).unwrap();
        let t = dir.join("c1.jsonl");
        std::fs::write(&t, "{}\n").unwrap();
        let now = paths::now();
        let transcript = t.to_string_lossy().into_owned();
        let hooked = Run { status: "done".into(), ts: 1000, done: 1000, transcript: transcript.clone(), ..Default::default() };
        let unhooked = Run { transcript, ..Default::default() };
        let p = Pane::default();
        // An idle Claude appended a few lines to the record: for one that has reported events it stops counting after a while;
        // for one that never has, it always counts
        assert!((now..=now + 1).contains(&written(&hooked).0));
        assert!(stirred(&p, &hooked, 500, now) >= now);
        assert_eq!(stirred(&p, &hooked, 500, now + 120), 1000);
        assert!(stirred(&p, &unhooked, 500, now + 120) >= now);
        // Background subagents keep writing after the main thread finishes: always counts
        std::fs::write(dir.join("c1/subagents/agent-a1.jsonl"), "{}\n").unwrap();
        assert!(stirred(&p, &hooked, 500, now + 120) >= now);
        assert_eq!(written(&Run::default()), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
