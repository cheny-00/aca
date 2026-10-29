//! Reading processes: which agent runs in a pane, and which command line and environment it was started with.
//!
//! Used in three places: the list recognizing "which account a claude / codex started by hand in a shell uses"; adopting a
//! session aca didn't start ([`crate::adopt`]); finding the agent process when stopping (it isn't necessarily the pane's root
//! process; there may be a shell around it).
//! Only our own processes can be read. The environment read is only used to find things like the configuration directory; it's
//! neither stored nor printed.

use std::collections::{HashMap, HashSet};
use std::process::Command;

use regex::Regex;

use crate::agent;

/// Process table: parent → foreground children (pid, name). Foreground = has a controlling terminal and is in the terminal's
/// foreground process group (like a + in `ps`'s stat)
pub type Kids = HashMap<u32, Vec<(u32, String)>>;

fn sh(args: &[&str]) -> String {
    Command::new(args[0]).args(&args[1..]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Ask the system directly (libproc on macOS, /proc on Linux: under 1ms for a thousand-odd processes); only run `ps` when that
/// can't be read: it reads every process's launch arguments one by one, ~50ms for the same number of processes
pub fn fg_kids() -> Kids {
    sys_fg_kids().unwrap_or_else(|| parse_ps(&sh(&["ps", "-ax", "-o", "pid=,ppid=,stat=,comm="])))
}

/// The name is the program file name (like tmux's pane_current_command): Claude's is a version number, a login shell is zsh not
/// -zsh
#[cfg(target_os = "macos")]
fn sys_fg_kids() -> Option<Kids> {
    // sys/proc_info.h: has a controlling terminal
    const PROC_FLAG_CONTROLT: u32 = 0x80;
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return None;
    }
    // More processes may start between the two calls; leave some extra room
    let mut pids = vec![0 as libc::pid_t; n as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if n <= 0 {
        return None;
    }
    let cstr = |b: &[libc::c_char]| {
        let b: Vec<u8> = b.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8_lossy(&b).into_owned()
    };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let mut kids = Kids::new();
    for &pid in &pids[..(n as usize).min(pids.len())] {
        let mut bi: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // One that just exited can't be read; skip it
        if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut bi).cast(), size) } != size {
            continue;
        }
        if bi.pbi_flags & PROC_FLAG_CONTROLT == 0 || bi.pbi_pgid != bi.e_tpgid {
            continue;
        }
        // pbi_name is longer (32 bytes); when it's empty, use pbi_comm (16 bytes)
        let name = if bi.pbi_name[0] != 0 { cstr(&bi.pbi_name) } else { cstr(&bi.pbi_comm) };
        kids.entry(bi.pbi_ppid).or_default().push((bi.pbi_pid, name));
    }
    Some(kids)
}

#[cfg(target_os = "linux")]
fn sys_fg_kids() -> Option<Kids> {
    let mut kids = Kids::new();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        // One that just exited can't be read; skip it
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
        if let Some((ppid, name)) = fg_stat(&stat) {
            kids.entry(ppid).or_default().push((pid, name));
        }
    }
    Some(kids)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sys_fg_kids() -> Option<Kids> {
    None
}

/// Linux's /proc/<pid>/stat: `pid (name) state ppid pgrp session tty_nr tpgid …`; returns (ppid, name) only for foreground ones.
/// The name can contain spaces and parentheses, so take up to the last `)`; tpgid is the terminal's foreground process group (-1
/// with no terminal), and equal to its own pgrp means foreground
#[cfg(any(target_os = "linux", test))]
fn fg_stat(stat: &str) -> Option<(u32, String)> {
    let (head, rest) = stat.rsplit_once(')')?;
    let name = head.split_once('(')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ppid = f.get(1)?.parse().ok()?;
    let (pgrp, tpgid) = (f.get(2)?.parse::<i64>().ok()?, f.get(5)?.parse::<i64>().ok()?);
    (tpgid > 0 && pgrp == tpgid).then(|| (ppid, name.to_string()))
}

/// Output of `ps -ax -o pid=,ppid=,stat=,comm=` → process table
pub fn parse_ps(out: &str) -> Kids {
    let mut kids = Kids::new();
    for l in out.lines() {
        let mut f = l.split_whitespace();
        let (Some(pid), Some(ppid), Some(stat)) = (f.next(), f.next(), f.next()) else { continue };
        let (Ok(pid), Ok(ppid)) = (pid.parse(), ppid.parse()) else { continue };
        if !stat.contains('+') {
            continue;
        }
        // The path may contain spaces
        let comm = f.collect::<Vec<_>>().join(" ");
        kids.entry(ppid).or_default().push((pid, comm.rsplit('/').next().unwrap_or("").to_string()));
    }
    kids
}

/// The agent running in the foreground under pid (started by hand in a shell, wrapped in a script): (which tool, its pid).
/// Which process name belongs to which tool is up to each tool ([`agent::Agent::runs_as`])
pub fn agent_under(kids: &Kids, pid: u32) -> Option<(&'static str, u32)> {
    let mut todo = vec![pid];
    let mut seen = HashSet::new();
    while let Some(p) = todo.pop() {
        if !seen.insert(p) {
            continue;
        }
        for (c, name) in kids.get(&p).into_iter().flatten() {
            if let Some(a) = agent::by_comm(name) {
                return Some((a, *c));
            }
            todo.push(*c);
        }
    }
    None
}

/// This process's own name (it must be in the foreground); not in the table = None
pub fn name_of(kids: &Kids, pid: u32) -> Option<&str> {
    kids.values().flatten().find(|(p, _)| *p == pid).map(|(_, n)| n.as_str())
}

/// The agent in a pane: the pane's root process itself (everything aca starts is like that), otherwise look under it. `pid` is
/// the pane's root process
pub fn agent_at(kids: &Kids, pid: u32) -> Option<(&'static str, u32)> {
    if pid == 0 {
        return None;
    }
    name_of(kids, pid).and_then(agent::by_comm).map(|a| (a, pid)).or_else(|| agent_under(kids, pid))
}

/// Whether it's still there (being able to signal it counts)
pub fn alive(pid: u32) -> bool {
    pid > 0 && (unsafe { libc::kill(pid as i32, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// The command line and environment a process was started with, one word per item, as is (spaces inside values kept)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Image {
    /// The last occurrence counts
    pub fn var(&self, name: &str) -> Option<&str> {
        self.env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn pairs(env: Vec<String>) -> Vec<(String, String)> {
    env.into_iter().filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect()
}

/// macOS's `kern.procargs2`: argc (an int), the program path, padding zeros, argc arguments, the environment, each 0-terminated;
/// after the environment, past an empty string, come a few entries the system adds itself (`executable_path=…`), not wanted
#[cfg(any(target_os = "macos", test))]
fn parse_procargs(buf: &[u8]) -> Option<Image> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let start = path_end + rest[path_end..].iter().position(|&b| b != 0)?;
    let mut words = rest[start..].split(|&b| b == 0).map(|w| String::from_utf8_lossy(w).into_owned());
    let argv: Vec<String> = words.by_ref().take(argc).collect();
    if argv.len() < argc {
        return None;
    }
    Some(Image { argv, env: pairs(words.take_while(|w| !w.is_empty()).collect()) })
}

#[cfg(target_os = "macos")]
fn sys_image(pid: u32) -> Option<Image> {
    let mut argmax: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 2, (&raw mut argmax).cast(), &mut len, std::ptr::null_mut(), 0) } != 0 || argmax <= 0 {
        return None;
    }
    let mut buf = vec![0u8; argmax as usize];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    // Someone else's process, or one that just exited: can't be read
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    parse_procargs(&buf[..size.min(buf.len())])
}

#[cfg(target_os = "linux")]
fn sys_image(pid: u32) -> Option<Image> {
    let words = |file: &str| -> Option<Vec<String>> {
        let b = std::fs::read(format!("/proc/{pid}/{file}")).ok()?;
        Some(b.split(|&c| c == 0).filter(|w| !w.is_empty()).map(|w| String::from_utf8_lossy(w).into_owned()).collect())
    };
    Some(Image { argv: words("cmdline")?, env: pairs(words("environ")?) })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sys_image(_pid: u32) -> Option<Image> {
    None
}

/// This process's command line and environment; unreadable (not our process, already exited) = None.
/// On macOS, programs shipped with the system (`/bin/sleep`, scripts run by `/bin/sh`) only give the command line, with an empty
/// environment; claude / codex aren't shipped with the system, so they can be read
pub fn image(pid: u32) -> Option<Image> {
    if pid == 0 { None } else { sys_image(pid) }
}

/// In the long string `ps -E` prints (command line followed by environment, space-separated), find `name=value`, the last
/// occurrence
fn var_in_ps(text: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"\b{}=(?:"([^"]*)"|'([^']*)'|([^\s;"']+))"#, regex::escape(name))).ok()?;
    let c = re.captures_iter(text).last()?;
    let v = c.get(1).or(c.get(2)).or(c.get(3)).map_or("", |m| m.as_str());
    (!v.is_empty()).then(|| v.to_string())
}

/// One variable from the process environment (for recognizing the account: `CLAUDE_CONFIG_DIR` / `CODEX_HOME`); unset = None.
/// Falls back to `ps -E` when the system can't be asked directly (that long string can't tell spaces inside values apart; good
/// enough)
pub fn env_var(pid: u32, name: &str) -> Option<String> {
    match image(pid) {
        Some(im) => im.var(name).filter(|v| !v.is_empty()).map(String::from),
        None => var_in_ps(&sh(&["ps", "-E", "-ww", "-o", "command=", "-p", &pid.to_string()]), name),
    }
}

/// Put in the background without waiting for it: its own process group (it keeps running if whoever started it exits or the
/// terminal goes away); the caller decides where its three standard streams go. When it exits, a thread reaps it — otherwise an
/// interface open for days would collect one more zombie for every session started / deleted, until the interface exits
pub fn detach(cmd: &mut Command) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut child = cmd.process_group(0).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_found_under_the_pane_or_as_the_pane_itself() {
        let ps = "  100     1 Ss   /bin/zsh\n  101   100 S+   /opt/x/2.1.284\n  200     1 Ss   -zsh\n  201   200 S+   /bin/sh\n  202   201 S+   /opt/homebrew/bin/codex\n\
                    300     1 Ss+  zsh\n  301   300 S    claude\n  400     1 S+   /Applications/Codex.app/Contents/MacOS/Codex (Renderer)\n  500     1 Ss+  /opt/x/claude\n";
        let kids = parse_ps(ps);
        assert_eq!(agent_under(&kids, 100), Some(("claude", 101))); // the process name is the version number
        assert_eq!(agent_under(&kids, 200), Some(("codex", 202))); // one wrapped in a script is found too
        assert_eq!(agent_under(&kids, 300), None); // one in the background doesn't count
        assert_eq!(agent_under(&kids, 400), None); // a desktop App isn't one
        // The pane's root process is the agent itself (started by aca): take it, don't look further down
        assert_eq!(agent_at(&kids, 500), Some(("claude", 500)));
        assert_eq!(agent_at(&kids, 100), Some(("claude", 101)));
        assert_eq!((agent_at(&kids, 300), agent_at(&kids, 0)), (None, None));
        assert_eq!((name_of(&kids, 202), name_of(&kids, 100)), (Some("codex"), None));
    }

    #[test]
    fn foreground_is_read_from_a_linux_stat_line() {
        assert_eq!(fg_stat("101 (2.1.284) S 100 101 100 34817 101 4194304 …"), Some((100, "2.1.284".into())));
        assert_eq!(fg_stat("300 (zsh) S 1 300 300 34818 302 4194560 …"), None); // the foreground is another process group
        assert_eq!(fg_stat("50 (tmux: server) S 1 50 50 0 -1 4194624 …"), None); // no terminal
        assert_eq!(fg_stat("7 (a) b (c)) R 6 7 6 34819 7 0"), Some((6, "a) b (c)".into()))); // parentheses in the name
        assert_eq!(fg_stat("garbage"), None);
    }

    #[test]
    fn the_command_line_and_environment_come_back_word_for_word() {
        // argc = 3; a few zeros of padding after the program path; past an empty string after the environment, what the system adds
        // itself
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend(b"/opt/bin/claude\0\0\0\0claude\0--model\0a b\0CLAUDE_CONFIG_DIR=/h/my dir\0X=1\0X=2\0EMPTY=\0\0executable_path=/opt/bin/claude\0");
        let im = parse_procargs(&buf).unwrap();
        assert_eq!(im.argv, ["claude", "--model", "a b"]);
        assert_eq!(im.var("CLAUDE_CONFIG_DIR"), Some("/h/my dir"));
        assert_eq!((im.var("X"), im.var("EMPTY"), im.var("executable_path")), (Some("2"), Some(""), None));
        // Truncated, or not this shape: no guessing
        assert_eq!(parse_procargs(&5i32.to_ne_bytes()), None);
        let mut short = 9i32.to_ne_bytes().to_vec();
        short.extend(b"/bin/x\0x\0");
        assert_eq!(parse_procargs(&short), None);
    }

    #[test]
    fn ps_output_is_only_a_fallback_and_takes_the_last_value() {
        let text = "bash -c export CODEX_HOME=/h/.a; exec codex CODEX_HOME=/h/.b TERM=xterm";
        assert_eq!(var_in_ps(text, "CODEX_HOME").as_deref(), Some("/h/.b"));
        assert_eq!(var_in_ps("claude MY_CODEX_HOME_X=1", "CODEX_HOME"), None);
        assert_eq!(var_in_ps("x CLAUDE_CONFIG_DIR='/h/a b' y", "CLAUDE_CONFIG_DIR").as_deref(), Some("/h/a b"));
    }

    #[test]
    fn this_process_can_read_itself() {
        let me = std::process::id();
        assert!(alive(me) && !alive(0));
        // On a supported system we can read ourselves: the first command line item is the test program
        if let Some(im) = image(me) {
            assert!(!im.argv.is_empty() && im.var("PATH").is_some(), "{:?}", im.argv);
        }
    }
}
