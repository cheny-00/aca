//! A router's thread: what was said to it, what it answered, what it sent out and which sessions it pulled over, one entry
//! each in the order it happened.
//!
//! Nothing keeps this on its own: it is read off the event stream (`events.jsonl` and the file it was last rotated into),
//! where every one of these already leaves a line — a turn's opening prompt and its last reply, a message put in an inbox
//! and its delivery, a session moved or created. So it reaches back as far as those two files do, and its words are the
//! events': cut to a line or two. The whole exchange is in the router's own conversation.
//!
//! Entries name other sessions by id; whoever shows them puts today's titles in.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::paths;
use crate::runtime::clip;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// You said it: typed to it, or sent with `aca msg` from outside any session
    You,
    /// Another session (`who`) said it to the router
    From,
    /// A session it sent out (`who`) reported something by itself; `text` is the event (`turn.done`, `agent.waiting`…)
    Notice,
    /// One turn of a session holding a ticket, and the result of marking a ticket done.
    WorkerReply,
    WorkerAsks,
    WorkerFailed,
    TaskDone,
    /// The router said it to another session (`who`)
    To,
    /// The router's last reply of a turn
    Reply,
    /// A turn of the router's failed; `text` is why
    Failed,
    /// The router asks you; `text` is the question
    Asks,
    /// The router moved a session (`who`) from group `text` into group `aux`
    Pulled,
    /// The router started a session (`who`), as its child or as nobody's; `text` is the title it gave it
    Started,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Seconds
    pub ts: u64,
    pub kind: Kind,
    /// The other session's id, where there is one
    pub who: String,
    pub text: String,
    /// See [`Kind::Pulled`]
    pub aux: String,
    /// A message's own id (empty for what isn't a message)
    pub id: String,
    /// A message not delivered yet
    pub pending: bool,
    /// What you said: the channel outside aca it came in through (`aca msg --via`); empty = said here
    pub via: String,
}

/// A thread keeps its latest this many entries
const KEEP: usize = 200;
/// How much of an entry's words is kept, in characters
const TEXT: usize = 300;
/// A prompt that opens a turn is compared with a message's words over this many characters (the events cut them at
/// different lengths)
const SAME: usize = 120;
/// Looking for the message a prompt delivers: this many of the thread's latest entries
const NEAR: usize = 20;

/// What opened a turn wasn't typed by you: a message aca delivered by typing (it is in the thread already, as the message
/// put in the inbox), or what Claude starts a turn for by itself — a background task's notice, another Claude session's
/// message (which comes in as a message when the turn ends, see `inbox::native`). Told by how each begins
pub(crate) fn not_typed(prompt: &str) -> bool {
    ["[aca] ", "<task-notification", "<cross-session-message"].iter().any(|p| prompt.starts_with(p))
}

/// Words on one line, cut for comparing
fn same_words(s: &str) -> String {
    s.trim_end_matches('…').split_whitespace().collect::<Vec<_>>().join(" ").chars().take(SAME).collect()
}

fn one_line(s: &str) -> String {
    clip(&s.split_whitespace().collect::<Vec<_>>().join(" "), TEXT)
}

/// The threads of the sessions named as routers, kept up to date with the event stream by [`Threads::refresh`]
#[derive(Default)]
pub struct Threads {
    routers: Vec<String>,
    claude_routers: Vec<String>,
    /// The live file's first line when it was last read: another one there means the file was rotated
    head: String,
    /// How many bytes of the live file have been read
    at: u64,
    threads: HashMap<String, Vec<Entry>>,
    /// Messages of yours already seen typed in as a turn's opening prompt: the same words typed once more are yours again
    typed_in: std::collections::HashSet<String>,
}

fn named(routers: &[String], id: &str) -> bool {
    !id.is_empty() && routers.iter().any(|r| r == id)
}

impl Threads {
    pub fn new() -> Threads {
        Threads::default()
    }

    /// This router's thread, oldest first (empty for a session that isn't one of the routers last named)
    pub fn of(&self, router: &str) -> &[Entry] {
        self.threads.get(router).map_or(&[], Vec::as_slice)
    }

    /// Bring the threads up to date. Only what was appended to the event stream since the last call is read; everything is
    /// read again when other routers are named or the file was rotated
    pub fn refresh(&mut self, routers: &[String]) {
        self.refresh_from(routers, &paths::events_log());
    }

    fn refresh_from(&mut self, routers: &[String], path: &Path) {
        let mut want = routers.to_vec();
        want.sort();
        want.dedup();
        let len = std::fs::metadata(path).map_or(0, |m| m.len());
        let head = first_line(path);
        if want == self.routers && head == self.head && len == self.at {
            return;
        }
        // Only Claude routers have a shift, including when stopped. Resolve it once per router for this batch of events.
        let claude_routers: Vec<_> = want.iter().filter(|id| !crate::runtime::load(id).shift.is_empty()).cloned().collect();
        if want != self.routers || claude_routers != self.claude_routers || head != self.head || len < self.at {
            *self = Threads { routers: want, claude_routers, head, ..Threads::default() };
            if self.routers.is_empty() {
                return;
            }
            // What the stream held before it was last rotated comes first
            if let Ok(old) = std::fs::read(path.with_extension("jsonl.1")) {
                self.take(&String::from_utf8_lossy(&old));
            }
        }
        if self.routers.is_empty() || len <= self.at {
            return;
        }
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let mut buf = vec![];
        if f.seek(SeekFrom::Start(self.at)).is_err() || f.read_to_end(&mut buf).is_err() {
            return;
        }
        // A line still being written is left for the next call
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return };
        self.take(&String::from_utf8_lossy(&buf[..=end]));
        self.at += end as u64 + 1;
    }

    /// Event lines, oldest first. Only the ones that name a router are parsed, and deliveries (which name only the message)
    fn take(&mut self, text: &str) {
        for line in text.lines() {
            let named = self.routers.iter().any(|r| line.contains(r.as_str()));
            if !named && !line.contains("\"message.delivered\"") {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                self.fold(&v);
            }
        }
    }

    fn push(&mut self, router: &str, e: Entry) {
        let t = self.threads.entry(router.to_string()).or_default();
        t.push(e);
        if t.len() > KEEP {
            let over = t.len() - KEEP;
            t.drain(..over);
        }
    }

    /// One event → the entries it makes, in the threads of the routers it names
    fn fold(&mut self, v: &Value) {
        let s = |k: &str| v[k].as_str().unwrap_or("");
        let (session, ts) = (s("session").to_string(), v["ts"].as_u64().unwrap_or(0) / 1000);
        let routers = self.routers.clone();
        let is = |id: &str| named(&routers, id);
        let entry = |kind: Kind, who: &str, text: &str| Entry {
            ts,
            kind,
            who: who.to_string(),
            text: one_line(text),
            aux: String::new(),
            id: String::new(),
            pending: false,
            via: String::new(),
        };
        if let Some(readers) = v["routers"].as_array() {
            let worker = match s("event") {
                "turn.done" => Some((Kind::WorkerReply, s("last").trim().lines().next().unwrap_or(""))),
                "agent.waiting" => Some((Kind::WorkerAsks, s("ask"))),
                "turn.failed" => Some((Kind::WorkerFailed, s("error"))),
                "task.done" => Some((Kind::TaskDone, s("result"))),
                _ => None,
            };
            if let Some((kind, text)) = worker {
                for router in readers.iter().filter_map(Value::as_str).filter(|id| is(id)) {
                    if router == session && kind != Kind::TaskDone { continue; }
                    let mut e = entry(kind, &session, text);
                    if kind == Kind::TaskDone { e.aux = v["n"].to_string(); }
                    self.push(router, e);
                }
            }
        }
        match s("event") {
            "turn.started" if is(&session) => {
                let raw = s("prompt");
                let prompt = if named(&self.claude_routers, &session) { crate::agent::paste_text(raw, true).1 } else { raw.into() };
                // The same words as a message of yours just put in its inbox: that message, typed in for you (once)
                let words = same_words(&prompt);
                let echo = self
                    .of(&session)
                    .iter()
                    .rev()
                    .take(NEAR)
                    .find(|e| {
                        e.kind == Kind::You
                            && !e.id.is_empty()
                            && !e.text.is_empty()
                            && !self.typed_in.contains(&e.id)
                            && words.starts_with(&same_words(&e.text))
                    })
                    .map(|e| e.id.clone());
                match echo {
                    Some(id) => drop(self.typed_in.insert(id)),
                    None if prompt.trim().is_empty() || not_typed(&prompt) => {}
                    None => self.push(&session, entry(Kind::You, "", &prompt)),
                }
            }
            "turn.done" if is(&session) && !s("last").trim().is_empty() => self.push(&session, entry(Kind::Reply, "", s("last"))),
            "turn.failed" if is(&session) => self.push(&session, entry(Kind::Failed, "", s("error"))),
            "agent.waiting" if is(&session) => self.push(&session, entry(Kind::Asks, "", s("ask"))),
            "ask.answered" if is(&session) && s("by") == "you" => self.push(&session, entry(Kind::You, "", s("answer"))),
            // The completion event already put the result on this thread.
            "message.sent" if matches!(s("what"), "task.done" | "task.turn.done" | "task.agent.waiting" | "task.turn.failed") => {}
            "message.sent" => {
                let from = s("from").to_string();
                let msg = |kind: Kind, who: &str, text: &str, pending: bool| Entry { id: s("id").to_string(), pending, ..entry(kind, who, text) };
                // One it sent itself for later is its own timer, not part of the exchange
                if from == session {
                    return;
                }
                if is(&session) {
                    let e = match (from.as_str(), s("kind")) {
                        // Yours may have come in through a channel outside aca, which it names
                        ("you", _) => Entry { via: s("via").to_string(), ..msg(Kind::You, "", s("text"), true) },
                        (_, "notice") => msg(Kind::Notice, &from, s("what").strip_prefix("task.").unwrap_or(s("what")), false),
                        _ => msg(Kind::From, &from, s("text"), true),
                    };
                    self.push(&session, e);
                }
                if is(&from) {
                    self.push(&from, msg(Kind::To, &session, s("text"), true));
                }
            }
            "message.delivered" => {
                let id = s("id");
                for t in self.threads.values_mut() {
                    if let Some(e) = t.iter_mut().rev().find(|e| e.id == id) {
                        e.pending = false;
                    }
                }
            }
            "session.moved" if is(s("by")) => {
                let e = Entry { aux: s("group").to_string(), ..entry(Kind::Pulled, &session, s("from")) };
                self.push(s("by"), e);
            }
            // The router whose command made it; a line written before that was recorded names it as the parent
            "session.created" => {
                let maker = if s("by").is_empty() { s("parent") } else { s("by") };
                if is(maker) {
                    self.push(maker, entry(Kind::Started, &session, s("title")));
                }
            }
            _ => {}
        }
    }
}

/// The file's first line (at most its first 256 bytes), to tell a rotated file from the one read before
fn first_line(path: &Path) -> String {
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    let got = &buf[..n];
    String::from_utf8_lossy(&got[..got.iter().position(|&b| b == b'\n').unwrap_or(n)]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(ts: u64, event: &str, session: &str, data: Value) -> String {
        let mut v = json!({ "ts": ts * 1000, "event": event, "session": session });
        v.as_object_mut().unwrap().extend(data.as_object().unwrap().clone());
        v.to_string()
    }

    fn threads(routers: &[&str], lines: &[String]) -> Threads {
        let mut t = Threads { routers: routers.iter().map(|r| r.to_string()).collect(), ..Threads::default() };
        t.take(&lines.join("\n"));
        t
    }

    /// (kind, who, text) of a thread, pending ones marked with a trailing *
    fn shape(t: &Threads, router: &str) -> Vec<(Kind, String, String)> {
        t.of(router).iter().map(|e| (e.kind, e.who.clone(), format!("{}{}", e.text, if e.pending { "*" } else { "" }))).collect()
    }

    fn row(kind: Kind, who: &str, text: &str) -> (Kind, String, String) {
        (kind, who.to_string(), text.to_string())
    }

    #[test]
    fn task_results_follow_the_worker_and_are_shown_once_even_when_completion_rings() {
        let readers = json!(["project", "desk"]);
        let t = threads(&["project", "desk", "other"], &[
            line(1, "turn.done", "w1", json!({"routers":readers,"last":"结果第一行\n细节"})),
            line(2, "agent.waiting", "w1", json!({"routers":readers,"ask":"同意吗？"})),
            line(3, "turn.failed", "w1", json!({"routers":readers,"error":"超时"})),
            line(4, "task.done", "w1", json!({"routers":readers,"n":9,"result":"验证通过"})),
            line(5, "message.sent", "project", json!({"from":"w1","kind":"msg","what":"task.done","text":"#9 完成"})),
            line(6, "turn.done", "no-ticket", json!({"last":"project is in the text only"})),
        ]);
        for id in ["project", "desk"] {
            assert_eq!(shape(&t, id), [row(Kind::WorkerReply, "w1", "结果第一行"), row(Kind::WorkerAsks, "w1", "同意吗？"),
                row(Kind::WorkerFailed, "w1", "超时"), row(Kind::TaskDone, "w1", "验证通过")]);
            assert_eq!(t.of(id)[3].aux, "9");
        }
        assert!(t.of("other").is_empty());
    }

    #[test]
    fn what_you_said_through_a_channel_outside_aca_keeps_the_channels_name() {
        let sent = |ts, id: &str, from: &str, via: Value| {
            line(ts, "message.sent", "r0000001", json!({ "id": id, "from": from, "kind": "msg", "what": null, "wake": true, "text": "你好", "at": null, "via": via }))
        };
        let lines = vec![
            sent(1, "m1", "you", json!("Discord")),
            sent(2, "m2", "you", Value::Null),
            // What Claude passed along from another session says how it came, not where you said it
            sent(3, "m3", "aaaa0001", json!("claude")),
        ];
        let t = threads(&["r0000001"], &lines);
        let got: Vec<(Kind, &str)> = t.of("r0000001").iter().map(|e| (e.kind, e.via.as_str())).collect();
        assert_eq!(got, [(Kind::You, "Discord"), (Kind::You, ""), (Kind::From, "")]);
    }

    #[test]
    fn a_thread_is_what_was_said_to_the_router_what_it_said_and_what_it_did_in_order() {
        let sent = |ts, to: &str, id: &str, from: &str, text: &str| {
            line(ts, "message.sent", to, json!({ "id": id, "from": from, "kind": "msg", "what": null, "wake": true, "text": text, "at": null, "via": null }))
        };
        let lines = vec![
            // Not the router's: another session's turn, a message between two others
            line(1, "turn.started", "aaaa0001", json!({ "prompt": "别的会话的事" })),
            sent(2, "aaaa0001", "m0", "bbbb0002", "和 router 无关"),
            // You typed to it; it pulled two sessions over and answered
            line(10, "turn.started", "r0000001", json!({ "prompt": "随手问里查过 NUC 的电压，\n拉过来接着查" })),
            line(11, "session.moved", "aaaa0001", json!({ "group": "nuc", "from": "scratch", "by": "r0000001" })),
            line(12, "session.moved", "bbbb0002", json!({ "group": "nuc", "from": "scratch", "by": "r0000001" })),
            line(13, "turn.done", "r0000001", json!({ "last": "拉过来 2 个。", "bg": 0, "secs": 3 })),
            // A session moved by you, and one the router had nothing to do with: not in its thread
            line(14, "session.moved", "cccc0003", json!({ "group": "nuc", "from": "scratch", "by": null })),
            // It hands something to a session: delivered. Another one is still waiting to be
            sent(20, "aaaa0001", "m1", "r0000001", "kubeconfig 换 staging"),
            line(21, "message.delivered", "aaaa0001", json!({ "id": "m1", "how": "bell" })),
            sent(22, "bbbb0002", "m2", "r0000001", "放 shots/，周五要发"),
            // A session asks it something; it starts one; that one reports by itself
            sent(30, "r0000001", "m3", "bbbb0002", "截图放哪个目录？"),
            line(31, "message.delivered", "r0000001", json!({ "id": "m3", "how": "bell" })),
            line(32, "session.created", "dddd0004", json!({ "title": "面板", "tool": "claude", "account": "", "dir": "/p", "group": "nuc", "parent": "r0000001" })),
            line(
                33,
                "message.sent",
                "r0000001",
                json!({ "id": "m4", "from": "dddd0004", "kind": "notice", "what": "turn.done", "wake": true, "text": "最后一句:「好了」", "at": null, "via": null }),
            ),
            // One it starts as nobody's child is still one it started: the line says whose command made it. One that
            // another session's command put under it isn't
            line(34, "session.created", "eeee0005", json!({ "title": "告警", "tool": "claude", "account": "", "dir": "/p", "group": "nuc", "parent": "", "by": "r0000001" })),
            line(35, "session.created", "ffff0006", json!({ "title": "别人建的", "tool": "claude", "account": "", "dir": "/p", "group": "nuc", "parent": "r0000001", "by": "aaaa0001" })),
            // It asks you, you answer from the list; then a turn of its fails
            line(40, "agent.waiting", "r0000001", json!({ "ask": "先做哪个？", "kind": "choice", "options": ["面板", "告警"] })),
            line(41, "ask.answered", "r0000001", json!({ "by": "you", "answer": "1", "ask": "先做哪个？", "kind": "choice" })),
            line(42, "turn.failed", "r0000001", json!({ "error": "rate limit", "kind": "transient" })),
            // A turn that ended without a word leaves nothing
            line(43, "turn.done", "r0000001", json!({ "last": "  ", "bg": 0, "secs": 1 })),
        ];
        let t = threads(&["r0000001"], &lines);
        assert_eq!(
            shape(&t, "r0000001"),
            [
                row(Kind::You, "", "随手问里查过 NUC 的电压， 拉过来接着查"),
                row(Kind::Pulled, "aaaa0001", "scratch"),
                row(Kind::Pulled, "bbbb0002", "scratch"),
                row(Kind::Reply, "", "拉过来 2 个。"),
                row(Kind::To, "aaaa0001", "kubeconfig 换 staging"),
                row(Kind::To, "bbbb0002", "放 shots/，周五要发*"),
                row(Kind::From, "bbbb0002", "截图放哪个目录？"),
                row(Kind::Started, "dddd0004", "面板"),
                row(Kind::Notice, "dddd0004", "turn.done"),
                row(Kind::Started, "eeee0005", "告警"),
                row(Kind::Asks, "", "先做哪个？"),
                row(Kind::You, "", "1"),
                row(Kind::Failed, "", "rate limit"),
            ]
        );
        assert_eq!((t.of("r0000001")[0].ts, t.of("r0000001")[1].aux.as_str()), (10, "nuc"));
        // A session that isn't a router has no thread
        assert!(t.of("aaaa0001").is_empty());
    }

    #[test]
    fn what_opens_a_turn_counts_as_yours_only_when_you_typed_it() {
        let sent = |ts, id: &str, from: &str, text: &str| {
            line(ts, "message.sent", "r0000001", json!({ "id": id, "from": from, "kind": "msg", "what": null, "wake": true, "text": text, "at": null, "via": null }))
        };
        let started = |ts, prompt: &str| line(ts, "turn.started", "r0000001", json!({ "prompt": prompt }));
        let long = "磁盘用到 90% 也告警，".repeat(40);
        let lines = vec![
            // Your message from outside, typed into its box by aca: one entry, not two — also when the events cut the two
            // at different lengths
            sent(1, "m1", "you", "面板这周要给人看"),
            started(2, "面板这周要给人看"),
            sent(3, "m2", "you", &clip(&long, 200)),
            started(4, &clip(&long, 400)),
            // A session's message typed in by aca, a background task's notice, another Claude session's own message
            sent(5, "m3", "bbbb0002", "截图放哪个目录？"),
            started(6, "[aca] 来自会话 bbbb0002「面板」claude:\n截图放哪个目录？"),
            started(7, "<task-notification>\n<task-id>x</task-id>"),
            started(8, "<cross-session-message from=\"x\">你好</cross-session-message>"),
            started(9, "   "),
            // What you type yourself, pasted things included
            started(10, "[Image #1] 看这张图"),
            started(11, "面板这周要给人看"),
            // A message it sends itself for later is its own timer
            line(12, "message.sent", "r0000001", json!({ "id": "m9", "from": "r0000001", "kind": "msg", "what": null, "wake": true, "text": "报一次进度", "at": 99, "via": null })),
        ];
        let t = threads(&["r0000001"], &lines);
        let got = shape(&t, "r0000001");
        assert_eq!(got.len(), 5, "{got:?}");
        assert_eq!(got[0], row(Kind::You, "", "面板这周要给人看*"));
        assert!(got[1].0 == Kind::You && got[1].2.starts_with("磁盘用到 90% 也告警") && got[1].2.ends_with('*'));
        assert_eq!(got[2], row(Kind::From, "bbbb0002", "截图放哪个目录？*"));
        assert_eq!(got[3], row(Kind::You, "", "[Image #1] 看这张图"));
        // The same words typed again after the message was typed in for you: yours
        assert_eq!(got[4], row(Kind::You, "", "面板这周要给人看"));
        // Said again long after: the earlier message is too far back to be the same one
        let far: Vec<String> = (0..NEAR as u64).map(|i| line(20 + i, "turn.done", "r0000001", json!({ "last": "好", "bg": 0, "secs": 1 }))).collect();
        let t = threads(&["r0000001"], &[vec![sent(1, "m1", "you", "面板这周要给人看")], far, vec![started(90, "面板这周要给人看")]].concat());
        assert_eq!(t.of("r0000001").last().map(|e| (e.kind, e.ts)), Some((Kind::You, 90)));
    }

    #[test]
    fn wrapped_claude_turns_omit_handoffs_and_keep_the_users_words_once() {
        let mut t = Threads { routers: vec!["r1".into(), "r2".into()], claude_routers: vec!["r1".into()], ..Default::default() };
        let wrapped = |body: &str| format!("\n\n<pasted_content id=\"c90b\">\n{body}\n</pasted_content id=\"c90b\">\n");
        let limit = crate::agent::lifecycle::PROMPT_CHARS;
        let started = |ts, router, prompt: &str| line(ts, "turn.started", router, json!({"prompt":clip(prompt, limit)}));
        let long = "我亲自贴的长话".repeat(90);
        let incoming = r#"请看 <pasted_content id="abcd">a<\b</pasted_content>"#;
        let paste = wrapped(r#"请看 <\pasted_content id="abcd">a<\b<\/pasted_content>"#);
        let short = wrapped("[aca] 项目交接 · probe");
        // Also exercise a cut inside the closing tag and a cut immediately after it.
        let at_close = "[aca] ".to_owned() + &" ".repeat(limit + 1 - wrapped("[aca] ").len());
        let cut_close = at_close.replacen("[aca] ", "words ", 1) + "1234567";
        let lines = [
            started(1, "r1", &short),
            started(2, "r1", &wrapped(&format!("[aca] 项目交接 · probe\n{long}"))),
            started(3, "r1", &wrapped(&long)),
            line(4, "message.sent", "r1", json!({"id":"m1","from":"you","kind":"msg","text":incoming,"via":"Discord"})),
            started(5, "r1", &paste),
            started(6, "r1", &paste),
            started(7, "r2", &short),
            started(8, "r1", &wrapped(&at_close)),
            started(9, "r1", &wrapped(&cut_close)),
        ];
        t.take(&lines.join("\n"));
        assert_eq!(
            shape(&t, "r1"),
            [
                row(Kind::You, "", &clip(&long, TEXT)),
                row(Kind::You, "", &format!("{incoming}*")),
                row(Kind::You, "", incoming),
                row(Kind::You, "", "words 1234567"),
            ]
        );
        assert_eq!(t.of("r1")[1].via, "Discord");
        assert_eq!(shape(&t, "r2"), [row(Kind::You, "", &one_line(&short))]);
    }

    #[test]
    fn two_routers_talking_are_in_each_others_threads_and_a_thread_keeps_its_latest_entries() {
        let sent = |ts, to: &str, id: &str, from: &str| {
            line(ts, "message.sent", to, json!({ "id": id, "from": from, "kind": "msg", "what": null, "wake": true, "text": "交给你", "at": null, "via": null }))
        };
        let mut lines = vec![sent(1, "r0000002", "m1", "r0000001"), line(2, "message.delivered", "r0000002", json!({ "id": "m1", "how": "bell" }))];
        let t = threads(&["r0000001", "r0000002"], &lines);
        assert_eq!(shape(&t, "r0000001"), [row(Kind::To, "r0000002", "交给你")]);
        assert_eq!(shape(&t, "r0000002"), [row(Kind::From, "r0000001", "交给你")]);
        lines.extend((0..KEEP as u64 + 5).map(|i| line(10 + i, "turn.done", "r0000001", json!({ "last": format!("第 {i} 句"), "bg": 0, "secs": 1 }))));
        let t = threads(&["r0000001", "r0000002"], &lines);
        // The message and the first five replies went: the latest KEEP stay
        assert_eq!(t.of("r0000001").len(), KEEP);
        assert_eq!(t.of("r0000001")[0].text, "第 5 句");
    }

    #[test]
    fn only_what_was_appended_is_read_and_a_rotated_stream_is_read_again_from_the_file_before_it() {
        let dir = std::env::temp_dir().join(format!("aca-thread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        let done = |ts: u64, last: &str| format!("{}\n", line(ts, "turn.done", "r0000001", json!({ "last": last, "bg": 0, "secs": 1 })));
        let append = |text: &str| {
            use std::io::Write;
            std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap().write_all(text.as_bytes()).unwrap();
        };
        let routers = ["r0000001".to_string()];
        let mut t = Threads::new();
        let said = |t: &Threads| t.of("r0000001").iter().map(|e| e.text.clone()).collect::<Vec<_>>();
        // No file yet, then one line, then one more: each call adds only the new one
        t.refresh_from(&routers, &path);
        assert!(said(&t).is_empty());
        append(&done(1, "一"));
        t.refresh_from(&routers, &path);
        assert_eq!(said(&t), ["一"]);
        let read = t.at;
        append(&done(2, "二"));
        // Half a line written: left alone until the rest is there
        let half = done(3, "三");
        append(&half[..20]);
        t.refresh_from(&routers, &path);
        assert_eq!(said(&t), ["一", "二"]);
        assert!(t.at > read);
        append(&half[20..]);
        t.refresh_from(&routers, &path);
        assert_eq!(said(&t), ["一", "二", "三"]);
        // Nothing new: nothing changes
        t.refresh_from(&routers, &path);
        assert_eq!(said(&t).len(), 3);
        // Rotated: the old file moves aside and a new one starts. Nothing is lost and nothing comes twice
        std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();
        append(&done(4, "四"));
        t.refresh_from(&routers, &path);
        assert_eq!(said(&t), ["一", "二", "三", "四"]);
        // Another router named: its thread is read from the start, the first one's kept
        append(&format!("{}\n", line(5, "turn.done", "r0000002", json!({ "last": "另一个", "bg": 0, "secs": 1 }))));
        t.refresh_from(&routers, &path);
        assert!(t.of("r0000002").is_empty());
        t.refresh_from(&["r0000002".to_string(), "r0000001".to_string()], &path);
        assert_eq!((said(&t).len(), t.of("r0000002").len()), (4, 1));
        // Nobody named: nothing kept, nothing read
        t.refresh_from(&[], &path);
        assert!(t.of("r0000001").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
