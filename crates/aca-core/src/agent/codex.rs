//! Codex。hook 用 `-c hooks.<事件>=…` 在启动时带进去(它的 hook 和 Claude 是同一套形状,状态处理共用);
//! 对话 id 等 hook 报上来;标题它自己起、写在窗格标题里(`<标题> | <目录>`)。
//!
//! 两件要知道的事(0.159 上实测):
//! - **信任**:Codex 对没见过的 hook 会在启动时问一次「Hooks need review」。选「Trust all」后它把信任记在
//!   那个 `CODEX_HOME` 的 config.toml 里,以后同一条 hook 命令不再问 —— 每个账号点一次,aca 的程序路径变了要再点一次。
//!   不想点:`[tools.codex] hook_trust = "bypass"`,启动时加 `--dangerously-bypass-hook-trust`
//!   (这一次启动里所有开着但没审过的 hook 都直接跑,包括别的插件的)。
//! - **带了 `-c` 就是单机模式**:Codex 不走它那个共用的后台 server(`codex agents` 里看不到这个会话)。
//!
//! 不用老的 `notify`:它只有「一轮结束」,而且 Codex 内部起标题的那次调用也会触发它,报上来的是另一个对话 id。
//!
//! 别的会话发来的消息排进它自己的队列(`codex queue`,0.152 起有),不往输入框里打字。0.159.3 上实测:
//! 带着 `-c` 起的(单机模式)窗口也每 10 秒看一次队列,空着就开一轮,在跑就等这一轮结束马上接上,
//! 你打了一半的草稿不动;按 Esc 打断后排着的不自己开跑,等下一轮正常跑完;退出了,resume 起来马上送。

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::{Agent, How, Live};
use crate::config::Tool;
use crate::paths::Res;
use crate::runtime::Run;
use crate::screen::Seen;
use crate::store::Session;
use crate::tr;

pub struct Codex;

const EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "Stop", "Interrupt", "PermissionRequest", "SessionEnd"];

/// 启动时的几道关(更新、信任目录、审 hook)和运行中的确认框
static ASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)enter (?:to )?(?:confirm|continue|submit)|esc to cancel|allow command\?|\[y/n\]").unwrap());
/// 「• Working (1m 05s • esc to interrupt)」
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\((?:(\d+)h )?(?:(\d+)m )?(\d+)s • [^)]*to interrupt\)").unwrap());
const SPINNER: &str = "⠁⠂⠄⡀⢀⠠⠐⠈⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// 一个事件的 `-c` 值:TOML 写的一条 hook,`hooks.Stop=[{ hooks = [{ type = "command", command = "…" }] }]`
fn hook_flag(event: &str, aca: &str) -> String {
    let mut handler = toml::Table::new();
    handler.insert("type".into(), "command".into());
    handler.insert("command".into(), format!("{} hook codex", super::sh_quote(aca)).into());
    let mut group = toml::Table::new();
    group.insert("hooks".into(), toml::Value::Array(vec![handler.into()]));
    format!("hooks.{event}={}", toml::Value::Array(vec![group.into()]))
}

fn plan(tool: &Tool, s: &Session, how: &How, aca: &str) -> Vec<String> {
    let mut argv = super::base(tool, "codex");
    match how {
        How::Fresh => {}
        How::Resume(conv) => argv.extend(["resume".to_string(), conv.clone()]),
        How::Fork(from) => argv.extend(["fork".to_string(), from.clone()]),
    }
    for ev in EVENTS {
        argv.extend(["-c".to_string(), hook_flag(ev, aca)]);
    }
    if tool.hook_trust == "bypass" {
        argv.push("--dangerously-bypass-hook-trust".into());
    }
    // 开着的插件给它的:MCP 服务(Codex 给 MCP 子进程的环境只有白名单,会话 id 写进 env),aca 备好的插件副本
    let mut env =
        vec![("ACA_SESSION_ID".to_string(), s.id.clone()), ("ACA_TOOL".into(), s.tool.clone()), ("ACA_BIN".into(), aca.to_string())];
    for k in ["ACA_HOME", "ACA_TMUX_SOCKET"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v));
        }
    }
    for (name, args) in &tool.mcp {
        argv.extend(["-c".to_string(), crate::plugin::mount::codex_mcp_flag(name, aca, args, &env)]);
    }
    for p in &tool.codex_plugins {
        argv.extend(["-c".to_string(), format!("plugins.{p}@aca.enabled=true")]);
    }
    argv.extend(super::extra(tool, s));
    argv
}

/// `-c` 后面这个是不是 aca 带进去的(hook、插件、插件的 MCP 服务)
fn ours(v: &str) -> bool {
    v.starts_with("hooks.") || (v.starts_with("plugins.") && v.contains("@aca.")) || v.starts_with("mcp_servers.aca")
}

/// 接管时不照搬的:开头「接哪段对话」的子命令(`resume <id>` / `fork <id>`)、aca 自己带进去的 hook 和跳过信任的那个开关
/// (起的时候会照配置重新拼)。`first` = 这是程序名后面的第一个词
fn not_carried(rest: &[String], first: bool) -> usize {
    match rest[0].as_str() {
        "resume" | "fork" if first => 1 + usize::from(rest.get(1).is_some_and(|v| !v.starts_with('-'))),
        "--dangerously-bypass-hook-trust" => 1,
        // 只有 resume / fork 认的开关:子命令不搬,它们也不能留
        "--last" | "--all" => 1,
        "-c" | "--config" if rest.get(1).is_some_and(|v| ours(v)) => 2,
        _ => 0,
    }
}

const MODEL: &[&str] = &["-m", "--model"];

/// `dir` 往下最多 `depth` 层里,名字是 `rollout-…<tail>` 的那个文件
fn rollout_in(dir: &Path, tail: &str, depth: u32) -> Option<PathBuf> {
    let mut dirs = vec![];
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let kind = e.file_type().ok()?;
        if kind.is_file() && name.starts_with("rollout-") && name.ends_with(tail) {
            return Some(e.path());
        }
        if kind.is_dir() && depth > 1 {
            dirs.push(e.path());
        }
    }
    // 新的日子在后面:倒着找,刚说过话的那段多半在最近的目录里
    dirs.sort();
    dirs.iter().rev().find_map(|d| rollout_in(d, tail, depth - 1))
}

/// 记录里最后一轮用的模型(`turn_context` 里记着)
fn last_model(log: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(log).ok()?;
    let mut model = None;
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        if !line.contains("\"turn_context\"") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if v["type"] == "turn_context"
            && let Some(m) = v["payload"]["model"].as_str().filter(|m| !m.is_empty())
        {
            model = Some(m.to_string());
        }
    }
    model
}

/// 一个账号默认用什么:config.toml 的 `model`、`model_provider`(没写是 openai);
/// 选了 profile 的(参数里的 `--profile` / `-p`,或者 config.toml 的 `profile`)先看 `[profiles.<它>]` 里的
struct Defaults {
    model: Option<String>,
    provider: String,
}

fn defaults(home: &Path, args: &[String]) -> Defaults {
    let t: toml::Table = std::fs::read_to_string(home.join("config.toml")).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let named = args.iter().position(|a| a == "--profile" || a == "-p").and_then(|i| args.get(i + 1)).cloned();
    let profile = named.or_else(|| t.get("profile").and_then(|v| v.as_str()).map(String::from));
    let section = profile.and_then(|p| t.get("profiles")?.get(&p)?.as_table().cloned()).unwrap_or_default();
    let get = |k: &str| section.get(k).or_else(|| t.get(k)).and_then(|v| v.as_str()).map(String::from);
    Defaults { model: get("model"), provider: get("model_provider").unwrap_or_else(|| "openai".into()) }
}

/// 文件末尾这么多字节,从下一个整行开始
fn tail(path: &str, n: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let start = f.metadata().ok()?.len().saturating_sub(n);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if start == 0 { text } else { text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default() })
}

/// 记录里的最后一轮已经结束了的:替它补的那个事件,带着结束的时刻 `at`。还在跑、被打断的是 None。
/// - 带着 `error` 结束:`StopFailure`(原文和类别)
/// - 正常结束:`Stop`(最后一条回复)。没写结束时刻的不补:分不出是不是上一轮的
///
/// `since` = hook 记的这一轮开始的时刻:比它早结束的是上一轮的(新的一轮刚开始、记录里还没写上),不算
/// 记录里最后一轮跑完时带的那条回复(`task_complete` 的 `last_agent_message`)
fn last_reply(text: &str) -> Option<String> {
    text.lines().rev().filter(|l| l.contains("\"task_complete\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        let reply = v["payload"]["last_agent_message"].as_str()?;
        (!reply.trim().is_empty()).then(|| reply.to_string())
    })
}

fn last_turn_end(text: &str, since: u64) -> Option<serde_json::Value> {
    let ends = |l: &&str| ["\"task_started\"", "\"task_complete\"", "\"turn_aborted\""].iter().any(|k| l.contains(k));
    let last: serde_json::Value = text.lines().rev().filter(ends).find_map(|l| serde_json::from_str(l).ok())?;
    let p = &last["payload"];
    let at = p["completed_at"].as_u64();
    if p["type"] != "task_complete" || at.is_some_and(|t| t < since) {
        return None;
    }
    if p["error"].is_null() {
        let reply = p["last_agent_message"].as_str().unwrap_or("");
        return Some(serde_json::json!({ "hook_event_name": "Stop", "last_assistant_message": reply, "at": at? }));
    }
    let message = p["error"]["message"].as_str().filter(|m| !m.trim().is_empty()).unwrap_or("这一轮出错了");
    // 类别:没带数据的写成字符串("server_overloaded"),带数据的写成只有一个键的对象({"http_connection_failed": {…}})
    let info = &p["error"]["codex_error_info"];
    let key = info.as_str().map(str::to_string).or_else(|| info.as_object()?.keys().next().cloned()).unwrap_or_default();
    // 连接、流出错的那几类带着 HTTP 状态码:看码(模型名写错是 404,再试也一样)
    let status = info[key.as_str()]["http_status_code"].as_u64();
    let kind = match (key.as_str(), status) {
        (_, Some(401 | 403)) => "account",
        (_, Some(408 | 429)) => "transient",
        (_, Some(400..=499)) => "fatal",
        (
            "rate_limit_exceeded"
            | "server_overloaded"
            | "flex_unavailable"
            | "http_connection_failed"
            | "response_stream_connection_failed"
            | "response_stream_disconnected"
            | "response_too_many_failed_attempts"
            | "internal_server_error",
            _,
        ) => "transient",
        ("usage_limit_exceeded" | "unauthorized" | "session_budget_exceeded", _) => "account",
        ("" | "other", _) => super::lifecycle::error_kind(message),
        _ => "fatal",
    };
    Some(serde_json::json!({ "hook_event_name": "StopFailure", "error": message, "error_kind": kind, "codex_error": key, "at": at }))
}

/// 窗格标题 `⠴ 修登录 | api` → `修登录`;还没起标题时只有目录名,不算
fn pane_title(title: &str) -> Option<String> {
    // 干活时前面是转圈的字符,标题还没起好时那个位置也是一个(`⠸ ⠸ | api`)
    let t = title.trim_start_matches(|c: char| c.is_whitespace() || SPINNER.contains(c));
    let (name, _dir) = t.rsplit_once(" | ")?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

impl Agent for Codex {
    fn name(&self) -> &str {
        "codex"
    }

    fn home_var(&self) -> Option<&str> {
        Some("CODEX_HOME")
    }

    fn default_home(&self) -> Option<&str> {
        Some("~/.codex")
    }

    /// 对话记录 `sessions/<年>/<月>/<日>/rollout-<时间>-<id>.jsonl`
    fn conv_files(&self, home: &Path, conv: &str) -> Vec<PathBuf> {
        if conv.is_empty() || conv.contains('/') {
            return vec![];
        }
        let tail = format!("-{conv}.jsonl");
        let found = rollout_in(&home.join("sessions"), &tail, 4);
        found.and_then(|f| f.strip_prefix(home).ok().map(Path::to_path_buf)).into_iter().collect()
    }

    /// 各账号 config.toml 里的默认模型不一样(这台机器上就是):原来那段用的模型不是新账号默认的,带上 `-m` 接着用它。
    /// 会话参数里原来的 `-m` 先拿掉再看要不要带(换回原账号时就不用带了)。工具、新账号里写死了模型的不动;
    /// 两个账号走的不是同一个服务(`model_provider`)的也不动,模型照新账号的,说一声
    fn rehome_args(&self, args: &[String], log: &Path, from: &Path, to: &Path, fixed: &[String]) -> (Vec<String>, String) {
        let keep = || (args.to_vec(), String::new());
        let Some(was) = last_model(log) else { return keep() };
        if fixed.iter().enumerate().any(|(i, _)| super::valued(&fixed[i..], MODEL) > 0) {
            return keep();
        }
        let (old, new) = (defaults(from, &[]), defaults(to, fixed));
        if old.provider != new.provider {
            let now = new.model.unwrap_or_default();
            return (
                args.to_vec(),
                tr!(
                    "两个账号走的服务不一样({} → {}),模型照新账号的 {now}(原来是 {was})",
                    "the accounts use different providers ({} → {}); the model is the new account's {now} (was {was})",
                    old.provider,
                    new.provider
                ),
            );
        }
        let mut out = vec![];
        let mut rest = args;
        while let Some(w) = rest.first() {
            let n = super::valued(rest, MODEL);
            if n == 0 {
                out.push(w.clone());
            }
            rest = &rest[n.max(1).min(rest.len())..];
        }
        if new.model.as_deref() == Some(was.as_str()) {
            return (out, String::new());
        }
        let note = match &new.model {
            Some(m) => tr!("带上 -m {was}(新账号默认是 {m})", "with -m {was} (the new account defaults to {m})"),
            None => tr!("带上 -m {was}", "with -m {was}"),
        };
        out.extend(["-m".to_string(), was]);
        (out, note)
    }

    fn can_fork(&self) -> bool {
        true
    }

    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>> {
        Ok(plan(tool, s, how, aca))
    }

    /// 知道对话 id 就接(第一句话发出去 hook 才报 id;起了没说过话的重新开)
    fn resume(&self, conv: &str, _r: &Run) -> How {
        if conv.is_empty() { How::Fresh } else { How::Resume(conv.to_string()) }
    }

    fn title_in_pane(&self) -> bool {
        true
    }

    fn title(&self, _r: &Run, title: &str) -> Option<String> {
        pane_title(title)
    }

    /// 正在跑的 Codex 不留「我现在是哪段对话」的记号(它的对话在共用的后台 server 里):只有命令行里写着的认得出
    /// (`codex resume <id>`)。手动新开的,对话 id 要等它在 aca 下面重启、说过一句话后 hook 报上来
    fn live(&self, _pid: u32, _home: Option<&str>, argv: &[String]) -> Live {
        let id = argv.get(2).filter(|v| !v.starts_with('-'));
        let conv = match argv.get(1).map(String::as_str) {
            Some("resume") => id.cloned().unwrap_or_default(),
            _ => String::new(),
        };
        Live { conv, args: super::carry(argv, |rest| not_carried(rest, rest.len() + 1 == argv.len())), ..Default::default() }
    }

    fn delivery(&self) -> super::Delivery {
        super::Delivery::Queue
    }

    /// 带一个 `-c`:`codex queue` 就在自己进程里写完队列就走,不会给这个账号拉起一个一直挂着的后台服务。
    /// 那个服务已经在跑时它会拒绝(怕两边各写各的),只能去掉 `-c` 交给那个服务
    fn queue_args(&self, conv: &str, text: &str, shared: bool) -> Vec<String> {
        let mut a: Vec<String> = if shared { vec![] } else { vec!["-c".into(), "notify=[]".into()] };
        a.extend(["queue", "--thread", conv, "--message", text].map(String::from));
        a
    }

    fn prompt_mark(&self) -> Option<char> {
        Some('›')
    }

    /// 「esc to interrupt」:按一下就停,它自己报 Interrupt
    fn interrupt_key(&self) -> Option<&'static str> {
        Some("Escape")
    }

    fn reports_interrupt(&self) -> bool {
        true
    }

    fn opening(&self, text: &str) -> Option<Vec<String>> {
        super::opening_after_dashes(text)
    }

    /// 「Folder access … Trust this folder? … › 1. Trust and continue / 2. Quit」。审 hook 的那道关
    /// (「2. Trust all and continue」)不算:那个归 `hook_trust` 管
    fn trust_keys(&self, lines: &[String]) -> Option<Vec<&'static str>> {
        let g = crate::answer::gate(lines)?;
        let yes = g.opts.iter().position(|o| o == "Trust and continue")?;
        (g.opts.len() == 2 && g.opts.iter().any(|o| o == "Quit")).then(|| crate::answer::gate_keys(&g, yes + 1))
    }

    /// 两种结束它不报 hook,都只在对话记录里留一条 `task_complete`:
    /// - 一轮出错(限流、过载、断流、额度……,它自己重试几次也不行之后):不跑 Stop hook,也没有别的 hook
    ///   (0.162 源码:采样出错直接跳出这一轮),那一条带着 `error`
    /// - 正常跑完、Stop hook 却没被信任:它不跑没信任的 hook,也不提示。信任是一条一条记的,
    ///   会有 UserPromptSubmit 信任了、Stop 没有的账号(0.162.1 上遇到过):每一轮都只报开始、不报结束。
    ///   Stop hook 是等它跑完才写 `task_complete` 的,所以记录里有了、hook 还说在跑,就是没跑
    fn unreported_end(&self, r: &Run) -> Option<serde_json::Value> {
        last_turn_end(&tail(&r.transcript, 256 * 1024)?, r.turn)
    }

    fn last_reply(&self, r: &Run) -> Option<String> {
        last_reply(&tail(&r.transcript, 1024 * 1024)?)
    }

    fn screen(&self, lines: &[String]) -> Seen {
        let nonempty: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
        let tail = &nonempty[nonempty.len().saturating_sub(14)..];
        if tail.iter().any(|l| ASK.is_match(l)) {
            // 问的那句:以问号结尾的;启动那几道关没有问号,取框里的第一行
            let q = tail
                .iter()
                .map(|l| l.trim())
                .find(|l| l.ends_with('?') || l.ends_with('？'))
                .or(tail.first().map(|l| l.trim()))
                .unwrap_or("");
            return Seen::Ask(q.to_string());
        }
        for l in tail.iter().rev() {
            if let Some(c) = RUNNING.captures(l) {
                let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
                return Seen::Running(n(1) * 3600 + n(2) * 60 + n(3));
            }
        }
        Seen::Quiet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn switching_accounts_keeps_the_model_unless_the_new_one_pins_it_or_uses_another_provider() {
        let d = std::env::temp_dir().join(format!("aca-rehome-{}", std::process::id()));
        let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
        for (dir, toml) in [
            (&a, "model = \"m-a\"\n"),
            (&b, "model = \"m-b\"\nprofile = \"fast\"\n[profiles.fast]\nmodel = \"m-fast\"\n"),
            (&c, "model = \"m-a\"\nmodel_provider = \"relay\"\n"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("config.toml"), toml).unwrap();
        }
        let log = d.join("rollout.jsonl");
        std::fs::write(
            &log,
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"m-x\"}}\n{\"type\":\"turn_context\",\"payload\":{\"model\":\"m-a\"}}\n",
        )
        .unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // b 选了 profile:默认是 profile 里的 m-fast,和最后一轮的 m-a 不一样,带上;原来的 -m 先拿掉
        let (args, note) = Codex.rehome_args(&s(&["-m", "old", "--yolo"]), &log, &a, &b, &[]);
        assert_eq!((args, note.contains("m-fast")), (s(&["--yolo", "-m", "m-a"]), true));
        // 参数里点名了别的 profile:看它的(没有这个 profile 就是顶上的 m-b)
        assert_eq!(Codex.rehome_args(&[], &log, &a, &b, &s(&["--profile", "none"])).0, s(&["-m", "m-a"]));
        // 回到默认就是 m-a 的账号:不用带
        assert_eq!(Codex.rehome_args(&s(&["--model=m-a"]), &log, &b, &a, &[]), (vec![], String::new()));
        // 新账号里写死了模型:不动
        assert_eq!(Codex.rehome_args(&s(&["--yolo"]), &log, &a, &b, &s(&["-m", "pinned"])).0, s(&["--yolo"]));
        // 走的不是同一个服务:模型照新账号的,说一声
        let (args, note) = Codex.rehome_args(&s(&["-m", "m-a"]), &log, &a, &c, &[]);
        assert_eq!(args, s(&["-m", "m-a"]));
        assert!(note.contains("openai → relay"), "{note}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_rollout_is_found_by_its_thread_id_under_sessions() {
        let d = std::env::temp_dir().join(format!("aca-rollouts-{}", std::process::id()));
        let f = d.join("sessions/2026/10/14/rollout-2026-10-14T09-00-00-th-1.jsonl");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "").unwrap();
        std::fs::write(f.with_file_name("rollout-2026-10-14T09-00-00-th-10.jsonl"), "").unwrap();
        assert_eq!(Codex.conv_files(&d, "th-1"), vec![PathBuf::from("sessions/2026/10/14/rollout-2026-10-14T09-00-00-th-1.jsonl")]);
        assert!(Codex.conv_files(&d, "th-2").is_empty() && Codex.conv_files(&d, "../x").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_folder_trust_question_is_answered_but_the_hook_review_is_not() {
        // 0.162 上抄下来的
        let ask = "  Folder access\n\n  /p/api\n\n  Trust this folder? Codex can read, edit, and run files here, subject to your\n  permission settings. Folder settings can run code automatically, even\n  without a model request. Continue only if you trust these files. Your trust\n  decision will be saved.\n\n› 1. Trust and continue\n  2. Quit\n\n  enter continue · esc quit\n";
        assert_eq!(Codex.trust_keys(&lines(ask)), Some(vec!["Enter"]));
        // 光标被挪到 Quit 上了:走回去
        let moved = ask.replace("› 1. Trust and continue\n  2. Quit", "  1. Trust and continue\n› 2. Quit");
        assert_eq!(Codex.trust_keys(&lines(&moved)), Some(vec!["Up", "Enter"]));
        // 审 hook 的那道关归 hook_trust 管;运行中的确认框、平常的输入框都不是
        let review = "  Hooks need review\n  6 hooks are new or changed.\n  Hooks can run outside the sandbox after you trust them.\n› 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n  enter confirm · esc skip";
        assert_eq!(Codex.trust_keys(&lines(review)), None);
        assert_eq!(Codex.trust_keys(&lines("  Allow command?\n› 1. Yes\n  2. No\n")), None);
        assert_eq!(Codex.trust_keys(&lines("• 做完了\n\n› Ask Codex to do anything\n")), None);
    }

    #[test]
    fn hooks_ride_along_as_config_overrides() {
        let flag = hook_flag("Stop", "/a b/aca");
        let (key, value) = flag.split_once('=').unwrap();
        assert_eq!(key, "hooks.Stop");
        // 值得是 Codex 读得懂的 TOML:一个数组,里面一组,组里一条命令
        let v: toml::Value = toml::from_str(&format!("x = {value}")).unwrap();
        let h = &v["x"][0]["hooks"][0];
        assert_eq!((h["type"].as_str(), h["command"].as_str()), (Some("command"), Some("'/a b/aca' hook codex")));
    }

    #[test]
    fn it_resumes_by_thread_id_and_only_bypasses_trust_when_told_to() {
        let s = Session { args: vec!["--search".into()], ..Default::default() };
        let argv = plan(&Tool::default(), &s, &How::Resume("th-1".into()), "aca");
        assert_eq!(argv[..3], ["codex", "resume", "th-1"]);
        assert_eq!(argv.iter().filter(|a| *a == "-c").count(), EVENTS.len());
        assert_eq!(argv.last().unwrap(), "--search");
        assert!(!argv.iter().any(|a| a.contains("bypass")));
        let t = Tool { cmd: vec!["/w/wrapper.sh".into()], hook_trust: "bypass".into(), ..Default::default() };
        let argv = plan(&t, &Session::default(), &How::Fork("th-1".into()), "aca");
        assert_eq!(argv[..3], ["/w/wrapper.sh", "fork", "th-1"]);
        assert_eq!(argv.last().unwrap(), "--dangerously-bypass-hook-trust");
        assert_eq!(Codex.resume("", &Run::default()), How::Fresh);
        assert_eq!(Codex.resume("th", &Run::default()), How::Resume("th".into()));
    }

    #[test]
    fn a_turn_that_ended_without_a_hook_is_read_from_the_end_of_its_rollout() {
        // 0.162 记录里的样子:一轮开始 task_started,结束 task_complete(出错的多一个 error),Esc 打断 turn_aborted
        let started = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t2"}}"#;
        let done = |extra: &str| {
            format!(r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t2","completed_at":200{extra}}}}}"#)
        };
        let usage = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex"}}}"#;
        let log = |lines: &[&str]| lines.join("\n");
        // 还在跑、被打断:没结束
        assert_eq!(last_turn_end(&log(&[started, usage]), 100), None);
        assert_eq!(last_turn_end(&log(&[started, r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}"#]), 100), None);
        // 正常结束(Stop hook 没被信任时它不报):补一个 Stop,带最后一条回复和结束的时刻
        let ok = done(r#","last_agent_message":"好了","error":null"#);
        let v = last_turn_end(&log(&[started, &ok, usage]), 100).unwrap();
        assert_eq!(
            (v["hook_event_name"].as_str(), v["last_assistant_message"].as_str(), v["at"].as_u64()),
            (Some("Stop"), Some("好了"), Some(200))
        );
        // 交给插件的「最后一条回复」也从这读:最后一个跑完的那一轮的,出错的那轮没有回复就再往前找
        assert_eq!(last_reply(&log(&[started, &ok, usage])).as_deref(), Some("好了"));
        let failed = done(r#","error":{"message":"x","codex_error_info":"server_overloaded"}"#);
        assert_eq!(last_reply(&log(&[&ok, started, &failed])).as_deref(), Some("好了"));
        assert_eq!(last_reply(&log(&[started, usage])), None);
        // 上一轮的(新的一轮刚开始,记录里还没写上)、没写结束时刻的(分不出是哪一轮的):不补
        assert_eq!(last_turn_end(&ok, 300), None);
        assert_eq!(
            last_turn_end(r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t2","last_agent_message":"好了"}}"#, 100),
            None
        );
        // 出错结束:类别不带数据的是字符串,带数据的是只有一个键的对象
        let overloaded = done(r#","error":{"message":"Our servers are currently overloaded.","codex_error_info":"server_overloaded"}"#);
        let v = last_turn_end(&log(&[started, &overloaded, usage]), 100).unwrap();
        assert_eq!(
            (v["hook_event_name"].as_str(), v["error"].as_str(), v["error_kind"].as_str(), v["at"].as_u64()),
            (Some("StopFailure"), Some("Our servers are currently overloaded."), Some("transient"), Some(200))
        );
        let http = done(r#","error":{"message":"bad gateway","codex_error_info":{"http_connection_failed":{"http_status_code":502}}}"#);
        assert_eq!(last_turn_end(&http, 100).unwrap()["error_kind"], "transient");
        // 带着 4xx 的看码:模型名写错(0.162 上实测是 http_connection_failed 带 404)再试也一样,401 是账号的
        let missing = done(
            r#","error":{"message":"unexpected status 404 Not Found: Model \"x\" is not supported","codex_error_info":{"http_connection_failed":{"http_status_code":404}}}"#,
        );
        assert_eq!(last_turn_end(&missing, 100).unwrap()["error_kind"], "fatal");
        let denied = done(r#","error":{"message":"no","codex_error_info":{"http_connection_failed":{"http_status_code":401}}}"#);
        assert_eq!(last_turn_end(&denied, 100).unwrap()["error_kind"], "account");
        let limit = done(r#","error":{"message":"You've hit your usage limit.","codex_error_info":"usage_limit_exceeded"}"#);
        assert_eq!(last_turn_end(&limit, 100).unwrap()["error_kind"], "account");
        let window = done(r#","error":{"message":"context window exceeded","codex_error_info":"context_window_exceeded"}"#);
        assert_eq!(last_turn_end(&window, 100).unwrap()["error_kind"], "fatal");
        // 认不出的类别:看原文
        let odd = done(r#","error":{"message":"429 Too Many Requests","codex_error_info":"other"}"#);
        assert_eq!(last_turn_end(&odd, 100).unwrap()["error_kind"], "transient");
        // 比这一轮开始还早结束的:是上一轮的(新的一轮刚开始,记录里还没写上)
        assert_eq!(last_turn_end(&overloaded, 300), None);
        // 读的是文件末尾:从下一个整行开始,半行不算
        let path = std::env::temp_dir().join(format!("aca-codex-tail-{}.jsonl", std::process::id()));
        std::fs::write(&path, format!("{}\n{started}\n{overloaded}\n", "x".repeat(50))).unwrap();
        let t = tail(&path.to_string_lossy(), (started.len() + overloaded.len() + 10) as u64).unwrap();
        assert!(t.starts_with(started) && last_turn_end(&t, 100).is_some(), "{t}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_running_codex_only_shows_its_thread_when_it_was_resumed_by_id() {
        let live = |words: &[&str]| Codex.live(1, None, &words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        let some = |words: &[&str]| Some(words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        // 手动新开的:不知道是哪段对话;参数照搬
        assert_eq!(
            live(&["codex", "--yolo", "--profile", "labs"]),
            Live { args: some(&["--yolo", "--profile", "labs"]), ..Default::default() }
        );
        let l = live(&["codex", "resume", "th-1", "--yolo"]);
        assert_eq!((l.conv.as_str(), l.args), ("th-1", some(&["--yolo"])));
        // resume --last、fork 出来的:现在这段的 id 不在命令行里
        assert_eq!(live(&["codex", "resume", "--last", "--yolo"]), Live { args: some(&["--yolo"]), ..Default::default() });
        assert_eq!(live(&["codex", "fork", "th-1"]).conv, "");
        // aca 自己带的 hook 和跳过信任的开关不搬;别的 -c 照搬
        let l = live(&["codex", "-c", "hooks.Stop=[{}]", "-c", "model=o3", "--dangerously-bypass-hook-trust", "--search"]);
        assert_eq!(l.args, some(&["-c", "model=o3", "--search"]));
        // 起的时候就交给它一句话的:不搬
        assert_eq!(live(&["codex", "修一下登录"]).args, None);
        assert_eq!(live(&[]).args, None);
        // 不在开头的 resume 是别的参数的值,不当成子命令
        assert_eq!(live(&["codex", "--profile", "resume"]).args, some(&["--profile", "resume"]));
    }

    #[test]
    fn its_own_title_is_the_part_of_the_pane_title_before_the_folder() {
        assert_eq!(pane_title("⠴ Reply with ok | api").as_deref(), Some("Reply with ok"));
        assert_eq!(pane_title("修登录 | a | b | api").as_deref(), Some("修登录 | a | b"));
        assert_eq!(pane_title("api"), None);
        assert_eq!(pane_title("⠸ | api"), None);
        assert_eq!(pane_title("⠸ ⠸ | api"), None);
        assert_eq!(pane_title(""), None);
    }

    #[test]
    fn startup_gates_and_confirmations_are_asking_and_the_timer_line_is_running() {
        // 这三屏是 0.159 上实际抓的
        let review = "  Hooks need review\n  6 hooks are new or changed.\n  Hooks can run outside the sandbox after you trust them.\n› 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\n  enter confirm · esc skip";
        assert_eq!(Codex.screen(&lines(review)), Seen::Ask("Hooks need review".into()));
        let trust = "  Folder access\n  /p/api\n  Trust this folder? Codex can read, edit, and run files here.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit";
        assert!(matches!(Codex.screen(&lines(trust)), Seen::Ask(_)));
        let update = "  Update available · 0.159.3 → 0.160.0\n› 1. Update now\n  2. Skip\n  enter continue · esc skip";
        assert!(matches!(Codex.screen(&lines(update)), Seen::Ask(_)));
        assert_eq!(
            Codex.screen(&lines("› 写一篇长文\n\n• Working (1m 05s • esc to interrupt)\n\n› Ask Codex to do anything")),
            Seen::Running(65)
        );
        assert_eq!(Codex.screen(&lines("• ok\n  Worked for 5s • 2:35 AM\n› Ask Codex to do anything\n  Context 97% left")), Seen::Quiet);
    }
}
