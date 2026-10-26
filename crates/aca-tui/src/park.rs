//! 会话里另开的窗口 / 窗格(prefix c、prefix % / "):列表里挂在会话下面,对它的操作只动这一格。
//! agent 那一格的停 / 重启是 `aca_core::session` 的事(也在原地,别的窗口不受影响)。
//! - `D`:停下这一格。程序停掉,窗格连最后的画面留着(remain-on-exit),列表里标 ■
//! - `⏎` 停着的那格 / `R`:在原地照它原来的命令再启动
//! - `d` 关一格:先把里面的进程收干净 —— tmux 关窗格只发 SIGHUP,不理它的(nohup、trap、有的 MCP)会留下来接着跑,
//!   窗格没了、哪儿都找不到

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

use aca_core::{t, tr};

use crate::data::tmux_cmd;

/// 要停 / 再启动的那一格
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pane {
    /// tmux 窗格 id(%12)
    pub id: String,
    /// 列表里怎么叫它(#2「zsh」),说话用
    pub label: String,
}

/// tmux 命令,失败时返回它说的
fn run(args: &[&str]) -> Result<String, String> {
    let o = tmux_cmd().args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// 窗格的几样
struct Info {
    pid: u32,
    dead: bool,
}

fn info(pane: &str) -> Result<Info, String> {
    let out = run(&["display-message", "-p", "-t", pane, "#{pane_pid}\t#{pane_dead}"])?;
    let (pid, dead) = out.split_once('\t').ok_or_else(|| tr!("找不到窗格 {pane}", "pane {pane} not found"))?;
    Ok(Info { pid: pid.parse().unwrap_or(0), dead: dead == "1" })
}

/// 进程表里 root 和它底下所有的进程
fn tree(root: u32) -> Vec<u32> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for l in out.lines() {
        let mut f = l.split_whitespace().filter_map(|x| x.parse::<u32>().ok());
        if let (Some(pid), Some(ppid)) = (f.next(), f.next()) {
            kids.entry(ppid).or_default().push(pid);
        }
    }
    let mut all = vec![root];
    let mut i = 0;
    while i < all.len() {
        all.extend(kids.get(&all[i]).into_iter().flatten().copied().filter(|&k| k > 1));
        i += 1;
    }
    all
}

fn signal(sig: &str, pids: &[u32]) {
    if !pids.is_empty() {
        let _ = Command::new("kill").arg("-s").arg(sig).args(pids.iter().map(u32::to_string)).stderr(std::process::Stdio::null()).status();
    }
}

/// 收掉 root 和它底下的进程:先 SIGHUP + SIGTERM(交互 shell 不理 TERM、Claude 收到 TERM 会好好收尾)。
/// 不等它们退(一个不肯退的 MCP 就得干等):两秒后还在的 SIGKILL,交给一个脱离界面的 sh 去做 ——
/// 弹窗里的界面进了会话就退了,等不到两秒。自己(界面开在要停的那格里)不杀
pub fn kill_tree(root: u32) {
    use std::process::Stdio;
    if root <= 1 {
        return;
    }
    let me = std::process::id();
    let pids: Vec<u32> = tree(root).into_iter().filter(|&p| p != me).collect();
    signal("HUP", &pids);
    signal("TERM", &pids);
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
    let _ = aca_core::proc::detach(
        Command::new("sh")
            .args(["-c", &format!("sleep 2; kill -9 {list} 2>/dev/null")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

/// 等它变成停着的格子(进程退了 tmux 才标上),最多等 ms
fn wait_dead(pane: &str, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until && info(pane).is_ok_and(|i| !i.dead) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// D:停下这一格(见文件头)
pub fn park(p: &Pane) -> Result<(), String> {
    let i = info(&p.id)?;
    if i.dead {
        return Err(t!("已停止", "already stopped").into());
    }
    run(&["set-option", "-p", "-t", &p.id, "remain-on-exit", "on"])?;
    kill_tree(i.pid);
    // 一般零点几秒就退了:等一小会儿,刷新出来就是 ■;没退完的下一次刷新再标
    wait_dead(&p.id, 400);
    Ok(())
}

/// ⏎ 停着的那格 / R:在原地照原来的命令再启动。在跑的先停掉
pub fn respawn(p: &Pane) -> Result<(), String> {
    let i = info(&p.id)?;
    if !i.dead {
        run(&["set-option", "-p", "-t", &p.id, "remain-on-exit", "on"])?;
        kill_tree(i.pid);
        // 给它一点时间好好收尾;还没退的 respawn -k 替它关
        wait_dead(&p.id, 1000);
    }
    // 还开着 remain-on-exit:起不来、自己退了也留在原地,看得见、能再 ⏎
    run(&["respawn-pane", "-k", "-t", &p.id]).map(drop)
}

/// d 关一格:先收掉里面的进程,再关(停着的格子进程早没了,直接关)
pub fn close(pane: &str) -> Result<(), String> {
    let i = info(pane)?;
    if !i.dead {
        kill_tree(i.pid);
    }
    match run(&["kill-pane", "-t", pane]) {
        // 进程退了窗格自己就关了
        Err(_) if info(pane).is_err() => Ok(()),
        r => r.map(drop),
    }
}

/// d 删普通 tmux 会话:先收掉每一格里的进程。target 是 tmux 认的会话写法(`=名字` / `$3`)
pub fn close_session(target: &str) {
    let out = run(&["list-panes", "-s", "-t", target, "-F", "#{pane_pid}\t#{pane_dead}"]).unwrap_or_default();
    for (pid, dead) in out.lines().filter_map(|l| l.split_once('\t')) {
        if dead != "1" {
            kill_tree(pid.parse().unwrap_or(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_always_starts_with_its_root() {
        // 自己这个进程:至少有自己;一个不存在的 pid 底下没有别的
        assert_eq!(tree(std::process::id())[0], std::process::id());
        assert_eq!(tree(u32::MAX), [u32::MAX]);
    }
}
