//! The mod aca passes into Claude: a little code that runs inside Claude Code's own process (it calls them function hooks,
//! available on 2.1.295; the interface is still early access and may change with any release). Like hooks it's **passed at
//! startup** (`--plugin-dir <state>/hooks/claude-mod/aca`), not installed into any config directory. Switched on with
//! `[tools.claude] mod = true`; without it, it isn't passed.
//!
//! For now it does one thing: every time Claude measures usage (`session.measure`: on startup, at the end of each turn,
//! whenever the limits move by a point) it hands that as-is to `aca limits --mod`, so subscription limits no longer depend
//! on the statusLine in its config directory. The statusLine path stays as it is, so if this side breaks, the worst case is
//! being back to running without the mod.
//!
//! The interface will change, so:
//! - **The mod only forwards** (`claude_mod.js`): one event, one `$` call. Parsing the format is on aca's side (aca-tui's
//!   `limits.rs`); when it changes, fix the Rust, not the mod
//! - **Before starting, let this Claude version check it itself** (`claude plugin validate --json`, 0.6 seconds): recorded
//!   once per program file; a version that fails doesn't get it. It catches a renamed or removed event; it doesn't catch a
//!   renamed `$` call or renamed event fields (tried on 2.1.295), which the next rule covers
//! - **Reconciling**: a session started with it has finished a turn (the `done` the hook records; that path is reliable) yet
//!   never reported once, and no session on this version has ever reported → record this version as not reporting and
//!   don't pass it from then on. Reports that arrive but can't be parsed are recorded by the receiving end on the spot
//!   ([`heard`])
//!
//! Kept in `<state>/mod/`: `builds/<this version's key>.json` is the verdict, `up/<session>` is which version it was started
//! with and when, `heard/<session>` means it has reported. Claude itself also puts type declarations in the mod's directory
//! (`.claude-plugin/types/`, under 1 MB).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Tool;
use crate::{paths, runtime};
use crate::{t, tr};

const MANIFEST: &str = "{ \"name\": \"aca\", \"version\": \"0.1.0\", \"description\": \"aca 起会话时带进来的:把用量交给 aca 记下\", \"author\": { \"name\": \"aca\" } }\n";
const HOOKS: &str = "{ \"modules\": [\"./register.js\"] }\n";
const MODULE: &str = include_str!("claude_mod.js");
/// The event the mod hooks: when checking, we need to see this version really recognizes it
const EVENT: &str = "session.measure";
/// Claude measures once right on startup (arrives within 2 seconds in tests); only a session up this long that has finished
/// a turn without reporting counts
const GRACE: u64 = 20;
const TIMEOUT: Duration = Duration::from_secs(5);
/// `up` / `heard` left by sessions that are gone are deleted after this long
const STALE: u64 = 30 * 86400;

/// Whether a Claude version gets this mod
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Verdict {
    /// The program file's name: it installs files named by version, so usually the version number
    pub build: String,
    pub ok: bool,
    /// Why it isn't passed
    pub why: String,
    pub at: u64,
}

fn root() -> PathBuf {
    paths::state_dir().join("mod")
}

fn verdict_file(key: &str) -> PathBuf {
    root().join("builds").join(format!("{key}.json"))
}

fn mark(dir: &str, id: &str) -> PathBuf {
    root().join(dir).join(id)
}

/// What `up/<session>` records: (which version, when it started)
fn up(id: &str) -> Option<(String, u64)> {
    let text = std::fs::read_to_string(mark("up", id)).ok()?;
    let (key, at) = text.trim().split_once(' ')?;
    Some((key.to_string(), at.parse().ok()?))
}

fn heard_from(id: &str) -> String {
    std::fs::read_to_string(mark("heard", id)).unwrap_or_default().trim().to_string()
}

fn load(key: &str) -> Option<Verdict> {
    serde_json::from_str(&std::fs::read_to_string(verdict_file(key)).ok()?).ok()
}

fn save(key: &str, v: &Verdict) {
    let _ = paths::write_atomic(&verdict_file(key), &serde_json::to_string(v).unwrap_or_default());
}

/// Change this version's verdict to "not passed" (only if it was recorded as passable: the first reason is kept)
fn refuse(key: &str, why: &str) {
    if let Some(v) = load(key).filter(|v| v.ok) {
        save(key, &Verdict { ok: false, why: why.into(), at: paths::now(), ..v });
    }
}

/// This Claude version's key: the program file's (the link target's) path, size and modification time, plus the mod's own
/// content. When it upgrades or the mod changes, the key changes and it's checked again. The second item is a name for people
fn build(program: &str) -> Option<(String, String)> {
    let real = std::fs::canonicalize(program).ok()?;
    let meta = std::fs::metadata(&real).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let mut h = DefaultHasher::new();
    (&real, meta.len(), mtime, MODULE, HOOKS, MANIFEST).hash(&mut h);
    Some((format!("{:016x}", h.finish()), real.file_name()?.to_string_lossy().into_owned()))
}

/// `claude plugin validate --json`'s report → whether this version accepts the mod. Passing isn't enough; it must actually
/// list that event: versions without function hooks ignore `modules` in hooks.json, so they report no error
fn judge(report: &Value) -> Result<(), String> {
    let parts: Vec<&Value> = std::iter::once(&report["manifest"]).chain(report["contents"].as_array().into_iter().flatten()).collect();
    let list = |key: &str| parts.iter().flat_map(move |p| p[key].as_array().into_iter().flatten()).collect::<Vec<_>>();
    if let Some(e) = list("errors").first() {
        // The error carries the file's full path; keep only the last part, which says what's wrong
        let msg = e["message"].as_str().unwrap_or("").split(';').next().unwrap_or("");
        return Err(msg.rsplit(": ").next().unwrap_or(msg).trim().to_string());
    }
    if report["success"] != true {
        return Err(t!("plugin validate 未通过", "its plugin validate didn't say it passed").into());
    }
    let hooked = list("notes").iter().filter_map(|n| n.as_str()).any(|n| n.contains("hooks:") && n.contains(EVENT));
    hooked.then_some(()).ok_or_else(|| tr!("此版本无法识别 {EVENT} hook", "this version didn't recognize the {EVENT} hook"))
}

/// Have this Claude version check it. Give it a config directory just for this check: it writes `.claude.json` into the config
/// directory as soon as it runs, and we don't want that written into yours
fn validate(program: &str, dir: &Path) -> Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(["plugin", "validate", "--json"]).arg(dir).env("CLAUDE_CONFIG_DIR", root().join("validate-home"));
    for k in super::FOREIGN {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| tr!("无法运行 {program}:{e}", "can't run {program}: {e}"))?;
    let mut out = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + TIMEOUT;
    while !matches!(child.try_wait(), Ok(Some(_))) {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(tr!("验证时 {} 秒内无响应", "no answer within {} seconds while validating", TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let report: Value = serde_json::from_str(&reader.join().unwrap_or_default())
        .map_err(|_| t!("plugin validate 未返回报告", "its plugin validate gave no report").to_string())?;
    judge(&report)
}

/// The mod's directory: write it (untouched if the content is the same — running sessions watch this directory and reload
/// on any change), return the path
fn write() -> Option<PathBuf> {
    let at = |name: &str| format!("claude-mod/aca/{name}");
    let manifest = super::hook_file(&at(".claude-plugin/plugin.json"), MANIFEST).ok()?;
    super::hook_file(&at("hooks/hooks.json"), HOOKS).ok()?;
    super::hook_file(&at("hooks/register.js"), MODULE).ok()?;
    Some(manifest.parent()?.parent()?.to_path_buf())
}

/// The reconciling half, pure: `ups` is the sessions started with this version (start time, last time a turn finished,
/// whether it ever reported). Some session up long enough has since finished a turn without reporting, and no session has
/// ever reported = this version doesn't report
fn silent(ups: &[(u64, u64, bool)]) -> bool {
    ups.iter().all(|u| !u.2) && ups.iter().any(|&(at, done, _)| done >= at + GRACE)
}

/// Go through `up/`: whether this version doesn't report; delete stale entries nobody cares about along the way
fn reconcile(key: &str, now: u64) -> bool {
    let Ok(rd) = std::fs::read_dir(root().join("up")) else { return false };
    let mut ups = vec![];
    for e in rd.flatten() {
        let id = e.file_name().to_string_lossy().into_owned();
        let Some((k, at)) = up(&id) else { continue };
        if now.saturating_sub(at) > STALE && !paths::run_file(&id).exists() {
            let _ = std::fs::remove_file(e.path());
            let _ = std::fs::remove_file(mark("heard", &id));
        } else if k == key {
            ups.push((at, runtime::load(&id).done, heard_from(&id) == key));
        }
    }
    silent(&ups)
}

/// Whether to pass the mod when starting this session: if so, return its directory and record which version the session
/// started with. Not passed when: it's off; the tool wraps the command itself (can't know which version runs inside, so the
/// verdict can't be recorded per version); this version fails the check; reconciling found this version doesn't report
pub(crate) fn offer(tool: &Tool, id: &str) -> Option<PathBuf> {
    let found = offered(tool);
    let now = paths::now();
    let _ = match &found {
        Some((_, key)) => paths::write_atomic(&mark("up", id), &format!("{key} {now}\n")),
        None => std::fs::remove_file(mark("up", id)),
    };
    found.map(|(dir, _)| dir)
}

fn offered(tool: &Tool) -> Option<(PathBuf, String)> {
    if !tool.mods || !tool.cmd.is_empty() {
        return None;
    }
    let program = super::which("claude")?;
    let (key, name) = build(&program)?;
    let dir = write()?;
    let now = paths::now();
    let verdict = load(&key).unwrap_or_else(|| {
        let checked = validate(&program, &dir);
        let v = Verdict { build: name, ok: checked.is_ok(), why: checked.err().unwrap_or_default(), at: now };
        save(&key, &v);
        v
    });
    if verdict.ok && reconcile(&key, now) {
        refuse(&key, t!("带它的会话已完成一轮,但未收到任何用量", "a session carrying it finished a turn without reporting any usage"));
        return None;
    }
    verdict.ok.then_some((dir, key))
}

/// Reported by the receiving end (`aca limits --mod`): this session's mod reported once. `understood` = what arrived could be
/// parsed; if not, the event's shape changed, and this version won't get it from now on (it would be no use)
pub fn heard(id: &str, understood: bool) {
    let Some((key, _)) = up(id) else { return };
    if heard_from(id) != key {
        let _ = paths::write_atomic(&mark("heard", id), &format!("{key}\n"));
    }
    if !understood {
        refuse(&key, t!("无法识别它上报的用量(事件格式已变)", "the usage it reports is no longer recognized (the event shape changed)"));
    }
}

/// For people: the verdict for the Claude version now on PATH (not checked yet = None)
pub fn verdict() -> Option<Verdict> {
    load(&build(&super::which("claude")?)?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_build_takes_the_mod_only_when_its_own_validator_lists_the_hook() {
        // The raw report of `claude plugin validate --json` on 2.1.295 (paths shortened)
        let passed = json!({"success": true, "manifest": {"errors": [], "warnings": [{"path": "author", "message": "No author"}], "notes": []},
            "contents": [{"type": "hooks", "errors": [], "notes": ["./register.js hooks: session.measure", "./register.js calls: $.process.run"]}]});
        assert_eq!(judge(&passed), Ok(()));
        // The event was renamed: it says itself that it's not an event
        let renamed = json!({"success": false, "manifest": {"errors": []}, "contents": [{"errors": [{"path": "modules../register.js",
            "message": "aca: /st/hooks/claude-mod/hooks/register.js:5: \"session.measure\" is not an event; $ is always spelled $.noun.event(...)"}], "notes": []}]});
        assert_eq!(judge(&renamed).unwrap_err(), "\"session.measure\" is not an event");
        // A version without function hooks: passes because the manifest is fine, but doesn't list the event
        let old = json!({"success": true, "manifest": {"errors": []}, "contents": []});
        assert!(judge(&old).unwrap_err().contains("session.measure"));
        // Not a report (old versions don't have this command and print usage)
        assert!(judge(&json!({})).is_err());
    }

    #[test]
    fn a_build_is_silent_when_turns_finish_and_nothing_ever_arrives() {
        // Just started, no turn finished yet: doesn't count
        assert!(!silent(&[(100, 0, false)]));
        // A turn finished 5 seconds after starting (resumed a conversation): the mod may not have had time to report, doesn't count
        assert!(!silent(&[(100, 105, false)]));
        // Finished a turn, never reported once
        assert!(silent(&[(100, 160, false)]));
        // Another session has reported: this version works, it's that session's own problem
        assert!(!silent(&[(100, 160, false), (90, 0, true)]));
        assert!(!silent(&[]));
    }

    #[test]
    fn the_shipped_module_stays_a_plain_forwarder() {
        // These two things are what the check looks at; each extra hooked event or `$` call is one more place that changes with
        // the interface
        assert_eq!(MODULE.matches("on('").count(), 1);
        assert!(MODULE.contains(&format!("on('{EVENT}'")));
        assert_eq!(MODULE.matches("$.").count(), 1);
        assert!(serde_json::from_str::<Value>(MANIFEST).is_ok() && serde_json::from_str::<Value>(HOOKS).is_ok());
    }
}
