//! `H` hand off: this session has run a while (context nearly full, or you want to carry on under another account), so open
//! another session to carry on its work. First pick the new session's tool / account from `n`'s tool menu (no shell), create
//! and start it in the original session's directory and group, then send a handoff note into its input box: where the
//! original session is, the conversation id, where the conversation record is, so it reads it all itself and carries on.
//!
//! A conversation record easily runs to tens of MB (mostly tool output), too much for the new session to read directly, so a
//! text version is extracted first (`aca_records::dialog`: what both sides said, reports handed back by subagents, and at the
//! end the files it changed). The handoff note gives the paths of both.
//!
//! Sending is `session::send_fresh`: wait until the new session has drawn its screen and gone idle, at most a minute. A new
//! account entering this directory for the first time first asks "trust this folder?" (Codex also "Hooks need review"); that
//! counts as needs you, and the note is sent only if you go in and answer it within the minute.
//!
//! If it can't be sent, the handoff note is left in the new session's note; `s` sends it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aca_core::{t, tr};
use aca_records::agent::Agent;
use aca_records::dialog::{dialog, render, save, unix_now};
use aca_records::words::tilde;
use aca_records::workdir::{Found, Where};

use crate::create::NewReq;
use crate::data::Session;
use crate::ops::Outcome;

/// The session being handed off
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub title: String,
    /// "Claude·work"
    pub label: String,
    pub agent: Agent,
    /// The registered directory
    pub dir: String,
    /// The lines `B` copies: Path / Repo / Branch / Session
    pub info: String,
    /// Conversation id, conversation record
    pub sid: String,
    pub log: PathBuf,
    /// Running: its pane (for the new session to look at its screen); not running = empty
    pub pane: String,
    /// Its note: extra instructions, carried over too
    pub note: Option<String>,
    /// For finding which directory it last worked in (see `workdir`)
    pub w: Where,
    pub conf: String,
}

/// Identify the highlighted session's current conversation: for a running Claude, the conversation id its process records
/// itself (it changes after /clear, resume); otherwise the registered one
pub fn source(s: &Session) -> Result<Source, String> {
    let w = crate::data::where_of(s);
    let conf = crate::mcp::home_of(s);
    let log = aca_records::workdir::log_of(&w, &conf).ok_or(t!("找不到对话记录", "can't find its conversation record"))?;
    let sid = match s.agent {
        Agent::Claude => log.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default(),
        _ => w.id.clone(),
    };
    let pane = if s.pid > 0 { s.pane.clone() } else { String::new() };
    Ok(Source {
        title: s.title.clone(),
        label: s.agent_label(),
        agent: s.agent,
        dir: s.dir.clone(),
        info: s.info.clone(),
        sid,
        log,
        pane,
        note: None,
        w,
        conf,
    })
}

/// Runs in the background: first extract the text version and write the note (nothing is created if the record can't be
/// read), then create the new session, finally send it over
pub fn run(from: &Source, req: &NewReq, _to: Agent) -> Outcome {
    let d = match dialog(&from.log, from.agent) {
        Ok(d) => d,
        Err(e) => return Outcome::Failed(tr!("交接失败:{e}", "not handed off: {e}")),
    };
    let text = render(&from.title, &from.log, from.agent, &d, unix_now(), aca_records::usage::local_offset());
    let digest = match save(&from.sid, &text) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(tr!("交接失败:{e}", "not handed off: {e}")),
    };
    let found = aca_records::workdir::find_in(&from.w, (!from.conf.is_empty()).then_some(from.conf.as_str()));
    let msg = prompt(from, &digest, text.lines().count(), &found);
    let id = match crate::create::create(req) {
        Ok(id) => id,
        Err(e) => return Outcome::Failed(tr!("新建失败:{e}", "creating it failed: {e}")),
    };
    match send(&id, &msg) {
        Ok(()) => Outcome::Made { msg: t!("已交接", "handed off").into(), id },
        Err(e) => Outcome::Handed {
            msg: tr!("发送失败:{e} · 交接说明已存入便签,s 发送", "not sent: {e} · the note has it, s sends it"),
            id,
            note: msg,
        },
    }
}

/// A new session takes a while to start: wait until it has drawn its screen and gone idle before sending
const SEND_WAIT: Duration = Duration::from_secs(60);

fn send(id: &str, text: &str) -> Result<(), String> {
    let store = aca_core::store::Store::open()?;
    aca_core::session::send_fresh(&store, &aca_core::config::load(), id, text, SEND_WAIT)
}

/// The handoff note written for the new session
pub fn prompt(from: &Source, digest: &Path, lines: usize, now: &Found) -> String {
    let mut p = String::new();
    let _ = writeln!(
        p,
        "{}",
        tr!(
            "接手「{}」:它在 {} 跑了一阵,现在换你接着做。\n",
            "Taking over “{}”: it has been working in {} for a while; now you carry on.\n",
            from.title,
            tilde(&from.dir)
        )
    );
    let _ = writeln!(
        p,
        "{}",
        tr!(
            "1. 先读它的对话:{}(共 {lines} 行:双方说的话、子代理交回来的报告,最后附它改过的文件)。越往后越新,长的话先从后往前读最后一两千行,再看开头几条弄清最初要什么。",
            "1. First read its conversation: {} ({lines} lines: what both sides said, reports handed back by subagents, and at the end the files it changed). Later is newer; if it's long, read the last one or two thousand lines backwards first, then the first few entries to see what was asked for originally.",
            digest.display()
        )
    );
    let _ = writeln!(
        p,
        "{}",
        tr!(
            "   要看某一步具体干了什么(跑了什么命令、改了哪些文件、输出是什么),去原始记录 {} 里按时间或关键词搜;它是 JSONL、很大,别整份读。",
            "   To see exactly what a step did (which commands ran, which files changed, what the output was), search the raw record {} by time or keyword; it's JSONL and large, don't read all of it.",
            from.log.display()
        )
    );
    p.push_str(t!("2. 以现状为准:记录里说做完了的不一定真落了地。先看看工作区现在的样子(git status / git diff、文字版最后列的那些文件)。\n", "2. Trust the current state: what the record says is done may not have landed. First look at the workspace as it is now (git status / git diff, the files listed at the end of the text version).\n"));
    p.push_str(
        t!("3. 读完用几句话写下你的理解:目标、做到哪了、下一步。然后直接接着做,不用等我确认。只有一种情况先停:它最后是在问我、或者在等我确认,", "3. When done reading, write down your understanding in a few sentences: the goal, how far it got, the next step. Then carry on directly, no need to wait for my confirmation. Stop first in only one case: it ended by asking me something or waiting for my confirmation, "),
    );
    p.push_str(t!("而最后的「另外交代」里没有回答它(没有这一节就是没回答)—— 这时把它问的转述给我,等我答。「另外交代」就是我提前给的答复。\n", "and the “Also” section at the end doesn't answer it (no such section means no answer) — then relay the question to me and wait for my answer. “Also” is my answer given in advance.\n"));
    if from.pane.is_empty() {
        p.push_str(t!("4. 原会话已经没在跑了。\n", "4. The original session isn't running any more.\n"));
    } else {
        let sock = std::env::var("ACA_TMUX_SOCKET").ok().filter(|s| !s.is_empty()).map(|s| format!("-L {s} ")).unwrap_or_default();
        let _ = writeln!(
            p,
            "{}",
            tr!(
                "4. 原会话还开着:别往它的终端里发东西、别动它的进程。想看它现在的屏幕:tmux {sock}capture-pane -p -t {}",
                "4. The original session is still open: don't send anything to its terminal and don't touch its processes. To see its screen now: tmux {sock}capture-pane -p -t {}",
                from.pane
            )
        );
    }
    p.push_str(t!("5. 它的对话开头要是也有这样一段交接说明,说明它也是接手的;需要更早的来龙去脉,顺着那里给的路径往前读。\n", "5. If its conversation also starts with a handoff note like this one, it took over too; for the earlier history, follow the paths given there backwards.\n"));
    p.push_str(t!("\n原会话\n", "\nOriginal session\n"));
    let _ = writeln!(p, "{}", tr!("- 标题:{}({})", "- Title: {} ({})", from.title, from.label));
    let mut has_dir = false;
    for l in from.info.lines() {
        let (k, v) = l.split_once(": ").unwrap_or(("", l));
        let k = match k {
            "Path" => {
                has_dir = true;
                t!("目录", "Directory")
            }
            "Repo" => t!("仓库", "Repository"),
            "Branch" => t!("分支", "Branch"),
            _ => continue,
        };
        let _ = writeln!(p, "- {k}:{v}");
    }
    if !has_dir {
        let _ = writeln!(p, "{}", tr!("- 目录:{}", "- Directory: {}", from.dir));
    }
    if !now.how.is_empty() && now.dir != from.dir {
        let _ = writeln!(p, "{}", tr!("- 最后在:{}({})", "- Last in: {} ({})", now.dir, now.how));
    }
    let _ = writeln!(p, "{}", tr!("- 对话 id:{}", "- Conversation id: {}", from.sid));
    let _ = writeln!(p, "{}", tr!("- 对话记录:{}", "- Conversation record: {}", from.log.display()));
    let _ = writeln!(p, "{}", tr!("- 文字版:{}", "- Text version: {}", digest.display()));
    if let Some(n) = from.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        let _ = write!(p, "{}", tr!("\n另外交代:\n{n}\n", "\nAlso:\n{n}\n"));
    }
    p.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(agent: Agent, log: PathBuf) -> Source {
        Source {
            title: "修登录".into(),
            label: "Claude·work".into(),
            agent,
            dir: "/Users/x/code/web".into(),
            info: "Repo: /Users/x/code/web\nPath: /Users/x/code/web-wt\nBranch: fix\nSession: s-1".into(),
            sid: "s-1".into(),
            log,
            pane: String::new(),
            note: None,
            w: Where::default(),
            conf: String::new(),
        }
    }

    #[test]
    fn prompt_points_at_both_files_and_carries_the_note() {
        let mut s = src(Agent::Claude, PathBuf::from("/c/p/s-1.jsonl"));
        let here = Found { dir: "/Users/x/code/web-wt/api".into(), how: "Claude 现在在这" };
        let p = prompt(&s, Path::new("/h/s-1.md"), 420, &here);
        assert!(p.starts_with("接手「修登录」:"), "{p}");
        assert!(p.contains("先读它的对话:/h/s-1.md(共 420 行:"), "{p}");
        assert!(p.contains("去原始记录 /c/p/s-1.jsonl 里"), "{p}");
        assert!(p.contains("只有一种情况先停:它最后是在问我、或者在等我确认,而最后的「另外交代」里没有回答它"), "{p}");
        assert!(p.contains("原会话已经没在跑了"), "{p}");
        assert!(p.contains("- 标题:修登录(Claude·work)\n- 仓库:/Users/x/code/web\n- 目录:/Users/x/code/web-wt\n- 分支:fix\n"), "{p}");
        assert!(p.contains("- 最后在:/Users/x/code/web-wt/api(Claude 现在在这)\n- 对话 id:s-1\n"), "{p}");
        assert!(p.ends_with("- 文字版:/h/s-1.md"), "{p}");
        assert!(!p.contains("Session:"), "{p}");

        s.pane = "%7".into();
        s.note = Some("  顺便把测试补上\n".into());
        s.info = String::new();
        let p = prompt(&s, Path::new("/h/s-1.md"), 3, &Found::default());
        assert!(p.contains("原会话还开着:别往它的终端里发东西、别动它的进程。想看它现在的屏幕:tmux capture-pane -p -t %7"), "{p}");
        assert!(p.contains("- 目录:/Users/x/code/web\n"), "{p}");
        assert!(!p.contains("最后在"), "{p}");
        assert!(p.ends_with("\n\n另外交代:\n顺便把测试补上"), "{p}");
    }
}
