//! Windows / panes opened inside a session (prefix c, prefix % / "): listed under the session; actions on one touch only that pane.
//! Stopping / restarting the agent's pane is up to `aca_core::session` (also in place, other windows unaffected).
//! - `D`: stop this pane. The program stops, the pane stays with its last screen (remain-on-exit), the list marks it ■
//! - `⏎` on a stopped pane / `R`: start its original command again in place
//! - `d` closes a pane: first clean up the processes inside; tmux closing a pane only sends SIGHUP, and the ones that ignore it
//!   (nohup, trap, some MCPs) stay behind running, with the pane gone and nowhere to be found

use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

use aca_core::{t, tr};

use crate::data::tmux_cmd;

/// The pane to stop / start again
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pane {
    /// tmux pane id (%12)
    pub id: String,
    /// What the list calls it (#2 "zsh"), for messages
    pub label: String,
}

/// A tmux command; on failure returns what it said
fn run(args: &[&str]) -> Result<String, String> {
    let o = tmux_cmd().args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// A few things about a pane
struct Info {
    pid: u32,
    dead: bool,
}

fn info(pane: &str) -> Result<Info, String> {
    let out = run(&["display-message", "-p", "-t", pane, "#{pane_pid}\t#{pane_dead}"])?;
    let (pid, dead) = out.split_once('\t').ok_or_else(|| tr!("找不到窗格 {pane}", "pane {pane} not found"))?;
    Ok(Info { pid: pid.parse().unwrap_or(0), dead: dead == "1" })
}

/// root and every process under it in the process table
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

/// Clean up root and the processes under it: first SIGHUP + SIGTERM (an interactive shell ignores TERM, Claude wraps up
/// properly on TERM).
/// Doesn't wait for them to exit (one MCP refusing to would leave us just waiting): SIGKILL for those still there after two
/// seconds, done by an sh detached from the interface, since the interface in a popup exits on entering a session and won't
/// last two seconds. Never kills itself (the interface open in the pane being stopped)
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

/// Wait for it to become a stopped pane (tmux marks it only once the process has exited), at most ms
fn wait_dead(pane: &str, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until && info(pane).is_ok_and(|i| !i.dead) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// D: stop this pane (see the file header)
pub fn park(p: &Pane) -> Result<(), String> {
    let i = info(&p.id)?;
    if i.dead {
        return Err(t!("已停止", "already stopped").into());
    }
    run(&["set-option", "-p", "-t", &p.id, "remain-on-exit", "on"])?;
    kill_tree(i.pid);
    // It usually exits within a fraction of a second: wait a little so the refresh shows ■; one not done yet is marked on the
    // next refresh
    wait_dead(&p.id, 400);
    Ok(())
}

/// ⏎ on a stopped pane / R: start its original command again in place. A running one is stopped first
pub fn respawn(p: &Pane) -> Result<(), String> {
    let i = info(&p.id)?;
    if !i.dead {
        run(&["set-option", "-p", "-t", &p.id, "remain-on-exit", "on"])?;
        kill_tree(i.pid);
        // Give it a moment to wrap up properly; respawn -k closes whatever hasn't exited
        wait_dead(&p.id, 1000);
    }
    // remain-on-exit is still on: if it fails to start or exits by itself it stays in place, visible, ready for ⏎ again
    run(&["respawn-pane", "-k", "-t", &p.id]).map(drop)
}

/// d closes a pane: first clean up the processes inside, then close it (a stopped pane's process is long gone, so just close)
pub fn close(pane: &str) -> Result<(), String> {
    let i = info(pane)?;
    if !i.dead {
        kill_tree(i.pid);
    }
    match run(&["kill-pane", "-t", pane]) {
        // The pane closed by itself once the process exited
        Err(_) if info(pane).is_err() => Ok(()),
        r => r.map(drop),
    }
}

/// d deleting an ordinary tmux session: first clean up the processes in every pane. target is a session target tmux
/// understands (`=NAME` / `$3`)
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
        // This process: at least itself; a pid that doesn't exist has nothing under it
        assert_eq!(tree(std::process::id())[0], std::process::id());
        assert_eq!(tree(u32::MAX), [u32::MAX]);
    }
}
