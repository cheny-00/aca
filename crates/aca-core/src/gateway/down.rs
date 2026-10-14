//! gateway 下面的一个 MCP 服务(一个子进程,stdio,一行一个 JSON-RPC)。
//!
//! 几个连接共用它时:请求编号由 gateway 重新发(谁的请求、原来的编号记在 `pending` 里,回来再换回去);
//! 握手(initialize)只做一次,工具列表缓存着,它说变了(list_changed)再重新拉;progress 的令牌也换成 gateway 自己的。
//! 它反过来问的:roots/list 回空的(每会话一份的回那个会话的目录),别的(sampling、elicitation)回「不支持」。
//! 进程退出了,在等它回答的都回出错;下次有人要再起,连着失败就等一会儿(2、4、8…最多 30 秒),连败 5 次停下不再自己起。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::serve::Client;
use super::{Spec, calls_log, log_of};
use crate::paths::{self, Res};
use crate::{t, tr};

const PROTOCOL: &str = "2025-11-25";
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// 连着失败这么多次就不再自己起(要 `aca plugin gateway restart <名字>` 或者改配置)
pub(crate) const GIVE_UP: u32 = 5;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum State {
    Idle,
    Ready,
    Failed { msg: String, at: Instant, fails: u32 },
}

/// 一个在等回答的请求
enum Waiter {
    /// gateway 自己问的(握手、列工具):回答交给等着的那个
    Sync(mpsc::Sender<Value>),
    /// 替某个连接转的:回答换回它原来的编号交给它
    Client { client: Weak<Client>, id: Value, started: Instant, tool: String },
}

pub(crate) struct Down {
    pub spec: Spec,
    /// 每会话一份的那个会话;共用的是空的
    pub session: String,
    /// 每会话一份的:会话的目录(它问 roots 时回这个)
    root: String,
    state: Mutex<State>,
    tools: Mutex<Vec<Value>>,
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, Waiter>>,
    /// gateway 的 progress 令牌 → (哪个连接, 它原来的令牌)
    progress: Mutex<HashMap<String, (Weak<Client>, Value)>>,
    next: AtomicU64,
    /// 第几次起的:老进程的读线程收尾时别动新进程的状态
    generation: AtomicU64,
    starting: Mutex<()>,
    /// 它的工具变了:gateway 去告诉连着的 agent
    on_change: Box<dyn Fn() + Send + Sync>,
}

impl Down {
    pub fn new(spec: Spec, session: String, root: String, on_change: Box<dyn Fn() + Send + Sync>) -> Arc<Down> {
        Arc::new(Down {
            spec,
            session,
            root,
            state: Mutex::new(State::Idle),
            tools: Mutex::new(vec![]),
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            progress: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            starting: Mutex::new(()),
            on_change,
        })
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    pub fn tools(&self) -> Vec<Value> {
        self.tools.lock().unwrap().clone()
    }

    /// 在跑就好;没跑起来过、退出了的,起一个(失败过的照退避的时间来)。返回原因
    pub fn ensure(self: &Arc<Self>) -> Res<()> {
        let _one = self.starting.lock().unwrap();
        match self.state() {
            State::Ready => return Ok(()),
            State::Failed { msg, fails, .. } if fails >= GIVE_UP => {
                return Err(tr!("连着失败 {fails} 次,停下了:{msg}", "failed {fails} times in a row, stopped: {msg}"));
            }
            State::Failed { msg, at, fails } if at.elapsed() < Duration::from_secs((1u64 << fails.min(5)).min(30)) => {
                return Err(tr!("刚失败过,等一会儿再起:{msg}", "failed just now; will retry shortly: {msg}"));
            }
            _ => {}
        }
        let r = self.start();
        if let Err(msg) = &r {
            let fails = match self.state() {
                State::Failed { fails, .. } => fails + 1,
                _ => 1,
            };
            self.note(&format!("没起来:{msg}"));
            self.stop_child();
            *self.state.lock().unwrap() = State::Failed { msg: msg.clone(), at: Instant::now(), fails };
        }
        r
    }

    fn start(self: &Arc<Self>) -> Res<()> {
        let log = log_of(&self.log_name());
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        let err = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| e.to_string())?;
        self.note(&format!("起:{} {}", self.spec.command, self.spec.args.join(" ")));
        let mut c = Command::new(&self.spec.command);
        c.args(&self.spec.args).envs(self.spec.env.iter().map(|(k, v)| (k, v)));
        if !self.session.is_empty() {
            c.env("ACA_SESSION_ID", &self.session);
        } else {
            // 共用的:不带哪个会话的身份(调用者在每个请求的 _meta 里)
            c.env_remove("ACA_SESSION_ID");
        }
        if !self.root.is_empty() {
            c.current_dir(&self.root);
        }
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(err))
            .process_group(0)
            .spawn()
            .map_err(|e| tr!("{} 起不来:{e}", "can't start {}: {e}", self.spec.command))?;
        let out = child.stdout.take().unwrap();
        *self.stdin.lock().unwrap() = child.stdin.take();
        *self.child.lock().unwrap() = Some(child);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let me = Arc::downgrade(self);
        std::thread::spawn(move || read_loop(me, out, generation));

        let init = self.request(
            "initialize",
            json!({ "protocolVersion": PROTOCOL, "capabilities": { "roots": { "listChanged": false } }, "clientInfo": { "name": "aca-gateway", "version": env!("CARGO_PKG_VERSION") } }),
        )?;
        if init.get("error").is_some() {
            return Err(tr!("握手被拒:{}", "handshake refused: {}", init["error"]));
        }
        self.write(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        let tools = self.list_tools()?;
        *self.tools.lock().unwrap() = tools;
        *self.state.lock().unwrap() = State::Ready;
        Ok(())
    }

    /// 拉一遍工具列表(分页的接着拉)
    fn list_tools(&self) -> Res<Vec<Value>> {
        let mut all = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params)?;
            let res = &r["result"];
            all.extend(res["tools"].as_array().cloned().unwrap_or_default());
            match res["nextCursor"].as_str() {
                Some(c) if !c.is_empty() && all.len() < 10_000 => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(all)
    }

    /// 自己问一句,等回答(最多 [`START_TIMEOUT`])
    fn request(&self, method: &str, params: Value) -> Res<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, Waiter::Sync(tx));
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        rx.recv_timeout(START_TIMEOUT).map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            tr!("{method} 没回答", "no answer to {method}")
        })
    }

    fn write(&self, msg: &Value) -> Res<()> {
        let mut g = self.stdin.lock().unwrap();
        let w = g.as_mut().ok_or(t!("它没在跑", "it isn't running"))?;
        writeln!(w, "{msg}").and_then(|_| w.flush()).map_err(|e| tr!("写不进去:{e}", "can't write to it: {e}"))
    }

    /// 替连接转一个工具调用(不等:回答来了由读线程交给它)。`params` 里的工具名已经换回它自己的
    pub fn forward(&self, client: &Arc<Client>, id: Value, mut params: Value, tool: &str) {
        let gid = self.next.fetch_add(1, Ordering::SeqCst);
        // progress 令牌换成 gateway 的,回来再换回去
        if let Some(tok) = params.pointer("/_meta/progressToken").cloned() {
            let mine = format!("gw-{gid}");
            self.progress.lock().unwrap().insert(mine.clone(), (Arc::downgrade(client), tok));
            params["_meta"]["progressToken"] = json!(mine);
        }
        self.pending.lock().unwrap().insert(
            gid,
            Waiter::Client { client: Arc::downgrade(client), id: id.clone(), started: Instant::now(), tool: tool.to_string() },
        );
        if let Err(e) = self.write(&json!({ "jsonrpc": "2.0", "id": gid, "method": "tools/call", "params": params })) {
            self.pending.lock().unwrap().remove(&gid);
            client.send(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": e } }));
        }
    }

    /// 连接说不要了(notifications/cancelled):找到它那条,告诉下面
    pub fn cancel(&self, client: &Arc<Client>, id: &Value) {
        let gid = self.pending.lock().unwrap().iter().find_map(|(g, w)| match w {
            Waiter::Client { client: c, id: i, .. } if i == id && c.upgrade().is_some_and(|c| Arc::ptr_eq(&c, client)) => Some(*g),
            _ => None,
        });
        if let Some(g) = gid {
            let _ = self.write(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": g } }));
        }
    }

    /// 停掉(下次有人要再起),连败的记录清掉
    pub fn reset(&self) {
        self.stop_child();
        *self.state.lock().unwrap() = State::Idle;
    }

    pub fn stop_child(&self) {
        self.stdin.lock().unwrap().take();
        if let Some(mut c) = self.child.lock().unwrap().take() {
            unsafe { libc::killpg(c.id() as i32, libc::SIGTERM) };
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }

    fn log_name(&self) -> String {
        if self.session.is_empty() { self.spec.name.clone() } else { format!("{}-{}", self.spec.name, self.session) }
    }

    fn note(&self, msg: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_of(&self.log_name())) {
            let _ = writeln!(f, "--- {} {msg}", paths::now());
        }
    }

    /// 它反过来问的
    fn answer(&self, msg: &Value) {
        let id = msg["id"].clone();
        let reply = match msg["method"].as_str().unwrap_or("") {
            "roots/list" => {
                let roots =
                    if self.root.is_empty() { json!([]) } else { json!([{ "uri": format!("file://{}", self.root), "name": "session" }]) };
                json!({ "jsonrpc": "2.0", "id": id, "result": { "roots": roots } })
            }
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            m => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("aca gateway doesn't support {m}") } }),
        };
        let _ = self.write(&reply);
    }
}

/// 读它的标准输出:回答交给等着的,它问的回它,通知照管
fn read_loop(me: Weak<Down>, out: std::process::ChildStdout, generation: u64) {
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        let Some(d) = me.upgrade() else { return };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            d.note(&format!("看不懂的一行:{}", line.chars().take(200).collect::<String>()));
            continue;
        };
        let is_reply = msg.get("result").is_some() || msg.get("error").is_some();
        match (msg.get("id"), msg["method"].as_str()) {
            (Some(id), None) if is_reply => {
                let Some(gid) = id.as_u64() else { continue };
                let w = d.pending.lock().unwrap().remove(&gid);
                match w {
                    Some(Waiter::Sync(tx)) => {
                        let _ = tx.send(msg);
                    }
                    Some(Waiter::Client { client, id, started, tool }) => {
                        let ok = msg.get("error").is_none() && msg["result"]["isError"].as_bool() != Some(true);
                        let mut reply = msg.clone();
                        reply["id"] = id;
                        let session = client.upgrade().map(|c| {
                            c.send(&reply);
                            c.session.clone()
                        });
                        log_call(&d.spec.name, &tool, session.as_deref().unwrap_or(""), started.elapsed(), ok);
                    }
                    None => {}
                }
            }
            (Some(_), Some(_)) => d.answer(&msg),
            (None, Some("notifications/tools/list_changed")) => {
                let d2 = d.clone();
                std::thread::spawn(move || {
                    if let Ok(t) = d2.list_tools() {
                        *d2.tools.lock().unwrap() = t;
                        (d2.on_change)();
                    }
                });
            }
            (None, Some("notifications/progress")) => {
                let tok = msg["params"]["progressToken"].as_str().unwrap_or("").to_string();
                let found = d.progress.lock().unwrap().get(&tok).cloned();
                if let Some((client, orig)) = found
                    && let Some(c) = client.upgrade()
                {
                    let mut m = msg.clone();
                    m["params"]["progressToken"] = orig;
                    c.send(&m);
                }
            }
            (None, Some("notifications/message")) => d.note(&msg["params"].to_string()),
            _ => {}
        }
    }
    // 退出了:这一代的状态记成失败,等着的都回出错
    let Some(d) = me.upgrade() else { return };
    if d.generation.load(Ordering::SeqCst) == generation {
        let was_ready = d.state() == State::Ready;
        d.note("退出了");
        d.stdin.lock().unwrap().take();
        if was_ready {
            *d.state.lock().unwrap() = State::Failed { msg: t!("退出了", "exited").into(), at: Instant::now(), fails: 1 };
        }
    }
    let waiting: Vec<(u64, Waiter)> = d.pending.lock().unwrap().drain().collect();
    for (_, w) in waiting {
        if let Waiter::Client { client, id, .. } = w
            && let Some(c) = client.upgrade()
        {
            c.send(
                &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": tr!("{} 退出了", "{} exited", d.spec.name) } }),
            );
        }
    }
}

/// 一次调用记一行(谁在用、调了什么、多久、成没成):status 和界面从这看
fn log_call(server: &str, tool: &str, session: &str, took: Duration, ok: bool) {
    let line = json!({ "ts": paths::now(), "session": session, "server": server, "tool": tool, "ms": took.as_millis() as u64, "ok": ok });
    let path = calls_log();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}
