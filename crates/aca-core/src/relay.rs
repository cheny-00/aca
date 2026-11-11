//! `aca pty -- <command…>`: a thin pseudo-terminal between tmux and the agent, so the agent can tell its own state over the
//! program status protocol (OSC 7501, <https://www.superlogical.com/rex/docs/build/program-status>).
//!
//! Why it's needed: tmux doesn't know OSC 7501. It neither answers the feature query (`ESC ] 7501 ; ? ST`) nor passes the
//! reports on. Claude Code only reports after the terminal answers the query, so inside tmux it never turns it on; Codex
//! reports without asking, and tmux drops them. The relay answers the query, takes the reports out of the output and records
//! the root record in the session's state file ([`crate::runtime::Program`]); every other byte passes through untouched, both
//! ways.
//!
//! With a report on record, [`crate::status::merge`] goes by it instead of the screen: pressing Esc to interrupt and approving
//! a permission prompt — the two things hooks don't report — are known the moment they happen. An agent that never reports
//! (one that doesn't speak the protocol, an old version) changes nothing: screen + hooks as before. That is the detection:
//! nothing is configured per tool, a report showing up is what turns it on, for this start of the agent only.
//!
//! Rules: never get in the way. Output is passed on first; recording happens on its own thread, and a failure there is
//! dropped. The relay exits when the agent does, with the agent's exit status.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use serde_json::json;

use crate::{event, paths, runtime};

const PREFIX: &[u8] = b"\x1b]7501;";
/// The answer to the feature query: supported, with the default states and kinds
const REPLY: &[u8] = b"\x1b]7501;?\x1b\\";
/// A sequence longer than this isn't a report (the protocol's own limits are well under it): pass it on as it is
const MAX: usize = 8192;

/// One OSC 7501 sequence taken out of the output
#[derive(Debug, PartialEq)]
pub enum Seq {
    /// The feature query
    Query,
    /// A report: the body after `7501;`
    Report(String),
}

/// Finds OSC 7501 sequences in output that arrives in pieces: a sequence can be cut anywhere, so an unfinished tail is held
/// until the next piece
#[derive(Default)]
pub struct Scan {
    held: Vec<u8>,
}

impl Scan {
    /// The bytes to pass on (everything except the sequences), and the sequences found
    pub fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Vec<Seq>) {
        // Nothing held and no ESC at all: the common case, no copying around
        if self.held.is_empty() && !data.contains(&0x1b) {
            return (data.to_vec(), vec![]);
        }
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(data);
        let (mut out, mut seqs, mut i) = (Vec::with_capacity(buf.len()), vec![], 0);
        loop {
            let rest = &buf[i..];
            let Some(at) = find(rest, PREFIX) else {
                // Hold a tail that could be the start of the prefix
                let keep = (1..PREFIX.len()).rev().find(|&n| rest.ends_with(&PREFIX[..n])).unwrap_or(0);
                out.extend_from_slice(&rest[..rest.len() - keep]);
                self.held = rest[rest.len() - keep..].to_vec();
                break;
            };
            out.extend_from_slice(&rest[..at]);
            let start = at + PREFIX.len();
            match end(&rest[start..]) {
                Some((n, skip)) => {
                    let body = String::from_utf8_lossy(&rest[start..start + n]).into_owned();
                    seqs.push(if body.trim() == "?" { Seq::Query } else { Seq::Report(body) });
                    i += start + n + skip;
                }
                None if rest.len() - start > MAX => {
                    out.extend_from_slice(&rest[at..start]);
                    i += start;
                }
                None => {
                    self.held = rest[at..].to_vec();
                    break;
                }
            }
        }
        (out, seqs)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Where the body ends: (its length, how many terminator bytes to drop). BEL or ST (`ESC \`); an ESC followed by anything
/// else also ends it (a terminal aborts the string there), and that ESC stays in the output. None = not ended yet
fn end(body: &[u8]) -> Option<(usize, usize)> {
    let n = body.iter().position(|&b| b == 0x07 || b == 0x1b)?;
    match (body[n], body.get(n + 1)) {
        (0x07, _) => Some((n, 1)),
        (_, Some(b'\\')) => Some((n, 2)),
        (_, Some(_)) => Some((n, 0)),
        (_, None) => None,
    }
}

/// A report, read by the protocol's rules: a malformed pair is skipped; a `msg` / `title` that isn't base64 of single-line
/// text throws the whole report away; a missing or unknown `state` too
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub state: String,
    /// The record it's about; empty = the root record (the program itself)
    pub id: String,
    pub kind: String,
    pub msg: String,
    pub title: String,
    pub app: String,
}

pub const STATES: &[&str] = &["idle", "working", "done", "blocked", "error"];

pub fn parse(body: &str) -> Option<Report> {
    let mut r = Report::default();
    for pair in body.split(':') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_lowercase()) || !v.bytes().all(value_byte) {
            continue;
        }
        match k {
            "state" => r.state = v.into(),
            "id" => r.id = v.into(),
            "kind" => r.kind = v.into(),
            "app" => r.app = v.into(),
            "msg" => r.msg = text(v)?,
            "title" => r.title = text(v)?,
            _ => {}
        }
    }
    if r.state != "blocked" || !matches!(r.kind.as_str(), "permission" | "question" | "auth") {
        r.kind.clear();
    }
    (STATES.contains(&r.state.as_str()) || r.state == "clear").then_some(r)
}

fn value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b)
}

/// Standard base64 of UTF-8 text with no control characters; padding optional
fn text(v: &str) -> Option<String> {
    let mut out = Vec::with_capacity(v.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for b in v.bytes().filter(|&b| b != b'=') {
        let n = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | n as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.chars().any(char::is_control)).then_some(s)
}

/// Records one report of the root record for this session. Records of the agent's own sub-tasks (`id`, e.g. Claude's subagents)
/// aren't used yet. `clear` with no id removes every record: the state is then unknown, and the list goes back to screen + hooks
pub fn record(id: &str, pid: u32, r: &Report) {
    if id.is_empty() || !r.id.is_empty() {
        return;
    }
    let state = if r.state == "clear" { "" } else { r.state.as_str() };
    let mut changed = false;
    runtime::update(id, |run| {
        let same = run.ps.as_ref().is_some_and(|p| p.pid == pid && p.state == state && p.kind == r.kind && p.msg == r.msg);
        if !same {
            run.ps = Some(runtime::Program { pid, state: state.into(), kind: r.kind.clone(), msg: r.msg.clone(), at: paths::now() });
            changed = true;
        }
    });
    if changed {
        event::emit("agent.status", id, json!({ "state": state, "kind": r.kind, "msg": r.msg }));
    }
}

/// The master side, for the SIGWINCH handler
static MASTER: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_winch(_: libc::c_int) {
    copy_size(0, MASTER.load(Ordering::Relaxed));
}

/// The window size of `from` onto `to` (only system calls: called from a signal handler too). Setting it on the master
/// sends the agent its own SIGWINCH
fn copy_size(from: RawFd, to: RawFd) {
    if to < 0 {
        return;
    }
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(from, libc::TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(to, libc::TIOCSWINSZ, &ws);
        }
    }
}

fn cloexec(fd: RawFd) {
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

/// A new pty pair, starting with the terminal settings and window size of our own terminal (when stdin is one)
fn open_pty() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let (mut m, mut s) = (-1, -1);
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        let mut ws: libc::winsize = std::mem::zeroed();
        let tty = libc::isatty(0) == 1;
        let tp = if tty && libc::tcgetattr(0, &mut t) == 0 { &mut t as *mut libc::termios } else { std::ptr::null_mut() };
        let wp = if tty && libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 { &mut ws as *mut libc::winsize } else { std::ptr::null_mut() };
        if libc::openpty(&mut m, &mut s, std::ptr::null_mut(), tp as _, wp as _) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        cloexec(m);
        cloexec(s);
        Ok((OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)))
    }
}

/// Our own terminal in raw mode (the agent's pty does the line discipline); the old settings come back when dropped
struct Raw(Option<libc::termios>);

impl Raw {
    fn on() -> Raw {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::isatty(0) != 1 || libc::tcgetattr(0, &mut t) != 0 {
                return Raw(None);
            }
            let mut raw = t;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSANOW, &raw);
            Raw(Some(t))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, t);
            }
        }
    }
}

fn code(st: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    st.code().or_else(|| st.signal().map(|s| 128 + s)).unwrap_or(1)
}

/// Runs `argv` behind the relay; returns its exit status
pub fn run(argv: &[String]) -> i32 {
    let Some((prog, args)) = argv.split_first() else {
        eprintln!("aca pty: no command");
        return 2;
    };
    let (master, slave) = match open_pty() {
        Ok(p) => p,
        // No pty to be had: run it directly, without status reports
        Err(_) => return Command::new(prog).args(args).status().map_or(127, code),
    };
    let io = |fd: &OwnedFd| fd.try_clone().map(Stdio::from);
    let (Ok(i), Ok(o), Ok(e)) = (io(&slave), io(&slave), io(&slave)) else { return 1 };
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(i).stdout(o).stderr(e);
    // Its own session with the new pty as its controlling terminal: job control, ^C and SIGWINCH reach it as before
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aca pty: {prog}: {e}");
            return 127;
        }
    };
    drop(slave);
    let raw = Raw::on();
    MASTER.store(master.as_raw_fd(), Ordering::Relaxed);
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_winch as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());
    }
    // The size may have changed between opening the pty and installing the handler
    copy_size(0, master.as_raw_fd());

    let Ok(m) = master.try_clone() else { return 1 };
    let to_agent = Arc::new(Mutex::new(File::from(m)));
    // Keys: our terminal → the agent
    {
        let to_agent = to_agent.clone();
        std::thread::spawn(move || {
            let mut stdin = unsafe { File::from_raw_fd(0) };
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if to_agent.lock().unwrap_or_else(|e| e.into_inner()).write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            std::mem::forget(stdin);
        });
    }
    // Recording: off the output path
    let (tx, rx) = mpsc::channel::<Report>();
    let recorder = std::thread::spawn(move || {
        let (id, pid) = (std::env::var("ACA_SESSION_ID").unwrap_or_default(), std::process::id());
        for r in rx {
            record(&id, pid, &r);
        }
    });
    // Output: the agent → our terminal, with the sequences taken out
    let mut from_agent = File::from(master);
    let mut stdout = unsafe { File::from_raw_fd(1) };
    let mut scan = Scan::default();
    let mut buf = vec![0u8; 65536];
    let fd = from_agent.as_raw_fd();
    loop {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut pfd, 1, 1000) };
        if n == 0 {
            // Quiet: if the agent is gone (something it started may still hold the pty open), stop here
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            continue;
        }
        if n < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        let got = match from_agent.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            // EIO: every holder of the agent's side has closed it
            Err(_) => break,
        };
        let (out, seqs) = scan.feed(&buf[..got]);
        if !out.is_empty() && stdout.write_all(&out).is_err() {
            break;
        }
        for s in seqs {
            match s {
                Seq::Query => {
                    let _ = to_agent.lock().unwrap_or_else(|e| e.into_inner()).write_all(REPLY);
                }
                Seq::Report(body) => {
                    if let Some(r) = parse(&body) {
                        let _ = tx.send(r);
                    }
                }
            }
        }
    }
    let _ = stdout.flush();
    std::mem::forget(stdout);
    // Our terminal went away while the agent still runs (the pane was closed): hang it up, as closing its terminal would
    if matches!(child.try_wait(), Ok(None)) {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGHUP);
        }
    }
    let status = child.wait().map_or(1, code);
    drop(tx);
    let _ = recorder.join();
    drop(raw);
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(parts: &[&[u8]]) -> (Vec<u8>, Vec<Seq>) {
        let mut s = Scan::default();
        let (mut out, mut seqs) = (vec![], vec![]);
        for p in parts {
            let (o, q) = s.feed(p);
            out.extend(o);
            seqs.extend(q);
        }
        (out, seqs)
    }

    #[test]
    fn sequences_are_taken_out_and_everything_else_passes_through() {
        let (out, seqs) = feed_all(&[b"a\x1b]7501;state=working:app=claude-code\x1b\\b\x1b]7501;?\x07c\x1b]0;title\x07"]);
        assert_eq!(out, b"abc\x1b]0;title\x07");
        assert_eq!(seqs, vec![Seq::Report("state=working:app=claude-code".into()), Seq::Query]);
    }

    #[test]
    fn a_sequence_cut_anywhere_is_still_found() {
        let whole: &[u8] = b"x\x1b]7501;state=done\x1b\\y";
        for cut in 1..whole.len() {
            let (out, seqs) = feed_all(&[&whole[..cut], &whole[cut..]]);
            assert_eq!(out, b"xy", "cut at {cut}");
            assert_eq!(seqs, vec![Seq::Report("state=done".into())], "cut at {cut}");
        }
    }

    #[test]
    fn other_escapes_and_a_lone_esc_at_the_end_are_left_alone() {
        let (out, seqs) = feed_all(&[b"\x1b[31mred\x1b[0m\x1b", b"[K"]);
        assert_eq!(out, b"\x1b[31mred\x1b[0m\x1b[K");
        assert!(seqs.is_empty());
    }

    #[test]
    fn an_unterminated_sequence_too_long_to_be_a_report_is_passed_on() {
        let mut big = b"\x1b]7501;".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX + 10));
        let (out, seqs) = feed_all(&[&big]);
        assert_eq!(out, big);
        assert!(seqs.is_empty());
    }

    #[test]
    fn an_esc_inside_ends_the_body_and_stays_in_the_output() {
        let (out, seqs) = feed_all(&[b"\x1b]7501;state=idle\x1b[0m"]);
        assert_eq!(out, b"\x1b[0m");
        assert_eq!(seqs, vec![Seq::Report("state=idle".into())]);
    }

    #[test]
    fn reports_read_by_the_protocol() {
        // From a real Claude Code 2.1.322: "approve Bash: touch marker1.txt"
        let r = parse("state=blocked:app=claude-code:kind=permission:msg=YXBwcm92ZSBCYXNoOiB0b3VjaCBtYXJrZXIxLnR4dA==").unwrap();
        assert_eq!((r.state.as_str(), r.kind.as_str(), r.msg.as_str()), ("blocked", "permission", "approve Bash: touch marker1.txt"));
        // Padding is optional; unknown keys and malformed pairs are skipped
        let r = parse("state=working:x=1:bad:=2:id=a2b2:title=UmVwbHkgd2l0aCBwb25n").unwrap();
        assert_eq!((r.id.as_str(), r.title.as_str()), ("a2b2", "Reply with pong"));
        // kind only goes with blocked, and only the known ones
        assert_eq!(parse("state=working:kind=permission").unwrap().kind, "");
        assert_eq!(parse("state=blocked:kind=nope").unwrap().kind, "");
        // No state / an unknown one / bad base64 / control characters: the whole report is dropped
        assert!(parse("app=x").is_none() && parse("state=sleeping").is_none());
        assert!(parse("state=done:msg=/w==").is_none(), "not UTF-8");
        // A byte outside the value set only skips that pair
        assert_eq!(parse("state=done:msg=@@@").unwrap().msg, "");
        assert!(parse("state=done:msg=YQpi").is_none(), "a\\nb");
        assert_eq!(parse("state=clear").unwrap().state, "clear");
    }
}
