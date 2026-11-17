//! Claude's conversation record -> the `trace` tree. The rules were tested in 2026-09 on this machine's 440 records
//! (2.1.141-2.1.285):
//! - A turn: a `user` line whose `origin.kind` is human / task-notification / peer / coordinator, with no tool_result in
//!   the content (old records have no origin and are recognized by content); `system/turn_duration`'s durationMs is the
//!   turn's duration.
//!   A prompt retyped before anything answered it (no descendants) is dropped, only the later one is kept
//! - A call: the lines with the same message.id (one content block per line), usage / stop_reason taken from the last line
//!   (the output count only grows); counted from the line before the first block (tool result / prompt) to the last block
//! - A tool: tool_use.id <-> tool_result.tool_use_id, attached under the call that issued it (split by message.id, not by
//!   following parentUuid: parallel tools and streamed blocks make that fork); hook time is in the hook_success attachment
//! - model `<synthetic>` with isApiErrorMessage is an API error, not a call
//! - On resume / continue the new file starts with a copy of the old file's tail (uuids, timestamps unchanged): the
//!   consecutive conversation lines at the start of the file that are more than 2 seconds older than the file's creation.
//!   session_id != sessionId can't be used: session_id is the session id of the process writing that line, and new lines
//!   written after a resume differ too (13 records, 5,704 lines on this machine); the session-start hook attachments in
//!   the first line or two are also a few seconds early, but attachments don't count
//! - Compaction: `system/compact_boundary`, drawn as one line (the compaction call itself isn't written to the record)
//! - Subagents: `<session id>/subagents/agent-<id>.jsonl`; the `.meta.json` next to it has the type, description, and
//!   which Agent tool started it (toolUseId), plus parentAgentId for nested ones (that tool is in the parent subagent's
//!   record). All 175 on this machine attach back.
//!   Agents now mostly run in the background: the tool returns right away, and when done a task-notification arrives in
//!   the parent session (task-id = agentId, with status).
//!   A forked subagent's first line is fork-context-ref (it continues the parent conversation; the parent's lines aren't
//!   copied in)
//!
//! Large fields (content blocks, tool results, attachments) are skipped with RawValue, base64 is not expanded (the longest
//! line is several MB).

use std::hash::{DefaultHasher, Hash, Hasher};

use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

use aca_core::{t, tr};

use crate::agent::Agent;
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

/// The few tool input fields the summary needs (the rest, like Write's whole content, are skipped)
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

/// The text inside <name>…</name>
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&format!("</{name}>"))? + start;
    Some(s[start..end].trim())
}

/// One record line
pub fn line(t: &mut Trace, buf: &[u8], off: u64) {
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
        // A forked subagent: carries on from the parent conversation (the parent's lines weren't copied in)
        Some("fork-context-ref") => {
            let i = t.add_top(Kind::Mark, ts, false);
            t.nodes[i].label = t!("上一段在父对话里", "earlier: in the parent conversation").into();
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

/// A tool's result came back
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

/// A message from the user side: a new turn, or a command, an interrupt
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
    // Command output, notes: not a turn
    if ["<local-command-", "<bash-stdout>", "<bash-stderr>"].iter().any(|p| text.starts_with(p)) {
        return;
    }
    let (name, label) = if let Some(cmd) = tag(text, "command-name") {
        let args = tag(text, "command-args").unwrap_or("");
        (cmd.to_string(), first_line(&format!("{cmd} {args}"), LABEL))
    } else if let Some(cmd) = tag(text, "bash-input") {
        ("!".to_string(), first_line(&format!("! {cmd}"), LABEL))
    } else if text.contains("<task-notification>") {
        // A background subagent / command finished: record which one and how it ended
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

/// Another message came before anything answered the previous one (no calls, not finished): it was retyped, drop the
/// previous one
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

/// The current turn; if there is none (the record starts halfway), make one up
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
            // An error earlier, success this time: a retry succeeded, so the turn doesn't count as an error
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
            // Old records don't split 5 minutes / 1 hour; counted as 5 minutes (same as usage.rs)
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

/// Find new subagents and attach each under the Agent tool that started it (searched from the top-level record; nested
/// ones go into the parent subagent's record).
/// Those whose starting tool hasn't been read yet, or whose record isn't written yet, are tried again next time. Returns
/// true if any new ones were attached
pub fn find_subs(t: &mut Trace) -> bool {
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
    // Attach the outer level first so nested ones can find it
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

/// Attach one subagent: find the tool that started it (in the top-level record, or in the record of the parentAgentId
/// subagent), read its record
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

/// Where a subagent sits: the tools along the way that started it
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

/// A tool's one-line summary: the command for Bash, the path for file reads / writes, the term for searches, "server ·
/// tool" for MCP
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

// ---------- Content read back (laid out as text in trace_page) ----------

/// Content blocks: a string or an array of blocks
fn blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) => vec![serde_json::json!({ "type": "text", "text": s })],
        Value::Array(v) => v.clone(),
        _ => vec![],
    }
}

/// The text in the blocks; images are written as [image]
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

/// Read back the lines this node recorded
fn lines_of(t: &Trace, n: &Node) -> Vec<Value> {
    n.src.iter().filter_map(|&x| t.read_at(x)).collect()
}

/// What a call thought and said (one paragraph per block)
pub fn said(t: &Trace, n: &Node) -> (Vec<String>, Vec<String>) {
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

/// Who asked and what: a prompt is the original text, commands and background task notifications get a summary. When the
/// record starts mid-turn (the made-up turn) it's nobody
pub fn asked(t: &Trace, n: &Node) -> (Asker, String) {
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

/// What the fake reply for an API error says
pub fn fail_text(t: &Trace, n: &Node) -> String {
    n.src.first().and_then(|&x| t.read_at(x)).map(|v| text_of(&content(&v))).unwrap_or_default()
}

/// Why the cache missed (only when the record says), put into words; unknown ones are written as is
pub fn call_note(t: &Trace, n: &Node) -> Option<String> {
    let last = t.read_at(*n.src.last()?)?;
    let why = match last.pointer("/message/diagnostics/cache_miss_reason/type")?.as_str()? {
        "unavailable" => return Some(t!("缓存未命中", "cache miss").into()),
        "ttl_expired" => t!("已过期", "it expired"),
        "previous_message_not_found" => t!("找不到上一条", "the previous message wasn't cached"),
        "messages_changed" => t!("前文已变化", "earlier messages changed"),
        "system_changed" => t!("系统提示已变化", "the system prompt changed"),
        "tools_changed" => t!("工具已变化", "the tools changed"),
        "model_changed" => t!("模型已更换", "the model changed"),
        x => x,
    };
    Some(tr!("缓存未命中:{why}", "cache miss: {why}"))
}

/// The input the tool was issued with
fn tool_input(t: &Trace, n: &Node) -> Option<Value> {
    let v = t.read_at(*n.src.first()?)?;
    blocks(&content(&v)).into_iter().find(|b| b.get("id").and_then(Value::as_str) == Some(n.id.as_str()))?.get("input").cloned()
}

/// The whole line with the tool's result (toolUseResult has the changes)
fn tool_result(t: &Trace, n: &Node) -> Option<Value> {
    t.read_at(*n.src.get(1)?)
}

/// The text for this tool in the result line
fn result_text(v: &Value, id: &str) -> Option<String> {
    let b = blocks(&content(v)).into_iter().find(|b| b.get("tool_use_id").and_then(Value::as_str) == Some(id))?;
    Some(text_of(b.get("content").unwrap_or(&Value::Null)))
}

/// Tools that change files: show the changes below, not the output (the output just says "done")
const FILE_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// A tool's content: the command for Bash, the changes for file-changing tools, the file content read for Read, the
/// tidied JSON input for the rest; all have output
pub fn io(t: &Trace, n: &Node) -> Io {
    let inp = tool_input(t, n);
    let res = tool_result(t, n);
    let out = res.as_ref().and_then(|v| result_text(v, &n.id));
    let s = |k: &str| inp.as_ref().and_then(|v| v.get(k)).and_then(Value::as_str);
    let path = s("file_path").or_else(|| s("notebook_path")).unwrap_or("").to_string();
    let mut io = Io::default();
    if FILE_TOOLS.contains(&n.name.as_str()) {
        let ch = changes(&n.name, inp.as_ref(), res.as_ref());
        // e stops at the first changed line
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

/// What Edit / Write changed. If the result has a structuredPatch, use it (with context, line numbers);
/// otherwise (new files, denied, no result yet) compare the input, leaving out identical leading and trailing lines
pub fn changes(name: &str, inp: Option<&Value>, res: Option<&Value>) -> Vec<Change> {
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

/// The lines of one hunk (" context" / "-removed" / "+added"), starting at line old of the original and line new of the
/// result
pub fn hunk<'a>(mut old: u64, mut new: u64, lines: impl Iterator<Item = &'a str>) -> Vec<Change> {
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
            // "\ No newline at end of file"
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

/// Old and new text: identical leading and trailing lines are left out (line numbers unknown)
fn pair(old: &str, new: &str) -> Vec<Change> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let line = |c: char| move |l: &&str| Change { c, no: None, text: l.to_string() };
    a[head..a.len() - tail].iter().map(line('-')).chain(b[head..b.len() - tail].iter().map(line('+'))).collect()
}

/// Read's result is "line number<Tab>content": split into (line number, content); the hint that follows isn't file
/// content and is dropped
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

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures {
    use super::*;
    use crate::agent::Agent;
    use serde_json::json;
    use std::io::Write;
    use std::path::PathBuf;

    /// A record file for tests (one per test)
    pub fn file(name: &str, lines: &[Value]) -> PathBuf {
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

    pub fn at(s: u32) -> String {
        format!("2026-09-30T01:00:{:02}.{:03}Z", s / 1000, s % 1000)
    }

    pub fn prompt(uuid: &str, parent: Option<&str>, ms: u32, text: &str) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "session_id": "s1",
               "origin": {"kind": "human"}, "promptId": "p", "message": {"role": "user", "content": text}})
    }

    pub fn reply(uuid: &str, parent: &str, ms: u32, id: &str, block: Value, out: u64, stop: Option<&str>) -> Value {
        json!({"type": "assistant", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "session_id": "s1",
               "requestId": format!("req-{id}"),
               "message": {"id": id, "model": "claude-opus-5-5", "role": "assistant", "content": [block], "stop_reason": stop,
                           "usage": {"input_tokens": 10, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 1000, "output_tokens": out,
                                     "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 100}}}})
    }

    pub fn bash(id: &str, cmd: &str) -> Value {
        json!({"type": "tool_use", "id": id, "name": "Bash", "input": {"command": cmd, "description": "run it"}})
    }

    pub fn done(uuid: &str, parent: &str, ms: u32, tool: &str, err: bool) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent, "timestamp": at(ms), "sessionId": "s1", "session_id": "s1", "promptId": "p",
               "toolUseResult": {"stdout": "ok", "stderr": "", "interrupted": false},
               "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool, "content": "ok", "is_error": err}]}})
    }

    pub fn turn_end(parent: &str, ms: u32, dur: u64) -> Value {
        json!({"type": "system", "subtype": "turn_duration", "uuid": format!("td-{ms}"), "parentUuid": parent, "timestamp": at(ms),
               "sessionId": "s1", "durationMs": dur})
    }

    /// born = 0: don't look for copied lines (test record timestamps are all older than the file)
    pub fn open(name: &str, lines: &[Value]) -> Trace {
        Trace::open_born(file(name, lines), Agent::Claude, 0).unwrap()
    }

    /// A stretch for interface tests: a copied turn, a normal turn (tools + end), a compaction, a turn with an API error,
    /// a turn still running
    pub fn fixture(name: &str) -> (Trace, PathBuf) {
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

    pub fn kinds(t: &Trace, i: usize) -> Vec<Kind> {
        t.nodes[i].kids.iter().map(|&k| t.nodes[k].kind).collect()
    }

    /// A stretch that starts background subagents: the first turn starts two (an Explore, a fork), and Explore starts one
    /// more; when done, a notification starts the second turn
    pub fn with_subs(name: &str) -> (PathBuf, Vec<Value>, Vec<Value>) {
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
        // Nested: the tool that started it is in a1's record
        write(
            "a2",
            json!({"agentType": "general-purpose", "description": "细看 ui.rs", "toolUseId": "ag2", "parentAgentId": "a1", "spawnDepth": 2}),
            &[
                prompt("t1", None, 2950, "细看 ui.rs"),
                reply("t2", "t1", 3050, "y1", json!({"type": "text", "text": "ui.rs 里有 2 处"}), 60, Some("end_turn")),
            ],
        );
        // Forked: the first line is fork-context-ref
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
}

#[cfg(test)]
mod tests {
    use super::fixtures::{at, bash, done, file, kinds, open, prompt, reply, turn_end};
    use super::*;
    use crate::agent::Agent;
    use serde_json::json;
    use std::io::Write;

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
        // Counted from the prompt line to the last block
        assert_eq!((n.t1 - n.t0, n.tok, n.stop.as_str(), n.label.as_str()), (8400, [10, 0, 100, 1000, 640], "tool_use", "先跑单测"));
        let tool = &t.nodes[n.kids[0]];
        assert_eq!(
            (tool.name.as_str(), tool.label.as_str(), tool.t1 - tool.t0, tool.state),
            ("Bash", "cargo test --release", 12_300, St::Ok)
        );
        // The second call is counted from the tool result line
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
                // Block 2 and block 1's result both hang under block 1 (parentUuid forks), still the same call
                done("r1", "a1", 1200, "t1", false),
                done("r2", "a2", 1300, "t2", true),
            ],
        );
        let call = t.nodes[t.roots[0]].kids[0];
        assert_eq!(t.nodes[call].kids.len(), 2);
        let tools: Vec<(String, St)> = t.nodes[call].kids.iter().map(|&k| (t.nodes[k].label.clone(), t.nodes[k].state)).collect();
        // No folder recorded for it: written as is
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
        // The error doesn't count as a call
        assert_eq!(t.sum(a).calls, 1);
        assert_eq!(t.nodes[b].state, St::Stopped);
        let tool = &t.nodes[t.nodes[t.nodes[b].kids[0]].kids[0]];
        assert_eq!((tool.state, tool.t1 - tool.t0), (St::Stopped, 4000));
    }

    #[test]
    fn copied_history_folds_into_one_row_and_is_not_counted() {
        let lines = vec![
            // A session-start hook attachment a few seconds older than the file doesn't count as copied
            json!({"type": "attachment", "uuid": "h0", "parentUuid": null, "timestamp": at(0), "sessionId": "s1", "attachment": {"type": "hook_success"}}),
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb", "parentUuid": null, "timestamp": at(0), "sessionId": "s1",
                   "compactMetadata": {"trigger": "manual", "preTokens": 182_000, "postTokens": 21_000, "durationMs": 38_000}}),
            prompt("o1", Some("cb"), 100, "以前的"),
            reply("o2", "o1", 1000, "m0", json!({"type": "text", "text": "好"}), 50, Some("end_turn")),
            // The file was created at 4 s: what follows is this conversation's own; lines written after a resume have a
            // different session_id and still count
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
    fn edits_show_the_real_patch_with_line_numbers_or_else_the_changed_lines_only() {
        let rows = |ch: Vec<Change>| ch.into_iter().map(|c| (c.c, c.no, c.text)).collect::<Vec<_>>();
        let s = |x: &str| x.to_string();
        let res = json!({"toolUseResult": {"structuredPatch": [
            {"oldStart": 7, "newStart": 7, "lines": [" a", "-b", "+c", "+d", " e"]},
            {"oldStart": 40, "newStart": 41, "lines": ["-f", "\\ No newline at end of file"]}]}});
        // Removed lines get the original line number, added and context lines the new one; a '~' line between two hunks
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
        // No result (denied, not back yet): compare the input, identical leading and trailing lines left out, no line numbers
        let inp = json!({"old_string": "x\nold\ny", "new_string": "x\nnew\nnewer\ny"});
        assert_eq!(rows(changes("Edit", Some(&inp), None)), [('-', None, s("old")), ('+', None, s("new")), ('+', None, s("newer"))]);
        // A new file: every line is added, starting at line 1
        let new_file = changes("Write", Some(&json!({"content": "l1\nl2"})), Some(&json!({"toolUseResult": {"structuredPatch": []}})));
        assert_eq!(rows(new_file), [('+', Some(1), s("l1")), ('+', Some(2), s("l2"))]);
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
