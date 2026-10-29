//! The gateway process (`aca plugin gateway serve`, started by the first relay, see [`super::connect`]).
//!
//! One thread per connection. A connection's first line says who it is: `{"aca":"hello","session":…}` is a relay (followed by
//! the agent's MCP messages), `{"aca":"ctl","cmd":…}` is a control command (status / stop / restart). Toward agents it only does
//! tools: handshake, listing tools (those of the servers below put together, names prefixed with `<server>__`, filtered by what
//! this session can reach), calling tools (forwarded under the original name, with the session added to `_meta`).
//! When the servers below or the configuration change, it tells connected agents the tool list changed (Claude fetches it again,
//! Codex only sees it after a restart).

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

/// A connected agent (through a relay)
pub(crate) struct Client {
    pub id: u64,
    pub session: String,
    since: Instant,
    out: Mutex<UnixStream>,
    /// The tools last listed to it: name → (which server, the original name)
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
    /// (server name, session): the session is empty for shared ones
    downs: Mutex<HashMap<(String, String), Arc<Down>>>,
    specs: Mutex<Vec<Spec>>,
    clients: Mutex<Vec<Arc<Client>>>,
    last: Mutex<Instant>,
    next: AtomicU64,
    started: Instant,
}

/// `aca plugin gateway serve`: runs until nobody has been connected for longer than the set minutes, or someone sends stop
pub fn serve() -> Res<i32> {
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let sock = sock_path();
    // A leftover socket file: if it can't be connected to, nobody is listening, so delete it and create it again
    if sock.exists() {
        if UnixStream::connect(&sock).is_ok() {
            return Err(tr!("gateway 已在运行({})", "the gateway is already running ({})", sock.display()));
        }
        let _ = std::fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock).map_err(|e| tr!("无法监听 {}:{e}", "can't listen on {}: {e}", sock.display()))?;
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
        // One that didn't say hello first (connected straight to the socket): the first line is already an MCP message
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

    /// A connection closed: if this session has no other connections, clean up its per-session servers
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

    /// The configuration changed (config.toml, plugins.toml, plugin registrations): replace the servers that changed and tell
    /// connected agents
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

    /// The tool list changed: tell everyone connected
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

    /// The tools listed to this connection: every server that can reach it (starting ones not yet started, several at once), tool
    /// names prefixed
    fn list(self: &Arc<Self>, client: &Arc<Client>) -> Vec<Value> {
        self.refresh();
        *self.last.lock().unwrap() = Instant::now();
        let (tool, kind, account) = facts(&client.session);
        let wants = decl::wants().unwrap_or_default();
        let reaches = |id: &str| client.session.is_empty() || wants.get(id).is_none_or(|w| w.reaches(&tool, &kind, &account));
        // Ones switched off for this session on the M page aren't offered
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
                    // The plugin-commands server's tool names are <plugin>__<command>: not offered if the plugin can't reach this session
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
            // Not listed yet (or the list changed): list again, then look again
            self.list(client);
            found = client.names.lock().unwrap().get(&name).cloned();
        }
        let Some((down, orig)) = found else {
            let msg = tr!("没有工具 {name}", "no tool {name}");
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
                        // stopped: stopped after failing repeatedly, won't start itself again
                        // (the status bar and the plugins page mark it, see plugin::health)
                        json!({ "name": d.spec.name, "session": d.session, "state": state, "tools": d.tools().len(), "error": error, "stopped": stopped })
                    })
                    .collect();
                // Ones nobody has asked for yet and that aren't started are listed too
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
            // What someone sees changed (one switched off for a session on the M page): everyone connected fetches the tool list again
            "changed" => {
                self.changed();
                json!({ "ok": true })
            }
            other => json!({ "error": format!("unknown command {other}") }),
        }
    }
}

/// The session's tool, which tool kind it starts as, and its account (deciding which plugins can reach it)
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
