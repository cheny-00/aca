//! Antigravity CLI 1.5.16 (macOS). A command in the per-launch settings receives JSON on
//! stdin whenever any title field changes. Its stdout becomes the pane title. A new conversation
//! gets its ID after startup; a resumed one does not emit SessionStart. Both denial and Esc
//! return to idle without a Stop hook. Only the transcript in antigravity-cli/brain has the reply.
//! Hooks and MCP load from an added workspace, but agy sorts workspace paths and runs commands
//! in the first one. Adding aca's private directory can change the project commands run in.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Agent, Events, How, Live, lifecycle};
use crate::config::Tool;
use crate::paths::{self, Res};
use crate::runtime::{AgyTitle, Run};
use crate::screen::Seen;
use crate::store::Session;

mod readonly;

pub struct Agy;

/// Subscription windows from a title redraw, independent of its lifecycle fields.
pub struct Limits {
    pub plan: String,
    pub windows: Vec<(&'static str, f64, i64)>,
}

impl Limits {
    pub fn from_title(v: &Value) -> Option<Self> {
        let quota = v.get("quota")?.as_object()?;
        let mut windows = vec![];
        for (key, label) in [("gemini-5h", "5h"), ("gemini-weekly", "7d"), ("3p-5h", "3p 5h"), ("3p-weekly", "3p 7d")] {
            let Some(w) = quota.get(key) else { continue };
            let reset = w["reset_time"].as_str()?;
            if key.starts_with("3p-") && reset.starts_with("1970-") {
                continue;
            }
            let remaining = w["remaining_fraction"].as_f64().filter(|n| (0.0..=1.0).contains(n))?;
            windows.push((label, (1.0 - remaining) * 100.0, paths::iso(reset)? as i64));
        }
        (!windows.is_empty()).then(|| Self { plan: v["plan_tier"].as_str().unwrap_or_default().into(), windows })
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hash = DefaultHasher::new();
        self.plan.hash(&mut hash);
        for (label, used, reset) in &self.windows {
            (label, used.to_bits(), reset).hash(&mut hash);
        }
        hash.finish()
    }
}

fn transcript(home: &Path, id: &str) -> Option<PathBuf> {
    (Path::new(id).file_name()?.to_str()? == id)
        .then(|| home.join(".gemini/antigravity-cli/brain").join(id).join(".system_generated/logs/transcript_full.jsonl"))
}

fn title_in(home: &Path, r: &Run) -> Option<String> {
    if let Some(title) = Agy.live_title(r, "") {
        return Some(title);
    }
    let id = r.agy.as_ref().map(|s| &s.conversation_id).filter(|id| !id.is_empty()).unwrap_or(&r.conv);
    if id.is_empty() {
        return None;
    }
    readonly::title(&home.join(".gemini/antigravity-cli/conversation_summaries.db"), id)
}

/// Stop at the newest user input even if this turn has no assistant text yet.
fn last_turn(path: &Path) -> Option<(String, Option<String>)> {
    let len = path.metadata().ok()?.len();
    let mut bytes = 64 * 1024;
    loop {
        let tail = super::tail(path.to_str()?, bytes)?;
        let mut reply = None;
        for line in tail.lines().rev() {
            let Ok(step) = serde_json::from_str::<Value>(line) else { continue };
            let content = step["content"].as_str().unwrap_or("");
            match step["type"].as_str() {
                Some("USER_INPUT") => {
                    let prompt = content.split_once("<USER_REQUEST>").and_then(|(_, t)| t.split_once("</USER_REQUEST>"));
                    return Some((prompt.map_or("", |(p, _)| p.trim()).into(), reply));
                }
                Some("PLANNER_RESPONSE") if reply.is_none() && !content.trim().is_empty() => reply = Some(content.to_string()),
                _ => {}
            }
        }
        if bytes >= len {
            return None;
        }
        bytes = bytes.saturating_mul(2).min(len);
    }
}

impl AgyTitle {
    pub(crate) fn pane_title(&self, r: &Run) -> String {
        let title = if r.locked {
            &r.named
        } else if let Some(title) = self.conversation_title.as_deref().filter(|s| !s.trim().is_empty()) {
            title
        } else if !r.auto_title.is_empty() {
            &r.auto_title
        } else {
            &r.named
        };
        title.chars().filter(|c| !c.is_control()).collect()
    }

    /// Keep each translated payload alongside its events: the hook runner also needs it for inbox delivery.
    pub(crate) fn apply(self, r: &mut Run, home: &Path, now: u64) -> Vec<(Value, Events)> {
        self.hooks(r, home)
            .into_iter()
            .map(|v| {
                let events = lifecycle::apply(r, &v, now);
                (v, events)
            })
            .collect()
    }

    fn hooks(self, r: &mut Run, home: &Path) -> Vec<Value> {
        let mut hooks = Vec::new();
        if r.agy.as_ref() == Some(&self) {
            return hooks;
        }
        let prev = r.agy.take().unwrap_or_default();
        let id = &self.conversation_id;
        let path = transcript(home, id);
        let turn = std::cell::LazyCell::new(|| path.as_deref().and_then(last_turn));
        let running = |s: &str| matches!(s, "working" | "tool_use" | "reviewing");
        let changed = !id.is_empty() && id != &prev.conversation_id;
        let same_turn = running(&prev.agent_state) && !(changed && !prev.conversation_id.is_empty());
        // The first working redraw can precede both the ID and the USER_INPUT write.
        if same_turn
            && r.prompt.is_empty()
            && let Some((prompt, _)) = turn.as_ref()
        {
            r.prompt = crate::runtime::clip(prompt, 400);
        }
        if changed {
            let mut start = json!({"hook_event_name":"SessionStart", "session_id":id, "transcript_path":path});
            // The first ID can arrive after the first working redraw. Keep that turn open.
            if same_turn {
                start["source"] = json!("compact");
            }
            hooks.push(start);
        }
        let state = self.agent_state.as_str();
        if matches!(state, "working" | "tool_use") && !same_turn {
            r.agy_previous_command = 0;
            hooks.push(json!({"hook_event_name":"UserPromptSubmit", "prompt":turn.as_ref().map(|t| t.0.as_str()).unwrap_or_default()}));
        }
        if self.tool_confirmation_pending == Some(true) && (prev.tool_confirmation_pending != Some(true) || !same_turn) {
            r.agy_confirmation += 1;
            hooks.push(json!({"hook_event_name":"PermissionRequest"}));
        } else if state == "working" && (prev.tool_confirmation_pending == Some(true) || prev.agent_state == "reviewing") {
            hooks.push(json!({"hook_event_name":"PostToolUse"}));
        }
        if state != prev.agent_state {
            match state {
                // These states were documented but absent from the 1.5.16 captures; infer no reason or keys.
                "reviewing" => hooks.push(json!({"hook_event_name":"Notification", "notification_type":"elicitation_dialog"})),
                "error" => hooks.push(json!({"hook_event_name":"StopFailure"})),
                "idle" if same_turn && r.status != "idle" => {
                    hooks.push(match turn.as_ref().and_then(|t| t.1.as_ref()) {
                        Some(reply) => json!({"hook_event_name":"Stop", "last_assistant_message":reply}),
                        None => json!({"hook_event_name":"Interrupt"}),
                    });
                }
                _ => {}
            }
        }
        if self.conversation_title != prev.conversation_title
            && let Some(title) = &self.conversation_title
        {
            if hooks.is_empty() {
                hooks.push(json!({"hook_event_name":"Notification"}));
            }
            hooks[0]["title"] = json!(title);
        }
        r.agy = Some(self);
        hooks
    }
}

const RESUME: &[&str] = &["--conversation", "-r", "--resume"];
const VALUED: &[&str] = &[
    "--add-dir",
    "--agent",
    "--effort",
    "--log-file",
    "--mode",
    "--model",
    "--project",
    "--settings",
    "--json-schema",
    "--print-timeout",
    "--input-format",
    "--output-format",
];

impl Agent for Agy {
    fn name(&self) -> &str {
        "agy"
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>> {
        let mut argv = super::base(tool, self.name());
        if let How::Resume(id) = how {
            argv.extend(["--conversation".into(), id.clone()]);
        }
        let mut extra = super::extra(tool, s);
        let mut settings = serde_json::Map::new();
        for value in super::settings_args(&mut extra) {
            settings.extend(super::settings_object(&value, &s.dir)?);
        }
        if extra.iter().any(|a| a == "--settings") {
            return Err(crate::t!("--settings 后面需要一个值", "--settings needs a value").into());
        }
        settings.insert("title".into(), json!({"type":"command", "command":format!("{} hook-title agy", super::sh_quote(aca))}));
        argv.extend(["--settings".into(), Value::Object(settings).to_string()]);
        argv.extend(extra);
        Ok(argv)
    }

    fn resume(&self, conv: &str, _: &Run) -> How {
        if conv.is_empty() { How::Fresh } else { How::Resume(conv.into()) }
    }

    fn on_hook(&self, r: &mut Run, v: &Value, now: u64) -> Events {
        if v.get("hook_event_name").is_some() {
            return lifecycle::apply(r, v, now);
        }
        let Ok(title) = serde_json::from_value::<AgyTitle>(v.clone()) else { return vec![] };
        title.apply(r, &paths::home(), now).into_iter().flat_map(|(_, events)| events).collect()
    }

    fn title(&self, r: &Run, _: &str) -> Option<String> {
        title_in(&paths::home(), r)
    }

    fn live_title(&self, r: &Run, _: &str) -> Option<String> {
        (!r.hook_title.is_empty()).then(|| r.hook_title.clone())
    }

    fn opening(&self, text: &str) -> Option<Vec<String>> {
        // Flag-like and multiline arguments are unverified; use the normal paste path for those.
        (!text.starts_with('-') && !text.contains(['\n', '\r'])).then(|| vec!["-i".into()])
    }

    fn trust_keys(&self, lines: &[String]) -> Option<Vec<&'static str>> {
        let tail: Vec<_> = lines.iter().rev().map(|l| l.trim()).filter(|l| !l.is_empty()).take(14).collect();
        let selected = tail.iter().position(|l| *l == "> Yes, I trust this folder")?;
        tail[selected + 1..].contains(&"Do you trust the contents of this project?").then(|| vec!["Enter"])
    }

    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        Some(("1", "4"))
    }

    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["Escape"])
    }

    fn screen(&self, lines: &[String]) -> Seen {
        let rows: Vec<_> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        // Only the command dialog's options have been observed. File and plan dialogs may differ.
        if let Some(end) = rows.iter().rposition(|l| *l == "Run this command?")
            && let Some(start) = rows[..end].iter().rposition(|l| *l == "Requesting permission for:")
            && rows.get(end + 1).is_some_and(|l| l.trim_start_matches("> ") == "1. Yes, run command")
            && rows[end + 1..].iter().any(|l| l.trim_start_matches("> ") == "4. No, cancel")
            && rows.iter().rev().take(2).any(|l| l.starts_with("↑/↓ Navigate"))
        {
            return Seen::Ask(rows[start + 1..end].join("\n"));
        }
        if rows.iter().rev().take(7).any(|l| l.trim_start_matches('⎿').trim() == "Interrupted · What should Antigravity CLI do instead?")
            && !rows.iter().rev().take(2).any(|l| l.contains("esc to cancel"))
        {
            return Seen::Quiet;
        }
        Seen::Unsure
    }

    fn live(&self, _: u32, _: Option<&str>, argv: &[String], _: &dyn Fn(&str) -> bool) -> Live {
        let mut conv = String::new();
        let args = super::carry_valued(argv, VALUED, |rest| {
            if matches!(rest[0].as_str(), "-c" | "--continue") {
                return 1;
            }
            let n = super::optional(rest, RESUME);
            match n {
                2 => conv.clone_from(&rest[1]),
                1 => {
                    conv = rest[0].split_once('=').map_or("", |(_, id)| id).into();
                }
                _ => return super::valued(rest, &["-i", "--prompt-interactive"]),
            }
            n
        });
        let transcript = transcript(&paths::home(), &conv).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        Live { conv, transcript, args, ..Default::default() }
    }

    fn last_reply(&self, r: &Run) -> Option<String> {
        last_turn(&transcript(&paths::home(), &r.conv)?)?.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn titles_prefer_the_hook_then_look_up_only_the_current_conversation() {
        let home = std::env::temp_dir().join(format!("agy ?#% {}", paths::short_id()));
        let dir = home.join(".gemini/antigravity-cli");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("conversation_summaries.db");
        let mut r = Run { conv: "fallback".into(), ..Default::default() };
        assert_eq!(title_in(&home, &r), None);
        assert!(!path.exists());
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(title_in(&home, &r), None);
        db.execute_batch(
            "CREATE TABLE conversation_summaries(conversation_id TEXT PRIMARY KEY, title TEXT NOT NULL);
             INSERT INTO conversation_summaries VALUES ('fallback','Old'), ('current','New'), ('empty','');",
        )
        .unwrap();
        assert_eq!(title_in(&home, &r).as_deref(), Some("Old"));
        for (id, expected) in [("", Some("Old")), ("current", Some("New")), ("empty", None), ("missing", None)] {
            r.agy = Some(AgyTitle { conversation_id: id.into(), ..Default::default() });
            assert_eq!(title_in(&home, &r).as_deref(), expected);
            assert_eq!(Agy.live_title(&r, "pane"), None);
        }
        r.agy = None;
        r.hook_title = "From hook".into();
        assert_eq!(title_in(&home, &r).as_deref(), Some("From hook"));
        assert_eq!(Agy.live_title(&r, "pane").as_deref(), Some("From hook"));
        drop(db);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn reading_a_wal_database_never_creates_missing_sidecars() {
        let home = std::env::temp_dir().join(format!("agy wal ?#% {}", paths::short_id()));
        let dir = home.join(".gemini/antigravity-cli");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("conversation_summaries.db");
        let wal = path.with_extension("db-wal");
        let shm = path.with_extension("db-shm");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE conversation_summaries(conversation_id TEXT PRIMARY KEY, title TEXT);")
            .unwrap();
        db.execute("INSERT INTO conversation_summaries VALUES ('c', 'In WAL')", []).unwrap();
        let r = Run { conv: "c".into(), ..Default::default() };
        assert_eq!(title_in(&home, &r).as_deref(), Some("In WAL"));
        // An empty title in the live WAL supersedes the one in the checkpoint.
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); UPDATE conversation_summaries SET title = '';").unwrap();
        assert_eq!(title_in(&home, &r), None);
        db.execute("UPDATE conversation_summaries SET title = 'In WAL'", []).unwrap();
        drop(db);
        assert!(!wal.exists() && !shm.exists());
        assert_eq!(title_in(&home, &r).as_deref(), Some("In WAL"));
        assert!(!wal.exists() && !shm.exists());
        fs::write(&wal, []).unwrap();
        assert_eq!(title_in(&home, &r).as_deref(), Some("In WAL"));
        assert_eq!(wal.metadata().unwrap().len(), 0);
        assert!(!shm.exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pane_titles_respect_the_lock_and_fall_back_to_the_session_name() {
        let mut frame = AgyTitle { conversation_title: Some("From stream".into()), ..Default::default() };
        let mut r = Run { named: "My name".into(), auto_title: "From database".into(), locked: true, ..Default::default() };
        assert_eq!(frame.pane_title(&r), "My name");
        r.locked = false;
        assert_eq!(frame.pane_title(&r), "From stream");
        frame.conversation_title = None;
        assert_eq!(frame.pane_title(&r), "From database");
        r.auto_title.clear();
        assert_eq!(frame.pane_title(&r), "My name");
    }

    #[test]
    fn launch_merges_settings_files_and_json_and_preserves_argument_boundaries() {
        let dir = std::env::temp_dir().join(paths::short_id());
        fs::create_dir(&dir).unwrap();
        let original = r#"{"colorScheme":"light","agentMode":"","title":{"type":"static"}}"#;
        fs::write(dir.join("my settings.json"), original).unwrap();
        let tool = Tool {
            cmd: strings(&["wrapper", "agy"]),
            args: strings(&["--settings", "my settings.json", "--model", "Gemini"]),
            ..Default::default()
        };
        let s =
            Session { dir: dir.to_string_lossy().into_owned(), args: strings(&["--settings={\"x\":1}", "--foo"]), ..Default::default() };
        let args = Agy.argv(&tool, &s, &Agy.resume("c1", &Run::default()), "/it's aca").unwrap();
        assert_eq!(args[..5], ["wrapper", "agy", "--conversation", "c1", "--settings"]);
        assert_eq!(args[6..], ["--model", "Gemini", "--foo"]);
        let settings: Value = serde_json::from_str(&args[5]).unwrap();
        assert_eq!(
            settings,
            json!({"colorScheme":"light", "agentMode":"", "x":1,
            "title":{"type":"command", "command":r#"'/it'\''s aca' hook-title agy"#}})
        );
        assert_eq!(fs::read_to_string(dir.join("my settings.json")).unwrap(), original);
        let fresh = Agy.argv(&Tool::default(), &Session::default(), &Agy.resume("", &Run::default()), "aca").unwrap();
        assert_eq!(fresh[..2], ["agy", "--settings"]);
        assert_eq!(serde_json::from_str::<Value>(&fresh[2]).unwrap().as_object().unwrap().len(), 1);
        for bad in ["--settings", "--settings=absent.json", "--settings={broken"] {
            assert!(Agy.argv(&Tool::default(), &Session { args: strings(&[bad]), ..s.clone() }, &How::Fresh, "aca").is_err());
        }
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(Agy.opening("hello"), Some(strings(&["-i"])));
        assert_eq!((Agy.opening("-v"), Agy.opening("a\nb")), (None, None));
        assert!(!Agy.can_fork() && Agy.home_var().is_none());
    }

    #[test]
    fn adopting_carries_flags_but_never_replays_an_opening() {
        for (flags, id) in [
            (vec!["--conversation", "abc"], "abc"),
            (vec!["--conversation=abc"], "abc"),
            (vec!["-r", "abc"], "abc"),
            (vec!["--resume", "abc"], "abc"),
            (vec!["--resume"], ""),
            (vec!["-c"], ""),
            (vec!["--continue"], ""),
        ] {
            let mut args = strings(&["agy", "-i", "hello"]);
            args.extend(strings(&flags));
            args.extend(strings(&["--model", "m"]));
            let live = Agy.live(0, None, &args, &|_| false);
            assert_eq!(live.conv, id);
            assert_eq!(live.args, Some(strings(&["--model", "m"])));
        }
        assert_eq!(crate::agent::by_comm("agy"), Some("agy"));
    }

    #[test]
    fn adopt_keeps_valued_options_with_spaces_and_separates_the_opening() {
        let settings = r#"{"trustedWorkspaces":["/tmp/my project"]}"#;
        let mut argv = strings(&["agy", "--conversation=c1", "--settings", settings, "--model", "Gemini 4 Neon (High)"]);
        let mut expected = argv[2..].to_vec();
        for flag in [
            "--add-dir",
            "--agent",
            "--effort",
            "--log-file",
            "--mode",
            "--model",
            "--project",
            "--settings",
            "--json-schema",
            "--print-timeout",
            "--input-format",
            "--output-format",
        ] {
            for option in [strings(&[flag, "two words"]), vec![format!("{flag}=two words")]] {
                argv.extend_from_slice(&option);
                expected.extend(option);
            }
        }
        argv.extend(strings(&["-i", "hello there", "--prompt-interactive=hello again", "--continue"]));
        let live = Agy.live(0, None, &argv, &|_| false);
        assert_eq!((live.conv.as_str(), live.args), ("c1", Some(expected)));
        for tail in [&["positional prompt"][..], &["--", "--model=literal"], &["--settings"]] {
            let mut args = argv.clone();
            args.extend(strings(tail));
            assert!(Agy.live(0, None, &args, &|_| false).args.is_none());
        }
    }

    // Trimmed 1.5.16 steps, including a planner tool call without any reply text.
    const TURN: &str = r#"{"step_index":5,"type":"USER_INPUT","content":"<USER_REQUEST>\nReply with the single word: ok\n</USER_REQUEST>\n<ADDITIONAL_METADATA>…</ADDITIONAL_METADATA>"}
{"step_index":6,"type":"SYSTEM_MESSAGE","content":"[Notice] All your subagents and background tasks have been stopped due to server restart."}
{"step_index":8,"type":"PLANNER_RESPONSE","content":"ok"}"#;
    const NO_REPLY: &str = r#"{"type":"USER_INPUT","content":"<USER_REQUEST>\nFirst run `pwd`, then run `rm notes.txt`, as two separate commands. Reply with the pwd output only.\n</USER_REQUEST>"}
{"type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{"CommandLine":"rm notes.txt"}}]}
{"type":"GENERIC","status":"ERROR","error":"permission check failed","content":"user denied permission to run command"}"#;

    #[test]
    fn only_planner_text_after_the_latest_user_input_is_a_reply() {
        let f = std::env::temp_dir().join(paths::short_id());
        fs::write(&f, format!("{TURN}\n{}\n{NO_REPLY}", json!({"type":"GENERIC","content":"x".repeat(70000)}))).unwrap();
        let (prompt, reply) = last_turn(&f).unwrap();
        assert!(prompt.starts_with("First run `pwd`"));
        assert_eq!(reply, None);
        fs::write(&f, format!("{TURN}\n{}", json!({"type":"GENERIC","content":"x".repeat(70000)}))).unwrap();
        assert_eq!(last_turn(&f), Some(("Reply with the single word: ok".into(), Some("ok".into()))));
        fs::remove_file(f).unwrap();
        assert!(transcript(Path::new("/test"), "../elsewhere").is_none());
    }

    fn redraw(r: &mut Run, home: &Path, json: &str) -> super::super::Events {
        let title: AgyTitle = serde_json::from_str(json).unwrap();
        title.apply(r, home, 123).into_iter().flat_map(|(_, events)| events).collect()
    }

    #[test]
    fn late_first_user_input_fills_the_open_or_closing_turn_without_a_second_start() {
        for (later, steps, terminal) in
            [("tool_use", TURN, "turn.done"), ("idle", TURN, "turn.done"), ("idle", NO_REPLY, "turn.interrupted")]
        {
            let home = std::env::temp_dir().join(paths::short_id());
            let file = transcript(&home, "new").unwrap();
            let mut r = Run::default();
            let mut v = json!({"agent_state":"working", "conversation_id":""});
            assert_eq!(redraw(&mut r, &home, &v.to_string()), [("turn.started", json!({"prompt":""}))]);
            v["conversation_id"] = json!("new");
            assert!(redraw(&mut r, &home, &v.to_string()).is_empty());
            assert!(r.prompt.is_empty());
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, steps).unwrap();
            v["agent_state"] = json!(later);
            let mut events = redraw(&mut r, &home, &v.to_string());
            let prompt = if steps == TURN { "Reply with the single word: ok" } else { "First run `pwd`" };
            assert!(r.prompt.starts_with(prompt), "{}", r.prompt);
            if later != "idle" {
                assert!(events.is_empty());
                v["agent_state"] = json!("idle");
                events = redraw(&mut r, &home, &v.to_string());
            }
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].0, terminal);
            if terminal == "turn.done" {
                assert_eq!(events[0].1, json!({"last":"ok", "secs":0, "bg":0}));
            }
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn captured_title_sequence_reports_approval_reply_denial_and_interrupt_once() {
        let home = std::env::temp_dir().join(paths::short_id());
        let file = transcript(&home, "93b602da-528f-41ac-bad3-7cf6c67af176").unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, TURN).unwrap();
        let working = r#"{"agent_state":"working","conversation_id":"93b602da-528f-41ac-bad3-7cf6c67af176"}"#;
        let asking = working.replace("working\"", "tool_use\",\"tool_confirmation_pending\":true");
        let idle = working.replace("working", "idle");
        let mut r = Run::default();
        assert!(redraw(&mut r, &home, r#"{"agent_state":"authenticating","conversation_id":""}"#).is_empty());
        assert!(redraw(&mut r, &home, r#"{"agent_state":"idle","conversation_id":""}"#).is_empty());
        assert_eq!(redraw(&mut r, &home, working), [("turn.started", json!({"prompt":"Reply with the single word: ok"}))]);
        assert_eq!(r.transcript, file.to_string_lossy());
        assert_eq!(redraw(&mut r, &home, &asking)[0].0, "agent.waiting");
        assert_eq!(r.status, "waiting");
        assert!(redraw(&mut r, &home, &asking).is_empty());
        assert!(redraw(&mut r, &home, working).is_empty());
        assert_eq!(r.status, "running");
        assert_eq!(redraw(&mut r, &home, &idle), [("turn.done", json!({"last":"ok","bg":0,"secs":0}))]);
        assert_eq!(r.status, "done");
        fs::write(&file, format!("{TURN}\n{NO_REPLY}")).unwrap();
        redraw(&mut r, &home, working);
        redraw(&mut r, &home, &asking);
        assert_eq!(redraw(&mut r, &home, &idle)[0].0, "turn.interrupted");
        assert_eq!(r.status, "idle");
        redraw(&mut r, &home, working);
        r.status = "idle".into(); // aca interrupt recorded Esc before the redraw.
        assert!(redraw(&mut r, &home, &idle).is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn late_id_title_review_and_failure_preserve_turn_boundaries() {
        let home = std::env::temp_dir().join(paths::short_id());
        let mut r = Run::default();
        let mut state = json!({"agent_state":"working","conversation_id":""});
        assert_eq!(Agy.on_hook(&mut r, &state, 123)[0].0, "turn.started");
        state["conversation_id"] = json!("c1");
        state["conversation_title"] = json!("Run Pwd And Sleep Commands");
        assert!(redraw(&mut r, &home, &state.to_string()).is_empty());
        assert_eq!((r.status.as_str(), r.hook_title.as_str()), ("running", "Run Pwd And Sleep Commands"));
        state["agent_state"] = json!("reviewing");
        assert_eq!(redraw(&mut r, &home, &state.to_string())[0].0, "agent.waiting");
        state["agent_state"] = json!("working");
        assert!(redraw(&mut r, &home, &state.to_string()).is_empty());
        assert_eq!(r.status, "running");
        state["agent_state"] = json!("error");
        assert_eq!(redraw(&mut r, &home, &state.to_string())[0].0, "turn.failed");
        assert!(redraw(&mut r, &home, &state.to_string()).is_empty());
        state["conversation_id"] = json!("c2");
        state["agent_state"] = json!("idle");
        assert_eq!(redraw(&mut r, &home, &state.to_string())[0].0, "conversation.changed");
    }

    #[test]
    fn only_the_captured_command_dialog_can_be_answered_with_digits() {
        let lines = |s: &str| s.lines().map(String::from).collect::<Vec<_>>();
        let ask = "Requesting permission for:\n   pwd\n\nRun this command?\n> 1. Yes, run command\n  \
            2. Yes, and always allow in this conversation for commands that start with 'pwd'\n  \
            3. Yes, and always allow for commands that start with 'pwd' (Persist to settings.json)\n  4. No, cancel\n\n  \
            ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command\nesc to cancel";
        let screen = lines(ask);
        assert_eq!(Agy.screen(&screen), Seen::Ask("pwd".into()));
        assert_eq!(Agy.permission_keys(), Some(("1", "4")));
        assert_eq!(Agy.interrupt_keys(), Some(["Escape"].as_slice()));
        assert!(!Agy.reports_interrupt());
        let waiting = Run { agy: Some(AgyTitle::default()), status: "waiting".into(), ask_kind: "permission".into(), ..Default::default() };
        assert_eq!(crate::answer::asking(&waiting, &screen).ask, "pwd");
        let unknown = lines("Allow creation of this file?\n> 1. Yes\n  2. Always allow");
        assert_eq!(Agy.screen(&unknown), Seen::Unsure);
        assert!(crate::answer::asking(&waiting, &unknown).kind.is_empty());
        assert_eq!(Agy.screen(&lines("⎿  Interrupted · What should Antigravity CLI do instead?")), Seen::Quiet);
        assert_eq!(Agy.screen(&lines("○ Running (sleep 30)\nesc to cancel")), Seen::Unsure);
        assert_eq!(Agy.screen(&lines("? for shortcuts")), Seen::Unsure);
        let trust = "Do you trust the contents of this project?\n> Yes, I trust this folder";
        assert_eq!(Agy.trust_keys(&lines(trust)), Some(vec!["Enter"]));
        assert_eq!(Agy.trust_keys(&lines(&trust.replace("> Yes", "Yes"))), None);
        assert_eq!(Agy.trust_keys(&screen), None);
    }
}
