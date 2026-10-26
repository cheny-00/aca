//! 读进程:窗格里跑的是哪家 agent、它是带着什么命令行和环境起的。
//!
//! 三处要用:列表认「shell 里手动起的 claude / codex 是哪个账号」;接管一个不是 aca 起的会话([`crate::adopt`]);
//! 停的时候找到 agent 进程(它不一定是窗格的根进程,外面可能包着一层 shell)。
//! 只读得到自己的进程。读到的环境只拿来找配置目录这一类,不留存、不打印。

use std::collections::{HashMap, HashSet};
use std::process::Command;

use regex::Regex;

use crate::agent;

/// 进程表:父进程 → 前台的子进程(pid, 名字)。前台 = 有控制终端、在终端的前台进程组里(同 `ps` 的 stat 带 +)
pub type Kids = HashMap<u32, Vec<(u32, String)>>;

fn sh(args: &[&str]) -> String {
    Command::new(args[0]).args(&args[1..]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// 直接问系统(macOS 的 libproc、Linux 的 /proc,一千多个进程不到 1ms);读不了才跑 `ps`:
/// 它要挨个读每个进程的启动参数,同样多的进程 ~50ms
pub fn fg_kids() -> Kids {
    sys_fg_kids().unwrap_or_else(|| parse_ps(&sh(&["ps", "-ax", "-o", "pid=,ppid=,stat=,comm="])))
}

/// 名字是程序文件名(同 tmux 的 pane_current_command):Claude 是版本号、登录 shell 是 zsh 不是 -zsh
#[cfg(target_os = "macos")]
fn sys_fg_kids() -> Option<Kids> {
    // sys/proc_info.h:有控制终端
    const PROC_FLAG_CONTROLT: u32 = 0x80;
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return None;
    }
    // 两次调用之间可能又起了进程,多留些位置
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
        // 刚退出的读不到,跳过
        if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut bi).cast(), size) } != size {
            continue;
        }
        if bi.pbi_flags & PROC_FLAG_CONTROLT == 0 || bi.pbi_pgid != bi.e_tpgid {
            continue;
        }
        // pbi_name 长一些(32 字),空的时候用 pbi_comm(16 字)
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
        // 刚退出的读不到,跳过
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

/// Linux 的 /proc/<pid>/stat:`pid (名字) 状态 ppid pgrp session tty_nr tpgid …`,前台的才返回 (ppid, 名字)。
/// 名字里可以有空格和括号,取到最后一个 `)`;tpgid 是终端的前台进程组(没有终端是 -1),等于自己的 pgrp 就是前台
#[cfg(any(target_os = "linux", test))]
fn fg_stat(stat: &str) -> Option<(u32, String)> {
    let (head, rest) = stat.rsplit_once(')')?;
    let name = head.split_once('(')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ppid = f.get(1)?.parse().ok()?;
    let (pgrp, tpgid) = (f.get(2)?.parse::<i64>().ok()?, f.get(5)?.parse::<i64>().ok()?);
    (tpgid > 0 && pgrp == tpgid).then(|| (ppid, name.to_string()))
}

/// `ps -ax -o pid=,ppid=,stat=,comm=` 的输出 → 进程表
pub fn parse_ps(out: &str) -> Kids {
    let mut kids = Kids::new();
    for l in out.lines() {
        let mut f = l.split_whitespace();
        let (Some(pid), Some(ppid), Some(stat)) = (f.next(), f.next(), f.next()) else { continue };
        let (Ok(pid), Ok(ppid)) = (pid.parse(), ppid.parse()) else { continue };
        if !stat.contains('+') {
            continue;
        }
        // 路径里可能有空格
        let comm = f.collect::<Vec<_>>().join(" ");
        kids.entry(ppid).or_default().push((pid, comm.rsplit('/').next().unwrap_or("").to_string()));
    }
    kids
}

/// pid 底下前台跑着的 agent(在 shell 里手动起的、外面包着脚本的):(哪一家, 它的 pid)。
/// 哪个进程名是哪一家,各家自己说([`agent::Agent::runs_as`])
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

/// 这个进程自己的名字(它得在前台);不在表里 = None
pub fn name_of(kids: &Kids, pid: u32) -> Option<&str> {
    kids.values().flatten().find(|(p, _)| *p == pid).map(|(_, n)| n.as_str())
}

/// 窗格里的 agent:窗格的根进程自己就是(aca 起的都是这样),不然看它底下的。`pid` 是窗格的根进程
pub fn agent_at(kids: &Kids, pid: u32) -> Option<(&'static str, u32)> {
    if pid == 0 {
        return None;
    }
    name_of(kids, pid).and_then(agent::by_comm).map(|a| (a, pid)).or_else(|| agent_under(kids, pid))
}

/// 还在不在(发得了信号就算在)
pub fn alive(pid: u32) -> bool {
    pid > 0 && (unsafe { libc::kill(pid as i32, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// 一个进程起的时候带的命令行和环境,一个词一项、原样(值里的空格都在)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Image {
    /// 最后一次出现的算数
    pub fn var(&self, name: &str) -> Option<&str> {
        self.env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn pairs(env: Vec<String>) -> Vec<(String, String)> {
    env.into_iter().filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect()
}

/// macOS 的 `kern.procargs2`:argc(一个 int)、程序路径、补齐的 0、argc 个参数、环境,都以 0 结尾;
/// 环境后面隔一个空串还有系统自己塞的几项(`executable_path=…`),不要
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
    // 别人的进程、刚退出的:读不到
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

/// 这个进程的命令行和环境;读不到(不是自己的进程、已经退了)= None。
/// macOS 上系统自带的程序(`/bin/sleep`、`/bin/sh` 跑着的脚本)只给命令行,环境是空的;claude / codex 不是系统自带的,读得到
pub fn image(pid: u32) -> Option<Image> {
    if pid == 0 { None } else { sys_image(pid) }
}

/// `ps -E` 打出来的那一长串(命令行接着环境,空格隔开)里找 `名字=值`,最后一次出现的
fn var_in_ps(text: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"\b{}=(?:"([^"]*)"|'([^']*)'|([^\s;"']+))"#, regex::escape(name))).ok()?;
    let c = re.captures_iter(text).last()?;
    let v = c.get(1).or(c.get(2)).or(c.get(3)).map_or("", |m| m.as_str());
    (!v.is_empty()).then(|| v.to_string())
}

/// 进程环境里的一个变量(认账号用:`CLAUDE_CONFIG_DIR` / `CODEX_HOME`);没设 = None。
/// 直接问系统问不到时退回 `ps -E`(那一长串没法分清值里的空格,凑合)
pub fn env_var(pid: u32, name: &str) -> Option<String> {
    match image(pid) {
        Some(im) => im.var(name).filter(|v| !v.is_empty()).map(String::from),
        None => var_in_ps(&sh(&["ps", "-E", "-ww", "-o", "command=", "-p", &pid.to_string()]), name),
    }
}

/// 放到后台、不等它:自成一个进程组(起它的退了、终端断了它接着跑),三个口接哪儿调用的自己定。
/// 它退了由一个线程收掉 —— 不收的话,开着好几天的界面底下每起 / 删一个会话就多挂一个僵尸,一直到界面退出
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
        assert_eq!(agent_under(&kids, 100), Some(("claude", 101))); // 进程名就是版本号
        assert_eq!(agent_under(&kids, 200), Some(("codex", 202))); // 包在脚本里的也找得到
        assert_eq!(agent_under(&kids, 300), None); // 在后台的不算
        assert_eq!(agent_under(&kids, 400), None); // 桌面 App 不是
        // 窗格的根进程自己就是 agent(aca 起的):认它,不往下找
        assert_eq!(agent_at(&kids, 500), Some(("claude", 500)));
        assert_eq!(agent_at(&kids, 100), Some(("claude", 101)));
        assert_eq!((agent_at(&kids, 300), agent_at(&kids, 0)), (None, None));
        assert_eq!((name_of(&kids, 202), name_of(&kids, 100)), (Some("codex"), None));
    }

    #[test]
    fn foreground_is_read_from_a_linux_stat_line() {
        assert_eq!(fg_stat("101 (2.1.284) S 100 101 100 34817 101 4194304 …"), Some((100, "2.1.284".into())));
        assert_eq!(fg_stat("300 (zsh) S 1 300 300 34818 302 4194560 …"), None); // 前台是别的进程组
        assert_eq!(fg_stat("50 (tmux: server) S 1 50 50 0 -1 4194624 …"), None); // 没有终端
        assert_eq!(fg_stat("7 (a) b (c)) R 6 7 6 34819 7 0"), Some((6, "a) b (c)".into()))); // 名字里的括号
        assert_eq!(fg_stat("garbage"), None);
    }

    #[test]
    fn the_command_line_and_environment_come_back_word_for_word() {
        // argc = 3;程序路径后面补了几个 0;环境之后隔一个空串是系统自己塞的
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend(b"/opt/bin/claude\0\0\0\0claude\0--model\0a b\0CLAUDE_CONFIG_DIR=/h/my dir\0X=1\0X=2\0EMPTY=\0\0executable_path=/opt/bin/claude\0");
        let im = parse_procargs(&buf).unwrap();
        assert_eq!(im.argv, ["claude", "--model", "a b"]);
        assert_eq!(im.var("CLAUDE_CONFIG_DIR"), Some("/h/my dir"));
        assert_eq!((im.var("X"), im.var("EMPTY"), im.var("executable_path")), (Some("2"), Some(""), None));
        // 截断了的、不是这个形状的:不猜
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
        // 在支持的系统上读得到自己:命令行第一项是测试程序
        if let Some(im) = image(me) {
            assert!(!im.argv.is_empty() && im.var("PATH").is_some(), "{:?}", im.argv);
        }
    }
}
