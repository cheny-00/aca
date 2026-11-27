//! The inbox: passing messages between sessions. Each session has an `inbox/<id>.jsonl`, one message per line, append
//! only; which ones were delivered is recorded next to it in `<id>.state`.
//!
//! Who puts things in: `aca msg` (people, agents, scripts), and a worker's own hooks — when it finishes a turn, errors,
//! exits or waits for a confirmation, it leaves a notice for the session that delegated it (`parent`) ([`notify`]).
//! Notices only go up: delegation forms a tree, so two agents can't keep waking each other up forever.
//!
//! How it's delivered depends on the receiving tool ([`Delivery`]):
//! - **Doorbell** (Claude): when Claude is idle, its input box is empty and you aren't typing, paste your messages and
//!   router messages quoting you into the input box. Other messages in that batch wait for the end of the turn.
//!   Otherwise touch `inbox/<id>.bell`. When Claude starts, `aca inbox watch` has it watch this file; on a
//!   change it runs `aca inbox take` in the background, which takes the undelivered messages, writes them to stderr and
//!   exits 2, and Claude hands that to the model as a system message (idle: starts a turn; running: added to this turn;
//!   needs you: waits until you've answered). If nobody takes it 10 seconds after ringing, this one didn't connect, so
//!   fall back to typing
//! - **Queue** (Codex): `codex queue --thread <conversation> --message <text>`; the Codex window checks its queue every
//!   10 seconds and starts a turn when idle. While it's running, waiting for your confirmation, or just interrupted with
//!   Esc, don't queue yet: wait until its own turn finishes (Stop hook) and queue then
//! - **Typing**: when the first two can't be used (a Claude with no doorbell connected, a Codex that hasn't been spoken
//!   to yet, other agents). Only paste and press Enter when the other side is idle, you aren't typing in that pane, and
//!   the input box has no unsent text of yours; otherwise look again after 20 seconds, and if it still can't, wait for
//!   its next hook
//!
//! There's no resident process: delivery is tried once at the moment of sending ([`deliver`]); what can't be delivered
//! waits for the other side's own hook to run next (a turn finishes, it restarts). If the screen looks like it's running
//! but no hook said a turn is running (just started and loading), look again after 20 seconds by itself; after
//! `aca start` it also tries once a few seconds later (Codex's SessionStart is only reported after the first prompt).
//! Every message put in and every one delivered is a line in the event stream (`message.sent` / `message.delivered`).
//!
//! Nothing is added to an agent unless someone speaks: when nobody sends anything, the receiving side's context holds not
//! a word from aca; whether a worker wakes the session that delegated it when it finishes is said at delegation time
//! (`aca new --notify`), and without it, it doesn't.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{self, Agent, Delivery};
use crate::config::Config;
use crate::i18n::Tr;
use crate::paths::{self, Res};
use crate::runtime::{self, Run, UserWords, clip};
use crate::schedule::Due;
use crate::status::{self, State};
use crate::store::{Session, Store};
use crate::{event, t, tmux, tr};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Msg {
    pub id: String,
    pub ts: u64,
    /// Who sent it: a session id; `you` when not sent from inside a session
    pub from: String,
    /// How the sender is shown to people: `ab12「修登录」claude:sub`
    pub who: String,
    pub to: String,
    /// msg = something someone said; notice = a notice sent automatically by a worker's hook
    pub kind: String,
    /// What the notice is about: turn.done / turn.failed / agent.exited / agent.waiting
    pub what: String,
    /// Whether to wake the other side for it; if not, it's carried along next time something else is delivered
    pub wake: bool,
    pub text: String,
    /// Scheduled (`aca msg --in`): delivered only at this moment; 0 = right away
    pub at: u64,
    /// How it came: empty = through aca; `claude` = Claude passed it straight from another Claude session (SendMessage), taken
    /// in from the receiver's conversation record afterwards ([`native`]); any other name = a channel outside aca your words
    /// came in through, as whatever relays them calls it (`aca msg --via`)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub via: String,
    /// The router prefixed this message with the user's words for this recipient.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quoted: bool,
}

impl Msg {
    fn carries_user_words(&self) -> bool {
        (self.from == "you" || self.quoted) && self.kind == "msg" && self.at == 0 && self.what.is_empty() && self.via != NATIVE
    }

    fn prepend_quote(&mut self, prefix: &str) {
        self.quoted = !prefix.is_empty();
        self.text.insert_str(0, prefix);
    }
}

/// What `via` says of a message Claude passed along by itself: not a name a channel can take
pub const NATIVE: &str = crate::plugin::Channel::NATIVE;

/// Which ones were delivered: `got` reached the other side; `queued` went into Codex's queue, but it hasn't been seen
/// starting a turn for them yet
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct Ledger {
    got: Vec<String>,
    queued: Vec<String>,
}

/// How far a message has got
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Scheduled, not due yet
    Scheduled,
    Pending,
    Queued,
    Got,
}

impl Stage {
    pub fn said(self) -> &'static str {
        match self {
            Stage::Scheduled => t!("定时", "scheduled"),
            Stage::Pending => t!("未送达", "not delivered"),
            Stage::Queued => t!("排队中", "queued"),
            Stage::Got => t!("已送达", "delivered"),
        }
    }
}

/// The result of one delivery attempt
#[derive(Clone, Debug, PartialEq)]
pub enum Sent {
    /// Nothing to wake it for
    Nothing,
    Starting,
    Bell,
    Queued,
    Typed,
    /// Scheduled: delivered at this moment
    Scheduled(u64),
    /// Held for now, and why
    Held(String),
}

impl Sent {
    pub fn said(&self) -> String {
        match self {
            Sent::Nothing => t!("无需唤醒", "no need to wake it").into(),
            Sent::Starting => t!("已启动新一班", "started a new shift").into(),
            Sent::Bell => t!("已按门铃", "rang the doorbell").into(),
            Sent::Queued => t!("已排入它的队列,空闲后会自动开始一轮", "queued; it starts a turn on its own once idle").into(),
            Sent::Typed => t!("已输入并发送", "typed in and sent").into(),
            Sent::Scheduled(at) => tr!("{} 送达", "delivered at {}", crate::schedule::clock(*at, paths::now())),
            Sent::Held(why) => tr!("暂存:{why}", "kept: {why}"),
        }
    }
}

/// Once this many arrive between the same pair of sessions within 10 minutes, the rest are stored without waking
const BRAKE: usize = 20;
const BRAKE_SECS: u64 = 600;
/// At this many messages the file is tidied: undelivered ones are all kept, delivered ones only the most recent this many
const TIDY_AT: usize = 500;
const KEEP_DONE: usize = 100;
/// If your last key press in that pane was within this many seconds, you count as typing
const TYPING_SECS: u64 = 15;
/// Rang the doorbell this long ago and nobody took it: this run of Claude didn't connect the doorbell
const BELL_CHECK_SECS: u64 = 10;
/// When typing can't deliver (you're typing, there's a draft), look again after this long
const RETRY_SECS: u64 = 20;

fn file(id: &str) -> PathBuf {
    paths::inbox_dir().join(format!("{id}.jsonl"))
}

fn ledger_file(id: &str) -> PathBuf {
    paths::inbox_dir().join(format!("{id}.state"))
}

pub fn bell(id: &str) -> PathBuf {
    paths::inbox_dir().join(format!("{id}.bell"))
}

/// This inbox's lock: putting, taking and marking delivered all happen under it, so of two processes taking at once only
/// one gets the messages
fn lock(id: &str) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(paths::inbox_dir());
    let f = std::fs::OpenOptions::new().create(true).append(true).open(paths::inbox_dir().join(format!("{id}.lock"))).ok()?;
    let _ = f.lock();
    Some(f)
}

pub fn all(id: &str) -> Vec<Msg> {
    std::fs::read_to_string(file(id)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn ledger(id: &str) -> Ledger {
    std::fs::read_to_string(ledger_file(id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(id: &str, l: &Ledger) {
    let _ = paths::write_atomic(&ledger_file(id), &serde_json::to_string(l).unwrap_or_default());
}

fn stage(l: &Ledger, m: &Msg) -> Stage {
    if l.got.contains(&m.id) {
        Stage::Got
    } else if l.queued.contains(&m.id) {
        Stage::Queued
    } else {
        Stage::Pending
    }
}

/// All messages, with how far each has got (`aca inbox`). Not delivered and not due yet = scheduled
pub fn list(id: &str) -> Vec<(Msg, Stage)> {
    let (l, now) = (ledger(id), paths::now());
    all(id)
        .into_iter()
        .map(|m| {
            let st = match stage(&l, &m) {
                Stage::Pending if m.at > now => Stage::Scheduled,
                st => st,
            };
            (m, st)
        })
        .collect()
}

/// Not delivered yet: hasn't reached the other side and isn't queued; a scheduled one counts only once it's due
pub fn pending(id: &str) -> Vec<Msg> {
    list(id).into_iter().filter(|(_, s)| *s == Stage::Pending).map(|(m, _)| m).collect()
}

/// Scheduled and not due yet, the earliest first (marked ◷ in the list; a session that has any isn't stopped while idle)
pub fn scheduled(id: &str) -> Vec<Msg> {
    let mut v: Vec<Msg> = list(id).into_iter().filter(|(_, s)| *s == Stage::Scheduled).map(|(m, _)| m).collect();
    v.sort_by_key(|m| m.at);
    v
}

/// Scheduled messages in the inbox that aren't due yet (see [`scheduled`]), listed like the ones it scheduled itself
pub fn scheduled_as_due(id: &str) -> Vec<Due> {
    scheduled(id)
        .into_iter()
        .map(|m| Due { cron: String::new(), recurring: false, prompt: m.text, next: Some(m.at), inbox: true })
        .collect()
}

/// Whether there's anything to wake it for
pub fn wants_wake(id: &str) -> bool {
    pending(id).iter().any(|m| m.wake)
}

/// Puts a message into `m.to`'s inbox, filling in the id and time; if the same sender said too much within 10 minutes,
/// it's stored without waking. Returns the message put in
pub fn post(mut m: Msg) -> Msg {
    m.id = format!("m{}", paths::short_id());
    m.ts = paths::now();
    let braked = {
        let _l = lock(&m.to);
        let msgs = all(&m.to);
        let recent = msgs.iter().filter(|x| x.from == m.from && m.ts.saturating_sub(x.ts) < BRAKE_SECS).count();
        let braked = m.wake && recent >= BRAKE;
        m.wake &= !braked;
        append_locked(&m.to, msgs, std::slice::from_ref(&m));
        braked && recent == BRAKE
    };
    sent(&m);
    // The message that just hit the brake: tell you once (not again for the ones after)
    if braked {
        event::emit("message.throttled", &m.to, json!({ "from": m.from, "count": BRAKE, "secs": BRAKE_SECS }));
    }
    m
}

/// Adds `new` to the end of `to`'s inbox, tidying it once it's grown big (call under the lock; `msgs` is what's in it now)
fn append_locked(to: &str, mut msgs: Vec<Msg>, new: &[Msg]) {
    msgs.extend_from_slice(new);
    if msgs.len() >= TIDY_AT {
        return tidy(to, msgs);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file(to)) {
        let text: String = new.iter().map(|m| format!("{}\n", serde_json::to_string(m).unwrap_or_default())).collect();
        let _ = f.write_all(text.as_bytes());
    }
}

/// The `message.sent` line in the event stream
fn sent(m: &Msg) {
    let or_null = |s: &str| if s.is_empty() { Value::Null } else { json!(s) };
    let at = if m.at == 0 { Value::Null } else { json!(m.at) };
    let data = json!({
        "id": m.id, "from": m.from, "kind": m.kind, "what": or_null(&m.what), "wake": m.wake, "text": clip(&m.text, 200), "at": at,
        "via": or_null(&m.via),
    });
    event::emit("message.sent", &m.to, data);
}

/// What Claude passed `to` straight from other Claude sessions (SendMessage), read from its record when a turn ends
/// ([`Agent::turn_record`]): put in its inbox as already delivered, so `aca inbox` and plugins see them alongside aca's own
/// (`message.sent` with `via`, then `message.delivered` with `how` = native). Taken once each, by Claude's own id for it.
/// The sender is the session whose Claude inbox socket ([`Run::peer`]) belongs to the process Claude checked; when aca
/// doesn't know it (started outside aca, or before aca recorded sockets), `who` keeps the name Claude has for it
pub fn native(to: &str, got: Vec<agent::Native>) {
    if got.is_empty() {
        return;
    }
    let senders = with_socket();
    let _l = lock(to);
    let msgs = all(to);
    let mut new: Vec<Msg> = vec![];
    for n in got {
        let id = format!("c{}", n.id.chars().filter(char::is_ascii_hexdigit).take(8).collect::<String>());
        if msgs.iter().chain(&new).any(|m| m.id == id) {
            continue;
        }
        let from = senders.iter().find(|(_, r)| sent_by(r, &n));
        new.push(Msg {
            id,
            ts: n.ts,
            from: from.map(|(s, _)| s.id.clone()).unwrap_or_default(),
            who: from.map_or_else(|| format!("「{}」claude", n.name), |(s, r)| who(s, r)),
            to: to.to_string(),
            kind: "msg".into(),
            via: NATIVE.into(),
            text: n.text,
            ..Default::default()
        });
    }
    if new.is_empty() {
        return;
    }
    append_locked(to, msgs, &new);
    new.iter().for_each(sent);
    mark_locked(to, &new.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), "native");
}

/// The sessions whose Claude has told aca its inbox socket, the most recently active first (a socket's name is a process id,
/// and one can be reused by a later process)
fn with_socket() -> Vec<(Session, Run)> {
    let Ok(store) = Store::open() else { return vec![] };
    let mut v: Vec<(Session, Run)> = store
        .sessions(false)
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let r = runtime::load(&s.id);
            (s, r)
        })
        .filter(|(_, r)| !r.peer.is_empty())
        .collect();
    v.sort_by_key(|(_, r)| std::cmp::Reverse(r.ts));
    v
}

/// Whether `n` came from the Claude whose socket this is: by the process Claude checked, else by the address it gives
fn sent_by(r: &Run, n: &agent::Native) -> bool {
    match n.pid {
        0 => !n.addr.is_empty() && r.peer == n.addr,
        pid => std::path::Path::new(&r.peer).file_stem().is_some_and(|f| *f == *pid.to_string()),
    }
}

/// Tidy: keep every undelivered one and only the latest few delivered; the delivered ids on record are trimmed to the
/// ones still there too (call under the lock)
fn tidy(id: &str, msgs: Vec<Msg>) {
    let mut l = ledger(id);
    let done: Vec<usize> = msgs.iter().enumerate().filter(|(_, m)| stage(&l, m) != Stage::Pending).map(|(i, _)| i).collect();
    let drop: std::collections::HashSet<usize> = done[..done.len().saturating_sub(KEEP_DONE)].iter().copied().collect();
    let kept: Vec<Msg> = msgs.into_iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, m)| m).collect();
    let text: String = kept.iter().map(|m| format!("{}\n", serde_json::to_string(m).unwrap_or_default())).collect();
    let _ = paths::write_atomic(&file(id), &text);
    l.got.retain(|x| kept.iter().any(|m| &m.id == x));
    l.queued.retain(|x| kept.iter().any(|m| &m.id == x));
    save(id, &l);
}

/// Marks as delivered (call under the lock), emitting a `message.delivered` for each; already marked ones aren't emitted
/// again
fn mark_locked(id: &str, ids: &[String], how: &str) {
    let mut l = ledger(id);
    let fresh: Vec<&String> = ids.iter().filter(|x| !l.got.contains(x)).collect();
    if fresh.is_empty() {
        return;
    }
    l.got.extend(fresh.iter().map(|x| x.to_string()));
    l.queued.retain(|x| !ids.contains(x));
    save(id, &l);
    for x in fresh {
        event::emit("message.delivered", id, json!({ "id": x, "how": how }));
    }
}

pub fn mark(id: &str, ids: &[String], how: &str) {
    let _l = lock(id);
    mark_locked(id, ids, how);
}

/// The doorbell rang: take every undelivered one out and mark them delivered. Only taken if there's one to wake for; if
/// there are only non-waking ones, leave them to be carried along next time
pub fn take(id: &str) -> Vec<Msg> {
    let _l = lock(id);
    if runtime::load(id).opening_mail.is_some() {
        return vec![];
    }
    let p = pending(id);
    if !p.iter().any(|m| m.wake) {
        return vec![];
    }
    mark_locked(id, &p.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), "bell");
    p
}

/// Queued in Codex's queue: doesn't count as delivered yet, only once it starts a turn for them (the original text shows up
/// in UserPromptSubmit)
fn mark_queued(id: &str, ids: &[String]) {
    let _l = lock(id);
    let mut l = ledger(id);
    l.queued.extend(ids.iter().filter(|x| !l.queued.contains(x) && !l.got.contains(x)).cloned().collect::<Vec<_>>());
    save(id, &l);
}

pub fn remove(id: &str) {
    for ext in ["jsonl", "state", "bell", "lock", "shift"] {
        let _ = std::fs::remove_file(paths::inbox_dir().join(format!("{id}.{ext}")));
    }
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "刚刚".into(),
        60..3600 => format!("{} 分钟前", secs / 60),
        3600..86400 => format!("{} 小时前", secs / 3600),
        _ => format!("{} 天前", secs / 86400),
    }
}

fn what_said(what: &str) -> &'static str {
    match what.strip_prefix("task.").unwrap_or(what) {
        "turn.done" => "跑完一轮",
        "turn.failed" => "出错了",
        "agent.exited" => "退出了",
        "agent.waiting" => "在等确认",
        _ => "有动静",
    }
}

/// The text the doorbell (Claude) takes, which it sees as a system message: lists the messages together, with who sent
/// each, how long ago, and how to reply. Notices only say what happened and where to look, never "go do X": phrased as a
/// command, the model may take it as a suspicious instruction. Messages from the session that delegated it (`parent`)
/// are marked as assigned work: unmarked, Claude takes a request to run a command as an outsider's instruction and won't
/// (tested on 2.1.295). `hears_done` = the parent gets a notice when this turn finishes: answering in place sends it
/// back, no need for another `aca msg`
pub fn render(msgs: &[Msg], now: u64, parent: &str, hears_done: bool) -> String {
    render_bell(msgs, now, parent, hears_done, false)
}

fn render_bell(msgs: &[Msg], now: u64, parent: &str, hears_done: bool, router: bool) -> String {
    let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    let mut out = format!("{} 条新消息({})", msgs.len(), ids.join(" "));
    if !router {
        out.push_str("· 全部:aca inbox");
    }
    for (i, m) in msgs.iter().enumerate() {
        let when = ago(now.saturating_sub(m.ts));
        let who = if m.from == "you" { "用户(在会话外面)".to_string() } else { m.who.clone() };
        if m.kind == "notice" {
            out += &format!("\n{}. {who} · {} · {when}\n   {}", i + 1, what_said(&m.what), m.text.replace('\n', "\n   "));
        } else if m.from == m.to {
            out += &format!("\n{}. 你自己 · 定时到点了 · {when}定的\n   「{}」", i + 1, m.text.replace('\n', "\n   "));
        } else {
            let back = match m.from.as_str() {
                "you" => String::new(),
                f if f == parent && hears_done => "  派你干活的会话交代的:直接在这里做、在这里回答,这一轮跑完它会收到你最后一句".into(),
                f if f == parent && router => "  派你干活的会话交代的:直接在这里做".into(),
                _ if router => String::new(),
                f if f == parent => format!("  派你干活的会话交代的:直接在这里做;要回它用 aca msg {f} \"…\""),
                f => format!("  回话:aca msg {f} \"…\""),
            };
            out += &format!("\n{}. {who} · 发来一句话 · {when}\n   「{}」{back}", i + 1, m.text.replace('\n', "\n   "));
        }
    }
    out
}

/// The text delivered by queueing / typing: to the agent it's just a user message, so only the message itself goes in.
/// What you send goes as is; from a session, a line in front says who; a notice says what happened. It doesn't say "this
/// was forwarded from another session, not typed by the user", and doesn't teach it how to reply: Codex takes those as
/// injection and won't comply; with just one line saying which session it's from, it complies (tested on 0.162). `parent`
/// = the session that delegated the receiving session
fn render_as_input(msgs: &[Msg], now: u64, parent: &str) -> String {
    let one = |m: &Msg| match (m.kind.as_str(), m.from.as_str()) {
        ("notice", _) => format!("[aca] {} {} · {}\n{}", m.who, what_said(&m.what), ago(now.saturating_sub(m.ts)), m.text),
        (_, "you") => m.text.clone(),
        (_, f) if f == m.to => format!("[aca] 定时到点了(你 {}定的):\n{}", ago(now.saturating_sub(m.ts)), m.text),
        (_, f) if f == parent => format!("[aca] 派你干活的会话 {} 说:\n{}", m.who, m.text),
        _ => format!("[aca] 来自会话 {}:\n{}", m.who, m.text),
    };
    msgs.iter().map(one).collect::<Vec<_>>().join("\n\n")
}

/// Whether the session that delegated it gets a notice when this turn finishes (see [`render`])
fn hears_done(cfg: &Config, s: &Session) -> bool {
    !s.parent.is_empty() && wanted(&runtime::load(&s.id).notify, &cfg.inbox.notify, "done")
}

/// The queued messages whose original text is in this user message: Codex started a turn for them
fn queued_in(id: &str, prompt: &str) -> Vec<String> {
    let queued = ledger(id).queued;
    if queued.is_empty() {
        return vec![];
    }
    all(id)
        .into_iter()
        .filter(|m| queued.contains(&m.id) && !m.text.trim().is_empty() && prompt.contains(m.text.trim()))
        .map(|m| m.id)
        .collect()
}

/// Whether the input box has unsent text of yours: find the last line starting with `mark` (that's the input box) and look
/// for non-gray (dim) text after the mark. Both tools' hints (`Try "…"`, `Ask Codex to do anything`) are gray (tested
/// 2026-10-08). None = no input box found on the screen
pub fn draft(mark: char, lines: &[String]) -> Option<bool> {
    let line = lines.iter().rev().find(|l| plain(l).trim_start().starts_with(mark))?;
    let mut dim = false;
    let mut seen_mark = false;
    let mut cs = line.chars().peekable();
    while let Some(c) = cs.next() {
        if c == '\x1b' {
            if cs.peek() == Some(&'[') {
                cs.next();
                let mut seq = String::new();
                for d in cs.by_ref() {
                    if d.is_ascii_alphabetic() {
                        if d == 'm' {
                            dim = sgr_dim(&seq, dim);
                        }
                        break;
                    }
                    seq.push(d);
                }
            }
            continue;
        }
        if !seen_mark {
            seen_mark = c == mark;
            continue;
        }
        if !c.is_whitespace() && !dim {
            return Some(true);
        }
    }
    Some(false)
}

/// Strips colors
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut cs = line.chars().peekable();
    while let Some(c) = cs.next() {
        if c == '\x1b' && cs.peek() == Some(&'[') {
            cs.next();
            for d in cs.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Whether it's still gray after one SGR (the part inside `ESC[…m`): 2 is gray, 0 / 22 / empty cancel it; what follows 38
/// or 48 is a color, skipped
fn sgr_dim(seq: &str, mut dim: bool) -> bool {
    let ps: Vec<&str> = seq.split(';').collect();
    let mut i = 0;
    while i < ps.len() {
        match ps[i] {
            "" | "0" | "22" => dim = false,
            "2" => dim = true,
            "38" | "48" | "58" => i += if ps.get(i + 1) == Some(&"5") { 2 } else { 4 },
            _ => {}
        }
        i += 1;
    }
    dim
}

/// How the sender is shown to people: `ab12「修登录」claude:sub`
pub fn who(s: &Session, r: &Run) -> String {
    let acct = if s.account.is_empty() { s.tool.clone() } else { format!("{}:{}", s.tool, agent::account_label(&s.account)) };
    format!("{}「{}」{acct}", s.id, status::title(s, r))
}

/// Whether to notify the parent of this: the session's own choice (`aca new --notify`) wins, otherwise the config's, and
/// if the config says nothing, nothing is notified (nobody is woken unless asked). `all` = all four
fn wanted(own: &str, default: &[String], short: &str) -> bool {
    let own: Vec<&str> = own.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
    let list: Vec<&str> = if own.is_empty() { default.iter().map(String::as_str).collect() } else { own };
    list.iter().any(|x| *x == short || *x == "all")
}

/// The text of a notice: only what happened and where to look
fn notice_text(id: &str, ev: &str, r: &Run) -> String {
    let one_line = |s: &str, n: usize| clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), n);
    match ev {
        "turn.done" if r.last.trim().is_empty() => format!("全文:aca output {id}"),
        "turn.done" => format!("最后一句:「{}」  全文:aca output {id}", one_line(&r.last, 120)),
        "turn.failed" if r.error_kind.is_empty() => format!("原因:{}", one_line(&r.error, 200)),
        "turn.failed" => format!("原因:{}({})", one_line(&r.error, 200), r.error_kind_said()),
        "agent.exited" => "它的进程退出了".into(),
        _ => match r.ask_kind.as_str() {
            "choice" if r.ask_multi => format!("在问:「{}」(一次问了好几题,得在它的界面里答)", r.ask),
            "choice" => {
                let opts: Vec<String> = r.ask_opts.iter().enumerate().map(|(i, o)| format!("{} {o}", i + 1)).collect();
                format!("在问:「{}」\n选项:{}  回答:aca answer {id} <选项号>", r.ask, opts.join("  "))
            }
            "permission" => format!("要你同意:{}\n回答:aca answer {id} yes|no", r.ask),
            _ => format!("在等你:{}", r.ask),
        },
    }
}

/// A hook's notices, one per watched ticket and at most one ordinary parent notice.
fn notices(store: &Store, s: &Session, r: &Run, ev: &str, short: &str, cfg: &Config) -> Vec<Msg> {
    let notice = |to: String, what: &str, text| Msg {
        from: s.id.clone(),
        who: who(s, r),
        to,
        kind: "notice".into(),
        what: what.into(),
        wake: true,
        text,
        ..Default::default()
    };
    let mut out = vec![];
    for t in store.watched_by_worker(&s.id).unwrap_or_default() {
        let Some(to) = crate::task::recipient(store, &t) else { continue };
        if to == s.id || store.get(&to).is_ok_and(|p| crate::sandbox::cage_of_session(cfg, &p).is_some()) {
            continue;
        }
        let state = match ev {
            "turn.done" => t!("跑完一轮", "finished a turn").into(),
            "turn.failed" => tr!("出错:{}", "failed: {}", r.error),
            "agent.waiting" => tr!("在问:{}", "asks: {}", r.ask),
            _ => t!("进程退出", "process exited").into(),
        };
        let mut text = format!("#{} · {state}", t.n);
        if !r.last.trim().is_empty() {
            text.push_str(&tr!("\n最后一句:{}", "\nLast reply: {}", clip(&r.last, 300)));
        }
        text.push_str(&tr!("\n全文:look {}", "\nFull reply: look {}", s.id));
        out.push(notice(to, &format!("task.{ev}"), text));
    }
    if wanted(&r.notify, &cfg.inbox.notify, short)
        && let Ok(parent) = store.get(&s.parent)
        && parent.id != s.id
        && !out.iter().any(|m| m.to == parent.id)
        && (ev == "agent.exited" || !store.task_routers(&s.id, None).is_ok_and(|routers| routers.contains(&parent.id)))
        && crate::sandbox::cage_of_session(cfg, &parent).is_none()
    {
        out.push(notice(parent.id, ev, notice_text(&s.id, ev, r)));
    }
    out
}

pub fn notify(id: &str, ev: &str, cfg: &Config) {
    let short = match ev {
        "turn.done" => "done",
        "turn.failed" => "failed",
        "agent.exited" => "exited",
        "agent.waiting" => "waiting",
        _ => return,
    };
    let Ok(store) = Store::open() else { return };
    let Ok(s) = store.get(id) else { return };
    for msg in notices(&store, &s, &runtime::load(id), ev, short, cfg) {
        let m = post(msg);
        if m.wake {
            later(&m.to, 0, &[]);
        }
    }
}

fn user_words(text: &str, via: &str, ts: u64) -> UserWords {
    UserWords { text: text.chars().take(4000).collect(), chars: text.chars().count(), via: via.into(), ts, sent_to: vec![] }
}

pub(crate) fn opening_words(msgs: &[Msg]) -> Option<UserWords> {
    let first = msgs.iter().find(|m| m.wake)?;
    (first.from == "you" && first.carries_user_words()).then(|| user_words(&first.text, &first.via, first.ts))
}

/// Claude can escape '<' inside a paste and use its final newline as the wrapper's separator.
pub(crate) fn input_hash(text: &str) -> u64 {
    exact_input_hash(text.replace(r"<\", "<").trim_end())
}

fn exact_input_hash(text: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn pasted_input_hash(r: &Run, text: &str) -> u64 {
    if r.shift.is_empty() { exact_input_hash(text) } else { input_hash(text) }
}

fn typed_words(r: &mut Run, prompt: &str, now: u64) -> Option<UserWords> {
    let (submitted, words) = if r.shift.is_empty() { (prompt, prompt.into()) } else { agent::paste_text(prompt, false) };
    if let Some((hash, origin)) = r.router_input.take()
        && hash == pasted_input_hash(r, submitted)
    {
        return origin;
    }
    // A different submit leaves the opening batch for the doorbell.
    r.opening_mail = None;
    (!words.trim().is_empty() && !crate::thread::not_typed(&words)).then(|| user_words(&words, "", now))
}

/// A recipient gets the original once in this turn, even if two tool calls run concurrently.
pub(crate) fn forward(store: &Store, me: Option<&Session>, to: &str, text: &str) -> String {
    router_quote(store, me, to) + text
}

fn router_quote(store: &Store, me: Option<&Session>, to: &str) -> String {
    let Some(router) = me.filter(|s| s.id != to && crate::task::is_router(store, s)) else { return String::new() };
    let mut prefix = String::new();
    runtime::update(&router.id, |r| {
        if let Some(words) = &mut r.user_words {
            prefix = quote(words, &router.title, to);
        }
    });
    prefix
}

fn quote(words: &mut UserWords, title: &str, to: &str) -> String {
    if words.sent_to.iter().any(|id| id == to) {
        return String::new();
    }
    words.sent_to.push(to.into());
    let via = if words.via.is_empty() { t!("界面", "interface") } else { &words.via };
    let clock = crate::schedule::clock(words.ts, words.ts);
    let text: String = words.text.chars().take(1500).collect();
    let n = words.chars;
    let cut = if n > 1500 { tr!("(共 {n} 字,已截)", " ({n} characters total, truncated)") } else { String::new() };
    tr!(
        "用户对「{title}」说({via},{clock}):\n「{text}」{cut}\n{title}:",
        "The user said to “{title}” ({via}, {clock}):\n“{text}”{cut}\n{title}: "
    )
}

/// The receiving side, in its own hook (waits for nothing):
/// - The prompt of this turn contains the original text of queued messages: mark them delivered
/// - The turn finished, or it just started: if anything is undelivered, start a detached delivery process (for doorbell
///   tools just starting, `aca inbox watch` handles it)
pub fn on_hook(id: &str, ev: &str, prompt: &str, bell_agent: bool) {
    if ev == "UserPromptSubmit" {
        let ids = queued_in(id, prompt);
        let router = Store::open().is_ok_and(|st| st.router_role(id).ok().flatten().is_some());
        if router || !ids.is_empty() || runtime::load(id).router_input.is_some() {
            let _mail = lock(id);
            let queued: Vec<_> = all(id).into_iter().filter(|m| ids.contains(&m.id)).collect();
            let mut confirmed = vec![];
            runtime::update(id, |r| {
                if !r.current_shift() {
                    return;
                }
                if prompt.starts_with("<task-notification>")
                    && prompt.contains(&format!("<summary>{}</summary>", agent::BELL_SUMMARY))
                    && let Some(via) = r.bell_via.take()
                {
                    r.turn_via = via;
                    return;
                }
                let words = if queued.is_empty() {
                    let words = typed_words(r, prompt, paths::now());
                    confirmed = r.opening_mail.take().unwrap_or_default();
                    words
                } else {
                    r.opening_mail = None;
                    opening_words(&queued)
                };
                r.turn_via = words.as_ref().map(|w| w.via.clone()).unwrap_or_default();
                if router {
                    r.user_words = words;
                }
            });
            mark_locked(id, &confirmed, "handoff");
        }
        if !ids.is_empty() {
            mark(id, &ids, "queue");
        }
    }
    if matches!(ev, "Stop" | "StopFailure" | "Interrupt" | "SessionEnd") {
        runtime::update(id, |r| {
            if r.current_shift() && !matches!(r.status.as_str(), "running" | "waiting") {
                // Without a matching submit, release the batch for redelivery after the turn.
                r.opening_mail = None;
                r.turn_via.clear();
                r.bell_via = None;
                r.user_words = None;
            }
        });
    }
    let again = matches!(ev, "Stop" | "StopFailure") || (ev == "SessionStart" && !bell_agent);
    if again && wants_wake(id) {
        later(id, 1, &[]);
    }
}

/// `aca start` / `new` just started it: if anything is undelivered, deliver once a few seconds later. Doorbell tools get
/// it once they connect the doorbell themselves (`aca inbox watch`); the rest can't wait for their SessionStart — Codex's
/// is only reported after the first prompt
pub fn launched(cfg: &Config, s: &Session) {
    let bell = agent::of(cfg, s).is_ok_and(|(a, _)| a.delivery() == Delivery::Bell);
    if !bell && wants_wake(&s.id) {
        later(&s.id, 5, &[]);
    }
}

pub(crate) fn connected_bell(cfg: &Config, s: &Session) -> bool {
    runtime::load(&s.id).bell && agent::of(cfg, s).is_ok_and(|(a, _)| a.delivery() == Delivery::Bell)
}

/// Running / waiting for a confirmation: don't deliver yet, usually wait for its own end-of-turn hook. If the screen looks
/// like running but no hook said a turn is running (just started and loading, the startup gates), that hook never comes:
/// look again by itself after a while
fn wait_turn(id: &str, retry: bool, why: &str) -> Res<Sent> {
    if !retry && !matches!(runtime::load(id).status.as_str(), "running" | "waiting") {
        later(id, RETRY_SECS, &["--retry"]);
    }
    Ok(Sent::Held(why.into()))
}

/// Delivers a scheduled one once in the background when it's due
fn later_at(id: &str, at: u64) {
    later(id, 0, &["--at", &at.to_string()]);
}

/// Delivers once in the background after `secs` seconds (`aca inbox deliver`): not waited for, so hooks and commands
/// return at once
fn later(id: &str, secs: u64, more: &[&str]) {
    let _ = crate::proc::detach(
        Command::new(agent::self_exe())
            .args(["inbox", "deliver", id, "--after", &secs.to_string()])
            .args(more)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

fn ring(id: &str) {
    let _ = std::fs::create_dir_all(paths::inbox_dir());
    // Changed in place, not replaced: with a new file, the side watching it might lose track
    let _ = std::fs::write(bell(id), paths::now_ms().to_string());
}

/// It isn't running: deliver once it starts
const NOT_RUNNING: Tr = Tr::new("它未运行:启动后再送达", "it isn't running: delivered when it starts");

/// One delivery attempt at the moment of sending (and each retry after): looks at which tool the other side is and what
/// it's doing now, and picks a way to deliver. `retry` = this is already a retry after a while
pub fn deliver(store: &Store, cfg: &Config, id: &str, retry: bool) -> Res<Sent> {
    let s = store.get(id)?;
    let shift = crate::router::shift_lock(store, cfg, &s)?;
    let slot = tmux::find(&s.id);
    let pane = slot.as_ref().filter(|p| !p.dead);
    let mut msgs = pending(&s.id);
    if !msgs.iter().any(|m| m.wake) {
        return Ok(Sent::Nothing);
    }
    let (agent, _) = agent::of(cfg, &s)?;
    if shift.is_some() {
        if crate::session::wake_shift(cfg, &s, slot.as_ref())? {
            return Ok(Sent::Starting);
        }
        // The hook may have acknowledged the opening while we checked the pane. Take a consistent new batch.
        let _mail = lock(&s.id);
        if runtime::load(&s.id).opening_mail.is_some() {
            return Ok(Sent::Held(t!("等新一班接收交接", "waiting for the new shift to receive its handoff").into()));
        }
        msgs = pending(&s.id);
        if !msgs.iter().any(|m| m.wake) {
            return Ok(Sent::Nothing);
        }
    }
    let r = runtime::load(&s.id);
    let held = |why: &str| Ok(Sent::Held(why.to_string()));
    let sent = match agent.delivery() {
        Delivery::Keep => {
            held(t!("它是 shell,不会输入到其中;可在 aca inbox 中查看", "it's a shell, nothing is typed into it; see aca inbox"))
        }
        Delivery::Bell if pane.is_none() => held(NOT_RUNNING.get()),
        Delivery::Bell if r.bell => {
            // A concurrent sender or FileChanged hook may have taken the first snapshot.
            let _mail = lock(&s.id);
            let mut msgs = pending(&s.id);
            let ids: Vec<String> = msgs.iter().filter(|m| m.wake).map(|m| m.id.clone()).collect();
            if ids.is_empty() {
                return Ok(Sent::Nothing);
            }
            msgs.retain(Msg::carries_user_words);
            // A connected bell takes over immediately if typing is held or fails.
            if !msgs.is_empty() && matches!(type_in(cfg, &s, &*agent, pane.unwrap(), &msgs, true), Ok(Sent::Typed)) {
                mark_locked(&s.id, &msgs.into_iter().map(|m| m.id).collect::<Vec<_>>(), "type");
                return Ok(Sent::Typed);
            }
            ring(&s.id);
            later(&s.id, BELL_CHECK_SECS, &["--check", &ids.join(",")]);
            Ok(Sent::Bell)
        }
        // Not into a cage's queue: that's a Codex run here on its config directory, which the one inside has open too
        Delivery::Queue if !r.conv.is_empty() && crate::sandbox::cage_of_session(cfg, &s).is_none() => {
            if pane.is_some() {
                match status::one(cfg, &s, true).state {
                    State::Running => {
                        return wait_turn(&s.id, retry, t!("它正在运行:本轮结束后再排入", "it's running: queued after this turn"));
                    }
                    State::Ask => {
                        return wait_turn(
                            &s.id,
                            retry,
                            t!(
                                "它正在等待确认:确认后、本轮结束时再排入",
                                "it's waiting for a confirmation: queued once that's answered and the turn ends"
                            ),
                        );
                    }
                    // In an interrupted conversation Codex doesn't start on queued messages by itself either (same after exiting and
                    // resuming; tested on 0.162): wait for the next turn to finish normally
                    _ if r.event == "Interrupt" => {
                        return held(t!(
                            "你刚按 Esc 中断了它:下一轮结束后再送达",
                            "you just interrupted it with Esc: delivered after your next turn ends"
                        ));
                    }
                    _ => {}
                }
            }
            let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
            match enqueue(cfg, &s, &*agent, &r.conv, &render_as_input(&msgs, paths::now(), &s.parent)) {
                Ok(()) => {
                    mark_queued(&s.id, &ids);
                    Ok(Sent::Queued)
                }
                Err(e) if pane.is_none() => held(&tr!(
                    "无法排入它的队列({e}),且它未运行:启动后再送达",
                    "can't queue it ({e}) and it isn't running: delivered when it starts"
                )),
                Err(_) => type_in(cfg, &s, &*agent, pane.unwrap(), &msgs, retry),
            }
        }
        _ => match pane {
            None => held(NOT_RUNNING.get()),
            Some(p) => type_in(cfg, &s, &*agent, p, &msgs, retry),
        },
    }?;
    if sent == Sent::Typed {
        mark(&s.id, &msgs.into_iter().map(|m| m.id).collect::<Vec<_>>(), "type");
    }
    Ok(sent)
}

/// Queue for Codex: first write with `-c` in our own process (without bringing up the background service); if this
/// account already has a background service running it refuses, so hand it to that service
fn enqueue(cfg: &Config, s: &Session, agent: &dyn Agent, conv: &str, text: &str) -> Res<()> {
    let run = |shared: bool| -> Res<()> {
        let l = agent::tool_cmd(cfg, s, &agent.queue_args(conv, text, shared))?;
        let mut c = Command::new(&l.argv[0]);
        c.args(&l.argv[1..]).envs(l.env).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        // Wrapped in a script (changing colors per pane): don't let it think it was started inside some pane
        c.env_remove("TMUX").env_remove("TMUX_PANE");
        if std::path::Path::new(&s.dir).is_dir() {
            c.current_dir(&s.dir);
        }
        let mut child = c.spawn().map_err(|e| tr!("无法启动:{e}", "can't start: {e}"))?;
        let t = Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
                break st;
            }
            if t.elapsed() > Duration::from_secs(20) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(t!("20 秒内无响应", "no answer within 20 seconds").into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        if status.success() { Ok(()) } else { Err(clip(err.trim(), 300)) }
    };
    match run(false) {
        Err(e) if e.contains("daemon is running") => run(true),
        other => other,
    }
}

/// Delivers by typing: only types when it's idle, you aren't typing in that pane, and the input box has no unsent text of
/// yours; otherwise look again after a while
fn type_in(cfg: &Config, s: &Session, agent: &dyn Agent, pane: &tmux::Pane, msgs: &[Msg], retry: bool) -> Res<Sent> {
    let why = match status::one(cfg, s, true).state {
        State::Running => return wait_turn(&s.id, retry, t!("它正在运行:本轮结束后再送达", "it's running: delivered after this turn")),
        State::Ask => {
            return wait_turn(
                &s.id,
                retry,
                t!("它正在等待确认:不会输入到确认框", "it's waiting for a confirmation: nothing is typed into the prompt"),
            );
        }
        _ if tmux::typing(&pane.pane, TYPING_SECS) => Some(t!("你正在该窗格中输入", "you're typing in that pane")),
        _ => match agent.prompt_mark().map(|m| draft(m, &tmux::capture_ansi(&pane.pane))) {
            Some(Some(true)) => Some(t!("输入框中有你未发送的内容", "its input box has text you haven't sent")),
            Some(None) => Some(t!("屏幕上找不到它的输入框", "can't find its input box on screen")),
            _ => None,
        },
    };
    if let Some(why) = why {
        // You're typing, or there's a draft: look again after a while; if it still can't, wait for its next hook
        if !retry {
            later(&s.id, RETRY_SECS, &["--retry"]);
        }
        return Ok(Sent::Held(why.into()));
    }
    let input = render_as_input(msgs, paths::now(), &s.parent);
    tmux::paste(&pane.pane, &input)?;
    let words = opening_words(msgs);
    if words.as_ref().is_some_and(|w| !w.via.is_empty()) || Store::open().is_ok_and(|st| crate::task::is_router(&st, s)) {
        runtime::update(&s.id, |r| r.router_input = Some((pasted_input_hash(r, &input), words)));
    }
    std::thread::sleep(Duration::from_millis(150));
    tmux::keys(&pane.pane, &["Enter"])?;
    Ok(Sent::Typed)
}

/// `aca msg`: puts a message in the other side's inbox and tries delivering it at once. Called from inside a session, the
/// sender is that session; `parent` = the session that delegated it. A nonzero `at` makes it scheduled: delivered at that
/// moment, and it may be sent to yourself (to wake yourself up later). `via`: the channel your words came in through,
/// named by whatever relays them (empty = none); only what is sent from outside a session can name one
pub fn send(store: &Store, cfg: &Config, to: &str, text: &str, at: u64, via: &str) -> Res<(Msg, Sent)> {
    let me = std::env::var("ACA_SESSION_ID").ok().filter(|x| !x.is_empty()).and_then(|x| store.get(&x).ok());
    send_as(store, cfg, me, to, text, at, via)
}

/// [`send`] from the sender named (None = you) instead of the session this process runs in: for what aca passes on in
/// someone's name, wherever it happens to run
pub fn send_as(store: &Store, cfg: &Config, me: Option<Session>, to: &str, text: &str, at: u64, via: &str) -> Res<(Msg, Sent)> {
    send_message(store, cfg, me, to, Msg { text: text.into(), at, via: via.into(), ..Default::default() })
}

pub(crate) fn send_message(store: &Store, cfg: &Config, me: Option<Session>, to: &str, mut msg: Msg) -> Res<(Msg, Sent)> {
    let (via, at) = (msg.via.as_str(), msg.at);
    if !via.is_empty() {
        // A session's own messages say who sent them already; a channel is where the user's words came from
        if me.is_some() {
            return Err(t!("--via 只能在会话外使用", "--via only works from outside a session").into());
        }
        crate::plugin::Channel::check_name(via)?;
    }
    let to = match to {
        "parent" => {
            let me = me.as_ref().ok_or(t!(
                "parent 指派出当前会话的那个会话:只能在会话中使用",
                "parent is the session that dispatched this one: only works from inside a session"
            ))?;
            store
                .get(&me.parent)
                .map_err(|_| t!("当前会话没有 parent(不是被派出的会话)", "this session has no parent (it wasn't dispatched)").to_string())?
        }
        key => store.get(key)?,
    };
    // A cage is closed (see crate::sandbox): you can talk to its sessions, other sessions can't (unless it takes work in)
    if let (Some(m), Some((name, _))) = (&me, crate::sandbox::cage_of_session(cfg, &to).filter(|(_, b)| !b.delegate))
        && m.id != to.id
    {
        return Err(tr!(
            "{} 在沙箱 {name} 中,沙箱是封闭的:不从别的会话往里传话([sandbox.{name}] 写 delegate = true 打开)",
            "{} is inside sandbox {name}, which is closed: no messages go in from other sessions (delegate = true in [sandbox.{name}] opens it)",
            to.id
        ));
    }
    let (from, who) = match &me {
        Some(m) if m.id == to.id && at == 0 => {
            return Err(t!("不能发送给自己(定时的可以:--in)", "can't send to yourself (a scheduled one can: --in)").into());
        }
        Some(m) => (m.id.clone(), who(m, &runtime::load(&m.id))),
        None => ("you".to_string(), "你".to_string()),
    };
    // Preserve whitespace and long pastes in the input box and the router's handoff.
    let keep_words = me.is_none() && msg.what.is_empty() && (crate::task::is_router(store, &to) || (at == 0 && connected_bell(cfg, &to)));
    if !keep_words {
        msg.text = clip(crate::task::said(store, me.as_ref(), &msg.text), 4000);
        if at == 0 && msg.what.is_empty() {
            msg.prepend_quote(&router_quote(store, me.as_ref(), &to.id));
        }
    }
    let what = if !msg.what.is_empty() && crate::task::is_router(store, &to) { msg.what } else { String::new() };
    let m = post(Msg { from, who, to: to.id.clone(), kind: "msg".into(), wake: true, what, ..msg });
    if m.wake && at > paths::now() {
        later_at(&to.id, at);
        return Ok((m, Sent::Scheduled(at)));
    }
    if !m.wake {
        return Ok((
            m,
            Sent::Held(
                t!("10 分钟内发送过多:此条只保存,不唤醒", "too many within 10 minutes: this one is only kept, nobody is woken").into(),
            ),
        ));
    }
    // The message is in the inbox now: a failed attempt (a router's new shift that didn't come up, a paste tmux refused…)
    // doesn't undo that. Saying it failed would have the sender send it again and the inbox hold it twice; instead it's
    // held, tried once more in a while, and otherwise waits for the receiver's own hooks like any held message
    let sent = deliver(store, cfg, &to.id, false).unwrap_or_else(|e| {
        later(&to.id, RETRY_SECS, &["--retry"]);
        let e = clip(e.trim(), 300);
        Sent::Held(tr!("无法送达({e}),{RETRY_SECS} 秒后重试一次", "couldn't deliver it ({e}); trying once more in {RETRY_SECS} seconds"))
    });
    Ok((m, sent))
}

/// `aca inbox take`: Claude's doorbell hook (FileChanged, asyncRewake). If there's something to wake for, writes it to
/// stderr and exits 2; otherwise exits 0
pub fn take_cmd() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    // Claude's background sessions inherit the original pane's environment; they aren't this session itself
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    let Some(text) = take_bell(&id) else { return 0 };
    eprint!("{text}");
    2
}

fn take_bell(id: &str) -> Option<String> {
    let store = Store::open().ok();
    let session = store.as_ref().and_then(|st| st.get(id).ok());
    let _shift = match (store.as_ref(), session.as_ref()) {
        (Some(st), Some(s)) => crate::router::shift_lock(st, &crate::config::load(), s).ok()?,
        _ => None,
    };
    if !runtime::load(id).current_shift() {
        return None;
    }
    let got = take(id);
    if got.is_empty() {
        return None;
    }
    let me = session.unwrap_or_default();
    let router = store.as_ref().is_some_and(|st| crate::task::is_router(st, &me));
    let words = opening_words(&got);
    runtime::update(id, |r| {
        if !matches!(r.status.as_str(), "running" | "waiting") {
            r.turn_via = words.as_ref().map(|w| w.via.clone()).unwrap_or_default();
        }
        if router {
            r.router_input = None;
            r.user_words = words;
            r.bell_via = Some(r.turn_via.clone());
        }
    });
    Some(render_bell(&got, paths::now(), &me.parent, hears_done(&crate::config::load(), &me), router))
}

/// `aca inbox watch`: the extra one run on Claude's SessionStart and CwdChanged. Replies with `watchPaths` so it watches
/// the doorbell; if anything is undelivered, rings once two seconds later (after it starts watching).
///
/// CwdChanged too because Claude replaces its watch list with what the CwdChanged hooks return whenever its working
/// directory changes, even a `cd` it resets right away (read in 2.1.318's code, reproduced: after `cd /tmp && pwd` the
/// doorbell went unanswered and the message fell back to typing)
pub fn watch_cmd() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|s| !s.is_empty()) else { return 0 };
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    // The reply has to name the event it answers
    let event = serde_json::from_str::<Value>(&input)
        .ok()
        .and_then(|v| v["hook_event_name"].as_str().map(str::to_string))
        .filter(|e| e == "CwdChanged")
        .unwrap_or_else(|| "SessionStart".into());
    if !runtime::load(&id).current_shift() {
        return 0;
    }
    let b = bell(&id);
    if !b.exists() {
        ring(&id);
    }
    runtime::update(&id, |r| r.bell = true);
    println!("{}", json!({ "hookSpecificOutput": { "hookEventName": event, "watchPaths": [b] } }));
    if wants_wake(&id) {
        later(&id, 2, &[]);
    }
    0
}

/// `aca inbox deliver <id> [--after secs] [--at time] [--retry] [--check id,…]`: the follow-up hooks and commands leave,
/// delivering once after a while. `--at`: a scheduled one is delivered at this moment. `--check`: the doorbell was rung and
/// these still weren't taken = this run of Claude didn't connect the doorbell, so use typing from now on
pub fn deliver_cmd(id: &str, after: u64, at: u64, retry: bool, check: &[String]) -> i32 {
    std::thread::sleep(Duration::from_secs(after));
    // The due moment goes by the wall clock: time asleep with the lid closed counts as passed, so it's delivered on waking
    while paths::now() < at {
        std::thread::sleep(Duration::from_secs(at.saturating_sub(paths::now()).min(30)));
    }
    // The session was deleted while waiting: don't open the registry (opening it would recreate the whole directory)
    if !paths::run_file(id).exists() {
        return 0;
    }
    if !check.is_empty() {
        if !pending(id).iter().any(|m| check.contains(&m.id)) {
            return 0;
        }
        runtime::update(id, |r| r.bell = false);
    }
    let Ok(store) = Store::open() else { return 0 };
    let _ = deliver(&store, &crate::config::load(), id, retry);
    0
}

/// What `aca wait` ended on
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// Finished a turn (or went idle)
    Done,
    Asking,
    Failed,
    Exited,
    Timeout,
}

impl Outcome {
    pub fn code(self) -> i32 {
        match self {
            Outcome::Done => 0,
            Outcome::Asking => 2,
            Outcome::Failed => 3,
            Outcome::Exited => 4,
            Outcome::Timeout => 124,
        }
    }
}

/// Whether a session counts as "done waiting" right now. `since` = when waiting started: in the first few seconds, a
/// finish has to come after waiting started (a prompt just sent hasn't become "running" yet, so it still looks like the
/// last turn's "finished"); with messages still undelivered, it hasn't started yet
fn outcome(cfg: &Config, s: &Session, since: u64, now: u64) -> Option<Outcome> {
    // Read the screen: Claude sends no hook when interrupted with Esc, so going by hooks alone it would be "running" forever
    let (pane, r, seen) = status::observe(cfg, s, true);
    let (state, _, _) = status::merge(pane.as_ref(), &r, seen.as_ref(), now);
    match state {
        State::Running => None,
        // Asking at startup whether to trust the folder, with the setting on: the follow-up left when starting it is about to
        // answer for you, so it doesn't need you yet
        State::Ask if crate::trust::answering(cfg, s) => None,
        State::Ask => Some(Outcome::Asking),
        State::Error => Some(Outcome::Failed),
        State::Exited | State::Stopped => Some(Outcome::Exited),
        State::Idle | State::Unread => {
            if pane.and_then(|p| r.codex_turn(p.pid)).is_some_and(|t| t != runtime::CodexTurn::Done) {
                return None;
            }
            let busy = list(&s.id).iter().any(|(m, st)| m.wake && *st != Stage::Got);
            let never = r.turn == 0 && r.done == 0;
            let early = now.saturating_sub(since) < 3 && r.done < since;
            // Ended the turn with background tasks still running (Claude): it's woken for a new turn when they end, so it
            // isn't done yet. One that never ends (a server left running) is waited on until --timeout
            let bg = r.status == "done" && r.bg > 0;
            (!busy && !never && !early && !bg).then_some(Outcome::Done)
        }
    }
}

/// `aca wait`: waits for these sessions to finish a turn / wait for a confirmation / error / exit. `any` = return as soon
/// as one does. Called from inside a session: notices from the ones waited for are marked delivered, so the doorbell
/// won't wake it for them again
pub fn wait(store: &Store, cfg: &Config, keys: &[String], any: bool, timeout: Option<Duration>) -> Res<Vec<(Session, Outcome)>> {
    let sessions: Vec<Session> = keys.iter().map(|k| store.get(k)).collect::<Res<_>>()?;
    if sessions.is_empty() {
        return Err(t!("需要会话", "needs a session").into());
    }
    let since = paths::now();
    let t = Instant::now();
    let mut done: Vec<Option<Outcome>> = vec![None; sessions.len()];
    loop {
        let now = paths::now();
        for (s, d) in sessions.iter().zip(done.iter_mut()) {
            if d.is_none() {
                *d = outcome(cfg, s, since, now);
            }
        }
        let finished = done.iter().filter(|d| d.is_some()).count();
        if finished == sessions.len() || (any && finished > 0) {
            break;
        }
        if timeout.is_some_and(|w| t.elapsed() >= w) {
            if !any {
                done.iter_mut().filter(|d| d.is_none()).for_each(|d| *d = Some(Outcome::Timeout));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let out: Vec<(Session, Outcome)> = sessions.into_iter().zip(done).filter_map(|(s, d)| d.map(|d| (s, d))).collect();
    if let Some(me) = std::env::var("ACA_SESSION_ID").ok().filter(|x| !x.is_empty()) {
        // Those sessions' hooks are putting notices here right now: wait for them to land, then mark them delivered
        std::thread::sleep(Duration::from_millis(600));
        let from: Vec<&str> = out.iter().filter(|(_, d)| *d != Outcome::Timeout).map(|(s, _)| s.id.as_str()).collect();
        let ids: Vec<String> = pending(&me)
            .into_iter()
            .filter(|m| m.kind == "notice" && from.contains(&m.from.as_str()) && m.ts + 2 >= since)
            .map(|m| m.id)
            .collect();
        if !ids.is_empty() {
            mark(&me, &ids, "wait");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_immediate_user_messages_and_router_quotes_carry_user_words() {
        // The interface and the external CLI both post as "you"; a relay also names its channel.
        let user = Msg { from: "you".into(), ..msg("m1", "msg", "可以") };
        let peer = msg("m2", "msg", "用户对「router」说(界面,10:00):\n「可以」");
        let mut words = user_words("可以", "Discord", 100);
        let mut from_router = || {
            let mut m = peer.clone();
            m.prepend_quote(&quote(&mut words, "router", &peer.to));
            m
        };
        let quoted = from_router();
        for m in [&user, &Msg { via: "Discord".into(), ..user.clone() }, &quoted] {
            assert!(m.carries_user_words(), "{m:?}");
        }
        assert!(opening_words(&[quoted]).is_none());
        for m in [
            from_router(),
            peer.clone(),
            Msg { kind: "notice".into(), ..user.clone() },
            Msg { what: "task.created".into(), ..user.clone() },
            Msg { at: 1, ..user.clone() },
            Msg { from: peer.to.clone(), at: 1, ..peer },
            Msg { via: NATIVE.into(), ..user },
        ] {
            assert!(!m.carries_user_words(), "{m:?}");
        }
    }

    #[test]
    fn old_inbox_lines_default_to_no_quote_and_only_quotes_write_the_flag() {
        let line = r#"{"id":"m1","from":"router","kind":"msg","wake":true,"text":"可以"}"#;
        let mut m: Msg = serde_json::from_str(line).unwrap();
        assert!(!m.quoted);
        assert!(serde_json::to_value(&m).unwrap().get("quoted").is_none());
        m.quoted = true;
        let record = serde_json::to_value(&m).unwrap();
        assert_eq!(record["quoted"], true);
        assert_eq!(serde_json::from_value::<Msg>(record).unwrap(), m);
    }

    fn msg(id: &str, kind: &str, text: &str) -> Msg {
        Msg {
            id: id.into(),
            ts: 100,
            from: "ef56".into(),
            who: "ef56「读文档」codex:labs".into(),
            to: "ab12".into(),
            kind: kind.into(),
            what: if kind == "notice" { "turn.done".into() } else { String::new() },
            wake: true,
            text: text.into(),
            at: 0,
            via: String::new(),
            quoted: false,
        }
    }

    #[test]
    fn watched_worker_events_go_to_the_project_or_its_dispatcher_only_while_assigned() {
        use crate::store::Task;
        let store = Store::memory();
        let worker = Session { id: "worker".into(), group: "project".into(), ..Default::default() };
        for id in ["project-router", "desk", "worker"] {
            store.insert(&Session { id: id.into(), ..worker.clone() }).unwrap();
        }
        let r =
            Run {
                last: "最后一句".into(), ask: "选哪项？".into(), error: "连接失败".into(), notify: "none".into(), ..Default::default()
            };
        let events = [("turn.done", "done"), ("agent.waiting", "waiting"), ("turn.failed", "failed"), ("agent.exited", "exited")];
        let check = |expected: Option<&str>| {
            for (ev, short) in events {
                let got = notices(&store, &worker, &r, ev, short, &Config::default());
                assert_eq!(got.len(), usize::from(expected.is_some()), "{ev}: {got:?}");
                if let Some(to) = expected {
                    let m = &got[0];
                    assert_eq!(m.to, to);
                    assert_eq!(m.what, format!("task.{ev}"));
                    assert!(m.wake && m.text.contains("#1") && m.text.contains(&r.last) && m.text.contains("look worker"));
                    if short == "waiting" {
                        assert!(m.text.contains(&r.ask));
                    }
                    if short == "failed" {
                        assert!(m.text.contains(&r.error));
                    }
                }
            }
        };
        let task = Task { group: worker.group.clone(), given_by: "desk".into(), session: worker.id.clone(), ..Task::default() };
        let t = store.task_add(&task).unwrap();
        check(None);
        store.with_watch(t.n, Some(true), || Ok(())).unwrap();
        check(Some("desk"));
        store.group_set_router("project", "project-router").unwrap();
        check(Some("project-router"));
        store.task_done(t.n, "好了").unwrap();
        check(None);
    }

    #[test]
    fn a_router_parent_gets_one_exit_notice_for_a_worker_with_a_ticket() {
        let store = Store::memory();
        let parent = Session { id: "router".into(), group: "project".into(), ..Default::default() };
        let worker = Session { id: "worker".into(), group: parent.group.clone(), parent: parent.id.clone(), ..Default::default() };
        store.insert(&parent).unwrap();
        store.insert(&worker).unwrap();
        store.group_set_router(&parent.group, &parent.id).unwrap();
        let task = crate::store::Task { group: parent.group.clone(), session: worker.id.clone(), ..Default::default() };
        let t = store.task_add(&task).unwrap();

        let mut cfg = Config::default();
        cfg.inbox.notify = vec!["exited".into(), "done".into(), "waiting".into(), "failed".into()];
        let run = Run::default();
        for (ev, short) in [("turn.done", "done"), ("agent.waiting", "waiting"), ("turn.failed", "failed")] {
            assert!(notices(&store, &worker, &run, ev, short, &cfg).is_empty(), "{ev}");
        }
        let check_exit = |what: &str| {
            let msgs = notices(&store, &worker, &run, "agent.exited", "exited", &cfg);
            assert_eq!(msgs.len(), 1, "{msgs:?}");
            assert_eq!(msgs[0].to, parent.id);
            assert_eq!(msgs[0].what, what);
            assert!(msgs[0].wake);
        };
        check_exit("agent.exited");
        store.with_watch(t.n, Some(true), || Ok(())).unwrap();
        check_exit("task.agent.exited");
    }

    #[test]
    fn doorbell_submit_preserves_the_taken_words_and_channel_for_forwarding() {
        // Exercise the real inbox, registry and hook in an isolated process.
        let name = "inbox::tests::doorbell_submit_preserves_the_taken_words_and_channel_for_forwarding";
        if std::env::var_os("ACA_TEST_BELL").is_none() {
            let dir = std::env::temp_dir().join(format!("aca-unit-bell-{}", paths::short_id()));
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name])
                .env("ACA_TEST_BELL", "1")
                .env("HOME", &dir)
                .env("ACA_HOME", &dir)
                .env("ACA_TMUX_SOCKET", dir.file_name().unwrap())
                .env_remove("ACA_ROUTER_SHIFT")
                .output()
                .unwrap();
            std::fs::remove_dir_all(dir).unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
            return;
        }
        let store = Store::open().unwrap();
        let s = Session { id: "ab12".into(), title: "aps".into(), tool: "claude".into(), group: "probe".into(), ..Default::default() };
        store.insert(&s).unwrap();
        store.group_set_router(&s.group, &s.id).unwrap();
        let cfg = crate::config::load();
        let (agent, _) = agent::of(&cfg, &s).unwrap();
        let words = "怎么还失败了,派个 agent 看看。…";
        for via in ["", "Discord"] {
            let m = post(Msg { via: via.into(), from: "you".into(), ..msg("", "msg", words) });
            let text = take_bell(&s.id).unwrap();
            let prompt = format!(
                "<task-notification>\n<summary>aca: 收件箱有新消息</summary>\n</task-notification>\n<system-reminder>\n[aca] {text}\n</system-reminder>"
            );
            crate::hook::feed(&cfg, &s.id, &*agent, &json!({"hook_event_name":"UserPromptSubmit", "prompt":prompt}));
            let source = if via.is_empty() { "界面" } else { via };
            let clock = crate::schedule::clock(m.ts, m.ts);
            assert_eq!(
                forward(&store, Some(&s), "worker", "查一下"),
                format!("用户对「aps」说({source},{clock}):\n「{words}」\naps:查一下")
            );
            assert_eq!(forward(&store, Some(&s), "worker", "补一句"), "补一句");
            assert_eq!(runtime::load(&s.id).turn_via, via);
            crate::hook::feed(&cfg, &s.id, &*agent, &json!({"hook_event_name":"Stop","last_assistant_message":"好"}));
        }
    }

    #[test]
    fn only_the_user_who_opened_this_turn_is_quoted_once_per_recipient() {
        let mut r = Run::default();
        let now = 1234;
        let clock = crate::schedule::clock(now, now);
        let mut words = typed_words(&mut r, "  可以,,你定吧\n ", now).unwrap();
        assert_eq!(quote(&mut words, "aps", "w1"), format!("用户对「aps」说(界面,{clock}):\n「  可以,,你定吧\n 」\naps:"));
        assert!(quote(&mut words, "aps", "w1").is_empty());
        assert!(!quote(&mut words, "aps", "w2").is_empty());
        let mut m = Msg { from: "you".into(), text: "界".repeat(4501), ts: now, ..msg("m1", "msg", "") };
        for via in ["", "Discord"] {
            m.via = via.into();
            let mut w = opening_words(std::slice::from_ref(&m)).unwrap();
            assert_eq!((w.text.chars().count(), w.chars, w.via.as_str()), (4000, 4501, via));
            let source = if via.is_empty() { "界面" } else { via };
            assert_eq!(
                quote(&mut w, "aps", "w1"),
                format!("用户对「aps」说({source},{clock}):\n「{}」(共 4501 字,已截)\naps:", "界".repeat(1500))
            );
        }
        r.router_input = Some((input_hash(&m.text), opening_words(std::slice::from_ref(&m))));
        assert_eq!(typed_words(&mut r, &m.text, 9999).unwrap().ts, now);
        assert_eq!(typed_words(&mut r, &m.text, 9999).unwrap().ts, 9999);
        for input in ["[aca] 来自会话 w1:消息", "<task-notification>ok", "<cross-session-message/>"] {
            assert!(typed_words(&mut r, input, now).is_none());
        }
        m.at = now;
        assert!(opening_words(std::slice::from_ref(&m)).is_none());
        m.from = "w1".into();
        m.at = 0;
        assert!(opening_words(&[m]).is_none());
    }

    #[test]
    fn only_a_single_claude_paste_confirms_the_expected_router_input() {
        let body = "  [aca] 项目交接\n「保留原话」\n`echo literal`  \n";
        let wrapped = format!("\n\n<pasted_content id=\"402f\">\n{body}\n</pasted_content id=\"402f\">\n");
        let matched = |expected: &str, prompt: &str| {
            let mut r = Run {
                shift: "current".into(),
                router_input: Some((input_hash(expected), Some(user_words("原信", "Discord", 1)))),
                opening_mail: Some(vec!["mail".into()]),
                ..Default::default()
            };
            let origin = typed_words(&mut r, prompt, 2);
            assert!(r.router_input.is_none());
            let confirmed = r.opening_mail.is_some();
            assert_eq!(origin.is_some_and(|w| w.via == "Discord" && w.text == "原信" && w.ts == 1), confirmed);
            confirmed
        };
        assert!(matched(body, body));
        assert!(matched(body, &wrapped));
        assert!(matched(body, &wrapped.replace("402f", "ab19")));
        assert!(!matched(body.trim(), &wrapped));
        assert!(!matched(body, &wrapped.replacen("402f", "ab19", 1)));
        assert!(!matched(body, &format!("前{wrapped}")));
        assert!(!matched(body, &format!("{wrapped}后")));
        assert!(!matched(&format!("前\n\n{body}\n后"), &format!("前{wrapped}后")));
        let pair = format!("{wrapped}{wrapped}");
        let joined = pair.trim().strip_prefix("<pasted_content id=\"402f\">\n").unwrap();
        let joined = joined.strip_suffix("\n</pasted_content id=\"402f\">").unwrap();
        assert!(!matched(joined, &pair));
    }

    #[test]
    fn a_wrapped_reply_pasted_by_aca_keeps_its_inbox_origin_once() {
        let mail = [Msg { from: "you".into(), via: "Discord".into(), ..msg("m1", "msg", "  请继续\n保留「这句」  ") }];
        let input = render_as_input(&mail, 1000, "");
        let origin = opening_words(&mail).unwrap();
        let mut router = Run {
            shift: "current".into(),
            router_input: Some((input_hash(&input), Some(origin.clone()))),
            ..Default::default()
        };
        let prompt = format!("\n\n<pasted_content id=\"402f\">\n{input}\n</pasted_content id=\"402f\">\n");
        assert_eq!(typed_words(&mut router, &prompt, 2000), Some(origin));
        assert_eq!(typed_words(&mut router, &prompt, 3000), Some(user_words(&input, "", 3000)));
    }

    #[test]
    fn claude_paste_quotes_keep_the_body_and_the_users_surrounding_text() {
        let body = "  第一行\n「第二行」\n ";
        let block = format!("<pasted_content id=\"402f\">\n{body}\n</pasted_content id=\"402f\">");
        let wrapped = format!("\n\n{block}\n");
        let mut router = Run { shift: "current".into(), ..Default::default() };
        let mut words = |prompt: &str| typed_words(&mut router, prompt, 1).unwrap().text;
        assert_eq!(words(&wrapped), body);
        let mixed = format!("我说\n{block} 接着 {}。\n", block.replace("402f", "a0be"));
        assert_eq!(words(&mixed), format!("我说\n{body} 接着 {body}。\n"));
        let mismatched = block.replacen("402f", "ab19", 1);
        assert_eq!(words(&mismatched), mismatched);
        assert_eq!(typed_words(&mut Run::default(), &wrapped, 1).unwrap().text, wrapped);
    }

    #[test]
    fn escaped_tags_and_a_final_newline_confirm_only_the_same_handoff() {
        let original = concat!(
            "[aca] 项目交接 · probe\n",
            "17:57  用户  <pasted_content id=\"402f\"> 上一班\n",
            "</pasted_content id=\"402f\">\n",
        );
        let received = concat!(
            "\n\n<pasted_content id=\"c90b\">\n",
            "[aca] 项目交接 · probe\n",
            "17:57  用户  <\\pasted_content id=\"402f\"> 上一班\n",
            "<\\/pasted_content id=\"402f\">\n",
            "</pasted_content id=\"c90b\">\n",
        );
        for (prompt, matches) in [(received.to_owned(), true), (received.replace("上一班", "下一班"), false)] {
            let mut router = Run {
                shift: "current".into(),
                opening_mail: Some(vec!["m1".into()]),
                router_input: Some((input_hash(original), None)),
                ..Default::default()
            };
            assert!(typed_words(&mut router, &prompt, 100).is_none());
            assert_eq!(router.opening_mail.is_some(), matches);
        }
        assert_eq!(input_hash("a<\\b \t\n"), input_hash("a<b"));
        assert_ne!(input_hash(" a<b"), input_hash("a<b"));
        let mut ordinary = Run::default();
        ordinary.router_input = Some((pasted_input_hash(&ordinary, "reply \n"), Some(user_words("reply \n", "Discord", 1))));
        assert_eq!(typed_words(&mut ordinary, "reply", 2), Some(user_words("reply", "", 2)));
    }

    #[test]
    fn claude_user_quotes_restore_only_paste_tag_escapes() {
        let original = r#"请保留 <pasted_content id="abcd"> 和 </pasted_content>，还有 a<\b 与 <\pasted_content-extra"#;
        let escaped = r#"请保留 <\pasted_content id="abcd"> 和 <\/pasted_content>，还有 a<\b 与 <\pasted_content-extra"#;
        let prompt = format!("\n\n<pasted_content id=\"c90b\">\n{escaped}\n</pasted_content id=\"c90b\">\n");
        let mut router = Run { shift: "current".into(), ..Default::default() };
        assert_eq!(typed_words(&mut router, &prompt, 100).unwrap().text, original);
        assert_eq!(typed_words(&mut Run::default(), &prompt, 100).unwrap().text, prompt);
    }

    #[test]
    fn only_router_doorbells_omit_the_inbox_and_reply_commands() {
        let m = msg("m1", "msg", "原话里有 aca inbox");
        let batch = std::slice::from_ref(&m);
        let body = "1 条新消息(m1)\n1. ef56「读文档」codex:labs · 发来一句话 · 刚刚\n   「原话里有 aca inbox」";
        assert_eq!(render_bell(batch, 100, "", false, true), body);
        assert_eq!(render_bell(batch, 100, "ef56", false, true), format!("{body}  派你干活的会话交代的:直接在这里做"));
        assert_eq!(
            render(batch, 100, "", false),
            "1 条新消息(m1)· 全部:aca inbox\n1. ef56「读文档」codex:labs · 发来一句话 · 刚刚\n   「原话里有 aca inbox」  回话:aca msg ef56 \"…\""
        );
    }

    #[test]
    fn what_the_agent_sees_says_who_what_and_where_to_look_and_carries_the_ids() {
        let a = msg("m0000000a", "notice", "最后一句:「整理好了」  全文:aca output ef56");
        let b = Msg { from: "you".into(), ..msg("m0000000b", "msg", "先别动数据库\n等我回来") };
        let c = msg("m0000000c", "msg", "补一下测试");
        let text = render(&[a.clone(), b.clone(), c.clone()], 220, "", false);
        assert!(text.starts_with("3 条新消息(m0000000a m0000000b m0000000c)"), "{text}");
        assert!(text.contains("1. ef56「读文档」codex:labs · 跑完一轮 · 2 分钟前\n   最后一句:「整理好了」"), "{text}");
        // Multiple lines are indented to line up; messages from the user outside have no 「回话」 (reply)
        assert!(text.contains("2. 用户(在会话外面) · 发来一句话 · 2 分钟前\n   「先别动数据库\n   等我回来」\n"), "{text}");
        assert!(text.contains("「补一下测试」  回话:aca msg ef56"), "{text}");
        // From the session that delegated it: marked as assigned work; it hears "finished", so answering in place gets back,
        // and it isn't told to aca msg again
        let text = render(std::slice::from_ref(&c), 220, "ef56", true);
        assert!(text.contains("「补一下测试」  派你干活的会话交代的:直接在这里做、在这里回答") && !text.contains("aca msg"), "{text}");
        let text = render(std::slice::from_ref(&c), 220, "ef56", false);
        assert!(text.contains("「补一下测试」  派你干活的会话交代的:直接在这里做;要回它用 aca msg ef56"), "{text}");
        // Queued / typed in: the user's go as is, a session's only gets one extra line saying who, without "not typed by the
        // user" or how to reply
        let other = Msg { from: "zz99".into(), who: "zz99「别的」claude:sub".into(), ..c.clone() };
        let input = render_as_input(&[a, b, c, other], 220, "ef56");
        assert_eq!(
            input,
            "[aca] ef56「读文档」codex:labs 跑完一轮 · 2 分钟前\n最后一句:「整理好了」  全文:aca output ef56\n\n\
             先别动数据库\n等我回来\n\n\
             [aca] 派你干活的会话 ef56「读文档」codex:labs 说:\n补一下测试\n\n\
             [aca] 来自会话 zz99「别的」claude:sub:\n补一下测试"
        );
        // A message it scheduled for itself is due: say it scheduled it itself and how long ago, don't teach it to reply to itself
        let me =
            Msg { from: "ab12".into(), who: "ab12「主线」claude:sub".into(), at: 200, ..msg("m0000000d", "msg", "回来看看 CI") };
        let text = render(std::slice::from_ref(&me), 220, "", false);
        assert!(text.ends_with("\n1. 你自己 · 定时到点了 · 2 分钟前定的\n   「回来看看 CI」"), "{text}");
        assert_eq!(render_as_input(&[me], 220, ""), "[aca] 定时到点了(你 2 分钟前定的):\n回来看看 CI");
    }

    #[test]
    fn a_draft_is_any_non_dim_character_after_the_prompt_mark() {
        let l = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Both tools' hints are gray (SGR 2), as captured 2026-10-08
        assert_eq!(draft('❯', &l(&["● 好了", "\x1b[0m❯ \x1b[2mTry \"fix typecheck errors\"\x1b[0m"])), Some(false));
        assert_eq!(draft('›', &l(&["\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[22m"])), Some(false));
        assert_eq!(draft('❯', &l(&["❯ "])), Some(false));
        // You typed something
        assert_eq!(draft('❯', &l(&["❯ 人打的草稿"])), Some(true));
        assert_eq!(draft('›', &l(&["\x1b[2m›\x1b[0m half"])), Some(true));
        // Gray followed by normal-colored text (gray canceled)
        assert_eq!(draft('❯', &l(&["❯ \x1b[2mTry\x1b[22m x"])), Some(true));
        // A 2 among color parameters isn't gray
        assert_eq!(draft('❯', &l(&["❯ \x1b[38;2;2;2;2mx"])), Some(true));
        // Earlier lines in the conversation also start with the mark: look at the last one
        assert_eq!(draft('›', &l(&["› 上一句我说的", "• 回复", "› \x1b[2mAsk Codex to do anything"])), Some(false));
        // No input box (waiting for you to choose, interface not drawn yet)
        assert_eq!(draft('❯', &l(&["Loading…"])), None);
    }

    #[test]
    fn who_gets_told_follows_the_session_then_the_config_and_otherwise_nobody() {
        // Nobody asked for it: no notice
        assert!(!wanted("", &[], "done") && !wanted("", &[], "waiting"));
        let cfg = vec!["done".to_string(), "failed".to_string()];
        assert!(wanted("", &cfg, "done") && !wanted("", &cfg, "waiting"));
        assert!(wanted("waiting, exited", &cfg, "waiting") && !wanted("waiting,exited", &cfg, "done"));
        assert!(!wanted("none", &cfg, "done"));
        assert!(wanted("all", &[], "exited") && wanted("", &["all".to_string()], "waiting"));
    }

    #[test]
    fn a_notice_only_says_what_happened_and_how_to_answer() {
        let r = Run { last: "改好了。\n\n细节:\n  1. 改了 a.rs".into(), ..Default::default() };
        assert_eq!(notice_text("w2", "turn.done", &r), "最后一句:「改好了。 细节: 1. 改了 a.rs」  全文:aca output w2");
        let ask =
            Run { ask: "用哪个库?".into(), ask_kind: "choice".into(), ask_opts: vec!["A".into(), "B".into()], ..Default::default() };
        assert_eq!(notice_text("w2", "agent.waiting", &ask), "在问:「用哪个库?」\n选项:1 A  2 B  回答:aca answer w2 <选项号>");
        let perm = Run { ask: "Bash:rm -rf build".into(), ask_kind: "permission".into(), ..Default::default() };
        assert!(notice_text("w2", "agent.waiting", &perm).ends_with("aca answer w2 yes|no"));
        let multi = Run { ask_multi: true, ..ask };
        assert!(notice_text("w2", "agent.waiting", &multi).contains("界面里答"));
    }

    #[test]
    fn sgr_turns_dim_on_and_off() {
        assert!(sgr_dim("2", false) && sgr_dim("1;2", false) && !sgr_dim("0", true) && !sgr_dim("22", true) && !sgr_dim("", true));
        assert!(!sgr_dim("38;5;2", false) && sgr_dim("38;5;8;2", false));
        assert_eq!(plain("\x1b[1m›\x1b[0m hi"), "› hi");
    }
}
