//! A session's runtime state: `~/.local/state/aca/run/<id>.json`. Hooks change it on every event; the list and the CLI read it.
//!
//! One file per session, not in the registry: a hook writing it doesn't wait for other sessions, and isn't held up by the
//! interface querying the database. Changing = take this session's lock → read → change → write back atomically, so two events
//! of the same session arriving back to back don't lose one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::paths;

/// A short piece of text a plugin hangs on the session row (`aca meta set`), picked by name in the list row
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Token {
    pub value: String,
    /// No longer shown after this moment; 0 = kept forever
    #[serde(default, skip_serializing_if = "is_zero")]
    pub until: u64,
}

/// How a hung piece of text is written on the row: "[pr #12]"; a name starting with _ writes only the value, for icons, numbers
/// and the like that read at a glance ("▣2")
pub fn mark(name: &str, value: &str) -> String {
    if name.starts_with('_') { value.into() } else { format!("[{name} {value}]") }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The user's opening words for a router turn, and the sessions that have already received them.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UserWords {
    pub text: String,
    pub chars: usize,
    pub via: String,
    pub ts: u64,
    pub sent_to: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Run {
    /// The state the hook reports: running / waiting / done / idle / error / ended; empty = no event received yet
    pub status: String,
    /// The last event and its time
    pub event: String,
    pub ts: u64,
    /// The id of the current conversation (changes after /clear or compaction), and every one it has had (billing uses them to
    /// assign usage to the session)
    pub conv: String,
    pub convs: Vec<String>,
    pub transcript: String,
    pub cwd: String,
    /// The question it asks when it needs you
    pub ask: String,
    /// What kind of question: choice (multiple choice) / permission (whether to let it run) / empty = can't tell (recognized only on
    /// the screen, the gates at startup)
    pub ask_kind: String,
    /// The options of a multiple-choice question (only the first question's); `ask_multi` = several questions at once, or
    /// multi-select: can't be answered with one number
    pub ask_opts: Vec<String>,
    pub ask_multi: bool,
    /// What you said this turn (truncated). A Claude turn opened by another session's message or a background task's notice
    /// gets what you last said yourself back when the turn ends ([`crate::agent::Agent::turn_record`])
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_words: Option<UserWords>,
    /// External channel that opened this turn. A later message in an active turn does not redirect its reply.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub turn_via: String,
    /// The channel at inbox take, restored when Claude submits the corresponding doorbell notification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bell_via: Option<String>,
    /// The next input pasted by aca (or its opening handoff), matched once by its fingerprint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router_input: Option<(u64, Option<UserWords>)>,
    /// Mail included in the handoff remains pending until UserPromptSubmit confirms its fingerprint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opening_mail: Option<Vec<String>>,
    /// This shift's MCP server has flushed its router tools to the client.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub tools_listed: bool,
    /// Initial conversation id for this router process, also passed to its hooks to discard late hooks from the old shift.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub shift: String,
    /// The id of the prompt that opened this turn (Claude's `prompt_id`, Codex's `turn_id`), also found in its record
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt_id: String,
    /// The four agy title fields that drive lifecycle events. Other title fields change on almost every redraw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agy: Option<AgyTitle>,
    /// Last recorded title quota, excluding the countdown that changes on every redraw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agy_quota: Option<u64>,
    /// Successive agy permission boxes can appear within the same second and in the same turn.
    #[serde(skip_serializing_if = "is_zero")]
    pub agy_confirmation: u64,
    /// A new title update may arrive while the previous command box is still visible.
    #[serde(skip_serializing_if = "is_zero")]
    pub agy_previous_command: u64,
    /// A Codex submit hook can finish before the relay processes its first working report for the same turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_submit_pid: Option<u32>,
    /// The last reply (truncated): `c` copy, notices and plugins all take it from here, without digging through the conversation
    /// record again
    pub last: String,
    /// Background tasks still running when this turn ended
    pub bg: u32,
    /// When this turn started / last finished
    pub turn: u64,
    pub done: u64,
    /// When it was last looked at: finishing after this counts as unread
    pub seen: u64,
    /// The title the agent set itself; `locked` = renamed by a person, no longer followed
    pub auto_title: String,
    /// The title reported in the hook (the protocol's `title`). Whether it's reported, and whether `auto_title` follows it, depends on
    /// each tool's `Agent::title`
    pub hook_title: String,
    pub locked: bool,
    /// The name handed to Claude by `aca hook-name`, or the registered fallback for agy's title command.
    pub named: String,
    /// Claude's inbox address for messages from other Claude sessions (`uds:` + this socket), as Claude exports it to hooks
    /// (`CLAUDE_CODE_MESSAGING_SOCKET`); a new one each start. Empty for others, and for Claude before 2.1.224
    #[serde(skip_serializing_if = "String::is_empty")]
    pub peer: String,
    /// Up to when the messages other sessions passed it through the agent's own channel have been taken into its inbox (see
    /// [`crate::inbox::native`]); 0 = not looked yet, start from the current turn
    #[serde(skip_serializing_if = "is_zero")]
    pub native_seen: u64,
    pub error: String,
    /// What kind of failure this turn was: transient (just carry on in a while) / account (limits, login: needs a person) / fatal
    /// (see [`crate::agent::lifecycle::error_kind`])
    pub error_kind: String,
    /// A forked session: which conversation it forked from. One stopped before saying anything after the fork has no conversation
    /// record of its own, and has to be forked again when started
    pub fork_from: String,
    /// The doorbell is connected: on this start, SessionStart had it watch its own doorbell file (`aca inbox watch`). Lowered at
    /// every start, raised once connected
    pub bell: bool,
    /// Which events to tell the parent about (some of done,failed,exited,waiting, comma-separated; none = no events); empty = as
    /// configured
    pub notify: String,
    pub tokens: BTreeMap<String, Token>,
    /// What plugins gave it on this start (see `plugin::mount::signature`): compared with the current one, a difference means a
    /// restart is needed to pick it up
    pub mounts: String,
    /// When the agent was started this time (`session::launch`): for a short while after starting, the startup gates get answered
    /// for you (see [`crate::trust`])
    #[serde(skip_serializing_if = "is_zero")]
    pub started: u64,
    /// Which conversation record it was at the moment of starting, and how long it already was (`session::launch`): schedules set
    /// this time are only written after that, so finding schedules only needs to read on from there ([`crate::schedule::due`]). A
    /// new conversation from /clear is a new file, read from the start as usual
    #[serde(skip_serializing_if = "String::is_empty")]
    pub started_file: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub started_len: u64,
    /// When aca stopped it for sitting idle ([`crate::session::stop_idle`]): the list draws ☾, apart from ■ for one a person stopped.
    /// Cleared when it's started again or a person stops it again
    #[serde(skip_serializing_if = "is_zero")]
    pub idle_stopped: u64,
    /// When it was brought back from the trash / unarchived ([`crate::session::archive`]): also counts for how long a stopped one has
    /// been untouched, so one brought back isn't auto-deleted again on the next round (`[idle] remove_after`)
    #[serde(skip_serializing_if = "is_zero")]
    pub restored: u64,
    /// When compacting the conversation started (Claude's PreCompact; manual /compact, a full context, and the compaction it does
    /// itself after nearly an hour idle are all reported), cleared once done. With compaction done on its own while idle, the hook
    /// state stays "done" the whole time; only this tells it's compacting, so it isn't stopped then ([`crate::session::stop_idle`])
    #[serde(skip_serializing_if = "is_zero")]
    pub compacting: u64,
    /// Account changes it carried on through ([`crate::session::rehome`]), one row each. The turns before a change are in both account
    /// directories; billing uses this to assign them back to the original one
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub moves: Vec<Move>,
    /// What the agent last reported about itself over the program status protocol, relayed by `aca pty` ([`crate::relay`]); None =
    /// nothing ever reported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ps: Option<Program>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct AgyTitle {
    pub agent_state: String,
    #[serde(default)]
    pub conversation_id: String,
    pub conversation_title: Option<String>,
    pub tool_confirmation_pending: Option<bool>,
}

/// The agent's own report of its state (OSC 7501's root record)
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Program {
    /// The relay that got it (it's the pane's root process): a report counts only while that process is still the one in the pane,
    /// so one left from an earlier start is never taken for the current one
    pub pid: u32,
    /// idle / working / done / blocked / error; empty = it cleared its records (state unknown)
    pub state: String,
    /// With blocked: permission / question / auth, or empty
    pub kind: String,
    /// One line on what it's doing or waiting for
    pub msg: String,
    pub at: u64,
    /// The turn witnessed by this local Codex relay. A startup prompt or a bare done has no completed turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_turn: Option<CodexTurn>,
    /// A late hook from the preceding turn must not supply the reply for an OSC-only submit.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub previous_turn_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexTurn {
    Ready,
    Working,
    Done,
}

/// One account change: when, which conversation, which configuration directory it was copied from and to
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Move {
    pub at: u64,
    pub conv: String,
    pub from: String,
    pub to: String,
}

impl Run {
    pub(crate) fn current_shift(&self) -> bool {
        self.shift.is_empty() || std::env::var("ACA_ROUTER_SHIFT").map_or(true, |generation| generation == self.shift)
    }

    pub fn codex_turn(&self, pane_pid: u32) -> Option<CodexTurn> {
        self.ps.as_ref().filter(|p| pane_pid != 0 && p.pid == pane_pid)?.codex_turn
    }

    /// The last time the agent really did something (0 if the hook never reported an event): this turn starting / finishing, aca
    /// starting it, starting to ask you. An idle Claude also redraws and adds a few lines to the conversation record when settings
    /// change or the window resizes, so the window's activity time and the record's modified time both become "just now"; the
    /// reminder after a while idle (Notification) doesn't count either. How long the list says it's been untouched, and auto-stop,
    /// both go by this
    pub fn hook_at(&self) -> u64 {
        if self.ts == 0 {
            return 0;
        }
        let asked = if self.status == "waiting" { self.ts } else { 0 };
        [self.turn, self.done, self.started, asked].into_iter().max().unwrap_or(0)
    }

    /// The ones not expired yet
    pub fn live_tokens(&self, now: u64) -> BTreeMap<String, String> {
        self.tokens.iter().filter(|(_, t)| t.until == 0 || t.until > now).map(|(k, t)| (k.clone(), t.value.clone())).collect()
    }

    /// No longer needs you: clear the question and the options together
    pub fn clear_ask(&mut self) {
        self.ask.clear();
        self.ask_kind.clear();
        self.ask_opts.clear();
        self.ask_multi = false;
    }

    /// No longer an error: clear the text and the kind together
    pub fn clear_error(&mut self) {
        self.error.clear();
        self.error_kind.clear();
    }

    /// The few words shown to people for the error kind (empty when there's no kind)
    pub fn error_kind_said(&self) -> &'static str {
        match self.error_kind.as_str() {
            "transient" => crate::t!(
                "临时错误:限流、过载或连接中断,稍后让它继续通常即可恢复",
                "transient: rate limit, overload or a dropped stream; letting it carry on in a while usually works"
            ),
            "account" => crate::t!("账号问题:额度、登录或欠费,需要人工处理", "account: quota, login or billing; needs a person"),
            "fatal" => {
                crate::t!("重试无效:上下文、请求或内容策略问题", "fatal: context, request or content policy; retrying won't help")
            }
            _ => "",
        }
    }

    pub fn note_conv(&mut self, conv: &str) {
        if conv.is_empty() {
            return;
        }
        if !self.convs.iter().any(|c| c == conv) {
            self.convs.push(conv.to_string());
        }
        self.conv = conv.to_string();
    }
}

pub fn load(id: &str) -> Run {
    std::fs::read_to_string(paths::run_file(id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Take the lock, read, change, write back; returns (before the change, after the change)
pub fn update(id: &str, f: impl FnOnce(&mut Run)) -> (Run, Run) {
    let path = paths::run_file(id);
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    // Lock a separate file that stays put: the state file itself is replaced by a new one every time
    let lock = std::fs::OpenOptions::new().create(true).append(true).open(path.with_extension("lock"));
    if let Ok(l) = &lock {
        let _ = l.lock();
    }
    let before = load(id);
    let mut after = before.clone();
    f(&mut after);
    if after != before {
        let _ = paths::write_atomic(&path, &serde_json::to_string(&after).unwrap_or_default());
    }
    (before, after)
}

/// Deletes the session's state file. Its account switches are kept first (see [`kept_moves`])
pub fn remove(id: &str) {
    let path = paths::run_file(id);
    append_moves(&moves_log(), &load(id).moves);
    let _ = std::fs::remove_file(path.with_extension("lock"));
    let _ = std::fs::remove_file(path);
}

/// The account switches of deleted sessions, one JSON line each. Their conversations' turns before a switch stay in both
/// account directories after the session is gone, and billing still needs to know which account ran them
fn moves_log() -> std::path::PathBuf {
    paths::state_dir().join("moves.jsonl")
}

fn append_moves(log: &std::path::Path, moves: &[Move]) {
    use std::io::Write;
    if moves.is_empty() {
        return;
    }
    let text: String = moves.iter().filter_map(|m| serde_json::to_string(m).ok()).map(|l| l + "\n").collect();
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(std::path::Path::new(".")));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// The account switches kept from deleted sessions ([`remove`]); lines that don't parse are skipped
pub fn kept_moves() -> Vec<Move> {
    read_moves(&moves_log())
}

fn read_moves(log: &std::path::Path) -> Vec<Move> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Keep the first n characters
pub fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_switches_outlive_the_session() {
        let dir = std::env::temp_dir().join(format!("aca-moves-{}", paths::short_id()));
        let log = dir.join("moves.jsonl");
        let m = |at: u64, conv: &str| Move { at, conv: conv.into(), from: "/ca".into(), to: "/cb".into() };
        assert!(read_moves(&log).is_empty());
        append_moves(&log, &[m(1, "c1"), m(2, "c1")]);
        // A session that never switched adds nothing
        append_moves(&log, &[]);
        append_moves(&log, &[m(3, "c2")]);
        assert_eq!(read_moves(&log), vec![m(1, "c1"), m(2, "c1"), m(3, "c2")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_conversation_is_remembered_once_and_becomes_current() {
        let mut r = Run::default();
        r.note_conv("a");
        r.note_conv("b");
        r.note_conv("a");
        r.note_conv("");
        assert_eq!((r.conv.as_str(), r.convs.len()), ("a", 2));
    }

    #[test]
    fn expired_tokens_are_not_shown() {
        let mut r = Run::default();
        r.tokens.insert("pr".into(), Token { value: "#12".into(), until: 0 });
        r.tokens.insert("ci".into(), Token { value: "红".into(), until: 100 });
        assert_eq!(r.live_tokens(99).len(), 2);
        assert_eq!(r.live_tokens(100).keys().collect::<Vec<_>>(), ["pr"]);
    }

    #[test]
    fn a_name_starting_with_underscore_shows_only_the_value() {
        assert_eq!((mark("pr", "#12").as_str(), mark("_float", "▣2").as_str()), ("[pr #12]", "▣2"));
    }

    #[test]
    fn clipping_counts_characters_not_bytes() {
        assert_eq!(clip("修登录bug", 3), "修登录…");
        assert_eq!(clip("ok", 3), "ok");
    }

    #[test]
    fn an_old_file_without_newer_fields_still_reads() {
        let r: Run = serde_json::from_str(r#"{"status":"done","conv":"x"}"#).unwrap();
        assert_eq!((r.status.as_str(), r.seen, r.tokens.len()), ("done", 0, 0));
    }
}
