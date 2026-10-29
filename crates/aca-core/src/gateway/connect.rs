//! The relay (`aca plugin gateway connect`): this is the MCP server agents start. stdio in, the gateway's Unix socket out; it only
//! passes bytes and doesn't run MCP — MCP is still the one copy under the gateway. On connecting it first says "which session I
//! am" (`ACA_SESSION_ID` from the environment). If the gateway isn't running: take a lock (two agents starting at once start only
//! one), look again, start one if there's still none, and connect once it's listening.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use super::{dir, lock_path, sock_path};
use crate::paths::Res;
use crate::{t, tr};

pub fn connect() -> Res<i32> {
    let s = reach()?;
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let hello = json!({ "aca": "hello", "session": std::env::var("ACA_SESSION_ID").unwrap_or_default(), "pid": std::process::id() });
    writeln!(w, "{hello}").map_err(|e| e.to_string())?;
    // agent → gateway: when the agent closes its input, half-close the socket so the gateway side sees the end
    std::thread::spawn(move || {
        let _ = pump(&mut std::io::stdin().lock(), &mut w);
        let _ = w.shutdown(std::net::Shutdown::Write);
    });
    // gateway → agent: when the gateway side closes, this process ends too (the agent sees the server exit)
    let mut r = s;
    let _ = pump(&mut r, &mut std::io::stdout().lock());
    Ok(0)
}

/// Copy bytes as they are, writing out and flushing every chunk read (MCP is one message per line; nothing can be held back)
fn pump(r: &mut impl Read, w: &mut impl Write) -> std::io::Result<()> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        w.write_all(&buf[..n])?;
        w.flush()?;
    }
}

/// Connect to the running gateway; start one if it isn't running
pub fn reach() -> Res<UnixStream> {
    let sock = sock_path();
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(lock_path()).map_err(|e| e.to_string())?;
    // Queue up: if another relay is starting it, wait until it's done
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("gateway.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    // Its own group: it keeps running when the relay that started it (and the agent) exit
    crate::proc::detach(
        Command::new(crate::agent::self_exe()).args(["plugin", "gateway", "serve"]).stdin(Stdio::null()).stdout(log).stderr(err),
    )
    .map_err(|e| tr!("无法启动 gateway:{e}", "can't start the gateway: {e}"))?;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if let Ok(s) = UnixStream::connect(&sock) {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(t!(
        "gateway 已启动但未开始监听(见 state/gateway/gateway.log)",
        "the gateway started but never listened (see state/gateway/gateway.log)"
    )
    .into())
}
