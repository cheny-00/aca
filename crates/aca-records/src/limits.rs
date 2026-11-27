//! Subscription quotas, with "when these numbers are from". Antigravity's title reports use
//! the same per-session window timestamps as Claude's status bar, without a transcript lookup.
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
//! Each report is the whole set of windows at that moment and replaces what the session held, so the panel follows the plan by
//! itself: a window a newer report doesn't have is left out (2026-11 a Max 20x account reported no 5h), one that comes (back) shows
//! with its first report. A session carried on under another account starts over: the old account's windows aren't the new one's.
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

use memchr::memmem::Finder;
use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::usage::{self, Quota, Source, Win, parse_ts};

/// What one session recorded
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Rec {
    /// Account dir (the transcripts are under its projects/)
    dir: String,
    wins: Vec<RWin>,
    /// Antigravity reports can omit the plan even when they have fresh quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agy: Option<AgyReport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AgyReport {
    at: i64,
    plan: Option<(u64, String)>,
}

impl Rec {
    fn read(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        aca_core::paths::write_atomic(path, &serde_json::to_string(self)?)
    }
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
    crate::paths::state_file("limits")
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
    let l = aca_core::launch::direct(cfg, spec, &args).ok()?;
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

/// The windows a session reported this time → its file; acct is its account dir. `live` = these numbers are current (handed
/// over by the mod, asked); otherwise the time is inferred from the transcript per the rules at the top of the file.
/// What's reported replaces what the file held: a window left out this time is gone. What the file held for another account
/// (the conversation carried on under another one, `aca restart --as`) isn't compared with.
/// Returns whether any window's numbers changed (a first report and a window gone count)
fn update(path: &Path, got: Vec<Got>, acct: &str, transcript: Option<&Path>, now: i64, live: bool) -> bool {
    if got.is_empty() {
        return false;
    }
    let old = Rec::read(path);
    let last = if live { None } else { transcript.and_then(last_reply) };
    let (rec, moved) = updated(&old, got, acct, last, now, live);
    if rec != old {
        let _ = rec.save(path);
    }
    moved
}

fn updated(old: &Rec, got: Vec<Got>, acct: &str, last: Option<i64>, now: i64, live: bool) -> (Rec, bool) {
    let held: &[RWin] = if old.dir == acct { &old.wins } else { &[] };
    let mut rec = Rec { dir: acct.to_string(), ..Default::default() };
    let mut moved = false;
    for (label, used, resets) in got {
        let before = held.iter().find(|w| w.label == label);
        moved |= before.is_none_or(|w| w.used != used || w.resets != resets);
        let at = match before {
            _ if live => now,
            Some(w) if w.used == used && w.resets == resets => w.at.max(last.unwrap_or(0)),
            Some(_) => now,
            None => last.unwrap_or(now),
        };
        rec.wins.retain(|w| w.label != label);
        rec.wins.push(RWin { label, used, resets, at });
    }
    moved |= held.iter().any(|h| rec.wins.iter().all(|w| w.label != h.label));
    rec.wins.sort_by_key(|w| order(&w.label));
    (rec, moved)
}

/// Called under the title hook's session lock. Only the bare agy session uses the Google quota.
pub fn record_agy(id: &str, limits: &aca_core::agent::AgyLimits) -> bool {
    if !plain(id) {
        return false;
    }
    let Ok(session) = aca_core::store::Store::open().and_then(|st| st.get(id)) else { return false };
    if session.tool != "agy" || !session.account.is_empty() {
        return true;
    }
    let Some(dir) = dir() else { return false };
    let home = aca_core::paths::home();
    let Some(src) = Source::agy(&aca_core::config::load(), &home.to_string_lossy(), aca_core::agent::on_path) else { return false };
    // Two sessions may redraw together. Serialize the merged comparison and the event it publishes.
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let Ok(lock) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("agy.lock")) else { return false };
    if lock.lock().is_err() {
        return false;
    }
    match save_agy(&dir.join(format!("agy.{id}.json")), &src, limits, aca_core::paths::now_ms()) {
        Ok(true) => emit_change(changed_for(&dir, &[src], "agy", "agy")),
        Ok(false) => {}
        Err(_) => return false,
    }
    true
}

fn save_agy(file: &Path, src: &Source, limits: &aca_core::agent::AgyLimits, at_ms: u64) -> std::io::Result<bool> {
    let now = (at_ms / 1000) as i64;
    let acct = src.dir.to_string_lossy();
    let old = Rec::read(file);
    let got = limits.windows.iter().map(|&(label, used, reset)| (label.into(), used, Some(reset))).collect();
    let (mut rec, moved) = updated(&old, got, &acct, None, now, false);
    let plan = if limits.plan.is_empty() { old.agy.as_ref().and_then(|a| a.plan.clone()) } else { Some((at_ms, limits.plan.clone())) };
    // A plan-only redraw must not retire third-party windows a fresher session has started using.
    let at = old.agy.as_ref().filter(|_| !moved).map_or(now, |a| a.at);
    rec.agy = Some(AgyReport { at, plan });
    if rec != old {
        rec.save(file)?;
    }
    Ok(moved)
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
    quotas(dir, srcs, now).into_iter().find(|q| q.agent == Agent::Claude && q.who == who)
}

/// Which path a copy came by: the status bar and the mod hand over the same windows (5h, 7d); asking Claude also gives the
/// per-model weekly ones (Fable)
#[derive(Clone, Copy, PartialEq)]
enum Via {
    Session,
    Ask,
}

/// One account while merging
struct Acc {
    dir: String,
    plan: Option<(u64, String)>,
    /// Per window, the newest copy and the path it came by
    wins: HashMap<String, (RWin, Via)>,
    /// Every file's report: its path, the time of its data (its newest window), the windows it has
    reports: Vec<(Via, i64, Vec<String>)>,
}

/// A report has every window its path knows of at that moment (Claude's statusLine docs: a window is "present only while the API
/// reports it and its resets_at has not passed"; asked, a window it doesn't have is null), so a window is gone once a newer
/// report that would have it doesn't. 5h and 7d come by every path; the others only by the path that recorded them
fn gone(label: &str, w: &RWin, via: Via, reports: &[(Via, i64, Vec<String>)]) -> bool {
    let everywhere = label == "5h" || label == "7d";
    reports.iter().any(|(v, at, has)| *at > w.at && (everywhere || *v == via) && has.iter().all(|l| l != label))
}

/// One account's merged copy
struct Merged {
    tool: &'static str,
    plan: String,
    /// Account name (as in billing)
    who: String,
    /// Account dir (recorded from what its sessions reported)
    dir: String,
    wins: Vec<RWin>,
}

/// Per account per window, the copy with the newest time (on a tie, the later reset and higher usage), sorted by account name;
/// leaving out windows a newer report no longer has (see [`gone`]), so the panel follows the plan: a window the account stops
/// having goes away, and one it gets (back) shows up with its first report.
/// Account names are the same as in billing (matched to srcs by account dir); also deletes session files untouched for eight days
fn merged(dir: &Path, srcs: &[Source]) -> Vec<Merged> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let names = names(srcs);
    let mut best: HashMap<(&str, String), Acc> = HashMap::new();
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
        let via = if e.file_name().to_string_lossy().starts_with("ask-") { Via::Ask } else { Via::Session };
        let key = if rec.agy.is_some() {
            let Some(s) = srcs.iter().find(|s| s.agent == Agent::Agy && s.dir == Path::new(&rec.dir)) else { continue };
            ("agy", s.who.clone())
        } else {
            ("claude", who_of(&names, &rec.dir))
        };
        let acc = best.entry(key).or_insert_with(|| Acc { dir: rec.dir, plan: None, wins: HashMap::new(), reports: vec![] });
        let at = rec.agy.as_ref().map(|a| a.at).or_else(|| rec.wins.iter().map(|w| w.at).max()).unwrap_or(i64::MIN);
        if let Some(a) = rec.agy {
            acc.plan = acc.plan.take().max(a.plan);
        }
        acc.reports.push((via, at, rec.wins.iter().map(|w| w.label.clone()).collect()));
        for w in rec.wins {
            let newer = |(o, _): &(RWin, Via)| (w.at, w.resets, w.used) > (o.at, o.resets, o.used);
            if acc.wins.get(&w.label).is_none_or(newer) {
                acc.wins.insert(w.label.clone(), (w, via));
            }
        }
    }
    let mut out: Vec<Merged> = best
        .into_iter()
        .map(|((tool, who), Acc { dir, plan, wins, reports })| {
            let mut wins: Vec<RWin> =
                wins.into_iter().filter(|(label, (w, via))| !gone(label, w, *via, &reports)).map(|(_, (w, _))| w).collect();
            wins.sort_by_key(|w| order(&w.label));
            Merged { tool, who, dir, wins, plan: plan.map(|p| p.1).unwrap_or_default() }
        })
        .collect();
    out.sort_by(|a, b| (&a.who, a.tool).cmp(&(&b.who, b.tool)));
    out
}

/// For the panel: each account's merged copy (see [`merged`])
pub fn quotas(dir: &Path, srcs: &[Source], now: i64) -> Vec<Quota> {
    merged(dir, srcs)
        .into_iter()
        .map(|m| Quota {
            agent: crate::agent::kind_of(m.tool),
            who: m.who,
            plan: m.plan,
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
    let cfg = aca_core::config::load();
    let accounts = crate::agent::accounts(&cfg);
    let srcs = usage::quota_sources(&cfg, &accounts, &home);
    let mut out: Vec<serde_json::Value> = vec![];
    if let Some(d) = dir() {
        out.extend(merged(&d, &srcs).iter().map(|m| account_json(m.tool, &m.who, &m.dir, &m.wins)));
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
    let _ = crate::paths::write_atomic(&file, &key);
    true
}

/// A Claude account just recorded and some session's numbers moved: if the merged copy differs from the last sent, return the
/// fields of `limits.changed`.
/// A session's own numbers moving doesn't mean the account's did (an idle old session's first report of the stale numbers it
/// holds loses to newer ones in the merge)
fn changed(dir: &Path, srcs: &[Source], acct: &str) -> Option<serde_json::Value> {
    changed_for(dir, srcs, "claude", &who_of(&names(srcs), acct))
}

fn changed_for(dir: &Path, srcs: &[Source], tool: &str, who: &str) -> Option<serde_json::Value> {
    let m = merged(dir, srcs).into_iter().find(|m| m.tool == tool && m.who == who)?;
    differs(dir, tool, &m.dir, &m.wins).then(|| account_json(tool, &m.who, &m.dir, &m.wins))
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
    let srcs = usage::sources(&crate::agent::accounts(&aca_core::config::load()), &home);
    emit_change(changed(dir, &srcs, acct));
}

fn emit_change(change: Option<serde_json::Value>) {
    if let Some(data) = change {
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
    let accounts = crate::agent::accounts(&aca_core::config::load());
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

    #[test]
    fn agy_windows_merge_with_their_own_timestamps_and_only_active_third_party_limits() {
        let root = std::env::temp_dir().join(aca_core::paths::short_id());
        let src = Source { agent: Agent::Agy, who: "agy".into(), dir: root.join(".gemini") };
        let srcs = std::slice::from_ref(&src);
        let mut title = serde_json::json!({"plan_tier":"Google AI Pro","quota":{
            "3p-5h":{"remaining_fraction":1,"reset_time":"1970-01-01T00:00:00Z","reset_in_seconds":0},
            "3p-weekly":{"remaining_fraction":1,"reset_time":"1970-01-01T00:00:00Z","reset_in_seconds":0},
            "gemini-5h":{"remaining_fraction":0.9871378,"reset_time":"2026-11-25T08:26:00Z","reset_in_seconds":17716},
            "gemini-weekly":{"remaining_fraction":0.98928446,"reset_time":"2026-12-02T03:26:00Z","reset_in_seconds":604516}
        }});
        let save = |id: &str, v: &serde_json::Value, at| {
            save_agy(&root.join(format!("agy.{id}.json")), &src, &aca_core::agent::AgyLimits::from_title(v).unwrap(), at * 1000u64).unwrap()
        };
        assert!(save("a", &title, 100));
        let q = &quotas(&root, srcs, 200)[0];
        assert_eq!((q.agent, q.who.as_str(), q.plan.as_str()), (Agent::Agy, "agy", "Google AI Pro"));
        assert_eq!(q.wins.len(), 2);
        assert!((q.wins[0].used - 1.28622).abs() < 1e-9 && (q.wins[1].used - 1.071554).abs() < 1e-9);
        assert_eq!(q.wins[0].resets, Some(parse_ts("2026-11-25T08:26:00Z")));
        let mut old_title = title.clone();
        let quota = &mut title["quota"];
        quota["3p-5h"] = quota["gemini-5h"].clone();
        quota["3p-weekly"] = quota["gemini-weekly"].clone();
        quota["gemini-5h"]["remaining_fraction"] = 0.5.into();
        title["plan_tier"] = "Google AI Ultra".into();
        assert!(save("b", &title, 110));
        title.as_object_mut().unwrap().remove("plan_tier");
        title["quota"]["gemini-5h"]["remaining_fraction"] = 0.75.into();
        assert!(save("a", &title, 120));
        let m = changed_for(&root, srcs, "agy", "agy").unwrap();
        let wins = m["wins"].as_array().unwrap();
        assert_eq!(wins.iter().map(|w| w["label"].as_str().unwrap()).collect::<Vec<_>>(), ["5h", "7d", "3p 5h", "3p 7d"]);
        assert_eq!((wins[0]["used"].as_f64(), wins[0]["at"].as_i64(), wins[1]["at"].as_i64()), (Some(25.0), Some(120), Some(110)));
        assert_eq!(Rec::read(&root.join("agy.a.json")).wins[1].at, 100);
        assert_eq!(quotas(&root, srcs, 200)[0].plan, "Google AI Ultra");
        assert!(changed_for(&root, srcs, "agy", "agy").is_none());
        assert!(save("c", &old_title, 105));
        old_title["plan_tier"] = "New plan".into();
        assert!(!save("c", &old_title, 130));
        let latest = &quotas(&root, srcs, 200)[0];
        assert_eq!((latest.plan.as_str(), latest.wins.len()), ("New plan", 4));
        assert!(changed_for(&root, srcs, "agy", "agy").is_none());
        assert!(quotas(&root, &[], 200).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

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
    fn windows_follow_what_the_newest_report_has() {
        let root = std::env::temp_dir().join(format!("aca-limits-gone-{}", std::process::id()));
        let (sub, ding) = (root.join(".claude-sub"), root.join(".claude-alt"));
        let (ps, pd) = (sub.join("projects/-x"), ding.join("projects/-x"));
        std::fs::create_dir_all(&ps).unwrap();
        std::fs::create_dir_all(&pd).unwrap();
        let store = root.join("limits");
        let t = |s: &str| parse_ts(s);
        let week_only = |sid: &str, transcript: &Path, seven: f64| {
            serde_json::json!({"session_id": sid, "transcript_path": transcript,
                "rate_limits": {"seven_day": {"used_percentage": seven, "resets_at": 9000}}})
            .to_string()
            .into_bytes()
        };
        let srcs = vec![
            Source { agent: Agent::Claude, who: "sub".into(), dir: sub.clone() },
            Source { agent: Agent::Claude, who: "ding".into(), dir: ding.clone() },
        ];
        let shown = |who: &str| -> Vec<(String, f64)> {
            let q = quotas(&store, &srcs, t("2026-11-17T12:00:00Z"));
            q.into_iter().find(|q| q.who == who).map(|q| q.wins.into_iter().map(|w| (w.label, w.used)).collect()).unwrap_or_default()
        };
        let file = |sid: &str| -> Rec { serde_json::from_slice(&std::fs::read(store.join(format!("{sid}.json"))).unwrap()).unwrap() };

        // a: an idle session from before the plan dropped 5h; b, newer, has no 5h: the account has none
        let (a, b) = (ps.join("a.jsonl"), ps.join("b.jsonl"));
        std::fs::write(&a, reply("2026-11-17T01:00:00Z")).unwrap();
        std::fs::write(&b, reply("2026-11-17T02:00:00Z")).unwrap();
        record(&payload("a", &a, 20.0, 40.0), &store, t("2026-11-17T01:00:05Z"));
        record(&week_only("b", &b, 41.0), &store, t("2026-11-17T02:00:05Z"));
        assert_eq!(shown("sub"), vec![("7d".into(), 41.0)]);
        // 5h comes back: shown with its first report
        std::fs::write(&b, reply("2026-11-17T03:00:00Z")).unwrap();
        record(&payload("b", &b, 1.0, 42.0), &store, t("2026-11-17T03:00:05Z"));
        assert_eq!(shown("sub"), vec![("5h".into(), 1.0), ("7d".into(), 42.0)]);
        // Gone again: b's own file drops it too (a report replaces what the file held), and a's older copy stays out
        record(&week_only("b", &b, 42.0), &store, t("2026-11-17T03:10:00Z"));
        assert_eq!(file("b").wins.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), vec!["7d"]);
        assert_eq!(shown("sub"), vec![("7d".into(), 42.0)]);

        // A session carried on under another account (`aca restart --as`): the old account's 5h doesn't come along, even in its own file
        let (cd, cs) = (pd.join("c.jsonl"), ps.join("c.jsonl"));
        std::fs::write(&cd, reply("2026-11-17T04:00:00Z")).unwrap();
        std::fs::write(&cs, reply("2026-11-17T04:00:00Z")).unwrap();
        record(&payload("c", &cd, 31.0, 56.0), &store, t("2026-11-17T04:00:05Z"));
        assert_eq!(shown("ding"), vec![("5h".into(), 31.0), ("7d".into(), 56.0)]);
        record(&week_only("c", &cs, 42.0), &store, t("2026-11-17T04:10:00Z"));
        let c = file("c");
        assert_eq!((c.dir.as_str(), c.wins.len()), (sub.to_str().unwrap(), 1));
        assert_eq!(shown("sub"), vec![("7d".into(), 42.0)]);
        // Even the same numbers count as the new account's first report (it moved: announced, asked)
        let (sub_s, ding_s) = (sub.to_string_lossy().into_owned(), ding.to_string_lossy().into_owned());
        let m = root.join("moved/m.json");
        assert!(update(&m, vec![("7d".into(), 56.0, Some(9000))], &ding_s, None, 100, true));
        assert!(update(&m, vec![("7d".into(), 56.0, Some(9000))], &sub_s, None, 200, true));

        // The per-model week only comes by asking: a newer status bar report without it doesn't take it away, a newer answer does
        let ask = ask_files(&store, &sub_s).0;
        let fable = |used: f64| ("Fable".to_string(), used, Some(9000));
        update(&ask, vec![("7d".into(), 42.0, Some(9000)), fable(5.0)], &sub_s, None, t("2026-11-17T05:00:00Z"), true);
        std::fs::write(&b, reply("2026-11-17T05:30:00Z")).unwrap();
        record(&week_only("b", &b, 43.0), &store, t("2026-11-17T05:30:05Z"));
        assert_eq!(shown("sub"), vec![("7d".into(), 43.0), ("Fable".into(), 5.0)]);
        update(&ask, vec![("7d".into(), 43.0, Some(9000))], &sub_s, None, t("2026-11-17T06:00:00Z"), true);
        let last = shown("sub");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(last, vec![("7d".into(), 43.0)]);
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
