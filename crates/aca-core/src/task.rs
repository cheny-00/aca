//! Task tickets: a thing to do in a group, handed to one session at a time (the table is in store.rs). Here is who may do
//! what to one, the words passed on when one changes hands or is done, and the events.
//!
//! What waits for what, and what to hand out next, is for the group's router to work out. Nothing here decides for it:
//! tickets are kept, shown, and word is passed. A ticket's line (`note`) is whatever was written there.
//!
//! Who may do what (`me`: the session acting, None = you):
//!
//! | | you | a router (any group's) | the session doing it | another session |
//! |---|---|---|---|---|
//! | make, hand over, take back, retitle | yes | yes | | |
//! | rewrite its line | yes | yes | its own | |
//! | mark done | yes | | its own | |
//! | delete | yes | | | |
//!
//! A router doesn't mark done: it hands work out, it doesn't sign it off.

use serde_json::{Value, json};

use crate::config::Config;
use crate::paths::Res;
use crate::store::{Session, Store, Task, TaskState};
use crate::{event, inbox, t, tr};

/// What a new ticket is made from. `to`: the session it's handed to right away (empty = nobody yet), `say`: what's said
/// handing it over
#[derive(Clone, Debug, Default)]
pub struct New {
    pub group: String,
    pub title: String,
    pub criteria: String,
    pub note: String,
    pub to: String,
    pub say: String,
    pub watch: bool,
}

/// What an act on a ticket sends out once the registry has it
#[derive(Clone, Debug, PartialEq)]
enum Out {
    Event { name: &'static str, session: String, data: Value },
    /// Words for a session, in the name of whoever acts. `must`: the act is of little use if they don't get through
    Word { to: String, text: String, must: bool },
}

/// The session this command runs in (None = outside any: you)
pub fn me(store: &Store) -> Option<Session> {
    std::env::var("ACA_SESSION_ID").ok().filter(|x| !x.is_empty()).and_then(|x| store.get(&x).ok())
}

/// A ticket's number as typed: `9` or `#9`
pub fn number(text: &str) -> Res<i64> {
    let digits = text.strip_prefix('#').unwrap_or(text);
    digits.parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(|| tr!("{text} 不是任务单号", "{text} is not a task number"))
}

/// How the state is written for scripts
pub fn state_name(s: TaskState) -> &'static str {
    match s {
        TaskState::Todo => "todo",
        TaskState::Doing => "doing",
        TaskState::Done => "done",
    }
}

/// A ticket for scripts: its fields, and the state they add up to
pub fn json(t: &Task) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or_default();
    v["state"] = json!(state_name(t.state()));
    v
}

/// Keep a router's handoff verbatim, including leading and trailing whitespace.
pub fn said<'a>(store: &Store, me: Option<&Session>, text: &'a str) -> &'a str {
    if me.is_some_and(|s| is_router(store, s)) { text } else { text.trim() }
}

fn id_of(me: Option<&Session>) -> &str {
    me.map_or("", |s| s.id.as_str())
}

pub(crate) fn is_router(store: &Store, s: &Session) -> bool {
    store.routers().is_ok_and(|v| v.iter().any(|(_, id)| *id == s.id))
}

/// You and routers make tickets and hand them over
fn hands_out(store: &Store, me: Option<&Session>) -> Res<()> {
    match me {
        Some(s) if !is_router(store, s) => {
            Err(t!("只有你和 router 可以新建任务、把任务交给会话", "only you and routers make tasks and hand them to sessions").into())
        }
        _ => Ok(()),
    }
}

fn event_of(name: &'static str, t: &Task, me: Option<&Session>, more: Value) -> Out {
    let mut data = json!({ "n": t.n, "group": t.group, "title": t.title, "by": id_of(me) });
    if let (Some(d), Value::Object(m)) = (data.as_object_mut(), more) {
        d.extend(m);
    }
    Out::Event { name, session: t.session.clone(), data }
}

/// Words for a session: none for nobody, none for oneself
fn word(me: Option<&Session>, to: &str, text: String, must: bool) -> Option<Out> {
    (!to.is_empty() && to != id_of(me)).then(|| Out::Word { to: to.into(), text, must })
}

/// Send out what an act produced: the events, and the words in the name of whoever acted. Words that had to get through
/// and didn't: the act stands (it's in the registry), and the error says so
fn send(store: &Store, cfg: &Config, me: Option<&Session>, t: &Task, outs: Vec<Out>) -> Res<()> {
    let mut failed = None;
    for o in outs {
        match o {
            Out::Event { name, session, data } => event::emit(name, &session, data),
            Out::Word { to, text, must } => {
                let what = match (must, t.state()) {
                    (true, _) => "",
                    (false, TaskState::Done) => "task.done",
                    _ => "task",
                };
                let msg = inbox::Msg { text, what: what.into(), ..Default::default() };
                if let Err(e) = inbox::send_message(store, cfg, me.cloned(), &to, msg)
                    && must
                {
                    failed.get_or_insert((to, e));
                }
            }
        }
    }
    match failed {
        Some((to, e)) => Err(tr!("任务 #{} 已记下,但未能通知 {to}:{e}", "task #{} is recorded, but {to} couldn't be told: {e}", t.n)),
        None => Ok(()),
    }
}

/// What the session a ticket is handed to is told: the ticket, what was said handing it over, and how to say it's done
pub fn brief(t: &Task) -> String {
    let mut v = vec![tr!("任务 #{}:{}", "Task #{}: {}", t.n, t.title)];
    if !t.criteria.is_empty() {
        v.push(tr!("完成标准:{}", "Done when: {}", t.criteria));
    }
    if !t.say.is_empty() {
        v.push(t.say.clone());
    }
    v.push(tr!("做完后运行:aca task done {} \"一句话说结果\"", "When it's done, run: aca task done {} \"the result in a sentence\"", t.n));
    v.join("\n")
}

/// The group's router, when it has one that isn't the session acting
fn router_of(store: &Store, group: &str, me: Option<&Session>) -> Option<String> {
    let routers = store.routers().ok()?;
    routers.into_iter().find(|(g, id)| g == group && id != id_of(me)).map(|(_, id)| id)
}

/// The session a ticket can be handed to: registered and not archived
fn doer(store: &Store, key: &str) -> Res<Session> {
    let s = store.get(key)?;
    if s.archived > 0 {
        return Err(tr!("会话 {} 已归档", "session {} is archived", s.id));
    }
    Ok(s)
}

/// A ticket's line is one line
fn line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Make a ticket. Handed to a session right away, that session is told; with nobody on it, the group's router is (unless
/// it made the ticket itself): it's the one to hand it out
pub fn add(store: &Store, cfg: &Config, me: Option<&Session>, n: New) -> Res<Task> {
    let (t, outs) = add_in(store, me, n)?;
    send(store, cfg, me, &t, outs).map(|()| t)
}

fn add_in(store: &Store, me: Option<&Session>, n: New) -> Res<(Task, Vec<Out>)> {
    hands_out(store, me)?;
    let group = n.group.trim_matches('/');
    if !store.groups()?.iter().any(|g| g.path == group) {
        return Err(tr!("没有分组 {group}", "no group {group}"));
    }
    let title = n.title.trim();
    if title.is_empty() {
        return Err(t!("任务需要一个名字(-t 名字)", "a task needs a title (-t TITLE)").into());
    }
    let to = if n.to.is_empty() { None } else { Some(doer(store, &n.to)?) };
    let new = Task {
        group: group.into(),
        title: title.into(),
        criteria: n.criteria.trim().into(),
        watch: n.watch,
        note: line(&n.note),
        session: to.as_ref().map(|s| s.id.clone()).unwrap_or_default(),
        by: id_of(me).into(),
        given_by: if to.is_some() { id_of(me).into() } else { String::new() },
        say: if to.is_some() { said(store, me, &n.say).into() } else { String::new() },
        ..Task::default()
    };
    let t = store.task_add(&new)?;
    let mut outs = vec![event_of("task.added", &t, me, json!({}))];
    match &to {
        Some(s) => {
            outs.push(event_of("task.given", &t, me, json!({ "say": t.say })));
            outs.extend(word(me, &s.id, brief(&t), true));
        }
        None => {
            if let Some(router) = router_of(store, &t.group, me) {
                let mut v = vec![tr!("#{} 新任务:{}", "#{} new task: {}", t.n, t.title)];
                if !t.criteria.is_empty() {
                    v.push(tr!("完成标准:{}", "Done when: {}", t.criteria));
                }
                if !t.note.is_empty() {
                    v.push(t.note.clone());
                }
                v.push(t!("还没有会话在做。", "No session has it yet.").into());
                outs.extend(word(me, &router, v.join("\n"), false));
            }
        }
    }
    Ok((t, outs))
}

/// Hand a ticket to a session (`to` empty or `-`: take it back, it waits again), saying `say`. The session is told the
/// ticket; the one that had it is told it no longer does. A done ticket handed over is open again
pub fn give(store: &Store, cfg: &Config, me: Option<&Session>, n: i64, to: &str, say: &str, watch: Option<bool>) -> Res<Task> {
    let (t, outs) = store.with_watch(n, watch, || give_in(store, me, n, to, say))?;
    send(store, cfg, me, &t, outs).map(|()| t)
}

fn give_in(store: &Store, me: Option<&Session>, n: i64, to: &str, say: &str) -> Res<(Task, Vec<Out>)> {
    hands_out(store, me)?;
    let was = store.task(n)?;
    let to = match to {
        "" | "-" => None,
        key => Some(doer(store, key)?),
    };
    let to_id = to.as_ref().map(|s| s.id.clone()).unwrap_or_default();
    store.task_give(n, &to_id, id_of(me), said(store, me, say))?;
    let t = store.task(n)?;
    let mut outs = vec![event_of("task.given", &t, me, json!({ "say": t.say }))];
    // Whoever had it stops. Not one that had finished it: nothing of theirs changes
    if was.state() == TaskState::Doing && was.session != to_id {
        let gone = match &to {
            Some(s) => tr!("任务 #{n} 已交给「{}」,不用再做。", "Task #{n} went to “{}”: no need to go on with it.", s.title),
            None => tr!("任务 #{n} 已取回,不用再做。", "Task #{n} was taken back: no need to go on with it."),
        };
        outs.extend(word(me, &was.session, gone, false));
    }
    if let Some(s) = &to {
        outs.extend(word(me, &s.id, brief(&t), true));
    }
    Ok((t, outs))
}

/// May this ticket go to a session about to be started (`aca new --task`)? Returns it
pub fn may_start(store: &Store, me: Option<&Session>, n: i64) -> Res<Task> {
    hands_out(store, me)?;
    store.task(n)
}

/// A ticket is the new session's from its first message on (`aca new --task`): recorded as handed over, with nothing
/// passed on — its opening message carries the ticket (see [`brief`])
pub(crate) fn start_with(
    store: &Store,
    cfg: &Config,
    me: Option<&Session>,
    n: i64,
    session: &str,
    say: &str,
    watch: Option<bool>,
) -> Res<Task> {
    hands_out(store, me)?;
    let (old, t) = store.assign_start(n, session, id_of(me), said(store, me, say), watch)?;
    let outs = vec![event_of("task.given", &t, me, json!({ "say": t.say }))];
    send(store, cfg, me, &t, outs).map(|()| old)
}

/// The one ticket a session is doing, for when it names none
fn mine(store: &Store, s: &Session) -> Res<Task> {
    let mut v: Vec<Task> = store.tasks()?.into_iter().filter(|t| t.state() == TaskState::Doing && t.session == s.id).collect();
    match v.len() {
        1 => Ok(v.remove(0)),
        0 => Err(t!("这个会话手上没有任务", "this session has no task").into()),
        n => Err(tr!("这个会话手上有 {n} 张任务,请写单号", "this session has {n} tasks: name the one")),
    }
}

/// Mark a ticket done, with what there is to say of the result. `n` None: the one the session acting is doing. Word goes
/// to the group's router, or with none to the session that handed the ticket over: what comes next is theirs to decide
pub fn done(store: &Store, cfg: &Config, me: Option<&Session>, n: Option<i64>, result: &str) -> Res<Task> {
    let (t, outs) = store.edit_tasks(|| done_in(store, me, n, result))?;
    send(store, cfg, me, &t, outs).map(|()| t)
}

fn done_in(store: &Store, me: Option<&Session>, n: Option<i64>, result: &str) -> Res<(Task, Vec<Out>)> {
    let was = match (n, me) {
        (Some(n), _) => store.task(n)?,
        (None, Some(s)) => mine(store, s)?,
        (None, None) => return Err(t!("需要任务单号", "missing a task number").into()),
    };
    if me.is_some_and(|s| s.id != was.session) {
        return Err(t!("只有你和做它的会话可以把任务标成完成", "only you and the session doing a task mark it done").into());
    }
    if was.state() == TaskState::Done {
        return Err(tr!("任务 #{} 已经完成", "task #{} is done already", was.n));
    }
    store.task_done(was.n, result.trim())?;
    let t = store.task(was.n)?;
    let mut data = json!({ "result": t.result });
    let routers = store.task_routers(&t.session, Some(t.n))?;
    if !routers.is_empty() {
        data["routers"] = json!(routers);
    }
    let mut outs = vec![event_of("task.done", &t, me, data)];
    if !t.watch && !store.tasks()?.iter().any(|other| other.group == t.group && other.state() == TaskState::Todo) {
        return Ok((t, outs));
    }
    let to = recipient(store, &t);
    if let Some(to) = to {
        let mut text = tr!("#{} 完成:{}", "#{} done: {}", t.n, t.title);
        if !t.result.is_empty() {
            text = format!("{text}\n{}", t.result);
        }
        outs.extend(word(me, &to, text, false));
    }
    Ok((t, outs))
}

pub(crate) fn recipient(store: &Store, t: &Task) -> Option<String> {
    router_of(store, &t.group, None).or_else(|| store.get(&t.given_by).ok().filter(|s| s.archived == 0).map(|s| s.id))
}

#[derive(Default)]
pub struct Changes<'a> {
    pub title: Option<&'a str>,
    pub criteria: Option<&'a str>,
    pub note: Option<&'a str>,
    pub watch: Option<bool>,
}

/// Rewrite what's given of a ticket's title, what counts as done, and its line. The line is also the doing session's to
/// rewrite; the rest is for whoever hands tickets out
pub fn set(store: &Store, cfg: &Config, me: Option<&Session>, n: i64, changes: Changes<'_>) -> Res<Task> {
    if changes.watch.is_some() {
        hands_out(store, me)?;
    }
    let (t, outs) = store.with_watch(n, changes.watch, || set_in(store, me, n, changes.title, changes.criteria, changes.note))?;
    send(store, cfg, me, &t, outs).map(|()| t)
}

fn set_in(
    store: &Store,
    me: Option<&Session>,
    n: i64,
    title: Option<&str>,
    criteria: Option<&str>,
    note: Option<&str>,
) -> Res<(Task, Vec<Out>)> {
    let was = store.task(n)?;
    let own_line = title.is_none() && criteria.is_none() && me.is_some_and(|s| s.id == was.session);
    if !own_line {
        hands_out(store, me)?;
    }
    let title = title.map(str::trim);
    if title.is_some_and(str::is_empty) {
        return Err(t!("任务的名字不能为空", "a task's title can't be empty").into());
    }
    let note = note.map(line);
    store.task_set(n, title, criteria.map(str::trim), note.as_deref())?;
    let t = store.task(n)?;
    let outs = vec![event_of("task.changed", &t, me, json!({}))];
    Ok((t, outs))
}

/// Delete a ticket for good: yours alone to do
pub fn remove(store: &Store, cfg: &Config, me: Option<&Session>, n: i64) -> Res<Task> {
    let (t, outs) = remove_in(store, me, n)?;
    send(store, cfg, me, &t, outs).map(|()| t)
}

fn remove_in(store: &Store, me: Option<&Session>, n: i64) -> Res<(Task, Vec<Out>)> {
    if me.is_some() {
        return Err(t!("只有你可以删除任务", "only you delete tasks").into());
    }
    let t = store.task(n)?;
    store.task_remove(n)?;
    let outs = vec![event_of("task.removed", &t, me, json!({}))];
    Ok((t, outs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, group: &str) -> Session {
        Session { id: id.into(), title: format!("会话{id}"), group: group.into(), tool: "shell".into(), dir: "/p".into(), ..Default::default() }
    }

    /// work has the router r1 and the sessions w1, w2; other has o1 and no router. Nothing here writes a file: the acts
    /// are tested up to what they'd send out
    fn fixture() -> (Store, impl Fn(&str) -> Session) {
        let store = Store::memory();
        for (id, g) in [("r1", "work"), ("w1", "work"), ("w2", "work"), ("o1", "other")] {
            store.insert(&s(id, g)).unwrap();
        }
        store.group_set_router("work", "r1").unwrap();
        let all = store.sessions(true).unwrap();
        (store, move |id: &str| all.iter().find(|x| x.id == id).cloned().unwrap())
    }

    fn new(group: &str, title: &str) -> New {
        New { group: group.into(), title: title.into(), ..New::default() }
    }

    /// The words among what was sent out: (to, text, whether they had to get through)
    fn words(outs: &[Out]) -> Vec<(&str, &str, bool)> {
        outs.iter()
            .filter_map(|o| match o {
                Out::Word { to, text, must } => Some((to.as_str(), text.as_str(), *must)),
                Out::Event { .. } => None,
            })
            .collect()
    }

    /// The events among it: (name, the session it's about, who acted)
    fn events(outs: &[Out]) -> Vec<(&str, &str, &str)> {
        outs.iter()
            .filter_map(|o| match o {
                Out::Event { name, session, data } => Some((*name, session.as_str(), data["by"].as_str().unwrap_or("?"))),
                Out::Word { .. } => None,
            })
            .collect()
    }

    #[test]
    fn you_and_routers_make_tickets_and_hand_them_over_and_nobody_else_does() {
        let (store, get) = fixture();
        let (r1, w1, o1) = (get("r1"), get("w1"), get("o1"));
        // Made by you, by a router; not by any other session
        let a = add_in(&store, None, new("work", " 修告警 ")).unwrap().0;
        let more = New { criteria: "告警响".into(), note: "等  #1\n改同一个文件".into(), ..new("/work/", "加面板") };
        let b = add_in(&store, Some(&r1), more).unwrap().0;
        assert_eq!((a.n, a.title.as_str(), a.by.as_str(), a.state()), (1, "修告警", "", TaskState::Todo));
        assert_eq!((b.n, b.by.as_str(), b.note.as_str(), b.group.as_str()), (2, "r1", "等 #1 改同一个文件", "work"));
        assert!(add_in(&store, Some(&w1), new("work", "x")).unwrap_err().contains("router"));
        assert!(add_in(&store, None, new("nope", "x")).unwrap_err().contains("nope"));
        assert!(add_in(&store, None, new("work", "  ")).is_err());
        // Handing over, taking back, retitling: the same people. A router of one group hands out another group's too
        assert!(give_in(&store, Some(&w1), 1, "w1", "").unwrap_err().contains("router"));
        assert!(give_in(&store, Some(&o1), 1, "o1", "").is_err());
        let t = give_in(&store, Some(&r1), 1, "o1", "你来").unwrap().0;
        assert_eq!((t.session.as_str(), t.given_by.as_str(), t.say.as_str(), t.state()), ("o1", "r1", "你来", TaskState::Doing));
        assert!(give_in(&store, None, 1, "nobody", "").is_err());
        assert!(set_in(&store, Some(&w1), 1, Some("改名"), None, None).is_err());
        let (t, outs) = set_in(&store, Some(&r1), 1, Some("修告警规则"), Some("响一次"), None).unwrap();
        assert_eq!((t.title.as_str(), t.criteria.as_str(), events(&outs)), ("修告警规则", "响一次", vec![("task.changed", "o1", "r1")]));
        // Its line: also the session doing it, its own only, and nothing else of the ticket
        assert_eq!(set_in(&store, Some(&o1), 1, None, None, Some("卡在\n权限")).unwrap().0.note, "卡在 权限");
        assert!(set_in(&store, Some(&w1), 1, None, None, Some("x")).is_err());
        assert!(set_in(&store, Some(&o1), 1, Some("x"), None, Some("x")).is_err());
        assert!(set_in(&store, None, 1, Some(" "), None, None).is_err());
        // Deleting: you alone
        assert!(remove_in(&store, Some(&r1), 2).unwrap_err().contains("只有你"));
        let (t, outs) = remove_in(&store, None, 2).unwrap();
        assert_eq!((t.n, events(&outs)), (2, vec![("task.removed", "", "")]));
        assert!(store.task(2).is_err());
        assert!(may_start(&store, Some(&w1), 1).is_err() && may_start(&store, Some(&r1), 9).is_err());
        assert_eq!(may_start(&store, Some(&r1), 1).unwrap().n, 1);
    }

    #[test]
    fn done_is_for_you_and_the_session_doing_it_and_a_router_does_not_sign_off() {
        let (store, get) = fixture();
        let (r1, w1, w2) = (get("r1"), get("w1"), get("w2"));
        add_in(&store, Some(&r1), New { to: "w1".into(), ..new("work", "修告警") }).unwrap();
        add_in(&store, Some(&r1), New { to: "w2".into(), ..new("work", "加面板") }).unwrap();
        add_in(&store, Some(&r1), new("work", "没人做的")).unwrap();
        // Not the router, not another session
        assert!(done_in(&store, Some(&r1), Some(1), "").unwrap_err().contains("做它的会话"));
        assert!(done_in(&store, Some(&w2), Some(1), "").is_err());
        // The session doing it, naming no number: the one it has
        let t = done_in(&store, Some(&w1), None, " 测试集群上响了 ").unwrap().0;
        assert_eq!((t.n, t.state(), t.result.as_str()), (1, TaskState::Done, "测试集群上响了"));
        assert!(done_in(&store, Some(&w1), None, "").unwrap_err().contains("没有任务"));
        assert!(done_in(&store, Some(&w1), Some(1), "").unwrap_err().contains("已经完成"));
        // You, any of them (a number is needed); one nobody has can be closed by you too
        assert!(done_in(&store, None, None, "").is_err());
        assert_eq!(done_in(&store, None, Some(2), "").unwrap().0.state(), TaskState::Done);
        assert_eq!(done_in(&store, None, Some(3), "不做了").unwrap().0.state(), TaskState::Done);
        // Two at once: it has to say which
        give_in(&store, None, 1, "w1", "").unwrap();
        give_in(&store, None, 2, "w1", "").unwrap();
        assert!(done_in(&store, Some(&w1), None, "").unwrap_err().contains("2 张"));
    }

    #[test]
    fn word_is_passed_when_a_ticket_changes_hands_is_made_with_nobody_on_it_or_is_done() {
        let (store, get) = fixture();
        let (r1, w1, o1) = (get("r1"), get("w1"), get("o1"));
        // Made by you with nobody on it: the group's router hears of it. Made by the router itself: nobody is told
        let outs = add_in(&store, None, New { criteria: "告警响".into(), note: "先看日志".into(), ..new("work", "修告警") }).unwrap().1;
        assert_eq!(words(&outs), [("r1", "#1 新任务:修告警\n完成标准:告警响\n先看日志\n还没有会话在做。", false)]);
        assert_eq!(events(&outs), [("task.added", "", "")]);
        let outs = add_in(&store, Some(&r1), new("work", "加面板")).unwrap().1;
        assert!(words(&outs).is_empty());
        assert_eq!(events(&outs), [("task.added", "", "r1")]);
        // Handed over: the session gets the ticket, what was said, and how to say it's done — words that must get through
        let outs = give_in(&store, Some(&r1), 1, "w1", "你昨天改过这块").unwrap().1;
        let want = "任务 #1:修告警\n完成标准:告警响\n你昨天改过这块\n做完后运行:aca task done 1 \"一句话说结果\"";
        assert_eq!(words(&outs), [("w1", want, true)]);
        assert_eq!(events(&outs), [("task.given", "w1", "r1")]);
        // To another session: the one that had it is told to stop, the new one gets the ticket
        let outs = give_in(&store, None, 1, "w2", "").unwrap().1;
        let w = words(&outs);
        assert_eq!(w[0], ("w1", "任务 #1 已交给「会话w2」,不用再做。", false));
        assert!(w[1].0 == "w2" && w[1].1.starts_with("任务 #1:修告警\n完成标准:告警响\n做完后运行") && w[1].2);
        // Taken back: told too, and the event names nobody
        let outs = give_in(&store, Some(&r1), 1, "-", "").unwrap().1;
        assert_eq!(words(&outs), [("w2", "任务 #1 已取回,不用再做。", false)]);
        assert_eq!(events(&outs), [("task.given", "", "r1")]);
        assert_eq!(store.task(1).unwrap().state(), TaskState::Todo);
        // Done by the session doing it: the group's router hears
        give_in(&store, Some(&r1), 1, "w1", "").unwrap();
        let outs = done_in(&store, Some(&w1), None, "响了").unwrap().1;
        assert_eq!(words(&outs), [("r1", "#1 完成:修告警\n响了", false)]);
        assert_eq!(events(&outs), [("task.done", "w1", "w1")]);
        // Opened again by handing it over: the one that had finished it isn't told to stop
        let outs = give_in(&store, Some(&r1), 1, "w2", "").unwrap().1;
        assert_eq!(words(&outs).len(), 1);
        // A router handing a ticket to itself, and finishing it: nobody to tell
        assert!(words(&give_in(&store, Some(&r1), 2, "r1", "").unwrap().1).is_empty());
        assert!(words(&done_in(&store, Some(&r1), Some(2), "").unwrap().1).is_empty());
        // Made with a session on it from the start: both events, the ticket for that session
        let outs = add_in(&store, Some(&r1), New { to: "w1".into(), say: "先做这个".into(), ..new("work", "写文档") }).unwrap().1;
        assert_eq!(events(&outs), [("task.added", "w1", "r1"), ("task.given", "w1", "r1")]);
        assert!(words(&outs)[0].1.contains("先做这个") && words(&outs)[0].2);
        // A group with no router or waiting ticket: completion needs no message to the dispatcher
        assert!(words(&add_in(&store, Some(&r1), new("other", "备份")).unwrap().1).is_empty());
        give_in(&store, Some(&r1), 4, "o1", "").unwrap();
        assert!(words(&done_in(&store, Some(&o1), Some(4), "").unwrap().1).is_empty());
        // Handed over by you there: nobody to tell
        let n = add_in(&store, None, New { to: "o1".into(), ..new("other", "清理") }).unwrap().0.n;
        assert!(words(&done_in(&store, Some(&o1), Some(n), "").unwrap().1).is_empty());
    }

    #[test]
    fn watched_completion_reaches_the_dispatcher_once_with_or_without_a_backlog() {
        let (store, get) = fixture();
        let dispatcher = get("r1");
        for (group, id) in [("work", "w1"), ("other", "o1")] {
            for backlog in [false, true] {
                if backlog {
                    add_in(&store, None, new(group, "later")).unwrap();
                }
                let t = add_in(&store, Some(&dispatcher), New { to: id.into(), watch: true, ..new(group, "fix") }).unwrap().0;
                let (_, outs) = done_in(&store, Some(&get(id)), Some(t.n), "ready").unwrap();
                let got = words(&outs);
                assert_eq!(got.len(), 1);
                assert_eq!(got[0].0, dispatcher.id);
                assert!(got[0].1.contains(&format!("#{} 完成", t.n)) && got[0].1.ends_with("ready"));
                assert!(done_in(&store, Some(&get(id)), Some(t.n), "again").is_err());
            }
        }
    }

    #[test]
    fn a_number_is_read_with_or_without_its_sign_and_json_says_the_state() {
        assert_eq!((number("9").unwrap(), number("#12").unwrap()), (9, 12));
        assert!(number("x").is_err() && number("0").is_err() && number("#").is_err() && number("-3").is_err());
        let t = Task { n: 4, group: "work".into(), title: "修告警".into(), session: "w1".into(), ..Task::default() };
        let v = json(&t);
        assert_eq!((v["n"].as_i64(), v["state"].as_str(), v["session"].as_str()), (Some(4), Some("doing"), Some("w1")));
        assert_eq!(json(&Task { done: 5, ..t.clone() })["state"], "done");
        assert_eq!(json(&Task { session: String::new(), ..t.clone() })["state"], "todo");
        // What a session handed a ticket is told
        let t = Task { criteria: "告警响".into(), say: "照这个做".into(), ..t };
        assert_eq!(brief(&t), "任务 #4:修告警\n完成标准:告警响\n照这个做\n做完后运行:aca task done 4 \"一句话说结果\"");
    }
}
