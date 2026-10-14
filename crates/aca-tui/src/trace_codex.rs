//! Codex 的 rollout → `trace` 的树。规则是 2026-09 在本机 2,429 份 rollout(0.107–0.159)上实测的:
//! - 一轮:`task_started` … `task_complete` / `turn_aborted`,耗时用结束事件写的 duration_ms(0.120+,没有就拿时间戳相减)。
//!   提问是 `user_message` 事件(≤ 0.147)或 `item_completed` 的 UserMessage(0.147+);跑着的时候插的话接在同一轮的提问后面
//! - 一次调用:上一次用量之后出现的 reasoning / message / 工具调用都归它。用量是一个「新的」`token_count`
//!   (累计数变了的,用它的 last_token_usage,和 `$` 的算法一样)。从前一个输入(工具输出 / 提问 / 开始)算起,
//!   到最后一个模型输出项为止(0.153+ 是 `token_usage_record`,0.130 以前是 token_count 本身),都是估的
//! - 工具:call_id 连调用和输出;命令跑了多久、退出码写在 `exec_command_end` 里,apply_patch 改了什么在 `patch_apply_end`
//! - 0.144+ 的 `exec` 是一段 JS,在里面调 tools.exec_command / apply_patch …:它跑的命令、改的文件记成
//!   `item_completed` 的 CommandExecution / FileChange(0.144–0.152 还有对不上 call_id 的 patch_apply_end),
//!   按开始时刻挂到那时在跑的 exec 下(跑到后台、exec 回了才跑完的也挂得回去);记录里没有的,从 JS 里认出字面写的命令
//! - 想的是加密的,只有少数老版本写了摘要
//! - 压缩是 `compacted` 行(可能好几 MB,只看类型):画一行。回退是 `thread_rolled_back{num_turns}`:画一行,
//!   前面那几轮还留在文件里,标成回退掉的(画灰、不编号;钱花出去了,照算)
//! - 子线程(子代理):`spawn_agent` 起的另一份 rollout,线程 id 在它的输出里(`agent_id`,老版本)或者它还没回的时候来的
//!   `sub_agent_activity` / `item_completed SubAgentActivity`(kind = started)里;按 id 在同一个 CODEX_HOME 里找记录
//! - 子线程、分叉出来的线程开头整段抄了父线程的历史(时间戳改成新线程创建的时刻,累计 token 接着父线程的往上加),
//!   不计,折成一块:有 history_base 的什么都没抄;有 subagent_history_start_ordinal 的,行上的 ordinal 比它小的是抄的;
//!   别的抄到第一个父线程里没有的 turn_id 为止(本机 134 份父线程都在,认得准);父线程的记录找不到时,
//!   到「前一轮还没结束又来了 task_started」为止
//!
//! 0.107 以前没有外层 type 的老格式不认。

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::LazyLock;

use memchr::memmem;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

use aca_core::{t, tr};

use crate::trace::{Asker, Change, Io, Kind, LABEL, Node, St, Trace, first_line, short_path, ts_ms, unmark};

type Src = (u64, u32);

/// 解析到一半的状态(跨着读新增的那截)
#[derive(Default)]
pub(crate) struct State {
    /// 读过第一行了;第一行没有外层 type 的是老格式
    seen: bool,
    pub(crate) old: bool,
    /// 版本(0, 155)
    pub(crate) ver: (u32, u32),
    /// 现在用的模型、思考强度
    model: String,
    effort: String,
    /// 这一轮是 task_started 开的(提问先到的老版本:task_started 来了就接着用提问开的那轮)
    started: bool,
    /// 上一句提问(user_message 和 UserMessage 都有时只算一次)
    prompt: u64,
    /// 还没等到用量的那次调用
    call: Option<usize>,
    /// 上一次的累计 token:额度刷新时重报的一样的数不算
    total: Option<u64>,
    /// 最后一个输入(工具输出 / 提问 / 开始)的时刻:下一次调用从它算起
    last_in: i64,
    /// exec:(开始时刻, 节点),按先后;还没回结果的那个
    execs: Vec<(i64, usize)>,
    open_exec: Option<usize>,
    /// exec 里真跑了命令 / 改了文件(头一行换成它们)
    exec: HashMap<usize, Ran>,
    /// 等着和下一个 web_search_call 配对的 web_search_end
    search: Option<Src>,
    /// 这份记录里开过的轮(子线程开头抄的是哪段,按父线程的认)
    pub(crate) turn_ids: HashSet<String>,
    /// 父线程开过的轮:打开子线程时给;单独打开分叉出来的线程时自己去读父线程的记录
    pub(crate) parent_turns: Option<HashSet<String>>,
    /// 开头抄父线程历史的那段怎么认、现在是不是还在那段里
    gate: Gate,
    pub(crate) copying: bool,
    /// 还没认出起了哪个子线程的 spawn_agent
    spawns: Vec<usize>,
    /// 这是个子线程:它的类型、名字(session_meta 里写的)
    pub(crate) role: String,
    pub(crate) nick: String,
    /// 派给它的那件事的名字:`agent_path` 的最后一段(0.144+ 的 `/root/layout_review`;派活的正文是加密的,只有这个名字看得懂)
    pub(crate) task: String,
}

/// 开头抄来的父线程历史到哪为止
#[derive(Default)]
enum Gate {
    /// 没抄(或者已经过了抄的那段)
    #[default]
    Own,
    /// 行上的 ordinal 到这个数为止是抄的
    Ordinal(u64),
    /// 到第一个父线程里没有的 turn_id
    Turns,
    /// 到前一轮还没结束又来的 task_started
    Open,
}

/// 一个 exec 里真跑了的
#[derive(Default)]
struct Ran {
    cmd: bool,
    files: Vec<String>,
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(borrow)]
    timestamp: Option<&'a str>,
    #[serde(rename = "type", borrow)]
    kind: Option<&'a str>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
    /// 0.154+:行的序号(子线程开头抄的那段按它认)
    ordinal: Option<u64>,
}

/// payload 里要用的字段(各种行的并起来;大的、形状不一的用 RawValue 等要时再解析)
#[derive(Deserialize, Default)]
#[serde(default)]
struct P<'a> {
    #[serde(rename = "type")]
    kind: Option<String>,
    cli_version: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    role: Option<String>,
    name: Option<String>,
    namespace: Option<String>,
    call_id: Option<String>,
    duration_ms: Option<f64>,
    exit_code: Option<f64>,
    success: Option<bool>,
    num_turns: Option<f64>,
    started_at_ms: Option<f64>,
    turn_id: Option<String>,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    subagent_history_start_ordinal: Option<f64>,
    agent_role: Option<String>,
    agent_nickname: Option<String>,
    agent_path: Option<String>,
    agent_thread_id: Option<String>,
    #[serde(rename = "kind", borrow)]
    kind_of: Option<&'a RawValue>,
    #[serde(borrow)]
    source: Option<&'a RawValue>,
    #[serde(borrow)]
    history_base: Option<&'a RawValue>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(borrow)]
    images: Option<&'a RawValue>,
    #[serde(borrow)]
    local_images: Option<&'a RawValue>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
    #[serde(borrow)]
    info: Option<&'a RawValue>,
    #[serde(borrow)]
    thread_settings: Option<&'a RawValue>,
    #[serde(borrow)]
    duration: Option<&'a RawValue>,
    #[serde(borrow)]
    item: Option<&'a RawValue>,
    #[serde(borrow)]
    arguments: Option<&'a RawValue>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    summary: Option<&'a RawValue>,
    #[serde(borrow)]
    action: Option<&'a RawValue>,
    #[serde(borrow)]
    output: Option<&'a RawValue>,
    #[serde(borrow)]
    changes: Option<&'a RawValue>,
    #[serde(borrow)]
    command: Option<&'a RawValue>,
    status: Option<String>,
}

/// item_completed 里的 item
#[derive(Deserialize, Default)]
#[serde(default)]
struct Item<'a> {
    #[serde(rename = "type")]
    kind: Option<String>,
    status: Option<String>,
    exit_code: Option<f64>,
    /// SubAgentActivity 的 started / interacted …(别的 item 的 kind 形状不一,先不解析)
    #[serde(rename = "kind", borrow)]
    kind_of: Option<&'a RawValue>,
    agent_thread_id: Option<String>,
    #[serde(borrow)]
    command: Option<&'a RawValue>,
    #[serde(borrow)]
    changes: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

fn key(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn value(r: Option<&RawValue>) -> Value {
    r.and_then(|r| serde_json::from_str(r.get()).ok()).unwrap_or(Value::Null)
}

fn string(r: Option<&RawValue>) -> Option<String> {
    r.and_then(|r| serde_json::from_str::<String>(r.get()).ok())
}

/// 行首那截里的时刻(压缩那行不解析)
fn stamp(head: &[u8]) -> i64 {
    let k = b"\"timestamp\":\"";
    let Some(at) = memmem::find(head, k) else { return 0 };
    let rest = &head[at + k.len()..];
    let end = rest.iter().position(|&b| b == b'"').unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end]).map_or(0, ts_ms)
}

/// 一行记录
pub(crate) fn line(t: &mut Trace, buf: &[u8], off: u64) {
    let head = &buf[..buf.len().min(120)];
    // 压缩那行带着整段替换后的历史,可能好几 MB:只看类型
    if memmem::find(head, b"\"type\":\"compacted\"").is_some() {
        mark(t, t!("压缩", "compaction").into(), stamp(head));
        return;
    }
    if memmem::find(head, b"\"type\":\"world_state\"").is_some() {
        return;
    }
    let Ok(l) = serde_json::from_slice::<Line>(buf) else { return };
    if !t.cx.seen {
        t.cx.seen = true;
        t.cx.old = l.kind.is_none();
    }
    if t.cx.old {
        return;
    }
    let ts = l.timestamp.map_or(0, ts_ms);
    if let (Gate::Ordinal(start), Some(o)) = (&t.cx.gate, l.ordinal) {
        t.cx.copying = o < *start;
    }
    let Some(p) = l.payload.and_then(|r| serde_json::from_str::<P>(r.get()).ok()) else { return };
    let src = (off, buf.len() as u32);
    match l.kind {
        Some("session_meta") if t.cx.ver == (0, 0) => {
            let v: Vec<u32> = p.cli_version.as_deref().unwrap_or("").split('.').map(|x| x.parse().unwrap_or(0)).collect();
            t.cx.ver = (v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0));
            if let Some(c) = p.cwd.clone().filter(|c| !c.is_empty()) {
                t.cwd = c;
            }
            meta(t, &p);
        }
        Some("turn_context") => {
            learn(t, p.model.as_deref().unwrap_or(""));
            if let Some(e) = p.effort {
                t.cx.effort = e;
            }
            if let Some(c) = p.cwd.filter(|c| !c.is_empty()) {
                t.cwd = c;
            }
        }
        Some("event_msg") => event(t, &p, ts, src),
        Some("response_item") => response(t, &p, ts, src),
        // 字段顺序不是 timestamp、type 在前的(上面没认出来)
        Some("compacted") => mark(t, t!("压缩", "compaction").into(), ts),
        // 这次回复写完了(0.153+)
        Some("token_usage_record") => {
            if let Some(c) = t.cx.call {
                let n = &mut t.nodes[c];
                n.t1 = ts.max(n.t0);
            }
        }
        _ => {}
    }
}

/// 第一行:是不是子线程 / 分叉出来的(开头抄了父线程的历史),是的话怎么认抄的那段
fn meta(t: &mut Trace, p: &P) {
    let src = value(p.source);
    let spawn = src.pointer("/subagent/thread_spawn");
    let get = |k: &str| spawn.and_then(|s| s.get(k)).and_then(Value::as_str).map(String::from);
    t.cx.role = p.agent_role.clone().or_else(|| get("agent_role")).unwrap_or_default();
    t.cx.nick = p.agent_nickname.clone().or_else(|| get("agent_nickname")).unwrap_or_default();
    let path = p.agent_path.clone().or_else(|| get("agent_path")).unwrap_or_default();
    t.cx.task = path.rsplit('/').next().unwrap_or("").to_string();
    let parent =
        get("parent_thread_id").or_else(|| p.parent_thread_id.clone()).or_else(|| p.forked_from_id.clone()).filter(|x| !x.is_empty());
    let Some(parent) = parent.filter(|_| p.history_base.is_none()) else { return };
    t.cx.copying = true;
    t.cx.gate = match p.subagent_history_start_ordinal {
        Some(n) => Gate::Ordinal(n as u64),
        None => {
            if t.cx.parent_turns.is_none() {
                t.cx.parent_turns = parent_turns(&t.path, &parent);
            }
            if t.cx.parent_turns.is_some() { Gate::Turns } else { Gate::Open }
        }
    };
}

/// 记录在哪个 CODEX_HOME 里(…/sessions/年/月/日/rollout-….jsonl)
fn home_of(path: &Path) -> Option<&Path> {
    path.ancestors().find(|a| a.file_name().is_some_and(|n| n == "sessions")).and_then(Path::parent)
}

/// 父线程的记录里开过的轮(只看 task_started 那几行)
fn parent_turns(path: &Path, parent: &str) -> Option<HashSet<String>> {
    use std::io::BufRead;
    let f = crate::workdir::codex_log(home_of(path)?, parent)?;
    let mut r = std::io::BufReader::new(std::fs::File::open(f).ok()?);
    let (mut out, mut buf) = (HashSet::new(), vec![]);
    while r.read_until(b'\n', &mut buf).ok()? > 0 {
        if memmem::find(&buf, b"\"task_started\"").is_some()
            && let Ok(l) = serde_json::from_slice::<Line>(&buf)
            && let Some(p) = l.payload.and_then(|r| serde_json::from_str::<P>(r.get()).ok())
            && let Some(id) = p.turn_id
        {
            out.insert(id);
        }
        buf.clear();
    }
    Some(out)
}

/// 子线程的任务名(见 `State::task`);不是子线程、老版本没有 = 空
pub(crate) fn task_of(t: &Trace) -> &str {
    &t.cx.task
}

/// 起了的子线程的记录:在同一个 CODEX_HOME 里按线程 id 找,读成一棵树挂到起它的工具下。
/// 起的时候记录可能还没写出来:下次读新增的时候再找,找 30 回(一分钟)还没有就算了。有新挂上的返回 true
pub(crate) fn find_subs(t: &mut Trace) -> bool {
    if t.want.is_empty() {
        return false;
    }
    let Some(home) = home_of(&t.path).map(Path::to_path_buf) else { return false };
    let mut grew = false;
    for (tool, id, tries) in std::mem::take(&mut t.want) {
        let sub = crate::workdir::codex_log(&home, &id)
            .and_then(|p| Trace::open_sub(p, crate::data::Agent::Codex, Some(t.cx.turn_ids.clone())).ok());
        match sub {
            Some(sub) => {
                t.nodes[tool].sub = Some(t.subs.len());
                t.subs.push(crate::trace::Sub { id, kind: sub.cx.role.clone(), desc: sub.cx.nick.clone(), t: sub });
                grew = true;
            }
            None if tries < 30 => t.want.push((tool, id, tries + 1)),
            None => {}
        }
    }
    grew
}

/// 认出了一个子线程:挂到起它的工具下(同一个子线程老版本输出里、新版本的事件里各说一次,只算一回)
fn spawned(t: &mut Trace, tool: usize, id: &str) {
    t.cx.spawns.retain(|&x| x != tool);
    if !id.is_empty() && !t.cx.copying && t.sub_ids.insert(id.to_string()) {
        t.want.push((tool, id.to_string(), 0));
    }
}

/// 子线程起来了(新版本):挂到还没认出子线程的那个 spawn_agent 下,没有就挂在在跑的 exec 下
fn started(t: &mut Trace, id: Option<&str>) {
    let Some(id) = id.filter(|_| !t.cx.copying) else { return };
    if t.sub_ids.contains(id) {
        return;
    }
    let tool = t.cx.spawns.iter().copied().find(|&x| t.nodes[x].sub.is_none()).or(t.cx.open_exec);
    if let Some(tool) = tool {
        spawned(t, tool, id);
    }
}

/// 回退了 n 轮:最近的 n 轮(还没回退掉的)标上
fn rollback(t: &mut Trace, n: usize) {
    let mut turns = vec![];
    for &r in t.roots.iter().rev() {
        match t.nodes[r].kind {
            Kind::Turn => turns.push(r),
            Kind::Copied => turns.extend(t.nodes[r].kids.iter().rev().copied().filter(|&k| t.nodes[k].kind == Kind::Turn)),
            _ => {}
        }
    }
    turns.retain(|&r| !t.nodes[r].rolled);
    for r in turns.into_iter().take(n) {
        t.nodes[r].rolled = true;
    }
}

/// 知道用的是哪个模型了。之前的调用还没模型(子线程常常先有用量、后有 thread_settings)的,就是它
fn learn(t: &mut Trace, m: &str) {
    if m.is_empty() || t.cx.model == m {
        return;
    }
    let first = t.cx.model.is_empty();
    t.cx.model = m.to_string();
    if first {
        let mi = t.model_idx(m);
        for n in t.nodes.iter_mut().filter(|n| n.kind == Kind::Call && n.model == 0) {
            n.model = mi;
        }
    }
}

fn mark(t: &mut Trace, label: String, ts: i64) {
    let i = t.add_top(Kind::Mark, ts, t.cx.copying);
    t.nodes[i].label = label;
    t.nodes[i].t1 = ts;
}

fn event(t: &mut Trace, p: &P, ts: i64, src: Src) {
    match p.kind.as_deref() {
        Some("task_started") => {
            // 抄来的那段到这里为止:父线程里没有的一轮 / 前一轮还没结束又开一轮
            let id = p.turn_id.clone().unwrap_or_default();
            let open = t.turn.is_some_and(|x| t.nodes[x].t1 == 0 && t.nodes[x].copied);
            let own = match t.cx.gate {
                Gate::Turns => t.cx.parent_turns.as_ref().is_some_and(|s| !s.contains(&id)),
                Gate::Open => open,
                _ => false,
            };
            if t.cx.copying && own {
                t.cx.copying = false;
                t.cx.gate = Gate::Own;
            }
            if !id.is_empty() {
                t.cx.turn_ids.insert(id);
            }
            start(t, ts);
        }
        Some("sub_agent_activity") if p.kind_of.is_some_and(|k| k.get() == "\"started\"") => started(t, p.agent_thread_id.as_deref()),
        Some("user_message") => {
            let n = |r: Option<&RawValue>| value(r).as_array().map_or(0, Vec::len);
            let mut text = string(p.message).unwrap_or_default();
            for _ in 0..n(p.images) + n(p.local_images) {
                text += t!("\n[图片]", "\n[image]");
            }
            prompt(t, &text, ts, src);
        }
        Some("item_completed") => item(t, p, ts, src),
        Some("exec_command_end" | "patch_apply_end" | "mcp_tool_call_end") => ended(t, p, src),
        Some("web_search_end") => t.cx.search = Some(src),
        Some("task_complete") => end(t, p, ts, src, false),
        Some("turn_aborted") => end(t, p, ts, src, true),
        Some("error") => {
            let turn = cur_turn(t, ts);
            let i = t.add(Kind::Fail, Some(turn), ts, t.cx.copying);
            let n = &mut t.nodes[i];
            n.label = first_line(&string(p.message).unwrap_or_default(), LABEL);
            n.t1 = ts;
            n.src.push(src);
            t.nodes[turn].state = St::Err;
        }
        Some("token_count") => usage(t, p, ts),
        Some("thread_settings_applied") => {
            let m = value(p.thread_settings).get("model").and_then(Value::as_str).unwrap_or("").to_string();
            learn(t, &m);
        }
        Some("thread_rolled_back") => {
            let n = p.num_turns.unwrap_or(0.0) as usize;
            rollback(t, n);
            mark(t, tr!("回退 {n} 轮", "rolled back {n} turns"), ts);
        }
        _ => {}
    }
}

/// 开始一轮。提问先到了的(老版本):那一轮就是它
fn start(t: &mut Trace, ts: i64) {
    t.cx.call = None;
    t.cx.open_exec = None;
    t.cx.last_in = ts;
    if let Some(cur) = t.turn
        && !t.cx.started
        && t.nodes[cur].copied == t.cx.copying
        && t.nodes[cur].t1 == 0
        && t.nodes[cur].kids.is_empty()
        && !t.nodes[cur].src.is_empty()
    {
        t.cx.started = true;
        return;
    }
    let i = t.add_top(Kind::Turn, ts, t.cx.copying);
    t.nodes[i].label = "…".into();
    t.turn = Some(i);
    t.cx.started = true;
    t.cx.prompt = 0;
}

/// 你说的:这一轮的提问,或者跑着的时候插的话(接在后面)
fn prompt(t: &mut Trace, text: &str, ts: i64, src: Src) {
    let h = key(text.trim());
    if t.cx.prompt == h {
        return;
    }
    t.cx.prompt = h;
    t.cx.last_in = ts;
    let turn = match t.turn {
        Some(x) if t.nodes[x].t1 == 0 && t.nodes[x].copied == t.cx.copying => x,
        _ => {
            let i = t.add_top(Kind::Turn, ts, t.cx.copying);
            t.turn = Some(i);
            t.cx.started = false;
            i
        }
    };
    let n = &mut t.nodes[turn];
    if n.src.is_empty() {
        n.label = first_line(text, LABEL);
    }
    n.src.push(src);
}

/// 一轮结束:写明的耗时、被打断、出错
fn end(t: &mut Trace, p: &P, ts: i64, src: Src, stopped: bool) {
    t.cx.call = None;
    t.cx.open_exec = None;
    t.cx.started = false;
    let Some(cur) = t.turn else { return };
    let n = &mut t.nodes[cur];
    match p.duration_ms.filter(|&d| d > 0.0) {
        Some(d) => {
            n.t1 = n.t0 + d as i64;
            n.exact = true;
        }
        None => n.t1 = ts.max(n.t0),
    }
    if stopped {
        n.state = St::Stopped;
        t.stop_open_tools(cur, ts);
        return;
    }
    if p.error.is_some() {
        n.state = St::Err;
        if !t.nodes[cur].kids.iter().any(|&k| t.nodes[k].kind == Kind::Fail) {
            let i = t.add(Kind::Fail, Some(cur), ts, t.cx.copying);
            let f = &mut t.nodes[i];
            f.label = first_line(&error_text(&value(p.error)), LABEL);
            f.t1 = ts;
            f.src.push(src);
        }
    }
}

/// 现在这一轮;没有(记录从半截开始)就补一个
fn cur_turn(t: &mut Trace, ts: i64) -> usize {
    match t.turn {
        Some(x) if t.nodes[x].copied == t.cx.copying => x,
        _ => {
            let i = t.add_top(Kind::Turn, ts, t.cx.copying);
            t.nodes[i].label = "…".into();
            t.turn = Some(i);
            i
        }
    }
}

/// 模型的输出归哪次调用:上一次用量之后的都归同一次,没有就开一次(从上一个输入算起)
fn call(t: &mut Trace, ts: i64) -> usize {
    if let Some(c) = t.cx.call {
        let n = &mut t.nodes[c];
        n.t1 = n.t1.max(ts);
        return c;
    }
    let turn = cur_turn(t, ts);
    let t0 = match t.cx.last_in {
        x if x > 0 && x <= ts => x,
        _ => ts,
    };
    let c = t.add(Kind::Call, Some(turn), t0, t.cx.copying);
    let model = t.cx.model.clone();
    let mi = t.model_idx(&model);
    let n = &mut t.nodes[c];
    n.model = mi;
    n.effort.clone_from(&t.cx.effort);
    n.t1 = ts;
    t.cx.call = Some(c);
    // 前面出过错、这次成了:这一轮不算出错
    if t.nodes[turn].state == St::Err {
        t.nodes[turn].state = St::Ok;
    }
    c
}

/// 一个「新的」token_count 就是一次调用的用量(和 `$` 一样:累计数没变的不算,输入里去掉缓存读、缓存写)
fn usage(t: &mut Trace, p: &P, ts: i64) {
    let Some(info) = p.info.and_then(|r| serde_json::from_str::<crate::usage::XInfo>(r.get()).ok()) else { return };
    let (Some(total), Some(last)) = (info.total_token_usage, info.last_token_usage) else { return };
    if t.cx.total == Some(total.total_tokens) {
        return;
    }
    t.cx.total = Some(total.total_tokens);
    let cached = last.cached_input_tokens.min(last.input_tokens);
    let write = last.cache_write_input_tokens.min(last.input_tokens - cached);
    let tok = [last.input_tokens - cached - write, write, 0, cached, last.output_tokens];
    if tok.iter().all(|&x| x == 0) {
        return;
    }
    let old = t.cx.ver < (0, 130);
    // 调用停在最后一个输出项,不拉长到这里(0.130 起 token_count 要等工具跑完才写)
    let c = match t.cx.call {
        Some(c) => c,
        None => call(t, ts),
    };
    let n = &mut t.nodes[c];
    n.tok = tok;
    n.think = last.reasoning_output_tokens;
    // 0.130 以前:token_count 紧跟着回复写;之后的要等工具跑完才写,不能当调用的结束
    if old {
        n.t1 = ts.max(n.t0);
    }
    t.cx.call = None;
}

fn response(t: &mut Trace, p: &P, ts: i64, src: Src) {
    match p.kind.as_deref() {
        Some("message") if p.role.as_deref() == Some("assistant") => {
            let c = call(t, ts);
            t.nodes[c].src.push(src);
            if t.nodes[c].label.is_empty() {
                let text = texts(&value(p.content), "output_text").join("\n");
                t.nodes[c].label = first_line(&unmark(&text), LABEL);
            }
        }
        Some("reasoning") => {
            let c = call(t, ts);
            t.nodes[c].src.push(src);
            if t.nodes[c].thought.is_empty() {
                let text = [texts(&value(p.summary), "summary_text"), texts(&value(p.content), "reasoning_text")].concat().join("\n");
                t.nodes[c].thought = first_line(&text, LABEL);
            }
        }
        Some("function_call") => {
            let args = string(p.arguments).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
            let (name, label) = fn_head(p.name.as_deref().unwrap_or(""), p.namespace.as_deref(), &args, &t.cwd);
            let i = tool(t, &name, label, p.call_id.as_deref().unwrap_or(""), ts, src);
            if p.name.as_deref() == Some("spawn_agent") && !t.cx.copying {
                t.cx.spawns.push(i);
            }
        }
        Some("custom_tool_call") => {
            let input = string(p.input).unwrap_or_default();
            let id = p.call_id.as_deref().unwrap_or("");
            match p.name.as_deref().unwrap_or("") {
                "exec" => {
                    let (name, label) = exec_head(&input, &t.cwd);
                    let i = tool(t, name, label, id, ts, src);
                    t.cx.execs.push((ts, i));
                    t.cx.open_exec = Some(i);
                    t.cx.exec.insert(i, Ran::default());
                }
                "apply_patch" => {
                    let label = files_label(&patch_files(&input), &t.cwd);
                    tool(t, "Patch", label, id, ts, src);
                }
                name => {
                    tool(t, name, first_line(&input, LABEL), id, ts, src);
                }
            }
        }
        Some("function_call_output" | "custom_tool_call_output") => output(t, p, ts, src),
        Some("web_search_call") => {
            let action = value(p.action);
            let q = action.get("query").and_then(Value::as_str).or_else(|| action.get("url").and_then(Value::as_str)).unwrap_or("");
            let label = first_line(q, LABEL);
            let i = tool(t, "Search", label, "", ts, src);
            if let Some(s) = t.cx.search.take() {
                t.nodes[i].src.push(s);
            }
            t.nodes[i].t1 = ts;
        }
        _ => {}
    }
}

/// 模型调了一个工具:挂在这次调用下
fn tool(t: &mut Trace, name: &str, label: String, id: &str, ts: i64, src: Src) -> usize {
    let c = call(t, ts);
    let i = t.add(Kind::Tool, Some(c), ts, t.cx.copying);
    let n = &mut t.nodes[i];
    n.name = name.to_string();
    n.label = label;
    n.id = id.to_string();
    n.src.push(src);
    if !id.is_empty() {
        t.tools.insert(id.to_string(), i);
    }
    i
}

/// 工具的输出回来了。输出头里写着退出码 / 脚本失败的,算出错
fn output(t: &mut Trace, p: &P, ts: i64, src: Src) {
    t.cx.last_in = ts;
    let Some(&i) = p.call_id.as_ref().and_then(|id| t.tools.get(id)) else { return };
    if t.cx.open_exec == Some(i) {
        t.cx.open_exec = None;
    }
    let text = out_text(&value(p.output));
    // 老版本 spawn_agent 的输出里写着子线程的 id
    if t.cx.spawns.contains(&i) {
        let id = serde_json::from_str::<Value>(&text).ok().and_then(|v| v.get("agent_id").and_then(Value::as_str).map(String::from));
        match id {
            Some(id) => spawned(t, i, &id),
            None => t.cx.spawns.retain(|&x| x != i),
        }
    }
    let (_, code, failed) = strip_head(&text);
    let n = &mut t.nodes[i];
    if !n.exact {
        n.t1 = ts.max(n.t0);
    }
    n.src.push(src);
    if n.state == St::Ok && (failed || code.is_some_and(|c| c != 0)) {
        n.state = St::Err;
    }
}

/// exec_command_end / patch_apply_end / mcp_tool_call_end:对得上 call_id 的写上耗时、出错;
/// 对不上的(0.144–0.152 在 exec 里跑的)挂在现在在跑的 exec 下
fn ended(t: &mut Trace, p: &P, src: Src) {
    let failed = p.exit_code.is_some_and(|x| x != 0.0) || p.success == Some(false) || p.status.as_deref() == Some("failed");
    match p.call_id.as_ref().and_then(|id| t.tools.get(id)).copied() {
        Some(i) => {
            let n = &mut t.nodes[i];
            n.src.push(src);
            if let Some(d) = dur_ms(&value(p.duration)) {
                n.t1 = n.t0 + d;
                n.exact = true;
            }
            if failed && n.state == St::Ok {
                n.state = St::Err;
            }
        }
        None => {
            let cmd = (p.kind.as_deref() == Some("exec_command_end")).then(|| cmd_text(&value(p.command)));
            let files = value(p.changes).as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
            inner(t, None, cmd, files, failed, src);
        }
    }
}

fn item(t: &mut Trace, p: &P, ts: i64, src: Src) {
    let Some(it) = p.item.and_then(|r| serde_json::from_str::<Item>(r.get()).ok()) else { return };
    let failed = it.status.as_deref() == Some("failed") || it.exit_code.is_some_and(|x| x != 0.0);
    match it.kind.as_deref() {
        Some("UserMessage") => prompt(t, &user_text(&value(it.content)), ts, src),
        Some("CommandExecution") => inner(t, p.started_at_ms, Some(cmd_text(&value(it.command))), vec![], failed, src),
        Some("FileChange") => {
            let files = value(it.changes).as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
            inner(t, p.started_at_ms, None, files, failed, src);
        }
        Some("McpToolCall" | "ImageView") => inner(t, p.started_at_ms, None, vec![], false, src),
        Some("SubAgentActivity") if it.kind_of.is_some_and(|k| k.get() == "\"started\"") => started(t, it.agent_thread_id.as_deref()),
        _ => {}
    }
}

/// exec 里跑的一条命令 / 改的文件:挂到开始时在跑的那个 exec 下(还没回结果的那个;它回了才跑完的,按开始时刻找)。
/// 头一行换成真跑了的命令(没有命令的写改的文件);有一条命令失败了就算出错(同 Claude 的 Bash 退出码不是 0)
fn inner(t: &mut Trace, start: Option<f64>, cmd: Option<String>, files: Vec<String>, failed: bool, src: Src) {
    let start = start.map(|x| x as i64).filter(|&x| x > 0);
    let e = match (t.cx.open_exec, start) {
        (Some(e), Some(s)) if t.nodes[e].t0 <= s => Some(e),
        (Some(e), None) => Some(e),
        (_, Some(s)) => t.cx.execs.iter().rev().find(|x| x.0 <= s).map(|x| x.1),
        (None, None) => t.cx.execs.last().map(|x| x.1),
    };
    let Some(e) = e else { return };
    let cwd = t.cwd.clone();
    let ran = t.cx.exec.entry(e).or_default();
    ran.files.extend(files);
    let n = &mut t.nodes[e];
    n.src.push(src);
    match cmd {
        Some(c) => {
            if !ran.cmd {
                ran.cmd = true;
                n.name = "Shell".into();
                n.label = first_line(&c, LABEL);
            }
            if failed && n.state == St::Ok {
                n.state = St::Err;
            }
        }
        None if !ran.cmd && !ran.files.is_empty() => {
            n.name = "Patch".into();
            n.label = files_label(&ran.files, &cwd);
        }
        None => {}
    }
}

// ---------- 摘要 ----------

/// 加密过的一段(0.144+ 线程之间递的话:`gAAAAAB…` 一长串,没有空格):不能当摘要写出来
fn sealed(x: &str) -> bool {
    x.len() > 40 && x.starts_with("gAAAA") && !x.contains(char::is_whitespace)
}

/// 函数工具在右边叫什么、头一行写什么
fn fn_head(name: &str, ns: Option<&str>, args: &Value, cwd: &str) -> (String, String) {
    let s = |k: &str| args.get(k).and_then(Value::as_str).filter(|x| !sealed(x));
    let one = |x: &str| first_line(x, LABEL);
    let (shown, label) = match name {
        "exec_command" => ("Shell", s("cmd").map(one).unwrap_or_default()),
        "shell" | "local_shell" => ("Shell", one(&cmd_text(args.get("command").unwrap_or(&Value::Null)))),
        "write_stdin" => {
            let id = args.get("session_id").map(|v| v.as_str().map_or_else(|| v.to_string(), String::from)).unwrap_or_default();
            let chars = s("chars").unwrap_or("");
            (
                "stdin",
                format!("#{id} {}", if chars.trim().is_empty() { t!("等输出", "waiting for output").to_string() } else { one(chars) }),
            )
        }
        "update_plan" => {
            let steps = args.get("plan").and_then(Value::as_array).map_or(0, Vec::len);
            ("Plan", s("explanation").map(one).filter(|x| !x.is_empty()).unwrap_or_else(|| tr!("{steps} 步", "{steps} steps")))
        }
        "view_image" => ("Image", s("path").map(|p| short_path(p, cwd)).unwrap_or_default()),
        "spawn_agent" => {
            let what = s("task_name").filter(|x| !x.is_empty()).or_else(|| s("message")).map(one).unwrap_or_default();
            ("Agent", [s("agent_type").unwrap_or(""), &what].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "))
        }
        "wait_agent" => ("Wait", String::new()),
        n if n.starts_with("mcp__") => {
            let mut parts = n.splitn(3, "__").skip(1);
            ("MCP", [parts.next(), parts.next()].into_iter().flatten().collect::<Vec<_>>().join(" · "))
        }
        _ if ns.is_some_and(|x| x.starts_with("mcp__")) => {
            ("MCP", format!("{} · {name}", ns.unwrap_or("").trim_start_matches("mcp__").trim_end_matches('_')))
        }
        _ => {
            let label = ["message", "prompt", "task", "description", "query", "path", "cmd", "explanation", "target", "id"]
                .iter()
                .find_map(|k| s(k))
                .map(one)
                .unwrap_or_default();
            return (name.to_string(), label);
        }
    };
    (shown.to_string(), label)
}

/// 几个文件写成一句:第一个 · 另 N 个
fn files_label(files: &[String], cwd: &str) -> String {
    match files {
        [] => String::new(),
        [one] => short_path(one, cwd),
        [first, rest @ ..] => tr!("{} 等 {} 个文件", "{} and {} files in all", short_path(first, cwd), rest.len() + 1),
    }
}

/// 命令:["/bin/zsh", "-lc", "…"] 取最后那段,别的用空格连起来
fn cmd_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [.., flag, script] if matches!(*flag, "-lc" | "-c") => script.to_string(),
                _ => parts.join(" "),
            }
        }
        _ => String::new(),
    }
}

static TOOLS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"tools\.([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap());
static CMD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"tools\.exec_command\(\s*\{[^}]*?\bcmd\s*:\s*"#).unwrap());
static PATCH_FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\*\*\* (?:Update|Add|Delete) File: ([^\n\\`"']+)"#).unwrap());

/// JS 的字符串字面量(s 从引号开始):内容;模板里的 ${…} 原样留着
fn js_str(s: &str) -> Option<String> {
    let mut cs = s.chars();
    let q = cs.next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let mut out = String::new();
    while let Some(c) = cs.next() {
        match c {
            '\\' => match cs.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = cs.by_ref().take(4).collect();
                    out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                }
                x => out.push(x),
            },
            c if c == q => return Some(out),
            c => out.push(c),
        }
    }
    None
}

/// exec 的 JS 里字面写的命令
fn lit_cmds(code: &str) -> Vec<String> {
    CMD.find_iter(code).filter_map(|m| js_str(&code[m.end()..])).filter(|c| !c.trim().is_empty()).collect()
}

/// exec 的 JS 里字面写的补丁(整段 *** Begin Patch … 所在的那个字符串)
fn lit_patch(code: &str) -> Option<String> {
    let at = code.find("*** Begin Patch")?;
    let q = code[..at].rfind(['`', '"', '\''])?;
    js_str(&code[q..])
}

/// exec 在右边叫什么、头一行写什么:字面写了命令的是 Shell,写了补丁的是 Patch,
/// 只调了一种别的工具的按那个工具叫;都认不出的叫 exec,写第一行代码(真跑了什么记录里有,再换)
fn exec_head(code: &str, cwd: &str) -> (&'static str, String) {
    if let Some(c) = lit_cmds(code).first() {
        return ("Shell", first_line(c, LABEL));
    }
    let files: Vec<String> = PATCH_FILE.captures_iter(code).map(|c| c[1].trim().to_string()).collect();
    if !files.is_empty() {
        return ("Patch", files_label(&files, cwd));
    }
    let first =
        code.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("//")).map(|l| first_line(l, LABEL)).unwrap_or_default();
    let mut used: Vec<&str> = TOOLS.captures_iter(code).map(|c| c.get(1).map_or("", |m| m.as_str())).collect();
    used.dedup();
    let name = match used.as_slice() {
        ["write_stdin", ..] if used.iter().all(|u| *u == "write_stdin") => "stdin",
        ["update_plan"] => "Plan",
        ["view_image"] => "Image",
        [m] if m.starts_with("mcp__") => "MCP",
        _ => "exec",
    };
    (name, first)
}

/// apply_patch 补丁里改的文件
fn patch_files(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| {
            l.strip_prefix("*** Update File: ").or_else(|| l.strip_prefix("*** Add File: ")).or_else(|| l.strip_prefix("*** Delete File: "))
        })
        .map(|p| p.trim().to_string())
        .collect()
}

/// {secs, nanos} / 秒数 → 毫秒
fn dur_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Object(m) => Some(m.get("secs")?.as_i64()? * 1000 + m.get("nanos").and_then(Value::as_i64).unwrap_or(0) / 1_000_000),
        Value::Number(n) => Some((n.as_f64()? * 1000.0) as i64),
        _ => None,
    }
}

// ---------- 读回来的内容(排成字在 trace_page) ----------

fn lines_of(t: &Trace, n: &Node) -> Vec<Value> {
    n.src.iter().filter_map(|&x| t.read_at(x)).collect()
}

/// 内容数组里某种块的字
fn texts(v: &Value, kind: &str) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some(kind))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .map(String::from)
        .collect()
}

/// UserMessage 的内容:字,图片写成 [图片]
fn user_text(v: &Value) -> String {
    let mut out = vec![];
    for b in v.as_array().into_iter().flatten() {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => out.push(b.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
            Some("image" | "local_image" | "input_image") => out.push(t!("[图片]", "[image]").into()),
            Some(k) => out.push(format!("[{k}]")),
            None => {}
        }
    }
    out.join("\n")
}

/// 工具输出:字符串,或者块的数组(字、图片)
fn out_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let mut out = String::new();
            for b in a {
                let piece = match b.get("type").and_then(Value::as_str) {
                    Some("input_text" | "output_text" | "text") => b.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                    Some("input_image" | "image") => t!("[图片]", "[image]").into(),
                    _ => continue,
                };
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out += &piece;
            }
            out
        }
        _ => String::new(),
    }
}

/// 去掉输出头(Script completed / Exit code: 0 / Process exited with code 0 / Wall time / Output: …),
/// 老的 apply_patch 输出是 JSON({"output", "metadata": {"exit_code"}})。返回:正文、退出码、脚本失败了没有
fn strip_head(text: &str) -> (String, Option<i64>, bool) {
    if text.starts_with('{')
        && let Ok(v) = serde_json::from_str::<Value>(text)
        && let Some(out) = v.get("output").and_then(Value::as_str)
    {
        return (out.to_string(), v.pointer("/metadata/exit_code").and_then(Value::as_i64), false);
    }
    let heads = [
        "Script ",
        "Wall time",
        "Exit code:",
        "Process exited with code",
        "Process running with",
        "Chunk ID:",
        "Original token count:",
        "Output:",
    ];
    if !heads.iter().any(|h| text.starts_with(h)) {
        return (text.to_string(), None, text.starts_with("apply_patch verification failed"));
    }
    let (mut code, mut failed, mut rest) = (None, false, "");
    let mut lines = text.split_inclusive('\n');
    let mut used = 0;
    for l in lines.by_ref() {
        let s = l.trim_end();
        if !heads.iter().any(|h| s.starts_with(h)) {
            break;
        }
        used += l.len();
        let num = |p: &str| s.strip_prefix(p).and_then(|x| x.trim().parse::<i64>().ok());
        code = code.or(num("Exit code:")).or(num("Process exited with code"));
        failed |= s.starts_with("Script failed");
        if s == "Output:" {
            rest = &text[used..];
            break;
        }
    }
    if rest.is_empty() && used < text.len() && !text[..used].contains("Output:") {
        rest = &text[used..];
    }
    (rest.to_string(), code, failed)
}

/// 一次调用想的、说的
pub(crate) fn said(t: &Trace, n: &Node) -> (Vec<String>, Vec<String>) {
    let (mut thought, mut text) = (vec![], vec![]);
    for v in lines_of(t, n) {
        let p = &v["payload"];
        match p.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                thought.extend(texts(&p["summary"], "summary_text"));
                thought.extend(texts(&p["content"], "reasoning_text"));
            }
            Some("message") => text.extend(texts(&p["content"], "output_text")),
            _ => {}
        }
    }
    let keep = |v: Vec<String>| v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    (keep(thought), keep(text))
}

/// 你问了什么(插的话接在后面);记录从一轮中间开始、没有提问的谁也不是
pub(crate) fn asked(t: &Trace, n: &Node) -> (Asker, String) {
    let mut parts = vec![];
    for v in lines_of(t, n) {
        let p = &v["payload"];
        match p.get("type").and_then(Value::as_str) {
            Some("user_message") => {
                let mut s = p.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                let imgs = ["images", "local_images"].iter().map(|k| p.get(k).and_then(Value::as_array).map_or(0, Vec::len)).sum::<usize>();
                for _ in 0..imgs {
                    s += t!("\n[图片]", "\n[image]");
                }
                parts.push(s);
            }
            Some("item_completed") => parts.push(user_text(&p["item"]["content"])),
            _ => {}
        }
    }
    if parts.is_empty() {
        return (
            Asker::Nobody,
            if n.label == "…" {
                t!("记录从这一轮中间开始", "the record starts mid-turn").into()
            } else {
                t!("没有提问", "no question").into()
            },
        );
    }
    (Asker::You, parts.join("\n\n").trim().to_string())
}

/// task_complete.error:字符串,或者带 message 的对象
fn error_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(m) => {
            m.get("message").and_then(Value::as_str).map_or_else(|| serde_json::to_string_pretty(v).unwrap_or_default(), String::from)
        }
        _ => String::new(),
    }
}

pub(crate) fn fail_text(t: &Trace, n: &Node) -> String {
    let Some(v) = n.src.first().and_then(|&x| t.read_at(x)) else { return String::new() };
    let p = &v["payload"];
    match p.get("type").and_then(Value::as_str) {
        Some("error") => p.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => error_text(&p["error"]),
    }
}

/// 改了哪些文件:{路径: {type: update, unified_diff} / {type: add | delete, content}}
fn file_changes(v: &Value) -> Vec<(String, Vec<Change>)> {
    let Some(m) = v.as_object() else { return vec![] };
    m.iter()
        .map(|(path, c)| {
            let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
            let ch = match s("type") {
                "add" => s("content").lines().zip(1..).map(|(l, k)| Change { c: '+', no: Some(k), text: l.to_string() }).collect(),
                "delete" => s("content").lines().zip(1..).map(|(l, k)| Change { c: '-', no: Some(k), text: l.to_string() }).collect(),
                _ => unified(s("unified_diff")),
            };
            (c.get("move_path").and_then(Value::as_str).unwrap_or(path).to_string(), ch)
        })
        .collect()
}

/// 统一 diff(@@ -7,3 +7,4 @@ 起头的几处)→ 带行号的改动
fn unified(diff: &str) -> Vec<Change> {
    static AT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@").unwrap());
    let mut out = vec![];
    let mut cur: Option<(u64, u64, Vec<&str>)> = None;
    let flush = |cur: &mut Option<(u64, u64, Vec<&str>)>, out: &mut Vec<Change>| {
        if let Some((a, b, lines)) = cur.take() {
            if !out.is_empty() {
                out.push(Change { c: '~', no: None, text: String::new() });
            }
            out.extend(crate::trace_claude::hunk(a, b, lines.into_iter()));
        }
    };
    for l in diff.lines() {
        if let Some(c) = AT.captures(l) {
            flush(&mut cur, &mut out);
            cur = Some((c[1].parse().unwrap_or(1), c[2].parse().unwrap_or(1), vec![]));
        } else if let Some((_, _, lines)) = cur.as_mut() {
            lines.push(l);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// apply_patch 的补丁 → 每个文件的改动(补丁里没有行号;新建的从第 1 行起)
fn patch_edits(patch: &str) -> Vec<(String, Vec<Change>)> {
    let mut out: Vec<(String, Vec<Change>)> = vec![];
    let mut add = false;
    for l in patch.lines() {
        if let Some(p) = l.strip_prefix("*** Update File: ").or_else(|| l.strip_prefix("*** Delete File: ")) {
            out.push((p.trim().to_string(), vec![]));
            add = false;
        } else if let Some(p) = l.strip_prefix("*** Add File: ") {
            out.push((p.trim().to_string(), vec![]));
            add = true;
        } else if let Some(p) = l.strip_prefix("*** Move to: ") {
            if let Some(last) = out.last_mut() {
                last.0 = p.trim().to_string();
            }
        } else if l.starts_with("*** ") {
            continue;
        } else if let Some((_, ch)) = out.last_mut() {
            if l.starts_with("@@") {
                if !ch.is_empty() {
                    ch.push(Change { c: '~', no: None, text: String::new() });
                }
                continue;
            }
            let mut cs = l.chars();
            let c = cs.next().unwrap_or(' ');
            let c = if matches!(c, '+' | '-') { c } else { ' ' };
            let no = add.then(|| ch.len() as u64 + 1);
            ch.push(Change { c, no, text: if l.is_empty() { String::new() } else { cs.as_str().to_string() } });
        }
    }
    out
}

/// update_plan 的计划:✓ 做完的、▸ 在做的、· 还没做的
fn plan_text(args: &Value) -> String {
    let mut out = vec![];
    if let Some(e) = args.get("explanation").and_then(Value::as_str).filter(|e| !e.trim().is_empty()) {
        out.push(e.trim().to_string());
    }
    for s in args.get("plan").and_then(Value::as_array).into_iter().flatten() {
        let mark = match s.get("status").and_then(Value::as_str) {
            Some("completed") => "✓",
            Some("in_progress") => "▸",
            _ => "·",
        };
        out.push(format!("{mark} {}", s.get("step").and_then(Value::as_str).unwrap_or("")));
    }
    out.join("\n")
}

/// 一个工具的内容。exec 的命令、改动取记录里真跑了的,没有才从 JS 里认;都认不出的露 JS 本身
pub(crate) fn io(t: &Trace, n: &Node) -> Io {
    let mut io = Io::default();
    let (mut code, mut patch, mut plan) = (None, None, false);
    let (mut ran, mut ran_out, mut edits, mut exits) = (vec![], vec![], vec![], vec![]);
    for v in lines_of(t, n) {
        let p = &v["payload"];
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
        match p.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let args: Value = serde_json::from_str(s("arguments")).unwrap_or(Value::Null);
                let a = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
                match s("name") {
                    "exec_command" => io.cmds.push(a("cmd").to_string()),
                    "shell" | "local_shell" => io.cmds.push(cmd_text(args.get("command").unwrap_or(&Value::Null))),
                    "write_stdin" if !a("chars").is_empty() => io.input = Some(("text", a("chars").to_string())),
                    "write_stdin" => {}
                    "update_plan" => {
                        io.out = Some(plan_text(&args));
                        plan = true;
                    }
                    "view_image" => io.input = Some(("text", a("path").to_string())),
                    _ => io.input = Some(("json", serde_json::to_string_pretty(&args).unwrap_or_default())),
                }
            }
            Some("custom_tool_call") => match s("name") {
                "exec" => code = Some(s("input").to_string()),
                "apply_patch" => patch = Some(s("input").to_string()),
                _ => io.input = Some(("text", s("input").to_string())),
            },
            Some("function_call_output" | "custom_tool_call_output") if !plan => {
                let (text, code, _) = strip_head(&out_text(&p["output"]));
                io.out = Some(text);
                io.exit = io.exit.or(code);
            }
            Some("exec_command_end") if p.get("call_id").and_then(Value::as_str) == Some(n.id.as_str()) => {
                if let Some(o) = p.get("aggregated_output").and_then(Value::as_str).filter(|o| !o.is_empty()) {
                    ran_out.push(o.to_string());
                }
                exits.extend(p.get("exit_code").and_then(Value::as_i64));
            }
            Some("exec_command_end") => {
                ran.push(cmd_text(&p["command"]));
                ran_out.push(s("aggregated_output").to_string());
                exits.extend(p.get("exit_code").and_then(Value::as_i64));
            }
            Some("patch_apply_end") => edits.extend(file_changes(&p["changes"])),
            Some("item_completed") => {
                let it = &p["item"];
                match it.get("type").and_then(Value::as_str) {
                    Some("CommandExecution") => {
                        ran.push(cmd_text(&it["command"]));
                        ran_out.push(it.get("aggregated_output").and_then(Value::as_str).unwrap_or("").to_string());
                        exits.extend(it.get("exit_code").and_then(Value::as_i64));
                    }
                    Some("FileChange") => edits.extend(file_changes(&it["changes"])),
                    _ => {}
                }
            }
            Some("web_search_call") => {
                let q = p["action"].get("query").and_then(Value::as_str).unwrap_or("");
                if !q.is_empty() {
                    io.input = Some(("text", q.to_string()));
                }
            }
            _ => {}
        }
    }
    if let Some(code) = code {
        io.cmds = if ran.is_empty() { lit_cmds(&code) } else { ran };
        if edits.is_empty()
            && let Some(p) = lit_patch(&code)
        {
            edits = patch_edits(&p);
        }
        if io.cmds.is_empty() && edits.is_empty() {
            io.input = Some(("js", code));
        }
        // 脚本自己没打印什么:只跑了一条命令的,拿那条命令的输出
        if io.out.as_deref().is_none_or(|o| o.trim().is_empty()) && ran_out.len() == 1 {
            io.out = ran_out.pop();
        }
    } else if let Some(p) = patch {
        if edits.is_empty() {
            edits = patch_edits(&p);
        }
    } else if let Some(o) = ran_out.pop().filter(|_| !io.cmds.is_empty()) {
        // exec_command:exec_command_end 里是完整的输出(给模型的那份可能截过)
        io.out = Some(o);
    }
    // 退出码:有失败的写第一个失败的
    if let Some(&x) = exits.iter().find(|&&x| x != 0).or(exits.last()) {
        io.exit = Some(x);
    }
    if let Some((path, ch)) = edits.first() {
        let line = ch.iter().find(|c| c.c == '+').or_else(|| ch.iter().find(|c| c.c == '-')).and_then(|c| c.no).unwrap_or(1);
        io.file = Some((path.clone(), line));
    }
    io.edits = edits;
    io
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::data::Agent;
    use serde_json::json;
    use std::io::Write;
    use std::path::PathBuf;

    pub(crate) fn at(ms: u32) -> String {
        format!("2026-09-30T01:{:02}:{:02}.{:03}Z", ms / 60_000, ms / 1000 % 60, ms % 1000)
    }

    fn row(ms: u32, kind: &str, payload: Value) -> Value {
        json!({"timestamp": at(ms), "type": kind, "payload": payload})
    }

    fn ev(ms: u32, payload: Value) -> Value {
        row(ms, "event_msg", payload)
    }

    fn ri(ms: u32, payload: Value) -> Value {
        row(ms, "response_item", payload)
    }

    fn meta(ver: &str) -> Value {
        row(
            0,
            "session_meta",
            json!({"id": "th", "cwd": "/p/app", "cli_version": ver, "source": "cli", "base_instructions": {"text": "long \"token_count\" text"}}),
        )
    }

    /// 一个新的累计数:total 每次都不一样
    fn tokens(ms: u32, total: u64, input: u64, cached: u64, out: u64) -> Value {
        ev(
            ms,
            json!({"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": total, "cached_input_tokens": 0, "output_tokens": 0, "total_tokens": total},
            "last_token_usage": {"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0, "output_tokens": out, "reasoning_output_tokens": 3, "total_tokens": input + out}},
            "rate_limits": null}),
        )
    }

    fn say(ms: u32, text: &str, phase: &str) -> Value {
        ri(ms, json!({"type": "message", "role": "assistant", "phase": phase, "content": [{"type": "output_text", "text": text}]}))
    }

    pub(crate) fn file(name: &str, lines: &[Value]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-trace-cx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("rollout-2026-09-30T01-00-00-th.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        for l in lines {
            // 照 Codex 写的顺序:timestamp、type 在前,payload 在后
            match (l.get("timestamp"), l.get("type"), l.get("payload")) {
                (Some(ts), Some(k), Some(p)) => writeln!(f, r#"{{"timestamp":{ts},"type":{k},"payload":{p}}}"#).unwrap(),
                _ => writeln!(f, "{l}").unwrap(),
            }
        }
        p
    }

    fn open(name: &str, lines: &[Value]) -> Trace {
        Trace::open_born(file(name, lines), Agent::Codex, 0).unwrap()
    }

    fn kinds(t: &Trace, i: usize) -> Vec<Kind> {
        t.nodes[i].kids.iter().map(|&k| t.nodes[k].kind).collect()
    }

    /// 0.155 的一轮:exec 里跑两条命令(一条跑到后台,exec 回了才完),下一次调用改文件,最后回复
    pub(crate) fn fixture(name: &str) -> (Trace, PathBuf) {
        let p = file(name, &fixture_lines());
        (Trace::open_born(p.clone(), Agent::Codex, 0).unwrap(), p)
    }

    fn fixture_lines() -> Vec<Value> {
        vec![
            meta("0.155.0"),
            ev(1000, json!({"type": "task_started", "turn_id": "t1", "started_at": 1})),
            row(1010, "turn_context", json!({"turn_id": "t1", "cwd": "/p/app", "model": "gpt-5.5", "effort": "high", "summary": "auto"})),
            ri(
                1020,
                json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>…</environment_context>"}]}),
            ),
            ev(
                1030,
                json!({"type": "item_completed", "thread_id": "th", "turn_id": "t1", "started_at_ms": 0, "completed_at_ms": 0,
                            "item": {"type": "UserMessage", "id": "u1", "content": [{"type": "text", "text": "跑一下测试\n顺便修掉"}, {"type": "local_image", "path": "/x.png"}]}}),
            ),
            ri(5000, json!({"type": "reasoning", "summary": [], "encrypted_content": "gAAA"})),
            say(6000, "先跑测试", "commentary"),
            ri(
                8000,
                json!({"type": "custom_tool_call", "name": "exec", "call_id": "c1", "status": "completed",
                            "input": "const r = await tools.exec_command({cmd: \"cargo test --release\", workdir: \"/p/app\"});\ntext(r.output);\nawait tools.exec_command({cmd: 'sleep 60', yield_time_ms: 10});"}),
            ),
            row(8100, "token_usage_record", json!({"response_id": "r1", "usage": {}})),
            ev(
                9000,
                json!({"type": "item_completed", "started_at_ms": ts_ms(&at(8200)), "completed_at_ms": ts_ms(&at(9000)),
                            "item": {"type": "CommandExecution", "id": "x1", "command": ["/bin/zsh", "-lc", "cargo test --release"], "cwd": "/p/app",
                                     "aggregated_output": "running 3 tests\ntest a ... ok\ntest b ... FAILED\n", "exit_code": 101, "status": "failed",
                                     "duration": {"secs": 0, "nanos": 800_000_000}}}),
            ),
            ri(
                9500,
                json!({"type": "custom_tool_call_output", "call_id": "c1", "output": [
                {"type": "input_text", "text": "Script completed\nWall time 1.5 seconds\nOutput:\n"},
                {"type": "input_text", "text": "running 3 tests\ntest a ... ok\ntest b ... FAILED"}]}),
            ),
            tokens(9510, 1000, 900, 800, 50),
            ri(12_000, json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "**修 b**\n改断言"}]})),
            ri(
                13_000,
                json!({"type": "custom_tool_call", "name": "exec", "call_id": "c2",
                             "input": "const patch = `*** Begin Patch\n*** Update File: src/b.rs\n@@\n-    assert_eq!(x, 1);\n+    assert_eq!(x, 2);\n*** End Patch`;\nawait tools.apply_patch(patch);"}),
            ),
            // 上一次的 sleep 60 这时才跑完:按开始时刻挂回第一个 exec
            ev(
                13_050,
                json!({"type": "item_completed", "started_at_ms": ts_ms(&at(9100)), "completed_at_ms": ts_ms(&at(13_050)),
                             "item": {"type": "CommandExecution", "id": "x2", "command": ["/bin/zsh", "-lc", "sleep 60"], "aggregated_output": "", "exit_code": 0, "status": "completed"}}),
            ),
            ev(
                13_100,
                json!({"type": "item_completed", "started_at_ms": ts_ms(&at(13_100)), "completed_at_ms": ts_ms(&at(13_100)),
                             "item": {"type": "FileChange", "id": "x3", "status": "completed", "changes": {"/p/app/src/b.rs": {"type": "update", "move_path": null,
                                      "unified_diff": "@@ -7,3 +7,3 @@\n fn b() {\n-    assert_eq!(x, 1);\n+    assert_eq!(x, 2);\n }\n"}}}}),
            ),
            ri(
                13_200,
                json!({"type": "custom_tool_call_output", "call_id": "c2", "output": [{"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"}]}),
            ),
            tokens(13_210, 2000, 1000, 900, 80),
            // 额度刷新时重报一样的累计数:不是新的调用
            tokens(13_220, 2000, 1000, 900, 80),
            say(20_000, "改好了:**b** 的断言写错了。\n\n- 测试全过", "final_answer"),
            tokens(20_100, 3000, 1100, 1000, 40),
            ev(
                20_200,
                json!({"type": "task_complete", "turn_id": "t1", "duration_ms": 19_000, "time_to_first_token_ms": 4000, "last_agent_message": "改好了"}),
            ),
            row(21_000, "compacted", json!({"message": "", "replacement_history": [{"type": "message"}]})),
            ev(30_000, json!({"type": "task_started", "turn_id": "t2"})),
            ev(30_010, json!({"type": "user_message", "message": "再跑一次", "images": [], "local_images": []})),
            ri(
                31_000,
                json!({"type": "function_call", "name": "exec_command", "call_id": "f1", "arguments": "{\"cmd\":\"cargo build\",\"yield_time_ms\":1000}"}),
            ),
            ev(
                35_000,
                json!({"type": "exec_command_end", "call_id": "f1", "command": ["/bin/zsh", "-lc", "cargo build"], "aggregated_output": "Finished\n", "exit_code": 0,
                             "duration": {"secs": 3, "nanos": 500_000_000}}),
            ),
            ri(
                35_100,
                json!({"type": "function_call_output", "call_id": "f1",
                             "output": "Chunk ID: ab12\nWall time: 3.5 seconds\nProcess exited with code 0\nOriginal token count: 2\nOutput:\nFinished"}),
            ),
            ri(
                36_000,
                json!({"type": "custom_tool_call", "name": "apply_patch", "call_id": "p1",
                             "input": "*** Begin Patch\n*** Add File: docs/n.md\n+# 笔记\n+第二行\n*** Update File: src/c.rs\n@@\n-a\n+b\n*** End Patch"}),
            ),
            ri(
                36_500,
                json!({"type": "custom_tool_call_output", "call_id": "p1", "output": "apply_patch verification failed: no such line"}),
            ),
            tokens(36_600, 4000, 500, 0, 30),
            ev(40_000, json!({"type": "turn_aborted", "turn_id": "t2", "reason": "interrupted", "duration_ms": 10_000})),
        ]
    }

    #[test]
    fn a_turn_is_its_calls_each_new_token_count_is_one_and_the_money_matches_the_bill() {
        let (t, p) = fixture("turns");
        let roots: Vec<Kind> = t.roots.iter().map(|&r| t.nodes[r].kind).collect();
        assert_eq!(roots, [Kind::Turn, Kind::Mark, Kind::Turn]);
        let turn = t.roots[0];
        let n = &t.nodes[turn];
        assert_eq!((n.label.as_str(), n.exact, t.dur(turn, false, 0), n.state), ("跑一下测试", true, 19_000, St::Ok));
        // 三次调用:重报的累计数不算
        assert_eq!(kinds(&t, turn), [Kind::Call, Kind::Call, Kind::Call]);
        let c1 = &t.nodes[n.kids[0]];
        // 从提问算起,到 token_usage_record;输入去掉缓存读
        assert_eq!((c1.t1 - c1.t0, c1.tok, c1.think, c1.label.as_str()), (8100 - 1030, [100, 0, 0, 800, 50], 3, "先跑测试"));
        assert_eq!((t.models[c1.model].as_str(), c1.effort.as_str()), ("gpt-5.5", "high"));
        let c2 = &t.nodes[n.kids[1]];
        // 第二次调用从上一个工具输出算起;想的有摘要就写第一句
        assert_eq!((c2.t0, c2.thought.as_str()), (ts_ms(&at(9500)), "**修 b**"));
        assert_eq!(t.nodes[n.kids[2]].label, "改好了:b 的断言写错了。");
        assert_eq!(t.nodes[t.roots[1]].label, "压缩");
        // 钱:和 $ 面板读同一份记录算的一样
        let s = t.total(false, 0).2;
        let mut seen = crate::usage::tests::codex_seen(&p);
        let bill: f64 = seen.drain(..).map(|(m, tok)| crate::usage::call_cost(&m, tok, false, false, 0).unwrap()).sum();
        assert_eq!(s.calls, 4);
        assert!((s.cost - bill).abs() < 1e-12, "{} vs {bill}", s.cost);
    }

    #[test]
    fn what_an_exec_ran_hangs_under_it_even_when_it_finishes_later() {
        let (t, _) = fixture("exec");
        let c1 = t.nodes[t.roots[0]].kids[0];
        let e1 = t.nodes[c1].kids[0];
        let n = &t.nodes[e1];
        // 头一行写真跑了的第一条命令;测试失败了算出错,退出码写失败的那条
        assert_eq!((n.name.as_str(), n.label.as_str(), n.t1 - n.t0, n.state), ("Shell", "cargo test --release", 1500, St::Err));
        let io = io(&t, n);
        assert_eq!(io.cmds, ["cargo test --release", "sleep 60"]);
        assert_eq!(io.out.as_deref(), Some("running 3 tests\ntest a ... ok\ntest b ... FAILED"));
        assert_eq!(io.exit, Some(101));
        // 第二个 exec:改文件,带行号(统一 diff 里的),e 停在改的那行
        let e2 = t.nodes[t.nodes[t.roots[0]].kids[1]].kids[0];
        assert_eq!((t.nodes[e2].name.as_str(), t.nodes[e2].label.as_str()), ("Patch", "src/b.rs"));
        let io = crate::trace::io(&t, e2);
        let rows: Vec<(char, Option<u64>)> = io.edits[0].1.iter().map(|c| (c.c, c.no)).collect();
        assert_eq!(rows, [(' ', Some(7)), ('-', Some(8)), ('+', Some(8)), (' ', Some(9))]);
        assert_eq!(io.file, Some(("/p/app/src/b.rs".into(), 8)));
        assert!(io.input.is_none() && io.cmds.is_empty());
    }

    #[test]
    fn old_style_tools_their_end_events_and_an_interrupted_turn() {
        let (t, _) = fixture("old-tools");
        let turn = t.roots[2];
        assert_eq!((t.nodes[turn].label.as_str(), t.nodes[turn].state), ("再跑一次", St::Stopped));
        let call = t.nodes[turn].kids[0];
        let (build, patch) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        let b = &t.nodes[build];
        // 耗时用 exec_command_end 写明的,输出去掉头
        assert_eq!((b.name.as_str(), b.label.as_str(), b.t1 - b.t0, b.exact, b.state), ("Shell", "cargo build", 3500, true, St::Ok));
        assert_eq!(io(&t, b).out.as_deref(), Some("Finished\n"));
        // apply_patch 校验没过:出错;两个文件,新建的从第 1 行起
        let p = &t.nodes[patch];
        assert_eq!((p.name.as_str(), p.label.as_str(), p.state), ("Patch", "docs/n.md 等 2 个文件", St::Err));
        let io = io(&t, p);
        assert_eq!(io.edits.len(), 2);
        assert_eq!(io.edits[0].1.iter().map(|c| (c.c, c.no)).collect::<Vec<_>>(), [('+', Some(1)), ('+', Some(2))]);
        assert_eq!(io.edits[1].1.iter().map(|c| c.c).collect::<String>(), "-+");
    }

    #[test]
    fn the_chat_steps_and_detail_read_back_what_codex_said_and_ran() {
        let (t, _) = fixture("page");
        let turn = t.roots[0];
        let plain = |ps: &[crate::trace::Para], w: usize| {
            crate::trace::wrap(ps, w).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>()
        };
        let c = crate::trace::chat(&t, turn, false, 0);
        assert_eq!(plain(&c.asked, 60), ["你  跑一下测试", "    顺便修掉", "    [图片]"]);
        assert_eq!(plain(&c.reply, 60)[0], "Codex  改好了:b 的断言写错了。");
        // 右边:exec 收着露输出开头,靠右写几条命令;改文件的露改动行
        let steps: Vec<String> = crate::trace::steps(&t, turn, false, 0, &|_| false).iter().flat_map(|s| plain(&s.paras, 70)).collect();
        assert!(steps.iter().any(|l| l.starts_with("▸ Shell cargo test --release") && l.ends_with("2 条命令 · 1.5s")), "{steps:#?}");
        assert!(steps.iter().any(|l| l == "  running 3 tests"), "{steps:#?}");
        assert!(steps.iter().any(|l| l.starts_with("▸ Patch src/b.rs") && l.ends_with("+1 -1")), "{steps:#?}");
        assert!(steps.iter().any(|l| l == "  8 +     assert_eq!(x, 2);"), "{steps:#?}");
        assert!(steps.iter().any(|l| l == "思考 修 b · 改断言"), "{steps:#?}");
        // 展开:两条命令都写出来
        let open: Vec<String> = crate::trace::steps(&t, turn, false, 0, &|_| true).iter().flat_map(|s| plain(&s.paras, 70)).collect();
        assert!(open.windows(2).any(|w| w[0] == "  $ cargo test --release" && w[1] == "  $ sleep 60"), "{open:#?}");
        // 详情:命令、退出码;e 写成 Markdown
        let e1 = t.nodes[t.nodes[turn].kids[0]].kids[0];
        let d = plain(&crate::trace::detail(&t, e1, false, 0), 80);
        assert!(d.iter().any(|l| l == "cargo test --release") && d.iter().any(|l| l == "sleep 60"), "{d:#?}");
        let Some(crate::trace::View::Text { text, .. }) = crate::trace::view(&t, turn, false, 0) else { panic!() };
        for want in [
            "## 你\n\n跑一下测试",
            "### Shell · cargo test --release",
            "```sh\ncargo test --release\nsleep 60\n```",
            "```diff\n@@ 第 7 行 @@",
        ] {
            assert!(text.contains(want), "缺「{want}」:{text}");
        }
    }

    #[test]
    fn a_block_whose_signature_stays_the_same_looks_the_same_after_the_record_grows() {
        // 在跑的会话:记录读到第一轮一半,再接着读下去。签名没变的块照旧用(界面不重排),必须和重排的一模一样
        let all = fixture_lines();
        let p = file("grow", &all[..13]);
        let mut t = Trace::open_born(p.clone(), Agent::Codex, 0).unwrap();
        let turn = t.roots[0];
        let now = ts_ms(&at(30_000));
        let snap = |t: &Trace, open: bool| -> Vec<(usize, u64, Vec<String>)> {
            let list = crate::trace::step_nodes(t, turn);
            let open = |_| open;
            (0..list.len())
                .map(|k| {
                    let s = crate::trace::step(t, turn, k, true, now, &open).unwrap();
                    (
                        list[k],
                        crate::trace::step_sig(t, turn, &list, k, true, now, &open),
                        crate::trace::wrap(&s.paras, 70).iter().map(crate::ansi::plain).collect(),
                    )
                })
                .collect()
        };
        for open in [false, true] {
            let mut before = snap(&t, open);
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            for l in &all[13..22] {
                let (ts, k, pl) = (&l["timestamp"], &l["type"], &l["payload"]);
                writeln!(f, r#"{{"timestamp":{ts},"type":{k},"payload":{pl}}}"#).unwrap();
            }
            assert!(t.tail().unwrap());
            let after = snap(&t, open);
            let mut kept = 0;
            for (node, sig, text) in &after {
                if let Some(b) = before.iter().find(|b| b.0 == *node).filter(|b| b.1 == *sig) {
                    assert_eq!(&b.2, text, "签名没变、样子变了:节点 {node}");
                    kept += 1;
                }
            }
            // 头(整轮的合计)、后台命令跑完了的 exec、不再是最后的那次调用要重排;第一次调用还是老样子
            let c1 = t.nodes[turn].kids[0];
            assert!(before.iter().chain(&after).filter(|b| b.0 == c1).map(|b| b.1).collect::<std::collections::HashSet<_>>().len() == 1);
            assert!(kept >= 1 && kept < after.len(), "kept={kept} of {}", after.len());
            // 重新来一遍(下一轮 open 从同一个起点长)
            before.clear();
            let _ = std::fs::write(
                &p,
                all[..13]
                    .iter()
                    .map(|l| format!("{{\"timestamp\":{},\"type\":{},\"payload\":{}}}\n", l["timestamp"], l["type"], l["payload"]))
                    .collect::<String>(),
            );
            t = Trace::open_born(p.clone(), Agent::Codex, 0).unwrap();
        }
    }

    #[test]
    fn js_literals_and_output_heads() {
        assert_eq!(
            lit_cmds(r#"await tools.exec_command({ cmd: "echo \"hi\"\nls", workdir: '/x' }); tools.exec_command({cmd: `git log -1`})"#),
            ["echo \"hi\"\nls", "git log -1"]
        );
        assert_eq!(lit_cmds("tools.exec_command({cmd})"), Vec::<String>::new());
        assert_eq!(
            exec_head("await tools.write_stdin({session_id: 3, chars: ''})", ""),
            ("stdin", "await tools.write_stdin({session_id: 3, chars: ''})".to_string())
        );
        assert_eq!(strip_head("Exit code: 1\nWall time: 0.2 seconds\nOutput:\nboom"), ("boom".to_string(), Some(1), false));
        assert_eq!(strip_head("Script failed\nWall time 0.1 seconds\nOutput:\nTypeError"), ("TypeError".to_string(), None, true));
        assert_eq!(
            strip_head(r#"{"output":"Success.\n","metadata":{"exit_code":0,"duration_seconds":0.1}}"#),
            ("Success.\n".to_string(), Some(0), false)
        );
        assert_eq!(strip_head("plain"), ("plain".to_string(), None, false));
        assert_eq!(cmd_text(&json!(["bash", "-lc", "make"])), "make");
        assert_eq!(cmd_text(&json!(["git", "status"])), "git status");
    }

    /// 一个 CODEX_HOME:sessions/2026/09/30/rollout-…-<id>.jsonl(行上有 ordinal 的照 Codex 写在 payload 前面)
    fn home_file(home: &std::path::Path, id: &str, lines: &[Value]) -> PathBuf {
        let d = home.join("sessions/2026/09/30");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("rollout-2026-09-30T01-00-00-{id}.jsonl"));
        let mut f = std::fs::File::create(&p).unwrap();
        for l in lines {
            let (ts, k, pl) = (&l["timestamp"], &l["type"], &l["payload"]);
            match l.get("ordinal") {
                Some(o) => writeln!(f, r#"{{"timestamp":{ts},"type":{k},"ordinal":{o},"payload":{pl}}}"#).unwrap(),
                None => writeln!(f, r#"{{"timestamp":{ts},"type":{k},"payload":{pl}}}"#).unwrap(),
            }
        }
        p
    }

    fn home(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-trace-cxh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn child_meta(id: &str, extra: Value) -> Value {
        let mut p = json!({"id": id, "cwd": "/p/app", "cli_version": "0.155.0", "agent_nickname": "Euclid", "agent_role": "explorer",
                           "source": {"subagent": {"thread_spawn": {"parent_thread_id": "th", "depth": 1, "agent_nickname": "Euclid", "agent_role": "explorer"}}}});
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        row(2150, "session_meta", p)
    }

    /// 父线程一轮里起两个子线程:一个新版本(起来时来 sub_agent_activity),一个老版本(id 在 spawn_agent 的输出里)
    pub(crate) fn family(name: &str, kid1_done: bool) -> (PathBuf, PathBuf) {
        let h = home(name);
        let spawn = |ms, id: &str, args: Value| {
            ri(ms, json!({"type": "function_call", "name": "spawn_agent", "call_id": id, "arguments": args.to_string()}))
        };
        let parent = home_file(
            &h,
            "th",
            &[
                meta("0.155.0"),
                ev(1000, json!({"type": "task_started", "turn_id": "t1"})),
                row(1010, "turn_context", json!({"turn_id": "t1", "cwd": "/p/app", "model": "gpt-5.5", "effort": "high"})),
                ev(
                    1030,
                    json!({"type": "item_completed", "started_at_ms": 0, "item": {"type": "UserMessage", "id": "u1", "content": [{"type": "text", "text": "派两个去查"}]}}),
                ),
                spawn(2000, "sp1", json!({"agent_type": "explorer", "message": "看看 b.rs 里的断言"})),
                ev(
                    2100,
                    json!({"type": "sub_agent_activity", "event_id": "e1", "occurred_at_ms": 1, "agent_thread_id": "kid1", "agent_path": "/root/a", "kind": "started"}),
                ),
                ri(2200, json!({"type": "function_call_output", "call_id": "sp1", "output": "{\"task_name\":\"/root/a\"}"})),
                spawn(2300, "sp2", json!({"message": "查 git log"})),
                ri(
                    2400,
                    json!({"type": "function_call_output", "call_id": "sp2", "output": "{\"agent_id\":\"kid2\",\"nickname\":\"Noether\"}"}),
                ),
                tokens(2500, 1000, 900, 800, 50),
                say(9000, "两个都派出去了", "final_answer"),
                tokens(9100, 2000, 1000, 900, 20),
                ev(9200, json!({"type": "task_complete", "turn_id": "t1", "duration_ms": 8200})),
            ],
        );
        // kid1:开头抄了父线程的第一轮(连那次的用量),到父线程里没有的 turn_id 为止
        let mut kid1 = vec![
            child_meta("kid1", json!({})),
            row(2150, "session_meta", json!({"id": "th", "cwd": "/p/app", "cli_version": "0.155.0", "source": "cli"})),
            ev(2150, json!({"type": "task_started", "turn_id": "t1"})),
            ev(
                2150,
                json!({"type": "item_completed", "started_at_ms": 0, "item": {"type": "UserMessage", "id": "u1", "content": [{"type": "text", "text": "派两个去查"}]}}),
            ),
            tokens(2150, 1000, 900, 800, 50),
            ev(2200, json!({"type": "task_started", "turn_id": "k1"})),
            row(2200, "turn_context", json!({"turn_id": "k1", "cwd": "/p/app", "model": "gpt-5.5", "effort": "medium"})),
            ev(2210, json!({"type": "user_message", "message": "看看 b.rs 里的断言", "images": [], "local_images": []})),
            ri(
                5000,
                json!({"type": "function_call", "name": "exec_command", "call_id": "f1", "arguments": "{\"cmd\":\"rg -n assert src/b.rs\"}"}),
            ),
        ];
        if kid1_done {
            kid1.extend([
                ri(
                    5500,
                    json!({"type": "function_call_output", "call_id": "f1", "output": "Exit code: 0\nOutput:\n8:    assert_eq!(x, 1);"}),
                ),
                tokens(5600, 1500, 400, 300, 70),
                say(7000, "b.rs 第 8 行:断言写反了。\n\n改成 2 就对了", "final_answer"),
                tokens(7100, 1900, 300, 200, 60),
                ev(7200, json!({"type": "task_complete", "turn_id": "k1", "duration_ms": 5000})),
            ]);
        }
        home_file(&h, "kid1", &kid1);
        // kid2(0.154+):行上有 ordinal,比 subagent_history_start_ordinal 小的是抄的
        let o = |mut v: Value, n: u64| {
            v["ordinal"] = json!(n);
            v
        };
        home_file(
            &h,
            "kid2",
            &[
                o(
                    child_meta("kid2", json!({"subagent_history_start_ordinal": 3, "agent_nickname": "Noether", "agent_role": "default"})),
                    0,
                ),
                o(ev(2450, json!({"type": "task_started", "turn_id": "t1"})), 1),
                o(tokens(2450, 1000, 900, 800, 50), 2),
                o(ev(2460, json!({"type": "task_started", "turn_id": "k2"})), 3),
                o(row(2465, "turn_context", json!({"turn_id": "k2", "cwd": "/p/app", "model": "gpt-5.5"})), 4),
                o(ev(2470, json!({"type": "user_message", "message": "查 git log", "images": [], "local_images": []})), 5),
                o(say(4000, "最近一次提交改了 b.rs", "final_answer"), 6),
                o(tokens(4100, 1300, 300, 0, 40), 7),
                o(ev(4200, json!({"type": "task_complete", "turn_id": "k2", "duration_ms": 1740})), 8),
            ],
        );
        (h, parent)
    }

    #[test]
    fn child_threads_hang_under_spawn_agent_and_the_parent_history_they_copied_is_not_counted() {
        let (_h, parent) = family("kids", true);
        let t = Trace::open_born(parent, Agent::Codex, 0).unwrap();
        let call = t.nodes[t.roots[0]].kids[0];
        let (sp1, sp2) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert_eq!((t.nodes[sp1].name.as_str(), t.nodes[sp1].label.as_str()), ("Agent", "explorer · 看看 b.rs 里的断言"));
        let k1 = t.sub_of(sp1).expect("kid1 按 sub_agent_activity 挂上");
        let k2 = t.sub_of(sp2).expect("kid2 按输出里的 agent_id 挂上");
        assert_eq!((k1.kind.as_str(), k1.desc.as_str(), k2.desc.as_str()), ("explorer", "Euclid", "Noether"));
        // 开头抄的父线程那一轮折成一块、不计;自己的那轮是第 1 轮,收到的是任务
        for k in [k1, k2] {
            let roots: Vec<Kind> = k.t.roots.iter().map(|&r| k.t.nodes[r].kind).collect();
            assert_eq!(roots, [Kind::Copied, Kind::Turn], "{}", k.id);
            assert_eq!(k.t.turn_no(k.t.roots[1]), Some(1));
        }
        assert_eq!(crate::trace::asked(&k1.t, k1.t.roots[1]), ("任务", "看看 b.rs 里的断言".to_string()));
        // 钱:子线程只算自己的用量(抄来的那次不算),算进起它的那一轮
        let cost = |m: &str, tok: [u64; 5]| crate::usage::call_cost(m, tok, false, false, 0).unwrap();
        let own = cost("gpt-5.5", [100, 0, 0, 300, 70]) + cost("gpt-5.5", [100, 0, 0, 200, 60]);
        assert!((k1.t.total(false, 0).2.cost - own).abs() < 1e-12);
        let kid2 = cost("gpt-5.5", [300, 0, 0, 0, 40]);
        assert!((k2.t.total(false, 0).2.cost - kid2).abs() < 1e-12, "{}", k2.t.total(false, 0).2.cost);
        let s = t.sum(t.roots[0]);
        assert_eq!((s.calls, s.subs), (2, 2));
        assert!((s.sub_cost - own - kid2).abs() < 1e-12);
        // 都干完了;右边写子线程的数字,露它最后说的
        assert!(!t.sub_live(sp1, true, ts_ms(&at(9000))));
        let plain = |ps: &[crate::trace::Para]| {
            crate::trace::wrap(ps, 80).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>()
        };
        let (ps, _) = crate::trace_page::tool_item(&t, sp1, false, 0, false);
        let text = plain(&ps);
        assert!(text[0].starts_with("· Agent explorer · 看看 b.rs 里的断言") && text[0].contains("2 调用"), "{text:#?}");
        assert_eq!(&text[1..], ["  b.rs 第 8 行:断言写反了。", "  改成 2 就对了"]);
    }

    #[test]
    fn a_child_still_working_is_live_and_its_file_is_found_even_when_it_shows_up_later() {
        let (h, parent) = family("kids-live", false);
        let kid2 = h.join("sessions/2026/09/30/rollout-2026-09-30T01-00-00-kid2.jsonl");
        let body = std::fs::read(&kid2).unwrap();
        std::fs::remove_file(&kid2).unwrap();
        let mut t = Trace::open_born(parent, Agent::Codex, 0).unwrap();
        let call = t.nodes[t.roots[0]].kids[0];
        let (sp1, sp2) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert!(t.sub_live(sp1, true, ts_ms(&at(9000))) && t.sub_of(sp2).is_none() && t.want.len() == 1);
        // 半小时没动静:会话被关掉时没干完的,不算在跑
        assert!(!t.sub_live(sp1, true, ts_ms(&at(5000)) + 31 * 60_000));
        std::fs::write(&kid2, body).unwrap();
        assert!(t.tail().unwrap());
        assert!(t.sub_of(sp2).is_some() && t.want.is_empty());
    }

    #[test]
    fn without_the_parent_record_the_copy_ends_where_a_turn_starts_before_the_last_one_ended() {
        // 分叉出来的(父线程的记录不在本机):抄来的最后一轮还没结束就又来了 task_started,那是它自己的
        let t = open(
            "fork",
            &[
                row(
                    0,
                    "session_meta",
                    json!({"id": "f", "cwd": "/p/app", "cli_version": "0.150.0", "forked_from_id": "gone", "source": "cli"}),
                ),
                ev(10, json!({"type": "task_started", "turn_id": "a"})),
                ev(10, json!({"type": "user_message", "message": "旧的一问", "images": [], "local_images": []})),
                ev(10, json!({"type": "task_complete", "turn_id": "a"})),
                ev(10, json!({"type": "task_started", "turn_id": "b"})),
                ev(10, json!({"type": "user_message", "message": "旧的第二问", "images": [], "local_images": []})),
                tokens(10, 5000, 4000, 3000, 100),
                ev(2000, json!({"type": "task_started", "turn_id": "c"})),
                ev(2010, json!({"type": "user_message", "message": "新的一问", "images": [], "local_images": []})),
                tokens(5000, 5500, 500, 0, 10),
                ev(5100, json!({"type": "task_complete", "turn_id": "c"})),
            ],
        );
        let roots: Vec<Kind> = t.roots.iter().map(|&r| t.nodes[r].kind).collect();
        assert_eq!(roots, [Kind::Copied, Kind::Turn]);
        assert_eq!(t.nodes[t.roots[0]].kids.len(), 2);
        assert_eq!(t.total(false, 0).2.calls, 1);
        // history_base:什么都没抄
        let t = open(
            "paged",
            &[
                row(
                    0,
                    "session_meta",
                    json!({"id": "g", "cwd": "/p/app", "cli_version": "0.154.0", "forked_from_id": "gone", "history_base": {"thread_id": "gone"}, "source": "cli"}),
                ),
                ev(10, json!({"type": "task_started", "turn_id": "a"})),
                tokens(1000, 100, 50, 0, 10),
            ],
        );
        assert_eq!(t.nodes[t.roots[0]].kind, Kind::Turn);
    }

    #[test]
    fn rolled_back_turns_are_greyed_and_unnumbered_but_their_money_stays() {
        let turn = |ms: u32, id: &str, ask: &str, total: u64| {
            vec![
                ev(ms, json!({"type": "task_started", "turn_id": id})),
                ev(ms + 10, json!({"type": "user_message", "message": ask, "images": [], "local_images": []})),
                say(ms + 500, "好", "final_answer"),
                tokens(ms + 600, total, 100, 0, 10),
                ev(ms + 700, json!({"type": "task_complete", "turn_id": id})),
            ]
        };
        let mut lines = vec![meta("0.150.0"), row(500, "turn_context", json!({"cwd": "/p/app", "model": "gpt-5.5"}))];
        lines.extend(turn(1000, "1", "一", 110));
        lines.extend(turn(2000, "2", "二", 220));
        lines.extend(turn(3000, "3", "三", 330));
        lines.push(ev(3900, json!({"type": "thread_rolled_back", "num_turns": 2})));
        lines.extend(turn(4000, "4", "改过的二", 440));
        let p = file("rollback", &lines);
        let t = Trace::open_born(p.clone(), Agent::Codex, 0).unwrap();
        let turns: Vec<usize> = t.roots.iter().copied().filter(|&r| t.nodes[r].kind == Kind::Turn).collect();
        let rolled: Vec<bool> = turns.iter().map(|&r| t.nodes[r].rolled).collect();
        assert_eq!(rolled, [false, true, true, false]);
        assert_eq!(turns.iter().map(|&r| t.turn_no(r)).collect::<Vec<_>>(), [Some(1), None, None, Some(2)]);
        assert_eq!(crate::trace::where_of(&t, turns[1]), "回退掉的");
        let (n, _, s) = t.total(false, 0);
        let bill: f64 = crate::usage::tests::codex_seen(&p)
            .into_iter()
            .map(|(m, tok)| crate::usage::call_cost(&m, tok, false, false, 0).unwrap())
            .sum();
        assert_eq!((n, s.calls), (2, 4));
        assert!((s.cost - bill).abs() < 1e-12);
        let c = crate::trace::chat(&t, turns[1], false, 0);
        assert!(crate::ansi::plain(&c.foot.body).starts_with("↶ 回退掉了 · "));
    }

    #[test]
    fn an_old_format_without_outer_types_is_refused() {
        let t = open(
            "old",
            &[json!({"id": "x", "timestamp": "2025-01-01T00:00:00Z", "instructions": ""}), json!({"type": "message", "role": "user"})],
        );
        assert!(t.cx.old && t.roots.is_empty());
    }
}
