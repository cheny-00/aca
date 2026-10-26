//! 定时:清单里的 `[[timers]]` —— 每隔一阵跑插件的一个能力(`run`)。
//!
//! aca 没有常驻进程,到点靠 macOS 的 launchd:开着的定时一个一个 LaunchAgent
//! (`~/Library/LaunchAgents/com.aca.timer.<插件>.<能力>.plist`,`StartInterval` = every),到点跑
//! `aca plugin timer tick <插件.能力>`。合盖睡着错过的几次,醒来只补跑一次。
//!
//! 开没开是 aca 记的(`~/.local/state/aca/timers/<插件.能力>.json` 的 `on`),launchd 只管到点把它叫起来:
//! - 没写 `manual` 的:插件开着就开着。链接、装、更新、开关、删插件时 [`sync`] / [`drop_plugin`] 照清单对一遍
//! - `manual = true` 的:平时关着,插件自己 `aca plugin timer on / off`(有活才开:远端任务有在等的才每 5 分钟去取,
//!   都了结了关掉 —— 没派任务的日子里什么都不跑)。插件停着时不跑,开回来照它原来的开关
//!
//! 到点那一次([`tick`]):插件、清单里的这个定时都在、开着才跑,不然把自己卸掉;输出进插件的日志,最多跑 [`LIMIT`];
//! 跑完记下时刻、用了多久、退出码(`aca plugin timer` 列出来)。
//! 跑着的那一次里插件说 on / off(比如都了结了):先只记下,launchd 那头等它跑完再装 / 卸 ——
//! 卸的时候 launchd 会把正在跑的这一次连同它起的进程一起收走。
//!
//! 测试、基准(设了 `ACA_HOME` 或 `ACA_TMUX_SOCKET`)不碰 launchd:plist 写在 `timers/` 里当作装上了,到点由测试自己 `tick`。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::tools::{Fed, run_fed};
use super::{
    Context, Entry, Manifest, command, command_ids, first_party, log_file, read_manifest, read_registry, registry, split_qualified,
};
use crate::paths::{self, Res};
use crate::tr;

pub use super::manifest::Timer;

/// 一次最多跑这么久:到了先 TERM 它那一组,3 秒不退 KILL(卡住的一次会挡住后面每一次:launchd 不同时起两个)
pub const LIMIT: Duration = Duration::from_secs(600);
/// 最短隔多久
pub const MIN_EVERY: u64 = 60;

/// `every` 的写法:30s / 10m / 2h / 1d,光写数是秒
pub fn parse_every(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = match s.char_indices().last()? {
        (i, 's') => (&s[..i], 1),
        (i, 'm') => (&s[..i], 60),
        (i, 'h') => (&s[..i], 3600),
        (i, 'd') => (&s[..i], 86400),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok().filter(|n| *n > 0).and_then(|n| n.checked_mul(unit))
}

/// 多久:45s / 5m / 2h30m / 1d6h
pub fn span(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => match (secs / 3600, secs % 3600 / 60) {
            (h, 0) => format!("{h}h"),
            (h, m) => format!("{h}h{m}m"),
        },
        _ => match (secs / 86400, secs % 86400 / 3600) {
            (d, 0) => format!("{d}d"),
            (d, h) => format!("{d}d{h}h"),
        },
    }
}

/// aca 记的一个定时
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct State {
    pub plugin: String,
    pub run: String,
    /// 要它开着:manual 的是插件说的;别的插件开着就开着
    pub on: bool,
    pub every: u64,
    /// launchd 那头装的时候的 every(0 = 没装):和 every 不一样就重装
    pub installed: u64,
    /// 开的时刻
    pub since: u64,
    /// 上次跑:开始的时刻(0 = 还没跑过)、用了几秒、退出码(None = 跑超了停掉 / 起不来,原因在 why)
    pub last: u64,
    pub secs: u64,
    pub rc: Option<i32>,
    pub why: String,
}

impl State {
    fn new(plugin: &str, t: &Timer) -> State {
        State { plugin: plugin.into(), run: t.run.clone(), every: t.secs(), ..Default::default() }
    }

    pub fn qualified(&self) -> String {
        format!("{}.{}", self.plugin, self.run)
    }
}

fn dir() -> PathBuf {
    paths::state_dir().join("timers")
}

fn file(q: &str) -> PathBuf {
    dir().join(format!("{q}.json"))
}

fn load(q: &str) -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(file(q)).ok()?).ok()
}

fn save(st: &State) -> Res<()> {
    let _ = std::fs::create_dir_all(dir());
    paths::write_atomic(&file(&st.qualified()), &serde_json::to_string_pretty(st).unwrap_or_default())
        .map_err(|e| tr!("无法记录:{e}", "can't record it: {e}"))
}

fn forget(q: &str) {
    let _ = std::fs::remove_file(file(q));
}

pub(crate) fn all() -> Vec<State> {
    let mut out: Vec<State> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str::<State>(&std::fs::read_to_string(e.path()).ok()?).ok())
        .filter(|s| !s.plugin.is_empty() && !s.run.is_empty())
        .collect();
    out.sort_by_key(State::qualified);
    out
}

/// launchd 里的名字:com.aca.timer.<插件>.<能力>(点、冒号换成 -)
pub fn label(plugin: &str, run: &str) -> String {
    format!("com.aca.timer.{}.{}", super::mount::safe(plugin), super::mount::safe(run))
}

/// 测试、基准里不碰 launchd
fn isolated() -> bool {
    ["ACA_HOME", "ACA_TMUX_SOCKET"].iter().any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

fn plist_path(label: &str) -> PathBuf {
    if isolated() { dir().join(format!("{label}.plist")) } else { paths::home().join(format!("Library/LaunchAgents/{label}.plist")) }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// 到点跑 `<这个 aca> plugin timer tick <插件.能力>`。PATH 照开它时的抄(launchd 自己的 PATH 里没有 tmux、jq)
pub fn plist(label: &str, exe: &str, q: &str, every: u64, path: &str, log: &str) -> String {
    let (label, exe, q, path, log) = (xml(label), xml(exe), xml(q), xml(path), xml(log));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>plugin</string>
        <string>timer</string>
        <string>tick</string>
        <string>{q}</string>
    </array>
    <!-- 周期性一次性任务:不用 KeepAlive,不然每次正常退出 launchd 都立刻重跑 -->
    <key>StartInterval</key>
    <integer>{every}</integer>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn ctl(args: &[&str]) -> Res<()> {
    let o = Command::new("launchctl").args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        Err(err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string())
    }
}

/// launchd 那头装着没有(测试里:plist 在不在)
fn loaded(label: &str) -> bool {
    if isolated() {
        return plist_path(label).exists();
    }
    Command::new("launchctl")
        .args(["print", &format!("{}/{label}", domain())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// 让 launchd 跟记的一样:`want` 就装上(every 变了重装),不然卸掉
fn apply(st: &mut State, want: bool) -> Res<()> {
    let label = label(&st.plugin, &st.run);
    let path = plist_path(&label);
    let service = format!("{}/{label}", domain());
    if !want {
        st.installed = 0;
        // 先删文件再卸:到点跑的那一次就是它起的,卸的时候会被一起收走
        let _ = std::fs::remove_file(&path);
        if !isolated() && loaded(&label) {
            ctl(&["bootout", &service]).map_err(|e| tr!("无法卸载系统定时器:{e}", "the system timer didn't uninstall: {e}"))?;
        }
        return Ok(());
    }
    let up = loaded(&label);
    if up && st.installed == st.every {
        return Ok(());
    }
    if up && !isolated() {
        let _ = ctl(&["bootout", &service]);
    }
    let log = dir().join("launchd.log");
    let text = plist(
        &label,
        &crate::agent::self_exe(),
        &st.qualified(),
        st.every,
        &std::env::var("PATH").unwrap_or_default(),
        &log.to_string_lossy(),
    );
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    paths::write_atomic(&path, &text).map_err(|e| tr!("无法写入定时器的 plist:{e}", "can't write the timer's plist: {e}"))?;
    if !isolated() {
        // 进过 disabled 名单的服务,bootstrap 只报「5: Input/output error」
        let _ = ctl(&["enable", &service]);
        ctl(&["bootstrap", &domain(), &path.to_string_lossy()])
            .map_err(|e| tr!("系统定时器安装失败:{e}", "the system timer didn't install: {e}"))?;
    }
    st.installed = st.every;
    Ok(())
}

/// 在这个定时自己跑着的那一次里(它的命令又调了 `aca plugin timer on / off`):launchd 那头等这一次跑完再动
fn inside(q: &str) -> bool {
    std::env::var("ACA_PLUGIN_TIMER").is_ok_and(|v| v == q)
}

/// `插件.能力` → 登记、清单、清单里的这个定时
fn declared(q: &str) -> Res<(Entry, Manifest, Timer)> {
    let (pid, run) = split_qualified(q)?;
    let mut e = registry().into_iter().find(|e| e.id == pid).ok_or_else(|| tr!("未链接插件 {pid}", "{pid} isn't linked"))?;
    // 自带的:刚升级了 aca 就先换成新写出来的那份
    if e.source == first_party::SOURCE && first_party::refresh_one(pid) {
        e = registry().into_iter().find(|e| e.id == pid).unwrap_or(e);
    }
    let (m, _) = read_manifest(Path::new(&e.path))?;
    let t = m
        .timers
        .iter()
        .find(|t| t.run == run)
        .cloned()
        .ok_or_else(|| tr!("{pid} 没有定时任务 {run}(见清单的 [[timers]])", "{pid} has no timer {run} ([[timers]] in its manifest)"))?;
    Ok((e, m, t))
}

/// `aca plugin timer on <插件.能力>`
pub fn on(q: &str) -> Res<String> {
    let (e, _, t) = declared(q)?;
    if !e.enabled {
        return Err(tr!("{} 已关闭:先运行 aca plugin enable {}", "{} is paused: aca plugin enable {} first", e.id, e.id));
    }
    let mut st = load(q).unwrap_or_else(|| State::new(&e.id, &t));
    let was = st.on;
    if !st.on {
        st.since = paths::now();
    }
    st.on = true;
    st.every = t.secs();
    let r = if inside(q) { Ok(()) } else { apply(&mut st, true) };
    save(&st)?;
    r?;
    Ok(if was {
        tr!("{q} 已开启:每 {} 运行一次", "{q} is on: runs every {}", span(st.every))
    } else {
        tr!("已开启 {q}:每 {} 运行一次", "turned on {q}: runs every {}", span(st.every))
    })
}

/// `aca plugin timer off <插件.能力>`。插件、清单里的定时没了也能关(收拾剩下的)
pub fn off(q: &str) -> Res<String> {
    let found = declared(q);
    if let Ok((e, _, t)) = &found
        && !t.manual
        && e.enabled
    {
        return Err(tr!(
            "{q} 随插件开关(清单中未写 manual):如需关闭,运行 aca plugin disable {}",
            "{q} follows its plugin (no manual in the manifest): to stop it, aca plugin disable {}",
            e.id
        ));
    }
    let (pid, run) = split_qualified(q)?;
    let mut st = load(q).unwrap_or(State { plugin: pid.into(), run: run.into(), ..Default::default() });
    let was = st.on || loaded(&label(pid, run));
    st.on = false;
    let r = if inside(q) { Ok(()) } else { apply(&mut st, false) };
    if found.is_ok() {
        save(&st)?;
    } else {
        forget(q);
    }
    r?;
    Ok(if was { tr!("已关闭 {q}", "turned off {q}") } else { tr!("{q} 已是关闭状态", "{q} was already off") })
}

/// 插件登记、更新、开关之后:照清单把它的定时对一遍 —— 清单里没了的卸掉、不记了;every 变了的重装;
/// 没写 manual 的插件开着就开;插件停着的都卸掉(开没开照记的,开回来接着用)。返回没办成的(给链接时的提醒)
pub fn sync(id: &str) -> Vec<String> {
    // 不经过 registry():它会先重新登记你自己的那份,而那又会回到这里
    let Some(e) = read_registry().into_iter().find(|e| e.id == id) else {
        drop_plugin(id);
        return vec![];
    };
    let timers = read_manifest(Path::new(&e.path)).map(|(m, _)| m.timers).unwrap_or_default();
    let mut problems = vec![];
    let mut fix = |st: &mut State, want: bool| {
        let q = st.qualified();
        if !inside(&q)
            && let Err(err) = apply(st, want)
        {
            problems.push(format!("{q}:{err}"));
        }
        let _ = save(st);
    };
    for mut st in all().into_iter().filter(|s| s.plugin == id) {
        match timers.iter().find(|t| t.run == st.run) {
            None => {
                let _ = apply(&mut st, false);
                forget(&st.qualified());
            }
            Some(t) => {
                st.every = t.secs();
                if !t.manual && !st.on {
                    st.on = true;
                    st.since = paths::now();
                }
                let want = st.on && e.enabled;
                fix(&mut st, want);
            }
        }
    }
    for t in timers.iter().filter(|t| !t.manual) {
        if load(&format!("{id}.{}", t.run)).is_none() {
            let mut st = State { on: true, since: paths::now(), ..State::new(id, t) };
            fix(&mut st, e.enabled);
        }
    }
    problems
}

/// 插件不用了(删了、取消链接):它的定时都卸掉、不记了
pub fn drop_plugin(id: &str) {
    for mut st in all().into_iter().filter(|s| s.plugin == id) {
        let _ = apply(&mut st, false);
        forget(&st.qualified());
    }
}

/// 跑一次:插件的环境,`ACA_INVOKED_BY=timer`、`ACA_PLUGIN_TIMER=<插件.能力>`,没有会话,最多 [`LIMIT`]
fn fire(e: &Entry, m: &Manifest, t: &Timer) -> Res<Fed> {
    let a = m.actions.iter().find(|a| a.id == t.run).ok_or_else(|| tr!("{} 没有能力 {}", "{} has no command {}", e.id, t.run))?;
    let q = format!("{}.{}", e.id, t.run);
    let ctx = Context { source: "timer".into(), ..Default::default() };
    let mut extra: Vec<(&str, &str)> = command_ids(&a.id, "{}").to_vec();
    extra.push(("ACA_PLUGIN_TIMER", &q));
    let mut c = command(e, m, &a.command, &ctx, &extra, &a.cwd);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    run_fed(c, LIMIT, None).map_err(|err| tr!("{} {err}", "{} {err}", a.command[0]))
}

/// 跑完:记下这一次(重读一遍:跑的时候插件可能开关过它),再让 launchd 跟记的一样 —— 卸放最后:会把这一次一起收走
fn settle(q: &str, enabled: bool, started: u64, got: &Res<Fed>) {
    let Some(mut st) = load(q) else { return };
    st.last = started;
    st.secs = paths::now().saturating_sub(started);
    (st.rc, st.why) = match got {
        Ok(f) => (f.code, f.late.map(|s| tr!("超过 {s} 秒未完成,已停止", "still running after {s} s; stopped")).unwrap_or_default()),
        Err(e) => (None, e.clone()),
    };
    let want = st.on && enabled;
    if !want {
        // 卸的时候这一次连同 aca 一起被收走,后面的存不到:先记成没装
        st.installed = 0;
    }
    let _ = save(&st);
    if apply(&mut st, want).is_ok() {
        let _ = save(&st);
    }
}

/// launchd 到点跑的:`aca plugin timer tick <插件.能力>`。不该跑的(插件没了、停了、清单里没这个定时、关着)把自己卸掉。
/// 输出进插件的日志;出了什么事记在 `aca plugin timer` 里
pub fn tick(q: &str) -> i32 {
    let found = declared(q);
    let st = load(q);
    let run = match (&found, &st) {
        (Ok((e, _, _)), Some(s)) => e.enabled && s.on,
        _ => false,
    };
    if !run {
        let Ok((pid, name)) = split_qualified(q) else { return 0 };
        let mut s = st.unwrap_or(State { plugin: pid.into(), run: name.into(), ..Default::default() });
        // 插件、定时都在,只是停着 / 关着:开没开照记的;没了:不记了
        if found.is_ok() {
            s.installed = 0;
            let _ = save(&s);
        } else {
            forget(q);
        }
        let _ = apply(&mut s, false);
        return 0;
    }
    let Ok((e, m, t)) = found else { return 0 };
    let started = paths::now();
    let got = fire(&e, &m, &t);
    {
        use std::io::Write;
        let log = log_file(&e.id);
        let _ = std::fs::create_dir_all(log.parent().unwrap_or(Path::new(".")));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            let _ = writeln!(f, "--- {started} timer {}", t.run);
            let text = match &got {
                Ok(fed) if fed.ok => fed.text(),
                Ok(fed) => match fed.code {
                    Some(c) => format!("{}\n{}", fed.text().trim_end(), tr!("(退出码 {c})", "(exit code {c})")),
                    None => fed.text(),
                },
                Err(err) => err.clone(),
            };
            if !text.trim().is_empty() {
                let _ = writeln!(f, "{}", text.trim_end());
            }
        }
    }
    settle(q, e.enabled, started, &got);
    0
}

/// `aca plugin timer run <插件.能力>`:现在跑一次,像到点了一样(输出打出来,关着也跑),返回它的退出码
pub fn run_now(q: &str) -> Res<i32> {
    let (e, m, t) = declared(q)?;
    if !e.enabled {
        return Err(tr!("{} 已关闭:先运行 aca plugin enable {}", "{} is paused: aca plugin enable {} first", e.id, e.id));
    }
    let started = paths::now();
    let got = fire(&e, &m, &t);
    settle(q, e.enabled, started, &got);
    let fed = got?;
    let text = fed.text();
    if !text.is_empty() {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
    Ok(fed.code.unwrap_or(1))
}

/// `aca plugin timer` 列的一行
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Row {
    pub id: String,
    pub every: u64,
    pub manual: bool,
    /// 要它开着(插件停着时也照记的说)
    pub on: bool,
    /// 插件开着
    pub enabled: bool,
    /// launchd 那头装着
    pub scheduled: bool,
    pub last: u64,
    pub secs: u64,
    pub rc: Option<i32>,
    pub why: String,
}

/// 全部插件的定时,登记的顺序
pub fn list() -> Vec<Row> {
    let mut out = vec![];
    for e in registry() {
        let Ok((m, _)) = read_manifest(Path::new(&e.path)) else { continue };
        for t in &m.timers {
            let q = format!("{}.{}", e.id, t.run);
            let st = load(&q).unwrap_or_default();
            out.push(Row {
                id: q,
                every: t.secs(),
                manual: t.manual,
                on: st.on || !t.manual,
                enabled: e.enabled,
                scheduled: loaded(&label(&e.id, &t.run)),
                last: st.last,
                secs: st.secs,
                rc: st.rc,
                why: st.why,
            });
        }
    }
    out
}

impl Row {
    /// 一行字:● 开着 / ○ 关着,每隔多久,上次跑得怎么样
    pub fn text(&self, now: u64) -> String {
        let mark = if self.on && self.enabled { "●" } else { "○" };
        let mut parts = vec![tr!("每 {}", "every {}", span(self.every))];
        if self.manual {
            parts.push(tr!("由插件自行开关", "switched by its plugin"));
        }
        parts.push(match (self.enabled, self.on) {
            (false, _) => tr!("插件已关闭", "plugin paused"),
            (true, true) => tr!("已开启", "on"),
            (true, false) => tr!("已关闭", "off"),
        });
        if self.last > 0 {
            let ago = span(now.saturating_sub(self.last));
            parts.push(match self.rc {
                Some(0) => tr!("上次 {ago} 前 ✓", "last ran {ago} ago ✓"),
                Some(c) => tr!("上次 {ago} 前 ✕ 退出码 {c}", "last ran {ago} ago ✕ exit code {c}"),
                None => tr!("上次 {ago} 前 ✕ {}", "last ran {ago} ago ✕ {}", self.why),
            });
        }
        format!("{mark} {}  {}", self.id, parts.join(" · "))
    }

    /// 记的和 launchd 对不上:说一句怎么办
    pub fn mismatch(&self) -> Option<String> {
        let want = self.on && self.enabled;
        match (want, self.scheduled) {
            (true, false) => Some(tr!(
                "{} 已开启,但系统定时器未安装:运行 aca plugin timer on {} 重新安装",
                "{} is on but the system timer isn't installed: aca plugin timer on {} to install it again",
                self.id,
                self.id
            )),
            (false, true) => Some(tr!(
                "{} 已关闭,但系统定时器仍在:到时会自动卸载,如需立即卸载:aca plugin timer off {}",
                "{} is off but the system timer is still installed: it removes itself when it fires; now: aca plugin timer off {}",
                self.id,
                self.id
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reads_seconds_minutes_hours_days_and_spans_write_them_back() {
        assert_eq!(parse_every("90"), Some(90));
        assert_eq!(parse_every("30s"), Some(30));
        assert_eq!(parse_every(" 5m "), Some(300));
        assert_eq!(parse_every("2h"), Some(7200));
        assert_eq!(parse_every("1d"), Some(86400));
        for bad in ["", "0m", "-5m", "5x", "m", "1.5h"] {
            assert_eq!(parse_every(bad), None, "{bad}");
        }
        assert_eq!(span(45), "45s");
        assert_eq!(span(300), "5m");
        assert_eq!(span(9000), "2h30m");
        assert_eq!(span(7200), "2h");
        assert_eq!(span(86400 + 6 * 3600), "1d6h");
    }

    #[test]
    fn the_launchd_job_runs_this_aca_on_the_timer_with_the_path_it_was_given() {
        assert_eq!(label("aca.remote", "collect"), "com.aca.timer.aca-remote.collect");
        assert_eq!(label("me.x", "a:b"), "com.aca.timer.me-x.a-b");
        let p = plist("com.aca.timer.aca-remote.collect", "/u/bin/aca", "aca.remote.collect", 300, "/opt/bin:/usr/bin", "/s/launchd.log");
        assert!(p.contains("<string>/u/bin/aca</string>\n        <string>plugin</string>\n        <string>timer</string>\n        <string>tick</string>\n        <string>aca.remote.collect</string>"), "{p}");
        assert!(p.contains("<key>StartInterval</key>\n    <integer>300</integer>"));
        assert!(p.contains("<string>/opt/bin:/usr/bin</string>") && !p.contains("KeepAlive</key>"));
        assert!(plist("l", "/a&b/aca", "q", 60, "", "").contains("<string>/a&amp;b/aca</string>"));
    }

    #[test]
    fn a_row_says_how_often_whether_its_on_and_how_the_last_run_went() {
        crate::i18n::with(crate::i18n::Lang::Zh, rows_in_chinese);
    }

    fn rows_in_chinese() {
        let r = Row {
            id: "aca.remote.collect".into(),
            every: 300,
            manual: true,
            on: true,
            enabled: true,
            scheduled: true,
            last: 1000,
            secs: 2,
            rc: Some(0),
            why: String::new(),
        };
        assert_eq!(r.text(1180), "● aca.remote.collect  每 5m · 由插件自行开关 · 已开启 · 上次 3m 前 ✓");
        assert_eq!(r.mismatch(), None);
        let bad = Row { rc: Some(2), scheduled: false, ..r.clone() };
        assert!(bad.text(1180).ends_with("上次 3m 前 ✕ 退出码 2"));
        assert!(bad.mismatch().unwrap().contains("系统定时器未安装"));
        let off = Row { on: false, last: 0, ..r.clone() };
        assert_eq!(off.text(1180), "○ aca.remote.collect  每 5m · 由插件自行开关 · 已关闭");
        assert!(off.mismatch().unwrap().contains("仍在"));
        let paused = Row { enabled: false, scheduled: false, manual: false, ..r };
        assert!(paused.text(1180).starts_with("○ aca.remote.collect  每 5m · 插件已关闭"));
        assert_eq!(paused.mismatch(), None);
    }
}
