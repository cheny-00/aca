//! Claude Code. Hooks are passed at startup with `--settings <file>`; we choose the conversation id (`--session-id`); the
//! title is the one it writes into the pane title (the conversation record only when that's missing); on screen we read
//! choice prompts and the "running" line.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::{Agent, How, Live};
use crate::config::Tool;
use crate::paths::{self, Res};
use crate::runtime::Run;
use crate::screen::{self, Seen};
use crate::store::Session;
use crate::{t, tr};

pub struct Claude;

/// Only hook the events the state needs. Of tool events only AskUserQuestion (it's asking you), not `*`: that would start
/// one more process on every tool call
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("StopFailure", None),
    ("Notification", None),
    ("PermissionRequest", None),
    ("PreCompact", None),
    ("PostCompact", None),
    ("SessionEnd", None),
    ("PreToolUse", Some("AskUserQuestion")),
    ("PostToolUse", Some("AskUserQuestion")),
];

static ASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Enter to select|esc to cancel|Do you want to|Would you like to").unwrap());
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\S\s+\S+…\s+\((?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?").unwrap());
/// Right after a turn starts, before the first token comes back (a dozen-odd seconds with a slow model), that line is just
/// "· Combobulating…" with no timer after it. Such a line is only recognized in the few lines right above the input box, and
/// must start with one of its spinner characters: text can have lines ending in an ellipsis too
static STARTING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[·✢✳✶✻✽]\s+\S.*?…(?:\s+\((?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?.*)?\s*$").unwrap());
/// How many lines above the input box to look at: between the "running" line and the input box there are only a line or two
/// of tips (`⎿  Tip: …`)
const NEAR: usize = 6;
/// The tail of the text: this many lines above the input box (see `body`)
const BODY: usize = 8;

/// Which line the input box is on: the bottom-most line starting with `❯`, with a rule above and below it. The `❯` lines
/// higher up are things you said; when a confirmation / choice prompt pops up the input box folds away, and the `❯` in it
/// is the cursor before an option, without rules on both sides
fn input_at(tail: &[&str]) -> Option<usize> {
    let rule = |l: &&str| l.trim_start().starts_with('─');
    let at = tail.iter().rposition(|l| l.trim_start().starts_with('❯'))?;
    (at > 0 && rule(&tail[at - 1]) && tail[at + 1..].iter().any(rule)).then_some(at)
}

/// The timer in that line (1h 9m 1s) → seconds; no timer = 0
fn secs(c: &regex::Captures) -> u64 {
    let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
    n(1) * 3600 + n(2) * 60 + n(3)
}

/// Content of the `--settings` file. Besides the events the state needs, there's the doorbell (passing messages between
/// sessions, see [`crate::inbox`]):
/// - SessionStart also runs `aca inbox watch`: it returns `watchPaths`, so Claude itself watches this session's doorbell file
/// - When the doorbell changes (FileChanged), `aca inbox take` runs in the background (`asyncRewake`): if there are undelivered
///   messages it writes them to stderr and exits 2, and Claude hands that to the model as a system prompt — idle, it starts a
///   turn; running, it's slipped into this turn; waiting for you, it's sent after you answer. With none it exits 0 and
///   nothing happens. `rewakeMessage` is the marker before that text, `rewakeSummary` the line shown in its interface
///   (tested on 2.1.294)
fn settings(aca: &str) -> String {
    let q = super::sh_quote(aca);
    let command = format!("{q} hook claude");
    let mut hooks: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|(ev, matcher)| {
            let mut entry = json!({ "hooks": [{ "type": "command", "command": command, "timeout": 10 }] });
            if let Some(m) = matcher {
                entry["matcher"] = json!(m);
            }
            (ev.to_string(), json!([entry]))
        })
        .collect();
    if let Some(list) = hooks["SessionStart"][0]["hooks"].as_array_mut() {
        list.push(json!({ "type": "command", "command": format!("{q} inbox watch"), "timeout": 10 }));
    }
    // On startup and on every prompt, hand it the session's name to use as its own (see hook::name_cmd)
    for ev in ["SessionStart", "UserPromptSubmit"] {
        if let Some(list) = hooks[ev][0]["hooks"].as_array_mut() {
            list.push(json!({ "type": "command", "command": format!("{q} hook-name"), "timeout": 10 }));
        }
    }
    let take = json!({
        "type": "command", "command": format!("{q} inbox take"), "timeout": 30,
        "asyncRewake": true, "rewakeMessage": "[aca]", "rewakeSummary": "aca: 收件箱有新消息",
    });
    hooks.insert("FileChanged".into(), json!([{ "hooks": [take] }]));
    serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap()
}

/// Pull out the `--settings` you passed yourself (in the tool's, account's or session's arguments). Claude only honors the
/// last `--settings` (tested on 2.1.288: with two side by side, none of the hooks in the first one ran), so it can't sit next
/// to aca's; they have to be merged into one ([`merge`])
fn take_settings(extra: &mut Vec<String>) -> Option<String> {
    let (mut found, mut i) = (None, 0);
    while i < extra.len() {
        if extra[i] == "--settings" && i + 1 < extra.len() {
            found = Some(extra.remove(i + 1));
            extra.remove(i);
        } else if let Some(v) = extra[i].strip_prefix("--settings=") {
            found = Some(v.to_string());
            extra.remove(i);
        } else {
            i += 1;
        }
    }
    found
}

/// Merge aca's hooks into your settings (a file, or a JSON snippet): under the same event yours come first and aca's after,
/// everything else as-is. Relative paths are resolved against the session's directory (Claude reads them there too)
fn merge(ours: &str, theirs: &str, dir: &str) -> Res<String> {
    let text = if theirs.trim_start().starts_with('{') {
        theirs.to_string()
    } else {
        let file = Path::new(dir).join(paths::expand(theirs));
        std::fs::read_to_string(&file)
            .map_err(|e| tr!("无法读取 --settings 指定的 {}:{e}", "can't read the --settings you passed {}: {e}", file.display()))?
    };
    let mut all: Value =
        serde_json::from_str(&text).map_err(|e| tr!("--settings 不是有效的 JSON:{e}", "the --settings you passed isn't JSON: {e}"))?;
    let ours: Value = serde_json::from_str(ours).unwrap();
    let hooks = all
        .as_object_mut()
        .ok_or(t!("--settings 不是 JSON 对象", "the --settings you passed isn't a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or(t!("--settings 中的 hooks 不是对象", "hooks in the --settings you passed isn't an object"))?;
    for (ev, entries) in ours["hooks"].as_object().unwrap() {
        let mine = hooks.entry(ev.clone()).or_insert_with(|| json!([]));
        let mine = mine
            .as_array_mut()
            .ok_or_else(|| tr!("--settings 中的 hooks.{ev} 不是数组", "hooks.{ev} in the --settings you passed isn't an array"))?;
        mine.extend(entries.as_array().unwrap().iter().cloned());
    }
    Ok(serde_json::to_string_pretty(&all).unwrap())
}

/// The pure half: no file writes. `settings` is the one to pass (already merged if you passed your own), `mods` is the
/// directory of aca's mod (not passed = None), `mcp` is the `--mcp-config` file for plugins' MCP servers (none = None)
fn plan(tool: &Tool, s: &Session, how: &How, settings: &str, mods: Option<&str>, mcp: Option<&str>) -> Vec<String> {
    let mut argv = super::base(tool, "claude");
    let mut a = |xs: &[&str]| argv.extend(xs.iter().map(|x| x.to_string()));
    match how {
        How::Fresh if s.conv.is_empty() => {}
        How::Fresh => a(&["--session-id", &s.conv]),
        How::Resume(conv) => a(&["--resume", conv]),
        How::Fork(from) => {
            a(&["--resume", from, "--fork-session"]);
            if !s.conv.is_empty() {
                a(&["--session-id", &s.conv]);
            }
        }
    }
    // --mcp-config takes several files: follow it right away with a flag, so your arguments after it aren't taken as its own
    if let Some(f) = mcp {
        a(&["--mcp-config", f]);
    }
    a(&["--settings", settings]);
    if let Some(dir) = mods {
        a(&["--plugin-dir", dir]);
    }
    // Claude plugins (commands, skills…) brought by enabled aca plugins: only for this start, not installed into its config
    // directory. Can be passed more than once
    for d in &tool.plugin_dirs {
        a(&["--plugin-dir", d]);
    }
    let mut extra = super::extra(tool, s);
    take_settings(&mut extra);
    argv.extend(extra);
    argv
}

/// The mark it writes at the start of the pane title: ✳ when idle, spinning ◐ ◑ while working (older versions spun braille
/// dots); inside tmux it doesn't spin and stays ✳
const MARKS: &str = "✳◐◑";
/// What it writes before it has a title
const UNTITLED: &str = "Claude Code";

/// Pane title `✳ 修登录` → `修登录` ("fix login"): the one you set with /rename, else the one it came up with, the same as
/// shown in its own interface. Not counted: titles not starting with its mark (terminal title turned off: tmux's default
/// host name, one a shell wrote), and no title yet
fn shown_title(pane_title: &str) -> Option<String> {
    let mut cs = pane_title.chars();
    let mark = cs.next()?;
    if !(MARKS.contains(mark) || screen::spinning(pane_title)) || cs.next() != Some(' ') {
        return None;
    }
    let t = cs.as_str().trim();
    (!t.is_empty() && t != UNTITLED).then(|| t.to_string())
}

/// The title in the conversation record: the name you gave it (custom-title), else the one it came up with (ai-title), the
/// last one written of each. Only reads the end of the file.
/// Not "whichever was written later": every so often it rewrites both, the one it came up with written after
/// The last reply with text at the end of the conversation record. Not counted: what spawned subagents said, and what Claude
/// Code filled in itself (model is `<synthetic>`)
fn transcript_reply(transcript: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    // The reply may be followed by several big chunks of tool output: look back further than for the title
    const TAIL: u64 = 1024 * 1024;
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter(|l| l.contains("\"type\":\"assistant\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        if v["type"] != "assistant" || v["isSidechain"] == true || v["message"]["model"] == "<synthetic>" {
            return None;
        }
        let parts: Vec<&str> =
            v["message"]["content"].as_array()?.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
        let reply = parts.join("\n");
        (!reply.trim().is_empty()).then_some(reply)
    })
}

fn transcript_title(transcript: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 256 * 1024;
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let last = |kind: &str, key: &str| {
        let tag = format!("\"type\":\"{kind}\"");
        text.lines().rev().find_map(|l| {
            if !l.contains(&tag) {
                return None;
            }
            let v: Value = serde_json::from_str(l).ok()?;
            let t = v.get(key)?.as_str()?.trim();
            (!t.is_empty()).then(|| t.to_string())
        })
    };
    last("custom-title", "customTitle").or_else(|| last("ai-title", "aiTitle"))
}

/// Claude turns the project directory into a folder name under projects: every non-alphanumeric character becomes -
fn project_key(dir: &str) -> String {
    dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// On the command line: the "which conversation to resume" arguments, plus the hooks, mod and aca plugin directories aca
/// passed itself: not carried over when adopting (rebuilt on start)
fn not_carried(rest: &[String]) -> usize {
    if matches!(rest[0].as_str(), "--continue" | "-c" | "--fork-session") {
        return 1;
    }
    let n = super::valued(rest, &["--session-id"]).max(super::optional(rest, &["--resume", "-r"]));
    if n > 0 {
        return n;
    }
    let n = super::valued(rest, &["--settings", "--plugin-dir", "--mcp-config"]);
    let file = if n == 2 { rest.get(1).map(String::as_str) } else { rest[0].split_once('=').map(|(_, v)| v) };
    if rest[0].starts_with("--plugin-dir") && file.is_some_and(crate::plugin::is_agent_dir) {
        return n;
    }
    if file.is_some_and(|f| Path::new(f).starts_with(paths::state_dir().join("hooks"))) { n } else { 0 }
}

/// Which conversation a running Claude is on now: it records this itself in `sessions/<pid>.json` in the config directory
/// (the current conversation id — it changes after /clear and compaction — the start directory, the title); older versions
/// without this file: look at `--session-id` / `--resume` on the command line
fn live_in(conf: &Path, pid: u32, argv: &[String]) -> Live {
    let own: Value = std::fs::read_to_string(conf.join("sessions").join(format!("{pid}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|v: &Value| v["pid"].as_u64() == Some(pid as u64))
        .unwrap_or_default();
    // Started as a fork (--resume an old one with --fork-session): the id on the command line is the old conversation's, not
    // its current one
    let from_argv = || {
        let at = argv.iter().position(|a| matches!(a.as_str(), "--session-id" | "--resume" | "-r"))?;
        argv.get(at + 1).filter(|v| !v.starts_with('-') && !argv.iter().any(|a| a == "--fork-session")).cloned()
    };
    let conv = own["sessionId"].as_str().map(String::from).or_else(from_argv).filter(|c| !c.contains('/')).unwrap_or_default();
    let projects = conf.join("projects");
    let file = format!("{conv}.jsonl");
    // Usually right in the folder for the start directory; Claude truncates and adds a hash when the name is too long, so then
    // search them one by one
    let transcript = (!conv.is_empty())
        .then(|| {
            let near = own["cwd"].as_str().map(|d| projects.join(project_key(d)).join(&file)).filter(|p| p.is_file());
            near.or_else(|| std::fs::read_dir(&projects).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file()))
        })
        .flatten();
    // With no name given it makes up a placeholder (directory name plus two characters: `aca-5e`, nameSource derived): not a title
    let named = own["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && own["nameSource"] != "derived").map(String::from);
    Live {
        title: named.or_else(|| transcript.as_deref().and_then(transcript_title)).unwrap_or_default(),
        transcript: transcript.map(|t| t.to_string_lossy().into_owned()).unwrap_or_default(),
        conv,
        args: super::carry(argv, not_carried),
    }
}

impl Agent for Claude {
    fn name(&self) -> &str {
        "claude"
    }

    fn home_var(&self) -> Option<&str> {
        Some("CLAUDE_CONFIG_DIR")
    }

    fn default_home(&self) -> Option<&str> {
        Some("~/.claude")
    }

    /// The conversation record `projects/<project>/<id>.jsonl`, the directory of the same name next to it (subagent records,
    /// large tool outputs saved out), plus `file-history/<id>` used by `/rewind` and the task list `tasks/<id>` (when present)
    fn conv_files(&self, home: &Path, conv: &str) -> Vec<PathBuf> {
        if conv.is_empty() || conv.contains('/') {
            return vec![];
        }
        let file = format!("{conv}.jsonl");
        let Some(dir) = std::fs::read_dir(home.join("projects")).ok().and_then(|d| d.flatten().find(|d| d.path().join(&file).is_file()))
        else {
            return vec![];
        };
        let project = Path::new("projects").join(dir.file_name());
        let mut out = vec![project.join(&file)];
        let extra = [project.join(conv), Path::new("file-history").join(conv), Path::new("tasks").join(conv)];
        out.extend(extra.into_iter().filter(|p| home.join(p).is_dir()));
        out
    }

    fn new_conv(&self) -> String {
        paths::uuid4()
    }

    fn can_fork(&self) -> bool {
        true
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>> {
        let ours = settings(aca);
        let file = match take_settings(&mut super::extra(tool, s)) {
            None => super::hook_file("claude.json", &ours)?,
            // The merged file is named by its content: sessions passing different ones don't overwrite each other
            Some(theirs) => {
                let all = merge(&ours, &theirs, &s.dir)?;
                let mut h = DefaultHasher::new();
                all.hash(&mut h);
                let file = super::hook_file(&format!("claude-{:016x}.json", h.finish()), &all)?;
                // It holds all of your settings (possibly with keys): readable only by us
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
                file
            }
        };
        let mods = super::claude_mod::offer(tool, &s.id).map(|d| d.to_string_lossy().into_owned());
        // Plugins' MCP servers: one --mcp-config named by its content (the session id is in the environment, not the file: sessions
        // with the same plugins share one)
        let mcp = match tool.mcp.is_empty() {
            true => None,
            false => {
                let text = crate::plugin::mount::claude_mcp_config(aca, &tool.mcp);
                let mut h = DefaultHasher::new();
                text.hash(&mut h);
                Some(super::hook_file(&format!("mcp-{:016x}.json", h.finish()), &text)?.to_string_lossy().into_owned())
            }
        };
        Ok(plan(tool, s, how, &file.to_string_lossy(), mods.as_deref(), mcp.as_deref()))
    }

    /// Can only resume if the conversation record exists (a session started but never talked to has none); a fork never talked
    /// to forks again from the original
    fn resume(&self, conv: &str, r: &Run) -> How {
        let t = Path::new(&r.transcript);
        let known = !conv.is_empty() && t.file_stem().is_some_and(|f| f.to_string_lossy() == conv) && t.is_file();
        match (known, r.fork_from.is_empty()) {
            (true, _) => How::Resume(conv.to_string()),
            (false, false) => How::Fork(r.fork_from.clone()),
            (false, true) => How::Fresh,
        }
    }

    fn title_in_pane(&self) -> bool {
        true
    }

    /// The title doesn't come from hooks. Look at the pane title first: it writes it there as soon as it has one, and it changes
    /// right after /rename. Only when it's not there (terminal title turned off) read the conversation record, and only on
    /// these events
    fn title(&self, r: &Run, pane_title: &str) -> Option<String> {
        shown_title(pane_title).or_else(|| {
            matches!(r.event.as_str(), "Stop" | "UserPromptSubmit" | "PostCompact")
                .then(|| transcript_title(Path::new(&r.transcript)))
                .flatten()
        })
    }

    fn live_title(&self, _r: &Run, pane_title: &str) -> Option<String> {
        shown_title(pane_title)
    }

    /// The process name is claude, or just a version number (`2.1.284`: it installs files named by version)
    fn runs_as(&self, comm: &str) -> bool {
        let version = comm.split('.').count() == 3 && comm.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        comm == "claude" || version
    }

    fn last_reply(&self, r: &Run) -> Option<String> {
        transcript_reply(Path::new(&r.transcript))
    }

    fn live(&self, pid: u32, home: Option<&str>, argv: &[String]) -> Live {
        let conf = home.map_or_else(|| PathBuf::from(paths::expand("~/.claude")), PathBuf::from);
        live_in(&conf, pid, argv)
    }

    fn delivery(&self) -> super::Delivery {
        super::Delivery::Bell
    }

    fn prompt_mark(&self) -> Option<char> {
        Some('❯')
    }

    /// The permission prompt's number and order of options vary (4 seen on 2.1.294, with "No" 4th), so do only two things:
    /// approve with 1, deny with Esc
    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        Some(("1", "Escape"))
    }

    /// One Esc stops it while running (no hook at all, not even Stop)
    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["Escape"])
    }

    fn opening(&self, text: &str) -> Option<Vec<String>> {
        super::opening_after_dashes(text)
    }

    /// "Accessing workspace: … ❯ No, exit / Yes, I trust this folder … Enter to confirm · Esc to cancel".
    /// On 2.1.29x the cursor starts on No (Enter right away exits); earlier it was "1. Yes, I trust this folder / 2. No, exit".
    /// Only recognized when the two options are next to each other with the cursor on one of them; move the cursor to Yes and
    /// press Enter
    fn trust_keys(&self, lines: &[String]) -> Option<Vec<&'static str>> {
        let rows: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        let tail = &rows[rows.len().saturating_sub(12)..];
        if !tail.iter().any(|l| l.contains("Enter to confirm")) {
            return None;
        }
        // An option line without the leading cursor and number
        let label = |l: &str| -> String {
            l.trim_start_matches('❯').trim_start().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.').trim_start().to_string()
        };
        let yes = tail.iter().position(|l| label(l).starts_with("Yes, I trust this folder"))?;
        let no = tail.iter().position(|l| label(l).starts_with("No, exit"))?;
        if yes.abs_diff(no) != 1 {
            return None;
        }
        let at = [yes, no].into_iter().find(|&i| tail[i].starts_with('❯'))?;
        let mut keys = vec![if yes > at { "Down" } else { "Up" }; yes.abs_diff(at)];
        keys.push("Enter");
        Some(keys)
    }

    /// The bottom of the screen: a choice / confirmation prompt (with the question it asks), or the "✻ Pondering… (1m 12s" line
    /// (with seconds run so far)
    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(25)..];
        let input = input_at(tail);
        // The input box is still there = no prompt popped up: a sentence like "Do you want to" that happens to be in text or a
        // command doesn't count as asking
        if input.is_none() && tail[tail.len().saturating_sub(12)..].iter().any(|l| ASK.is_match(l)) {
            let q = tail.iter().map(|l| l.trim()).find(|l| l.ends_with('?') || l.ends_with('？')).unwrap_or("");
            return Seen::Ask(q.to_string());
        }
        if let Some(c) = tail.iter().rev().find_map(|l| RUNNING.captures(l)) {
            return Seen::Running(secs(&c));
        }
        // The few lines above the rule over the input box
        let near = input.map_or(&[][..], |at| &tail[(at - 1).saturating_sub(NEAR)..at - 1]);
        if let Some(c) = near.iter().rev().find_map(|l| STARTING.captures(l)) {
            return Seen::Running(secs(&c));
        }
        // While streaming text the spinner line isn't at the bottom; only the status bar below the input box says
        // "esc to interrupt" (2.1.295)
        match input {
            Some(at) if tail[at + 1..].iter().any(|l| l.contains("esc to interrupt")) => Seen::Running(0),
            _ => Seen::Quiet,
        }
    }

    /// [`BODY`] lines above the rule over the input box. While 2.1.300 streams text there's no spinner line and no esc to
    /// interrupt in the status bar; only these lines change. Trailing spaces are stripped: a screen captured with colors by the
    /// interface, once stripped of colors, differs by a few trailing spaces from one captured without
    fn body(&self, lines: &[String]) -> Option<u64> {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.trim_end()).filter(|l| !l.trim().is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(25)..];
        let at = input_at(tail)?;
        let mut h = DefaultHasher::new();
        tail[(at - 1).saturating_sub(BODY)..at - 1].hash(&mut h);
        Some(h.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(conv: &str) -> Session {
        Session {
            id: "ab12".into(),
            tool: "claude".into(),
            conv: conv.into(),
            args: vec!["--model".into(), "haiku".into()],
            ..Default::default()
        }
    }

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn the_folder_trust_question_is_answered_by_walking_to_yes() {
        // Copied from 2.1.29x: the cursor starts on No
        let ask = "────────────────\n Accessing workspace:\n\n /p/api\n\n Quick safety check: Is this a project you created or one you trust? (Like your\n own code, a well-known open source project, or work from your team). If not,\n take a moment to review what's in this folder first.\n\n Claude Code'll be able to read, edit, and execute files here.\n\n Security guide\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n";
        assert_eq!(Claude.trust_keys(&lines(ask)), Some(vec!["Down", "Enter"]));
        // An earlier version: numbered, with Yes on top and the cursor on it
        let old = " Do you trust the files in this folder?\n\n /p/api\n\n ❯ 1. Yes, I trust this folder\n   2. No, exit\n\n Enter to confirm · Esc to cancel\n";
        assert_eq!(Claude.trust_keys(&lines(old)), Some(vec!["Enter"]));
        // Cursor on No below: move up
        let below = " ❯ 2. No, exit\n".to_string();
        let up = old.replace(" ❯ 1. Yes", "   1. Yes").replace("   2. No, exit\n", &below);
        assert_eq!(Claude.trust_keys(&lines(&up)), Some(vec!["Up", "Enter"]));
        // Other prompts, text that mentions these words, half-drawn ones (cursor not shown yet): don't press
        let perm = "● 要改 3 个文件\n\n Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. Yes, allow all edits during this session\n   3. No\n\n Esc to cancel\n";
        assert_eq!(Claude.trust_keys(&lines(perm)), None);
        assert_eq!(Claude.trust_keys(&lines("● 它会问 Yes, I trust this folder 还是 No, exit\n\n❯ ")), None);
        assert_eq!(Claude.trust_keys(&lines(&ask.replace("❯ No, exit", "  No, exit"))), None);
        assert_eq!(Claude.trust_keys(&lines(&ask.replace(" Enter to confirm · Esc to cancel\n", ""))), None);
        assert_eq!(Claude.trust_keys(&[]), None);
    }

    #[test]
    fn it_starts_resumes_and_forks_with_hooks_passed_at_launch() {
        let t = Tool { args: vec!["-Q".into()], ..Default::default() };
        assert_eq!(
            plan(&t, &s("c-1"), &How::Fresh, "/st/claude.json", None, None).join(" "),
            "claude --session-id c-1 --settings /st/claude.json -Q --model haiku"
        );
        let plain = Tool::default();
        assert_eq!(
            plan(&plain, &s("c-1"), &How::Resume("c-9".into()), "/st/c.json", None, None).join(" "),
            "claude --resume c-9 --settings /st/c.json --model haiku"
        );
        assert_eq!(
            plan(&plain, &s("c-2"), &How::Fork("c-1".into()), "/st/c.json", None, None).join(" "),
            "claude --resume c-1 --fork-session --session-id c-2 --settings /st/c.json --model haiku"
        );
        // A tool with its own command (a wrapper script): the arguments still follow
        let wrapped = Tool { cmd: vec!["/w/cc.sh".into(), "-x".into()], ..Default::default() };
        assert_eq!(plan(&wrapped, &s(""), &How::Fresh, "/st/c.json", None, None)[..4], ["/w/cc.sh", "-x", "--settings", "/st/c.json"]);
        // aca's mod is passed at startup like the hooks: right after --settings, with your own arguments still last
        assert_eq!(
            plan(&plain, &s("c-1"), &How::Fresh, "/st/c.json", Some("/st/hooks/claude-mod/aca"), None).join(" "),
            "claude --session-id c-1 --settings /st/c.json --plugin-dir /st/hooks/claude-mod/aca --model haiku"
        );
        // Plugins' MCP servers: --mcp-config right after --settings, so it doesn't swallow later arguments as its own
        assert_eq!(
            plan(&plain, &s("c-1"), &How::Fresh, "/st/c.json", None, Some("/st/hooks/mcp-1.json")).join(" "),
            "claude --session-id c-1 --mcp-config /st/hooks/mcp-1.json --settings /st/c.json --model haiku"
        );
    }

    #[test]
    fn your_own_settings_are_merged_with_the_hooks_into_the_one_file_claude_reads() {
        // --settings from the tool (the account's arguments follow it) and the session: both forms recognized, the last one wins,
        // other arguments untouched
        let t = Tool { args: vec!["--settings=/a/first.json".into(), "-Q".into()], ..Default::default() };
        let mut sess = s("c-1");
        sess.args = vec!["--settings".into(), "mine.json".into(), "--model".into(), "haiku".into()];
        let mut extra = crate::agent::extra(&t, &sess);
        assert_eq!(take_settings(&mut extra).as_deref(), Some("mine.json"));
        assert_eq!(extra, ["-Q", "--model", "haiku"]);
        // Only the merged one is left in the command
        assert_eq!(
            plan(&t, &sess, &How::Fresh, "/st/merged.json", None, None).join(" "),
            "claude --session-id c-1 --settings /st/merged.json -Q --model haiku"
        );

        // Your settings kept as-is; under the same event your hook comes first and aca's after
        let mine = r#"{"env":{"ANTHROPIC_BASE_URL":"https://x"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#;
        let all: Value = serde_json::from_str(&merge(&settings("/bin/aca"), mine, "/nowhere").unwrap()).unwrap();
        assert_eq!(all["env"]["ANTHROPIC_BASE_URL"], "https://x");
        let stop = all["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(
            (stop.len(), stop[0]["hooks"][0]["command"].as_str(), stop[1]["hooks"][0]["command"].as_str()),
            (2, Some("say done"), Some("'/bin/aca' hook claude"))
        );
        // The events the state needs, plus the doorbell
        assert_eq!(all["hooks"].as_object().unwrap().len(), EVENTS.len() + 1);

        // Given as a file: relative paths resolve against the session's directory
        let dir = std::env::temp_dir().join(format!("aca-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.json"), r#"{"model":"opus"}"#).unwrap();
        let all: Value = serde_json::from_str(&merge(&settings("/bin/aca"), "mine.json", &dir.to_string_lossy()).unwrap()).unwrap();
        assert_eq!((all["model"].as_str(), all["hooks"]["Stop"].as_array().unwrap().len()), (Some("opus"), 1));
        // Unreadable, not JSON: say so before starting, rather than going in with a broken one
        assert!(merge("{}", "nope.json", &dir.to_string_lossy()).unwrap_err().contains("nope.json"));
        std::fs::write(dir.join("bad.json"), "hooks: []").unwrap();
        assert!(merge("{}", "bad.json", &dir.to_string_lossy()).unwrap_err().contains("JSON"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_settings_file_only_hooks_what_status_and_the_doorbell_need() {
        let v: Value = serde_json::from_str(&settings("/a b/it's/aca")).unwrap();
        let h = v["hooks"].as_object().unwrap();
        assert_eq!(h.len(), EVENTS.len() + 1);
        assert_eq!(h["Stop"][0]["hooks"][0]["command"], r#"'/a b/it'\''s/aca' hook claude"#);
        assert_eq!(h["PreToolUse"][0]["matcher"], "AskUserQuestion");
        assert!(h["Stop"][0].get("matcher").is_none());
        // Doorbell: on startup report the file to watch; when it changes fetch the mail in the background and exit 2 to wake it
        let start = h["SessionStart"][0]["hooks"].as_array().unwrap();
        assert_eq!((start.len(), start[1]["command"].as_str()), (3, Some(r#"'/a b/it'\''s/aca' inbox watch"#)));
        // Name: handed to it on startup and on every prompt
        assert_eq!(start[2]["command"], r#"'/a b/it'\''s/aca' hook-name"#);
        assert_eq!(h["UserPromptSubmit"][0]["hooks"][1]["command"], r#"'/a b/it'\''s/aca' hook-name"#);
        let take = &h["FileChanged"][0]["hooks"][0];
        assert_eq!((take["command"].as_str(), take["asyncRewake"].as_bool()), (Some(r#"'/a b/it'\''s/aca' inbox take"#), Some(true)));
        assert!(h["FileChanged"][0].get("matcher").is_none());
    }

    #[test]
    fn resuming_needs_a_transcript_for_the_current_conversation() {
        // Started but never talked to: no conversation record, start fresh with the registered id
        assert_eq!(Claude.resume("c1", &Run::default()), How::Fresh);
        let dir = std::env::temp_dir().join(format!("aca-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("c2.jsonl");
        std::fs::write(&t, "{}\n").unwrap();
        let r = Run { transcript: t.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(Claude.resume("c2", &r), How::Resume("c2".into()));
        // The record belongs to another conversation: don't risk resuming
        assert_eq!(Claude.resume("c3", &r), How::Fresh);
        std::fs::remove_dir_all(&dir).unwrap();
        // A fork never talked to: no record of its own, fork again from the original
        assert_eq!(Claude.resume("c4", &Run { fork_from: "c1".into(), ..Default::default() }), How::Fork("c1".into()));
    }

    #[test]
    fn the_title_is_the_one_on_the_pane_and_only_its_own() {
        assert_eq!(shown_title("✳ 修登录 bug").as_deref(), Some("修登录 bug"));
        // The spinner characters while working: current and older versions
        assert_eq!(shown_title("◐ 修登录").as_deref(), Some("修登录"));
        assert_eq!(shown_title("⠂ 修登录").as_deref(), Some("修登录"));
        // No title yet
        assert_eq!(shown_title("✳ Claude Code"), None);
        assert_eq!(shown_title("✳ "), None);
        // Not written by it: tmux's default host name, one a shell wrote, empty
        assert_eq!(shown_title("my-mac.local"), None);
        assert_eq!(shown_title("~/workspace/app"), None);
        assert_eq!(shown_title("✳修登录"), None);
        assert_eq!(shown_title(""), None);
        // On list refresh only the pane title is checked, the record isn't read
        let stopped = Run { event: "Stop".into(), transcript: "/nonexistent".into(), ..Default::default() };
        assert_eq!(Claude.live_title(&stopped, "✳ 修登录").as_deref(), Some("修登录"));
        assert_eq!(Claude.live_title(&stopped, "my-mac.local"), None);
    }

    #[test]
    fn the_last_reply_is_read_from_the_end_of_the_transcript() {
        let dir = std::env::temp_dir().join(format!("aca-reply-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        let say = |extra: &str, content: &str| {
            format!("{{\"type\":\"assistant\"{extra},\"message\":{{\"model\":\"m\",\"content\":{content}}}}}\n")
        };
        let text = |t: &str| format!("[{{\"type\":\"text\",\"text\":\"{t}\"}}]");
        // The last one with text: skip a later one that only called tools, what a subagent said, and what Claude Code filled in
        let log = [
            say("", &text("先看一下")),
            "{\"type\":\"user\",\"message\":{\"content\":\"\\\"type\\\":\\\"assistant\\\" 这几个字出现在对话里\"}}\n".to_string(),
            say("", &text("跑这条:\\n\\n```sh\\nmake\\n```")),
            say("", "[{\"type\":\"tool_use\",\"name\":\"Bash\"}]"),
            say(",\"isSidechain\":true", &text("子 agent 说的")),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"No response requested.\"}]}}\n".to_string(),
        ]
        .concat();
        std::fs::write(&f, log).unwrap();
        let r = Run { transcript: f.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(Claude.last_reply(&r).as_deref(), Some("跑这条:\n\n```sh\nmake\n```"));
        // No record, no reply in the record yet
        assert_eq!(Claude.last_reply(&Run::default()), None);
        std::fs::write(&f, "{\"type\":\"user\",\"message\":{\"content\":\"你好\"}}\n").unwrap();
        assert_eq!(Claude.last_reply(&r), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_title_on_the_pane_the_transcript_says_and_your_rename_beats_its_own() {
        let dir = std::env::temp_dir().join(format!("aca-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        // Every so often it rewrites both, the one it came up with after
        std::fs::write(
            &f,
            "{\"type\":\"user\",\"message\":{\"content\":\"\\\"type\\\":\\\"ai-title\\\" 这几个字出现在对话里\"}}\n\
             {\"type\":\"ai-title\",\"aiTitle\":\"旧的\",\"sessionId\":\"x\"}\n\
             {\"type\":\"custom-title\",\"customTitle\":\"以前改的\",\"sessionId\":\"x\"}\n\
             {\"type\":\"assistant\"}\n\
             {\"type\":\"custom-title\",\"customTitle\":\"修登录 bug\",\"sessionId\":\"x\"}\n\
             {\"type\":\"ai-title\",\"aiTitle\":\"它自己起的\",\"sessionId\":\"x\"}\n\
             {\"type\":\"assistant\"}\n",
        )
        .unwrap();
        let at = |event: &str| Run { event: event.into(), transcript: f.to_string_lossy().into_owned(), ..Default::default() };
        assert_eq!(Claude.title(&at("Stop"), "").as_deref(), Some("修登录 bug"));
        // If the pane title has one, use it without reading the record
        assert_eq!(Claude.title(&at("Stop"), "✳ 窗格上的").as_deref(), Some("窗格上的"));
        assert_eq!(Claude.title(&at("Stop"), "✳ Claude Code").as_deref(), Some("修登录 bug"));
        // The record isn't read on every event
        assert_eq!(Claude.title(&at("PermissionRequest"), ""), None);
        assert_eq!(Claude.title(&at("PermissionRequest"), "✳ 窗格上的").as_deref(), Some("窗格上的"));
        // Never renamed: the last of the ones it came up with
        std::fs::write(
            &f,
            "{\"type\":\"ai-title\",\"aiTitle\":\"旧的\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"新的\"}\n{\"type\":\"assistant\"}\n",
        )
        .unwrap();
        assert_eq!(Claude.title(&at("Stop"), "").as_deref(), Some("新的"));
        std::fs::write(&f, "{\"type\":\"user\"}\n").unwrap();
        assert_eq!(Claude.title(&at("Stop"), ""), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_running_claude_says_which_conversation_it_is_on() {
        let conf = std::env::temp_dir().join(format!("aca-live-{}", std::process::id()));
        let proj = conf.join("projects/-p-my-app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(conf.join("sessions")).unwrap();
        std::fs::write(proj.join("c-new.jsonl"), "{\"type\":\"ai-title\",\"aiTitle\":\"记录里的标题\"}\n").unwrap();
        let argv: Vec<String> =
            ["claude", "--session-id", "c-old", "--dangerously-skip-permissions", "--model", "opus"].map(String::from).to_vec();
        // Its own record: the current conversation (after /clear, different from the command line's), start directory, title
        std::fs::write(conf.join("sessions/42.json"), r#"{"pid":42,"sessionId":"c-new","cwd":"/p/my.app","name":"修登录"}"#).unwrap();
        let l = live_in(&conf, 42, &argv);
        assert_eq!((l.conv.as_str(), l.title.as_str()), ("c-new", "修登录"));
        assert_eq!(l.transcript, proj.join("c-new.jsonl").to_string_lossy());
        // Arguments carried over, except the two words saying which conversation to resume
        assert_eq!(l.args.as_deref(), Some(&["--dangerously-skip-permissions", "--model", "opus"].map(String::from)[..]));
        // Its made-up placeholder name isn't a title: use the conversation record's
        std::fs::write(
            conf.join("sessions/42.json"),
            r#"{"pid":42,"sessionId":"c-new","cwd":"/p/my.app","name":"my-app-5e","nameSource":"derived"}"#,
        )
        .unwrap();
        assert_eq!(live_in(&conf, 42, &argv).title, "记录里的标题");
        // No name given: use the one it came up with in the conversation record; if the start directory doesn't match the folder
        // name (truncated for being too long), search one by one
        std::fs::write(conf.join("sessions/42.json"), r#"{"pid":42,"sessionId":"c-new","cwd":"/elsewhere"}"#).unwrap();
        let l = live_in(&conf, 42, &argv);
        assert_eq!((l.title.as_str(), l.transcript.is_empty()), ("记录里的标题", false));
        // That file was left by another process (pid reused), or there's none: look at the command line; never talked to means no
        // conversation record
        std::fs::write(conf.join("sessions/43.json"), r#"{"pid":7,"sessionId":"c-new"}"#).unwrap();
        let l = live_in(&conf, 43, &argv);
        assert_eq!((l.conv.as_str(), l.transcript.as_str(), l.title.as_str()), ("c-old", "", ""));
        // Started as a fork: the one on the command line is the old conversation
        let forked: Vec<String> = ["claude", "--resume", "c-old", "--fork-session"].map(String::from).to_vec();
        assert_eq!(live_in(&conf, 44, &forked), Live { args: Some(vec![]), ..Default::default() });
        std::fs::remove_dir_all(&conf).unwrap();
    }

    #[test]
    fn only_switches_and_single_words_are_carried_over() {
        let carried = |words: &[&str]| crate::agent::carry(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>(), not_carried);
        let some = |words: &[&str]| Some(words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        assert_eq!(carried(&["claude"]), some(&[]));
        assert_eq!(
            carried(&["claude", "--resume", "c-1", "--model=opus", "-r=c-2", "--continue", "--verbose"]),
            some(&["--model=opus", "--verbose"])
        );
        // --resume without an id (it pops up a picker itself): drop just that word
        assert_eq!(carried(&["claude", "--resume", "--model", "opus"]), some(&["--model", "opus"]));
        // The hooks aca passed itself aren't carried; your own --settings are (merged with aca's on start)
        let ours = paths::state_dir().join("hooks/claude.json").to_string_lossy().into_owned();
        assert_eq!(carried(&["claude", "--settings", &ours, "--settings", "mine.json"]), some(&["--settings", "mine.json"]));
        assert_eq!(carried(&["claude", &format!("--settings={ours}")]), some(&[]));
        // aca's mod isn't carried either; your own --plugin-dir is
        let mods = paths::state_dir().join("hooks/claude-mod/aca").to_string_lossy().into_owned();
        assert_eq!(carried(&["claude", "--plugin-dir", &mods, "--plugin-dir", "/my/mod"]), some(&["--plugin-dir", "/my/mod"]));
        // Started with a message handed to it: can't tell which are arguments, carry nothing
        assert_eq!(carried(&["claude", "--dangerously-skip-permissions", "修一下 登录"]), None);
        assert_eq!(carried(&["claude", "fix"]), None);
        assert_eq!(carried(&["claude", "--model", "opus", "fix"]), None);
        assert_eq!(carried(&[]), None);
    }

    #[test]
    fn the_process_is_named_claude_or_just_its_version() {
        assert!(Claude.runs_as("claude") && Claude.runs_as("2.1.284"));
        // The desktop app (`Claude`, `Claude Helper`) isn't it
        assert!(!Claude.runs_as("1.2") && !Claude.runs_as("a.b.c") && !Claude.runs_as("Claude") && !Claude.runs_as("Claude Helper"));
        assert_eq!(
            (crate::agent::by_comm("2.1.284"), crate::agent::by_comm("codex"), crate::agent::by_comm("zsh")),
            (Some("claude"), Some("codex"), None)
        );
    }

    #[test]
    fn a_dialog_is_asking_and_its_question_is_the_line_ending_in_a_question_mark() {
        let scr = lines("● 要改 3 个文件\n\n Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel");
        assert_eq!(Claude.screen(&scr), Seen::Ask("Do you want to make this edit to main.rs?".into()));
    }

    #[test]
    fn the_activity_line_means_running_and_gives_the_elapsed_time() {
        assert_eq!(Claude.screen(&lines("● 读文件\n\n✻ Pondering… (1m 12s · ↓ 2.1k tokens)\n\n❯ ")), Seen::Running(72));
        assert_eq!(Claude.screen(&lines("✶ Cooking… (2h 3m 4s)")), Seen::Running(7384));
        assert_eq!(Claude.screen(&lines("● 做完了\n\n❯ ")), Seen::Quiet);
        assert_eq!(Claude.screen(&[]), Seen::Quiet);
    }

    /// The input box and the lines below it (as on 2.1.295); `title` is the title embedded at the right of the rule
    fn with_input(above: &str, title: &str) -> Vec<String> {
        let bar = "─".repeat(40);
        lines(&format!(
            "{above}\n{bar}{title}\n❯ \n{bar}\n  ━━╌╌ 20% │ Opus 5.5 │ aca\n  $56.854 6446m53s\n  ⏵⏵ bypass permissions on (shift+tab to cycle)"
        ))
    }

    #[test]
    fn a_turn_that_just_started_has_no_timer_yet_and_still_counts_as_running() {
        // The first dozen-odd seconds after sending (before the model's first token) there's just this line, with a tip line below
        let tip = "  ⎿  Tip: Run /install-github-app to tag @claude right from your Github issues and PRs";
        assert_eq!(Claude.screen(&with_input(&format!("❯ 接着改\n· Combobulating…\n{tip}"), "")), Seen::Running(0));
        assert_eq!(Claude.screen(&with_input("❯ 接着改\n✢ Nucleating…", " 修登录 ─")), Seen::Running(0));
        // With a timer, same as before
        assert_eq!(Claude.screen(&with_input("· Combobulating… (16s)", "")), Seen::Running(16));
        assert_eq!(Claude.screen(&with_input("✻ Compacting conversation… (1m 5s · esc to interrupt)", "")), Seen::Running(65));
        // The finished line has no ellipsis; lines in text ending in an ellipsis don't start with those characters, or are far
        // from the input box
        assert_eq!(Claude.screen(&with_input("  都改好了。\n✻ Cooked for 13m 36s · done 10:48 AM", "")), Seen::Quiet);
        assert_eq!(Claude.screen(&with_input("⏺ 先这样…\n  - 剩下的回头再说…", "")), Seen::Quiet);
        let far = format!("· 顺手记一笔…\n{}", ["  正文"; 7].join("\n"));
        assert_eq!(Claude.screen(&with_input(&far, "")), Seen::Quiet);
        // No input box (can't recognize this version's layout): don't guess
        assert_eq!(Claude.screen(&lines("· Combobulating…\n❯ ")), Seen::Quiet);
    }

    #[test]
    fn while_it_streams_text_only_the_status_bar_says_it_is_running() {
        // Captured while 2.1.295 streamed text: no spinner line at the bottom, only the status bar below the input box says esc to
        // interrupt
        let bar = "─".repeat(40);
        let streaming = format!(
            "  have emerged across the Red Sea in Yemen.\n  Yemen and the Sufi Connection\n  By the fifteenth century, coffee was being cultivated…\n\
             {bar}\n❯ \n{bar}\n  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · ← 1 agent"
        );
        assert_eq!(Claude.screen(&lines(&streaming)), Seen::Running(0));
        // Finished: the status bar doesn't have that phrase (a shell still running in the background doesn't count)
        let after = format!(
            "⏺ 写完了。\n✻ Worked for 9s · done 2:12 PM · 1 shell still running\n{bar}\n❯ \n{bar}\n  ⏵⏵ auto mode on · 1 shell · ← 1 agent · ↓ to manage"
        );
        assert_eq!(Claude.screen(&lines(&after)), Seen::Quiet);
        // These words appearing in text don't count: only look below the input box
        assert_eq!(Claude.screen(&with_input("⏺ 按 esc to interrupt 可以停", "")), Seen::Quiet);
    }

    /// How 2.1.300 looks while streaming text (captured locally on 2026-10-16, words changed): no spinner line, no esc to
    /// interrupt in the status bar, and the pane title is ✳ too. `text` is the tail of the text, `cost` the part of the status
    /// bar that refreshes on its own, `input` the text typed in the input box
    fn streaming(text: &str, cost: &str, input: &str) -> Vec<String> {
        let bar = "─".repeat(40);
        lines(&format!(
            "⏺ 两条路的差别在于:\n{text}\n{bar} 修登录 ─\n❯ {input}\n{bar}\n  ━╌╌╌╌ 11% │ Opus 5.5 │ aca\n  {cost} 40m41s\n  \
             ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent"
        ))
    }

    #[test]
    fn on_2_1_300_text_coming_out_shows_only_as_the_lines_above_the_input_changing() {
        let a = streaming("  - 第一条", "$1.841", "");
        let b = streaming("  - 第一条\n  - 第二条", "$1.841", "");
        // The screen itself can't tell it's running
        assert_eq!((Claude.screen(&a), Claude.screen(&b)), (Seen::Quiet, Seen::Quiet));
        // The text grew by a line: the fingerprint changed
        assert!(Claude.body(&a).is_some() && Claude.body(&a) != Claude.body(&b));
        // None of these count: the status bar below the input box refreshing (every 10 / 60 seconds even when idle), typing in the
        // input box, extra trailing spaces
        for same in
            [streaming("  - 第一条", "$1.902", ""), streaming("  - 第一条", "$1.841", "接着改"), streaming("  - 第一条   ", "$1.841", "")]
        {
            assert_eq!(Claude.body(&same), Claude.body(&a));
        }
        // A prompt popped up and the input box folded away: can't tell, don't look
        assert_eq!(Claude.body(&lines("Do you want to proceed?\n❯ 1. Yes\n  2. No")), None);
    }

    #[test]
    fn asking_means_the_dialog_replaced_the_input_box() {
        let bar = "─".repeat(40);
        // Permission prompt
        let allow = format!(
            "⏺ Bash(touch hello.txt)\n  ⎿  Waiting…\n{bar}\n Bash command\n touch hello.txt\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel · Tab to amend"
        );
        assert_eq!(Claude.screen(&lines(&allow)), Seen::Ask("Do you want to proceed?".into()));
        // AskUserQuestion: with the cursor on the last option, there's a rule right above it and none below
        let pick = format!(
            "{bar}\n ☐ Favorite\nWhat is your favorite color?\n  1. Red\n  2. Blue\n  3. Type something.\n{bar}\n❯ 4. Chat about this\nEnter to select · ↑/↓ to navigate · Esc to cancel"
        );
        assert_eq!(Claude.screen(&lines(&pick)), Seen::Ask("What is your favorite color?".into()));
        // The input box is still there: these words in a reply or command don't count as asking; running stays running
        assert_eq!(Claude.screen(&with_input("⏺ 改好了。Would you like to run the tests?", "")), Seen::Quiet);
        let cmd = "⏺ Bash(grep -n 'Do you want to|esc to cancel' src/data.rs)\n  ⎿  Running…\n✽ Hashing… (6m 6s · ↓ 28.4k tokens)";
        assert_eq!(Claude.screen(&with_input(cmd, "")), Seen::Running(366));
    }
}
