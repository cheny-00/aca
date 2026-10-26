//! 会话真正在哪个目录干活(看改动、预览顶上的统计用)。
//!
//! 登记的路径是建会话时填的,Claude 的进程 cwd 也一直停在启动时那个;可 Claude 在里面 cd 过、
//! 进了 worktree,或者登记的就是上一级(底下好几个仓库)时,真正在改的是别处。所以以 agent 自己的记录为准:
//! - Claude:`$CLAUDE_CONFIG_DIR/sessions/<pid>.json` 给出它现在的会话 id → `projects/*/<id>.jsonl` 对话记录,
//!   最后一条带 `cwd` 的就是它现在的目录(Bash 里 cd 了、进了 worktree 都会跟着变)
//! - Codex:登记的对话 id(hook 报的)→ `$CODEX_HOME/sessions/**/rollout-*-<id>.jsonl`,最后一个 `cwd`
//! - 那个目录不在 git 仓库里(比如停在上一级):用它最近改的文件(Edit / Write、apply_patch)所在的仓库
//! - 都没有:agent 进程的 cwd,再没有就是登记的目录
//!
//! 只读文件末尾一截,不解析整份记录;记录里的内容不留存,只取路径。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use aca_core::t;

use crate::data::Agent;

/// 去哪儿找:会话的这几样(`Session` 里的)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Where {
    /// 登记的目录(没登记的是窗格当前目录)
    pub dir: String,
    /// agent 进程,0 = 没在跑
    pub pid: u32,
    pub agent: Agent,
    /// 登记的 Claude / Codex 对话 id(`B` 复制的那个)
    pub id: String,
}

impl Where {
    pub fn of(s: &crate::data::Session) -> Where {
        let id = s.info.lines().find_map(|l| l.strip_prefix("Session: ")).unwrap_or("").to_string();
        Where { dir: s.dir.clone(), pid: s.pid, agent: s.agent, id }
    }
}

/// 找到的目录,和是怎么找到的(和登记的不一样时写出来)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub dir: String,
    pub how: &'static str,
}

/// 对话记录最多读末尾这么多
const TAIL: u64 = 2 << 20;

static CWD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""cwd":"((?:[^"\\]|\\.)*)""#).unwrap());
/// Claude 改文件:Edit / Write 的输入,和改完回来的 filePath
static CLAUDE_EDIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""name":"(?:Edit|Write|MultiEdit|NotebookEdit)","input":\{[^{}]*?"(?:file_path|notebook_path)":"((?:[^"\\]|\\.)*)"|"filePath":"((?:[^"\\]|\\.)*)""#)
        .unwrap()
});
/// Codex 改文件:apply_patch 里的 `*** Update File: 路径`(在 JSON 字符串里,换行是 \n)
static CODEX_EDIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\*\*\* (?:Update|Add) File: ((?:[^"\\]|\\[^n])+)"#).unwrap());

fn unescape(raw: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string())
}

/// 文件末尾一截(从一行开头起)
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

/// Claude 把项目目录变成 projects 底下的文件夹名:不是字母数字的都换成 -
fn project_key(dir: &str) -> String {
    dir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

static SID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""sessionId":"([^"]+)""#).unwrap());

/// Claude 的对话记录:先按 sessions/<pid>.json 找现在的会话 id(/clear、resume 之后会变),没有就用登记的
fn claude_log(conf: &Path, pid: u32, id: &str) -> Option<PathBuf> {
    let reg = std::fs::read_to_string(conf.join("sessions").join(format!("{pid}.json"))).unwrap_or_default();
    let sid = SID.captures(&reg).map(|c| c[1].to_string()).filter(|_| pid > 0).unwrap_or_else(|| id.to_string());
    if sid.is_empty() || sid.contains('/') {
        return None;
    }
    let file = format!("{sid}.jsonl");
    let projects = conf.join("projects");
    // 通常就在启动目录对应的文件夹里;名字太长时 Claude 会截断加 hash,那就挨个找
    let start = CWD.captures(&reg).map(|c| unescape(&c[1]));
    if let Some(p) = start.map(|d| projects.join(project_key(&d)).join(&file)).filter(|p| p.is_file()) {
        return Some(p);
    }
    std::fs::read_dir(&projects).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file())
}

/// Codex 的 rollout:按会话 id 找,从最近的日期往前翻
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

/// 它现在这段对话的记录文件。conf = 配置目录(CLAUDE_CONFIG_DIR / CODEX_HOME),空 = 默认的 ~/.claude / ~/.codex
pub fn log_of(w: &Where, conf: &str) -> Option<PathBuf> {
    let dir = |d: &str| if conf.is_empty() { home().join(d) } else { PathBuf::from(conf) };
    match w.agent {
        Agent::Claude => claude_log(&dir(".claude"), w.pid, &w.id),
        Agent::Codex => codex_log(&dir(".codex"), &w.id),
        _ => None,
    }
}

/// 所在 git 仓库的根(往上找 .git,不跑 git)
fn repo_root(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|d| d.join(".git").exists())
}

fn in_repo(dir: &Path) -> bool {
    repo_root(dir).is_some()
}

/// 从记录末尾读出:最后的 cwd、最近改的文件(绝对路径)
fn read_log(log: &Path, agent: Agent) -> (Option<String>, Option<String>) {
    let Some(text) = tail(log) else { return (None, None) };
    let cwd = CWD.captures_iter(&text).last().map(|c| unescape(&c[1]));
    let edited = match agent {
        Agent::Codex => CODEX_EDIT.captures_iter(&text).last().map(|c| unescape(c[1].trim_end())),
        _ => CLAUDE_EDIT.captures_iter(&text).last().and_then(|c| c.get(1).or(c.get(2))).map(|m| unescape(m.as_str())),
    };
    // apply_patch 里常是相对路径:接在 cwd 后面
    let edited = edited.map(|e| match &cwd {
        Some(c) if !e.starts_with('/') => format!("{}/{e}", c.trim_end_matches('/')),
        _ => e,
    });
    (cwd, edited)
}

/// agent 进程的 cwd(lsof,只读得到自己的进程)
fn proc_cwd(pid: u32) -> Option<String> {
    let out = std::process::Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix('n')).map(String::from).filter(|d| d.starts_with('/'))
}

/// 找它真正干活的目录。conf = agent 的配置目录(CLAUDE_CONFIG_DIR / CODEX_HOME,None = 默认的 ~/.claude / ~/.codex)
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
        // 停在不是仓库的地方(比如上一级):看它最近改的文件在哪个仓库
        if let Some(root) = edited.as_deref().map(Path::new).and_then(Path::parent).filter(|d| d.is_dir()).and_then(repo_root) {
            return Found {
                dir: root.to_string_lossy().into_owned(), how: t!("最近修改的文件所在仓库", "its recent edits are here")
            };
        }
        if let Some(d) = cwd {
            return Found { dir: d, how: here };
        }
    }
    // 没有记录可读(手动起的 Codex 之类):登记的不是仓库而进程在仓库里,用进程的
    let lost = !w.dir.is_empty() && !in_repo(Path::new(&w.dir));
    if w.pid > 0
        && lost
        && let Some(d) = proc_cwd(w.pid).filter(|d| *d != w.dir && in_repo(Path::new(d)))
    {
        return Found { dir: d, how: t!("进程所在目录", "the process is here") };
    }
    registered()
}

/// 同 `find_in`,配置目录从 agent 进程的环境变量里读
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
        // sessions/<pid>.json:现在的会话 id 和启动目录
        std::fs::create_dir_all(conf.join("sessions")).unwrap();
        std::fs::write(conf.join("sessions/42.json"), format!(r#"{{"pid":42,"sessionId":"s-new","cwd":"{top_s}"}}"#)).unwrap();
        let proj = conf.join("projects").join(project_key(top_s));
        std::fs::create_dir_all(&proj).unwrap();
        let line = |cwd: &Path| format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", cwd.display());
        let w = Where { dir: top_s.into(), pid: 42, agent: Agent::Claude, id: "s-old".into() };
        // cd 进了仓库:跟着它
        std::fs::write(proj.join("s-new.jsonl"), line(&top) + &line(&repo)).unwrap();
        assert_eq!(find_in(&w, conf.to_str()), Found { dir: repo.to_str().unwrap().into(), how: "Claude 当前目录" });
        // 停在上一级、用绝对路径改了仓库里的文件:看文件
        let edit = format!(
            "{{\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{{\"replace_all\":false,\"file_path\":\"{}\"}}}}]}},\"cwd\":\"{top_s}\"}}\n",
            repo.join("src/main.rs").display()
        );
        std::fs::write(proj.join("s-new.jsonl"), line(&repo) + &edit).unwrap();
        assert_eq!(find_in(&w, conf.to_str()), Found { dir: repo.to_str().unwrap().into(), how: "最近修改的文件所在仓库" });
        // 记录找不到:登记的目录
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
