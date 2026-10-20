//! `H` 交接:这个会话跑了一阵(上下文快满了、想换个账号接着跑),另开一个会话接着干。
//! 先从 `n` 的工具菜单里挑新会话用哪个工具 / 账号(没有 shell),在原会话的目录、分组里建好启动,
//! 再把一段交接说明发进它的输入框:原会话在哪、对话 id、对话记录在哪,让它自己读完接着做。
//!
//! 对话记录动辄几十 MB(大半是工具输出),新会话直接读不动,所以先抽一份文字版:双方说的话、子代理交回来的报告,
//! 最后附它改过的文件(`~/.local/state/aca/ui/handoff/<对话 id>.md`,30 天前的顺手删),说明里两份的路径都给。
//!
//! 发送是 `session::send_fresh`:等新会话把界面画出来、空下来再发,最多等一分钟。新账号头一回进这个目录
//! 会先问「信任这个文件夹吗」(Codex 还有「Hooks need review」),那算「在等你」,一分钟里进去选了它才发。
//!
//! 发不出去就把说明留在新会话的便签里,`s` 再发。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use aca_core::{t, tr};

use crate::create::{NewReq, tilde};
use crate::data::{Agent, Session};
use crate::ops::Outcome;
use crate::workdir::{Found, Where};

/// 文字版留多久
const KEEP: Duration = Duration::from_secs(30 * 86400);

/// 要交接出去的那个会话
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub title: String,
    /// 「Claude·work」
    pub label: String,
    pub agent: Agent,
    /// 登记的目录
    pub dir: String,
    /// `B` 复制的那几行:Path / Repo / Branch / Session
    pub info: String,
    /// 对话 id、对话记录
    pub sid: String,
    pub log: PathBuf,
    /// 在跑:它的窗格(新会话想看它的屏幕用);没在跑 = 空
    pub pane: String,
    /// 它的便签:另外交代的话,一起带过去
    pub note: Option<String>,
    /// 找它最后在哪个目录干活用(见 `workdir`)
    pub w: Where,
    pub conf: String,
}

/// 认出高亮会话现在这段对话:在跑的 Claude 看它进程自己记的对话 id(/clear、resume 之后会变),其余用登记的
pub fn source(s: &Session) -> Result<Source, String> {
    let w = Where::of(s);
    let conf = crate::mcp::home_of(s);
    let log = crate::workdir::log_of(&w, &conf).ok_or(t!("找不到它的对话记录", "can't find its conversation record"))?;
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

/// 后台跑:先抽文字版、写好说明(读不了就不建),再建新会话,最后发过去
pub fn run(from: &Source, req: &NewReq, _to: Agent) -> Outcome {
    let d = match dialog(&from.log, from.agent) {
        Ok(d) => d,
        Err(e) => return Outcome::Failed(tr!("没交接:{e}", "not handed off: {e}")),
    };
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let text = render(from, &d, now, crate::usage::local_offset());
    let digest = match save(&from.sid, &text) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(tr!("没交接:{e}", "not handed off: {e}")),
    };
    let found = crate::workdir::find_in(&from.w, (!from.conf.is_empty()).then_some(from.conf.as_str()));
    let msg = prompt(from, &digest, text.lines().count(), &found);
    let id = match crate::create::create(req) {
        Ok(id) => id,
        Err(e) => return Outcome::Failed(tr!("新建失败:{e}", "creating it failed: {e}")),
    };
    match send(&id, &msg) {
        Ok(()) => Outcome::Made { msg: t!("已交接", "handed off").into(), id },
        Err(e) => {
            Outcome::Handed {
                msg: tr!("没发出去:{e} · 说明在便签里,s 发送", "not sent: {e} · the note has it, s sends it"), id, note: msg
            }
        }
    }
}

/// 新会话起来要一会儿:等它画出界面、空下来再发
const SEND_WAIT: Duration = Duration::from_secs(60);

fn send(id: &str, text: &str) -> Result<(), String> {
    let store = aca_core::store::Store::open()?;
    aca_core::session::send_fresh(&store, &aca_core::config::load(), id, text, SEND_WAIT)
}

/// 写给新会话的交接说明
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

#[derive(Clone, Debug, PartialEq)]
enum Who {
    User,
    Agent,
    /// 压缩上下文时写的摘要(之前的对话被压成了这一段)
    Summary,
    /// 子代理交回来的报告,后面是派它时写的说明
    Sub(String),
}

/// 一段话:谁说的、什么时候(秒,0 = 不知道)、说了什么
#[derive(Debug, PartialEq)]
struct Said {
    who: Who,
    at: i64,
    text: String,
}

/// 它改过的文件:路径、最后一次改的时刻、是不是删掉了(挪走也算删)
#[derive(Debug, PartialEq)]
struct Touched {
    path: String,
    at: i64,
    gone: bool,
}

/// 从对话记录里抽出来的
#[derive(Debug, Default, PartialEq)]
struct Dialog {
    said: Vec<Said>,
    files: Vec<Touched>,
}

impl Dialog {
    /// 同一方连着说的并成一段(Claude 一条回复按内容块拆成好几行记);摘要、子代理报告各是各的
    fn say(&mut self, s: Option<Said>) {
        let Some(s) = s else { return };
        match self.said.last_mut() {
            Some(last) if last.who == s.who && matches!(s.who, Who::User | Who::Agent) => {
                last.text.push_str("\n\n");
                last.text.push_str(&s.text);
            }
            _ => self.said.push(s),
        }
    }

    /// 同一个文件只留最后一次
    fn touch(&mut self, path: &str, at: i64, gone: bool) {
        if path.is_empty() {
            return;
        }
        self.files.retain(|f| f.path != path);
        self.files.push(Touched { path: path.to_string(), at, gone });
    }
}

/// 对话记录 → 说的话(按顺序)和改过的文件(按最后一次改的时刻)
fn dialog(log: &Path, agent: Agent) -> Result<Dialog, String> {
    let mut d = Dialog::default();
    if agent == Agent::Codex {
        each_line(log, |v| codex_line(v, &mut d))?;
    } else {
        let mut agents = HashMap::new();
        each_line(log, |v| claude_line(v, &mut d, &mut agents))?;
        // 子代理自己的记录在 <对话 id>/subagents/ 下,只看它们改了哪些文件
        let subs = log.with_extension("").join("subagents");
        for f in std::fs::read_dir(subs)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        {
            let _ = each_line(&f, |v| claude_edits(v, &mut d));
        }
    }
    if d.said.is_empty() {
        return Err(t!("对话记录里还没有说过话", "nothing has been said in the conversation yet").into());
    }
    d.files.sort_by_key(|f| f.at);
    Ok(d)
}

fn each_line(p: &Path, mut f: impl FnMut(&Value)) -> Result<(), String> {
    let file = std::fs::File::open(p).map_err(|e| tr!("读不了对话记录({e})", "can't read the conversation record ({e})"))?;
    for line in BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { break };
        if let Ok(v) = serde_json::from_slice::<Value>(&line) {
            f(&v);
        }
    }
    Ok(())
}

fn at(v: &Value) -> i64 {
    crate::usage::parse_ts(v["timestamp"].as_str().unwrap_or(""))
}

fn said(who: Who, v: &Value, text: &str) -> Option<Said> {
    let text = text.trim();
    (!text.is_empty()).then(|| Said { who, at: at(v), text: text.to_string() })
}

/// 内容里的文字:字符串原样,数组只取 `text` 块(思考、工具调用、工具结果、图片都不要)
fn texts(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Claude 改过的文件:编辑工具(Edit / Write)做成了,结果里有 `filePath`(没改成的没有);
/// 新版 Claude Code 的 Bash 结果里还有 `bashEditDiff`,命令行改了哪些文件
fn claude_edits(v: &Value, d: &mut Dialog) {
    let r = &v["toolUseResult"];
    if let Some(p) = r["filePath"].as_str() {
        d.touch(p, at(v), false);
    }
    for p in r["bashEditDiff"]["changedFiles"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        d.touch(p, at(v), false);
    }
}

/// Claude:`user` / `assistant` 的文字块,加上子代理的报告;子代理自己的(isSidechain)、Claude Code 塞的(isMeta)不要。
/// agents:派子代理的调用 id → 说明,等它的报告回来时用
fn claude_line(v: &Value, d: &mut Dialog, agents: &mut HashMap<String, String>) {
    claude_edits(v, d);
    if v["isSidechain"] == true || v["isMeta"] == true {
        return;
    }
    let content = &v["message"]["content"];
    match v["type"].as_str() {
        Some("assistant") => {
            for b in content.as_array().into_iter().flatten() {
                if b["type"] == "tool_use"
                    && matches!(b["name"].as_str(), Some("Agent" | "Task"))
                    && let Some(id) = b["id"].as_str()
                {
                    agents.insert(id.to_string(), b["input"]["description"].as_str().unwrap_or("").to_string());
                }
            }
            d.say(said(Who::Agent, v, &texts(content)));
        }
        Some("user") => {
            // 前台跑的子代理,报告就是工具结果;后台跑的(新版默认)这里只回一句「已在后台启动」,报告在之后的通知里
            for b in content.as_array().into_iter().flatten().filter(|b| b["type"] == "tool_result") {
                if let Some(desc) = b["tool_use_id"].as_str().and_then(|id| agents.get(id))
                    && v["toolUseResult"]["status"] != "async_launched"
                {
                    d.say(said(Who::Sub(desc.clone()), v, &texts(&b["content"])));
                }
            }
            let text = texts(content);
            if v["isCompactSummary"] == true {
                return d.say(said(Who::Summary, v, &text));
            }
            if let Some((desc, report)) = sub_report(&text) {
                return d.say(said(Who::Sub(desc), v, &report));
            }
            if let Some(t) = claude_user_text(&text) {
                d.say(said(Who::User, v, &t));
            }
        }
        _ => {}
    }
}

/// 后台子代理跑完的通知:`<summary>Agent "说明" completed</summary>` 和 `<result>报告</result>`。
/// 别的后台任务(命令、MCP)的通知不算
fn sub_report(t: &str) -> Option<(String, String)> {
    let s = t.trim_start();
    if !s.starts_with("<task-notification>") {
        return None;
    }
    let desc = tag(s, "summary")?.strip_prefix("Agent \"")?.split('"').next()?.to_string();
    Some((desc, tag(s, "result")?.to_string()))
}

/// 用户那边混着 Claude Code 自己塞的:斜杠命令写成一行「/命令 参数」,命令输出、后台任务的通知不要
fn claude_user_text(t: &str) -> Option<String> {
    let s = t.trim_start();
    if ["<local-command-stdout>", "<local-command-stderr>", "<local-command-caveat>", "<task-notification>"]
        .iter()
        .any(|x| s.starts_with(x))
    {
        return None;
    }
    if let Some(name) = tag(s, "command-name") {
        return Some(format!("{name} {}", tag(s, "command-args").unwrap_or("")).trim_end().to_string());
    }
    Some(t.to_string())
}

/// `<name>里面</name>` 的里面
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let i = s.find(&open)? + open.len();
    let j = s[i..].find(&format!("</{name}>"))? + i;
    Some(s[i..j].trim())
}

/// Codex 每轮开头替你塞进去的(AGENTS.md、环境、技能说明…),不是人说的
const CODEX_INJECTED: [&str; 7] = [
    "<environment_context>",
    "# AGENTS.md",
    "<user_instructions>",
    "<skill>",
    "<recommended_plugins>",
    "<codex_internal_context",
    "<turn_aborted>",
];

/// Codex:`response_item` 里 user / assistant 的 message(developer 的是系统说明,不要),压缩时的 `compacted`;
/// 改过的文件看 apply_patch 做成了的 `patch_apply_end`,新版另记一份 `FileChange`(同样的路径,并掉)
fn codex_line(v: &Value, d: &mut Dialog) {
    let p = &v["payload"];
    match (v["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or("")) {
        ("response_item", "message") => {
            let who = match p["role"].as_str() {
                Some("user") => Who::User,
                Some("assistant") => Who::Agent,
                _ => return,
            };
            let parts: Vec<&str> = p["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["text"].as_str())
                .filter(|t| !CODEX_INJECTED.iter().any(|x| t.trim_start().starts_with(x)))
                .collect();
            d.say(said(who, v, &parts.join("\n")));
        }
        ("compacted", _) => d.say(said(Who::Summary, v, p["message"].as_str().unwrap_or(""))),
        ("event_msg", "patch_apply_end") if p["success"] == true => codex_changes(&p["changes"], at(v), d),
        ("event_msg", "item_completed") if p["item"]["type"] == "FileChange" && p["item"]["status"] == "completed" => {
            codex_changes(&p["item"]["changes"], at(v), d)
        }
        _ => {}
    }
}

/// `changes`:路径 → {type: add / update / delete, move_path}
fn codex_changes(c: &Value, at: i64, d: &mut Dialog) {
    for (path, ch) in c.as_object().into_iter().flatten() {
        match ch["move_path"].as_str() {
            Some(to) => {
                d.touch(path, at, true);
                d.touch(to, at, false);
            }
            None => d.touch(path, at, ch["type"] == "delete"),
        }
    }
}

/// 文字版:开头写原始记录在哪、省了什么,之后一段一段「=== 谁 · 月-日 时:分 ===」,最后是改过的文件
fn render(from: &Source, d: &Dialog, now: i64, off: i64) -> String {
    let codex = from.agent == Agent::Codex;
    let mut o = tr!("# 「{}」的对话(文字版)\n\n", "# Conversation of “{}” (text version)\n\n", from.title);
    let _ = writeln!(o, "{}", tr!("- 原始记录:{}", "- Raw record: {}", from.log.display()));
    let _ = writeln!(o, "{}", tr!("- 抽出时间:{}", "- Extracted: {}", stamp(now, off)));
    o.push_str(t!("- 只留了双方说的话和子代理交回来的报告:工具调用、命令输出、思考过程都省了,要细节按时间去原始记录里找\n", "- Only what both sides said and the reports subagents handed back: tool calls, command output and thinking are left out; for details, look in the raw record by time\n"));
    o.push_str(t!("- 最后一节是它改过的文件\n", "- The last section lists the files it changed\n"));
    for s in &d.said {
        let who = match &s.who {
            Who::User => t!("用户", "User").to_string(),
            Who::Agent => (if codex { "Codex" } else { "Claude" }).to_string(),
            Who::Summary => t!("压缩摘要", "Compaction summary").to_string(),
            Who::Sub(desc) if desc.is_empty() => t!("子代理报告", "Subagent report").to_string(),
            Who::Sub(desc) => tr!("子代理报告 · {desc}", "Subagent report · {desc}"),
        };
        let _ = write!(o, "\n=== {who} · {} ===\n\n{}\n", stamp(s.at, off), s.text);
    }
    o.push_str(t!("\n=== 它改过的文件 ===\n\n", "\n=== Files it changed ===\n\n"));
    o.push_str(t!(
        "按最后一次改的时间排,最近的在最后。只有记录里写明了的:",
        "Ordered by the time of the last change, newest last. Only what the record states: "
    ));
    o.push_str(if codex {
        t!("apply_patch 改的都在,用命令行改的(重定向、sed -i 之类)不在这里。\n\n", "everything changed by apply_patch is here; changes made on the command line (redirects, sed -i and the like) aren't.\n\n")
    } else {
        t!("编辑工具改的都在(子代理改的也算),用命令行改的(重定向、sed -i 之类)只有新版 Claude Code 才记。\n\n", "everything changed by the edit tools is here (subagents' too); changes made on the command line (redirects, sed -i and the like) are only recorded by newer Claude Code.\n\n")
    });
    if d.files.is_empty() {
        o.push_str(t!("- 没有\n", "- none\n"));
    }
    for f in &d.files {
        let _ = writeln!(o, "- {} {}{}", stamp(f.at, off), f.path, if f.gone { t!("(删了)", " (deleted)") } else { "" });
    }
    o
}

/// 本地时间「09-30 14:05」
fn stamp(ts: i64, off: i64) -> String {
    if ts == 0 {
        return "?".into();
    }
    let t = ts + off;
    let (_, m, d) = crate::usage::civil_from_days(t.div_euclid(86400));
    format!("{m:02}-{d:02} {:02}:{:02}", t.rem_euclid(86400) / 3600, t.rem_euclid(3600) / 60)
}

/// 写进 ~/.local/state/aca/ui/handoff/<对话 id>.md(同一段对话再交接一次就覆盖成新的),顺手删 30 天前的
fn save(sid: &str, text: &str) -> std::io::Result<PathBuf> {
    let dir = crate::state::state_file("handoff").ok_or_else(|| std::io::Error::other(t!("没有 HOME", "no HOME")))?;
    let name = if sid.is_empty() || sid.contains(['/', '\\']) || sid.starts_with('.') { "dialog".to_string() } else { sid.to_string() };
    let path = dir.join(format!("{name}.md"));
    crate::state::write_atomic(&path, text)?;
    prune(&dir, KEEP);
    Ok(path)
}

fn prune(dir: &Path, keep: Duration) {
    let now = SystemTime::now();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > keep);
        if old && e.path().extension().is_some_and(|x| x == "md") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-ho-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

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

    fn said_of(d: &Dialog) -> Vec<(Who, String)> {
        d.said.iter().map(|s| (s.who.clone(), s.text.clone())).collect()
    }

    fn files_of(d: &Dialog) -> Vec<(&str, bool)> {
        d.files.iter().map(|f| (f.path.as_str(), f.gone)).collect()
    }

    #[test]
    fn claude_keeps_what_was_said_and_what_subagents_reported() {
        let root = scratch("claude");
        let log = root.join("s-1.jsonl");
        let lines = [
            r#"{"type":"permission-mode","mode":"default"}"#,
            r#"{"type":"user","message":{"role":"user","content":"帮我修登录"},"timestamp":"2026-09-30T01:00:00Z"}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>x</local-command-caveat>"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"想"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"先看看"}]},"timestamp":"2026-09-30T01:00:05Z"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"一大堆输出"}]}}"#,
            // 前台子代理:报告就是工具结果
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a1","name":"Agent","input":{"description":"查登录","prompt":"…"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a1","content":[{"type":"text","text":"cookie 过期了"}]}]},"toolUseResult":{"status":"completed"}}"#,
            // 后台子代理:先回一句已启动(不要),报告在通知里
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a2","name":"Agent","input":{"description":"跑测试"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a2","content":"Async agent launched"}]},"toolUseResult":{"status":"async_launched"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"修好了"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"子代理说的"}]}}"#,
            r#"{"type":"user","message":{"content":"<task-notification>\n<task-id>b1</task-id>\n<summary>Background command \"sleep\" completed</summary>\n<result>不是子代理</result>\n</task-notification>"}}"#,
            r#"{"type":"user","message":{"content":"<task-notification>\n<task-id>a2</task-id>\n<status>completed</status>\n<summary>Agent \"跑测试\" completed</summary>\n<result>全过了</result>\n</task-notification>"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args>留下登录</command-args>"}}"#,
            r#"{"type":"user","message":{"content":"<local-command-stdout>Compacted</local-command-stdout>"}}"#,
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued…"}}"#,
            "不是 JSON",
            r#"{"type":"user","message":{"content":[{"type":"text","text":"再加个测试"}]}}"#,
        ];
        std::fs::write(&log, lines.join("\n")).unwrap();
        let d = dialog(&log, Agent::Claude).unwrap();
        assert_eq!(
            said_of(&d),
            vec![
                (Who::User, "帮我修登录".into()),
                (Who::Agent, "先看看".into()),
                (Who::Sub("查登录".into()), "cookie 过期了".into()),
                (Who::Agent, "修好了".into()),
                (Who::Sub("跑测试".into()), "全过了".into()),
                (Who::User, "/compact 留下登录".into()),
                (Who::Summary, "This session is being continued…".into()),
                (Who::User, "再加个测试".into()),
            ]
        );
        assert!(d.files.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_lists_the_files_it_and_its_subagents_changed() {
        let root = scratch("claude-files");
        let log = root.join("s-1.jsonl");
        let lines = [
            r#"{"type":"user","message":{"content":"改一下"},"timestamp":"2026-09-30T01:00:00Z"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]},"toolUseResult":{"filePath":"/r/a.rs","oldString":"x"},"timestamp":"2026-09-30T01:01:00Z"}"#,
            // 没改成的 Edit 没有 filePath;Read 的在 file 里面
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]},"toolUseResult":"Error: String to replace not found"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]},"toolUseResult":{"type":"text","file":{"filePath":"/r/read.rs"}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]},"toolUseResult":{"stdout":"","bashEditDiff":{"changedFiles":["/r/b.txt"]}},"timestamp":"2026-09-30T01:03:00Z"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]},"toolUseResult":{"type":"update","filePath":"/r/a.rs"},"timestamp":"2026-09-30T01:04:00Z"}"#,
        ];
        std::fs::write(&log, lines.join("\n")).unwrap();
        let subs = root.join("s-1/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(
            subs.join("agent-x.jsonl"),
            r#"{"type":"user","isSidechain":true,"toolUseResult":{"filePath":"/r/sub.rs"},"timestamp":"2026-09-30T01:02:00Z"}"#,
        )
        .unwrap();
        std::fs::write(subs.join("agent-x.meta.json"), r#"{"toolUseResult":{"filePath":"/r/no.rs"}}"#).unwrap();
        let d = dialog(&log, Agent::Claude).unwrap();
        // 按最后一次改的时间排,同一个文件只留一次
        assert_eq!(files_of(&d), vec![("/r/sub.rs", false), ("/r/b.txt", false), ("/r/a.rs", false)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_drops_what_it_injects_itself_and_lists_patched_files() {
        let root = scratch("codex");
        let log = root.join("rollout-x-c-1.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"c-1"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"系统说明"}]}}"#,
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions\n…"}]}}"##,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>…</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"配一下 6.1"}]},"timestamp":"2026-09-30T01:44:29.710Z"}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch"}}"#,
            r#"{"type":"event_msg","payload":{"type":"patch_apply_end","success":true,"changes":{"/r/a.toml":{"type":"update","move_path":null}}},"timestamp":"2026-09-30T01:45:00Z"}"#,
            r#"{"type":"event_msg","payload":{"type":"patch_apply_end","success":false,"changes":{"/r/failed.toml":{"type":"update"}}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","status":"completed","changes":{"/r/old.rs":{"type":"update","move_path":"/r/new.rs"},"/r/tmp.txt":{"type":"delete"}}}},"timestamp":"2026-09-30T01:46:00Z"}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"配好了"}]}}"#,
            r#"{"type":"compacted","payload":{"message":""}}"#,
            r#"{"type":"compacted","payload":{"message":"之前:配了 6.1"}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"配好了"}}"#,
        ];
        std::fs::write(&log, lines.join("\n")).unwrap();
        let d = dialog(&log, Agent::Codex).unwrap();
        assert_eq!(
            said_of(&d),
            vec![(Who::User, "配一下 6.1".into()), (Who::Agent, "配好了".into()), (Who::Summary, "之前:配了 6.1".into())]
        );
        assert_eq!(files_of(&d), vec![("/r/a.toml", false), ("/r/old.rs", true), ("/r/new.rs", false), ("/r/tmp.txt", true)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_conversation_is_not_handed_off() {
        let root = scratch("empty");
        let log = root.join("s.jsonl");
        std::fs::write(&log, r#"{"type":"permission-mode"}"#).unwrap();
        assert_eq!(dialog(&log, Agent::Claude).unwrap_err(), "对话记录里还没有说过话");
        assert!(dialog(&root.join("nope.jsonl"), Agent::Claude).unwrap_err().starts_with("读不了对话记录"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn digest_heads_each_turn_with_who_and_local_time_and_ends_with_the_files() {
        let s = src(Agent::Claude, PathBuf::from("/c/p/s-1.jsonl"));
        let mut d = Dialog::default();
        d.say(Some(Said { who: Who::User, at: 1_790_730_000, text: "修登录".into() }));
        d.say(Some(Said { who: Who::Sub("查登录".into()), at: 1_790_730_060, text: "cookie 过期了".into() }));
        d.say(Some(Said { who: Who::Agent, at: 0, text: "好".into() }));
        let t = render(&s, &d, 1_790_730_000, 8 * 3600);
        assert!(t.starts_with("# 「修登录」的对话(文字版)\n\n- 原始记录:/c/p/s-1.jsonl\n- 抽出时间:09-30 09:00\n"), "{t}");
        assert!(t.contains("\n=== 用户 · 09-30 09:00 ===\n\n修登录\n\n=== 子代理报告 · 查登录 · 09-30 09:01 ===\n\ncookie 过期了\n\n=== Claude · ? ===\n\n好\n"), "{t}");
        assert!(t.ends_with("\n=== 它改过的文件 ===\n\n按最后一次改的时间排,最近的在最后。只有记录里写明了的:编辑工具改的都在(子代理改的也算),用命令行改的(重定向、sed -i 之类)只有新版 Claude Code 才记。\n\n- 没有\n"), "{t}");
        d.touch("/r/a.rs", 1_790_730_120, false);
        d.touch("/r/b.rs", 1_790_730_180, true);
        let t = render(&src(Agent::Codex, PathBuf::from("/x.jsonl")), &d, 0, 8 * 3600);
        assert!(t.contains("=== Codex · ? ==="), "{t}");
        assert!(
            t.ends_with(
                "apply_patch 改的都在,用命令行改的(重定向、sed -i 之类)不在这里。\n\n- 09-30 09:02 /r/a.rs\n- 09-30 09:03 /r/b.rs(删了)\n"
            ),
            "{t}"
        );
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

    #[test]
    fn prune_drops_only_old_digests() {
        let root = scratch("prune");
        std::fs::write(root.join("a.md"), "x").unwrap();
        std::fs::write(root.join("keep.txt"), "x").unwrap();
        prune(&root, Duration::from_secs(3600));
        assert!(root.join("a.md").exists());
        std::thread::sleep(Duration::from_millis(20));
        prune(&root, Duration::ZERO);
        assert!(!root.join("a.md").exists() && root.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
