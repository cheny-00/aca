//! Which directory a session is really working in (for the diff page and the stats atop the preview).
//!
//! The registered path is the one given when the session was created, and Claude's process cwd stays where it started too; but
//! when Claude has cd'd inside, entered a worktree, or the registered path is a level up (several repositories under it), what's
//! really being changed is elsewhere. So the agent's own records win:
//! - Claude: `$CLAUDE_CONFIG_DIR/sessions/<pid>.json` gives its current session id → the `projects/*/<id>.jsonl` conversation
//!   record, where the last entry with a `cwd` is its current directory (it follows a cd in Bash or entering a worktree)
//! - Codex: the registered conversation id (reported by the hook) → `$CODEX_HOME/sessions/**/rollout-*-<id>.jsonl`, the last `cwd`
//! - If that directory isn't in a git repository (e.g. it stayed a level up): the repository of the file it changed most recently
//!   (Edit / Write, apply_patch)
//! - None of those: the agent process's cwd, failing that the registered directory
//!
//! Reads only the tail of the file, never parses a whole record; nothing from the record is kept but paths.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use aca_core::t;

use crate::data::Agent;

/// Where to look: these fields of a session (from `Session`)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Where {
    /// The registered directory (for an unregistered one, the pane's current directory)
    pub dir: String,
    /// The agent process, 0 = not running
    pub pid: u32,
    pub agent: Agent,
    /// The registered Claude / Codex conversation id (the one `B` copies)
    pub id: String,
}

impl Where {
    pub fn of(s: &crate::data::Session) -> Where {
        let id = s.info.lines().find_map(|l| l.strip_prefix("Session: ")).unwrap_or("").to_string();
        Where { dir: s.dir.clone(), pid: s.pid, agent: s.agent, id }
    }
}

/// The directory found, and how it was found (shown when it differs from the registered one)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub dir: String,
    pub how: &'static str,
}

/// Read at most this much of a conversation record's tail
const TAIL: u64 = 2 << 20;

static CWD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""cwd":"((?:[^"\\]|\\.)*)""#).unwrap());
/// Claude changing files: Edit / Write input, and the filePath returned afterwards
static CLAUDE_EDIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""name":"(?:Edit|Write|MultiEdit|NotebookEdit)","input":\{[^{}]*?"(?:file_path|notebook_path)":"((?:[^"\\]|\\.)*)"|"filePath":"((?:[^"\\]|\\.)*)""#)
        .unwrap()
});
/// Codex changing files: `*** Update File: PATH` in apply_patch (inside a JSON string, newlines are \n)
static CODEX_EDIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\*\*\* (?:Update|Add) File: ((?:[^"\\]|\\[^n])+)"#).unwrap());

fn unescape(raw: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string())
}

/// The tail of a file (starting at a line start)
fn tail(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let start = if from > 0 { buf.iter().position(|&b| b == b'\n').map_or(buf.len(), |i| i + 1) } else { 0 };
    Some(String::from_utf8_lossy(&buf[start..]).into_owned())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// How Claude turns a project directory into a folder name under projects: everything not alphanumeric becomes -
fn project_key(dir: &str) -> String {
    dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

static SID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""sessionId":"([^"]+)""#).unwrap());

/// Claude's conversation record: first the current session id from sessions/<pid>.json (it changes after /clear, resume), else
/// the registered one
fn claude_log(conf: &Path, pid: u32, id: &str) -> Option<PathBuf> {
    let reg = std::fs::read_to_string(conf.join("sessions").join(format!("{pid}.json"))).unwrap_or_default();
    let sid = SID.captures(&reg).map(|c| c[1].to_string()).filter(|_| pid > 0).unwrap_or_else(|| id.to_string());
    if sid.is_empty() || sid.contains('/') {
        return None;
    }
    let file = format!("{sid}.jsonl");
    let projects = conf.join("projects");
    // Usually in the folder for the start directory; when the name is too long Claude truncates it and adds a hash, so search
    // them one by one
    let start = CWD.captures(&reg).map(|c| unescape(&c[1]));
    if let Some(p) = start.map(|d| projects.join(project_key(&d)).join(&file)).filter(|p| p.is_file()) {
        return Some(p);
    }
    std::fs::read_dir(&projects).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file())
}

/// Codex's rollout: find it by session id, going back from the most recent date
pub(crate) fn codex_log(conf: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    let sorted = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|d| d.path()).collect();
        v.sort();
        v.reverse();
        v
    };
    let tail_id = format!("-{id}.jsonl");
    for y in sorted(&conf.join("sessions")) {
        for m in sorted(&y) {
            for d in sorted(&m) {
                if let Some(f) = sorted(&d).into_iter().find(|f| f.to_str().is_some_and(|s| s.ends_with(&tail_id))) {
                    return Some(f);
                }
            }
        }
    }
    None
}

/// The record file of its current conversation. conf = config directory (CLAUDE_CONFIG_DIR / CODEX_HOME), empty = the default
/// ~/.claude / ~/.codex
pub fn log_of(w: &Where, conf: &str) -> Option<PathBuf> {
    let dir = |d: &str| if conf.is_empty() { home().join(d) } else { PathBuf::from(conf) };
    match w.agent {
        Agent::Claude => claude_log(&dir(".claude"), w.pid, &w.id),
        Agent::Codex => codex_log(&dir(".codex"), &w.id),
        _ => None,
    }
}

/// The root of the containing git repository (looks up for .git, doesn't run git)
fn repo_root(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|d| d.join(".git").exists())
}

fn in_repo(dir: &Path) -> bool {
    repo_root(dir).is_some()
}

/// Read from the record's tail: the last cwd, the most recently changed file (absolute path)
fn read_log(log: &Path, agent: Agent) -> (Option<String>, Option<String>) {
    let Some(text) = tail(log) else { return (None, None) };
    let cwd = CWD.captures_iter(&text).last().map(|c| unescape(&c[1]));
    let edited = match agent {
        Agent::Codex => CODEX_EDIT.captures_iter(&text).last().map(|c| unescape(c[1].trim_end())),
        _ => CLAUDE_EDIT.captures_iter(&text).last().and_then(|c| c.get(1).or(c.get(2))).map(|m| unescape(m.as_str())),
    };
    // apply_patch paths are often relative: append them to cwd
    let edited = edited.map(|e| match &cwd {
        Some(c) if !e.starts_with('/') => format!("{}/{e}", c.trim_end_matches('/')),
        _ => e,
    });
    (cwd, edited)
}

/// The agent process's cwd (lsof; only readable for our own processes)
fn proc_cwd(pid: u32) -> Option<String> {
    let out = std::process::Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix('n')).map(String::from).filter(|d| d.starts_with('/'))
}

/// Find the directory it's really working in. conf = the agent's config directory (CLAUDE_CONFIG_DIR / CODEX_HOME, None = the
/// default ~/.claude / ~/.codex)
pub fn find_in(w: &Where, conf: Option<&str>) -> Found {
    let registered = || Found { dir: w.dir.clone(), how: "" };
    let log = match w.agent {
        Agent::Claude => claude_log(&conf.map_or_else(|| home().join(".claude"), PathBuf::from), w.pid, &w.id),
        Agent::Codex => codex_log(&conf.map_or_else(|| home().join(".codex"), PathBuf::from), &w.id),
        _ => None,
    };
    let here = if w.agent == Agent::Codex {
        t!("Codex 当前目录", "Codex is here now")
    } else {
        t!("Claude 当前目录", "Claude is here now")
    };
    if let Some(log) = log {
        let (cwd, edited) = read_log(&log, w.agent);
        let cwd = cwd.filter(|d| Path::new(d).is_dir());
        if let Some(d) = cwd.as_ref().filter(|d| in_repo(Path::new(d))) {
            return Found { dir: d.clone(), how: here };
        }
        // Stayed somewhere that isn't a repository (e.g. a level up): see which repository its most recently changed file is in
        if let Some(root) = edited.as_deref().map(Path::new).and_then(Path::parent).filter(|d| d.is_dir()).and_then(repo_root) {
            return Found {
                dir: root.to_string_lossy().into_owned(), how: t!("最近修改的文件所在仓库", "its recent edits are here")
            };
        }
        if let Some(d) = cwd {
            return Found { dir: d, how: here };
        }
    }
    // No record to read (a Codex started by hand and the like): if the registered one isn't a repository but the process is in
    // one, use the process's
    let lost = !w.dir.is_empty() && !in_repo(Path::new(&w.dir));
    if w.pid > 0
        && lost
        && let Some(d) = proc_cwd(w.pid).filter(|d| *d != w.dir && in_repo(Path::new(d)))
    {
        return Found { dir: d, how: t!("进程所在目录", "the process is here") };
    }
    registered()
}

/// Same as `find_in`, with the config directory read from the agent process's environment
pub fn find(w: &Where) -> Found {
    let conf = (w.pid > 0).then(|| crate::data::config_home(w.pid, w.agent)).flatten();
    find_in(w, conf.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-wd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn log_of_finds_the_conversation_file_by_the_registered_id() {
        let root = scratch("logof");
        let conf = root.join("conf");
        let proj = conf.join("projects").join("-p-app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("s-1.jsonl"), "{}\n").unwrap();
        let w = |agent, id: &str| Where { dir: "/p/app".into(), pid: 0, agent, id: id.into() };
        let c = conf.to_str().unwrap();
        assert_eq!(log_of(&w(Agent::Claude, "s-1"), c), Some(proj.join("s-1.jsonl")));
        assert_eq!(log_of(&w(Agent::Claude, "s-2"), c), None);
        assert_eq!(log_of(&w(Agent::Claude, "../x"), c), None);
        assert_eq!(log_of(&w(Agent::None, "s-1"), c), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_follows_its_own_cwd_then_the_files_it_edits() {
        let root = scratch("claude");
        let (conf, top, repo) = (root.join("conf"), root.join("ws"), root.join("ws/app"));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let top_s = top.to_str().unwrap();
        // sessions/<pid>.json: the current session id and start directory
        std::fs::create_dir_all(conf.join("sessions")).unwrap();
        std::fs::write(conf.join("sessions/42.json"), format!(r#"{{"pid":42,"sessionId":"s-new","cwd":"{top_s}"}}"#)).unwrap();
        let proj = conf.join("projects").join(project_key(top_s));
        std::fs::create_dir_all(&proj).unwrap();
        let line = |cwd: &Path| format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", cwd.display());
        let w = Where { dir: top_s.into(), pid: 42, agent: Agent::Claude, id: "s-old".into() };
        // cd'd into the repository: follow it
        std::fs::write(proj.join("s-new.jsonl"), line(&top) + &line(&repo)).unwrap();
        assert_eq!(find_in(&w, conf.to_str()), Found { dir: repo.to_str().unwrap().into(), how: "Claude 当前目录" });
        // Stayed a level up and changed a file in the repository by absolute path: go by the file
        let edit = format!(
            "{{\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{{\"replace_all\":false,\"file_path\":\"{}\"}}}}]}},\"cwd\":\"{top_s}\"}}\n",
            repo.join("src/main.rs").display()
        );
        std::fs::write(proj.join("s-new.jsonl"), line(&repo) + &edit).unwrap();
        assert_eq!(find_in(&w, conf.to_str()), Found { dir: repo.to_str().unwrap().into(), how: "最近修改的文件所在仓库" });
        // Record not found: the registered directory
        let w = Where { pid: 0, id: String::new(), ..w };
        assert_eq!(find_in(&w, conf.to_str()), Found { dir: top_s.into(), how: "" });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_reads_the_rollout_for_its_session_id() {
        let root = scratch("codex");
        let (conf, repo) = (root.join("conf"), root.join("repo"));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let day = conf.join("sessions/2026/09/28");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::create_dir_all(conf.join("sessions/2026/09/29")).unwrap();
        let body = format!("{{\"type\":\"turn_context\",\"payload\":{{\"cwd\":\"{}\"}}}}\n", repo.display());
        std::fs::write(day.join("rollout-2026-09-28T10-00-00-abc-123.jsonl"), body).unwrap();
        let w = Where { dir: "/elsewhere".into(), pid: 0, agent: Agent::Codex, id: "abc-123".into() };
        assert_eq!(find_in(&w, conf.to_str()), Found { dir: repo.to_str().unwrap().into(), how: "Codex 当前目录" });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_patch_paths_are_relative_to_its_cwd() {
        let root = scratch("codex-patch");
        let log = root.join("r.jsonl");
        let text = r#"{"payload":{"cwd":"/ws"}}
{"payload":{"arguments":"*** Begin Patch\n*** Update File: app/src/a.rs\n@@\n-x\n+y\n*** End Patch"}}
"#;
        std::fs::write(&log, text).unwrap();
        assert_eq!(read_log(&log, Agent::Codex), (Some("/ws".into()), Some("/ws/app/src/a.rs".into())));
        let _ = std::fs::remove_dir_all(&root);
    }
}
