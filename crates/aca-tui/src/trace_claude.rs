//! Claude 的对话记录 → `trace` 的树。规则是 2026-09 在本机 440 份记录(2.1.141–2.1.285)上实测的:
//! - 一轮:`user` 行,`origin.kind` 是 human / task-notification / peer / coordinator,内容里没有 tool_result
//!   (老记录没有 origin,按内容认);`system/turn_duration` 的 durationMs 是这一轮的耗时。
//!   没人回应就被重打的提问(没有后代)丢掉,只留后面那句
//! - 一次调用:同一个 message.id 的几行(一行一个内容块),usage / stop_reason 取最后一行(输出数只增不减);
//!   从第一块的上一行(工具结果 / 提问)算起,到最后一块为止
//! - 工具:tool_use.id ↔ tool_result.tool_use_id,挂在发起它的那次调用下(按 message.id 分,不顺 parentUuid:
//!   并行工具、流式块会让它分叉);钩子的耗时在 hook_success 附件里
//! - model 是 `<synthetic>` 且 isApiErrorMessage 的是 API 出错,不算调用
//! - resume / 继续时新文件开头抄了旧文件的尾巴(uuid、时间戳照旧):文件开头连着的、比文件创建早 2 秒以上的对话行。
//!   不能看 session_id ≠ sessionId —— session_id 是写这一行的进程的会话 id,resume 之后接着写的新行也不一样
//!   (本机 13 份记录 5,704 行);开头那一两行会话启动的钩子附件也会早几秒,附件不算
//! - 压缩:`system/compact_boundary`,只画一行(压缩那次调用本身不写进记录)
//! - 子代理:`<会话 id>/subagents/agent-<id>.jsonl`,旁边的 `.meta.json` 写着类型、说明、是哪个 Agent 工具起的(toolUseId),
//!   嵌套的还有 parentAgentId(那个工具在上一层子代理的记录里)。本机 175 个全挂得回去。
//!   现在的 Agent 多是后台跑:工具马上返回,跑完了在父会话里来一条 task-notification(task-id = agentId,带 status)。
//!   fork 出来的子代理第一行是 fork-context-ref(接着父对话,父对话的行不抄进来)
//!
//! 大字段(内容块、工具结果、附件)用 RawValue 跳过,不展开 base64(最长的一行有好几 MB)。

use std::hash::{DefaultHasher, Hash, Hasher};

use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

use aca_core::{t, tr};

use crate::data::Agent;
use crate::trace::{Asker, Change, Io, Kind, LABEL, Node, St, Sub, Trace, first_line, fmt_dur, fmt_tok, short_path, ts_ms, unmark};

#[derive(Deserialize)]
struct Rec<'a> {
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    parent: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "isMeta")]
    meta: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    compact_summary: Option<bool>,
    #[serde(rename = "isApiErrorMessage")]
    api_error: Option<bool>,
    #[serde(borrow)]
    error: Option<&'a RawValue>,
    #[serde(rename = "apiErrorStatus", borrow)]
    api_status: Option<&'a RawValue>,
    origin: Option<Origin>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(rename = "toolUseResult", borrow)]
    result: Option<&'a RawValue>,
    #[serde(rename = "toolDenialKind")]
    denial: Option<String>,
    #[serde(rename = "durationMs")]
    duration: Option<f64>,
    #[serde(rename = "compactMetadata")]
    compact: Option<Compact>,
    #[serde(borrow)]
    attachment: Option<&'a RawValue>,
    effort: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct Origin {
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Compact {
    #[serde(rename = "preTokens")]
    pre: Option<u64>,
    #[serde(rename = "postTokens")]
    post: Option<u64>,
    #[serde(rename = "durationMs")]
    dur: Option<u64>,
}

#[derive(Deserialize)]
struct Msg<'a> {
    id: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    tool_use_id: Option<String>,
    is_error: Option<bool>,
    thinking: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_creation: Option<Cache>,
    speed: Option<String>,
    inference_geo: Option<String>,
    server_tool_use: Option<Server>,
    output_tokens_details: Option<OutDetails>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Cache {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Server {
    web_search_requests: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OutDetails {
    thinking_tokens: Option<u64>,
}

/// 工具输入里摘要要用的几个字段(别的,比如 Write 的整份内容,跳过不读)
#[derive(Deserialize, Default)]
#[serde(default)]
struct Input {
    command: Option<String>,
    description: Option<String>,
    file_path: Option<String>,
    notebook_path: Option<String>,
    path: Option<String>,
    pattern: Option<String>,
    url: Option<String>,
    query: Option<String>,
    subagent_type: Option<String>,
    skill: Option<String>,
    prompt: Option<String>,
}

#[derive(Deserialize)]
struct Attachment {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "toolUseID")]
    tool: Option<String>,
    #[serde(rename = "durationMs")]
    dur: Option<f64>,
}

fn key(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// <name>…</name> 里的字
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&format!("</{name}>"))? + start;
    Some(s[start..end].trim())
}

/// 一行记录
pub(crate) fn line(t: &mut Trace, buf: &[u8], off: u64) {
    let Ok(r) = serde_json::from_slice::<Rec>(buf) else { return };
    let ts = r.timestamp.as_deref().map_or(0, ts_ms);
    if let Some(u) = &r.uuid {
        t.at.insert(key(u), ts);
    }
    let talk = matches!(r.kind.as_deref(), Some("user" | "assistant" | "system"));
    if talk && t.lead && (t.born == 0 || ts >= t.born - 2000) {
        t.lead = false;
    }
    let copied = talk && t.lead;
    if !copied && let Some(c) = r.cwd.as_ref().filter(|c| !c.is_empty()) {
        t.cwd.clone_from(c);
    }
    let src = (off, buf.len() as u32);
    match r.kind.as_deref() {
        Some("user") => user(t, &r, ts, src, copied),
        Some("assistant") => assistant(t, &r, ts, src, copied),
        Some("system") => system(t, &r, ts, copied),
        Some("attachment") => attachment(t, &r),
        // fork 出来的子代理:接着父对话往下干(父对话的行没抄进来)
        Some("fork-context-ref") => {
            let i = t.add_top(Kind::Mark, ts, false);
            t.nodes[i].label = t!("接着父对话", "continues the parent conversation").into();
            t.nodes[i].t1 = ts;
        }
        _ => {}
    }
}

fn user(t: &mut Trace, r: &Rec, ts: i64, src: (u64, u32), copied: bool) {
    if r.compact_summary == Some(true) {
        return;
    }
    let Some(m) = r.message.and_then(|m| serde_json::from_str::<Msg>(m.get()).ok()) else { return };
    let Some(c) = m.content else { return };
    let raw = c.get().trim_start();
    let text = if raw.starts_with('[') {
        let Ok(blocks) = serde_json::from_str::<Vec<Block>>(raw) else { return };
        let mut text = String::new();
        let mut results = false;
        for b in &blocks {
            match b.kind.as_str() {
                "tool_result" => {
                    results = true;
                    result(t, r, b, ts, src);
                }
                "text" => {
                    text.push_str(b.text.as_deref().unwrap_or(""));
                    text.push('\n');
                }
                _ => {}
            }
        }
        if results {
            return;
        }
        text
    } else {
        serde_json::from_str::<String>(raw).unwrap_or_default()
    };
    prompt(t, r, &text, ts, src, copied);
}

/// 工具的结果回来了
fn result(t: &mut Trace, r: &Rec, b: &Block, ts: i64, src: (u64, u32)) {
    let Some(&i) = b.tool_use_id.as_ref().and_then(|id| t.tools.get(id)) else { return };
    let interrupted = r.result.is_some_and(|x| x.get().contains("\"interrupted\":true"));
    let n = &mut t.nodes[i];
    n.t1 = ts.max(n.t0);
    n.state = match () {
        _ if r.denial.is_some() => St::Denied,
        _ if interrupted => St::Stopped,
        _ if b.is_error == Some(true) => St::Err,
        _ => St::Ok,
    };
    n.src.push(src);
}

/// 用户这边的一句话:新的一轮,或者命令、打断
fn prompt(t: &mut Trace, r: &Rec, text: &str, ts: i64, src: (u64, u32), copied: bool) {
    if r.meta == Some(true) {
        return;
    }
    let text = text.trim();
    if text.starts_with("[Request interrupted by user") {
        if let Some(cur) = t.turn {
            t.nodes[cur].state = St::Stopped;
            t.stop_open_tools(cur, ts);
        }
        return;
    }
    // 命令的输出、说明:不是一轮
    if ["<local-command-", "<bash-stdout>", "<bash-stderr>"].iter().any(|p| text.starts_with(p)) {
        return;
    }
    let (name, label) = if let Some(cmd) = tag(text, "command-name") {
        let args = tag(text, "command-args").unwrap_or("");
        (cmd.to_string(), first_line(&format!("{cmd} {args}"), LABEL))
    } else if let Some(cmd) = tag(text, "bash-input") {
        ("!".to_string(), first_line(&format!("! {cmd}"), LABEL))
    } else if text.contains("<task-notification>") {
        // 后台的子代理 / 命令跑完了:记下是哪个、怎么结束的
        if let Some(id) = tag(text, "task-id") {
            let st = match tag(text, "status") {
                Some("failed") => St::Err,
                Some("killed" | "stopped") => St::Stopped,
                _ => St::Ok,
            };
            t.done.insert(id.to_string(), st);
        }
        let what = tag(text, "summary").map(|s| first_line(s, LABEL)).unwrap_or_default();
        (String::new(), if what.is_empty() { t!("↩ 后台任务", "↩ background task").into() } else { format!("↩ {what}") })
    } else {
        let kind = r.origin.as_ref().and_then(|o| o.kind.as_deref());
        if !matches!(kind, None | Some("human" | "task-notification" | "peer" | "coordinator")) || text.is_empty() {
            return;
        }
        (String::new(), first_line(text, LABEL))
    };
    drop_abandoned(t);
    let i = t.add_top(Kind::Turn, ts, copied);
    let n = &mut t.nodes[i];
    n.name = name;
    n.label = label;
    n.src.push(src);
    t.turn = Some(i);
}

/// 上一句没人回应(没有调用、没结束)就又来了一句:那是重打的,丢掉上一句
fn drop_abandoned(t: &mut Trace) {
    let Some(cur) = t.turn else { return };
    let n = &t.nodes[cur];
    if n.kind != Kind::Turn || !n.kids.is_empty() || n.t1 != 0 || n.state != St::Ok || !n.name.is_empty() {
        return;
    }
    match n.parent {
        Some(p) => t.nodes[p].kids.retain(|&k| k != cur),
        None => t.roots.retain(|&k| k != cur),
    }
    t.turn = None;
}

/// 现在这一轮;没有(记录从半截开始)就补一个
fn cur_turn(t: &mut Trace, ts: i64, copied: bool) -> usize {
    match t.turn {
        Some(x) if t.nodes[x].copied == copied => x,
        _ => {
            let i = t.add_top(Kind::Turn, ts, copied);
            t.nodes[i].label = "…".into();
            t.turn = Some(i);
            i
        }
    }
}

fn assistant(t: &mut Trace, r: &Rec, ts: i64, src: (u64, u32), copied: bool) {
    let Some(m) = r.message.and_then(|m| serde_json::from_str::<Msg>(m.get()).ok()) else { return };
    let model = m.model.clone().unwrap_or_default();
    if model == "<synthetic>" {
        if r.api_error == Some(true) {
            let turn = cur_turn(t, ts, copied);
            let i = t.add(Kind::Fail, Some(turn), ts, copied);
            let status = r.api_status.map(|s| s.get().trim_matches('"').to_string()).filter(|s| s != "null");
            let kind = r.error.and_then(|e| serde_json::from_str::<String>(e.get()).ok());
            t.nodes[i].label = [status, kind].into_iter().flatten().collect::<Vec<_>>().join(" ");
            t.nodes[i].t1 = ts;
            t.nodes[i].src.push(src);
            t.nodes[turn].state = St::Err;
        }
        return;
    }
    let turn = cur_turn(t, ts, copied);
    let id = m.id.clone().unwrap_or_default();
    let call = match t.calls.get(&id) {
        Some(&c) => c,
        None => {
            let t0 = r.parent.as_deref().and_then(|p| t.at.get(&key(p)).copied()).filter(|&x| x > 0 && x <= ts).unwrap_or(ts);
            let c = t.add(Kind::Call, Some(turn), t0, copied);
            let mi = t.model_idx(&model);
            let n = &mut t.nodes[c];
            n.id.clone_from(&id);
            n.model = mi;
            t.calls.insert(id, c);
            // 前面出过错、这次成了:是重试成功,这一轮不算出错
            if t.nodes[turn].state == St::Err {
                t.nodes[turn].state = St::Ok;
            }
            c
        }
    };
    {
        let n = &mut t.nodes[call];
        n.t1 = ts.max(n.t0);
        n.src.push(src);
        if let Some(e) = &r.effort {
            n.effort.clone_from(e);
        }
        if let Some(s) = &m.stop_reason {
            n.stop.clone_from(s);
        }
        if let Some(u) = &m.usage {
            // 老记录没有分 5 分钟 / 1 小时,算 5 分钟的(同 usage.rs)
            let (w5, w1) = match &u.cache_creation {
                Some(c) if c.ephemeral_5m_input_tokens.unwrap_or(0) + c.ephemeral_1h_input_tokens.unwrap_or(0) > 0 => {
                    (c.ephemeral_5m_input_tokens.unwrap_or(0), c.ephemeral_1h_input_tokens.unwrap_or(0))
                }
                _ => (u.cache_creation_input_tokens.unwrap_or(0), 0),
            };
            let tok = [u.input_tokens.unwrap_or(0), w5, w1, u.cache_read_input_tokens.unwrap_or(0), u.output_tokens.unwrap_or(0)];
            if tok[4] >= n.tok[4] {
                n.tok = tok;
                n.think = u.output_tokens_details.as_ref().and_then(|d| d.thinking_tokens).unwrap_or(0);
                n.searches = u.server_tool_use.as_ref().and_then(|s| s.web_search_requests).unwrap_or(0);
                n.fast = u.speed.as_deref() == Some("fast");
                n.us = u.inference_geo.as_deref() == Some("us");
            }
        }
    }
    let Some(c) = m.content else { return };
    let Ok(blocks) = serde_json::from_str::<Vec<Block>>(c.get()) else { return };
    for b in blocks {
        match b.kind.as_str() {
            "text" if t.nodes[call].label.is_empty() => t.nodes[call].label = first_line(&unmark(b.text.as_deref().unwrap_or("")), LABEL),
            "thinking" if t.nodes[call].thought.is_empty() => {
                t.nodes[call].thought = first_line(b.thinking.as_deref().unwrap_or(""), LABEL)
            }
            "tool_use" => {
                let name = b.name.clone().unwrap_or_default();
                let inp: Input = b.input.and_then(|x| serde_json::from_str(x.get()).ok()).unwrap_or_default();
                let label = tool_label(&name, &inp, &t.cwd);
                let i = t.add(Kind::Tool, Some(call), ts, copied);
                let n = &mut t.nodes[i];
                n.name = name;
                n.label = label;
                n.id = b.id.clone().unwrap_or_default();
                n.src.push(src);
                if let Some(id) = b.id {
                    t.tools.insert(id, i);
                }
            }
            _ => {}
        }
    }
}

fn system(t: &mut Trace, r: &Rec, ts: i64, copied: bool) {
    match r.subtype.as_deref() {
        Some("turn_duration") => {
            if let (Some(cur), Some(d)) = (t.turn, r.duration) {
                let n = &mut t.nodes[cur];
                n.t1 = n.t0 + d as i64;
                n.exact = true;
            }
        }
        Some("compact_boundary") => {
            let i = t.add_top(Kind::Mark, ts, copied);
            let c = r.compact.as_ref();
            let mut label = t!("压缩", "compaction").to_string();
            if let Some((a, b)) = c.and_then(|c| Some((c.pre?, c.post?))) {
                label += &format!(" {} → {}", fmt_tok(a), fmt_tok(b));
            }
            if let Some(d) = c.and_then(|c| c.dur) {
                label += &format!(" · {}", fmt_dur(d as i64));
            }
            t.nodes[i].label = label;
            t.nodes[i].t1 = ts;
        }
        _ => {}
    }
}

fn attachment(t: &mut Trace, r: &Rec) {
    let Some(a) = r.attachment.and_then(|a| serde_json::from_str::<Attachment>(a.get()).ok()) else { return };
    if !matches!(a.kind.as_deref(), Some("hook_success" | "hook_non_blocking_error")) {
        return;
    }
    if let (Some(&i), Some(d)) = (a.tool.as_ref().and_then(|id| t.tools.get(id)), a.dur) {
        t.nodes[i].hook += d as u64;
    }
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "agentType")]
    kind: Option<String>,
    description: Option<String>,
    #[serde(rename = "toolUseId")]
    tool: Option<String>,
    #[serde(rename = "parentAgentId")]
    parent: Option<String>,
}

/// 找新的子代理,挂到起它的那个 Agent 工具下(最上面那份记录找;嵌套的挂进上一层子代理的那份)。
/// 起它的工具还没读到、记录还没写出来的,下次再找。有新挂上的返回 true
pub(crate) fn find_subs(t: &mut Trace) -> bool {
    if t.is_sub {
        return false;
    }
    let dir = t.path.with_extension("").join("subagents");
    let Ok(rd) = std::fs::read_dir(&dir) else { return false };
    let mut metas = vec![];
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_prefix("agent-")).and_then(|n| n.strip_suffix(".meta.json")) else { continue };
        if t.sub_ids.contains(id) {
            continue;
        }
        if let Some(m) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Meta>(&b).ok()) {
            metas.push((id.to_string(), m));
        }
    }
    let mut grew = false;
    // 上一层的先挂上,嵌套的才找得到它
    loop {
        let before = metas.len();
        metas.retain(|(id, m)| !attach(t, id, m, &dir));
        if metas.len() == before {
            break;
        }
        grew = true;
    }
    grew
}

/// 一个子代理挂上去:找到起它的工具(在最上面那份,或者 parentAgentId 那个子代理的记录里),读它的记录
fn attach(root: &mut Trace, id: &str, m: &Meta, dir: &std::path::Path) -> bool {
    let path = match &m.parent {
        Some(p) => match sub_path(root, p) {
            Some(x) => x,
            None => return false,
        },
        None => vec![],
    };
    let owner = at_mut(root, &path);
    let Some(&tool) = m.tool.as_ref().and_then(|x| owner.tools.get(x)) else { return false };
    let Ok(sub) = Trace::open_sub(dir.join(format!("agent-{id}.jsonl")), Agent::Claude, None) else { return false };
    owner.nodes[tool].sub = Some(owner.subs.len());
    owner.subs.push(Sub {
        id: id.to_string(),
        kind: m.kind.clone().unwrap_or_default(),
        desc: m.description.clone().unwrap_or_default(),
        t: sub,
    });
    root.sub_ids.insert(id.to_string());
    true
}

/// 某个子代理在第几层:一路上起它的那些工具
fn sub_path(t: &Trace, id: &str) -> Option<Vec<usize>> {
    for (i, n) in t.nodes.iter().enumerate() {
        let Some(s) = n.sub.and_then(|k| t.subs.get(k)) else { continue };
        if s.id == id {
            return Some(vec![i]);
        }
        if let Some(mut rest) = sub_path(&s.t, id) {
            rest.insert(0, i);
            return Some(rest);
        }
    }
    None
}

fn at_mut<'a>(t: &'a mut Trace, path: &[usize]) -> &'a mut Trace {
    match path.split_first() {
        Some((&i, rest)) => match t.nodes[i].sub {
            Some(k) => at_mut(&mut t.subs[k].t, rest),
            None => t,
        },
        None => t,
    }
}

/// 工具的一句话摘要:Bash 写命令、读写文件写路径、搜索写词、MCP 写「服务 · 工具」
fn tool_label(name: &str, i: &Input, cwd: &str) -> String {
    let path = || i.file_path.as_deref().or(i.notebook_path.as_deref()).or(i.path.as_deref()).map(|p| short_path(p, cwd));
    let s = match name {
        "Bash" => i.command.as_deref().map(|c| first_line(c, LABEL)),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => path(),
        "Grep" | "Glob" => i.pattern.clone(),
        "WebFetch" => i.url.as_deref().map(|u| u.split("://").nth(1).unwrap_or(u).split('/').next().unwrap_or(u).to_string()),
        "WebSearch" => i.query.clone(),
        "Agent" | "Task" => {
            Some([i.subagent_type.as_deref(), i.description.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · "))
        }
        "Skill" => i.skill.clone(),
        n if n.starts_with("mcp__") => {
            let mut parts = n.splitn(3, "__").skip(1);
            Some([parts.next(), parts.next()].into_iter().flatten().collect::<Vec<_>>().join(" · "))
        }
        _ => i
            .description
            .clone()
            .or(i.command.clone())
            .or(i.query.clone())
            .or_else(path)
            .or(i.prompt.as_deref().map(|p| first_line(p, LABEL))),
    };
    s.map(|x| first_line(&x, LABEL)).unwrap_or_default()
}

// ---------- 读回来的内容(排成字在 trace_page) ----------

/// 内容块:字符串或者块的数组
fn blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) => vec![serde_json::json!({ "type": "text", "text": s })],
        Value::Array(v) => v.clone(),
        _ => vec![],
    }
}

/// 块里的字;图片写成 [图片]
fn text_of(content: &Value) -> String {
    let mut out = vec![];
    for b in blocks(content) {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => out.push(b.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
            Some("image") => out.push(t!("[图片]", "[image]").into()),
            Some("tool_reference") => out.push(format!("[{}]", b.get("tool_name").and_then(Value::as_str).unwrap_or(t!("工具", "tool")))),
            Some(k) => out.push(format!("[{k}]")),
            None => {}
        }
    }
    out.join("\n")
}

fn content(v: &Value) -> Value {
    v.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(Value::Null)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// 读回这个节点记下的几行
fn lines_of(t: &Trace, n: &Node) -> Vec<Value> {
    n.src.iter().filter_map(|&x| t.read_at(x)).collect()
}

/// 一次调用想的、说的(一块一段)
pub(crate) fn said(t: &Trace, n: &Node) -> (Vec<String>, Vec<String>) {
    let (mut thought, mut text) = (vec![], vec![]);
    for v in lines_of(t, n) {
        for b in blocks(&content(&v)) {
            let get = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
            match b.get("type").and_then(Value::as_str) {
                Some("text") => text.push(get("text")),
                Some("thinking") => thought.push(get("thinking")),
                _ => {}
            }
        }
    }
    thought.retain(|s| !s.is_empty());
    text.retain(|s| !s.is_empty());
    (thought, text)
}

/// 谁问的、问了什么:提问是原文,命令、后台任务的通知写摘要。记录从一轮中间开始的(补出来的那轮)谁也不是
pub(crate) fn asked(t: &Trace, n: &Node) -> (Asker, String) {
    if let Some(what) = n.label.strip_prefix('↩') {
        return (Asker::Notice, what.trim().to_string());
    }
    if !n.name.is_empty() {
        return (Asker::You, n.label.clone());
    }
    match n.src.first().and_then(|&x| t.read_at(x)) {
        Some(v) => (Asker::You, text_of(&content(&v)).trim().to_string()),
        None => (Asker::Nobody, t!("记录从这一轮中间开始", "the record starts mid-turn").into()),
    }
}

/// API 出错那条假回复里写的
pub(crate) fn fail_text(t: &Trace, n: &Node) -> String {
    n.src.first().and_then(|&x| t.read_at(x)).map(|v| text_of(&content(&v))).unwrap_or_default()
}

/// 缓存为什么没命中(记录里写了才有)
pub(crate) fn call_note(t: &Trace, n: &Node) -> Option<String> {
    let last = t.read_at(*n.src.last()?)?;
    let why = last.pointer("/message/diagnostics/cache_miss_reason/type")?.as_str()?;
    Some(tr!("缓存没命中:{why}", "cache miss: {why}"))
}

/// 工具发起时的输入
fn tool_input(t: &Trace, n: &Node) -> Option<Value> {
    let v = t.read_at(*n.src.first()?)?;
    blocks(&content(&v)).into_iter().find(|b| b.get("id").and_then(Value::as_str) == Some(n.id.as_str()))?.get("input").cloned()
}

/// 工具的结果那一整行(toolUseResult 里有改动)
fn tool_result(t: &Trace, n: &Node) -> Option<Value> {
    t.read_at(*n.src.get(1)?)
}

/// 结果那行里给这个工具的字
fn result_text(v: &Value, id: &str) -> Option<String> {
    let b = blocks(&content(v)).into_iter().find(|b| b.get("tool_use_id").and_then(Value::as_str) == Some(id))?;
    Some(text_of(b.get("content").unwrap_or(&Value::Null)))
}

/// 改文件的工具:底下露改动,不露输出(输出只是「改好了」)
const FILE_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// 一个工具的内容:Bash 是命令,改文件的是改动,Read 是读到的文件内容,别的是整理过的 JSON 输入;都有输出
pub(crate) fn io(t: &Trace, n: &Node) -> Io {
    let inp = tool_input(t, n);
    let res = tool_result(t, n);
    let out = res.as_ref().and_then(|v| result_text(v, &n.id));
    let s = |k: &str| inp.as_ref().and_then(|v| v.get(k)).and_then(Value::as_str);
    let path = s("file_path").or_else(|| s("notebook_path")).unwrap_or("").to_string();
    let mut io = Io::default();
    if FILE_TOOLS.contains(&n.name.as_str()) {
        let ch = changes(&n.name, inp.as_ref(), res.as_ref());
        // e 停在改完的第一行
        let line = ch.iter().find(|c| c.c == '+').or_else(|| ch.iter().find(|c| c.c == '-')).and_then(|c| c.no).unwrap_or(1);
        io.file = Some((path.clone(), line));
        io.edits = vec![(path.clone(), ch)];
    } else if n.name == "Bash" {
        io.cmds = s("command").map(|c| vec![c.trim().to_string()]).unwrap_or_default();
        io.note = s("description").map(String::from);
    } else if let Some(v) = &inp {
        io.input = Some(("json", pretty(v)));
    }
    if n.name == "Read" {
        io.file = Some((path.clone(), inp.as_ref().and_then(|v| v.get("offset")).and_then(Value::as_u64).unwrap_or(1)));
        let rows = out.as_deref().filter(|_| n.state == St::Ok).map(numbered).unwrap_or_default();
        if !rows.is_empty() {
            io.read = Some((path, rows));
        }
    }
    io.out = out;
    io
}

/// Edit / Write 改了什么。结果里有 structuredPatch 就用它(带上下文、行号);
/// 没有的(新建的文件、被拒的、还没结果的)拿输入比,头尾相同的行不列
pub(crate) fn changes(name: &str, inp: Option<&Value>, res: Option<&Value>) -> Vec<Change> {
    let ch = |c, no, text: &str| Change { c, no, text: text.to_string() };
    let hunks = res.and_then(|r| r.pointer("/toolUseResult/structuredPatch")).and_then(Value::as_array).filter(|h| !h.is_empty());
    if let Some(hunks) = hunks {
        let mut out = vec![];
        for h in hunks {
            if !out.is_empty() {
                out.push(ch('~', None, ""));
            }
            let at = |k: &str| h.get(k).and_then(Value::as_u64).unwrap_or(1);
            let lines = h.get("lines").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
            out.extend(hunk(at("oldStart"), at("newStart"), lines));
        }
        return out;
    }
    let Some(inp) = inp else { return vec![] };
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match name {
        "Write" => s(inp, "content").lines().zip(1..).map(|(l, k)| ch('+', Some(k), l)).collect(),
        "NotebookEdit" => s(inp, "new_source").lines().map(|l| ch('+', None, l)).collect(),
        "Edit" => pair(&s(inp, "old_string"), &s(inp, "new_string")),
        "MultiEdit" => inp
            .get("edits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|e| pair(&s(e, "old_string"), &s(e, "new_string")))
            .collect(),
        _ => vec![],
    }
}

/// 一处改动的几行(" 上下文" / "-删的" / "+加的"),从原来的第 old 行、改完的第 new 行起
pub(crate) fn hunk<'a>(mut old: u64, mut new: u64, lines: impl Iterator<Item = &'a str>) -> Vec<Change> {
    let mut out = vec![];
    for l in lines {
        let mut cs = l.chars();
        match cs.next() {
            Some('-') => {
                out.push(Change { c: '-', no: Some(old), text: cs.as_str().to_string() });
                old += 1;
            }
            Some('+') => {
                out.push(Change { c: '+', no: Some(new), text: cs.as_str().to_string() });
                new += 1;
            }
            // 「\ No newline at end of file」
            Some('\\') => {}
            _ => {
                out.push(Change { c: ' ', no: Some(new), text: cs.as_str().to_string() });
                old += 1;
                new += 1;
            }
        }
    }
    out
}

/// 新旧两段:头尾相同的行不列(不知道在第几行)
fn pair(old: &str, new: &str) -> Vec<Change> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let line = |c: char| move |l: &&str| Change { c, no: None, text: l.to_string() };
    a[head..a.len() - tail].iter().map(line('-')).chain(b[head..b.len() - tail].iter().map(line('+'))).collect()
}

/// Read 的结果是「行号<Tab>内容」:拆成 (行号, 内容);后面跟的提示不是文件内容,不要
fn numbered(text: &str) -> Vec<(u64, String)> {
    let mut out = vec![];
    for l in text.lines() {
        let t = l.trim_start();
        let Some((no, rest)) = t.split_once('\t').or_else(|| t.split_once('→')) else { break };
        let Ok(no) = no.parse() else { break };
        out.push((no, rest.to_string()));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ansi::Styled;
    use crate::data::Agent;
    use crate::theme;
    use crate::trace::{Step, View};
    use crate::trace_page::{fenced, flow, step, steps, tool_item, view};
    use serde_json::json;
    use std::io::Write;
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    /// 测试用的记录文件(每个测试一份)
    pub(crate) fn file(name: &str, lines: &[Value]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-trace-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s1.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        p
    }

    pub(crate) fn at(s: u32) -> String {
        format!("2026-09-30T01:00:{:02}.{:03}Z", s / 1000, s % 1000)
    }

    pub(crate) fn prompt(uuid: &str, parent: Option<&str>, ms: u32, text: &str) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "session_id": "s1",
               "origin": {"kind": "human"}, "promptId": "p", "message": {"role": "user", "content": text}})
    }

    pub(crate) fn reply(uuid: &str, parent: &str, ms: u32, id: &str, block: Value, out: u64, stop: Option<&str>) -> Value {
        json!({"type": "assistant", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "session_id": "s1",
               "requestId": format!("req-{id}"),
               "message": {"id": id, "model": "claude-opus-5-5", "role": "assistant", "content": [block], "stop_reason": stop,
                           "usage": {"input_tokens": 10, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 1000, "output_tokens": out,
                                     "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 100}}}})
    }

    pub(crate) fn bash(id: &str, cmd: &str) -> Value {
        json!({"type": "tool_use", "id": id, "name": "Bash", "input": {"command": cmd, "description": "run it"}})
    }

    pub(crate) fn done(uuid: &str, parent: &str, ms: u32, tool: &str, err: bool) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "session_id": "s1", "promptId": "p",
               "toolUseResult": {"stdout": "ok", "stderr": "", "interrupted": false},
               "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool, "content": "ok", "is_error": err}]}})
    }

    pub(crate) fn turn_end(parent: &str, ms: u32, dur: u64) -> Value {
        json!({"type": "system", "subtype": "turn_duration", "uuid": format!("td-{ms}"), "parentUuid": parent, "timestamp": at(ms),
               "sessionId": "s1", "durationMs": dur})
    }

    /// born = 0:不找抄过来的(测试记录的时间戳都比文件早)
    fn open(name: &str, lines: &[Value]) -> Trace {
        Trace::open_born(file(name, lines), Agent::Claude, 0).unwrap()
    }

    /// 界面测试用的一段:抄过来的一轮、正常的一轮(工具 + 结束)、压缩、API 出错的一轮、还在跑的一轮
    pub(crate) fn fixture(name: &str) -> (Trace, PathBuf) {
        let lines = vec![
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb", "parentUuid": null, "timestamp": at(0), "sessionId": "s1",
                   "compactMetadata": {"trigger": "manual", "preTokens": 182_000, "postTokens": 21_000, "durationMs": 38_000}}),
            prompt("o1", Some("cb"), 100, "以前问过的"),
            reply("o2", "o1", 1000, "m0", json!({"type": "text", "text": "好"}), 50, Some("end_turn")),
            {
                let mut u = prompt("u1", Some("o2"), 5000, "看看两栏为什么对不齐");
                u["cwd"] = json!("/p/app");
                u
            },
            reply("b1", "u1", 7000, "n1", json!({"type": "thinking", "thinking": "先确认是不是宽字符的问题。\n中文一个字占两格"}), 5, None),
            reply("b1b", "b1", 8000, "n1", json!({"type": "text", "text": "先看 bill.rs 的列宽"}), 5, None),
            reply(
                "b2",
                "b1b",
                9000,
                "n1",
                json!({"type": "tool_use", "id": "e0", "name": "Grep", "input": {"pattern": "width(", "path": "/p/app/src"}}),
                640,
                Some("tool_use"),
            ),
            json!({"type": "user", "uuid": "q0", "parentUuid": "b2", "timestamp": at(9200), "sessionId": "s1",
                   "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "e0",
                               "content": "src/bill.rs:12:    let w = s.len();\nsrc/bill.rs:40:    pad(w)\nsrc/ui.rs:88:    s.width()\nsrc/ui.rs:90:    clip(s, w)"}]}}),
            reply(
                "b3",
                "q0",
                11_000,
                "n2",
                json!({"type": "tool_use", "id": "e1", "name": "Edit",
                   "input": {"file_path": "/p/app/src/bill.rs", "old_string": "let w = s.len();", "new_string": "let w = s.width();"}}),
                200,
                Some("tool_use"),
            ),
            json!({"type": "user", "uuid": "q1", "parentUuid": "b3", "timestamp": at(11_100), "sessionId": "s1",
                   "toolUseResult": {"filePath": "/p/app/src/bill.rs", "structuredPatch": [{"oldStart": 11, "oldLines": 3, "newStart": 11, "newLines": 3,
                                     "lines": [" fn col(s: &str) -> usize {", "-    let w = s.len();", "+    let w = s.width();", "     w"]},
                                    {"oldStart": 40, "oldLines": 3, "newStart": 40, "newLines": 3,
                                     "lines": [" fn pad(w: usize) {", "-    pad(w)", "+    pad(w.max(1))", " }"]}]},
                   "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "e1", "content": "The file has been updated."}]}}),
            reply("b4", "q1", 13_000, "n3", bash("e2", "cargo test --release"), 90, Some("tool_use")),
            json!({"type": "user", "uuid": "q2", "parentUuid": "b4", "timestamp": at(21_800), "sessionId": "s1",
                   "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "e2",
                               "content": "running 220 tests\ntest bill::cols ... ok\ntest ui::wide ... ok\ntest ui::narrow ... ok\n\ntest result: ok. 220 passed"}]}}),
            reply(
                "b5",
                "q2",
                25_000,
                "n4",
                json!({"type": "text", "text": "改好了:**len()** 数的是字节,一个中文占 3 字节。\n\n- 换成按显示宽度算的 `width()`\n- 测试全过了"}),
                410,
                Some("end_turn"),
            ),
            turn_end("b5", 25_100, 20_000),
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb2", "parentUuid": null, "timestamp": at(26_000), "sessionId": "s1",
                   "compactMetadata": {"trigger": "manual", "preTokens": 97_000, "postTokens": 12_000, "durationMs": 21_000}}),
            prompt("u2", Some("cb2"), 30_000, "再试一次"),
            json!({"type": "assistant", "uuid": "e1", "parentUuid": "u2", "timestamp": at(31_000), "sessionId": "s1",
                   "isApiErrorMessage": true, "error": "server_error", "apiErrorStatus": 529,
                   "message": {"id": "x", "model": "<synthetic>", "content": [{"type": "text", "text": "API Error: overloaded"}]}}),
            prompt("u3", Some("e1"), 40_000, "跑一下测试"),
            reply(
                "a4",
                "u3",
                44_000,
                "m3",
                json!({"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/p/app/src/usage.rs"}}),
                7,
                None,
            ),
            reply("a5", "a4", 44_100, "m3", bash("t3", "cargo build --release"), 9, Some("tool_use")),
            json!({"type": "user", "uuid": "r2", "parentUuid": "a4", "timestamp": at(44_200), "sessionId": "s1",
                   "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2",
                               "content": "     1\tuse std::io;\n     2\t\n     3\tfn main() {\n     4\t    run();\n     5\t}\n\n<system-reminder>not file content</system-reminder>"}]}}),
        ];
        let p = file(name, &lines);
        (Trace::open_born(p.clone(), Agent::Claude, ts_ms(&at(4000))).unwrap(), p)
    }

    fn kinds(t: &Trace, i: usize) -> Vec<Kind> {
        t.nodes[i].kids.iter().map(|&k| t.nodes[k].kind).collect()
    }

    /// 起了后台子代理的一段:第一轮起两个(一个 Explore,一个 fork),Explore 里又起了一个;跑完了来一条通知开第二轮
    pub(crate) fn with_subs(name: &str) -> (PathBuf, Vec<Value>, Vec<Value>) {
        let agent = |id: &str, kind: &str, desc: &str| json!({"type": "tool_use", "id": id, "name": "Agent", "input": {"subagent_type": kind, "description": desc, "prompt": "找出所有 width( 的调用"}});
        let launched = |uuid: &str, parent: &str, ms: u32, tool: &str, agent: &str| {
            json!({"type": "user", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "promptId": "p",
                   "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": agent},
                   "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool, "content": "Async agent launched successfully."}]}})
        };
        let main = vec![
            prompt("u1", None, 1000, "找一下 width 用在哪"),
            reply("b1", "u1", 2000, "n1", agent("ag1", "Explore", "找 width"), 30, None),
            reply("b1b", "b1", 2010, "n1", agent("ag3", "fork", "接着想"), 30, Some("tool_use")),
            launched("q1", "b1", 2100, "ag1", "a1"),
            launched("q3", "b1b", 2110, "ag3", "a3"),
            reply("b2", "q3", 3000, "n2", json!({"type": "text", "text": "在后台找了"}), 20, Some("end_turn")),
            turn_end("b2", 3100, 2100),
        ];
        let later = vec![
            json!({"type": "user", "uuid": "u2", "parentUuid": "b2", "timestamp": at(20_000), "sessionId": "s1", "origin": {"kind": "task-notification"},
                   "message": {"role": "user", "content": "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>ag1</tool-use-id>\n<status>completed</status>\n<summary>Agent \"找 width\" completed</summary>\n</task-notification>"}}),
            reply("b3", "u2", 21_000, "n3", json!({"type": "text", "text": "它找到了 4 处"}), 20, Some("end_turn")),
            turn_end("b3", 21_100, 1100),
        ];
        let p = file(name, &main);
        let subs = p.with_extension("").join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let write = |id: &str, meta: Value, lines: &[Value]| {
            std::fs::write(subs.join(format!("agent-{id}.meta.json")), meta.to_string()).unwrap();
            std::fs::write(subs.join(format!("agent-{id}.jsonl")), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        };
        write(
            "a1",
            json!({"agentType": "Explore", "description": "找 width", "toolUseId": "ag1", "spawnDepth": 1}),
            &[
                prompt("s1", None, 2050, "找出所有 width( 的调用"),
                reply(
                    "s2",
                    "s1",
                    2900,
                    "x1",
                    json!({"type": "tool_use", "id": "ag2", "name": "Agent", "input": {"subagent_type": "general-purpose", "description": "细看 ui.rs"}}),
                    40,
                    Some("tool_use"),
                ),
                done("s3", "s2", 3100, "ag2", false),
                reply("s4", "s3", 4000, "x2", bash("sb1", "rg -n 'width\\('"), 50, Some("tool_use")),
                done("s5", "s4", 5000, "sb1", false),
                reply(
                    "s6",
                    "s5",
                    9000,
                    "x3",
                    json!({"type": "text", "text": "找到 4 处:\n- src/bill.rs:12\n- src/ui.rs:88\n- src/ui.rs:90\n- src/trace.rs:7"}),
                    300,
                    Some("end_turn"),
                ),
            ],
        );
        // 嵌套的:起它的工具在 a1 的记录里
        write(
            "a2",
            json!({"agentType": "general-purpose", "description": "细看 ui.rs", "toolUseId": "ag2", "parentAgentId": "a1", "spawnDepth": 2}),
            &[
                prompt("t1", None, 2950, "细看 ui.rs"),
                reply("t2", "t1", 3050, "y1", json!({"type": "text", "text": "ui.rs 里有 2 处"}), 60, Some("end_turn")),
            ],
        );
        // fork 出来的:第一行是 fork-context-ref
        write(
            "a3",
            json!({"agentType": "fork", "description": "接着想", "toolUseId": "ag3", "spawnDepth": 1, "isFork": true}),
            &[
                json!({"type": "fork-context-ref", "agentId": "a3", "parentSessionId": "s1", "parentLastUuid": "b1b", "contextLength": 12, "timestamp": at(2020)}),
                prompt("f1", None, 2030, "接着想"),
                reply("f2", "f1", 2500, "z1", json!({"type": "text", "text": "想好了"}), 10, Some("end_turn")),
            ],
        );
        (p, main, later)
    }

    #[test]
    fn subagents_hang_under_the_agent_that_started_them_and_their_money_counts_in_that_turn() {
        let (p, main, later) = with_subs("subs");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            writeln!(f, "{l}").unwrap();
        }
        let t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        let turn = t.roots[0];
        let call = t.nodes[turn].kids[0];
        let (ag1, ag3) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        let s1 = t.sub_of(ag1).expect("Explore 挂在 ag1 下");
        assert_eq!((s1.kind.as_str(), s1.desc.as_str(), s1.t.is_sub), ("Explore", "找 width", true));
        // 嵌套的挂进 a1 自己的记录,不在最上面这层
        assert_eq!(t.subs.len(), 2);
        let ag2 = s1.t.nodes[s1.t.nodes[s1.t.roots[0]].kids[0]].kids[0];
        assert_eq!(s1.t.sub_of(ag2).map(|s| s.kind.as_str()), Some("general-purpose"));
        // fork 出来的开头一行「接着父对话」
        let s3 = t.sub_of(ag3).unwrap();
        assert_eq!((s3.t.nodes[s3.t.roots[0]].kind, s3.t.nodes[s3.t.roots[0]].label.as_str()), (Kind::Mark, "接着父对话"));
        // 子代理收到的是任务;干完了、也收到了通知:不在跑
        assert_eq!(crate::trace::asked(&s1.t, s1.t.roots[0]), ("任务", "找出所有 width( 的调用".to_string()));
        assert!(!t.sub_live(ag1, true, ts_ms(&at(30_000))) && !t.sub_live(ag3, true, ts_ms(&at(30_000))));
        assert_eq!(t.sub_dur(ag1, false, 0), 9000 - 2050);
        // 钱:四份记录 $ 读出来的加起来;子代理的算进起它的那一轮
        let bill = |files: &[&str]| -> f64 {
            files
                .iter()
                .flat_map(|f| crate::usage::tests::claude_seen(&p.parent().unwrap().join(f)))
                .map(|(m, tok)| crate::usage::call_cost(&m, tok, false, false, 0).unwrap())
                .sum()
        };
        let subs = bill(&["s1/subagents/agent-a1.jsonl", "s1/subagents/agent-a2.jsonl", "s1/subagents/agent-a3.jsonl"]);
        let (turns, _, all) = t.total(false, 0);
        assert!((all.cost - bill(&["s1.jsonl"]) - subs).abs() < 1e-12, "{} vs {}", all.cost, bill(&["s1.jsonl"]) + subs);
        // 第一轮:自己的两次调用 + 三个子代理(连嵌套的)的钱
        let s = t.sum(turn);
        assert_eq!((turns, s.calls, s.subs), (2, 2, 3));
        assert!((s.sub_cost - subs).abs() < 1e-12 && (s.cost + t.sum(t.roots[1]).cost - all.cost).abs() < 1e-12);
        // 右边:Agent 那一步写子代理的数字,底下露它最后说的前 3 行
        let plain = |ps: &[crate::trace::Para], w: usize| {
            crate::trace::wrap(ps, w).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>()
        };
        let (ps, fold) = tool_item(&t, ag1, false, 0, false);
        let text = plain(&ps, 80);
        assert!(fold && text[0].starts_with("▸ Agent Explore · 找 width") && text[0].contains("3 调用 · $"), "{text:#?}");
        assert_eq!(&text[1..], ["  找到 4 处:", "  src/bill.rs:12", "  src/ui.rs:88", "  … 另 2 行"]);
        let c = crate::trace::chat(&t, turn, false, 0);
        assert!(crate::ansi::plain(&c.foot.body).contains("3 子代理"), "{:?}", crate::ansi::plain(&c.foot.body));
        // e:连子代理的过程一起写进去
        let Some(View::Text { text, .. }) = view(&t, ag1, false, 0) else { panic!() };
        assert!(text.contains("## 任务\n\n找出所有 width( 的调用") && text.contains("rg -n 'width\\('"), "{text}");
        drop(main);
    }

    #[test]
    fn a_background_subagent_runs_until_it_says_its_last_word_and_later_ones_are_found_as_the_record_grows() {
        let (p, main, later) = with_subs("subs-live");
        // a1 还没写完(停在跑命令)、也没有通知
        let a1 = p.with_extension("").join("subagents/agent-a1.jsonl");
        let full = std::fs::read_to_string(&a1).unwrap();
        let cut: String = full.lines().take(4).map(|l| format!("{l}\n")).collect();
        std::fs::write(&a1, &cut).unwrap();
        // a3 的 meta 晚一点才写出来
        let a3 = p.with_extension("").join("subagents/agent-a3.meta.json");
        let meta3 = std::fs::read_to_string(&a3).unwrap();
        std::fs::remove_file(&a3).unwrap();
        let mut t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        let call = t.nodes[t.roots[0]].kids[0];
        let (ag1, ag3) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert!(t.sub_live(ag1, true, ts_ms(&at(6000))) && !t.sub_live(ag1, false, ts_ms(&at(6000))));
        assert!(t.sub_of(ag3).is_none());
        let busy = crate::trace::chat(&t, t.roots[0], true, ts_ms(&at(6000)));
        assert!(crate::ansi::plain(&busy.foot.body).starts_with("● 子代理在跑 · "));
        // 接着写:a1 写完、a3 出来、通知到了
        std::fs::write(&a1, &full).unwrap();
        std::fs::write(&a3, meta3).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            writeln!(f, "{l}").unwrap();
        }
        assert!(t.tail().unwrap());
        assert!(!t.sub_live(ag1, true, ts_ms(&at(30_000))) && t.sub_of(ag3).is_some());
        assert_eq!(t.sub_of(ag1).unwrap().t.nodes.iter().filter(|n| n.kind == Kind::Call).count(), 3);
        drop(main);
    }

    #[test]
    fn one_reply_split_into_blocks_is_one_call_and_its_last_line_has_the_usage() {
        let t = open(
            "blocks",
            &[
                prompt("u1", None, 1000, "跑一下测试\n第二行"),
                reply("a1", "u1", 4000, "m1", json!({"type": "text", "text": "先跑单测"}), 5, None),
                reply("a2", "a1", 9400, "m1", bash("t1", "cargo test --release"), 640, Some("tool_use")),
                done("r1", "a2", 21_700, "t1", false),
                reply("a3", "r1", 24_900, "m2", json!({"type": "text", "text": "测试全过了"}), 410, Some("end_turn")),
                turn_end("a3", 25_000, 24_000),
            ],
        );
        assert_eq!(t.roots.len(), 1);
        let turn = t.roots[0];
        assert_eq!(t.nodes[turn].label, "跑一下测试");
        assert_eq!(kinds(&t, turn), [Kind::Call, Kind::Call]);
        let c1 = t.nodes[turn].kids[0];
        let n = &t.nodes[c1];
        // 从提问那行算起,到最后一块
        assert_eq!((n.t1 - n.t0, n.tok, n.stop.as_str(), n.label.as_str()), (8400, [10, 0, 100, 1000, 640], "tool_use", "先跑单测"));
        let tool = &t.nodes[n.kids[0]];
        assert_eq!(
            (tool.name.as_str(), tool.label.as_str(), tool.t1 - tool.t0, tool.state),
            ("Bash", "cargo test --release", 12_300, St::Ok)
        );
        // 第二次调用从工具结果那行算起
        let c2 = &t.nodes[t.nodes[turn].kids[1]];
        assert_eq!(c2.t1 - c2.t0, 3200);
        assert!(t.nodes[turn].exact);
        assert_eq!(t.dur(turn, false, 0), 24_000);
        let s = t.sum(turn);
        assert_eq!((s.calls, s.tools, s.out), (2, 1, 1050));
        let want = crate::usage::call_cost("claude-opus-5-5", [10, 0, 100, 1000, 640], false, false, 0).unwrap()
            + crate::usage::call_cost("claude-opus-5-5", [10, 0, 100, 1000, 410], false, false, 0).unwrap();
        assert!((s.cost - want).abs() < 1e-12);
    }

    #[test]
    fn parallel_tools_hang_under_the_call_that_asked_for_them() {
        let t = open(
            "parallel",
            &[
                prompt("u1", None, 0, "看两个文件"),
                reply(
                    "a1",
                    "u1",
                    1000,
                    "m1",
                    json!({"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/p/app/src/a.rs"}}),
                    5,
                    None,
                ),
                reply(
                    "a2",
                    "a1",
                    1100,
                    "m1",
                    json!({"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/p/app/src/b.rs"}}),
                    9,
                    Some("tool_use"),
                ),
                // 块 2 和块 1 的结果都挂在块 1 下面(parentUuid 分叉),照样归同一次调用
                done("r1", "a1", 1200, "t1", false),
                done("r2", "a2", 1300, "t2", true),
            ],
        );
        let call = t.nodes[t.roots[0]].kids[0];
        assert_eq!(t.nodes[call].kids.len(), 2);
        let tools: Vec<(String, St)> = t.nodes[call].kids.iter().map(|&k| (t.nodes[k].label.clone(), t.nodes[k].state)).collect();
        // 没有它记的目录:照原样写
        assert_eq!(tools, [("/p/app/src/a.rs".to_string(), St::Ok), ("/p/app/src/b.rs".to_string(), St::Err)]);
    }

    #[test]
    fn denied_interrupted_and_api_errors() {
        let mut denied = done("r1", "a1", 2000, "t1", true);
        denied["toolDenialKind"] = json!("user-rejected");
        let t = open(
            "errors",
            &[
                prompt("u1", None, 0, "删掉它"),
                reply("a1", "u1", 1000, "m1", bash("t1", "rm -rf x"), 5, Some("tool_use")),
                denied,
                json!({"type": "assistant", "uuid": "e1", "parentUuid": "r1", "timestamp": at(3000), "sessionId": "s1",
                       "isApiErrorMessage": true, "error": "server_error", "apiErrorStatus": 529,
                       "message": {"id": "x", "model": "<synthetic>", "content": [{"type": "text", "text": "API Error"}],
                                   "usage": {"input_tokens": 0, "output_tokens": 0}}}),
                prompt("u2", Some("e1"), 4000, "再来"),
                reply("a2", "u2", 5000, "m2", bash("t2", "sleep 100"), 5, Some("tool_use")),
                json!({"type": "user", "uuid": "i1", "parentUuid": "a2", "timestamp": at(9000), "sessionId": "s1",
                       "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}}),
            ],
        );
        let (a, b) = (t.roots[0], t.roots[1]);
        assert_eq!(kinds(&t, a), [Kind::Call, Kind::Fail]);
        assert_eq!(t.nodes[a].state, St::Err);
        let fail = &t.nodes[t.nodes[a].kids[1]];
        assert_eq!(fail.label, "529 server_error");
        assert_eq!(t.nodes[t.nodes[t.nodes[a].kids[0]].kids[0]].state, St::Denied);
        // 出错那条不算调用
        assert_eq!(t.sum(a).calls, 1);
        assert_eq!(t.nodes[b].state, St::Stopped);
        let tool = &t.nodes[t.nodes[t.nodes[b].kids[0]].kids[0]];
        assert_eq!((tool.state, tool.t1 - tool.t0), (St::Stopped, 4000));
    }

    #[test]
    fn copied_history_folds_into_one_row_and_is_not_counted() {
        let lines = vec![
            // 会话启动的钩子附件比文件早几秒也不算抄的
            json!({"type": "attachment", "uuid": "h0", "parentUuid": null, "timestamp": at(0), "sessionId": "s1", "attachment": {"type": "hook_success"}}),
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb", "parentUuid": null, "timestamp": at(0), "sessionId": "s1",
                   "compactMetadata": {"trigger": "manual", "preTokens": 182_000, "postTokens": 21_000, "durationMs": 38_000}}),
            prompt("o1", Some("cb"), 100, "以前的"),
            reply("o2", "o1", 1000, "m0", json!({"type": "text", "text": "好"}), 50, Some("end_turn")),
            // 文件是 4 秒时建的:往后的是这段对话自己的;resume 之后写的行 session_id 不一样,照样算
            json!({"type": "user", "uuid": "u1", "parentUuid": "o2", "timestamp": at(5000), "sessionId": "s1", "session_id": "other",
                   "origin": {"kind": "human"}, "message": {"role": "user", "content": "接着来"}}),
            reply("a1", "u1", 6000, "m1", json!({"type": "text", "text": "嗯"}), 7, Some("end_turn")),
        ];
        let t = Trace::open_born(file("copied", &lines), Agent::Claude, ts_ms(&at(4000))).unwrap();
        assert_eq!(t.roots.iter().map(|&r| t.nodes[r].kind).collect::<Vec<_>>(), [Kind::Copied, Kind::Turn]);
        let g = t.roots[0];
        assert_eq!(kinds(&t, g), [Kind::Mark, Kind::Turn]);
        assert_eq!(t.nodes[t.nodes[g].kids[0]].label, "压缩 182k → 21k · 38.0s");
        let (turns, _, s) = t.total(false, 0);
        assert_eq!((turns, s.calls, s.out), (1, 1, 7));
        assert_eq!(t.turn_no(t.roots[1]), Some(1));
    }

    #[test]
    fn a_prompt_nobody_answered_gives_way_to_the_retyped_one_but_commands_stay() {
        let t = open(
            "retyped",
            &[
                prompt("u1", None, 0, "打错了"),
                prompt("u2", Some("u1"), 3000, "重打的"),
                reply("a1", "u2", 4000, "m1", json!({"type": "text", "text": "好"}), 3, Some("end_turn")),
                json!({"type": "user", "uuid": "c1", "parentUuid": "a1", "timestamp": at(5000), "sessionId": "s1",
                       "message": {"role": "user", "content": "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>"}}),
                json!({"type": "user", "uuid": "c2", "parentUuid": "c1", "timestamp": at(5001), "sessionId": "s1",
                       "message": {"role": "user", "content": "<local-command-stdout>Set model</local-command-stdout>"}}),
                json!({"type": "user", "uuid": "m1", "parentUuid": "c2", "timestamp": at(5002), "sessionId": "s1", "isMeta": true,
                       "message": {"role": "user", "content": "caveat"}}),
                prompt("u3", Some("m1"), 6000, "下一句"),
            ],
        );
        let labels: Vec<&str> = t.roots.iter().map(|&r| t.nodes[r].label.as_str()).collect();
        assert_eq!(labels, ["重打的", "/model opus", "下一句"]);
    }

    #[test]
    fn a_half_written_last_line_waits_for_the_rest() {
        let p = file("tail", &[prompt("u1", None, 0, "开始")]);
        let mut t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        let line = reply("a1", "u1", 2000, "m1", json!({"type": "text", "text": "好"}), 3, Some("end_turn")).to_string();
        let (a, b) = line.split_at(40);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        write!(f, "{a}").unwrap();
        assert!(!t.tail().unwrap());
        assert!(t.nodes[t.roots[0]].kids.is_empty());
        writeln!(f, "{b}").unwrap();
        assert!(t.tail().unwrap());
        assert_eq!(kinds(&t, t.roots[0]), [Kind::Call]);
        assert!(t.running(t.roots[0], true));
    }

    #[test]
    fn huge_images_are_skipped_not_expanded_and_the_detail_reads_them_back() {
        let big = "A".repeat(2 << 20);
        let t = open(
            "detail",
            &[
                prompt("u1", None, 0, "看图"),
                reply(
                    "a1",
                    "u1",
                    1000,
                    "m1",
                    json!({"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/p/x.png"}}),
                    5,
                    Some("tool_use"),
                ),
                json!({"type": "user", "uuid": "r1", "parentUuid": "a1", "timestamp": at(1500), "sessionId": "s1",
                       "toolUseResult": {"type": "image", "file": {"base64": big}},
                       "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1",
                                   "content": [{"type": "image", "source": {"type": "base64", "data": big}}]}]}}),
            ],
        );
        let tool = t.nodes[t.nodes[t.roots[0]].kids[0]].kids[0];
        let text: Vec<String> = crate::trace::wrap(&crate::trace::detail(&t, tool, false, 0), 200).iter().map(crate::ansi::plain).collect();
        assert!(text.iter().any(|l| l == "[图片]"), "{text:?}");
        assert!(text.iter().any(|l| l.contains("/p/x.png")), "{text:?}");
        let call = t.nodes[t.roots[0]].kids[0];
        let text: Vec<String> = crate::trace::wrap(&crate::trace::detail(&t, call, false, 0), 200).iter().map(crate::ansi::plain).collect();
        assert!(text.iter().any(|l| l.contains("缓存写 100(1 小时)")), "{text:?}");
    }

    #[test]
    fn edits_show_the_real_patch_with_line_numbers_or_else_the_changed_lines_only() {
        let rows = |ch: Vec<Change>| ch.into_iter().map(|c| (c.c, c.no, c.text)).collect::<Vec<_>>();
        let s = |x: &str| x.to_string();
        let res = json!({"toolUseResult": {"structuredPatch": [
            {"oldStart": 7, "newStart": 7, "lines": [" a", "-b", "+c", "+d", " e"]},
            {"oldStart": 40, "newStart": 41, "lines": ["-f", "\\ No newline at end of file"]}]}});
        // 删的写原来的行号,加的、上下文写改完的;两处中间一行 '~'
        assert_eq!(
            rows(changes("Edit", None, Some(&res))),
            [
                (' ', Some(7), s("a")),
                ('-', Some(8), s("b")),
                ('+', Some(8), s("c")),
                ('+', Some(9), s("d")),
                (' ', Some(10), s("e")),
                ('~', None, s("")),
                ('-', Some(40), s("f"))
            ]
        );
        // 没有结果(被拒、还没回来):拿输入比,头尾相同的行不列,不知道行号
        let inp = json!({"old_string": "x\nold\ny", "new_string": "x\nnew\nnewer\ny"});
        assert_eq!(rows(changes("Edit", Some(&inp), None)), [('-', None, s("old")), ('+', None, s("new")), ('+', None, s("newer"))]);
        // 新建的文件:每行都是加的,从第 1 行起
        let new_file = changes("Write", Some(&json!({"content": "l1\nl2"})), Some(&json!({"toolUseResult": {"structuredPatch": []}})));
        assert_eq!(rows(new_file), [('+', Some(1), s("l1")), ('+', Some(2), s("l2"))]);
    }

    /// 右边一个工具排成的字(宽 60)
    fn tool_text(t: &Trace, i: usize, open: bool) -> (Vec<String>, bool) {
        let (ps, fold) = tool_item(t, i, false, 0, open);
        (crate::trace::wrap(&ps, 60).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect(), fold)
    }

    #[test]
    fn a_tool_folds_to_a_peek_and_o_opens_all_of_it() {
        let (t, _) = fixture("fold");
        let tool = |name: &str| (0..t.nodes.len()).find(|&i| t.nodes[i].name == name).unwrap();
        // 收着:头一行写名字、相对路径,靠右写改了几行;底下只露改动的行(带行号),上下文不露
        let (text, fold) = tool_text(&t, tool("Edit"), false);
        assert!(fold);
        assert!(text[0].starts_with("▸ Edit  src/bill.rs ") && text[0].ends_with("+2 -2"), "{text:?}");
        assert_eq!(
            text[1..],
            ["  12 -     let w = s.len();", "  12 +     let w = s.width();", "  41 -     pad(w)", "  41 +     pad(w.max(1))"]
        );
        // 上色:代码按 .rs 上色,加的那行铺淡绿底一直到行尾、删的淡红底,行号不铺
        let p = theme::pal();
        let lines = crate::trace::wrap(&tool_item(&t, tool("Edit"), false, 0, false).0, 60);
        let seg_of = |l: &Styled, w: &str| l.iter().find(|s| s.text.contains(w)).map(|s| s.style).unwrap();
        assert_eq!(seg_of(&lines[2], "let").fg, Some(p.syn_kw));
        assert_eq!(seg_of(&lines[2], "width").bg, Some(p.add_bg));
        assert_eq!(seg_of(&lines[1], "len").bg, Some(p.del_bg));
        assert_eq!(seg_of(&lines[2], "12").bg, None);
        assert_eq!(lines[2].last().unwrap().style.bg, Some(p.add_bg));
        assert_eq!(crate::ansi::plain(&lines[2]).width(), 60);
        // 展开:带上下文,两处中间一行点线
        let (text, _) = tool_text(&t, tool("Edit"), true);
        assert!(text[0].starts_with("▾ Edit"), "{text:?}");
        assert_eq!(
            text[1..6],
            [
                "  11   fn col(s: &str) -> usize {",
                "  12 -     let w = s.len();",
                "  12 +     let w = s.width();",
                "  13       w",
                "  ┈┈┈┈┈"
            ]
        );
        // Read 收着什么都不露,靠右写读了几行;展开是带行号的文件内容,后面跟的提示不要
        let (text, fold) = tool_text(&t, tool("Read"), false);
        assert!(fold && text.len() == 1 && text[0].starts_with("▸ Read  src/usage.rs") && text[0].ends_with("5 行"), "{text:?}");
        let (text, _) = tool_text(&t, tool("Read"), true);
        assert_eq!(text[1..], ["  1  use std::io;", "  2", "  3  fn main() {", "  4      run();", "  5  }"]);
        let lines = crate::trace::wrap(&tool_item(&t, tool("Read"), false, 0, true).0, 60);
        assert_eq!(seg_of(&lines[1], "use").fg, Some(p.syn_kw));
        // Bash:收着露输出的头 3 行;展开先写整条命令,再是全部输出
        let (text, _) = tool_text(&t, tool("Bash"), false);
        assert_eq!(text[1..], ["  running 220 tests", "  test bill::cols ... ok", "  test ui::wide ... ok", "  … 另 3 行"]);
        let (text, _) = tool_text(&t, tool("Bash"), true);
        assert_eq!(text[1], "  $ cargo test --release");
        let lines = crate::trace::wrap(&tool_item(&t, tool("Bash"), false, 0, true).0, 60);
        assert_ne!(seg_of(&lines[1], "cargo").fg, None, "命令上色:{:?}", lines[1]);
        assert_eq!(text.last().unwrap(), "  test result: ok. 220 passed");
        // 全露了的不能 o
        let (text, fold) = tool_text(&t, tool("Grep"), false);
        assert!(fold && text.last().unwrap() == "  … 另 1 行", "{text:?}");
    }

    #[test]
    fn coloring_never_changes_how_many_lines_a_block_takes() {
        // 界面先不上色地量高度、看得见了再上色:两边必须一样高
        let (t, _) = fixture("same-height");
        for &turn in t.roots.iter().filter(|&&r| t.nodes[r].kind == Kind::Turn) {
            for open in [false, true] {
                for w in [30, 43, 80] {
                    let heights = |ss: Vec<Step>| ss.iter().map(|s| crate::trace::wrap(&s.paras, w).len()).collect::<Vec<_>>();
                    let plain = heights(crate::hl::plain(|| steps(&t, turn, false, 0, &|_| open)));
                    assert_eq!(plain, heights(steps(&t, turn, false, 0, &|_| open)), "第 {turn} 轮 open={open} w={w}");
                    // 一块块单独排的和一起排的一样
                    let one: Vec<Step> = (0..plain.len()).map(|k| step(&t, turn, k, false, 0, &|_| open).unwrap()).collect();
                    assert_eq!(plain, heights(one));
                }
            }
        }
    }

    #[test]
    fn e_shows_the_file_itself_or_a_markdown_copy_with_everything() {
        let (t, _) = fixture("view");
        let node = |name: &str| (0..t.nodes.len()).find(|&i| t.nodes[i].name == name).unwrap();
        let md = |i: usize| match view(&t, i, false, 0) {
            Some(View::Text { text, ext, line: 1 }) if ext == "md" => text,
            x => panic!("{x:?}"),
        };
        // 文件不在了:改动写成 diff 代码块,每处写从第几行起
        let edit = md(node("Edit"));
        for want in
            ["# Edit · src/bill.rs", "```diff", "@@ 第 11 行 @@", "-    let w = s.len();", "+    let w = s.width();", "@@ 第 40 行 @@"]
        {
            assert!(edit.contains(want), "缺「{want}」:{edit}");
        }
        // 读到的内容按文件的语言写,没有行号、没有后面跟的提示
        let read = md(node("Read"));
        assert!(read.contains("```rs\nuse std::io;\n\nfn main() {") && !read.contains("system-reminder"), "{read}");
        // 一轮:你说的、每次调用(数字、想的引用、说的)、每个工具的命令和全部输出
        let turn = md(t.roots.iter().copied().find(|&r| t.nodes[r].label == "看看两栏为什么对不齐").unwrap());
        for want in [
            "# 第 1 轮 · ",
            "## 你\n\n看看两栏为什么对不齐",
            "## 第 1 轮 · 第 1 次调用 · 4.0s",
            "> 先确认是不是宽字符的问题。",
            "### Grep · width(",
            "src/ui.rs:90:",
            "### Bash · cargo test --release · 8.8s",
            "```sh\n# run it\ncargo test --release",
            "test result: ok. 220 passed",
            "- 测试全过了",
        ] {
            assert!(turn.contains(want), "缺「{want}」:{turn}");
        }
        // 压缩那行没有可看的
        assert_eq!(view(&t, t.roots[0], false, 0), None);
        // 输出里有 ``` 的,外面的围栏多一个
        assert_eq!(fenced("text", "a\n```\nb"), "````text\na\n```\nb\n````\n\n");
        // 文件还在:看文件本身,停在读的那行 / 改完的第一行
        let dir = std::env::temp_dir().join(format!("aca-trace-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.rs");
        std::fs::write(&f, "fn a() {}\n").unwrap();
        let fp = f.to_string_lossy().to_string();
        let t = open(
            "view-real",
            &[
                prompt("u1", None, 0, "看"),
                reply(
                    "a1",
                    "u1",
                    1000,
                    "m1",
                    json!({"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": fp, "offset": 30}}),
                    5,
                    None,
                ),
                reply(
                    "a2",
                    "a1",
                    1100,
                    "m1",
                    json!({"type": "tool_use", "id": "t2", "name": "Edit", "input": {"file_path": fp, "old_string": "a", "new_string": "b"}}),
                    9,
                    Some("tool_use"),
                ),
                done("r1", "a1", 1200, "t1", false),
                json!({"type": "user", "uuid": "r2", "parentUuid": "a2", "timestamp": at(1300), "sessionId": "s1",
                       "toolUseResult": {"structuredPatch": [{"oldStart": 7, "newStart": 7, "lines": [" x", "-a", "+b"]}]},
                       "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": "ok"}]}}),
            ],
        );
        let call = t.nodes[t.roots[0]].kids[0];
        let (read, edit) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert_eq!(view(&t, read, false, 0), Some(View::File { path: fp.clone(), line: 30 }));
        assert_eq!(view(&t, edit, false, 0), Some(View::File { path: fp, line: 8 }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_call_keeps_the_first_line_of_what_it_thought_and_said_without_marks() {
        let t = open(
            "said",
            &[
                prompt("u1", None, 0, "问"),
                reply("a1", "u1", 1000, "m1", json!({"type": "thinking", "thinking": "\n先想想\n再说"}), 5, None),
                reply("a2", "a1", 1100, "m1", json!({"type": "text", "text": "## **结论**:用 `width()`"}), 9, Some("end_turn")),
            ],
        );
        let call = &t.nodes[t.nodes[t.roots[0]].kids[0]];
        assert_eq!((call.thought.as_str(), call.label.as_str()), ("先想想", "结论:用 width()"));
        // 对话里:数字写在分隔线上、想的摊成一段(没有句末标点的行后面用 · 隔开)、说的全文;展开了想的照原来分行
        let text = |open: bool| -> Vec<String> {
            flow(&t, t.roots[0], false, 0, false, &|_| open)
                .iter()
                .flat_map(|s| crate::trace::wrap(&s.paras, 80))
                .map(|l| crate::ansi::plain(&l))
                .collect()
        };
        let closed = text(false);
        assert!(closed[0].starts_with("你  问"), "{closed:?}");
        assert!(closed.iter().any(|l| l.starts_with("── ") && l.contains("出 9") && l.ends_with("──") && l.width() == 80), "{closed:?}");
        assert!(closed.iter().any(|l| l == "思考 先想想 · 再说"), "{closed:?}");
        assert!(closed.iter().any(|l| l == "结论:用 width()"), "{closed:?}");
        let open = text(true);
        assert!(open.windows(2).any(|w| w[0] == "思考 先想想" && w[1] == "     再说"), "{open:?}");
    }

    #[test]
    fn a_long_thought_folds_to_five_lines_wherever_it_is_read_and_a_short_one_shows_whole() {
        let long = "先确认是不是宽字符的问题。".repeat(12);
        let t = open(
            "thought",
            &[
                prompt("u1", None, 0, "问"),
                reply("a1", "u1", 1000, "m1", json!({"type": "thinking", "thinking": long}), 5, None),
                reply("a2", "a1", 1100, "m2", json!({"type": "thinking", "thinking": "**看一眼**\n\n就改"}), 5, None),
                reply("a3", "a2", 1200, "m3", json!({"type": "text", "text": "好了"}), 9, Some("end_turn")),
            ],
        );
        let turn = t.roots[0];
        let call = |k: usize, open: bool| step(&t, turn, k, false, 0, &|_| open).unwrap();
        let plain = |ls: Vec<crate::ansi::Styled>| ls.iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>();
        // 手机(39 格)和桌面(57 格)都只露 5 行,末尾 …;这一步能 o
        for w in [39, 57] {
            let (lines, clipped) = crate::trace::wrap_clipped(&call(1, false).paras, w);
            let thought: Vec<String> = plain(lines).into_iter().skip(1).filter(|l| !l.is_empty()).collect();
            assert_eq!(thought.len(), 5, "{w}: {thought:#?}");
            assert!(thought[0].starts_with("思考 先确认") && thought[4].ends_with('…') && clipped, "{w}: {thought:#?}");
            assert!(thought.iter().all(|l| l.width() <= w), "{w}: {thought:#?}");
        }
        // 展开了不截
        let (lines, clipped) = crate::trace::wrap_clipped(&call(1, true).paras, 39);
        assert!(lines.len() > 6 && !clipped);
        // 短的整段露出来,标题后面用 · 隔开,没东西可展开
        let (lines, clipped) = crate::trace::wrap_clipped(&call(2, false).paras, 39);
        assert_eq!((plain(lines)[1].as_str(), clipped), ("思考 看一眼 · 就改", false));
    }

    #[test]
    fn tool_labels_are_one_short_line() {
        let i = |v: Value| serde_json::from_value::<Input>(v).unwrap();
        assert_eq!(tool_label("Bash", &i(json!({"command": "cd x &&\nmake"})), ""), "cd x &&");
        assert_eq!(tool_label("Edit", &i(json!({"file_path": "/p/app/src/ui.rs"})), "/p/app"), "src/ui.rs");
        assert_eq!(tool_label("WebFetch", &i(json!({"url": "https://example.com/a/b"})), ""), "example.com");
        assert_eq!(tool_label("Agent", &i(json!({"subagent_type": "Explore", "description": "查去重"})), ""), "Explore · 查去重");
        assert_eq!(tool_label("mcp__srv__do_it", &i(json!({})), ""), "srv · do_it");
        assert_eq!(ts_ms("2026-09-30T01:00:02.5Z") - ts_ms("2026-09-30T01:00:00.000Z"), 2500);
    }
}

#[cfg(test)]
mod probe {
    use crate::data::Agent;
    use crate::trace::{Kind, Trace};
    use std::path::PathBuf;

    /// 本机真实记录上核对子代理(只打印数字):ACA_TRACE_SUBS = 一行一个记录文件的清单
    #[test]
    #[ignore]
    fn probe_subs() {
        let Ok(list) = std::env::var("ACA_TRACE_SUBS") else { return };
        fn count(t: &crate::trace::Trace) -> usize {
            t.subs.iter().map(|s| 1 + count(&s.t)).sum()
        }
        fn files(t: &crate::trace::Trace, out: &mut Vec<PathBuf>) {
            for s in &t.subs {
                out.push(s.t.path.clone());
                files(&s.t, out);
            }
        }
        fn copied(t: &crate::trace::Trace) -> (usize, usize) {
            let mut c = t
                .roots
                .iter()
                .filter(|&&r| t.nodes[r].kind == Kind::Copied)
                .map(|&r| t.nodes[r].kids.iter().filter(|&&k| t.nodes[k].kind == Kind::Turn).count())
                .sum::<usize>();
            let mut own = t.roots.iter().filter(|&&r| t.nodes[r].kind == Kind::Turn).count();
            for s in &t.subs {
                let (a, b) = copied(&s.t);
                c += a;
                own += b;
            }
            (c, own)
        }
        let price = |seen: Vec<(String, [u64; 5])>| -> f64 {
            seen.iter().filter_map(|(m, tok)| crate::usage::call_cost(m, *tok, false, false, 0)).sum()
        };
        for (k, p) in std::fs::read_to_string(list).unwrap().lines().enumerate() {
            let codex = p.contains("rollout-");
            let agent = if codex { Agent::Codex } else { Agent::Claude };
            let t0 = std::time::Instant::now();
            let Ok(t) = Trace::open(p.into(), agent) else { continue };
            let el = t0.elapsed();
            let mut fs = vec![];
            files(&t, &mut fs);
            let (s, rolled) = (t.total(false, 0).2, t.nodes.iter().filter(|n| n.rolled).count());
            let (cp, own) = copied(&t);
            if codex {
                // 父线程自己的 $ + 子线程在记录视图里的钱(子线程的 $ 连抄来的一起算了,另列)
                let mine = price(crate::usage::tests::codex_seen(std::path::Path::new(p)));
                let kids_bill: f64 = fs.iter().map(|f| price(crate::usage::tests::codex_seen(f))).sum();
                let expected = std::fs::read_to_string(p).unwrap_or_default().matches("\"spawn_agent\"").count();
                println!(
                    "#{k} codex direct={} subs={} spawn_mentions={expected} want={} copied_turns={cp} own_turns={own} rolled={rolled} cost={:.4} own_bill={mine:.4} subs_cost={:.4} subs_bill_with_copies={kids_bill:.4} {:.0}ms",
                    t.subs.len(),
                    count(&t),
                    t.want.len(),
                    s.cost,
                    s.sub_cost,
                    el.as_secs_f64() * 1000.0
                );
            } else {
                let metas = std::fs::read_dir(std::path::Path::new(p).with_extension("").join("subagents"))
                    .map_or(0, |d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json")).count());
                let bill: f64 = price(crate::usage::tests::claude_seen(std::path::Path::new(p)))
                    + fs.iter().map(|f| price(crate::usage::tests::claude_seen(f))).sum::<f64>();
                // 没写完、也没收到通知的(不管多久没动静)
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
                let live = (0..t.nodes.len()).filter(|&i| t.sub_live(i, true, i64::MIN / 2)).count();
                for i in (0..t.nodes.len()).filter(|&i| t.sub_live(i, true, i64::MIN / 2)) {
                    let sub = t.sub_of(i).unwrap();
                    let last = sub.t.roots.iter().map(|&r| sub.t.last_at(r)).max().unwrap_or(0);
                    let lc = sub.t.nodes.iter().rev().find(|n| n.kind == Kind::Call);
                    let open = sub.t.nodes.iter().filter(|n| n.kind == Kind::Tool && n.t1 == 0).count();
                    let fails = sub.t.nodes.iter().filter(|n| n.kind == Kind::Fail).count();
                    let parent_age = (now - t.roots.iter().map(|&r| t.last_at(r)).max().unwrap_or(0)) / 60_000;
                    println!(
                        "   live? age={}min parent_age={parent_age}min stop={:?} open_tools={open} fails={fails} tool_done={}",
                        (now - last) / 60_000,
                        lc.map(|c| c.stop.as_str()),
                        t.nodes[i].t1 > 0
                    );
                }
                // 画一屏要算的:整段的合计、每一轮的合计、左边每一轮的对话块
                let t1 = std::time::Instant::now();
                let _ = t.total(false, 0);
                for &r in &t.roots {
                    let _ = t.sum(r);
                }
                let sums = t1.elapsed();
                let t2 = std::time::Instant::now();
                for &r in t.roots.iter().filter(|&&r| t.nodes[r].kind == Kind::Turn) {
                    let _ = crate::trace::chat(&t, r, false, 0);
                }
                println!(
                    "#{k} claude subs={}/{metas} live_if_running={live} copied_turns={cp} cost={:.4} bill={bill:.4} diff={:.6} open={:.0}ms sums={:.2}ms chat_all={:.0}ms",
                    count(&t),
                    s.cost,
                    s.cost - bill,
                    el.as_secs_f64() * 1000.0,
                    sums.as_secs_f64() * 1000.0,
                    t2.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
    }

    /// 只在手动时跑:ACA_TRACE_PROBE=<记录文件> cargo test --release probe -- --ignored --nocapture。只打印数量和耗时
    #[test]
    #[ignore]
    fn probe_real_file() {
        let Ok(p) = std::env::var("ACA_TRACE_PROBE") else { return };
        let codex = p.contains("rollout-");
        let agent = if codex { crate::data::Agent::Codex } else { crate::data::Agent::Claude };
        let t0 = std::time::Instant::now();
        let t = crate::trace::Trace::open(p.clone().into(), agent).unwrap();
        let el = t0.elapsed();
        if codex {
            // 工具按名字数;exec 认出了什么;没有提问的轮;钱和 $ 读同一份算的比
            let mut names = std::collections::BTreeMap::new();
            for n in t.nodes.iter().filter(|n| n.kind == crate::trace::Kind::Tool) {
                *names.entry(n.name.clone()).or_insert(0usize) += 1;
            }
            let no_prompt = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn && t.nodes[r].src.is_empty()).count();
            let steer = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn && t.nodes[r].src.len() > 1).count();
            let bill: f64 = crate::usage::tests::codex_seen(std::path::Path::new(&p))
                .iter()
                .filter_map(|(m, tok)| crate::usage::call_cost(m, *tok, false, false, 0))
                .sum();
            let calls = crate::usage::tests::codex_seen(std::path::Path::new(&p)).len();
            let s = t.total(false, 0).2;
            println!(
                "codex ver={:?} tools={names:?} no_prompt_turns={no_prompt} steer_turns={steer} calls={} bill_calls={calls} cost={:.4} bill={bill:.4}",
                t.cx.ver, s.calls, s.cost
            );
            // 每个工具读回来整理一遍要多久(右边展开、详情都要读)
            let t4 = std::time::Instant::now();
            let (mut with_cmd, mut with_edit, mut js, mut empty) = (0, 0, 0, 0);
            for i in (0..t.nodes.len()).filter(|&i| t.nodes[i].kind == crate::trace::Kind::Tool && t.nodes[i].name != "stdin") {
                let io = crate::trace::io(&t, i);
                match () {
                    _ if !io.cmds.is_empty() => with_cmd += 1,
                    _ if !io.edits.is_empty() => with_edit += 1,
                    _ if io.input.as_ref().is_some_and(|x| x.0 == "js") => js += 1,
                    _ => empty += 1,
                }
            }
            println!("io: cmd={with_cmd} edit={with_edit} js={js} other={empty} in {:?}", t4.elapsed());
        }
        let (turns, dur, s) = t.total(false, 0);
        let copied = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Copied).count();
        let marks = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Mark).count();
        let fails = t.nodes.iter().filter(|n| n.kind == crate::trace::Kind::Fail).count();
        let open_tools = t.nodes.iter().filter(|n| n.kind == crate::trace::Kind::Tool && n.t1 == 0).count();
        let exact = t.roots.iter().filter(|&&r| t.nodes[r].exact).count();
        let mut durs: Vec<(i64, bool, usize, usize)> = t
            .roots
            .iter()
            .filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn)
            .map(|&r| (t.dur(r, false, 0) / 1000, t.nodes[r].exact, t.nodes[r].kids.len(), t.sum(r).tools))
            .collect();
        durs.sort_by_key(|d| -d.0);
        let ex: i64 = durs.iter().filter(|d| d.1).map(|d| d.0).sum();
        let nx: i64 = durs.iter().filter(|d| !d.1).map(|d| d.0).sum();
        println!("top turns (secs, exact, kids, tools): {:?}  exact_sum={ex}s inexact_sum={nx}s", &durs[..durs.len().min(8)]);
        // 最大的一轮(节点最多)排成对话、折好要多久
        let big = t
            .roots
            .iter()
            .copied()
            .filter(|&r| t.nodes[r].kind == crate::trace::Kind::Turn)
            .max_by_key(|&r| t.nodes[r].kids.len())
            .unwrap();
        // 界面的做法:先不上色地排整轮、整段对话(量高度),再给一屏看得见的上色
        let t1 = std::time::Instant::now();
        let items = crate::hl::plain(|| crate::trace::steps(&t, big, false, 0, &|_| false));
        let lines: usize = items.iter().map(|s| crate::trace::wrap(&s.paras, 80).len()).sum();
        let el_flow = t1.elapsed();
        let t3 = std::time::Instant::now();
        let all: usize = crate::hl::plain(|| crate::trace::steps(&t, big, false, 0, &|_| true))
            .iter()
            .map(|s| crate::trace::wrap(&s.paras, 80).len())
            .sum();
        let el_all = t3.elapsed();
        let t2 = std::time::Instant::now();
        let chat: usize = crate::hl::plain(|| {
            t.roots
                .iter()
                .filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn)
                .map(|&r| {
                    let c = crate::trace::chat(&t, r, false, 0);
                    crate::trace::wrap(&c.asked, 60).len() + crate::trace::wrap(&c.reply, 60).len()
                })
                .sum()
        });
        let el_chat = t2.elapsed();
        // 上色一屏(50 行):全展开的那轮从头数到 50 行为止的块;第一次要编译用到的语法
        let screen = |open: bool| {
            let t5 = std::time::Instant::now();
            let (mut k, mut n) = (0, 0);
            while n < 50 {
                let Some(s) = crate::trace::step(&t, big, k, false, 0, &|_| open) else { break };
                n += crate::trace::wrap(&s.paras, 80).len();
                k += 1;
            }
            t5.elapsed()
        };
        let (cold, closed, open) = (screen(false), screen(false), crate::hl::upto(50, || screen(true)));
        println!(
            "biggest turn: kids={} items={} plain: steps={lines} lines {el_flow:?}, all open={all} lines {el_all:?}; whole chat lines={chat} in {el_chat:?}; color one screen: first {cold:?}, again {closed:?}, all open {open:?}",
            t.nodes[big].kids.len(),
            items.len()
        );
        println!(
            "bytes={} nodes={} turns={turns} exact_turns={exact} calls={} tools={} open_tools={open_tools} fails={fails} marks={marks} copied_groups={copied} models={} work={}s cost={:.2} parse={:?}",
            t.off,
            t.nodes.len(),
            s.calls,
            s.tools,
            t.models.len() - 1,
            dur / 1000,
            s.cost,
            el
        );
    }
}
