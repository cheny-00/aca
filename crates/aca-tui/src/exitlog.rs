//! 界面不是自己退出时留个底:终端被关掉(kitty 关了窗口、ssh 断了)、程序出错。这时屏幕已经没了或者靠不住,
//! 话只能写进文件 —— ~/.local/state/aca/ui/exit.log 每次追加一段细节。出错退出的再在 ui/last-exit 记一行,
//! 下次打开界面时底栏说一次就删掉;终端关掉、断线是常事,只记进 exit.log,不再提。
//!
//! 终端没了时不再碰它:往没了的终端打印会出错,ratatui 的出错善后又去恢复终端、又打印,出错时再出错,
//! 程序直接中止(SIGABRT),一个字都没留下(2026-10-14 kitty ⌘W 关窗那次)。所以 ratatui::init 装的善后
//! 换成这里的:先写文件,再把终端还回去,还不回去也不打印。
//! 后台线程出错不会让界面退出:写进文件,底栏马上说一声(不然就是哪样东西悄悄不动了)。
//!
//! 终端没了也不一定有信号、不一定报错:tmux 关掉窗格时没有 SIGHUP,读终端只是一直读到 0 个字节。crossterm 0.29
//! 读到 0 个字节(或 EIO 以外的错)会在里面一直重读、不返回,主线程卡死、CPU 占满,永远不退出。只有写终端会报 EIO,
//! 所以另起一个线程看着(watch):每隔一会儿往终端写 0 个字节(屏幕上什么也不出现),报错了、或者收到挂断信号,
//! 就记下终端没了;主线程下一轮看到就照常收尾,卡住了没反应就由这个线程替它收尾、退出。

use std::io::Write;
use std::os::fd::AsFd;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aca_core::{t, tr};

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// 本地时区偏移(秒):开界面时算好,出错时不再去查
static TZ: AtomicI64 = AtomicI64::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();
/// 收到了 SIGHUP:终端挂断了
static HUP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
/// 看门的线程发现终端没了
static GONE: AtomicBool = AtomicBool::new(false);
/// 是因为写终端报错发现的(不只是收到了挂断信号)
static EIO: AtomicBool = AtomicBool::new(false);
/// 终端没了的那一笔记过了没有:主线程和看门的线程都可能来记,只记一次
static LOGGED: Mutex<bool> = Mutex::new(false);
/// 后台线程出的错,等主循环拿去在底栏说
static LIVE: Mutex<Option<String>> = Mutex::new(None);

/// exit.log 超过这么大就挪成 exit.log.1,重新记
const CAP: u64 = 256 * 1024;
/// 看门的线程多久看一次终端。SIGHUP 来了不用等它,这个探针只管终端没发信号就没了的情况;连着两次没写进去才算,
/// 加上 GRACE 三四秒内发现,所以不用看得太勤(250 毫秒时采样里它是界面进程除等待之外最大的一项)
const PROBE: Duration = Duration::from_secs(1);
/// 发现终端没了以后,等主线程自己收尾这么久(它一轮最多 150 毫秒);还在 = 卡住了
const GRACE: Duration = Duration::from_millis(1500);

fn dir() -> PathBuf {
    aca_core::paths::state_dir().join("ui")
}

/// 开界面时、ratatui::init 之后调。prev = init 之前的出错善后(std 默认的:把出错信息打到标准错误,打不出去就算了);
/// release = 把终端还回去,主线程出错时调
pub fn install(prev: Hook, release: fn() -> std::io::Result<()>) {
    TZ.store(crate::usage::local_offset(), Ordering::Relaxed);
    let _ = STARTED.set(Instant::now());
    let flag = HUP.get_or_init(|| Arc::new(AtomicBool::new(false)));
    // 接住了它,挂断时就不是被直接杀掉,而是主循环看到后收尾、记一笔
    let _ = signal_hook::flag::register(signal_hook::consts::SIGHUP, flag.clone());
    if let Ok(out) = std::io::stdout().as_fd().try_clone_to_owned() {
        std::thread::spawn(move || watch(std::fs::File::from(out)));
    }
    // ratatui 装的那个扔掉:它恢复终端失败就 eprintln,终端没了时在这里再出错
    drop(std::panic::take_hook());
    std::panic::set_hook(Box::new(move |info| {
        let main = std::thread::current().name() == Some("main");
        on_panic(info, main);
        if main {
            let _ = release();
            prev(info);
        }
        // 后台线程的不往屏幕上打:界面还开着,打出来的字会留在画面上
    }));
}

/// 收到过挂断信号(SIGHUP)
fn hung_up() -> bool {
    HUP.get().is_some_and(|f| f.load(Ordering::Relaxed))
}

/// 终端没了:收到过挂断信号,或者看门的线程写终端报错了。主循环每轮看一次,是就收尾退出
pub fn gone() -> bool {
    hung_up() || GONE.load(Ordering::Relaxed)
}

/// 看门的线程:往终端写 0 个字节,终端在时什么也不发生,没了报 EIO。连着两次报错才算(一次可能是碰巧)
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
    // 主线程到现在还没退出:卡住了(多半是在 crossterm 里一直读 0 个字节)。替它收尾:记一笔,
    // 回收站里到期的另起进程真删,退出。输入法不切回去:那要在主线程上做
    log_gone(None, None, true);
    if crate::trash::Trash::open(crate::trash::Trash::default_path()).due(aca_core::paths::now()) {
        crate::trash::purge_detached();
    }
    std::process::exit(0);
}

/// 后台线程出错了,还没在底栏说过的那一句
pub fn take_live() -> Option<String> {
    LIVE.lock().ok()?.take()
}

/// 终端没了,界面跟着退出:记一笔。why = 主循环停下来时的错(没有 = 看到终端没了停的),restore = 还终端时报的错
pub fn term_gone(why: Option<&anyhow::Error>, restore: Option<&std::io::Error>) {
    log_gone(why, restore, false);
}

/// stuck = 主线程卡住了,是看门的线程来记的
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

/// 上次的界面出错退出了:底栏说一句。说完就删,只说一次
pub fn take_notice() -> Option<String> {
    take_notice_in(&dir(), now_secs(), crate::usage::local_offset())
}

fn take_notice_in(dir: &Path, now: i64, tz: i64) -> Option<String> {
    let path = dir.join("last-exit");
    let line = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let mut f = line.trim_end_matches('\n').splitn(4, '\t');
    let at = f.next()?.parse::<i64>().ok()?;
    // 旧版本还会记终端关掉的(term),不说
    if f.next()? != "panic" {
        return None;
    }
    // 跳过开在哪
    let note = f.nth(1).unwrap_or("");
    let when = when(at, now, tz);
    Some(tr!(
        "上次界面于 {when} 异常退出:{note} · 详见 exit.log",
        "the last interface quit on an error at {when}: {note} · details in exit.log"
    ))
}

/// 开在哪:写进 last-exit 的是代号,说的时候再翻成当时的话
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

/// 一段:标题行(几点、哪个进程、开了多久)+ 细节
fn entry(now: i64, tz: i64, up: Option<Duration>, body: &str) -> String {
    let pid = std::process::id();
    let up = up.map(|d| tr!(" · 运行了 {}", " · up {}", span(d))).unwrap_or_default();
    format!("── {} · pid {pid}{up} ──\n{body}\n", stamp(now, tz))
}

/// 追加一段;太大了先挪走旧的。写不了就算了(这时候没有别处可说)
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

/// last-exit:一行 `<几时(秒)>\t<panic>\t<开在哪>\t<出错信息>`,只留最近一次
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

/// 2026-10-14 22:59:33(本地时间)
fn stamp(at: i64, tz: i64) -> String {
    let t = at + tz;
    let (y, m, d) = crate::usage::civil_from_days(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    format!("{y}-{m:02}-{d:02} {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// 今天的写 22:59,别的天写 10/14 22:59
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

/// 出错信息收成一行,太长截断
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
        // 旧版本记下的那种:不说,也删掉
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
        // 说过就删了
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
