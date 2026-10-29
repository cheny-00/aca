//! Codex. Hooks are passed at startup with `-c hooks.<event>=…` (its hooks have the same shape as Claude's, so the state
//! handling is shared); the conversation id waits for a hook to report it; it comes up with its title itself and writes it
//! into the pane title (`<title> | <directory>`).
//!
//! Two things to know (tested on 0.159):
//! - **Trust**: Codex asks once at startup about hooks it hasn't seen ("Hooks need review"). After "Trust all" it records
//!   the trust in that `CODEX_HOME`'s config.toml and doesn't ask again for the same hook command — one click per account,
//!   and again whenever aca's program path changes. To skip clicking: `[tools.codex] hook_trust = "bypass"`, which adds
//!   `--dangerously-bypass-hook-trust` at startup (every enabled but unreviewed hook in that run just runs, other plugins'
//!   included).
//! - **Passing `-c` means standalone mode**: Codex doesn't use its shared background server (`codex agents` doesn't show
//!   this session).
//!
//! The old `notify` isn't used: it only has "turn ended", and the call Codex makes internally to come up with a title fires
//! it too, reporting a different conversation id.
//!
//! Messages from other sessions go into its own queue (`codex queue`, since 0.152) rather than being typed into the input
//! box. Tested on 0.159.3: a window started with `-c` (standalone mode) also checks the queue every 10 seconds; idle, it
//! starts a turn; running, it picks up right after this turn ends, leaving your half-typed draft alone; after an Esc
//! interrupt queued ones don't start on their own but wait for the next turn to finish normally; once it has exited,
//! they're sent as soon as it's resumed.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::{Agent, How, Live, tail};
use crate::config::Tool;
use crate::paths::Res;
use crate::runtime::Run;
use crate::screen::Seen;
use crate::store::Session;
use crate::tr;

pub struct Codex;

/// Hooked events. Of tool events only `request_user_input` (it's asking you; only in Plan mode; the same shape as Claude's
/// AskUserQuestion), and matcher is a regex: not all of them, as that would start one more process for every command run.
/// `request_user_input_async` isn't hooked: it returns right after asking and carries on, the question stays on screen,
/// and reading the screen catches it
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("Stop", None),
    ("Interrupt", None),
    ("PermissionRequest", None),
    ("PreCompact", None),
    ("PostCompact", None),
    ("SessionEnd", None),
    ("PreToolUse", Some("^request_user_input$")),
    ("PostToolUse", Some("^request_user_input$")),
];

/// The startup gates (update, trusting the folder, reviewing hooks), plus confirmation prompts and questions to you while
/// running
static ASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)enter (?:to )?(?:confirm|continue|submit)|esc to cancel|allow command\?|\[y/n\]").unwrap());
/// "• Working (1m 05s • esc to interrupt)", or when compacting "• Compacting context (4s • esc to interrupt)"
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\((?:(\d+)h )?(?:(\d+)m )?(\d+)s • [^)]*to interrupt\)").unwrap());
/// The line above a question to you, "Question 1/2 (2 unanswered)": the next line is the question
static QUESTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Question \d+/\d+\b").unwrap());
/// While waiting for you (a confirmation prompt, a question to you), the spinner cell at the start of the pane title turns
/// into a blinking "[ ! ] Action Required" / "[ . ] Action Required" (0.163)
static ACTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[ \S \] Action Required(?: \| |$)").unwrap());
const SPINNER: &str = "⠁⠂⠄⡀⢀⠠⠐⠈⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// One event's `-c` value: one hook written in TOML, `hooks.Stop=[{ hooks = [{ type = "command", command = "…" }] }]`,
/// with an extra `matcher` for tool events
fn hook_flag(event: &str, matcher: Option<&str>, aca: &str) -> String {
    let mut handler = toml::Table::new();
    handler.insert("type".into(), "command".into());
    handler.insert("command".into(), format!("{} hook codex", super::sh_quote(aca)).into());
    let mut group = toml::Table::new();
    if let Some(m) = matcher {
        group.insert("matcher".into(), m.into());
    }
    group.insert("hooks".into(), toml::Value::Array(vec![handler.into()]));
    format!("hooks.{event}={}", toml::Value::Array(vec![group.into()]))
}

fn plan(tool: &Tool, s: &Session, how: &How, aca: &str) -> Vec<String> {
    let mut argv = super::base(tool, "codex");
    match how {
        How::Fresh => {}
        How::Resume(conv) => argv.extend(["resume".to_string(), conv.clone()]),
        How::Fork(from) => argv.extend(["fork".to_string(), from.clone()]),
    }
    for (ev, matcher) in EVENTS {
        argv.extend(["-c".to_string(), hook_flag(ev, *matcher, aca)]);
    }
    if tool.hook_trust == "bypass" {
        argv.push("--dangerously-bypass-hook-trust".into());
    }
    // What enabled plugins give it: the plugin copies aca prepared (MCP servers included); only MCP servers that can't be
    // copied go through -c mcp_servers (Codex gives MCP child processes an allowlisted environment only, so the session id is
    // written into env)
    let mut env =
        vec![("ACA_SESSION_ID".to_string(), s.id.clone()), ("ACA_TOOL".into(), s.tool.clone()), ("ACA_BIN".into(), aca.to_string())];
    for k in ["ACA_HOME", "ACA_TMUX_SOCKET"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v));
        }
    }
    // The commands it runs need these too (`aca msg --in` and `aca new` find the session by them): a config that inherits only a
    // few basic variables, like `shell_environment_policy.inherit = "core"`, won't pass them on from the launch environment, so
    // set them directly (verified 2026-10-29)
    for (k, v) in &env {
        argv.extend(["-c".to_string(), format!("shell_environment_policy.set.{k}={}", toml::Value::String(v.clone()))]);
    }
    for (name, args) in &tool.mcp {
        argv.extend(["-c".to_string(), crate::plugin::mount::codex_mcp_flag(name, aca, args, &env)]);
    }
    for p in &tool.codex_plugins {
        argv.extend(["-c".to_string(), format!("plugins.{p}@aca.enabled=true")]);
    }
    argv.extend(super::extra(tool, s));
    argv
}

/// Whether this value after `-c` was passed by aca (hooks, plugins, plugins' MCP servers, the session variables for commands)
fn ours(v: &str) -> bool {
    v.starts_with("hooks.")
        || (v.starts_with("plugins.") && v.contains("@aca."))
        || v.starts_with("mcp_servers.aca")
        || v.starts_with("shell_environment_policy.set.ACA_")
}

/// Not carried over when adopting: a leading subcommand saying which conversation to resume (`resume <id>` / `fork <id>`),
/// plus the hooks aca passed itself and the trust-skipping flag (rebuilt from the config on start). `first` = this is the
/// first word after the program name
fn not_carried(rest: &[String], first: bool) -> usize {
    match rest[0].as_str() {
        "resume" | "fork" if first => 1 + usize::from(rest.get(1).is_some_and(|v| !v.starts_with('-'))),
        "--dangerously-bypass-hook-trust" => 1,
        // Flags only resume / fork accept: the subcommand isn't carried, so these can't stay either
        "--last" | "--all" => 1,
        "-c" | "--config" if rest.get(1).is_some_and(|v| ours(v)) => 2,
        _ => 0,
    }
}

const MODEL: &[&str] = &["-m", "--model"];

/// The file named `rollout-…<tail>` within `depth` levels under `dir`
fn rollout_in(dir: &Path, tail: &str, depth: u32) -> Option<PathBuf> {
    let mut dirs = vec![];
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let kind = e.file_type().ok()?;
        if kind.is_file() && name.starts_with("rollout-") && name.ends_with(tail) {
            return Some(e.path());
        }
        if kind.is_dir() && depth > 1 {
            dirs.push(e.path());
        }
    }
    // Newer days come later: search backwards, a conversation just talked to is most likely in the latest directory
    dirs.sort();
    dirs.iter().rev().find_map(|d| rollout_in(d, tail, depth - 1))
}

/// The model used by the record's last turn (recorded in `turn_context`)
fn last_model(log: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(log).ok()?;
    let mut model = None;
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        if !line.contains("\"turn_context\"") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if v["type"] == "turn_context"
            && let Some(m) = v["payload"]["model"].as_str().filter(|m| !m.is_empty())
        {
            model = Some(m.to_string());
        }
    }
    model
}

/// What an account uses by default: config.toml's `model` and `model_provider` (openai if unset); with a profile selected
/// (`--profile` / `-p` in the arguments, or `profile` in config.toml), `[profiles.<it>]` is checked first
struct Defaults {
    model: Option<String>,
    provider: String,
}

fn defaults(home: &Path, args: &[String]) -> Defaults {
    let t: toml::Table = std::fs::read_to_string(home.join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let named = args.iter().position(|a| a == "--profile" || a == "-p").and_then(|i| args.get(i + 1)).cloned();
    let profile = named.or_else(|| t.get("profile").and_then(|v| v.as_str()).map(String::from));
    let section = profile.and_then(|p| t.get("profiles")?.get(&p)?.as_table().cloned()).unwrap_or_default();
    let get = |k: &str| section.get(k).or_else(|| t.get(k)).and_then(|v| v.as_str()).map(String::from);
    Defaults { model: get("model"), provider: get("model_provider").unwrap_or_else(|| "openai".into()) }
}

/// If the record's last turn has already ended: the event to fill in for it, with the end time `at`. Still running or
/// interrupted is None.
/// - Ended with `error`: `StopFailure` (the raw text and its kind)
/// - Ended normally: `Stop` (the last reply). Not filled in without an end time: can't tell it isn't the previous turn's
///
/// `since` = when the hook recorded this turn starting: anything that ended earlier is the previous turn's (the new turn
/// just started and isn't in the record yet), and doesn't count
/// The reply that comes with the record's last finished turn (`last_agent_message` of `task_complete`)
fn last_reply(text: &str) -> Option<String> {
    text.lines().rev().filter(|l| l.contains("\"task_complete\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        let reply = v["payload"]["last_agent_message"].as_str()?;
        (!reply.trim().is_empty()).then(|| reply.to_string())
    })
}

fn last_turn_end(text: &str, since: u64) -> Option<serde_json::Value> {
    let ends = |l: &&str| ["\"task_started\"", "\"task_complete\"", "\"turn_aborted\""].iter().any(|k| l.contains(k));
    let last: serde_json::Value = text.lines().rev().filter(ends).find_map(|l| serde_json::from_str(l).ok())?;
    let p = &last["payload"];
    let at = p["completed_at"].as_u64();
    if p["type"] != "task_complete" || at.is_some_and(|t| t < since) {
        return None;
    }
    if p["error"].is_null() {
        let reply = p["last_agent_message"].as_str().unwrap_or("");
        return Some(serde_json::json!({ "hook_event_name": "Stop", "last_assistant_message": reply, "at": at? }));
    }
    let message = p["error"]["message"].as_str().filter(|m| !m.trim().is_empty()).unwrap_or("本轮出错");
    // Kind: without data it's a string ("server_overloaded"), with data an object with a single key
    // ({"http_connection_failed": {…}})
    let info = &p["error"]["codex_error_info"];
    let key = info.as_str().map(str::to_string).or_else(|| info.as_object()?.keys().next().cloned()).unwrap_or_default();
    // The connection and stream error kinds carry an HTTP status code: go by the code (a misspelled model name is 404, and
    // retrying won't change that)
    let status = info[key.as_str()]["http_status_code"].as_u64();
    let kind = match (key.as_str(), status) {
        (_, Some(401 | 403)) => "account",
        (_, Some(408 | 429)) => "transient",
        (_, Some(400..=499)) => "fatal",
        (
            "rate_limit_exceeded"
            | "server_overloaded"
            | "flex_unavailable"
            | "http_connection_failed"
            | "response_stream_connection_failed"
            | "response_stream_disconnected"
            | "response_too_many_failed_attempts"
            | "internal_server_error",
            _,
        ) => "transient",
        ("usage_limit_exceeded" | "unauthorized" | "session_budget_exceeded", _) => "account",
        ("" | "other", _) => super::lifecycle::error_kind(message),
        _ => "fatal",
    };
    Some(serde_json::json!({ "hook_event_name": "StopFailure", "error": message, "error_kind": kind, "codex_error": key, "at": at }))
}

/// Which conversation a running Codex is on now: (conversation id, record, title). It leaves no "which conversation this
/// process is on" file, but its log database (`logs_2.sqlite`, in config.toml's `sqlite_home`, the config directory if
/// unset) records the process (`pid:<pid>:<uuid>`) and thread on every row. Of the threads this process logged most
/// recently, the first that's a real conversation: it has a write lock (`thread-writer-locks/<id>.lock`; the thread of its
/// internal title call doesn't), and isn't a subthread it spawned (`source` is a JSON snippet in threads of the state
/// database `state_5.sqlite`). The title and record location also come from threads (`name` is the title it came up with,
/// `rollout_path`); after `/new` a conversation not talked to yet isn't in threads, only its id. Tested on 0.163
fn live_in(home: &Path, pid: u32) -> Option<(String, String, String)> {
    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    let t: toml::Table = std::fs::read_to_string(home.join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let dir = t.get("sqlite_home").and_then(|v| v.as_str()).map_or_else(|| home.to_path_buf(), |d| PathBuf::from(crate::paths::expand(d)));
    let open = |name: &str| -> Option<Connection> {
        let db = dir.join(name);
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        db.is_file().then(|| Connection::open_with_flags(format!("file:{}?mode=ro", db.display()), flags).ok()).flatten()
    };
    let logs = open("logs_2.sqlite")?;
    // Log ids only grow: the most recently written come first. The pid is compared as a range (`pid:12:` to `pid:12;`):
    // `like 'pid:1%'` would hit `pid:12`
    let (lo, hi) = (format!("pid:{pid}:"), format!("pid:{pid};"));
    let mut st = logs
        .prepare(
            "select thread_id from logs where process_uuid >= ?1 and process_uuid < ?2 and thread_id is not null \
             group by thread_id order by max(id) desc limit 20",
        )
        .ok()?;
    let threads: Vec<String> = st.query_map([&lo, &hi], |r| r.get(0)).ok()?.flatten().collect();
    let state = open("state_5.sqlite");
    let row = |id: &str| -> Option<(String, String, String)> {
        let q = "select source, coalesce(name, ''), rollout_path from threads where id = ?1";
        state.as_ref()?.query_row(q, [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().ok().flatten()
    };
    threads.into_iter().filter(|id| !id.contains('/') && home.join("thread-writer-locks").join(format!("{id}.lock")).is_file()).find_map(
        |id| match row(&id) {
            Some((source, _, _)) if source.trim_start().starts_with('{') => None,
            Some((_, name, rollout)) => Some((id, rollout, name)),
            None => Some((id, String::new(), String::new())),
        },
    )
}

/// Pane title `⠴ 修登录 | api` → `修登录` ("fix login"); with no title yet it's only the directory name, which doesn't count.
/// While waiting for you it's `[ ! ] Action Required | 修登录 | api`: that part is the spinner cell, not the title
fn pane_title(title: &str) -> Option<String> {
    let t = ACTION.replace(title.trim_start(), "");
    // While working a spinner character comes first, and before the title is ready that spot holds one too (`⠸ ⠸ | api`)
    let t = t.trim_start_matches(|c: char| c.is_whitespace() || SPINNER.contains(c));
    let (name, _dir) = t.rsplit_once(" | ")?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

impl Agent for Codex {
    fn name(&self) -> &str {
        "codex"
    }

    fn home_var(&self) -> Option<&str> {
        Some("CODEX_HOME")
    }

    fn default_home(&self) -> Option<&str> {
        Some("~/.codex")
    }

    /// The conversation record `sessions/<year>/<month>/<day>/rollout-<time>-<id>.jsonl`
    fn conv_files(&self, home: &Path, conv: &str) -> Vec<PathBuf> {
        if conv.is_empty() || conv.contains('/') {
            return vec![];
        }
        let tail = format!("-{conv}.jsonl");
        let found = rollout_in(&home.join("sessions"), &tail, 4);
        found.and_then(|f| f.strip_prefix(home).ok().map(Path::to_path_buf)).into_iter().collect()
    }

    /// Each account's config.toml has a different default model (true on this machine): if the model the conversation used
    /// isn't the new account's default, pass `-m` to keep using it. The old `-m` in the session's arguments is removed first,
    /// then we decide whether to pass one (not needed when switching back to the original account). Left alone when the tool
    /// or new account fixes the model; also left alone when the two accounts use different services (`model_provider`): the
    /// model follows the new account, with a note
    fn rehome_args(&self, args: &[String], log: &Path, from: &Path, to: &Path, fixed: &[String]) -> (Vec<String>, String) {
        let keep = || (args.to_vec(), String::new());
        let Some(was) = last_model(log) else { return keep() };
        if fixed.iter().enumerate().any(|(i, _)| super::valued(&fixed[i..], MODEL) > 0) {
            return keep();
        }
        let (old, new) = (defaults(from, &[]), defaults(to, fixed));
        if old.provider != new.provider {
            let now = new.model.unwrap_or_default();
            return (
                args.to_vec(),
                tr!(
                    "两个账号使用的服务不同({} → {}),模型改用新账号的 {now}(原为 {was})",
                    "the accounts use different providers ({} → {}); the model is the new account's {now} (was {was})",
                    old.provider,
                    new.provider
                ),
            );
        }
        let mut out = vec![];
        let mut rest = args;
        while let Some(w) = rest.first() {
            let n = super::valued(rest, MODEL);
            if n == 0 {
                out.push(w.clone());
            }
            rest = &rest[n.max(1).min(rest.len())..];
        }
        if new.model.as_deref() == Some(was.as_str()) {
            return (out, String::new());
        }
        let note = match &new.model {
            Some(m) => tr!("已加上 -m {was}(新账号默认为 {m})", "with -m {was} (the new account defaults to {m})"),
            None => tr!("已加上 -m {was}", "with -m {was}"),
        };
        out.extend(["-m".to_string(), was]);
        (out, note)
    }

    fn can_fork(&self) -> bool {
        true
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>> {
        Ok(plan(tool, s, how, aca))
    }

    /// Resume if the conversation id is known (the hook only reports the id once the first message is sent; a session never
    /// talked to starts fresh)
    fn resume(&self, conv: &str, _r: &Run) -> How {
        if conv.is_empty() { How::Fresh } else { How::Resume(conv.to_string()) }
    }

    fn title_in_pane(&self) -> bool {
        true
    }

    fn title(&self, _r: &Run, title: &str) -> Option<String> {
        pane_title(title)
    }

    /// A running Codex leaves no "which conversation I'm on now" file: read its log database (see [`live_in`]), which follows
    /// it after `/new` too. When the log database can't tell (older versions, no database), only what the command line says
    /// can be recognized (`codex resume <id>`)
    fn live(&self, pid: u32, home: Option<&str>, argv: &[String]) -> Live {
        let args = super::carry(argv, |rest| not_carried(rest, rest.len() + 1 == argv.len()));
        let conf = home.map_or_else(|| PathBuf::from(crate::paths::expand("~/.codex")), PathBuf::from);
        if let Some((conv, transcript, title)) = live_in(&conf, pid) {
            let transcript = match transcript.is_empty() {
                true => self.conv_files(&conf, &conv).first().map(|f| conf.join(f).to_string_lossy().into_owned()).unwrap_or_default(),
                false => transcript,
            };
            return Live { conv, transcript, title, args };
        }
        let id = argv.get(2).filter(|v| !v.starts_with('-'));
        let conv = match argv.get(1).map(String::as_str) {
            Some("resume") => id.cloned().unwrap_or_default(),
            _ => String::new(),
        };
        Live { conv, args, ..Default::default() }
    }

    fn delivery(&self) -> super::Delivery {
        super::Delivery::Queue
    }

    /// Pass a `-c`: then `codex queue` writes the queue within its own process and leaves, rather than starting a long-lived
    /// background service for this account. When that service is already running it refuses (to avoid both writing
    /// separately), so the only way is to drop `-c` and hand it to that service
    fn queue_args(&self, conv: &str, text: &str, shared: bool) -> Vec<String> {
        let mut a: Vec<String> = if shared { vec![] } else { vec!["-c".into(), "notify=[]".into()] };
        a.extend(["queue", "--thread", conv, "--message", text].map(String::from));
        a
    }

    fn prompt_mark(&self) -> Option<char> {
        Some('›')
    }

    /// "› 1. Yes, proceed (y) / 2. Yes, and don't ask again … (p) / 3. No, and tell Codex what to do differently (esc)":
    /// approve with y; deny with Esc, which stops the turn and it reports Interrupt itself (tested on 0.163)
    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        Some(("y", "Escape"))
    }

    /// "esc to interrupt": one press stops it, and it reports Interrupt itself
    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        Some(&["Escape"])
    }

    fn reports_interrupt(&self) -> bool {
        true
    }

    fn opening(&self, text: &str) -> Option<Vec<String>> {
        super::opening_after_dashes(text)
    }

    /// "Folder access … Trust this folder? … › 1. Trust and continue / 2. Quit". The hook review gate ("2. Trust all and
    /// continue") doesn't count: that's `hook_trust`'s business
    fn trust_keys(&self, lines: &[String]) -> Option<Vec<&'static str>> {
        let g = crate::answer::gate(lines)?;
        let yes = g.opts.iter().position(|o| o == "Trust and continue")?;
        (g.opts.len() == 2 && g.opts.iter().any(|o| o == "Quit")).then(|| crate::answer::gate_keys(&g, yes + 1))
    }

    /// Two kinds of ending report no hook, and only leave a `task_complete` in the conversation record:
    /// - A turn errors out (rate limits, overload, a dropped stream, usage limits…, after its own few retries fail): no Stop
    ///   hook, no other hook either (0.162 source: a sampling error breaks straight out of the turn); that entry carries `error`
    /// - It finishes normally but the Stop hook isn't trusted: it doesn't run untrusted hooks, and doesn't say so. Trust is
    ///   recorded per hook, so an account can have UserPromptSubmit trusted and Stop not (seen on 0.162.1): every turn only
    ///   reports its start, never its end. The Stop hook is only run after `task_complete` is written, so if the record has it
    ///   and the hook still says running, the hook didn't run
    fn unreported_end(&self, r: &Run) -> Option<serde_json::Value> {
        last_turn_end(&tail(&r.transcript, 256 * 1024)?, r.turn)
    }

    fn last_reply(&self, r: &Run) -> Option<String> {
        last_reply(&tail(&r.transcript, 1024 * 1024)?)
    }

    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(14)..];
        if tail.iter().any(|l| ASK.is_match(l)) {
            // The question asked: for a question to you it's the line under "Question 1/1" (not necessarily ending in a question
            // mark); otherwise the one ending in a question mark; the startup gates have none, so take the first non-empty line in the
            // box (when logged out that screen has a braille-dot logo on top, whose last few lines fall into tail)
            let rows: Vec<&str> = tail.iter().map(|l| l.trim()).collect();
            let q = rows
                .iter()
                .rposition(|l| QUESTION.is_match(l))
                .and_then(|i| rows.get(i + 1).copied())
                .or_else(|| rows.iter().copied().find(|l| l.ends_with('?') || l.ends_with('？')))
                .or_else(|| rows.iter().copied().find(|l| l.chars().any(char::is_alphanumeric)))
                .unwrap_or("");
            return Seen::Ask(q.to_string());
        }
        for l in tail.iter().rev() {
            if let Some(c) = RUNNING.captures(l) {
                let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
                return Seen::Running(n(1) * 3600 + n(2) * 60 + n(3));
            }
        }
        Seen::Quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn switching_accounts_keeps_the_model_unless_the_new_one_pins_it_or_uses_another_provider() {
        let d = std::env::temp_dir().join(format!("aca-rehome-{}", std::process::id()));
        let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
        for (dir, toml) in [
            (&a, "model = \"m-a\"\n"),
            (&b, "model = \"m-b\"\nprofile = \"fast\"\n[profiles.fast]\nmodel = \"m-fast\"\n"),
            (&c, "model = \"m-a\"\nmodel_provider = \"relay\"\n"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("config.toml"), toml).unwrap();
        }
        let log = d.join("rollout.jsonl");
        std::fs::write(
            &log,
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"m-x\"}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"m-a\"}}\n",
        )
        .unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // b selects a profile: the default is the profile's m-fast, not the last turn's m-a, so pass it; the old -m is removed first
        let (args, note) = Codex.rehome_args(&s(&["-m", "old", "--yolo"]), &log, &a, &b, &[]);
        assert_eq!((args, note.contains("m-fast")), (s(&["--yolo", "-m", "m-a"]), true));
        // The arguments name another profile: use it (no such profile means the top-level m-b)
        assert_eq!(Codex.rehome_args(&[], &log, &a, &b, &s(&["--profile", "none"])).0, s(&["-m", "m-a"]));
        // Back to the account whose default is m-a: nothing to pass
        assert_eq!(Codex.rehome_args(&s(&["--model=m-a"]), &log, &b, &a, &[]), (vec![], String::new()));
        // The new account fixes the model: left alone
        assert_eq!(Codex.rehome_args(&s(&["--yolo"]), &log, &a, &b, &s(&["-m", "pinned"])).0, s(&["--yolo"]));
        // Not the same service: the model follows the new account, with a note
        let (args, note) = Codex.rehome_args(&s(&["-m", "m-a"]), &log, &a, &c, &[]);
        assert_eq!(args, s(&["-m", "m-a"]));
        assert!(note.contains("openai → relay"), "{note}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_rollout_is_found_by_its_thread_id_under_sessions() {
        let d = std::env::temp_dir().join(format!("aca-rollouts-{}", std::process::id()));
        let f = d.join("sessions/2026/10/14/rollout-2026-10-14T09-00-00-th-1.jsonl");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "").unwrap();
        std::fs::write(f.with_file_name("rollout-2026-10-14T09-00-00-th-10.jsonl"), "").unwrap();
        assert_eq!(Codex.conv_files(&d, "th-1"), vec![PathBuf::from("sessions/2026/10/14/rollout-2026-10-14T09-00-00-th-1.jsonl")]);
        assert!(Codex.conv_files(&d, "th-2").is_empty() && Codex.conv_files(&d, "../x").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_folder_trust_question_is_answered_but_the_hook_review_is_not() {
        // Copied from 0.162
        let ask = "  Folder access\n\n  /p/api\n\n  Trust this folder? Codex can read, edit, and run files here, subject to your\n  permission settings. Folder settings can run code automatically, even\n  without a model request. Continue only if you trust these files. Your trust\n  decision will be saved.\n\n› 1. Trust and continue\n  2. Quit\n\n  enter continue · esc quit\n";
        assert_eq!(Codex.trust_keys(&lines(ask)), Some(vec!["Enter"]));
        // The cursor was moved to Quit: move it back
        let moved = ask.replace("› 1. Trust and continue\n  2. Quit", "  1. Trust and continue\n› 2. Quit");
        assert_eq!(Codex.trust_keys(&lines(&moved)), Some(vec!["Up", "Enter"]));
        // The hook review gate is hook_trust's business; neither a confirmation prompt while running nor the usual input box is it
        let review = "  Hooks need review\n  6 hooks are new or changed.\n  Hooks can run outside the sandbox after you trust them.\n› 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n  enter confirm · esc skip";
        assert_eq!(Codex.trust_keys(&lines(review)), None);
        assert_eq!(Codex.trust_keys(&lines("  Allow command?\n› 1. Yes\n  2. No\n")), None);
        assert_eq!(Codex.trust_keys(&lines("• 做完了\n\n› Ask Codex to do anything\n")), None);
    }

    #[test]
    fn hooks_ride_along_as_config_overrides() {
        let flag = hook_flag("Stop", None, "/a b/aca");
        let (key, value) = flag.split_once('=').unwrap();
        assert_eq!(key, "hooks.Stop");
        // The value must be TOML Codex can read: an array with one group, holding one command
        let v: toml::Value = toml::from_str(&format!("x = {value}")).unwrap();
        let h = &v["x"][0]["hooks"][0];
        assert_eq!((h["type"].as_str(), h["command"].as_str()), (Some("command"), Some("'/a b/aca' hook codex")));
        assert!(v["x"][0].get("matcher").is_none());
        // Of tool events only the one that asks you (matcher is a regex; tested on 0.163, running Bash doesn't trigger it)
        let (key, value) = EVENTS
            .iter()
            .find(|e| e.0 == "PreToolUse")
            .map(|e| hook_flag(e.0, e.1, "aca"))
            .unwrap()
            .split_once('=')
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .unwrap();
        let v: toml::Value = toml::from_str(&format!("x = {value}")).unwrap();
        assert_eq!((key.as_str(), v["x"][0]["matcher"].as_str()), ("hooks.PreToolUse", Some("^request_user_input$")));
        assert!(EVENTS.iter().filter(|e| e.0.ends_with("ToolUse")).all(|e| e.1.is_some()));
        assert!(EVENTS.iter().any(|e| e.0 == "PreCompact") && EVENTS.iter().any(|e| e.0 == "PostCompact"));
    }

    #[test]
    fn a_permission_is_granted_with_y_and_refused_with_escape() {
        assert_eq!(Codex.permission_keys(), Some(("y", "Escape")));
    }

    #[test]
    fn it_resumes_by_thread_id_and_only_bypasses_trust_when_told_to() {
        let s = Session { id: "s1".into(), args: vec!["--search".into()], ..Default::default() };
        let argv = plan(&Tool::default(), &s, &How::Resume("th-1".into()), "aca");
        assert_eq!(argv[..3], ["codex", "resume", "th-1"]);
        assert_eq!(argv.iter().filter(|a| a.starts_with("hooks.")).count(), EVENTS.len());
        // The commands it runs get the session id too, whatever variables the config inherits
        assert!(argv.iter().any(|a| a == "shell_environment_policy.set.ACA_SESSION_ID=\"s1\""), "{argv:?}");
        assert_eq!(argv.last().unwrap(), "--search");
        assert!(!argv.iter().any(|a| a.contains("bypass")));
        let t = Tool { cmd: vec!["/w/wrapper.sh".into()], hook_trust: "bypass".into(), ..Default::default() };
        let argv = plan(&t, &Session::default(), &How::Fork("th-1".into()), "aca");
        assert_eq!(argv[..3], ["/w/wrapper.sh", "fork", "th-1"]);
        assert!(argv.iter().any(|a| a == "--dangerously-bypass-hook-trust"));
        assert_eq!(Codex.resume("", &Run::default()), How::Fresh);
        assert_eq!(Codex.resume("th", &Run::default()), How::Resume("th".into()));
    }

    #[test]
    fn a_turn_that_ended_without_a_hook_is_read_from_the_end_of_its_rollout() {
        // What 0.162 records look like: a turn starts with task_started and ends with task_complete (one with an error has an
        // extra error), an Esc interrupt is turn_aborted
        let started = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t2"}}"#;
        let done = |extra: &str| {
            format!(r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t2","completed_at":200{extra}}}}}"#)
        };
        let usage = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex"}}}"#;
        let log = |lines: &[&str]| lines.join("\n");
        // Still running, interrupted: not ended
        assert_eq!(last_turn_end(&log(&[started, usage]), 100), None);
        assert_eq!(last_turn_end(&log(&[started, r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}"#]), 100), None);
        // Ended normally (it doesn't report when the Stop hook isn't trusted): fill in a Stop with the last reply and the end time
        let ok = done(r#","last_agent_message":"好了","error":null"#);
        let v = last_turn_end(&log(&[started, &ok, usage]), 100).unwrap();
        assert_eq!(
            (v["hook_event_name"].as_str(), v["last_assistant_message"].as_str(), v["at"].as_u64()),
            (Some("Stop"), Some("好了"), Some(200))
        );
        // The "last reply" given to plugins is read here too: from the last finished turn; a failed turn has no reply, so look
        // further back
        assert_eq!(last_reply(&log(&[started, &ok, usage])).as_deref(), Some("好了"));
        let failed = done(r#","error":{"message":"x","codex_error_info":"server_overloaded"}"#);
        assert_eq!(last_reply(&log(&[&ok, started, &failed])).as_deref(), Some("好了"));
        assert_eq!(last_reply(&log(&[started, usage])), None);
        // The previous turn's (the new turn just started and isn't in the record yet), or one with no end time (can't tell which
        // turn): not filled in
        assert_eq!(last_turn_end(&ok, 300), None);
        assert_eq!(
            last_turn_end(r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t2","last_agent_message":"好了"}}"#, 100),
            None
        );
        // Ended in an error: a kind without data is a string, one with data an object with a single key
        let overloaded = done(r#","error":{"message":"Our servers are currently overloaded.","codex_error_info":"server_overloaded"}"#);
        let v = last_turn_end(&log(&[started, &overloaded, usage]), 100).unwrap();
        assert_eq!(
            (v["hook_event_name"].as_str(), v["error"].as_str(), v["error_kind"].as_str(), v["at"].as_u64()),
            (Some("StopFailure"), Some("Our servers are currently overloaded."), Some("transient"), Some(200))
        );
        let http = done(r#","error":{"message":"bad gateway","codex_error_info":{"http_connection_failed":{"http_status_code":502}}}"#);
        assert_eq!(last_turn_end(&http, 100).unwrap()["error_kind"], "transient");
        // 4xx: go by the code: a misspelled model name (tested on 0.162: http_connection_failed with 404) won't change on retry,
        // 401 is the account's problem
        let missing = done(
            r#","error":{"message":"unexpected status 404 Not Found: Model \"x\" is not supported","codex_error_info":{"http_connection_failed":{"http_status_code":404}}}"#,
        );
        assert_eq!(last_turn_end(&missing, 100).unwrap()["error_kind"], "fatal");
        let denied = done(r#","error":{"message":"no","codex_error_info":{"http_connection_failed":{"http_status_code":401}}}"#);
        assert_eq!(last_turn_end(&denied, 100).unwrap()["error_kind"], "account");
        let limit = done(r#","error":{"message":"You've hit your usage limit.","codex_error_info":"usage_limit_exceeded"}"#);
        assert_eq!(last_turn_end(&limit, 100).unwrap()["error_kind"], "account");
        let window = done(r#","error":{"message":"context window exceeded","codex_error_info":"context_window_exceeded"}"#);
        assert_eq!(last_turn_end(&window, 100).unwrap()["error_kind"], "fatal");
        // Unrecognized kind: look at the raw text
        let odd = done(r#","error":{"message":"429 Too Many Requests","codex_error_info":"other"}"#);
        assert_eq!(last_turn_end(&odd, 100).unwrap()["error_kind"], "transient");
        // Ended before this turn started: it's the previous turn's (the new turn just started and isn't in the record yet)
        assert_eq!(last_turn_end(&overloaded, 300), None);
        // It reads the end of the file: starting at the next whole line, half lines don't count
        let path = std::env::temp_dir().join(format!("aca-codex-tail-{}.jsonl", std::process::id()));
        std::fs::write(&path, format!("{}\n{started}\n{overloaded}\n", "x".repeat(50))).unwrap();
        let t = tail(&path.to_string_lossy(), (started.len() + overloaded.len() + 10) as u64).unwrap();
        assert!(t.starts_with(started) && last_turn_end(&t, 100).is_some(), "{t}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_running_codex_only_shows_its_thread_when_it_was_resumed_by_id() {
        // Give it a config directory that doesn't exist: don't touch the real ~/.codex log database
        let live = |words: &[&str]| Codex.live(1, Some("/nonexistent/aca-test"), &words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        let some = |words: &[&str]| Some(words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        // Started fresh by hand: no idea which conversation; arguments carried over
        assert_eq!(
            live(&["codex", "--yolo", "--profile", "labs"]),
            Live { args: some(&["--yolo", "--profile", "labs"]), ..Default::default() }
        );
        let l = live(&["codex", "resume", "th-1", "--yolo"]);
        assert_eq!((l.conv.as_str(), l.args), ("th-1", some(&["--yolo"])));
        // From resume --last or fork: the current conversation's id isn't on the command line
        assert_eq!(live(&["codex", "resume", "--last", "--yolo"]), Live { args: some(&["--yolo"]), ..Default::default() });
        assert_eq!(live(&["codex", "fork", "th-1"]).conv, "");
        // The hooks aca passed itself and the trust-skipping flag aren't carried; other -c are
        let l = live(&[
            "codex",
            "-c",
            "hooks.Stop=[{}]",
            "-c",
            "shell_environment_policy.set.ACA_SESSION_ID=\"s1\"",
            "-c",
            "model=o3",
            "--dangerously-bypass-hook-trust",
            "--search",
        ]);
        assert_eq!(l.args, some(&["-c", "model=o3", "--search"]));
        // Started with a message handed to it: carry nothing
        assert_eq!(live(&["codex", "修一下登录"]).args, None);
        assert_eq!(live(&[]).args, None);
        // A resume that isn't first is another flag's value, not a subcommand
        assert_eq!(live(&["codex", "--profile", "resume"]).args, some(&["--profile", "resume"]));
    }

    #[test]
    fn its_own_title_is_the_part_of_the_pane_title_before_the_folder() {
        assert_eq!(pane_title("⠴ Reply with ok | api").as_deref(), Some("Reply with ok"));
        assert_eq!(pane_title("修登录 | a | b | api").as_deref(), Some("修登录 | a | b"));
        assert_eq!(pane_title("api"), None);
        assert_eq!(pane_title("⠸ | api"), None);
        assert_eq!(pane_title("⠸ ⠸ | api"), None);
        assert_eq!(pane_title(""), None);
        // While waiting for you, 0.163 turns the spinner cell into a blinking phrase, which isn't the title (it used to be taken
        // as the title, and the tmux session name was changed to it too)
        assert_eq!(
            pane_title("[ . ] Action Required | Ask to choose option A or B | proj").as_deref(),
            Some("Ask to choose option A or B")
        );
        assert_eq!(pane_title("[ ! ] Action Required | Run ls and reply OK | proj").as_deref(), Some("Run ls and reply OK"));
        assert_eq!(pane_title("[ . ] Action Required | agent-factory-runtime"), None);
        assert_eq!(pane_title("[ ! ] Action Required"), None);
    }

    #[test]
    fn startup_gates_and_confirmations_are_asking_and_the_timer_line_is_running() {
        // These three screens were captured on 0.159
        let review = "  Hooks need review\n  6 hooks are new or changed.\n  Hooks can run outside the sandbox after you trust them.\n› 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n  enter confirm · esc skip";
        assert_eq!(Codex.screen(&lines(review)), Seen::Ask("Hooks need review".into()));
        let trust = "  Folder access\n  /p/api\n  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit";
        assert!(matches!(Codex.screen(&lines(trust)), Seen::Ask(_)));
        let update = "  Update available · 0.159.3 → 0.160.0\n› 1. Update now\n  2. Skip\n  enter continue · esc skip";
        assert!(matches!(Codex.screen(&lines(update)), Seen::Ask(_)));
        // Logged out: captured on 0.163; the last few lines of the logo on top aren't the question
        let login = "  ⢸⣿⣿⡿⢿⣿⣿⣿⣦⣤⣴⣿⣿⣿⡿⠟⠋⢸⣿⣿  ⣿⣿⣿⣴⣿⣿⣿⠃\n  ⠘⣿⣿⣷ ⠈⠛⠿⣿⣿⣿⠿⠋⠉ ⣠⣴⣿⣿⣿  ⣿⣿⣿⣿⣿⠟⠁\n   ⠹⣿⣿⣿⣤⣀ ⠈⠉⠁⢀⣤⣶⣿⣿⣿⡿⠟⠃ ⣸⣿⣿⡟⠋⠁\n    \
                     ⠙⠿⣿⣿⣿⣿⣶⣾⣿⣿⣿⣿⠟⠛⠁  ⣠⣴⣿⣿⡟\n      ⠈⠙⠛⠛⠛⠿⠟⢿⣿⣿⣿⣿⣿⣾⣿⣿⣿⡿⠏\n\n  Welcome to Codex, OpenAI's command-line coding agent\n\n  \
                     Sign in with ChatGPT to use Codex as part of your paid plan\n  or connect an API key for usage-based billing\n\n\
                     > 1. Sign in with ChatGPT\n     Usage included with Plus, Pro, Business, and Enterprise plans\n\n  \
                     2. Sign in with Device Code\n     Sign in from another device with a one-time code\n\n  \
                     3. Provide your own API key\n     Pay for what you use\n\n  Press enter to continue";
        assert_eq!(Codex.screen(&lines(login)), Seen::Ask("Welcome to Codex, OpenAI's command-line coding agent".into()));
        assert_eq!(
            Codex.screen(&lines("› 写一篇长文\n\n• Working (1m 05s • esc to interrupt)\n\n› Ask Codex to do anything")),
            Seen::Running(65)
        );
        assert_eq!(Codex.screen(&lines("• ok\n  Worked for 5s • 2:35 AM\n› Ask Codex to do anything\n  Context 97% left")), Seen::Quiet);
        // Captured on 0.163: a question to you (not necessarily ending in a question mark; take the line under "Question 1/1"),
        // a permission prompt, a manual compaction
        let question = "› Use request_user_input to ask me to choose between option A and option B. Do nothing else?\n\n  Question 1/1 (1 unanswered)\n  Choose option A or option B.\n\n  › 1. Option A           Select option A.\n    2. Option B           Select option B.\n    3. None of the above  Optionally, add details in notes (tab)\n\n  tab to add notes | enter to submit answer | esc to interrupt";
        assert_eq!(Codex.screen(&lines(question)), Seen::Ask("Choose option A or option B.".into()));
        let perm = "• Running touch made.txt\n\n  Would you like to run the following command?\n\n  Environment: local\n\n  Reason: Allow creating made.txt in the current directory?\n\n  $ touch made.txt\n\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again for commands that start with `touch made.txt` (p)\n  3. No, and tell Codex what to do differently (esc)\n\n  Press enter to confirm or esc to cancel";
        assert_eq!(Codex.screen(&lines(perm)), Seen::Ask("Would you like to run the following command?".into()));
        assert_eq!(
            Codex.screen(&lines("› /compact\n\n• Compacting context (4s • esc to interrupt)\n\n› Ask Codex to do anything")),
            Seen::Running(4)
        );
    }

    #[test]
    fn a_running_codex_is_found_in_its_log_by_process_id() {
        use rusqlite::Connection;
        let d = std::env::temp_dir().join(format!("aca-codex-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("thread-writer-locks")).unwrap();
        // The two tables of 0.163 (only the columns used)
        let logs = Connection::open(d.join("logs_2.sqlite")).unwrap();
        logs.execute_batch("create table logs (id integer primary key autoincrement, ts integer, thread_id text, process_uuid text);")
            .unwrap();
        let state = Connection::open(d.join("state_5.sqlite")).unwrap();
        state
            .execute_batch("create table threads (id text primary key, rollout_path text not null, source text not null, name text);")
            .unwrap();
        let log = |pid: &str, th: Option<&str>| {
            logs.execute("insert into logs (ts, thread_id, process_uuid) values (0, ?1, ?2)", rusqlite::params![th, pid]).unwrap();
        };
        let lock = |th: &str| std::fs::write(d.join("thread-writer-locks").join(format!("{th}.lock")), "").unwrap();
        let thread = |th: &str, source: &str, name: &str| {
            state.execute("insert into threads values (?1, ?2, ?3, ?4)", [th, &format!("/r/{th}.jsonl"), source, name]).unwrap();
        };
        // Process 12: first talked in th-a; its internal title call (th-t, no write lock, not in threads); a spawned subthread th-s
        thread("th-a", "cli", "修登录");
        lock("th-a");
        thread("th-s", r#"{"subagent":{"thread_spawn":{"parent_thread_id":"th-a"}}}"#, "");
        lock("th-s");
        log("pid:12:u1", None);
        log("pid:12:u1", Some("th-a"));
        log("pid:12:u1", Some("th-t"));
        log("pid:12:u1", Some("th-s"));
        // Process 1 is someone else (`pid:1` mustn't hit `pid:12`)
        thread("th-o", "cli", "别的");
        lock("th-o");
        log("pid:1:u2", Some("th-o"));
        assert_eq!(live_in(&d, 12), Some(("th-a".into(), "/r/th-a.jsonl".into(), "修登录".into())));
        assert_eq!(live_in(&d, 1).map(|l| l.0).as_deref(), Some("th-o"));
        assert_eq!(live_in(&d, 3), None);
        // After /new: the new conversation has a write lock but isn't in threads yet, so only its id is known
        lock("th-n");
        log("pid:12:u1", Some("th-n"));
        assert_eq!(live_in(&d, 12), Some(("th-n".into(), String::new(), String::new())));
        // When adopting: what's recognized wins over the command line; when the log database can't tell, the command line is used
        // as before
        let argv: Vec<String> = ["codex", "resume", "th-x", "--search"].map(String::from).to_vec();
        let l = Codex.live(12, Some(&d.to_string_lossy()), &argv);
        assert_eq!((l.conv.as_str(), l.args), ("th-n", Some(vec!["--search".to_string()])));
        assert_eq!(Codex.live(3, Some(&d.to_string_lossy()), &argv).conv, "th-x");
        // The database kept elsewhere (config.toml's sqlite_home)
        let moved = d.join("db");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::rename(d.join("logs_2.sqlite"), moved.join("logs_2.sqlite")).unwrap();
        std::fs::rename(d.join("state_5.sqlite"), moved.join("state_5.sqlite")).unwrap();
        assert_eq!(live_in(&d, 1), None);
        std::fs::write(d.join("config.toml"), format!("sqlite_home = \"{}\"\n", moved.display())).unwrap();
        assert_eq!(live_in(&d, 1).map(|l| l.2).as_deref(), Some("别的"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
