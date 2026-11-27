//! Actions on the selected session: `R` restart, `X` carry on under another account, `r` rename, `m` move to another group,
//! `⇧↑` `⇧↓` move within the group (on a group, reorder groups), `f` / `F` fork, `H` hand off, `M` attach MCP, `d` delete
//! (on a group, delete the group), `A` adopt, plus `^Z` undoing a delete / stop and bringing back from / deleting for good in
//! the trash; all are functions in aca-core (`session::*`, the registry). `s` sends a reply into its input box.
//! Stop and delete wait for the processes to exit cleanly (a few seconds at most), so the run loop runs them on a background
//! thread and the interface doesn't stall.

use aca_core::answer::Unanswered;
use aca_core::session::{self, Unsent};
use aca_core::store::Store;
use aca_core::{t, tr};
use aca_records::agent::Agent;

use crate::create::{NewReq, Removed, fzf_with};
use crate::handoff::Source;

/// The session to act on
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// The session's key (id for a registered one, session name for an ordinary tmux session)
    pub name: String,
    pub title: String,
    pub profile: String,
    /// Registered session id; empty = ordinary tmux session
    pub id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// force: unused for now (kept for the confirmation path)
    Restart { t: Target, force: bool },
    /// unlock: unlock the title afterwards (`^Z` back to a title that wasn't locked; renaming always locks)
    Rename { t: Target, to: String, unlock: bool },
    /// from: its current group (for `^Z` to move it back); groups: the existing groups (differing only in case counts as the
    /// same one); the target group is created if missing. home: the directory a group created here works in, made if missing
    /// (empty = none recorded; nothing to do with a group that's already there)
    Move { t: Target, from: String, to: String, groups: Vec<String>, home: String },
    /// D: stop the agent (the pane stays with its last screen), the registration stays
    Stop { t: Target },
    /// D on a window / pane opened inside a session: stop only that pane (see park.rs)
    Park { t: Target, p: crate::park::Pane },
    /// ⏎ on such a stopped pane, ^Z undoing the above: start that pane again in place
    Respawn { t: Target, p: crate::park::Pane },
    /// s: send it a reply (pasted into the pane's input box, then Enter); key is its note's key, which holds the reply while it's
    /// written, deleted once sent. A registered one is looked at again right before (`session::say`); pane is for the others:
    /// unregistered sessions, and a window / split pane inside a session (its id is empty, so it never lands in the session's
    /// own pane)
    Send { t: Target, pane: String, text: String, key: String },
    /// s on a row waiting for you: answer it (an option's number, yes, no) — only while it still asks what the box showed
    /// (`answer::answer_for_you`)
    Answer { t: Target, reply: String, shown: aca_core::answer::Asking },
    /// f / F: fork a new session carrying the conversation. Empty title = the default "<title> (fork)", unlocked (Claude can
    /// still name it later); a given one is locked
    Fork { t: Target, title: Option<String> },
    /// M: attach / detach project-level MCPs (writes .mcp.json under dir); dir is the registered directory, tidied up afterwards
    Mcp { t: Target, dir: String, on: Vec<String>, off: Vec<String> },
    /// d: delete the session (see `create::remove`); with pane = only close a window opened inside the session / a split-off pane
    Remove { t: Target, pane: Option<String> },
    /// ⇧↑ ⇧↓: move within the group to just before / just after (after) another session (whose id is by)
    Order { t: Target, by: String, after: bool },
    /// ⇧↑ ⇧↓ on a group: move the group (with its subgroups) to just before / after its sibling by. Not aimed at any session
    GroupOrder { group: String, by: String, after: bool },
    /// H: create a new session per req (to is its agent), send it from's handoff note (see `handoff`).
    /// note: from's note was carried over; delete it once the new session is created (its key)
    Handoff { t: Target, from: Box<Source>, req: NewReq, to: Agent, note: Option<String> },
    /// An account picked in X's box: switch to it, restart in place, resume this conversation (see `session::rehome`).
    /// to is "tool:account", label is the name shown in the menu
    Rehome { t: Target, to: String, label: String },
    /// A: adopt an ordinary tmux session (not started by aca), registering it as shown in the confirmation (see `aca_core::adopt`)
    Adopt { t: Target, plan: Box<aca_core::adopt::Plan> },
    /// `^Z`: undo a stop (start it again)
    Revive { t: Target },
    /// Bring an entry back from the trash (⏎ on the `b` page, `^Z` undoing a delete, see trash.rs); start: these sessions start
    /// again (undone with ^Z, they were running before the delete). Not aimed at any session
    Restore { item: Box<crate::trash::Item>, start: Vec<String> },
    /// Delete a trash entry for good now (d y on the `b` page). Not aimed at any session
    Purge { item: Box<crate::trash::Item> },
    /// Create, delete, edit a group (see groups.rs). Not aimed at any session
    Group(crate::groups::Act),
    /// Remote tasks: test the server (see hooks.rs). Not aimed at any session
    /// The L page: install / update / remove / enable or disable a plugin (see plugins.rs). Not aimed at any session
    Plugins(crate::plugins::Act),
    /// The tasks page: make a ticket, rewrite it, hand it over, mark it done, delete it. Not aimed at any session
    Task(TaskAct),
    /// Done together after selecting several with v: the same job on several sessions (delete, stop, restart, move group; ^Z
    /// undoing them starts them again / moves them back), or on several trash entries (restore, delete for good), see `many`.
    /// Not aimed at any session
    Many(Vec<Job>),
}

/// What the tasks page does to a task ticket, all of it in your name (see `aca_core::task`)
#[derive(Clone, Debug, PartialEq)]
pub enum TaskAct {
    /// n: a new one in this group, with nobody on it yet
    Add { group: String, title: String, criteria: String },
    /// e: its title and what counts as done, rewritten
    Set { n: i64, title: String, criteria: String },
    /// m: hand it to this session (`to` its id, `name` its title); an empty `to` takes it back
    Give { n: i64, to: String, name: String },
    /// c: mark it done
    Done { n: i64 },
    /// d: delete it
    Remove { n: i64 },
}

impl TaskAct {
    fn doing(&self) -> String {
        match self {
            TaskAct::Add { title, .. } => tr!("正在新建任务「{}」…", "making task “{}”…", short(title)),
            TaskAct::Set { n, .. } => tr!("正在修改任务 #{n}…", "changing task #{n}…"),
            TaskAct::Give { n, to, .. } if to.is_empty() => tr!("正在取回任务 #{n}…", "taking task #{n} back…"),
            TaskAct::Give { n, name, .. } => tr!("正在将任务 #{n} 交给「{}」…", "handing task #{n} to “{}”…", short(name)),
            TaskAct::Done { n } => tr!("正在完成任务 #{n}…", "marking task #{n} done…"),
            TaskAct::Remove { n } => tr!("正在删除任务 #{n}…", "deleting task #{n}…"),
        }
    }

    fn run(&self, store: &Store, cfg: &aca_core::config::Config) -> Outcome {
        use aca_core::task;
        let r = match self {
            TaskAct::Add { group, title, criteria } => {
                let new = task::New { group: group.clone(), title: title.clone(), criteria: criteria.clone(), ..Default::default() };
                task::add(store, cfg, None, new).map(|t| tr!("已新建任务 #{}", "made task #{}", t.n))
            }
            TaskAct::Set { n, title, criteria } => {
                task::set(store, cfg, None, *n, task::Changes { title: Some(title), criteria: Some(criteria), ..Default::default() })
                    .map(|t| tr!("已修改任务 #{}", "changed task #{}", t.n))
            }
            TaskAct::Give { n, to, name } => task::give(store, cfg, None, *n, to, "", None).map(|t| match to.is_empty() {
                true => tr!("已取回任务 #{}", "took task #{} back", t.n),
                false => tr!("已将任务 #{} 交给「{}」", "handed task #{} to “{}”", t.n, short(name)),
            }),
            TaskAct::Done { n } => task::done(store, cfg, None, Some(*n), "").map(|t| tr!("任务 #{} 已完成", "task #{} is done", t.n)),
            TaskAct::Remove { n } => task::remove(store, cfg, None, *n).map(|t| tr!("已删除任务 #{}", "deleted task #{}", t.n)),
        };
        match r {
            Ok(msg) => Outcome::Done(msg),
            Err(e) => Outcome::Failed(e),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done(String),
    Failed(String),
    /// A new session was created (fork, handoff): the highlight follows. id is its session id
    Made {
        msg: String,
        id: String,
    },
    /// Handoff: the new session is created but the handoff note wasn't sent; it's left in its note (note)
    Handed {
        msg: String,
        id: String,
        note: String,
    },
    /// A session / group deleted with d went to the trash (see trash.rs): the interface records this entry so ^Z can undo it
    Binned {
        msg: String,
        item: Box<crate::trash::Item>,
    },
    /// `Job::Many`: each one's outcome, matching its jobs one to one (deletes don't come here; they merge into one `Binned`)
    Many(Vec<Outcome>),
}

impl Job {
    /// The session it's aimed at; reordering groups has none
    pub fn target(&self) -> Option<&Target> {
        Some(match self {
            Job::Restart { t, .. }
            | Job::Rename { t, .. }
            | Job::Move { t, .. }
            | Job::Stop { t }
            | Job::Park { t, .. }
            | Job::Respawn { t, .. }
            | Job::Send { t, .. }
            | Job::Answer { t, .. }
            | Job::Fork { t, .. }
            | Job::Mcp { t, .. }
            | Job::Remove { t, .. }
            | Job::Order { t, .. }
            | Job::Handoff { t, .. }
            | Job::Rehome { t, .. }
            | Job::Adopt { t, .. }
            | Job::Revive { t, .. } => t,
            Job::GroupOrder { .. }
            | Job::Restore { .. }
            | Job::Purge { .. }
            | Job::Group(_)
            | Job::Plugins(_)
            | Job::Task(_)
            | Job::Many(_) => {
                return None;
            }
        })
    }

    /// What the bottom bar shows while it runs
    pub fn doing(&self) -> String {
        match self {
            Job::Restart { t, .. } => tr!("正在重启「{}」…", "restarting “{}”…", short(&t.title)),
            Job::Rename { to, .. } => tr!("正在重命名为「{}」…", "renaming to “{}”…", short(to)),
            Job::Move { t, to, .. } => tr!("正在将「{}」移到 {to}…", "moving “{}” to {to}…", short(&t.title)),
            Job::Stop { t } => tr!("正在停止「{}」…", "stopping “{}”…", short(&t.title)),
            Job::Park { p, .. } => tr!("正在停止「{}」…", "stopping “{}”…", short(&p.label)),
            Job::Respawn { p, .. } => tr!("正在重启「{}」…", "restarting “{}”…", short(&p.label)),
            Job::Send { t, .. } => tr!("正在发送给「{}」…", "sending to “{}”…", short(&t.title)),
            Job::Answer { t, .. } => tr!("正在回答「{}」…", "answering “{}”…", short(&t.title)),
            Job::Fork { t, .. } => tr!("正在分叉「{}」…", "forking “{}”…", short(&t.title)),
            Job::Mcp { t, .. } => tr!("正在修改「{}」的 MCP…", "changing the MCPs of “{}”…", short(&t.title)),
            Job::Remove { t, pane: Some(_) } => tr!("正在关闭「{}」…", "closing “{}”…", short(&t.title)),
            Job::Remove { t, pane: None } => tr!("正在删除「{}」…", "deleting “{}”…", short(&t.title)),
            Job::Order { t, .. } => tr!("正在移动「{}」…", "moving “{}”…", short(&t.title)),
            Job::GroupOrder { group, .. } => tr!("正在移动分组 {}…", "moving group {}…", short(group)),
            Job::Handoff { t, .. } => tr!("正在交接「{}」…", "handing off “{}”…", short(&t.title)),
            Job::Rehome { t, label, .. } => tr!("正在将「{}」切换到 {label}…", "moving “{}” to {label}…", short(&t.title)),
            Job::Adopt { t, .. } => tr!("正在接管「{}」…", "adopting “{}”…", short(&t.title)),
            Job::Revive { t, .. } => tr!("正在撤销「{}」…", "undoing “{}”…", short(&t.title)),
            Job::Restore { item, .. } => tr!("正在恢复{}…", "restoring {}…", item.name()),
            Job::Purge { item } => tr!("正在永久删除{}…", "deleting {} for good…", item.name()),
            Job::Group(a) => a.doing(),
            Job::Plugins(a) => a.doing(),
            Job::Task(a) => a.doing(),
            Job::Many(jobs) => {
                let n = jobs.len();
                match jobs.first() {
                    Some(Job::Plugins(a)) => a.doing(),
                    Some(Job::Remove { .. }) => tr!("正在删除 {n} 个…", "deleting {n}…"),
                    Some(Job::Stop { .. }) => tr!("正在停止 {n} 个…", "stopping {n}…"),
                    Some(Job::Restart { .. }) => tr!("正在重启 {n} 个…", "restarting {n}…"),
                    Some(Job::Revive { .. }) => tr!("正在撤销停止 {n} 个…", "undoing the stop of {n}…"),
                    Some(Job::Restore { .. }) => tr!("正在恢复 {n} 条…", "restoring {n}…"),
                    Some(Job::Purge { .. }) => tr!("正在永久删除 {n} 条…", "deleting {n} for good…"),
                    Some(Job::Move { to, .. }) if jobs.iter().all(|j| matches!(j, Job::Move { to: x, .. } if x == to)) => {
                        tr!("正在将 {n} 个移到 {to}…", "moving {n} to {to}…")
                    }
                    _ => tr!("正在移动 {n} 个…", "moving {n}…"),
                }
            }
        }
    }
}

/// What to call several sessions together: one is "<title>", several are "<first>" and n-1 more. In Chinese no space goes
/// before it (「已删除{}」 (deleted {}))
pub fn named(first: &str, n: usize) -> String {
    let more = n.saturating_sub(1);
    match n {
        0 | 1 => tr!("「{}」", "“{}”", short(first)),
        _ => tr!("「{}」等 {n} 个", "“{}” and {more} more", short(first)),
    }
}

/// What the bottom bar says when `Job::Many` is done: how many worked, and why the first failed one didn't
pub fn many_msg(jobs: &[Job], outs: &[Outcome]) -> String {
    let ok = outs.iter().filter(|o| matches!(o, Outcome::Done(_))).count();
    let bad: Vec<&str> = outs.iter().filter_map(|o| if let Outcome::Failed(e) = o { Some(e.as_str()) } else { None }).collect();
    let n = bad.len();
    if ok == 0 {
        return match bad.first() {
            Some(e) if n > 1 => tr!("{n} 个均失败:{e}", "none of the {n} worked: {e}"),
            Some(e) => e.to_string(),
            None => String::new(),
        };
    }
    let mut msg = match jobs.first() {
        Some(Job::Plugins(crate::plugins::Act::Enable(_, on))) if n == 0 => return crate::channels::switched(*on),
        Some(Job::Plugins(_)) => tr!("已修改 {ok} 个插件", "changed {ok} plugins"),
        Some(Job::Stop { .. }) => tr!("已停止 {ok} 个", "stopped {ok}"),
        Some(Job::Restart { .. }) => tr!("已重启 {ok} 个", "restarted {ok}"),
        Some(Job::Revive { .. }) => tr!("已撤销停止:{ok} 个已重新启动", "undid the stop: {ok} started again"),
        Some(Job::Restore { .. }) => tr!("已恢复 {ok} 条", "restored {ok}"),
        Some(Job::Purge { .. }) => tr!("已永久删除 {ok} 条", "deleted {ok} for good"),
        Some(Job::Move { to, .. }) if jobs.iter().all(|j| matches!(j, Job::Move { to: x, .. } if x == to)) => {
            tr!("已将 {ok} 个移到 {to}", "moved {ok} to {to}")
        }
        _ => tr!("已移动 {ok} 个", "moved {ok}"),
    };
    if let Some(e) = bad.first() {
        msg += &tr!(" · {n} 个失败:{e}", " · {n} failed: {e}");
    }
    msg
}

/// What the bottom bar says once answered: allowed / denied for a permission prompt, otherwise the option picked (`2 JSON 文件`
/// (2 JSON file))
pub fn answered_msg(title: &str, reply: &str, said: &str) -> String {
    match reply {
        "yes" => tr!("已允许「{}」", "allowed “{}”", short(title)),
        "no" => tr!("已拒绝「{}」", "denied “{}”", short(title)),
        _ => tr!("已回答「{}」:{said}", "answered “{}”: {said}", short(title)),
    }
}

/// The answer box (or the answer it sent) no longer matches what the session asks: nothing was pressed
pub fn changed_msg() -> String {
    t!("问题变了,没回答", "the question changed; nothing answered").into()
}

/// A reply not sent because it started asking you something: what to press to answer it
pub fn asking_msg() -> String {
    tr!("未发送:{} · s 回答", "not sent: {} · s answers it", t!("正在等待回应", "it's waiting for your choice"))
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(16).collect();
    if s.chars().count() > 16 {
        out.push('…');
    }
    out
}

pub fn run(job: &Job) -> Outcome {
    let store = match Store::open() {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    let cfg = aca_core::config::load();
    match job {
        Job::Restart { t, .. } => match session::restart(&store, &cfg, &t.id, false) {
            Ok(()) => Outcome::Done(tr!("已重启「{}」", "restarted “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("重启失败:{e}", "restart failed: {e}")),
        },
        Job::Rename { t, to, unlock } => match session::rename(&store, &t.id, to) {
            Ok(_) if *unlock => match session::unlock(&store, &t.id) {
                Ok(()) => Outcome::Done(tr!("已改名为「{}」", "renamed to “{}”", short(to))),
                Err(e) => Outcome::Failed(tr!("已重命名,但解锁失败:{e}", "renamed, but unlocking failed: {e}")),
            },
            Ok(_) => Outcome::Done(tr!("已改名为「{}」", "renamed to “{}”", short(to))),
            Err(e) => Outcome::Failed(tr!("改名失败:{e}", "rename failed: {e}")),
        },
        Job::Move { t, to, groups, home, .. } => {
            // Differing only in case counts as the same: move into the existing one
            let (path, made) = match groups.iter().find(|g| g.eq_ignore_ascii_case(to)) {
                Some(g) => (g.clone(), false),
                None => (to.clone(), true),
            };
            if made
                && !home.is_empty()
                && let Err(e) = group_at_home(&store, &path, home)
            {
                return Outcome::Failed(tr!("移动分组失败:{e}", "moving group failed: {e}"));
            }
            match session::move_to(&store, &t.id, &path) {
                Ok(_) => Outcome::Done(tr!(
                    "已将「{}」移到{} {path}",
                    "moved “{}” to{} {path}",
                    short(&t.title),
                    if made { t!("新分组", " new group") } else { "" }
                )),
                Err(e) => Outcome::Failed(tr!("移动分组失败:{e}", "moving group failed: {e}")),
            }
        }
        Job::Stop { t } => match session::stop(&store, &t.id) {
            Ok(()) => Outcome::Done(tr!("已停止「{}」", "stopped “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("停止失败:{e}", "stop failed: {e}")),
        },
        Job::Park { p, .. } => match crate::park::park(p) {
            Ok(()) => Outcome::Done(tr!("已停止「{}」", "stopped “{}”", short(&p.label))),
            Err(e) => Outcome::Failed(tr!("停止失败:{e}", "stop failed: {e}")),
        },
        Job::Respawn { p, .. } => match crate::park::respawn(p) {
            Ok(()) => Outcome::Done(tr!("已重启「{}」", "restarted “{}”", short(&p.label))),
            Err(e) => Outcome::Failed(tr!("重启失败:{e}", "restart failed: {e}")),
        },
        Job::Send { t, pane, text, .. } => {
            let sent = match t.id.as_str() {
                "" => crate::data::send_into(pane, text).map_err(String::from),
                id => match session::say(&store, &cfg, id, text) {
                    Ok(()) => Ok(()),
                    Err(Unsent::Asking) => return Outcome::Failed(asking_msg()),
                    Err(Unsent::Stopped) => {
                        return Outcome::Failed(tr!("未发送:{}", "not sent: {}", t!("未运行 · ⏎ 启动", "not running · ⏎ starts it")));
                    }
                    Err(Unsent::Failed(e)) => Err(e),
                },
            };
            match sent {
                Ok(()) => Outcome::Done(tr!("已发给「{}」", "sent to “{}”", short(&t.title))),
                Err(e) => Outcome::Failed(tr!("发送失败:{e}", "not sent: {e}")),
            }
        }
        Job::Answer { t, reply, shown } => match aca_core::answer::answer_for_you(&store, &cfg, &t.id, reply, shown) {
            Ok(said) => Outcome::Done(answered_msg(&t.title, reply, &said)),
            Err(Unanswered::Changed) => Outcome::Failed(changed_msg()),
            Err(Unanswered::Failed(e)) => Outcome::Failed(tr!("未回答:{e}", "not answered: {e}")),
        },
        Job::Fork { t, title } => match session::fork(&store, &cfg, &t.id, title.as_deref().unwrap_or("")) {
            Ok(s) => Outcome::Made { msg: tr!("已分叉「{}」", "forked “{}”", short(&s.title)), id: s.id },
            Err(e) => Outcome::Failed(tr!("分叉失败:{e}", "fork failed: {e}")),
        },
        Job::Mcp { dir, on, off, .. } => crate::mcp::apply(dir, on, off),
        Job::Remove { t, pane } => {
            // Closing a pane (the session stays) or deleting the session: the English verbs inflect differently, so the
            // sentences are written separately
            let close = pane.is_some();
            match crate::create::remove(t, pane.as_deref()) {
                Ok(Removed::Archived { was, group }) => {
                    let s = crate::trash::Gone { id: t.id.clone(), title: t.title.clone(), group, archived: was };
                    let item = Box::new(crate::trash::Item::session(&t.profile, s));
                    Outcome::Binned { msg: tr!("已删除「{}」 · ^Z 撤销", "deleted “{}” · ^Z undoes", short(&t.title)), item }
                }
                Ok(Removed::Gone) if close => Outcome::Done(tr!("已关闭「{}」", "closed “{}”", short(&t.title))),
                Ok(Removed::Gone) => Outcome::Done(tr!("已删除「{}」", "deleted “{}”", short(&t.title))),
                Err(e) if close => Outcome::Failed(tr!("关闭失败:{e}", "close failed: {e}")),
                Err(e) => Outcome::Failed(tr!("删除失败:{e}", "delete failed: {e}")),
            }
        }
        Job::Order { t, by, after } => order(&store, t, by, *after),
        Job::GroupOrder { group, by, after } => group_order(&store, group, by, *after),
        Job::Handoff { from, req, to, .. } => crate::handoff::run(from, req, *to),
        Job::Rehome { t, to, label } => match session::rehome(&store, &cfg, &t.id, to) {
            Ok(r) => {
                let mut msg = match r.copied {
                    0 => tr!("已切换到 {label}", "moved to {label}"),
                    _ => tr!("已切换到 {label} · 发送「继续」即可接着做", "moved to {label} · say “continue” to carry on"),
                };
                if !r.note.is_empty() {
                    msg = format!("{msg} · {}", r.note);
                }
                Outcome::Done(msg)
            }
            Err(e) => Outcome::Failed(tr!("换账号失败:{e}", "switching accounts failed: {e}")),
        },
        Job::Adopt { plan, .. } => match aca_core::adopt::apply(&store, plan) {
            // The process was left alone and doesn't carry aca's hooks yet: only a restart gives it unread (finished, not seen)
            Ok((s, _)) => Outcome::Made {
                msg: tr!(
                    "已接管「{}」 · R 重启后才能识别任务完成",
                    "adopted “{}” · restart (R) so aca can tell when it finishes",
                    short(plan.title())
                ),
                id: s.id,
            },
            Err(e) => Outcome::Failed(tr!("接管失败:{e}", "adopt failed: {e}")),
        },
        Job::Revive { t } => match crate::create::start(&t.id) {
            Ok(()) => Outcome::Done(tr!("已撤销停止:「{}」已重新启动", "undid the stop: “{}” started again", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("撤销失败:{e}", "undo failed: {e}")),
        },
        Job::Restore { item, start } => restore(&store, item, start),
        Job::Purge { item } => {
            let n = crate::trash::drop_now(&store, item);
            match (item.group.is_empty(), n) {
                (true, _) | (false, 0) => Outcome::Done(tr!("已永久删除{}", "deleted {} for good", item.name())),
                _ => Outcome::Done(tr!("已永久删除{}及其中 {n} 个会话", "deleted {} and its {n} sessions for good", item.name())),
            }
        }
        Job::Group(a) => crate::groups::job(a),
        Job::Plugins(a) => crate::plugins::job(a),
        Job::Task(a) => a.run(&store, &cfg),
        Job::Many(jobs) => many(jobs),
    }
}

/// A batch selected with v: each does its own, all waited on together (stop and delete wait for processes to exit cleanly; one
/// by one would take several rounds); moves go one after another (a group to be created is created once), trash entries one
/// after another too (restoring recreates groups and their order; writing together would garble it). Deletes merge into one
/// trash entry and ^Z brings them back together
fn many(jobs: &[Job]) -> Outcome {
    if !jobs.is_empty() && jobs.iter().all(|j| matches!(j, Job::Remove { pane: None, .. })) {
        return remove_many(jobs);
    }
    // Plugin switches share the registry and plugins.toml, so write them in order.
    if jobs.iter().any(|j| matches!(j, Job::Move { .. } | Job::Restore { .. } | Job::Purge { .. } | Job::Plugins(_))) {
        return Outcome::Many(jobs.iter().map(run).collect());
    }
    Outcome::Many(std::thread::scope(|sc| {
        let hs: Vec<_> = jobs.iter().map(|j| sc.spawn(move || run(j))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Outcome::Failed("panic".into()))).collect()
    }))
}

/// Several sessions deleted together: each archived (waiting together for processes to exit cleanly), the deleted ones recorded
/// as one trash entry; the ones not deleted are reported as such and stay in the list
fn remove_many(jobs: &[Job]) -> Outcome {
    let ts: Vec<&Target> = jobs.iter().filter_map(Job::target).collect();
    let got: Vec<Result<Removed, String>> = std::thread::scope(|sc| {
        let hs: Vec<_> = ts.iter().map(|&t| sc.spawn(move || crate::create::remove(t, None))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("panic".into()))).collect()
    });
    let mut gone = vec![];
    let mut bad = vec![];
    for (t, r) in ts.iter().zip(got) {
        match r {
            Ok(Removed::Archived { was, group }) => {
                gone.push(crate::trash::Gone { id: t.id.clone(), title: t.title.clone(), group, archived: was })
            }
            Ok(Removed::Gone) => {}
            Err(e) => bad.push(e),
        }
    }
    let n = bad.len();
    if gone.is_empty() {
        return Outcome::Failed(match bad.first() {
            Some(e) if n > 1 => tr!("{n} 个均未删除:{e}", "none of the {n} were deleted: {e}"),
            Some(e) => tr!("删除失败:{e}", "delete failed: {e}"),
            None => t!("删除失败", "delete failed").to_string(),
        });
    }
    let item = crate::trash::Item::sessions(&ts[0].profile, gone);
    let mut msg = tr!("已删除{} · ^Z 撤销", "deleted {} · ^Z undoes", item.name());
    if let Some(e) = bad.first() {
        msg += &tr!(" · {n} 个未删除:{e}", " · {n} weren't deleted: {e}");
    }
    Outcome::Binned { msg, item: Box::new(item) }
}

/// Bring a trash entry back: recreate the groups as recorded (if one of the same name was created meanwhile, go into it without
/// changing its settings), unarchive (ones already archived before the delete stay so), start the ones in start again. Ones
/// deleted for good elsewhere meanwhile are skipped and reported
fn restore(store: &Store, item: &crate::trash::Item, start: &[String]) -> Outcome {
    let had: Vec<String> = store.groups().unwrap_or_default().into_iter().map(|g| g.path).collect();
    // A note deleted on its own: back in its notebook. Its group deleted meanwhile is recreated, as for a session (a note in a
    // group that isn't there shows nowhere, until a group of that name is made again)
    if let Some(n) = &item.note {
        let what = item.name();
        if store.note_unbin(&n.id).is_err() {
            return Outcome::Done(tr!("{what}已被永久删除,无法恢复", "{what} is gone for good; can't restore"));
        }
        if !n.group.is_empty() && !had.contains(&n.group) {
            let _ = store.group_create(&n.group, "");
        }
        return Outcome::Done(tr!("已恢复{what}", "restored {what}"));
    }
    for g in &item.groups {
        if had.contains(&g.path) {
            continue;
        }
        if let Err(e) = store.group_create(&g.path, "") {
            return Outcome::Failed(tr!("恢复失败:{e}", "restore failed: {e}"));
        }
        let _ = store.group_order(&g.path, g.order);
        // If the directory became another group's default meanwhile: don't take it
        if !g.dir.is_empty() {
            let _ = store.group_set_dir(&g.path, &g.dir);
        }
    }
    // The notes deleted along with it are back in their notebooks, beside any written there meanwhile. An entry from before
    // notebooks carries the group's one note as text: it becomes a note (once: bringing back may be tried again)
    let _ = store.notes_unbin_with(&item.id);
    for g in item.groups.iter().filter(|g| !g.note.trim().is_empty()) {
        if !store.notes_in(&g.path).is_ok_and(|v| v.iter().any(|n| n.text == g.note)) {
            let _ = store.note_add(&g.path, &g.note, "");
        }
    }
    let (mut back, mut lost, mut cold) = (0, 0, vec![]);
    for s in item.sessions.iter().filter(|s| !s.archived) {
        if !store.get(&s.id).is_ok_and(|x| x.id == s.id) {
            lost += 1;
            continue;
        }
        // A session deleted on its own: if its group was deleted meanwhile, recreate it (otherwise it has no place to go)
        if !s.group.is_empty() && !had.contains(&s.group) && !item.groups.iter().any(|g| g.path == s.group) {
            let _ = store.group_create(&s.group, "");
        }
        if let Err(e) = session::archive(store, &s.id, false) {
            return Outcome::Failed(tr!("恢复失败:{e}", "restore failed: {e}"));
        }
        back += 1;
        if start.contains(&s.id)
            && let Err(e) = crate::create::start(&s.id)
        {
            cold.push(e);
        }
    }
    let what = item.name();
    let mut msg = match (item.group.is_empty(), back) {
        (true, 0) => return Outcome::Done(tr!("{what}已被永久删除,无法恢复", "{what} is gone for good; can't restore")),
        (true, _) | (false, 0) => tr!("已恢复{what}", "restored {what}"),
        (false, n) => tr!("已恢复{what} · {n} 个会话", "restored {what} · {n} sessions"),
    };
    // A group, or several deleted together: some were deleted for good meanwhile; say so
    if lost > 0 && (!item.group.is_empty() || item.sessions.len() > 1) {
        msg += &tr!(" · {lost} 个已被永久删除", " · {lost} already gone for good");
    }
    // The registration is back, it just didn't start: counts as restored (undoing again wouldn't start it either); say so
    if let Some(e) = cold.first() {
        msg += &tr!(" · 启动失败:{e}", " · didn't start: {e}");
    }
    Outcome::Done(msg)
}

/// Renumber ids in the given order (1, 2, 3...): order within a group and the order of groups are both kept this way
fn renumber(ids: &[String], set: impl Fn(&str, i64) -> Result<(), String>) -> Result<(), String> {
    ids.iter().enumerate().try_for_each(|(i, id)| set(id, i as i64 + 1))
}

/// The order after moving `me` to just before / after `by`; either one missing = None
fn moved(mut v: Vec<String>, me: &str, by: &str, after: bool) -> Option<Vec<String>> {
    let at = v.iter().position(|x| x == me)?;
    let x = v.remove(at);
    let to = v.iter().position(|x| x == by)? + usize::from(after);
    v.insert(to, x);
    Some(v)
}

/// ⇧↑ ⇧↓: move to just before / after by and renumber the whole group (archived ones included, keeping their relative places).
/// Says nothing on success: the list has already moved
fn order(store: &Store, t: &Target, by: &str, after: bool) -> Outcome {
    let run = || -> Result<(), String> {
        let (me, it) = (store.get(&t.id)?, store.get(by)?);
        if me.group != it.group {
            return Err(t!("已不在同一分组", "they're no longer in the same group").into());
        }
        let mut all: Vec<_> = store.sessions(true)?.into_iter().filter(|s| s.group == me.group).collect();
        all.sort_by_key(|s| (s.order, s.created));
        let ids = moved(all.into_iter().map(|s| s.id).collect(), &me.id, &it.id, after)
            .ok_or(t!("找不到目标位置", "can't find where to move it"))?;
        renumber(&ids, |id, n| store.set_order(id, n))
    };
    match run() {
        Ok(()) => Outcome::Done(String::new()),
        Err(e) => Outcome::Failed(tr!("移动失败:{e}", "moving failed: {e}")),
    }
}

/// ⇧↑ ⇧↓ on a group: move to just before / after its sibling by; subgroups follow (where a subgroup sits depends only on its
/// parent). All siblings are renumbered; other levels keep their numbers (they only compare with their own siblings). Says
/// nothing on success
fn group_order(store: &Store, group: &str, by: &str, after: bool) -> Outcome {
    let Some(sibs) = crate::data::group_siblings(group) else {
        return Outcome::Failed(tr!("移动分组失败:分组 {group} 不存在", "moving group failed: no group {group}"));
    };
    let Some(ids) = moved(sibs, group, by, after) else {
        return Outcome::Failed(tr!(
            "移动分组失败:{by} 和 {group} 不在同一级",
            "moving group failed: {by} and {group} aren't on the same level"
        ));
    };
    match renumber(&ids, |g, n| store.group_order(g, n)) {
        Ok(()) => Outcome::Done(String::new()),
        Err(e) => Outcome::Failed(tr!("移动分组失败:{e}", "moving group failed: {e}")),
    }
}

/// Tidy a typed group name: trim / and whitespace at both ends, whitespace becomes - (a group name must be typeable as is on
/// the command line), empty levels go. Nothing left = None
pub fn clean_group(name: &str) -> Option<String> {
    let parts: Vec<String> =
        name.split('/').map(|p| p.split_whitespace().collect::<Vec<_>>().join("-")).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A group `m` creates, working in `dir`: the directory is made if it's missing, then the group with it as its default.
/// A directory that can't be made, or that another group has, leaves no group behind
fn group_at_home(store: &Store, group: &str, dir: &str) -> Result<(), String> {
    let abs = aca_core::paths::expand(dir);
    std::fs::create_dir_all(&abs)
        .map_err(|e| tr!("无法创建目录 {}:{e}", "can't make directory {}: {e}", aca_records::words::tilde(&abs)))?;
    store.group_create_in(group, &aca_core::paths::real(&abs))
}

/// What a name typed into `m`'s list means
#[derive(Clone, Debug, PartialEq)]
pub enum Typed {
    /// A group that's already there, as it's written in the registry
    Existing(String),
    /// A group to create, and the directory it would work in (empty = none recorded)
    New { group: String, dir: String },
}

/// A name typed into `m`'s list (or the row picked there) → the group it means. The name of a group that's there, in any
/// case, is that group. Otherwise it's a new one: a bare name goes under the group being moved out of (`under`; empty when
/// the ones moved aren't in one group), a name with a `/` in it is the whole path from the top, so `/x` is `x` at the top.
/// A new group works in `<its parent's directory>/<its own name>`; no parent, or a parent with no directory = none.
/// `groups`: every group with where it works. Nothing left of the name once tidied = None
pub fn typed_group(typed: &str, under: &str, groups: &[(String, String)]) -> Option<Typed> {
    let name = clean_group(typed)?;
    let there = |path: &str| groups.iter().find(|(g, _)| g.eq_ignore_ascii_case(path)).map(|(g, _)| Typed::Existing(g.clone()));
    if let Some(t) = there(&name) {
        return Some(t);
    }
    let path = if typed.contains('/') || under.is_empty() { name } else { format!("{under}/{name}") };
    if let Some(t) = there(&path) {
        return Some(t);
    }
    let dir = path.rsplit_once('/').and_then(|(up, last)| {
        let (_, d) = groups.iter().find(|(g, d)| g == up && !d.is_empty())?;
        Some(format!("{}/{last}", d.trim_end_matches('/')))
    });
    Some(Typed::New { group: path, dir: dir.unwrap_or_default() })
}

/// A group in the `m` list
#[derive(Clone, Debug, PartialEq)]
pub struct GroupRow {
    pub group: String,
    /// Where this group works: its recorded default directory, else the directory most of its sessions are in; neither = empty
    pub dir: String,
    /// Same directory as the session being moved
    pub same: bool,
}

/// `m`: pick the group to move into with fzf. Moving changes only the group, the session keeps running in its directory, so
/// each row carries that group's directory and a mismatch shows at a glance.
/// Searches group names only (a typed new name won't get matched by another group's directory); when the typed name matches
/// nothing, Enter = create the group it means (`typed_group`). What that would be, and the directory it would work in, is
/// written above the header as the name is typed; Tab then asks for the directory before anything is created.
/// what: what is being moved (see `named`); here: its directory (empty when the several being moved aren't in one directory);
/// under, groups: as for `typed_group`.
/// Returns the row picked or the name typed, and whether it was Tab that ended it
pub fn pick_group(
    rows: &[GroupRow],
    what: &str,
    here: &str,
    under: &str,
    groups: &[(String, String)],
) -> Result<Option<(String, bool)>, String> {
    let head = if here.is_empty() {
        tr!("移动{what}", "move {what}")
    } else {
        tr!("移动{what} · 只换分组,目录不变:{}", "move {what} · only the group changes, it stays in {}", aca_records::words::tilde(here))
    };
    let header = format!("--header={head}");
    let mut args = vec![
        "--ansi",
        "--delimiter=\t",
        "--with-nth=2..",
        "--nth=1",
        "--accept-nth=1",
        t!("--prompt=移到 ", "--prompt=move to "),
        &header,
        "--bind=enter:accept-or-print-query",
    ];
    // The two bindings ask this program (see `typed_says`): only it knows what a typed name would make. If it can't say where it
    // is, the list works without them: nothing shown ahead, Tab steps through the candidates
    let mut env = vec![];
    if let Some(me) = std::env::current_exe().ok().and_then(|p| p.to_str().map(String::from)) {
        args.extend([
            r#"--bind=result:transform-header:"$ACA_SELF" ui --typed-group"#,
            r#"--bind=tab:transform:"$ACA_SELF" ui --typed-group tab"#,
        ]);
        let all: Vec<String> = groups.iter().map(|(g, d)| format!("{g}\x1f{d}")).collect();
        env = vec![("ACA_SELF", me), ("ACA_MOVE_HEAD", head), ("ACA_MOVE_UNDER", under.to_string()), ("ACA_MOVE_GROUPS", all.join("\x1e"))];
    }
    let got = fzf_with(&group_lines(rows), &args, &env)?;
    Ok(got.map(|out| match out.split_once('\n') {
        Some((TAB, name)) => (name.to_string(), true),
        _ => (out, false),
    }))
}

/// First line of what `m`'s fzf prints when Tab ended it; the typed name follows on the next line
const TAB: &str = "tab";

/// `aca ui --typed-group [tab]`: run by `m`'s fzf (see `pick_group`) each time its results change and when Tab is pressed, with
/// its query and match count in the environment
pub fn typed_says(tab: bool) -> String {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let groups: Vec<(String, String)> =
        var("ACA_MOVE_GROUPS").split('\x1e').filter_map(|l| l.split_once('\x1f')).map(|(g, d)| (g.to_string(), d.to_string())).collect();
    let typed = if var("FZF_MATCH_COUNT") == "0" { typed_group(&var("FZF_QUERY"), &var("ACA_MOVE_UNDER"), &groups) } else { None };
    // The header starts two columns in
    let room = var("FZF_COLUMNS").parse::<usize>().map_or(usize::MAX, |c| c.saturating_sub(2));
    typed_answer(tab, typed.as_ref(), &var("ACA_MOVE_HEAD"), room)
}

/// What fzf is told. Tab: the action to take — on a name that would make a new group, end with that name for its directory to
/// be asked; otherwise step through the candidates as it always did. Not Tab: the header — on such a name, a line above the
/// usual one saying which group it makes and where it would work; when that doesn't fit in `room` columns, the directory
/// goes on a line of its own
fn typed_answer(tab: bool, typed: Option<&Typed>, head: &str, room: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let Some(Typed::New { group, dir }) = typed else {
        return if tab { "up".into() } else { head.into() };
    };
    if tab {
        return format!("become:printf '{TAB}\\n%s' {{q}}");
    }
    let (b, d, off) = ("\x1b[1m", "\x1b[2m", "\x1b[0m");
    let (what, key) = (t!("新分组", "new group"), t!("Tab 改目录", "Tab directory"));
    let dir = aca_records::words::tilde(dir);
    let width = format!("+ {group}  {what}  {dir}   {key}").width();
    match (dir.is_empty(), width <= room) {
        (true, _) => format!("+ {b}{group}{off}  {what}   {key}\n{head}"),
        (false, true) => format!("+ {b}{group}{off}  {what}  {d}{dir}{off}   {key}\n{head}"),
        (false, false) => format!("+ {b}{group}{off}  {what}   {key}\n  {d}{dir}{off}\n{head}"),
    }
}

/// Three fields per line, Tab-separated: the group name (printed when selected), the displayed name (padded to one width so
/// the Tab after it lines up), the directory
fn group_lines(rows: &[GroupRow]) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let w = rows.iter().map(|r| r.group.width()).max().unwrap_or(0);
    let (b, d, green, off) = ("\x1b[1m", "\x1b[2m", "\x1b[32m", "\x1b[0m");
    rows.iter()
        .map(|r| {
            let pad = " ".repeat(w - r.group.width());
            let dir = aca_records::words::tilde(&r.dir);
            let dir = match (r.dir.is_empty(), r.same) {
                (true, _) => String::new(),
                (false, true) => format!("{green}{dir}  {}{off}", t!("同目录", "same directory")),
                (false, false) => format!("{d}{dir}{off}"),
            };
            format!("{}\t{b}{}{off}{pad}  \t{dir}", r.group, r.group)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_brought_back_has_its_notes_again_beside_any_written_meanwhile() {
        use crate::trash::{GroupRec, Item};
        let store = Store::memory();
        store.group_create("fun/x", "").unwrap();
        store.note_add("fun", "整体的想法", "").unwrap();
        let rec = |path: &str, note: &str| GroupRec { path: path.into(), order: 1, dir: String::new(), space: None, note: note.into() };
        // The entry of an older version carries the one note a group had as text (here: fun/x's)
        let item =
            Item { id: "x1".into(), group: "fun".into(), groups: vec![rec("fun", ""), rec("fun/x", "旧版的便签")], ..Item::default() };
        store.notes_bin_group("fun", &item.id).unwrap();
        store.group_drop("fun").unwrap();
        // A fun made meanwhile got a note of its own
        store.group_create("fun", "").unwrap();
        store.note_add("fun", "新的", "").unwrap();
        let texts = |g: &str| {
            let mut v: Vec<String> = store.notes_in(g).unwrap().into_iter().map(|n| n.text).collect();
            v.sort();
            v
        };
        assert_eq!(texts("fun"), ["新的"]);
        assert!(matches!(restore(&store, &item, &[]), Outcome::Done(_)));
        assert_eq!(texts("fun"), ["整体的想法", "新的"]);
        assert_eq!(texts("fun/x"), ["旧版的便签"]);
        // Brought back once more (the first try stopped halfway): not a second copy
        restore(&store, &item, &[]);
        assert_eq!((texts("fun").len(), texts("fun/x").len()), (2, 1));
        // An entry deleted for good takes the notes deleted along with it, and no others
        store.notes_bin_group("fun/x", "x2").unwrap();
        crate::trash::drop_now(&store, &Item { id: "x2".into(), ..item.clone() });
        assert_eq!(store.notes().unwrap().len(), 2);
        assert_eq!(store.notes_unbin_with("x2").unwrap(), 0);
    }

    /// A note deleted on its own is an entry of the trash page too
    #[test]
    fn a_note_deleted_on_its_own_comes_back_and_its_group_too_if_that_went_meanwhile() {
        use crate::trash::Item;
        let store = Store::memory();
        store.group_create("fun", "").unwrap();
        let a = store.note_add("fun", "发版窗口\n周二", "").unwrap();
        let b = store.note_add("fun", "另一条", "").unwrap();
        store.note_bin(&a.id).unwrap();
        // `aca group rm` deletes the group's notes each on its own
        store.group_remove("fun").unwrap();
        let gone = store.notes_gone().unwrap();
        assert_eq!(gone.len(), 2);
        let item = |id: &str| Item::note(gone.iter().find(|n| n.id == id).unwrap());
        assert_eq!(item(&a.id).name(), "笔记「发版窗口」");
        assert_eq!(restore(&store, &item(&a.id), &[]), Outcome::Done("已恢复笔记「发版窗口」".into()));
        assert!(store.groups().unwrap().iter().any(|g| g.path == "fun"));
        assert_eq!(store.notes_in("fun").unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>(), std::slice::from_ref(&a.id));
        // Deleted for good: only while it's still deleted
        crate::trash::drop_now(&store, &item(&a.id));
        assert_eq!(store.notes_in("fun").unwrap().len(), 1);
        crate::trash::drop_now(&store, &item(&b.id));
        assert!(store.notes_gone().unwrap().is_empty());
        assert_eq!(restore(&store, &item(&b.id), &[]), Outcome::Done("笔记「另一条」已被永久删除,无法恢复".into()));
    }

    #[test]
    fn a_batch_says_how_many_worked_and_why_the_first_failed_one_did_not() {
        let t = |s: &str| Target { name: s.into(), title: s.into(), profile: "default".into(), id: format!("id-{s}") };
        let stops = [Job::Stop { t: t("a") }, Job::Stop { t: t("b") }, Job::Stop { t: t("c") }];
        let ok = || Outcome::Done(String::new());
        let bad = |e: &str| Outcome::Failed(e.into());
        assert_eq!(many_msg(&stops, &[ok(), ok(), ok()]), "已停止 3 个");
        assert_eq!(many_msg(&stops, &[ok(), bad("停止失败:x"), bad("停止失败:y")]), "已停止 1 个 · 2 个失败:停止失败:x");
        assert_eq!(many_msg(&stops, &[bad("停止失败:x"), bad("停止失败:y"), bad("z")]), "3 个均失败:停止失败:x");
        assert_eq!(many_msg(&stops[..1], &[bad("停止失败:x")]), "停止失败:x");
        let mv = |s: &str, to: &str| Job::Move { t: t(s), from: "f".into(), to: to.into(), groups: vec![], home: String::new() };
        assert_eq!(many_msg(&[mv("a", "g"), mv("b", "g")], &[ok(), ok()]), "已将 2 个移到 g");
        assert_eq!(many_msg(&[mv("a", "g"), mv("b", "h")], &[ok(), ok()]), "已移动 2 个");
        assert_eq!(Job::Many(stops.to_vec()).doing(), "正在停止 3 个…");
        assert_eq!(named("一个很长很长很长很长很长很长的标题", 3), "「一个很长很长很长很长很长很长的标…」等 3 个");
        assert_eq!(named("a", 1), "「a」");
    }

    #[test]
    fn typed_group_names_are_tidied() {
        assert_eq!(clean_group(" fun/new thing "), Some("fun/new-thing".into()));
        assert_eq!(clean_group("/a//b/"), Some("a/b".into()));
        assert_eq!(clean_group("工作  台"), Some("工作-台".into()));
        assert_eq!(clean_group(" / "), None);
    }

    #[test]
    fn a_typed_name_is_a_group_thats_there_or_a_new_one_under_the_one_moved_out_of() {
        let groups: Vec<(String, String)> = [("fun", "/w/fun"), ("fun/manga", ""), ("Zeta", ""), ("home", "/h/")]
            .iter()
            .map(|(g, d)| (g.to_string(), d.to_string()))
            .collect();
        let new = |group: &str, dir: &str| Some(Typed::New { group: group.into(), dir: dir.into() });
        let there = |g: &str| Some(Typed::Existing(g.into()));
        let typed = |name: &str, under: &str| typed_group(name, under, &groups);
        // Bare: under the group moved out of, in a directory of that name under its directory
        assert_eq!(typed("rss", "home"), new("home/rss", "/h/rss"));
        assert_eq!(typed(" new  thing ", "fun"), new("fun/new-thing", "/w/fun/new-thing"));
        // Its parent has no directory, or there's nothing to go under: no directory
        assert_eq!(typed("x", "Zeta"), new("Zeta/x", ""));
        assert_eq!(typed("x", "fun/manga"), new("fun/manga/x", ""));
        assert_eq!(typed("x", ""), new("x", ""));
        // A / in it: the whole path from the top, wherever it's typed from
        assert_eq!(typed("fun/x", "home"), new("fun/x", "/w/fun/x"));
        assert_eq!(typed("/x", "home"), new("x", ""));
        assert_eq!(typed("a/b", "home"), new("a/b", ""));
        // A group that's there, written in any case, top level or under this one: that group, as the registry writes it
        assert_eq!(typed("zeta", "fun"), there("Zeta"));
        assert_eq!(typed("Manga", "fun"), there("fun/manga"));
        assert_eq!(typed("FUN/manga", ""), there("fun/manga"));
        assert_eq!(typed(" / ", "fun"), None);
    }

    #[test]
    fn ms_list_is_told_what_a_new_name_makes_and_tab_asks_its_directory_only_then() {
        let head = "移动「x」";
        let new = Typed::New { group: "home/rss".into(), dir: "/h/rss".into() };
        // The header: the new group's line above the usual one ("+ home/rss  新分组  /h/rss   Tab 改目录" is 39 columns)
        assert_eq!(
            typed_answer(false, Some(&new), head, 39),
            "+ \x1b[1mhome/rss\x1b[0m  新分组  \x1b[2m/h/rss\x1b[0m   Tab 改目录\n移动「x」"
        );
        // Too narrow for that: the directory on its own line
        assert_eq!(
            typed_answer(false, Some(&new), head, 38),
            "+ \x1b[1mhome/rss\x1b[0m  新分组   Tab 改目录\n  \x1b[2m/h/rss\x1b[0m\n移动「x」"
        );
        let bare = Typed::New { group: "rss".into(), dir: String::new() };
        assert_eq!(typed_answer(false, Some(&bare), head, 10), "+ \x1b[1mrss\x1b[0m  新分组   Tab 改目录\n移动「x」");
        // Tab: fzf ends with the mark and the name as typed
        assert_eq!(typed_answer(true, Some(&new), head, 80), "become:printf 'tab\\n%s' {q}");
        // Something matches, nothing typed, or the name is a group that's there: the usual header, and Tab steps as before
        for typed in [None, Some(Typed::Existing("fun".into()))] {
            assert_eq!(typed_answer(false, typed.as_ref(), head, 80), head);
            assert_eq!(typed_answer(true, typed.as_ref(), head, 80), "up");
        }
    }

    #[test]
    fn a_group_m_creates_gets_its_directory_made_and_none_is_left_when_it_cant_be() {
        let store = Store::memory();
        let root = std::env::temp_dir().join(format!("aca-ops-home-{}-{}", std::process::id(), aca_core::paths::short_id()));
        let dir = root.join("home/rss");
        let at = |p: &std::path::Path| p.to_string_lossy().into_owned();
        group_at_home(&store, "home/rss", &at(&dir)).unwrap();
        assert!(dir.is_dir());
        let default = |g: &str| store.groups().unwrap().into_iter().find(|x| x.path == g).map(|x| x.default_path);
        assert_eq!(default("home/rss"), Some(aca_core::paths::real(&at(&dir))));
        assert_eq!(default("home"), Some(String::new()));
        // The directory is another group's: no group
        assert!(group_at_home(&store, "feeds", &at(&dir)).unwrap_err().contains("home/rss"));
        assert_eq!(default("feeds"), None);
        // It can't be made (a file is in the way): no group either
        std::fs::write(root.join("file"), "").unwrap();
        assert!(group_at_home(&store, "blocked", &at(&root.join("file/sub"))).unwrap_err().starts_with("无法创建目录"));
        assert_eq!(default("blocked"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn group_rows_show_the_name_padded_then_the_directory() {
        let row = |group: &str, dir: &str, same| GroupRow { group: group.into(), dir: dir.into(), same };
        let lines = group_lines(&[row("fun/aca", "/x/aca", true), row("w", "/x/w", false), row("空", "", false)]);
        assert_eq!(
            lines,
            [
                "fun/aca\t\x1b[1mfun/aca\x1b[0m  \t\x1b[32m/x/aca  同目录\x1b[0m",
                "w\t\x1b[1mw\x1b[0m        \t\x1b[2m/x/w\x1b[0m",
                // A Chinese character takes two columns: one space less padding
                "空\t\x1b[1m空\x1b[0m       \t",
            ]
        );
    }

    #[test]
    fn moving_takes_itself_out_first_then_lands_next_to_the_other() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        // a b S c d
        assert_eq!(moved(v("a b S c d"), "S", "b", false).unwrap(), v("a S b c d"));
        assert_eq!(moved(v("a b S c d"), "S", "a", false).unwrap(), v("S a b c d"));
        assert_eq!(moved(v("a b S c d"), "S", "c", true).unwrap(), v("a b c S d"));
        assert_eq!(moved(v("a b S c d"), "S", "d", true).unwrap(), v("a b c d S"));
        // Already there: unchanged
        assert_eq!(moved(v("a b S c d"), "S", "b", true).unwrap(), v("a b S c d"));
        assert_eq!(moved(v("a b S c d"), "S", "c", false).unwrap(), v("a b S c d"));
        assert!(moved(v("a b"), "x", "a", false).is_none());
        assert!(moved(v("a b"), "a", "x", false).is_none());
    }

    #[test]
    fn reordering_renumbers_a_group_and_leaves_other_groups_alone() {
        let store = Store::memory();
        let mk = |id: &str, group: &str, order: i64| aca_core::store::Session {
            id: id.into(),
            title: id.into(),
            tool: "shell".into(),
            group: group.into(),
            order,
            dir: "/".into(),
            created: 1,
            ..Default::default()
        };
        for s in [mk("a", "g", 5), mk("b", "g", 9), mk("c", "g", 12), mk("x", "other", 1)] {
            store.insert(&s).unwrap();
        }
        let t = Target { name: "c".into(), title: "c".into(), profile: "default".into(), id: "c".into() };
        assert_eq!(order(&store, &t, "a", false), Outcome::Done(String::new()));
        let got: Vec<(String, i64)> = ["c", "a", "b", "x"].iter().map(|id| (id.to_string(), store.get(id).unwrap().order)).collect();
        assert_eq!(got, [("c".to_string(), 1), ("a".to_string(), 2), ("b".to_string(), 3), ("x".to_string(), 1)]);
        // Not in the same group: no move
        assert_eq!(order(&store, &t, "x", false), Outcome::Failed("移动失败:已不在同一分组".into()));
    }
}
