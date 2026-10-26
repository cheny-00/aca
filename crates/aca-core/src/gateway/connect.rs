//! 转接(`aca plugin gateway connect`):agent 起的 MCP 服务就是它。stdio 进,gateway 的 Unix socket 出,只转字节,
//! 不跑 MCP —— MCP 还是 gateway 下面那一份。连上先说一句「我是哪个会话」(环境里的 `ACA_SESSION_ID`)。
//! gateway 没在跑:加锁(两个 agent 同时起,只拉起一个),再看一眼,还没有就起一个,等它听上了再连。

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
    // agent → gateway:agent 关了输入就半关 socket,gateway 那头收到结束
    std::thread::spawn(move || {
        let _ = pump(&mut std::io::stdin().lock(), &mut w);
        let _ = w.shutdown(std::net::Shutdown::Write);
    });
    // gateway → agent:gateway 那头断了,这个进程也就结束(agent 会看到服务退出)
    let mut r = s;
    let _ = pump(&mut r, &mut std::io::stdout().lock());
    Ok(0)
}

/// 照搬字节,每读到一块就写出去、冲掉(MCP 一行一条,不能攒着)
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

/// 连上在跑的 gateway;没在跑就起一个
pub fn reach() -> Res<UnixStream> {
    let sock = sock_path();
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(lock_path()).map_err(|e| e.to_string())?;
    // 排队:别的转接正在起它的话,等它起完
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("gateway.log")).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    // 自成一组:起它的这个转接(和 agent)退了,它接着跑
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
