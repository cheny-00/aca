//! hook 事件 → 会话状态。这就是 aca 的 **hook 协议**:Claude Code 和 Codex 的 hook 天生是这个形状
//! (事件名、`session_id`、`transcript_path`、`prompt`、`last_assistant_message`、`tool_name` / `tool_input` …),
//! 别的 agent 写个转发按这个形状报就行。
//! 哪家没有的事件(Claude 没有 Interrupt)自然走不到那一支。问你的题 Claude 是 AskUserQuestion、Codex 是 request_user_input,
//! 载荷是同一个形状(`tool_input.questions[0].question`、`options[].label`)。
//! 协议比 Claude 多一个字段:`title`,任何事件上都能带,是 agent 自己起的标题。

use serde_json::{Value, json};

use super::Events;
use crate::runtime::{Run, clip};
use crate::t;

/// 工具调用给人看的一句:命令 / 文件 / 网址,哪个有用哪个
fn tool_brief(v: &Value) -> String {
    let input = &v["tool_input"];
    let what = ["command", "file_path", "url", "pattern", "description"].iter().find_map(|k| input[*k].as_str()).unwrap_or("");
    let name = v["tool_name"].as_str().unwrap_or("");
    clip(&if what.is_empty() { name.to_string() } else { format!("{name}:{what}") }, 200)
}

/// 进「在等你」;同一句已经报过就不再发事件。`kind`:choice / permission(见 [`Run::ask_kind`])
fn waiting(r: &mut Run, ask: String, kind: &str, out: &mut Events) {
    let already = r.status == "waiting" && r.ask == ask;
    r.status = "waiting".into();
    r.ask = ask;
    if r.ask_kind != kind {
        r.ask_kind = kind.to_string();
        r.ask_opts.clear();
        r.ask_multi = false;
    }
    if !already {
        out.push(("agent.waiting", json!({ "ask": r.ask, "kind": r.ask_kind, "options": r.ask_opts })));
    }
}

/// AskUserQuestion / request_user_input 的题:记下第一题的选项(`aca answer` 按数字答),一次问好几题或者能多选的标出来
fn choice(r: &mut Run, input: &Value) {
    let qs = input["questions"].as_array().map(Vec::as_slice).unwrap_or_default();
    let first = qs.first().unwrap_or(&Value::Null);
    r.ask_kind = "choice".into();
    r.ask_opts = first["options"]
        .as_array()
        .map(|o| o.iter().map(|x| clip(x["label"].as_str().or(x.as_str()).unwrap_or(""), 80)).collect())
        .unwrap_or_default();
    r.ask_multi = qs.len() > 1 || first["multiSelect"].as_bool() == Some(true);
}

/// Claude 的 `StopFailure`:`error` 是错误的类型,屏幕上那句原话在 `last_assistant_message`(官方文档)。
/// 类型说得清的照类型;订阅额度用完报的也是 `rate_limit`,和真的限流分不开,再看原话
fn claude_kind(kind: &str, text: &str) -> Option<&'static str> {
    Some(match kind {
        "authentication_failed" | "oauth_org_not_allowed" | "account_on_hold" | "billing_error" | "cloud_credential_error" => "account",
        "rate_limit" if error_kind(text) == "account" => "account",
        "rate_limit" | "overloaded" | "server_error" => "transient",
        "invalid_request" | "model_not_found" | "max_output_tokens" => "fatal",
        _ => return None,
    })
}

/// 一轮失败是哪一类,看错误原文(报了类别的那家用它报的,见 Codex 的 `unreported_end`):
/// - transient:限流、过载、断流、5xx、超时,过一会儿再说一句「继续」多半就好了
/// - account:额度用完、没登录、欠费,得人去处理
/// - fatal:别的(上下文爆了、请求不对、内容策略),再试也一样
pub fn error_kind(text: &str) -> &'static str {
    let t = text.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    // 订阅额度用完:Claude 屏幕上是「You've hit your session limit · resets 2:40pm」(2.1.29x),也有 weekly、5-hour 的
    let plan = has(&["hit your", "session limit", "weekly limit", "-hour limit", "opus limit", "out of extra usage"]);
    if plan || has(&["401", "403", "unauthorized", "authentication", "usage limit", "quota", "credit", "billing", "payment", "budget"]) {
        "account"
    } else if has(&[
        "429",
        "rate limit",
        "rate_limit",
        "overloaded",
        "529",
        "500",
        "502",
        "503",
        "504",
        "timeout",
        "timed out",
        "stream",
        "disconnected",
        "connection",
        "capacity",
        "try again",
    ]) {
        "transient"
    } else {
        "fatal"
    }
}

/// 它用 AskUserQuestion / request_user_input 问你:第一题的那句就是「它问的」
fn question(r: &mut Run, v: &Value, out: &mut Events) {
    let q = v["tool_input"]["questions"][0]["question"].as_str().unwrap_or("").to_string();
    // 选项先记上:「在等你」的那个事件里带着它们
    choice(r, &v["tool_input"]);
    waiting(r, clip(&q, 200), "choice", out);
}

/// 一个事件 → 状态怎么变、要发哪些事件。纯函数
pub fn apply(r: &mut Run, v: &Value, now: u64) -> Events {
    let s = |k: &str| v[k].as_str().unwrap_or("");
    let ev = s("hook_event_name");
    let mut out: Events = vec![];
    let from = r.conv.clone();
    r.event = ev.to_string();
    r.ts = now;
    r.note_conv(s("session_id"));
    if !s("transcript_path").is_empty() {
        r.transcript = s("transcript_path").to_string();
    }
    if !s("cwd").is_empty() {
        r.cwd = s("cwd").to_string();
    }
    if !s("title").trim().is_empty() {
        r.hook_title = clip(s("title").trim(), 200);
    }
    if r.conv != from && !from.is_empty() {
        out.push(("conversation.changed", json!({ "conv": r.conv, "from": from })));
    }
    // 压完(压完接着干的 SessionStart 也算)、这一轮结束、退出:不在压了。状态照下面各自的规矩走
    if matches!(ev, "PostCompact" | "SessionStart" | "Stop" | "StopFailure" | "SessionEnd") {
        r.compacting = 0;
    }
    match ev {
        // 压缩完接着干的那次 SessionStart 不是「空下来了」
        "SessionStart" if s("source") != "compact" => {
            r.status = "idle".into();
            r.clear_ask();
            r.clear_error();
        }
        "UserPromptSubmit" => {
            r.status = "running".into();
            r.turn = now;
            r.prompt = clip(s("prompt"), 400);
            r.clear_ask();
            r.clear_error();
            out.push(("turn.started", json!({ "prompt": r.prompt })));
        }
        // 只挂了问你的那个工具(Claude 的 AskUserQuestion、Codex 的 request_user_input):它在问你
        "PreToolUse" => question(r, v, &mut out),
        "PostToolUse" if r.status == "waiting" => {
            r.status = "running".into();
            r.clear_ask();
        }
        // 要确认的权限模式下,AskUserQuestion 还会再报一个 PermissionRequest:屏幕上是同一道选择题,不是权限框(2.1.295 实测)
        "PermissionRequest" if s("tool_name") == "AskUserQuestion" => question(r, v, &mut out),
        "PermissionRequest" => waiting(r, tool_brief(v), "permission", &mut out),
        "Notification" if matches!(s("notification_type"), "permission_prompt" | "elicitation_dialog") => {
            // 权限框通常已经由 PermissionRequest 报过,问的那句、是哪种框都留它的
            let known = r.status == "waiting" && !r.ask.is_empty();
            let ask = if known { r.ask.clone() } else { clip(s("message"), 200) };
            let kind = match (known, s("notification_type")) {
                (true, _) => r.ask_kind.clone(),
                (false, "permission_prompt") => "permission".to_string(),
                _ => String::new(),
            };
            waiting(r, ask, &kind, &mut out);
        }
        "Stop" => {
            r.status = "done".into();
            r.done = now;
            r.last = clip(s("last_assistant_message"), 4000);
            r.bg = v["background_tasks"].as_array().map_or(0, |a| a.len()) as u32;
            r.clear_ask();
            let secs = if r.turn == 0 { 0 } else { now.saturating_sub(r.turn) };
            out.push(("turn.done", json!({ "last": r.last, "bg": r.bg, "secs": secs })));
        }
        "StopFailure" => {
            r.status = "error".into();
            r.done = now;
            // 原话在 last_assistant_message 里的(Claude),error 只是类型
            let said = v["last_assistant_message"].as_str().filter(|t| !t.trim().is_empty());
            let kind = v["error"].as_str().unwrap_or("");
            r.error = clip(said.or(v["error"].as_str()).or(v["message"].as_str()).unwrap_or(t!("这一轮失败了", "this turn failed")), 400);
            r.clear_ask();
            r.error_kind = match v["error_kind"].as_str() {
                Some(k) => k.to_string(),
                None => said.and_then(|t| claude_kind(kind, t)).unwrap_or_else(|| error_kind(&r.error)).to_string(),
            };
            out.push(("turn.failed", json!({ "error": r.error, "kind": r.error_kind })));
        }
        // 你按 Esc 打断的(Codex 会报,Claude 不报):人就在跟前,不算「跑完没看」
        "Interrupt" => {
            r.status = "idle".into();
            r.clear_ask();
            out.push(("turn.interrupted", json!({})));
        }
        // 开始压缩:状态不动(闲着自己压的那种前后都没有一轮,一直是「跑完」),只记下正在压
        "PreCompact" => r.compacting = now,
        // 手动 /compact 结束时不发 Stop
        "PostCompact" if s("trigger") == "manual" && r.status == "running" => {
            r.status = "done".into();
            r.done = now;
        }
        "SessionEnd" => {
            r.status = "ended".into();
            r.clear_ask();
            // /clear、/resume 也发 SessionEnd,紧跟着就是新对话的 SessionStart:那不是退出
            if !matches!(s("reason"), "clear" | "resume") {
                out.push(("agent.exited", json!({ "reason": s("reason") })));
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(r: &mut Run, json: &str, now: u64) -> Vec<&'static str> {
        apply(r, &serde_json::from_str(json).unwrap(), now).into_iter().map(|e| e.0).collect()
    }

    #[test]
    fn a_turn_goes_running_then_done_and_keeps_the_last_reply() {
        let mut r = Run::default();
        assert!(
            ev(
                &mut r,
                r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"c1","transcript_path":"/t/c1.jsonl","cwd":"/p"}"#,
                10
            )
            .is_empty()
        );
        assert_eq!((r.status.as_str(), r.conv.as_str(), r.transcript.as_str()), ("idle", "c1", "/t/c1.jsonl"));
        assert_eq!(ev(&mut r, r#"{"hook_event_name":"UserPromptSubmit","session_id":"c1","prompt":"修一下登录"}"#, 20), ["turn.started"]);
        assert_eq!((r.status.as_str(), r.turn, r.prompt.as_str()), ("running", 20, "修一下登录"));
        let stop = r#"{"hook_event_name":"Stop","session_id":"c1","last_assistant_message":"好了","background_tasks":[{"id":1}]}"#;
        let out = apply(&mut r, &serde_json::from_str(stop).unwrap(), 65);
        assert_eq!((r.status.as_str(), r.done, r.last.as_str(), r.bg), ("done", 65, "好了", 1));
        assert_eq!((out[0].0, out[0].1["secs"].as_u64()), ("turn.done", Some(45)));
    }

    #[test]
    fn asking_sticks_until_answered_and_is_announced_once() {
        let mut r = Run { status: "running".into(), ..Default::default() };
        assert_eq!(
            ev(&mut r, r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#, 1),
            ["agent.waiting"]
        );
        assert_eq!((r.status.as_str(), r.ask.as_str()), ("waiting", "Bash:rm -rf build"));
        // 紧跟着的通知说的是同一件事
        assert!(
            ev(
                &mut r,
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}"#,
                2
            )
            .is_empty()
        );
        assert_eq!(r.ask, "Bash:rm -rf build");
        // 空闲提醒不算在等你
        let mut idle = Run { status: "done".into(), ..Default::default() };
        assert!(
            ev(&mut idle, r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"waiting for your input"}"#, 3)
                .is_empty()
        );
        assert_eq!(idle.status, "done");
        // 它问你的问题
        let mut q = Run { status: "running".into(), ..Default::default() };
        ev(
            &mut q,
            r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"用哪个库?"}]}}"#,
            4,
        );
        assert_eq!((q.status.as_str(), q.ask.as_str()), ("waiting", "用哪个库?"));
        ev(&mut q, r#"{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion"}"#, 5);
        assert_eq!((q.status.as_str(), q.ask.as_str()), ("running", ""));
    }

    #[test]
    fn what_kind_of_question_it_is_and_its_options_are_kept_for_answering() {
        // 2.1.294 的 AskUserQuestion:选项在 questions[0].options 里,「在等你」的事件带着它们
        let mut r = Run { status: "running".into(), ..Default::default() };
        let pre = r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"选 A 还是 B?","header":"测试","multiSelect":false,"options":[{"label":"A","description":"a"},{"label":"B","description":"b"}]}]}}"#;
        let out = apply(&mut r, &serde_json::from_str(pre).unwrap(), 1);
        assert_eq!(
            (r.ask_kind.as_str(), r.ask_opts.as_slice(), r.ask_multi),
            ("choice", ["A".to_string(), "B".to_string()].as_slice(), false)
        );
        assert_eq!(out[0].1["options"], json!(["A", "B"]));
        // 要确认的模式下跟着来的 PermissionRequest:还是这道选择题,也不再报一次
        let perm = pre.replace("PreToolUse", "PermissionRequest");
        assert!(apply(&mut r, &serde_json::from_str(&perm).unwrap(), 2).is_empty());
        assert_eq!((r.ask.as_str(), r.ask_kind.as_str(), r.ask_opts.len()), ("选 A 还是 B?", "choice", 2));
        // 回答完:一起清掉
        ev(&mut r, r#"{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion"}"#, 2);
        assert!(r.ask_kind.is_empty() && r.ask_opts.is_empty());
        // 一次问两题:按一个数字答不了
        let two = r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"一?","options":[{"label":"x"}]},{"question":"二?","options":[{"label":"y"}]}]}}"#;
        ev(&mut r, two, 3);
        assert!(r.ask_multi);
        // 权限框:后面跟着的通知不改它是哪种框
        let mut p = Run { status: "running".into(), ..Default::default() };
        ev(&mut p, r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"}}"#, 1);
        ev(&mut p, r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"needs permission"}"#, 2);
        assert_eq!((p.ask_kind.as_str(), p.ask_opts.len()), ("permission", 0));
        // 只有通知、没有 PermissionRequest 的(MCP 要你填东西):看不出是哪种
        let mut e = Run::default();
        ev(&mut e, r#"{"hook_event_name":"Notification","notification_type":"elicitation_dialog","message":"填一下"}"#, 1);
        assert_eq!((e.status.as_str(), e.ask_kind.as_str()), ("waiting", ""));
    }

    #[test]
    fn clear_changes_the_conversation_and_compaction_does_not_look_idle() {
        let mut r = Run { status: "running".into(), ..Default::default() };
        r.note_conv("c1");
        assert!(ev(&mut r, r#"{"hook_event_name":"SessionStart","source":"compact","session_id":"c1"}"#, 1).is_empty());
        assert_eq!(r.status, "running");
        assert_eq!(
            ev(&mut r, r#"{"hook_event_name":"SessionStart","source":"clear","session_id":"c2","transcript_path":"/t/c2.jsonl"}"#, 2),
            ["conversation.changed"]
        );
        assert_eq!((r.status.as_str(), r.conv.as_str(), r.convs.len()), ("idle", "c2", 2));
        // 手动压缩结束没有 Stop
        r.status = "running".into();
        ev(&mut r, r#"{"hook_event_name":"PreCompact","trigger":"manual","session_id":"c2"}"#, 3);
        assert_eq!((r.status.as_str(), r.compacting), ("running", 3));
        ev(&mut r, r#"{"hook_event_name":"PostCompact","trigger":"manual","session_id":"c2"}"#, 3);
        assert_eq!((r.status.as_str(), r.done, r.compacting), ("done", 3, 0));
        // 闲着快一小时它自己压(trigger 是 auto):前后都没有一轮,一直是跑完,只是记着正在压;压完接着的 SessionStart 也清掉
        ev(&mut r, r#"{"hook_event_name":"PreCompact","trigger":"auto","session_id":"c2"}"#, 5);
        assert_eq!((r.status.as_str(), r.compacting), ("done", 5));
        ev(&mut r, r#"{"hook_event_name":"PostCompact","trigger":"auto","session_id":"c2"}"#, 6);
        assert_eq!((r.status.as_str(), r.done, r.compacting), ("done", 3, 0));
        ev(&mut r, r#"{"hook_event_name":"PreCompact","trigger":"auto","session_id":"c2"}"#, 7);
        ev(&mut r, r#"{"hook_event_name":"SessionStart","source":"compact","session_id":"c2"}"#, 8);
        assert_eq!((r.status.as_str(), r.compacting), ("done", 0));
        assert!(ev(&mut r, r#"{"hook_event_name":"SessionEnd","reason":"clear","session_id":"c2"}"#, 4).is_empty());
        assert_eq!(ev(&mut r, r#"{"hook_event_name":"SessionEnd","reason":"prompt_input_exit","session_id":"c2"}"#, 4), ["agent.exited"]);
        assert_eq!(r.status, "ended");
    }

    #[test]
    fn an_interrupted_turn_is_idle_not_unread() {
        // Codex 0.159 实际发来的两条(去掉了路径)
        let mut r = Run::default();
        ev(
            &mut r,
            r#"{"session_id":"01a0","turn_id":"t1","hook_event_name":"UserPromptSubmit","model":"gpt-6.1-sol","permission_mode":"bypassPermissions","prompt":"写一篇长文"}"#,
            10,
        );
        assert_eq!(ev(&mut r, r#"{"session_id":"01a0","turn_id":"t1","hook_event_name":"Interrupt"}"#, 13), ["turn.interrupted"]);
        assert_eq!((r.status.as_str(), r.done, r.conv.as_str()), ("idle", 0, "01a0"));
    }

    #[test]
    fn codex_asks_with_request_user_input_and_reports_its_compaction_like_claude() {
        // Codex 0.163 实际发来的(去掉了路径、id):Plan 模式里问你一道题,答完;手动 /compact
        let mut r = Run { status: "running".into(), ..Default::default() };
        let pre = r#"{"session_id":"01a1","hook_event_name":"PreToolUse","permission_mode":"default","tool_name":"request_user_input","tool_input":{"questions":[{"header":"Choice","id":"choice","question":"Choose option A or option B.","options":[{"label":"Option A","description":"Select option A."},{"label":"Option B","description":"Select option B."}]}]},"tool_use_id":"call_05"}"#;
        assert_eq!(ev(&mut r, pre, 1), ["agent.waiting"]);
        assert_eq!(
            (r.status.as_str(), r.ask.as_str(), r.ask_kind.as_str(), r.ask_opts.as_slice(), r.ask_multi),
            ("waiting", "Choose option A or option B.", "choice", ["Option A".to_string(), "Option B".to_string()].as_slice(), false)
        );
        let post = r#"{"session_id":"01a1","hook_event_name":"PostToolUse","tool_name":"request_user_input","tool_response":"{\"answers\":{\"choice\":{\"answers\":[\"Option B\"]}}}"}"#;
        ev(&mut r, post, 2);
        assert_eq!((r.status.as_str(), r.ask_kind.as_str()), ("running", ""));
        // 权限框:Bash 加一句为什么要
        let perm = r#"{"session_id":"01a1","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch made.txt","description":"Allow creating made.txt in the current directory?"}}"#;
        assert_eq!(ev(&mut r, perm, 3), ["agent.waiting"]);
        assert_eq!((r.ask.as_str(), r.ask_kind.as_str()), ("Bash:touch made.txt", "permission"));
        let mut idle = Run { status: "done".into(), done: 4, ..Default::default() };
        ev(&mut idle, r#"{"session_id":"01a1","hook_event_name":"PreCompact","trigger":"manual"}"#, 5);
        assert_eq!((idle.status.as_str(), idle.compacting), ("done", 5));
        ev(&mut idle, r#"{"session_id":"01a1","hook_event_name":"PostCompact","trigger":"manual"}"#, 9);
        assert_eq!((idle.status.as_str(), idle.done, idle.compacting), ("done", 4, 0));
    }

    #[test]
    fn a_failed_turn_says_whether_trying_again_could_help() {
        assert_eq!(error_kind("API Error: 529 Overloaded"), "transient");
        assert_eq!(error_kind("stream disconnected before completion"), "transient");
        assert_eq!(error_kind("You've hit your usage limit. Upgrade or try later"), "account");
        assert_eq!(error_kind("You've hit your session limit · resets 2:40pm (Asia/Taipei)"), "account");
        assert_eq!(error_kind("Weekly limit reached ∙ resets Oct 20"), "account");
        assert_eq!(error_kind("API Error: Rate limit reached"), "transient");
        assert_eq!(error_kind("401 Unauthorized"), "account");
        assert_eq!(error_kind("prompt is too long"), "fatal");
        // 报了类别的(Codex 那边补的)用它报的;没报的看原文
        let mut r = Run { status: "running".into(), ..Default::default() };
        let out = apply(&mut r, &json!({"hook_event_name": "StopFailure", "error": "boom", "error_kind": "transient"}), 5);
        assert_eq!((r.status.as_str(), r.error_kind.as_str(), out[0].1["kind"].as_str()), ("error", "transient", Some("transient")));
        apply(&mut r, &json!({"hook_event_name": "StopFailure", "error": "rate limit reached"}), 6);
        assert_eq!(r.error_kind, "transient");
        // Claude:error 是类型,原话在 last_assistant_message。订阅额度用完也报 rate_limit,看原话分出来
        let claude = |kind: &str, said: &str| {
            let mut r = Run::default();
            apply(&mut r, &json!({"hook_event_name": "StopFailure", "error": kind, "last_assistant_message": said}), 6);
            (r.error, r.error_kind)
        };
        assert_eq!(
            claude("rate_limit", "You've hit your session limit · resets 2:40pm (Asia/Taipei)"),
            ("You've hit your session limit · resets 2:40pm (Asia/Taipei)".to_string(), "account".to_string())
        );
        assert_eq!(claude("rate_limit", "API Error: Rate limit reached").1, "transient");
        assert_eq!(claude("authentication_failed", "API Error: 401").1, "account");
        assert_eq!(claude("max_output_tokens", "API Error: output too long").1, "fatal");
        // 下一轮开始:原文和类别一起清掉
        apply(&mut r, &json!({"hook_event_name": "UserPromptSubmit", "prompt": "继续"}), 7);
        assert!(r.error.is_empty() && r.error_kind.is_empty());
    }

    #[test]
    fn any_event_can_carry_the_agents_own_title() {
        let mut r = Run::default();
        ev(&mut r, r#"{"hook_event_name":"Stop","session_id":"p1","title":" 修登录 ","last_assistant_message":"好"}"#, 1);
        assert_eq!((r.hook_title.as_str(), r.status.as_str()), ("修登录", "done"));
        // 没带就不动
        ev(&mut r, r#"{"hook_event_name":"UserPromptSubmit","session_id":"p1","prompt":"再来"}"#, 2);
        assert_eq!(r.hook_title, "修登录");
    }
}
