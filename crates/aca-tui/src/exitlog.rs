//! A record for when the interface didn't quit on its own: the terminal was closed (kitty window closed, ssh dropped), or
//! the program panicked. By then the screen is gone or can't be trusted, so the only place to say anything is a file:
//! ~/.local/state/aca/ui/exit.log gets a block of details appended each time. A panic exit also writes one line to
//! ui/last-exit, which the bottom bar says once the next time the interface opens and then deletes; a closed terminal or a
//! dropped connection is routine, so it only goes into exit.log and isn't mentioned again.
//!
//! Once the terminal is gone it's left alone: printing to a gone terminal fails, ratatui's panic hook then restores the
//! terminal and prints again, failing while failing, and the program aborts (SIGABRT) without leaving a word (the
//! 2026-10-14 kitty ⌘W window close). So the hook ratatui::init installs is replaced with the one here: write the file
//! first, then hand the terminal back, and don't print if it can't be handed back.
//! A panic in a background thread doesn't make the interface quit: it goes into the file and the bottom bar says so right
//! away (otherwise something would just quietly stop working).
//!
//! A gone terminal doesn't necessarily send a signal or report an error: when tmux closes a pane there's no SIGHUP, reading
//! the terminal just keeps returning 0 bytes. crossterm 0.29, on reading 0 bytes (or an error other than EIO), keeps
//! rereading internally without returning: the main thread hangs, a CPU is maxed out, and it never quits. Only writing to
//! the terminal reports EIO, so a separate thread watches (watch): every so often it writes 0 bytes to the terminal
//! (nothing appears on screen); on an error, or on the hangup signal, it records that the terminal is gone; the main
//! thread sees that on its next round and finishes up as usual, and if it's stuck and doesn't respond, this thread
//! finishes up and quits for it.

use std::io::Write;
use std::os::fd::AsFd;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aca_core::{t, tr};

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Local time zone offset (seconds): computed when the interface opens, not looked up again on a panic
static TZ: AtomicI64 = AtomicI64::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();
/// Got SIGHUP: the terminal hung up
static HUP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
/// The watchdog thread found the terminal gone
static GONE: AtomicBool = AtomicBool::new(false);
/// Found because writing to the terminal failed (not just the hangup signal)
static EIO: AtomicBool = AtomicBool::new(false);
/// Whether the terminal-gone entry has been written: both the main thread and the watchdog may write it, only once
static LOGGED: Mutex<bool> = Mutex::new(false);
/// A background thread's panic, waiting for the main loop to say it in the bottom bar
static LIVE: Mutex<Option<String>> = Mutex::new(None);

/// Once exit.log is bigger than this it's moved to exit.log.1 and started fresh
const CAP: u64 = 256 * 1024;
/// How often the watchdog checks the terminal. SIGHUP doesn't wait for it; this probe only covers the terminal vanishing
/// without a signal. It takes two failed writes in a row, plus GRACE, so it's found within three or four seconds and
/// needn't check too often (at 250 ms it was the biggest item in a sample of the interface process apart from waiting)
const PROBE: Duration = Duration::from_secs(1);
/// After finding the terminal gone, how long to wait for the main thread to finish up itself (a round takes at most 150 ms);
/// still there = stuck
const GRACE: Duration = Duration::from_millis(1500);

fn dir() -> PathBuf {
    aca_core::paths::state_dir().join("ui")
}

/// Called when the interface opens, after ratatui::init. prev = the panic hook from before init (std's default: print the
/// panic message to stderr, never mind if that fails); release = hand the terminal back, called on a main-thread panic
pub fn install(prev: Hook, release: fn() -> std::io::Result<()>) {
    TZ.store(crate::usage::local_offset(), Ordering::Relaxed);
    let _ = STARTED.set(Instant::now());
    let flag = HUP.get_or_init(|| Arc::new(AtomicBool::new(false)));
    // With it caught, a hangup doesn't just kill us: the main loop sees it, finishes up and writes an entry
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, flag.clone());
    if let Ok(out) = std::io::stdout().as_fd().try_clone_to_owned() {
        std::thread::spawn(move || watch(std::fs::File::from(out)));
    }
    // Throw away the one ratatui installed: when restoring the terminal fails it eprintln!s, which panics again here when the
    // terminal is gone
    drop(std::panic::take_hook());
    std::panic::set_hook(Box::new(move |info| {
        let main = std::thread::current().name() == Some("main");
        on_panic(info, main);
        if main {
            let _ = release();
            prev(info);
        }
        // A background thread's isn't printed to the screen: the interface is still open, printed text would stay on it
    }));
}

/// Got the hangup signal (SIGHUP)
fn hung_up() -> bool {
    HUP.get().is_some_and(|f| f.load(Ordering::Relaxed))
}

/// The terminal is gone: got the hangup signal, or the watchdog's write to the terminal failed. The main loop checks once a
/// round and, if so, finishes up and quits
pub fn gone() -> bool {
    hung_up() || GONE.load(Ordering::Relaxed)
}

/// The watchdog thread: writes 0 bytes to the terminal; nothing happens while it's there, EIO once it's gone. Only two
/// failures in a row count (one could be a fluke)
fn watch(out: std::fs::File) {
    let mut strikes = 0;
    loop {
        std::thread::sleep(PROBE);
        strikes = if (&out).write(&[]).is_err() { strikes + 1 } else { 0 };
        if strikes >= 2 {
            EIO.store(true, Ordering::Relaxed);
            break;
        }
        if hung_up() {
            break;
        }
    }
    GONE.store(true, Ordering::Relaxed);
    std::thread::sleep(GRACE);
    // The main thread still hasn't quit: it's stuck (most likely reading 0 bytes forever inside crossterm). Finish up for it:
    // write an entry, really delete what's due in the trash in a separate process, quit. The input method isn't switched
    // back: that has to happen on the main thread
    log_gone(None, None, true);
    if crate::trash::Trash::open(crate::trash::Trash::default_path()).due(aca_core::paths::now()) {
        crate::trash::purge_detached();
    }
    std::process::exit(0);
}

/// A background thread panicked: the message not yet said in the bottom bar
pub fn take_live() -> Option<String> {
    LIVE.lock().ok()?.take()
}

/// The terminal is gone and the interface quits with it: write an entry. why = the error the main loop stopped on (none =
/// it stopped on seeing the terminal gone), restore = the error from handing the terminal back
pub fn term_gone(why: Option<&anyhow::Error>, restore: Option<&std::io::Error>) {
    log_gone(why, restore, false);
}

/// stuck = the main thread is stuck, and the watchdog is the one writing it
fn log_gone(why: Option<&anyhow::Error>, restore: Option<&std::io::Error>, stuck: bool) {
    let Ok(mut logged) = LOGGED.lock() else { return };
    if std::mem::replace(&mut *logged, true) {
        return;
    }
    let name = place_name(place());
    let mut body = tr!("终端已关闭,界面随之退出({name})\n", "the terminal went away and the interface quit with it ({name})\n");
    if let Some(e) = why {
        body += &tr!("  主循环终止于:{e:#}\n", "  the main loop stopped on: {e:#}\n");
    }
    if hung_up() {
        body += t!("  收到挂断信号(SIGHUP)\n", "  got the hangup signal (SIGHUP)\n");
    }
    if EIO.load(Ordering::Relaxed) {
        body += t!("  写入终端出错(EIO)\n", "  writing to the terminal failed (EIO)\n");
    }
    if stuck {
        body += t!(
            "  主线程无响应(正在等待终端输入),由看门线程收尾\n",
            "  the main thread was stuck waiting for terminal input; the watchdog finished up\n"
        );
    }
    if let Some(e) = restore {
        body += &tr!("  还原终端失败:{e}\n", "  restoring the terminal: {e}\n");
    }
    let dir = dir();
    let now = now_secs();
    append(&dir, &entry(now, TZ.load(Ordering::Relaxed), uptime(), &body));
}

fn on_panic(info: &PanicHookInfo<'_>, main: bool) {
    let thread = std::thread::current().name().unwrap_or(t!("后台", "background")).to_string();
    let msg = payload(info);
    let mut body = tr!("程序出错(线程 {thread}):{msg}\n", "the program failed (thread {thread}): {msg}\n");
    if let Some(at) = info.location() {
        body += &tr!("  位置:{at}\n", "  at {at}\n");
    }
    body += t!("  调用栈:\n", "  backtrace:\n");
    for l in std::backtrace::Backtrace::force_capture().to_string().lines() {
        body += &format!("    {l}\n");
    }
    let dir = dir();
    let now = now_secs();
    append(&dir, &entry(now, TZ.load(Ordering::Relaxed), uptime(), &body));
    let short = one_line(&msg, 80);
    if main {
        write_last(&dir, now, "panic", place(), &short);
    } else if let Ok(mut live) = LIVE.lock() {
        *live = Some(tr!("后台出错:{short} · 详见 exit.log", "something failed in the background: {short} · details in exit.log"));
    }
}

/// The last interface quit on a panic: the bottom bar says so. Deleted once said, said only once
pub fn take_notice() -> Option<String> {
    take_notice_in(&dir(), now_secs(), crate::usage::local_offset())
}

fn take_notice_in(dir: &Path, now: i64, tz: i64) -> Option<String> {
    let path = dir.join("last-exit");
    let line = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let mut f = line.trim_end_matches('\n').splitn(4, '\t');
    let at = f.next()?.parse::<i64>().ok()?;
    // Older versions also recorded a closed terminal (term): not said
    if f.next()? != "panic" {
        return None;
    }
    // Skip where it was open
    let note = f.nth(1).unwrap_or("");
    let when = when(at, now, tz);
    Some(tr!(
        "上次界面于 {when} 异常退出:{note} · 详见 exit.log",
        "the last interface quit on an error at {when}: {note} · details in exit.log"
    ))
}

/// Where it's open: last-exit gets a code, turned into words of the language in use when it's said
fn place() -> &'static str {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if crate::data::in_popup() {
        "popup"
    } else if set("TMUX") {
        "tmux"
    } else if set("SSH_CONNECTION") || set("SSH_TTY") {
        "ssh"
    } else if set("KITTY_WINDOW_ID") {
        "kitty"
    } else {
        "term"
    }
}

fn place_name(code: &str) -> &'static str {
    match code {
        "popup" => t!("tmux 弹窗", "tmux popup"),
        "tmux" => t!("tmux 窗格", "tmux pane"),
        "ssh" => "ssh",
        "kitty" => t!("kitty 窗口", "kitty window"),
        _ => t!("终端", "terminal"),
    }
}

fn payload(info: &PanicHookInfo<'_>) -> String {
    let p = info.payload();
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "?".into())
}

/// One block: a heading line (when, which process, how long it was up) + details
fn entry(now: i64, tz: i64, up: Option<Duration>, body: &str) -> String {
    let pid = std::process::id();
    let up = up.map(|d| tr!(" · 运行了 {}", " · up {}", span(d))).unwrap_or_default();
    format!("── {} · pid {pid}{up} ──\n{body}\n", stamp(now, tz))
}

/// Append a block; if it's too big, move the old one away first. If it can't be written, never mind (there's nowhere else
/// to say it at this point)
fn append(dir: &Path, text: &str) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("exit.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > CAP) {
        let _ = std::fs::rename(&path, dir.join("exit.log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// last-exit: one line `<when (seconds)>\t<panic>\t<where it was open>\t<panic message>`, only the latest kept
fn write_last(dir: &Path, at: i64, kind: &str, place: &str, note: &str) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("last-exit"), format!("{at}\t{kind}\t{place}\t{}\n", note.replace(['\t', '\n'], " ")));
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn uptime() -> Option<Duration> {
    STARTED.get().map(Instant::elapsed)
}

/// 2026-10-14 22:59:33 (local time)
fn stamp(at: i64, tz: i64) -> String {
    let t = at + tz;
    let (y, m, d) = crate::usage::civil_from_days(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    format!("{y}-{m:02}-{d:02} {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Today is written 22:59, other days 10/14 22:59
fn when(at: i64, now: i64, tz: i64) -> String {
    let (t, today) = (at + tz, (now + tz).div_euclid(86400));
    let s = t.rem_euclid(86400);
    let clock = format!("{:02}:{:02}", s / 3600, s / 60 % 60);
    if t.div_euclid(86400) == today {
        return clock;
    }
    let (_, m, d) = crate::usage::civil_from_days(t.div_euclid(86400));
    format!("{m}/{d} {clock}")
}

fn span(d: Duration) -> String {
    let s = d.as_secs();
    let (days, h, m, sec) = (s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
    if days > 0 {
        tr!("{days} 天 {h} 小时", "{days}d {h}h")
    } else if h > 0 {
        tr!("{h} 小时 {m} 分", "{h}h {m}m")
    } else {
        tr!("{m} 分 {sec} 秒", "{m}m {sec}s")
    }
}

/// The panic message folded into one line, cut if too long
fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    format!("{}…", s.chars().take(max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-exitlog-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    // 2026-10-14 22:59:33 +08:00
    const AT: i64 = 1_791_989_973;
    const CST: i64 = 8 * 3600;

    #[test]
    fn the_terminal_going_away_is_not_told() {
        // The kind older versions recorded: not said, and deleted too
        let d = tmp("term");
        write_last(&d, AT, "term", "kitty", "");
        assert_eq!(take_notice_in(&d, AT + 600, CST), None);
        assert!(!d.join("last-exit").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_error_on_another_day_says_the_date_and_the_message() {
        let d = tmp("panic");
        write_last(&d, AT, "panic", "ssh", "index out of\tbounds\nline two");
        let m = take_notice_in(&d, AT + 86400, CST).unwrap();
        assert_eq!(m, "上次界面于 10/14 22:59 异常退出:index out of bounds line two · 详见 exit.log");
        // Deleted once said
        assert_eq!(take_notice_in(&d, AT + 86400, CST), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_line_is_dropped_without_a_word() {
        let d = tmp("broken");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("last-exit"), "not a time\tterm\n").unwrap();
        assert_eq!(take_notice_in(&d, AT, CST), None);
        assert!(!d.join("last-exit").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn entries_append_and_the_log_rolls_over_when_big() {
        let d = tmp("roll");
        append(&d, &entry(AT, CST, Some(Duration::from_secs(2 * 86400 + 5 * 3600 + 7)), "终端没了\n"));
        let log = std::fs::read_to_string(d.join("exit.log")).unwrap();
        assert!(log.starts_with("── 2026-10-14 22:59:33 · pid "), "{log}");
        assert!(log.contains(" · 运行了 2 天 5 小时 ──\n终端没了\n"), "{log}");
        append(&d, "second\n");
        assert!(std::fs::read_to_string(d.join("exit.log")).unwrap().ends_with("second\n"));
        std::fs::write(d.join("exit.log"), "x".repeat(CAP as usize + 1)).unwrap();
        append(&d, "fresh\n");
        assert_eq!(std::fs::read_to_string(d.join("exit.log")).unwrap(), "fresh\n");
        assert_eq!(std::fs::metadata(d.join("exit.log.1")).unwrap().len(), CAP + 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn spans_and_messages_stay_short() {
        assert_eq!(span(Duration::from_secs(3 * 3600 + 4 * 60 + 5)), "3 小时 4 分");
        assert_eq!(span(Duration::from_secs(65)), "1 分 5 秒");
        assert_eq!(one_line("a\n  b", 10), "a b");
        assert_eq!(one_line("一二三四五", 3), "一二三…");
    }
}
