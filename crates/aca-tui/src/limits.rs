//! Claude's subscription quotas, with "when these numbers are from".
//!
//! Recording only "who wrote last" doesn't work: each session's statusLine holds the rate_limits from its own last API call,
//! and an idle session redrawing its status bar (editing settings.json redraws every session's) writes the old numbers back,
//! so the percentage on the panel jumps backwards. rate_limits only has percentages and reset times, no timestamp, so the
//! statusLine goes through `aca limits -- <original command>` first (or `aca statusline`, which draws it itself and records
//! the same way), keeping one copy per session, with each window stamped with the time of its data:
//! - The first time this session reports this window: take the time of the last reply in its transcript (by the time the
//!   status bar refreshes it's already written, and an idle old session reports exactly the numbers from then); no reply
//!   at all = a just-opened session that just made its call, so now
//! - The numbers changed: now (a session's numbers only change when it has just called the API itself)
//! - Unchanged: the later of the previous time and the last reply's
//!
//! The panel takes, per account per window, the copy with the newest time, so it keeps up even when a quota resets early
//! and the numbers go down.
//! Stored in ~/.local/state/aca/ui/limits/<session id>.json; deleted after eight days untouched.
//!
//! There's another path that skips the status bar: the mod aca loads when starting Claude (`aca_core::agent::claude_mod`,
//! only with `[tools.claude] mod = true`) hands every usage measurement Claude takes to `aca limits --mod`. It only hands
//! over fresh numbers (on start, at each turn end, whenever a quota moves a point; a session that resumes an old conversation
//! also hands over the account's current numbers on start, tested on 2.1.295), so the time is the moment it was handed over,
//! no transcript lookup needed.
//! Each path has its own file (`mod-<aca session id>.json`): when one session reports through both, numbers differing in
//! one decimal place aren't taken by each other as "changed".
//! Every session runs this program on every refresh (the hook also runs once per event), so installing a new aca must copy
//! it to a temporary name and rename it into place,
//! never copy straight over the original: processes starting mid-copy would read half a program.
//!
//! Both paths above only have 5h and 7d; the per-model weekly quota (Fable) is only in Claude's own `/usage`. Hence a third:
//! when an account has a session reporting new numbers and [`ASK_EVERY`] has passed since the last ask, start
//! `aca limits --ask` in the background to ask Claude itself (see [`ask`]),
//! stored in `ask-<account>.json`, the time again being the moment it asked.
//!
//! Two things for outside programs (e.g. a menu bar), neither going online nor polling: `aca limits --json` prints the
//! current numbers (merged per account; Codex's read from its latest transcript, see [`usage::codex_now`]); when the numbers
//! move, a `limits.changed` is written to the event stream (see [`announce`]),
//! so subscribers don't have to ask every so often. Codex has no status bar; the hook starts an `aca limits --codex` to take
//! a look when one of its turns finishes.

use std::collections::HashMap;
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aca_core::agent::claude_mod::Verdict;
use aca_core::config::Tool;
use aca_core::{t, tr};
use memchr::memmem::Finder;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use serde::{Deserialize, Serialize};

use crate::data::Agent;
use crate::overlay::{Fx, Setting};
use crate::usage::{self, Quota, Source, Win, parse_ts};

/// What one session recorded
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Rec {
    /// Account dir (the transcripts are under its projects/)
    dir: String,
    wins: Vec<RWin>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RWin {
    label: String,
    used: f64,
    resets: Option<i64>,
    /// When these numbers are from
    at: i64,
}

pub fn dir() -> Option<PathBuf> {
    crate::state::state_file("limits")
}

/// `aca limits [-- command…]`: record the quotas this statusLine call handed over, then pass the input unchanged to the
/// command after it, relaying its output and exit code. Errors on our side stay silent: if a status bar command reports an
/// error, Claude Code draws the status bar empty
pub fn run(args: &[String]) -> i32 {
    // These two have no input: don't read stdin (reading it would hang when typing --json in a terminal)
    match args.first().map(String::as_str) {
        Some("--json") => return print_json(),
        Some("--codex") => return codex_turn(),
        _ => {}
    }
    let mut payload = vec![];
    let _ = std::io::stdin().read_to_end(&mut payload);
    if args.first().is_some_and(|a| a == "--ask") {
        return args.get(1).map_or(0, |acct| ask(acct));
    }
    if args.first().is_some_and(|a| a == "--mod") {
        return from_mod(&payload);
    }
    if let Some(d) = dir() {
        record(&payload, &d, now());
    }
    let cmd: Vec<&String> = args.iter().skip_while(|a| *a != "--").skip(1).collect();
    let Some((prog, rest)) = cmd.split_first() else { return 0 };
    let child = Command::new(prog).args(rest).stdin(Stdio::piped()).spawn();
    let Ok(mut child) = child else { return 1 };
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(&payload);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Deserialize)]
struct Payload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    rate_limits: Option<serde_json::Map<String, serde_json::Value>>,
}

fn label_of(key: &str) -> String {
    match key {
        "five_hour" => "5h".into(),
        "seven_day" => "7d".into(),
        _ => key.into(),
    }
}

/// A fixed order for windows: 5h, 7d, the rest by name
fn order(label: &str) -> (u8, String) {
    (
        match label {
            "5h" => 0,
            "7d" => 1,
            _ => 2,
        },
        label.to_string(),
    )
}

/// An id usable in a file name
fn plain(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// One window: (name, percent used, when it resets)
type Got = (String, f64, Option<i64>);

/// Record once: each window's numbers and time in the session file are updated by the rules at the top of the file;
/// nothing is written if unchanged
pub fn record(payload: &[u8], dir: &Path, now: i64) {
    let Ok(p) = serde_json::from_slice::<Payload>(payload) else { return };
    let (Some(sid), Some(rl)) = (p.session_id, p.rate_limits) else { return };
    if !plain(&sid) {
        return;
    }
    let got: Vec<Got> = rl
        .iter()
        .filter_map(|(k, v)| Some((label_of(k), v.get("used_percentage")?.as_f64()?, v.get("resets_at").and_then(|x| x.as_i64()))))
        .collect();
    let transcript = p.transcript_path.map(PathBuf::from);
    let acct = account_dir(transcript.as_deref());
    let moved = update(&dir.join(format!("{sid}.json")), got, &acct, transcript.as_deref(), now, false);
    if moved {
        announce(dir, &acct);
    }
    ask_if_due(dir, &acct, moved, now);
}

/// The `session.measure` the mod hands over (as on Claude 2.1.295):
/// `{"rateLimits": [{"kind": "five_hour", "percentUsed": 6, "resetsAt": "2026-10-09T11:40:00.000Z"}, …], "context": …, "cost": …}`.
/// For a non-subscription account `rateLimits` is empty. Not even the key / not this shape = unrecognized (None):
/// the interface changed
fn measured(payload: &[u8]) -> Option<Vec<Got>> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.get("rateLimits")?
        .as_array()?
        .iter()
        .map(|w| {
            let resets = w.get("resetsAt").and_then(|x| x.as_str()).map(parse_ts).filter(|&t| t > 0);
            Some((label_of(w.get("kind")?.as_str()?), w.get("percentUsed")?.as_f64()?, resets))
        })
        .collect()
}

/// `aca limits --mod`: aca's mod inside Claude hands over a usage measurement. The session comes from `ACA_SESSION_ID` in the
/// environment, the account from `CLAUDE_CONFIG_DIR` (subprocesses the mod starts carry the session's environment).
/// Like the hook: always exits 0, writes nothing
fn from_mod(payload: &[u8]) -> i32 {
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|id| plain(id)) else { return 0 };
    // Claude's background sessions inherit the original pane's environment; they aren't this session itself (as in `aca hook`)
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    let got = measured(payload);
    aca_core::agent::claude_mod::heard(&id, got.is_some());
    if let (Some(got), Some(d)) = (got, dir()) {
        let (acct, now) = (account_dir(None), now());
        let moved = update(&d.join(format!("mod-{id}.json")), got, &acct, None, now, true);
        if moved {
            announce(&d, &acct);
        }
        ask_if_due(&d, &acct, moved, now);
    }
    0
}

/// How often to ask Claude itself, per account (seconds)
const ASK_EVERY: i64 = 300;
/// Longest wait when asking Claude (seconds): it answers in two or three seconds in practice
const ASK_WAIT: u64 = 20;

/// An account's copy from asking Claude itself (`ask-<account>.json`, read together with the sessions' by `quotas`) and its
/// lock (`.lock`, holding the time of the last ask)
fn ask_files(dir: &Path, acct: &str) -> (PathBuf, PathBuf) {
    let mut h = DefaultHasher::new();
    std::fs::canonicalize(acct).unwrap_or_else(|_| PathBuf::from(acct)).hash(&mut h);
    let key = format!("ask-{:016x}", h.finish());
    (dir.join(format!("{key}.json")), dir.join(format!("{key}.lock")))
}

/// The status bar / mod just recorded: if this account's numbers moved (some session just called the API) and [`ASK_EVERY`]
/// has passed since the last ask, or it was never asked, start an `aca limits --ask` in the background. Only the lock file's
/// mtime is checked here, so dozens of sessions refreshing together cost next to nothing;
/// if several start together, only one actually asks (see [`ask`])
fn ask_if_due(dir: &Path, acct: &str, moved: bool, now: i64) {
    let (_, lock) = ask_files(dir, acct);
    let last = std::fs::metadata(&lock).and_then(|m| m.modified()).ok();
    let age = last.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| now - d.as_secs() as i64);
    // In tests "itself" is the test binary
    if !due(age, moved) || cfg!(test) {
        return;
    }
    // Own process group, no pipes: the status bar command waits for its own output, not for this
    let _ = aca_core::proc::detach(
        Command::new(aca_core::agent::self_exe())
            .args(["limits", "--ask", acct])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

/// age = seconds since the last ask, None if never asked
fn due(age: Option<i64>, moved: bool) -> bool {
    age.is_none_or(|a| moved && a >= ASK_EVERY)
}

/// `aca limits --ask <account dir>`: ask Claude itself for this account's quotas (the SDK's `get_usage`, same source as
/// `/usage`); the per-model weekly quota (Fable) is only here. Starts a `claude -p` with this account's environment (as
/// `aca run`: no hooks), reading no settings, connecting no MCP, keeping no transcript, closed once answered. Only asks
/// accounts with a registered config dir: an unregistered one might carry the outer session's account.
/// Locks the lock file first, then checks again when the last ask was, so of several started together only one asks.
/// Like the hook: always exits 0, writes nothing
fn ask(acct: &str) -> i32 {
    let Some(dir) = dir() else { return 0 };
    let (file, lock) = ask_files(&dir, acct);
    let _ = std::fs::create_dir_all(&dir);
    let Ok(held) = std::fs::OpenOptions::new().create(true).append(true).open(&lock) else { return 0 };
    if held.try_lock().is_err() {
        return 0;
    }
    let now = now();
    let last: Option<i64> = std::fs::read_to_string(&lock).ok().and_then(|s| s.trim().parse().ok());
    if last.is_some_and(|t| now - t < ASK_EVERY) {
        return 0;
    }
    // Failing to get an answer (not logged in, rate-limited) counts as asked too: otherwise the next refresh comes again
    let _ = std::fs::write(&lock, now.to_string());
    let cfg = aca_core::config::load();
    let Some(spec) = spec_of(&cfg, acct) else { return 0 };
    if let Some(got) = asked(&cfg, &spec)
        && update(&file, got, acct, None, self::now(), true)
    {
        announce(&dir, acct);
    }
    0
}

/// Account dir → how `aca run` writes it (`claude:work`): only for accounts with a registered config dir
fn spec_of(cfg: &aca_core::config::Config, acct: &str) -> Option<String> {
    let real = std::fs::canonicalize(acct).ok()?;
    let same = |home: &str| !home.is_empty() && std::fs::canonicalize(home).is_ok_and(|h| h == real);
    let name = cfg.accounts.get("claude")?.iter().find(|(_, a)| same(&a.home))?.0;
    Some(format!("claude:{name}"))
}

/// Start Claude and ask once; if it answers, those are the windows; no answer, can't start, not a subscription
/// (rate_limits is null) = None / empty
fn asked(cfg: &aca_core::config::Config, spec: &str) -> Option<Vec<Got>> {
    use std::os::unix::process::CommandExt;
    let args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--no-session-persistence",
    ]
    .map(String::from);
    let l = aca_core::agent::direct(cfg, spec, &args).ok()?;
    let mut child = Command::new(&l.argv[0])
        .args(&l.argv[1..])
        .envs(l.env)
        .current_dir(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    // Close once sent: it exits by itself after answering this one (tested on 2.1.303)
    let ask =
        serde_json::json!({"type": "control_request", "request_id": "aca", "request": {"subtype": "get_usage", "skip_behaviors": true}});
    if let Some(mut i) = child.stdin.take() {
        let _ = writeln!(i, "{ask}");
    }
    let began = std::time::Instant::now();
    while child.try_wait().ok()?.is_none() {
        if began.elapsed().as_secs() >= ASK_WAIT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    usage_of(&child.wait_with_output().ok()?.stdout)
}

/// The answer to `get_usage` (as on Claude 2.1.303, marked experimental in the SDK):
/// `{"type":"control_response","response":{"subtype":"success","request_id":"aca","response":{"rate_limits":{
/// "five_hour":{"utilization":47,"resets_at":"2026-10-19T11:30:00.428198+00:00",…},"seven_day":{…},…,
/// "model_scoped":[{"display_name":"Fable","utilization":48,"resets_at":"…"}]},…}}}`.
/// The window names in model_scoped are Claude's (Fable), recorded as given
fn usage_of(out: &[u8]) -> Option<Vec<Got>> {
    out.split(|&b| b == b'\n').find_map(|line| {
        let v: serde_json::Value = serde_json::from_slice(line).ok()?;
        let r = v.get("response").filter(|r| r.get("request_id").and_then(|x| x.as_str()) == Some("aca"))?;
        let rl = r.pointer("/response/rate_limits")?;
        let win = |label: String, w: &serde_json::Value| {
            let resets = w.get("resets_at").and_then(|x| x.as_str()).map(parse_ts).filter(|&t| t > 0);
            Some((label, w.get("utilization")?.as_f64()?, resets))
        };
        let mut got: Vec<Got> = ["five_hour", "seven_day"].iter().filter_map(|k| win(label_of(k), rl.get(*k)?)).collect();
        let scoped = rl.get("model_scoped").and_then(|x| x.as_array()).into_iter().flatten();
        got.extend(scoped.filter_map(|m| win(m.get("display_name")?.as_str()?.to_string(), m)));
        Some(got)
    })
}

/// The line at the top of the billing panel's quota section: the mod is on (`[tools.<the claude one>] mod = true`) but isn't
/// loaded on the current Claude version.
/// Off, loaded, or no session started yet (unverified) = empty. Why it isn't loaded is written on the mod row of the settings page
pub fn mod_note(cfg: &aca_core::config::Config) -> String {
    let on = cfg.names().iter().filter_map(|n| cfg.tool(n.split(':').next()?)).any(|t| t.kind == "claude" && t.mods);
    match on.then(aca_core::agent::claude_mod::verdict).flatten() {
        Some(v) if !v.ok => t!("额度可能不是最新数据", "quotas may be out of date").into(),
        _ => String::new(),
    }
}

/// The read/write hooks for the mod row in `^S` (swapped for fakes in tests)
#[derive(Clone, Copy)]
pub struct ModIo {
    /// How `[tools.claude]` is written now
    tool: fn() -> Tool,
    /// Whether the current Claude version loads it (unverified = None)
    verdict: fn() -> Option<Verdict>,
    set: fn(&str, &str, toml::Value) -> Result<(), String>,
}

/// A row on the settings page: "m  Claude mod  on", followed by why if it's on but has no effect. ⏎ toggles it on the
/// spot, changing `[tools.claude] mod`;
/// open sessions are untouched, they follow on their next start (new, restart)
pub struct ModRow {
    io: ModIo,
}

impl ModRow {
    pub fn real() -> ModRow {
        ModRow {
            io: ModIo {
                tool: || aca_core::config::load().tool("claude").unwrap_or_default(),
                verdict: aca_core::agent::claude_mod::verdict,
                set: crate::config::set,
            },
        }
    }

    /// On but with no effect: the why written in brackets on the row and said in the bottom bar when turning it on.
    /// Loadable (unverified counts) = None
    fn not_carried(&self, tool: &Tool) -> Option<(String, String)> {
        // Wrapped in a command of its own: no telling which Claude version runs inside, so not loaded
        if !tool.cmd.is_empty() {
            return Some((
                t!("已自定义命令", "custom command").into(),
                t!("已自定义命令,mod 不生效", "a custom command is set; the mod has no effect").into(),
            ));
        }
        match (self.io.verdict)() {
            Some(v) if !v.ok => {
                Some((format!("Claude {}", v.build), tr!("Claude {} 未加载 mod:{}", "Claude {} won't load the mod: {}", v.build, v.why)))
            }
            _ => None,
        }
    }
}

impl Setting for ModRow {
    fn key(&self) -> char {
        'm'
    }

    fn name(&self) -> &'static str {
        "Claude mod"
    }

    fn value(&self) -> Vec<Span<'static>> {
        let tool = (self.io.tool)();
        let bold = Style::new().add_modifier(Modifier::BOLD);
        if !tool.mods {
            return vec![Span::styled(t!("关", "off"), bold)];
        }
        let mut out = vec![Span::styled(t!("开", "on"), bold)];
        if let Some((short, _)) = self.not_carried(&tool) {
            out.push(Span::styled(tr!(" · 不生效({short})", " · no effect ({short})"), crate::theme::warn()));
        }
        out
    }

    fn mark(&self) -> &'static str {
        ""
    }

    fn enter(&self, fx: &mut Fx) {
        let tool = (self.io.tool)();
        let on = !tool.mods;
        if let Err(e) = (self.io.set)("tools.claude", "mod", toml::Value::Boolean(on)) {
            return fx.say(e);
        }
        // Open sessions are untouched, only newly started / restarted ones follow
        fx.say(match self.not_carried(&tool) {
            Some((_, why)) if on => why,
            _ => t!("已保存,新会话生效", "saved; new sessions use it").into(),
        });
    }
}

/// The windows a session reported this time → its file; acct is its account dir. `live` = these numbers are current (handed
/// over by the mod, asked); otherwise the time is inferred from the transcript per the rules at the top of the file.
/// Returns whether any window's numbers changed (a first report counts)
fn update(path: &Path, mut got: Vec<Got>, acct: &str, transcript: Option<&Path>, now: i64, live: bool) -> bool {
    if got.is_empty() {
        return false;
    }
    got.sort_by_key(|w| order(&w.0));
    let old: Rec = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let last = if live { None } else { transcript.and_then(last_reply) };
    let mut rec = Rec { dir: acct.to_string(), wins: old.wins.clone() };
    let mut moved = false;
    for (label, used, resets) in got {
        let before = old.wins.iter().find(|w| w.label == label);
        moved |= before.is_none_or(|w| w.used != used || w.resets != resets);
        let at = match before {
            _ if live => now,
            Some(w) if w.used == used && w.resets == resets => w.at.max(last.unwrap_or(0)),
            Some(_) => now,
            None => last.unwrap_or(now),
        };
        let w = RWin { label: label.clone(), used, resets, at };
        match rec.wins.iter_mut().find(|w| w.label == label) {
            Some(x) => *x = w,
            None => rec.wins.push(w),
        }
    }
    rec.wins.sort_by_key(|w| order(&w.label));
    if rec != old
        && let Ok(text) = serde_json::to_string(&rec)
    {
        let _ = crate::state::write_atomic(path, &text);
    }
    moved
}

/// The transcript is `<account dir>/projects/<project>/<session>.jsonl`; if it doesn't match, guess by Claude Code's own rules
fn account_dir(transcript: Option<&Path>) -> String {
    let from_path =
        transcript.and_then(|t| t.parent()?.parent()).filter(|p| p.file_name().is_some_and(|n| n == "projects")).and_then(Path::parent);
    match from_path {
        Some(d) => d.to_string_lossy().into_owned(),
        None => std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| format!("{}/.claude", std::env::var("HOME").unwrap_or_default())),
    }
}

static F_REPLY: std::sync::LazyLock<Finder<'static>> = std::sync::LazyLock::new(|| Finder::new(b"\"type\":\"assistant\""));

/// Time of the last reply in the transcript: read backwards from the end chunk by chunk, at most 8 MB (one line of tool output
/// can be several MB)
fn last_reply(path: &Path) -> Option<i64> {
    const CHUNK: u64 = 256 << 10;
    const LIMIT: u64 = 8 << 20;
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut end = len;
    // The half line at the start of the later chunk, appended to the earlier chunk
    let mut carry: Vec<u8> = vec![];
    while end > 0 && len - end < LIMIT {
        let start = end.saturating_sub(CHUNK);
        let mut buf = vec![0; (end - start) as usize];
        f.seek(SeekFrom::Start(start)).ok()?;
        f.read_exact(&mut buf).ok()?;
        buf.extend_from_slice(&carry);
        let cut = if start == 0 { 0 } else { memchr::memchr(b'\n', &buf).map_or(buf.len(), |i| i + 1) };
        for line in buf[cut..].rsplit(|&b| b == b'\n') {
            if F_REPLY.find(line).is_none() {
                continue;
            }
            #[derive(Deserialize)]
            struct L {
                #[serde(rename = "type")]
                kind: Option<String>,
                timestamp: Option<String>,
            }
            if let Ok(L { kind: Some(k), timestamp: Some(ts) }) = serde_json::from_slice::<L>(line)
                && k == "assistant"
            {
                return Some(parse_ts(&ts)).filter(|&t| t > 0);
            }
        }
        carry = buf[..cut].to_vec();
        end = start;
    }
    None
}

/// Registered Claude accounts: (real dir, name)
fn names(srcs: &[Source]) -> Vec<(PathBuf, &str)> {
    srcs.iter().filter(|s| s.agent == Agent::Claude).filter_map(|s| Some((std::fs::canonicalize(&s.dir).ok()?, s.who.as_str()))).collect()
}

/// Account dir → the account name in billing: the registered name if it matches one, else the directory name
fn who_of(names: &[(PathBuf, &str)], dir: &str) -> String {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| PathBuf::from(dir));
    names
        .iter()
        .find(|(d, _)| *d == real)
        .map(|(_, w)| w.to_string())
        .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or("").trim_start_matches('.').to_string())
}

/// For the status bar: the copy for the account of this transcript (as `quotas`, the newest across all of this account's
/// sessions, not what this session itself holds).
/// None = this account never recorded any window
pub fn quota_for(dir: &Path, srcs: &[Source], transcript: Option<&Path>, now: i64) -> Option<Quota> {
    let who = who_of(&names(srcs), &account_dir(transcript));
    quotas(dir, srcs, now).into_iter().find(|q| q.who == who)
}

/// One account's merged copy
struct Merged {
    /// Account name (as in billing)
    who: String,
    /// Account dir (recorded from what its sessions reported)
    dir: String,
    wins: Vec<RWin>,
}

/// Per account per window, the copy with the newest time (on a tie, the later reset and higher usage), sorted by account name.
/// Account names are the same as in billing (matched to srcs by account dir); also deletes session files untouched for eight days
fn merged(dir: &Path, srcs: &[Source]) -> Vec<Merged> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let names = names(srcs);
    let mut best: HashMap<String, (String, HashMap<String, RWin>)> = HashMap::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let age = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a.as_secs() > 8 * 86400) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let Some(rec) = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Rec>(&b).ok()) else { continue };
        let (_, wins) = best.entry(who_of(&names, &rec.dir)).or_insert_with(|| (rec.dir.clone(), HashMap::new()));
        for w in rec.wins {
            let newer = |o: &RWin| (w.at, w.resets, w.used) > (o.at, o.resets, o.used);
            if wins.get(&w.label).is_none_or(newer) {
                wins.insert(w.label.clone(), w);
            }
        }
    }
    let mut out: Vec<Merged> = best
        .into_iter()
        .map(|(who, (dir, m))| {
            let mut wins: Vec<RWin> = m.into_values().collect();
            wins.sort_by_key(|w| order(&w.label));
            Merged { who, dir, wins }
        })
        .collect();
    out.sort_by(|a, b| a.who.cmp(&b.who));
    out
}

/// For the panel: each account's merged copy (see [`merged`])
pub fn quotas(dir: &Path, srcs: &[Source], now: i64) -> Vec<Quota> {
    merged(dir, srcs)
        .into_iter()
        .map(|m| Quota {
            agent: Agent::Claude,
            who: m.who,
            plan: String::new(),
            updated: m.wins.iter().map(|w| w.at).max().unwrap_or(0).min(now),
            wins: m.wins.into_iter().map(|w| Win { label: w.label, used: w.used, resets: w.resets }).collect(),
            note: String::new(),
        })
        .collect()
}

// ---------- For outside programs: --json and limits.changed ----------

/// What one account looks like in `--json` / `limits.changed`: `{"tool", "name", "dir", "wins": [{"label", "used", "resets", "at"}]}`
fn account_json(tool: &str, name: &str, dir: &str, wins: &[RWin]) -> serde_json::Value {
    let wins: Vec<serde_json::Value> =
        wins.iter().map(|w| serde_json::json!({ "label": w.label, "used": w.used, "resets": w.resets, "at": w.at })).collect();
    serde_json::json!({ "tool": tool, "name": name, "dir": dir, "wins": wins })
}

/// One Codex account's copy → windows (all from the same record, the time being the moment it was recorded)
fn codex_wins(q: &Quota) -> Vec<RWin> {
    q.wins.iter().map(|w| RWin { label: w.label.clone(), used: w.used, resets: w.resets, at: q.updated }).collect()
}

/// Each Codex subscription account's current quotas: read the tail of its latest few transcripts (`usage::codex_now`, not a
/// scan of the whole dir), skipping ones logged in with an API key (`usage::subscribed`). (account dir, copy)
fn codex_all(accounts: &[(Agent, String, String)], home: &str) -> Vec<(PathBuf, Quota)> {
    usage::sources(accounts, home)
        .into_iter()
        .filter(|s| s.agent == Agent::Codex)
        .filter_map(|s| {
            let q = usage::codex_now(&s)?;
            Some((s.dir, usage::subscribed(vec![q], accounts, home).pop()?))
        })
        .collect()
}

/// `aca limits --json`: every account's current quotas, one JSON printed to stdout (`{"now", "accounts": […]}`).
/// Claude's are the recorded ones merged (as in the billing panel), Codex's are read from its latest transcripts.
/// Accounts never recorded aren't listed
fn print_json() -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = crate::data::accounts(&aca_core::config::load());
    let srcs = usage::sources(&accounts, &home);
    let mut out: Vec<serde_json::Value> = vec![];
    if let Some(d) = dir() {
        out.extend(merged(&d, &srcs).iter().map(|m| account_json("claude", &m.who, &m.dir, &m.wins)));
    }
    for (dir, q) in codex_all(&accounts, &home) {
        out.push(account_json("codex", &q.who, &dir.to_string_lossy(), &codex_wins(&q)));
    }
    println!("{}", serde_json::json!({ "now": now(), "accounts": out }));
    0
}

/// Where an account's last-sent numbers are kept: `seen-<hash>.last` (not .json, so merging won't take it for a session's)
fn seen_file(dir: &Path, tool: &str, acct: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    tool.hash(&mut h);
    std::fs::canonicalize(acct).unwrap_or_else(|_| PathBuf::from(acct)).hash(&mut h);
    dir.join(format!("seen-{:016x}.last", h.finish()))
}

/// Compare this account's numbers with the last sent: each window's name, usage and reset time (the time doesn't count: unmoved
/// numbers were merely reported again).
/// If changed, record these and return true
fn differs(dir: &Path, tool: &str, acct: &str, wins: &[RWin]) -> bool {
    let key: String = wins.iter().map(|w| format!("{} {} {}\n", w.label, w.used, w.resets.unwrap_or(0))).collect();
    let file = seen_file(dir, tool, acct);
    if std::fs::read_to_string(&file).is_ok_and(|old| old == key) {
        return false;
    }
    let _ = crate::state::write_atomic(&file, &key);
    true
}

/// A Claude account just recorded and some session's numbers moved: if the merged copy differs from the last sent, return the
/// fields of `limits.changed`.
/// A session's own numbers moving doesn't mean the account's did (an idle old session's first report of the stale numbers it
/// holds loses to newer ones in the merge)
fn changed(dir: &Path, srcs: &[Source], acct: &str) -> Option<serde_json::Value> {
    let who = who_of(&names(srcs), acct);
    let m = merged(dir, srcs).into_iter().find(|m| m.who == who)?;
    differs(dir, "claude", acct, &m.wins).then(|| account_json("claude", &m.who, &m.dir, &m.wins))
}

/// Which session reported the event: sessions aca starts have `ACA_SESSION_ID` in their environment, carried by subprocesses
/// the status bar, mod and hook start; empty otherwise
fn reporter() -> String {
    std::env::var("ACA_SESSION_ID").ok().filter(|id| plain(id)).unwrap_or_default()
}

/// After a record where this session's numbers moved (the status bar refreshes every 10 seconds; unmoved numbers never get
/// here): if the account's merged copy changed too, write a `limits.changed` to the event stream. Like the hook: errors stay
/// silent
fn announce(dir: &Path, acct: &str) {
    // Tests write fake accounts; don't send them to the real event stream
    if cfg!(test) {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let srcs = usage::sources(&crate::data::accounts(&aca_core::config::load()), &home);
    if let Some(data) = changed(dir, &srcs, acct) {
        aca_core::event::emit("limits.changed", &reporter(), data);
    }
}

/// `aca limits --codex`: started in the background by the hook when a Codex turn finishes (Codex has no status bar). Compare
/// each Codex account's numbers with the last sent, and send `limits.changed` for the changed ones. Like the hook: always
/// exits 0, writes nothing
fn codex_turn() -> i32 {
    let Some(dir) = dir() else { return 0 };
    // The turn's last usage entry (with quotas) is written by Codex in the background; wait for it to hit the disk
    std::thread::sleep(std::time::Duration::from_secs(1));
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = crate::data::accounts(&aca_core::config::load());
    let _ = std::fs::create_dir_all(&dir);
    for (acct, q) in codex_all(&accounts, &home) {
        let (acct, wins) = (acct.to_string_lossy(), codex_wins(&q));
        if differs(&dir, "codex", &acct, &wins) {
            aca_core::event::emit("limits.changed", &reporter(), account_json("codex", &q.who, &acct, &wins));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(sid: &str, transcript: &Path, five: f64, seven: f64) -> Vec<u8> {
        serde_json::json!({
            "session_id": sid,
            "transcript_path": transcript,
            "rate_limits": {
                "five_hour": {"used_percentage": five, "resets_at": 5000},
                "seven_day": {"used_percentage": seven, "resets_at": 9000}
            }
        })
        .to_string()
        .into_bytes()
    }

    fn reply(ts: &str) -> String {
        format!("{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{}}}}\n")
    }

    #[test]
    fn an_idle_session_redrawing_old_numbers_loses_to_the_fresh_one() {
        let root = std::env::temp_dir().join(format!("aca-limits-{}", std::process::id()));
        let acct = root.join(".claude-work");
        let proj = acct.join("projects/-x");
        std::fs::create_dir_all(&proj).unwrap();
        let store = root.join("limits");
        let fresh = proj.join("a.jsonl");
        let idle = proj.join("b.jsonl");
        let t = |s: &str| parse_ts(s);
        // b's last reply at 12:47 (80 / 39 then), a's at 13:48 (98 / 41)
        std::fs::write(&idle, reply("2026-09-30T04:47:40Z") + "{\"type\":\"system\",\"timestamp\":\"2026-09-30T05:50:00Z\"}\n").unwrap();
        std::fs::write(&fresh, reply("2026-09-30T05:48:00Z")).unwrap();
        record(&payload("a", &fresh, 98.0, 41.0), &store, t("2026-09-30T05:48:01Z"));
        // 13:50 b redraws: old numbers, and the time is old too
        record(&payload("b", &idle, 80.0, 39.0), &store, t("2026-09-30T05:50:43Z"));
        let srcs = vec![Source { agent: Agent::Claude, who: "work".into(), dir: acct.clone() }];
        let q = quotas(&store, &srcs, t("2026-09-30T06:00:00Z"));
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].who, "work");
        assert_eq!(q[0].wins.iter().map(|w| w.used).collect::<Vec<_>>(), vec![98.0, 41.0]);
        assert_eq!(q[0].updated, t("2026-09-30T05:48:00Z"));

        // Quota reset early: b called the API once and its numbers dropped, but they're newer → follow them
        record(&payload("b", &idle, 2.0, 30.0), &store, t("2026-09-30T06:10:00Z"));
        let q = quotas(&store, &srcs, t("2026-09-30T06:10:00Z"));
        assert_eq!(q[0].wins.iter().map(|w| w.used).collect::<Vec<_>>(), vec![2.0, 30.0]);
        assert_eq!(q[0].updated, t("2026-09-30T06:10:00Z"));

        // a's numbers unchanged, replied once more: the time follows the last reply; a redrawing doesn't overwrite b's new numbers
        std::fs::write(&fresh, reply("2026-09-30T05:48:00Z") + &reply("2026-09-30T05:55:00Z")).unwrap();
        record(&payload("a", &fresh, 98.0, 41.0), &store, t("2026-09-30T06:20:00Z"));
        let a: Rec = serde_json::from_slice(&std::fs::read(store.join("a.json")).unwrap()).unwrap();
        assert_eq!(a.wins[0].at, t("2026-09-30T05:55:00Z"));
        let q = quotas(&store, &srcs, t("2026-09-30T06:20:00Z"));
        assert_eq!(q[0].wins.iter().map(|w| w.used).collect::<Vec<_>>(), vec![2.0, 30.0]);

        // A just-opened session with no reply line yet: now
        let new = proj.join("c.jsonl");
        std::fs::write(&new, "{\"type\":\"user\"}\n").unwrap();
        record(&payload("c", &new, 3.0, 31.0), &store, t("2026-09-30T06:30:00Z"));
        let q = quotas(&store, &srcs, t("2026-09-30T06:30:00Z"));
        assert_eq!((q[0].wins[0].used, q[0].updated), (3.0, t("2026-09-30T06:30:00Z")));

        // No quotas / a malformed session id: not recorded
        record(br#"{"session_id":"d","transcript_path":"/x/projects/p/d.jsonl"}"#, &store, 1);
        record(&payload("../e", &new, 1.0, 1.0), &store, 1);
        let names: Vec<String> =
            std::fs::read_dir(&store).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn what_the_mod_hands_over_reads_like_the_status_line() {
        // Claude 2.1.295's session.measure as handed over; the status bar at the same moment gave resets_at 1791546000 / 1792047600
        let e = br#"{"context":{"tokens":42868,"window":1000000,"percent":4},"rateLimits":[{"kind":"seven_day","percentUsed":17.5,"resetsAt":"2026-10-15T07:00:00.000Z"},{"kind":"five_hour","percentUsed":6,"resetsAt":"2026-10-09T11:40:00.000Z"}],"cost":{"usd":0.00379882},"changed":["context","cost"]}"#;
        let got = measured(e).unwrap();
        assert_eq!(got, vec![("7d".to_string(), 17.5, Some(1792047600)), ("5h".to_string(), 6.0, Some(1791546000))]);
        // Not a subscription account: empty, but recognized
        assert_eq!(measured(br#"{"context":{"window":200000},"rateLimits":[]}"#), Some(vec![]));
        // Changed shape (key gone, fields in a window renamed), not JSON: unrecognized
        assert_eq!(measured(br#"{"context":{},"limits":[]}"#), None);
        assert_eq!(measured(br#"{"rateLimits":[{"kind":"five_hour","used":6}]}"#), None);
        assert_eq!(measured(b"Usage: aca"), None);

        // Stored in its own file; what's handed over is that moment's numbers, so the time moves even when unchanged (the status bar
        // path doesn't move it if unchanged); empty isn't written
        let root = std::env::temp_dir().join(format!("aca-limits-mod-{}", std::process::id()));
        let file = root.join("mod-ab12.json");
        // A first report counts as moved, a repeat with the same numbers doesn't
        let first = update(&file, got.clone(), "/x/.claude", None, 700, true);
        let again = update(&file, got, "/x/.claude", None, 900, true);
        assert!(!update(&root.join("mod-none.json"), vec![], "/x/.claude", None, 900, true));
        let rec: Rec = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let left = std::fs::read_dir(&root).unwrap().count();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(first && !again);
        assert_eq!(
            rec.wins.iter().map(|w| (w.label.as_str(), w.used, w.at)).collect::<Vec<_>>(),
            vec![("5h", 6.0, 900), ("7d", 17.5, 900)]
        );
        assert_eq!(left, 1);
    }

    #[test]
    fn what_claude_says_when_asked_adds_the_per_model_week() {
        // Claude 2.1.303's answer to get_usage (trimmed): Fable is the per-model weekly quota, not in the status bar
        let out = br#"{"type":"system","subtype":"init"}
{"type":"control_response","response":{"subtype":"success","request_id":"aca","response":{"subscription_type":"max","rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":12,"resets_at":"2026-10-19T11:30:00.608788+00:00"},"seven_day":{"utilization":40,"resets_at":"2026-10-22T06:59:59.609015+00:00"},"seven_day_opus":null,"limits":[],"model_scoped":[{"display_name":"Fable","utilization":48,"resets_at":"2026-10-22T06:59:59.609039+00:00"}]},"behaviors":null}}}
"#;
        let (five, week) = (parse_ts("2026-10-19T11:30:00Z"), parse_ts("2026-10-22T06:59:59Z"));
        let got = usage_of(out).unwrap();
        assert_eq!(got, vec![("5h".into(), 12.0, Some(five)), ("7d".into(), 40.0, Some(week)), ("Fable".into(), 48.0, Some(week))]);
        // Not a subscription account: rate_limits is null, nothing recorded; answers to other requests and non-JSON aren't recognized
        let api_key =
            br#"{"type":"control_response","response":{"request_id":"aca","response":{"rate_limits_available":false,"rate_limits":null}}}"#;
        assert_eq!(usage_of(api_key), Some(vec![]));
        assert_eq!(usage_of(br#"{"type":"control_response","response":{"request_id":"other","response":{}}}"#), None);
        assert_eq!(usage_of(b"Error: not logged in\n"), None);

        // Never asked → ask; once asked, wait until this account's numbers move again and five minutes have passed
        assert!(due(None, false));
        assert!(!due(Some(ASK_EVERY + 1), false) && !due(Some(ASK_EVERY - 1), true));
        assert!(due(Some(ASK_EVERY), true));

        // What's asked and what the status bar recorded merge per account: Fable comes after 5h and 7d
        let root = std::env::temp_dir().join(format!("aca-limits-ask-{}", std::process::id()));
        let acct = root.join(".claude-sub");
        std::fs::create_dir_all(&acct).unwrap();
        let store = root.join("limits");
        let acct_s = acct.to_string_lossy().into_owned();
        update(&store.join("s1.json"), vec![("5h".into(), 13.0, Some(five)), ("7d".into(), 40.0, Some(week))], &acct_s, None, 2000, true);
        update(&ask_files(&store, &acct_s).0, got, &acct_s, None, 1000, true);
        let srcs = vec![Source { agent: Agent::Claude, who: "sub".into(), dir: acct.clone() }];
        let q = quotas(&store, &srcs, 3000);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(
            q[0].wins.iter().map(|w| (w.label.as_str(), w.used)).collect::<Vec<_>>(),
            vec![("5h", 13.0), ("7d", 40.0), ("Fable", 48.0)]
        );
    }

    thread_local! {
        /// Fake settings row: how `[tools.claude]` is written now, this version's verdict, what was written to config.toml
        static TOOL: std::cell::RefCell<Tool> = std::cell::RefCell::new(Tool::default());
        static VERDICT: std::cell::RefCell<Option<Verdict>> = const { std::cell::RefCell::new(None) };
        static WROTE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(vec![]) };
    }

    #[test]
    fn the_settings_row_flips_the_mod_and_says_whether_this_claude_takes_it() {
        let row = ModRow {
            io: ModIo {
                tool: || TOOL.with(|t| t.borrow().clone()),
                verdict: || VERDICT.with(|v| v.borrow().clone()),
                set: |sec, key, v| {
                    WROTE.with(|w| w.borrow_mut().push(format!("[{sec}] {key} = {v}")));
                    // Once written, the next read sees the new value
                    TOOL.with(|t| t.borrow_mut().mods = v.as_bool().unwrap());
                    Ok(())
                },
            },
        };
        let shown = |row: &ModRow| row.value().iter().map(|s| s.content.to_string()).collect::<String>();
        let enter = |row: &ModRow| {
            let mut fx = Fx::default();
            row.enter(&mut fx);
            fx.flash.unwrap()
        };
        assert_eq!((row.key(), row.mark(), shown(&row).as_str()), ('m', "", "关"));
        // On: Claude never started yet, unverified — shows just 「开」 ("on")
        assert_eq!(enter(&row), "已保存,新会话生效");
        assert_eq!(WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()), ["[tools.claude] mod = true"]);
        assert_eq!(shown(&row), "开");
        // This version verified and loadable: still shows just 「开」 ("on")
        let verdict = |ok: bool, why: &str| Some(Verdict { build: "2.1.295".into(), ok, why: why.into(), at: 1 });
        VERDICT.with(|v| *v.borrow_mut() = verdict(true, ""));
        assert_eq!(shown(&row), "开");
        // This version doesn't load it: marked on the row; turn it off and on again, the bottom bar says why
        VERDICT.with(|v| *v.borrow_mut() = verdict(false, "它交来的用量认不出了"));
        assert_eq!((shown(&row).as_str(), row.value()[1].style), ("开 · 不生效(Claude 2.1.295)", crate::theme::warn()));
        assert_eq!(enter(&row), "已保存,新会话生效");
        assert_eq!(shown(&row), "关");
        assert_eq!(enter(&row), "Claude 2.1.295 未加载 mod:它交来的用量认不出了");
        assert_eq!(
            WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()),
            ["[tools.claude] mod = false", "[tools.claude] mod = true"]
        );
        // The tool is wrapped in a command of its own: no version loads it
        TOOL.with(|t| t.borrow_mut().cmd = vec!["wrap.sh".into()]);
        assert_eq!(shown(&row), "开 · 不生效(已自定义命令)");
    }

    #[test]
    fn limits_changed_goes_out_only_when_the_accounts_merged_numbers_move() {
        let root = std::env::temp_dir().join(format!("aca-limits-changed-{}", std::process::id()));
        let acct = root.join(".claude-sub");
        let proj = acct.join("projects/-x");
        std::fs::create_dir_all(&proj).unwrap();
        let store = root.join("limits");
        let (fresh, idle) = (proj.join("a.jsonl"), proj.join("b.jsonl"));
        let t = |s: &str| parse_ts(s);
        std::fs::write(&fresh, reply("2026-10-26T01:00:00Z")).unwrap();
        std::fs::write(&idle, reply("2026-10-25T20:00:00Z")).unwrap();
        let srcs = vec![Source { agent: Agent::Claude, who: "sub".into(), dir: acct.clone() }];
        let acct_s = acct.to_string_lossy().into_owned();
        let used = |v: &serde_json::Value| v["wins"].as_array().unwrap().iter().map(|w| w["used"].as_f64().unwrap()).collect::<Vec<_>>();

        // First time: send, with the account name, dir, and each window's numbers and time
        record(&payload("a", &fresh, 7.0, 48.0), &store, t("2026-10-26T01:00:05Z"));
        let first = changed(&store, &srcs, &acct_s).unwrap();
        assert_eq!(
            (first["tool"].as_str(), first["name"].as_str(), first["dir"].as_str()),
            (Some("claude"), Some("sub"), Some(acct_s.as_str()))
        );
        assert_eq!(used(&first), vec![7.0, 48.0]);
        assert_eq!((first["wins"][0]["label"].as_str(), first["wins"][0]["resets"].as_i64()), (Some("5h"), Some(5000)));
        assert_eq!(first["wins"][0]["at"].as_i64(), Some(t("2026-10-26T01:00:00Z")));
        // Same numbers reported again: don't send
        assert_eq!(changed(&store, &srcs, &acct_s), None);
        // An idle old session's first report of the stale numbers it holds: its own numbers moved, the account's merged copy didn't
        // — don't send
        record(&payload("b", &idle, 3.0, 40.0), &store, t("2026-10-26T01:01:00Z"));
        assert_eq!(changed(&store, &srcs, &acct_s), None);
        // Really moved: send the new ones
        record(&payload("a", &fresh, 9.0, 48.0), &store, t("2026-10-26T01:10:00Z"));
        assert_eq!(changed(&store, &srcs, &acct_s).map(|v| used(&v)), Some(vec![9.0, 48.0]));
        // The file recording what was last sent isn't .json: merging won't take it for a session
        let q = quotas(&store, &srcs, t("2026-10-26T01:10:00Z"));
        let seen = std::fs::read_dir(&store).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".last")).count();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!((q.len(), seen), (1, 1));
    }

    #[test]
    fn codex_quotas_come_from_the_tail_of_its_latest_record_and_go_out_once_per_change() {
        let home = std::env::temp_dir().join(format!("aca-limits-codex-{}", std::process::id()));
        let day = home.join(".codex/sessions/2026/10/26");
        std::fs::create_dir_all(&day).unwrap();
        let rate = |used: f64, ts: &str| {
            format!(
                r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{used},"window_minutes":300,"resets_at":1793000000}},"secondary":{{"used_percent":5.0,"window_minutes":10080,"resets_at":1793500000}},"plan_type":"plus"}}}}}}"#
            ) + "\n"
        };
        let file = day.join("rollout-2026-10-26T01-00-00-a.jsonl");
        std::fs::write(&file, rate(30.0, "2026-10-26T01:00:00Z") + &rate(33.0, "2026-10-26T01:05:00Z")).unwrap();
        let h = home.to_string_lossy().into_owned();
        // Unregistered default dir: the name is the directory name without the dot; both windows have the last entry's numbers and time
        let got = codex_all(&[], &h);
        assert_eq!(got.len(), 1);
        let (dir, q) = &got[0];
        let v = account_json("codex", &q.who, &dir.to_string_lossy(), &codex_wins(q));
        let wins: Vec<(&str, f64, i64, i64)> = v["wins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (w["label"].as_str().unwrap(), w["used"].as_f64().unwrap(), w["resets"].as_i64().unwrap(), w["at"].as_i64().unwrap()))
            .collect();
        let at = parse_ts("2026-10-26T01:05:00Z");
        assert_eq!((v["tool"].as_str(), v["name"].as_str()), (Some("codex"), Some("codex")));
        assert_eq!(wins, vec![("5h", 33.0, 1793000000, at), ("7d", 5.0, 1793500000, at)]);
        // A turn finished with numbers unchanged: don't send; changed: send once
        let store = home.join("limits");
        std::fs::create_dir_all(&store).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert!(differs(&store, "codex", &d, &codex_wins(q)));
        assert!(!differs(&store, "codex", &d, &codex_wins(q)));
        std::fs::write(&file, rate(33.0, "2026-10-26T01:05:00Z") + &rate(41.0, "2026-10-26T01:20:00Z")).unwrap();
        let (_, q2) = codex_all(&[], &h).pop().unwrap();
        assert!(differs(&store, "codex", &d, &codex_wins(&q2)));
        // Same dir, Claude and Codex keep two separate copies
        assert!(differs(&store, "claude", &d, &codex_wins(&q2)));
        // Logged in with an API key: no subscription quotas, not listed
        std::fs::write(home.join(".codex/auth.json"), r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-x"}"#).unwrap();
        let none = codex_all(&[], &h);
        std::fs::remove_dir_all(&home).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn the_last_reply_is_found_behind_a_huge_tool_output() {
        let p = std::env::temp_dir().join(format!("aca-last-{}.jsonl", std::process::id()));
        let big = format!("{{\"type\":\"user\",\"x\":\"{}\"}}\n", "a".repeat(700 << 10));
        let nested = "{\"type\":\"user\",\"toolUseResult\":{\"type\":\"assistant\",\"timestamp\":\"2030-01-01T00:00:00Z\"}}\n";
        std::fs::write(&p, reply("2026-09-30T01:00:00Z") + &reply("2026-09-30T02:00:00Z") + &big + nested + &big).unwrap();
        let got = last_reply(&p);
        std::fs::write(&p, "{\"type\":\"user\"}\n").unwrap();
        let none = last_reply(&p);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(got, Some(parse_ts("2026-09-30T02:00:00Z")));
        assert_eq!(none, None);
    }
}
