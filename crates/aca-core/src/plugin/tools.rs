//! `aca plugin tools`:把开着的插件露给 agent 的能力(`expose = ["agent"]`)包成 MCP 工具。挂载时这个服务叫 `aca`(见 [`super::mount`])。
//!
//! stdio,一行一个 JSON-RPC。agent 起它,环境里的 `ACA_SESSION_ID` 就是在调的那个会话:列出来的只有够得着这个会话的插件
//! (plugins.toml 的 agents),调的时候插件命令拿到的会话也是它。
//!
//! 只做工具这一样:initialize / tools/list / tools/call / ping。按 2025-11-25 那一版(握手一次);
//! 新版(2026-07-28)的客户端先问 `server/discover` 的,这里回「没有这个方法」,它们退回旧版握手 —— 两家现在两版都认。
//! 一次调用最多跑 [`TIMEOUT`],Codex 默认 300 秒不等;要等更久的(等一个 worker 跑完)让命令先回「还在跑」,下次再问。

use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Action, Entry, Manifest, decl, mount, read_manifest, registry};
use crate::paths::Res;
use crate::tr;

const PROTOCOL: &str = "2025-11-25";
const KNOWN: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
pub const TIMEOUT: Duration = Duration::from_secs(280);

pub fn serve() -> Res<i32> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(_) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } })),
        };
        if let Some(r) = reply {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
    Ok(0)
}

/// 一条消息 → 回什么(通知不回)
pub(crate) fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => {
            let want = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL);
            let v = if KNOWN.contains(&want) { want } else { PROTOCOL };
            Ok(json!({
                "protocolVersion": v,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "aca", "version": env!("CARGO_PKG_VERSION") },
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": offered().iter().map(|t| t.schema()).collect::<Vec<_>>() })),
        "tools/call" => Ok(call(&msg["params"])),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, m)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": m } }),
    })
}

/// 一个露给 agent 的能力
struct Offered {
    entry: Entry,
    manifest: Manifest,
    action: Action,
}

impl Offered {
    /// 工具名:`<插件>__<能力>`(只有字母数字 _ -,Claude 再在前面加 mcp__aca__)
    fn name(&self) -> String {
        format!("{}__{}", mount::safe(&self.entry.id), self.action.id)
    }

    fn schema(&self) -> Value {
        let a = &self.action;
        let mut props = serde_json::Map::new();
        let mut required = vec![];
        for name in a.args.keys() {
            let (ty, optional) = a.arg(name).unwrap_or(("string", true));
            let p = match ty {
                "int" => json!({ "type": "integer" }),
                "bool" => json!({ "type": "boolean" }),
                "path" => json!({ "type": "string", "description": "a file or directory path" }),
                _ => json!({ "type": "string" }),
            };
            props.insert(name.clone(), p);
            if !optional {
                required.push(name.clone());
            }
        }
        let desc = if a.desc.is_empty() { &a.title } else { &a.desc };
        json!({
            "name": self.name(),
            "description": format!("{desc}({}:{})", self.manifest.name, a.id),
            "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
        })
    }
}

/// 在调的是哪个会话:gateway 转来的请求在 `_meta` 里写着(`dev.aca/session`),直接挂的看环境里的 `ACA_SESSION_ID`
pub(crate) fn caller(params: &Value) -> super::Context {
    match params.pointer("/_meta/dev.aca~1session").and_then(Value::as_str) {
        Some(s) => super::agent_context_for(s),
        None => super::agent_context(),
    }
}

/// 这个会话(环境里的 `ACA_SESSION_ID`)能用的:开着的、够得着它的插件里露给 agent 的能力。
/// 共用的那一份(gateway 下面、没有会话)列全部,由 gateway 按会话挑
fn offered() -> Vec<Offered> {
    offered_for(&super::agent_context())
}

fn offered_for(ctx: &super::Context) -> Vec<Offered> {
    let s = (!ctx.session.is_empty()).then(|| crate::store::Store::open().ok()?.get(&ctx.session).ok()).flatten();
    let cfg = crate::config::load();
    let (tool, account) = s.as_ref().map(|s| (s.tool.clone(), s.account.clone())).unwrap_or_default();
    let kind = cfg.tool(&tool).map(|t| t.kind).unwrap_or_default();
    let wants = decl::wants().unwrap_or_default();
    let mut out = vec![];
    for e in registry().into_iter().filter(|e| e.enabled) {
        // 给哪些:你在 plugins.toml 里定的(没写 = 清单里写 agent 的),见 yours
        let allowed = super::yours::agent_tools(&e, wants.get(&e.id));
        if allowed.is_empty() || (s.is_some() && !wants.get(&e.id).is_none_or(|w| w.reaches(&tool, &kind, &account))) {
            continue;
        }
        let Ok((m, _)) = read_manifest(Path::new(&e.path)) else { continue };
        for a in m.actions.iter().filter(|a| allowed.contains(&a.id)) {
            out.push(Offered { entry: e.clone(), manifest: m.clone(), action: a.clone() });
        }
    }
    out
}

fn text(t: String, error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": t }], "isError": error })
}

/// 调一个工具:参数照清单的类型查一遍,跑那条命令(参数在 `ACA_PLUGIN_ARGS_JSON`),输出原样交回去。
/// 出错(参数不对、命令退出码不是 0、超时)也是正常的回答,`isError` 标出来,agent 看得到原因
fn call(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let ctx = caller(params);
    let Some(mut o) = offered_for(&ctx).into_iter().find(|o| o.name() == name) else {
        return text(tr!("没有 {name} 这个工具(插件停了、或者不给这个会话)", "no tool {name} (plugin off, or not for this session)"), true);
    };
    // 自带的:刚升级了 aca 就先换成新写出来的那份
    if o.entry.source == super::first_party::SOURCE
        && super::first_party::refresh_one(&o.entry.id)
        && let Some(now) = offered_for(&ctx).into_iter().find(|o| o.name() == name)
    {
        o = now;
    }
    let args = match check(&o.action, &params["arguments"]) {
        Ok(a) => a,
        Err(e) => return text(e, true),
    };
    let mut c = super::command(&o.entry, &o.manifest, &o.action.command, &ctx, &super::command_ids(&o.action.id, &args), &o.action.cwd);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let fed = (!o.action.input.is_empty()).then(|| super::input_text(&o.action.input, &ctx));
    match run_fed(c, TIMEOUT, fed) {
        Ok(f) => text(f.text(), !f.ok),
        Err(e) => text(e, true),
    }
}

/// MCP 给的参数(已经是 JSON)照清单的类型查:少了必须给的、多了没声明的、类型不对的都说。返回交给命令的那段 JSON
pub(crate) fn check(a: &Action, given: &Value) -> Res<String> {
    let empty = serde_json::Map::new();
    let given = given.as_object().unwrap_or(&empty);
    for (k, v) in given {
        let Some((ty, _)) = a.arg(k) else {
            return Err(tr!("{} 没有参数 {k}", "{} has no argument {k}", a.id));
        };
        let ok = match ty {
            "int" => v.is_i64() || v.is_u64(),
            "bool" => v.is_boolean(),
            _ => v.is_string(),
        };
        if !ok {
            return Err(tr!("参数 {k} 要是 {ty}", "argument {k} must be {ty}"));
        }
    }
    if let Some(k) = a.args.keys().find(|k| !given.contains_key(*k) && a.arg(k).is_some_and(|(_, opt)| !opt)) {
        return Err(tr!("{} 要给参数 {k}", "{} needs argument {k}", a.id));
    }
    let mut v = given.clone();
    for (k, val) in v.iter_mut() {
        if a.arg(k).is_some_and(|(t, _)| t == "path") {
            *val = crate::paths::expand(val.as_str().unwrap_or_default()).into();
        }
    }
    Ok(Value::Object(v).to_string())
}

/// 跑完一条插件命令收下的
#[derive(Debug, PartialEq)]
pub struct Fed {
    pub ok: bool,
    /// 退出码;被信号打断的、停掉了的是 None
    pub code: Option<i32>,
    pub out: String,
    pub err: String,
    /// 过了这么多秒还没完,停掉了
    pub late: Option<u64>,
}

impl Fed {
    /// 给人看、给 agent 看的:标准输出,后面接标准错误;停掉了的说一句
    pub fn text(&self) -> String {
        let mut text = self.out.clone();
        if !self.err.trim().is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&self.err);
        }
        match self.late {
            Some(secs) => tr!("{text}\n(过了 {secs} 秒还没完,停掉了)", "{text}\n(still running after {secs} s; stopped)"),
            None => text,
        }
    }
}

/// 发了 TERM 再等这么久,还不退就 KILL;它自己退出(或被杀)之后,输出最多再等这么久(后台进程占着管道不放时不跟着等)
const GRACE: Duration = if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(3) };

/// 跑完(最多等 `limit`)。到时间了先 TERM 它那一组,不退再 KILL。
/// `input` 给了就写进它的标准输入(插件的 `input`)
pub(crate) fn run_fed(mut c: std::process::Command, limit: Duration, input: Option<String>) -> Res<Fed> {
    use std::os::unix::process::CommandExt;
    if input.is_some() {
        c.stdin(Stdio::piped());
    }
    let mut child = c.process_group(0).spawn().map_err(|e| tr!("起不来:{e}", "can't start: {e}"))?;
    if let (Some(text), Some(mut w)) = (input, child.stdin.take()) {
        // 另起一个线程写:它不读完也不会卡住这边(管道满了只卡那个线程)
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = w.write_all(text.as_bytes());
        });
    }
    // 读到一点放一点:它留在后台的进程占着输出不放时,这边拿走已经读到的就走,不跟着一直等
    type Got = (std::sync::Arc<std::sync::Mutex<Vec<u8>>>, std::sync::mpsc::Receiver<()>);
    let read = |r: Option<Box<dyn Read + Send>>| -> Got {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let (tx, rx) = std::sync::mpsc::channel();
        let b = buf.clone();
        std::thread::spawn(move || {
            if let Some(mut r) = r {
                let mut chunk = [0u8; 8192];
                loop {
                    match r.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => b.lock().unwrap().extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            }
            let _ = tx.send(());
        });
        (buf, rx)
    };
    let out = read(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    // 另一个线程等它退出,这边最多等 limit:它一退出马上知道(不隔一阵看一眼 —— 那样每条命令平白多等几十毫秒)
    let pid = child.id() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let status = match rx.recv_timeout(limit) {
        Ok(st) => Some(st.map_err(|e| e.to_string())?),
        Err(_) => {
            unsafe { libc::killpg(pid, libc::SIGTERM) };
            // 把 TERM 忽略掉的:过一会儿 KILL(KILL 收不到也躲不掉)
            if rx.recv_timeout(GRACE).is_err() {
                unsafe { libc::killpg(pid, libc::SIGKILL) };
                let _ = rx.recv();
            }
            None
        }
    };
    let take = |(buf, done): Got| {
        let _ = done.recv_timeout(GRACE);
        String::from_utf8_lossy(&std::mem::take(&mut *buf.lock().unwrap())).into_owned()
    };
    let (out, err) = (take(out), take(err));
    Ok(match status {
        Some(st) => Fed { ok: st.success(), code: st.code(), out, err, late: None },
        None => Fed { ok: false, code: None, out, err, late: Some(limit.as_secs()) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action() -> Action {
        let (m, _) = super::super::parse(
            "id = \"p\"\nname = \"x\"\nversion = \"1\"\n[[commands]]\nid = \"go\"\ntitle = \"走\"\nargs = { task = \"string\", n = \"int?\", dir = \"path?\" }\nexpose = [\"agent\"]\ncommand = [\"true\"]",
        )
        .unwrap();
        m.actions[0].clone()
    }

    #[test]
    fn the_handshake_and_unknown_methods_answer_like_mcp() {
        let r =
            handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], false);
        let r =
            handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } })).unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL);
        // 新版客户端先探的那个:没有这个方法,它退回旧版握手
        let r = handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "server/discover" })).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
        assert_eq!(handle(&json!({ "jsonrpc": "2.0", "id": "x", "method": "ping" })).unwrap()["result"], json!({}));
    }

    #[test]
    fn arguments_from_an_agent_are_checked_against_the_manifest() {
        let a = action();
        let v: Value = serde_json::from_str(&check(&a, &json!({ "task": "修", "n": 2 })).unwrap()).unwrap();
        assert_eq!(v, json!({ "task": "修", "n": 2 }));
        assert!(check(&a, &json!({ "n": 2 })).unwrap_err().contains("task"));
        assert!(check(&a, &json!({ "task": "x", "n": "2" })).unwrap_err().contains("n"));
        assert!(check(&a, &json!({ "task": "x", "who": 1 })).unwrap_err().contains("who"));
    }

    #[test]
    fn input_goes_to_the_commands_stdin() {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", "tr a-z A-Z"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let f = run_fed(c, TIMEOUT, Some("hi\n".into())).unwrap();
        assert_eq!((f.ok, f.out.as_str()), (true, "HI\n"));
    }

    fn sh(script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", script]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        c
    }

    #[test]
    fn a_command_that_runs_too_long_is_stopped_and_says_so() {
        let f = run_fed(sh("echo started; sleep 5"), Duration::from_millis(300), None).unwrap();
        assert!(!f.ok && f.late == Some(0) && f.text().contains("started") && f.text().contains("0 秒"), "{f:?}");
        let f = run_fed(sh("echo out; echo err >&2; exit 3"), TIMEOUT, None).unwrap();
        assert_eq!((f.ok, f.out.as_str(), f.err.as_str(), f.text()), (false, "out\n", "err\n", "out\nerr\n".to_string()));
    }

    #[test]
    fn one_that_ignores_term_is_killed_and_one_that_leaves_a_process_holding_its_output_does_not_hang() {
        let t = std::time::Instant::now();
        let f = run_fed(sh("trap '' TERM; echo stubborn; sleep 30"), Duration::from_millis(200), None).unwrap();
        assert!(!f.ok && f.out == "stubborn\n" && f.late.is_some(), "{f:?}");
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        // 退出了,后台还有个进程拿着它的标准输出:收下已有的就回来
        let t = std::time::Instant::now();
        let f = run_fed(sh("sleep 3 & echo done"), TIMEOUT, None).unwrap();
        assert!(f.ok && f.out == "done\n", "{f:?}");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }
}
