//! One MCP server under the gateway (a child process, stdio, one JSON-RPC message per line).
//!
//! When several connections share it: the gateway issues request ids anew (whose request it is and its original id are kept in
//! `pending`, and swapped back on reply); the handshake (initialize) happens once, the tool list is cached and fetched again when
//! it says it changed (list_changed); progress tokens are swapped for the gateway's own too. What it asks in return: roots/list
//! gets an empty answer (a per-session one gets that session's directory), anything else (sampling, elicitation) gets "not
//! supported". When the process exits, everything waiting for its answer gets an error; the next one to need it starts it again,
//! and after consecutive failures it waits a while (2, 4, 8… up to 30 seconds); after 5 failures in a row it stops starting itself.

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
/// After this many failures in a row it no longer starts itself (needs `aca plugin gateway restart <name>` or a config change)
pub(crate) const GIVE_UP: u32 = 5;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum State {
    Idle,
    Ready,
    Failed { msg: String, at: Instant, fails: u32 },
}

/// A request waiting for an answer
enum Waiter {
    /// Asked by the gateway itself (handshake, listing tools): the answer goes to whoever is waiting
    Sync(mpsc::Sender<Value>),
    /// Forwarded for a connection: the answer gets its original id back and goes to it
    Client { client: Weak<Client>, id: Value, started: Instant, tool: String },
}

pub(crate) struct Down {
    pub spec: Spec,
    /// For a per-session one, that session; empty for shared ones
    pub session: String,
    /// For a per-session one: the session's directory (the answer when it asks for roots)
    root: String,
    state: Mutex<State>,
    tools: Mutex<Vec<Value>>,
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, Waiter>>,
    /// The gateway's progress token → (which connection, its original token)
    progress: Mutex<HashMap<String, (Weak<Client>, Value)>>,
    next: AtomicU64,
    /// Which start this is: when an old process's reader thread wraps up, it mustn't touch the new process's state
    generation: AtomicU64,
    starting: Mutex<()>,
    /// Its tools changed: the gateway goes and tells the connected agents
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

    /// Fine if it's running; if it never started or exited, start one (one that failed goes by its backoff time). Returns the reason
    pub fn ensure(self: &Arc<Self>) -> Res<()> {
        let _one = self.starting.lock().unwrap();
        match self.state() {
            State::Ready => return Ok(()),
            State::Failed { msg, fails, .. } if fails >= GIVE_UP => {
                return Err(tr!("连续失败 {fails} 次,已停止:{msg}", "failed {fails} times in a row, stopped: {msg}"));
            }
            State::Failed { msg, at, fails } if at.elapsed() < Duration::from_secs((1u64 << fails.min(5)).min(30)) => {
                return Err(tr!("刚刚失败,稍后再启动:{msg}", "failed just now; will retry shortly: {msg}"));
            }
            _ => {}
        }
        let r = self.start();
        if let Err(msg) = &r {
            let fails = match self.state() {
                State::Failed { fails, .. } => fails + 1,
                _ => 1,
            };
            self.note(&tr!("启动失败:{msg}", "failed to start: {msg}"));
            self.stop_child();
            *self.state.lock().unwrap() = State::Failed { msg: msg.clone(), at: Instant::now(), fails };
        }
        r
    }

    fn start(self: &Arc<Self>) -> Res<()> {
        let log = log_of(&self.log_name());
        let _ = std::fs::create_dir_all(log.parent().unwrap());
        let err = std::fs::OpenOptions::new().create(true).append(true).open(&log).map_err(|e| e.to_string())?;
        self.note(&tr!("启动:{} {}", "starting: {} {}", self.spec.command, self.spec.args.join(" ")));
        let mut c = Command::new(&self.spec.command);
        c.args(&self.spec.args).envs(self.spec.env.iter().map(|(k, v)| (k, v)));
        if !self.session.is_empty() {
            c.env("ACA_SESSION_ID", &self.session);
        } else {
            // Shared: carries no session's identity (the caller is in each request's _meta)
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
            .map_err(|e| tr!("无法启动 {}:{e}", "can't start {}: {e}", self.spec.command))?;
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
            return Err(tr!("握手被拒绝:{}", "handshake refused: {}", init["error"]));
        }
        self.write(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        let tools = self.list_tools()?;
        *self.tools.lock().unwrap() = tools;
        *self.state.lock().unwrap() = State::Ready;
        Ok(())
    }

    /// Fetch the tool list once (following pages)
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

    /// Ask it something ourselves and wait for the answer (at most [`START_TIMEOUT`])
    fn request(&self, method: &str, params: Value) -> Res<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, Waiter::Sync(tx));
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        rx.recv_timeout(START_TIMEOUT).map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            tr!("{method} 无响应", "no answer to {method}")
        })
    }

    fn write(&self, msg: &Value) -> Res<()> {
        let mut g = self.stdin.lock().unwrap();
        let w = g.as_mut().ok_or(t!("未运行", "it isn't running"))?;
        writeln!(w, "{msg}").and_then(|_| w.flush()).map_err(|e| tr!("无法写入:{e}", "can't write to it: {e}"))
    }

    /// Forward a tool call for a connection (no waiting: the reader thread hands it the answer when it comes). The tool name in
    /// `params` has already been changed back to its own
    pub fn forward(&self, client: &Arc<Client>, id: Value, mut params: Value, tool: &str) {
        let gid = self.next.fetch_add(1, Ordering::SeqCst);
        // Swap the progress token for the gateway's, and back again on the way back
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

    /// The connection says it no longer wants it (notifications/cancelled): find its entry and tell the server below
    pub fn cancel(&self, client: &Arc<Client>, id: &Value) {
        let gid = self.pending.lock().unwrap().iter().find_map(|(g, w)| match w {
            Waiter::Client { client: c, id: i, .. } if i == id && c.upgrade().is_some_and(|c| Arc::ptr_eq(&c, client)) => Some(*g),
            _ => None,
        });
        if let Some(g) = gid {
            let _ = self.write(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": g } }));
        }
    }

    /// Stop it (the next one to need it starts it again), and clear the failure record
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

    /// What it asks in return
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

/// Read its standard output: answers go to whoever is waiting, its questions get answered, notifications are handled
fn read_loop(me: Weak<Down>, out: std::process::ChildStdout, generation: u64) {
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        let Some(d) = me.upgrade() else { return };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            d.note(&tr!("无法解析的一行:{}", "unreadable line: {}", line.chars().take(200).collect::<String>()));
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
    // It exited: record this generation's state as failed, and everything waiting gets an error
    let Some(d) = me.upgrade() else { return };
    if d.generation.load(Ordering::SeqCst) == generation {
        let was_ready = d.state() == State::Ready;
        d.note(t!("已退出", "exited"));
        d.stdin.lock().unwrap().take();
        if was_ready {
            *d.state.lock().unwrap() = State::Failed { msg: t!("已退出", "exited").into(), at: Instant::now(), fails: 1 };
        }
    }
    let waiting: Vec<(u64, Waiter)> = d.pending.lock().unwrap().drain().collect();
    for (_, w) in waiting {
        if let Waiter::Client { client, id, .. } = w
            && let Some(c) = client.upgrade()
        {
            c.send(
                &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": tr!("{} 已退出", "{} exited", d.spec.name) } }),
            );
        }
    }
}

/// One line per call (who's using it, what was called, how long, whether it worked): status and the interface read from here
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
