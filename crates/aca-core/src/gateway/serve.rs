//! gateway 进程(`aca plugin gateway serve`,由第一个转接拉起来,见 [`super::connect`])。
//!
//! 一个连接一个线程。连接的第一行说它是谁:`{"aca":"hello","session":…}` 是转接(后面是 agent 的 MCP 消息),
//! `{"aca":"ctl","cmd":…}` 是控制命令(status / stop / restart)。对 agent 这头只做工具:握手、列工具(下面各个服务的合起来,
//! 名字加上 `<服务>__` 前缀,按这个会话够得着的过滤)、调工具(换回原来的名字转下去,`_meta` 里加上会话)。
//! 下面的服务、配置改了,告诉连着的 agent 工具列表变了(Claude 会重新拉,Codex 要重启才看得到)。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::down::{Down, State};
use super::{Spec, catalog, dir, pid_path, settings, sock_path};
use crate::paths::{self, Res};
use crate::plugin::{decl, mount};
use crate::tr;

const KNOWN: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const PROTOCOL: &str = "2025-11-25";

/// 一个连上来的 agent(经转接)
pub(crate) struct Client {
    pub id: u64,
    pub session: String,
    since: Instant,
    out: Mutex<UnixStream>,
    /// 上次列给它的工具:名字 → (哪个服务, 原来的名字)
    names: Mutex<HashMap<String, (Arc<Down>, String)>>,
}

impl Client {
    pub fn send(&self, msg: &Value) {
        let mut o = self.out.lock().unwrap();
        let _ = writeln!(o, "{msg}");
        let _ = o.flush();
    }
}

struct Gw {
    /// (服务名, 会话):共用的会话是空的
    downs: Mutex<HashMap<(String, String), Arc<Down>>>,
    specs: Mutex<Vec<Spec>>,
    clients: Mutex<Vec<Arc<Client>>>,
    last: Mutex<Instant>,
    next: AtomicU64,
    started: Instant,
}

/// `aca plugin gateway serve`:跑到没人连着超过设置的分钟数,或者有人发了 stop
pub fn serve() -> Res<i32> {
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let sock = sock_path();
    // 留下来的 socket 文件:连不上就是没人在听,删掉重建
    if sock.exists() {
        if UnixStream::connect(&sock).is_ok() {
            return Err(tr!("gateway 已经在跑({})", "the gateway is already running ({})", sock.display()));
        }
        let _ = std::fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock).map_err(|e| tr!("听不了 {}:{e}", "can't listen on {}: {e}", sock.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    }
    let _ = std::fs::write(pid_path(), std::process::id().to_string());
    let gw = Arc::new(Gw {
        downs: Mutex::new(HashMap::new()),
        specs: Mutex::new(catalog()),
        clients: Mutex::new(vec![]),
        last: Mutex::new(Instant::now()),
        next: AtomicU64::new(1),
        started: Instant::now(),
    });
    let idle = settings().idle;
    let g = gw.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            if g.clients.lock().unwrap().is_empty() && g.last.lock().unwrap().elapsed() > idle {
                g.shutdown();
            }
        }
    });
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let g = gw.clone();
        std::thread::spawn(move || g.handle(conn));
    }
    Ok(0)
}

impl Gw {
    fn shutdown(&self) -> ! {
        for d in self.downs.lock().unwrap().values() {
            d.stop_child();
        }
        let _ = std::fs::remove_file(sock_path());
        let _ = std::fs::remove_file(pid_path());
        std::process::exit(0)
    }

    fn handle(self: &Arc<Self>, conn: UnixStream) {
        let Ok(out) = conn.try_clone() else { return };
        let mut lines = BufReader::new(conn).lines();
        let Some(Ok(first)) = lines.next() else { return };
        let hello: Value = serde_json::from_str(&first).unwrap_or_default();
        if hello["aca"] == "ctl" {
            let reply = self.ctl(&hello);
            let mut o = out;
            let _ = writeln!(o, "{reply}");
            if hello["cmd"] == "stop" {
                self.shutdown();
            }
            return;
        }
        *self.last.lock().unwrap() = Instant::now();
        let client = Arc::new(Client {
            id: self.next.fetch_add(1, Ordering::SeqCst),
            session: hello["session"].as_str().unwrap_or("").to_string(),
            since: Instant::now(),
            out: Mutex::new(out),
            names: Mutex::new(HashMap::new()),
        });
        self.clients.lock().unwrap().push(client.clone());
        // 没先打招呼的(直接连 socket 的):第一行就是 MCP 消息
        if hello["aca"] != "hello" {
            self.on_message(&client, &hello);
        }
        for line in lines {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.on_message(&client, &msg),
                Err(_) => client.send(&json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } })),
            }
        }
        self.gone(&client);
    }

    /// 连接断了:这个会话没有别的连接了,它那几个每会话一份的服务收掉
    fn gone(&self, client: &Arc<Client>) {
        let mut cs = self.clients.lock().unwrap();
        cs.retain(|c| c.id != client.id);
        let still = cs.iter().any(|c| c.session == client.session);
        drop(cs);
        *self.last.lock().unwrap() = Instant::now();
        if !still && !client.session.is_empty() {
            let mut downs = self.downs.lock().unwrap();
            downs.retain(|(_, s), d| {
                let mine = *s == client.session;
                if mine {
                    d.stop_child();
                }
                !mine
            });
        }
    }

    fn on_message(self: &Arc<Self>, client: &Arc<Client>, msg: &Value) {
        let method = msg["method"].as_str().unwrap_or("");
        let Some(id) = msg.get("id").cloned() else {
            if method == "notifications/cancelled" {
                let req = &msg["params"]["requestId"];
                let downs: Vec<Arc<Down>> = client.names.lock().unwrap().values().map(|(d, _)| d.clone()).collect();
                for d in downs {
                    d.cancel(client, req);
                }
            }
            return;
        };
        let reply = |result: Value| client.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        match method {
            "initialize" => {
                let want = msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL);
                let v = if KNOWN.contains(&want) { want } else { PROTOCOL };
                reply(json!({
                    "protocolVersion": v,
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": "aca-gateway", "version": env!("CARGO_PKG_VERSION") },
                }));
            }
            "ping" => reply(json!({})),
            "tools/list" => reply(json!({ "tools": self.list(client) })),
            "tools/call" => self.call(client, id, &msg["params"]),
            _ => client.send(
                &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("method not found: {method}") } }),
            ),
        }
    }

    /// 配置变了(config.toml、plugins.toml、插件登记):换掉变了的服务,告诉连着的 agent
    fn refresh(&self) {
        let now = catalog();
        let mut specs = self.specs.lock().unwrap();
        if *specs == now {
            return;
        }
        let mut downs = self.downs.lock().unwrap();
        downs.retain(|(name, _), d| {
            let keep = now.iter().any(|s| &s.name == name && *s == d.spec);
            if !keep {
                d.stop_child();
            }
            keep
        });
        *specs = now;
        drop(downs);
        drop(specs);
        self.changed();
    }

    /// 工具列表变了:告诉每个连着的
    fn changed(&self) {
        for c in self.clients.lock().unwrap().iter() {
            c.send(&json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }));
        }
    }

    fn down(self: &Arc<Self>, spec: &Spec, session: &str) -> Arc<Down> {
        let who = if spec.per_session { session.to_string() } else { String::new() };
        let mut downs = self.downs.lock().unwrap();
        downs
            .entry((spec.name.clone(), who.clone()))
            .or_insert_with(|| {
                let root = if who.is_empty() { String::new() } else { session_dir(&who) };
                let me = Arc::downgrade(self);
                Down::new(
                    spec.clone(),
                    who,
                    root,
                    Box::new(move || {
                        if let Some(g) = me.upgrade() {
                            g.changed();
                        }
                    }),
                )
            })
            .clone()
    }

    /// 列给这个连接的工具:每个够得着它的服务(没起的先起,几个一起起),工具名加上前缀
    fn list(self: &Arc<Self>, client: &Arc<Client>) -> Vec<Value> {
        self.refresh();
        *self.last.lock().unwrap() = Instant::now();
        let (tool, kind, account) = facts(&client.session);
        let wants = decl::wants().unwrap_or_default();
        let reaches = |id: &str| client.session.is_empty() || wants.get(id).is_none_or(|w| w.reaches(&tool, &kind, &account));
        // M 页里对这个会话关掉的不给
        let off = super::off_for(&client.session);
        let specs: Vec<Spec> = self
            .specs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.plugin.as_deref().is_none_or(reaches) && !off.contains(&s.name))
            .cloned()
            .collect();
        let downs: Vec<Arc<Down>> = specs.iter().map(|s| self.down(s, &client.session)).collect();
        std::thread::scope(|sc| {
            for d in &downs {
                sc.spawn(move || {
                    let _ = d.ensure();
                });
            }
        });
        let reg = crate::plugin::registry();
        let mut names = HashMap::new();
        let mut out = vec![];
        for d in downs.iter().filter(|d| d.state() == State::Ready) {
            for t in d.tools() {
                let orig = t["name"].as_str().unwrap_or("").to_string();
                let shown = if d.spec.tools {
                    // 能力服务的工具名是 <插件>__<能力>:插件够不着这个会话的不给
                    let owner = reg.iter().find(|e| orig.starts_with(&format!("{}__", mount::safe(&e.id))));
                    if owner.is_some_and(|e| !reaches(&e.id)) {
                        continue;
                    }
                    orig.clone()
                } else {
                    format!("{}__{orig}", d.spec.name)
                };
                let mut t = t.clone();
                t["name"] = json!(shown);
                names.insert(shown, (d.clone(), orig));
                out.push(t);
            }
        }
        *client.names.lock().unwrap() = names;
        out
    }

    fn call(self: &Arc<Self>, client: &Arc<Client>, id: Value, params: &Value) {
        *self.last.lock().unwrap() = Instant::now();
        let name = params["name"].as_str().unwrap_or("").to_string();
        let mut found = client.names.lock().unwrap().get(&name).cloned();
        if found.is_none() {
            // 还没列过(或者列表变了):重列一遍再找
            self.list(client);
            found = client.names.lock().unwrap().get(&name).cloned();
        }
        let Some((down, orig)) = found else {
            let msg = tr!("没有 {name} 这个工具", "no tool {name}");
            return client
                .send(&json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": msg }], "isError": true } }));
        };
        if let Err(e) = down.ensure() {
            return client
                .send(&json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": e }], "isError": true } }));
        }
        let mut p = params.clone();
        p["name"] = json!(orig);
        if !p["_meta"].is_object() {
            p["_meta"] = json!({});
        }
        p["_meta"]["dev.aca/session"] = json!(client.session);
        down.forward(client, id, p, &orig);
    }

    fn ctl(self: &Arc<Self>, msg: &Value) -> Value {
        match msg["cmd"].as_str().unwrap_or("") {
            "status" => {
                let clients: Vec<Value> = self
                    .clients
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|c| json!({ "session": c.session, "secs": c.since.elapsed().as_secs() }))
                    .collect();
                let mut servers: Vec<Value> = self
                    .downs
                    .lock()
                    .unwrap()
                    .values()
                    .map(|d| {
                        let (state, error, stopped) = match d.state() {
                            State::Idle => ("idle", String::new(), false),
                            State::Ready => ("ready", String::new(), false),
                            State::Failed { msg, fails, .. } => ("failed", format!("{msg} (×{fails})"), fails >= super::down::GIVE_UP),
                        };
                        // stopped:连着失败停下了,不会再自己起(状态栏、插件页标出来,见 plugin::health)
                        json!({ "name": d.spec.name, "session": d.session, "state": state, "tools": d.tools().len(), "error": error, "stopped": stopped })
                    })
                    .collect();
                // 还没人要过、没起的也列上
                for s in self.specs.lock().unwrap().iter() {
                    if !servers.iter().any(|x| x["name"] == json!(s.name)) {
                        servers.push(json!({ "name": s.name, "session": "", "state": "idle", "tools": 0, "error": "" }));
                    }
                }
                json!({ "pid": std::process::id(), "up": self.started.elapsed().as_secs(), "clients": clients, "servers": servers })
            }
            "restart" => {
                let name = msg["server"].as_str().unwrap_or("");
                let mut n = 0;
                for ((s, _), d) in self.downs.lock().unwrap().iter() {
                    if s == name || name.is_empty() {
                        d.reset();
                        n += 1;
                    }
                }
                json!({ "ok": true, "restarted": n })
            }
            "stop" => json!({ "ok": true }),
            // 给谁看什么变了(M 页按会话关了一个):连着的都重新来拉工具列表
            "changed" => {
                self.changed();
                json!({ "ok": true })
            }
            other => json!({ "error": format!("unknown command {other}") }),
        }
    }
}

/// 会话的工具、按哪家起、账号(决定它够得着哪些插件)
pub(crate) fn facts(session: &str) -> (String, String, String) {
    let Some(s) = (!session.is_empty()).then(|| crate::store::Store::open().ok()?.get(session).ok()).flatten() else {
        return Default::default();
    };
    let kind = crate::config::load().tool(&s.tool).map(|t| t.kind).unwrap_or_default();
    (s.tool, kind, s.account)
}

fn session_dir(session: &str) -> String {
    crate::store::Store::open()
        .ok()
        .and_then(|st| st.get(session).ok())
        .map(|s| s.dir)
        .unwrap_or_else(|| paths::home().to_string_lossy().into_owned())
}
