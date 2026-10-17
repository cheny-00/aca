//! `aca statusline`:Claude 的状态栏,一个进程里记额度(见 limits)、画两行。
//!
//! 每个会话每回一条消息都跑一次(设了 refreshInterval 还按秒跑),几十个会话一起就不少,所以不起 bash / jq,要起 git 的也尽量少:
//! - 分支每次直接读 HEAD 文件,不起 git、不进缓存:切了分支下次刷新就对,也读不到别的目录的
//! - 改了几个文件、几行、领先落后要起 git(大仓库里两个 git 一百多毫秒 CPU),按工作区的根缓存:开在同一个工作区里的会话,
//!   不管在哪一层目录,共用一份。这个会话在对话(对话记录刚动过)的缓存 5 秒;闲着的只是定时器在叫它,放宽到 60 秒 ——
//!   旁边有会话在干活的话,它读到的还是那边 5 秒内算的。HEAD、index 动了(提交、暂存、切分支)不等到期,马上重算。
//!   2026-10-14 实测:35 个会话(28 个闲着)光这两个 git 就占 15% 个核,闲了 20 小时的也在每 10 秒算一遍
//! - Python 的版本读 .venv/pyvenv.cfg;node 的版本按 node 程序的路径和修改时间记着,换了才重问
//!
//! 额度那段写的是这个账号所有会话里最新的那份,不是这个会话自己手里的(闲着的会话手里是旧数)。
//! 闲着又没人连着看的会话不重算,照上次画的原样打(见 `still`):不然它们跟着别的会话一起重画,自动停也等不到它们闲下来。
//! 出什么错都只是少画一段:状态栏命令一报错,Claude Code 就把整条画成空的。
//! 配色是 Catppuccin Latte,图标要 Nerd Font。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::usage::{Quota, Win};

type Rgb = (u8, u8, u8);
const MAUVE: Rgb = (136, 57, 239);
const RED: Rgb = (210, 15, 57);
const PEACH: Rgb = (254, 100, 11);
const YELLOW: Rgb = (223, 142, 29);
const GREEN: Rgb = (64, 160, 43);
const TEAL: Rgb = (23, 146, 153);
const LAVENDER: Rgb = (114, 135, 253);
const SUBTEXT: Rgb = (92, 95, 119);
const OVERLAY: Rgb = (156, 160, 176);

const I_MODEL: char = '\u{F0626}';
const I_GIT: char = '\u{E725}';
const I_STAGED: char = '\u{F0417}';
const I_MODIFIED: char = '\u{F06FF}';
const I_UNTRACKED: char = '\u{F02D7}';
const I_DIR: char = '\u{F024B}';
const I_PY: char = '\u{F0320}';
const I_NODE: char = '\u{F0399}';

/// git 的数多久重算一次(秒):这个会话在对话时
const GIT_TTL: u64 = 5;
/// 这个会话闲着时(别的会话算过的照样用,见 `git_ttl`)
const GIT_IDLE_TTL: u64 = 60;
/// 对话记录这么久没动 = 闲着。比 refreshInterval(10 秒)长几拍:一轮刚完,格式化之类收尾的改动还赶得上
const ACTIVE: u64 = 30;

/// 工作区里的数:暂存了、改了没暂存、没跟踪的文件各几个,没暂存的改动加减几行,比上游领先落后几个提交
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Counts {
    staged: u32,
    modified: u32,
    untracked: u32,
    added: u64,
    removed: u64,
    ahead: u32,
    behind: u32,
}

/// 画之前查好的东西:画的那一步不碰磁盘
#[derive(Default)]
struct Facts {
    /// 不在仓库里 = None,git 那段不画
    branch: Option<String>,
    git: Counts,
    /// (名字, 版本)
    venv: Option<(String, String)>,
    node: Option<String>,
    quota: Option<Quota>,
    home: String,
    now: i64,
}

pub fn run() -> i32 {
    let mut payload = vec![];
    let _ = std::io::stdin().read_to_end(&mut payload);
    let now = crate::limits::now();
    let limits = crate::limits::dir();
    if let Some(d) = &limits {
        crate::limits::record(&payload, d, now);
    }
    let v: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    let path = |p: &str| v.pointer(p).and_then(Value::as_str).filter(|x| !x.is_empty()).map(PathBuf::from);
    let (dir, project) = (path("/workspace/current_dir"), path("/workspace/project_dir"));
    let home = std::env::var("HOME").unwrap_or_default();
    let cache = std::env::temp_dir().join("aca-statusline");
    let transcript = path("/transcript_path");
    // 闲着又没人在看:照上次画的原样打(见 `still`)
    let last = v.pointer("/session_id").and_then(Value::as_str).filter(|id| plain(id)).map(|id| cache.join(format!("last-{id}")));
    if let Some(last) = &last
        && still(transcript.as_deref().and_then(idle_secs), watched)
        && let Ok(text) = std::fs::read_to_string(last)
    {
        let _ = std::io::stdout().write_all(text.as_bytes());
        return 0;
    }
    let found = dir.as_deref().and_then(repo);
    let branch = found.as_ref().and_then(|(_, head)| branch(head));
    let git = match (&found, &branch) {
        (Some((root, head)), Some(_)) => {
            let ttl = git_ttl(transcript.as_deref().and_then(idle_secs));
            cached(&cache, "git", &root.to_string_lossy(), &stamp(head), ttl, || count(root))
        }
        _ => Counts::default(),
    };
    let quota = limits.and_then(|d| {
        let srcs = crate::usage::sources(&crate::data::accounts(&aca_core::config::load()), &home);
        crate::limits::quota_for(&d, &srcs, transcript.as_deref(), now)
    });
    let active = std::env::var_os("VIRTUAL_ENV").filter(|x| !x.is_empty()).map(PathBuf::from);
    let facts = Facts {
        branch,
        git,
        venv: venv(active, std::env::var("CONDA_DEFAULT_ENV").ok(), dir.as_deref(), project.as_deref()),
        node: dir
            .as_deref()
            .filter(|d| ["package.json", "package-lock.json", "yarn.lock"].iter().any(|f| d.join(f).is_file()))
            .and_then(|_| node(&cache)),
        quota,
        home,
        now,
    };
    let out = render(&v, &facts);
    if let Some(last) = &last {
        let _ = crate::state::write_atomic(last, &out);
    }
    // Claude Code 等不及会把管道关掉:写不进去不算错
    let _ = std::io::stdout().write_all(out.as_bytes());
    0
}

/// 照上次画的原样打、不重算:对话记录 ACTIVE 秒没动了(闲着),又没人在看(`watched`)。
/// 状态栏里有几样闲着也会变:总时长按分钟走,额度跟着同账号别的会话变,git 的数跟着同目录别的会话变。一变 Claude 就重画,
/// 屏幕上有了输出,自动停(`[idle] stop_after`)就一直认它没闲下来;几十个闲着的会话也一起陪着重画,白费电。
/// 原样打 Claude 就不重画。有人连进去看,下一次刷新(refreshInterval 的 10 秒内)就是新的;在说话的会话照常每次重算
fn still(idle: Option<u64>, watched: impl FnOnce() -> bool) -> bool {
    idle.is_some_and(|s| s >= ACTIVE) && !watched()
}

/// 有没有人在看这个会话:它在的 tmux 会话连着客户端(aca 的弹窗开在它上面也算)。不在 tmux 里、问不到的都当有人在看,照常画
fn watched() -> bool {
    let Some(pane) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) else { return true };
    !aca_core::tmux::run(&["display-message", "-p", "-t", &pane, "#{session_attached}"]).is_ok_and(|n| n.trim() == "0")
}

/// 会话号拿来起文件名:只认字母、数字和 -
fn plain(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// 一段带颜色的字;`attr` 是颜色前面的属性(`1;` 粗、`2;` 淡、`3;` 斜)
fn paint(c: Rgb, attr: &str, text: impl std::fmt::Display) -> String {
    format!("\x1b[{attr}38;2;{};{};{}m{text}\x1b[0m", c.0, c.1, c.2)
}

/// 用了百分之多少算什么颜色:上下文和额度一个标准
fn level(pct: f64) -> Rgb {
    if pct >= 85.0 {
        RED
    } else if pct >= 60.0 {
        YELLOW
    } else {
        GREEN
    }
}

/// 第一行:上下文 │ 模型 │ git │ 目录;第二行:额度 │ 花费和时长 │ Python │ node。没有的那段不画,整行都没有的那行不占地方
fn render(v: &Value, f: &Facts) -> String {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).filter(|x| !x.is_empty());
    let n = |p: &str| v.pointer(p).and_then(Value::as_f64);
    let mut l1: Vec<String> = vec![];
    if let Some(used) = n("/context_window/used_percentage") {
        const W: usize = 15;
        let u = used.max(0.0) as usize;
        let filled = (u * W / 100).min(W);
        let bar = format!("{}{}", "━".repeat(filled), "╌".repeat(W - filled));
        l1.push(format!("{} {}", paint(level(u as f64), "", bar), paint(SUBTEXT, "", format!("{u}%"))));
    }
    if let Some(m) = s("/model/display_name") {
        l1.push(paint(MAUVE, "1;", format!("{I_MODEL} {m}")));
    }
    if let Some(b) = &f.branch {
        l1.push(git(b, &f.git));
    }
    if let Some(d) = s("/workspace/current_dir") {
        l1.push(paint(LAVENDER, "", format!("{I_DIR} {}", place(d, s("/workspace/project_dir"), &f.home))));
    }
    let mut l2: Vec<String> = vec![];
    if let Some(q) = f.quota.as_ref().and_then(|q| quota(q, f.now)) {
        l2.push(q);
    }
    let cost = n("/cost/total_cost_usd").unwrap_or(0.0);
    if cost != 0.0 {
        let took = span(n("/cost/total_duration_ms").unwrap_or(0.0).max(0.0) as u64 / 1000);
        l2.push(format!("{}{}", paint(SUBTEXT, "", format!("${cost:.3} ")), paint(SUBTEXT, "2;", took)));
    }
    if let Some((name, ver)) = &f.venv {
        let ver = if ver.is_empty() { String::new() } else { paint(YELLOW, "2;", format!("({ver})")) };
        l2.push(format!("{}{ver}", paint(YELLOW, "", format!("{I_PY} {name}"))));
    }
    if let Some(ver) = &f.node {
        l2.push(paint(GREEN, "", format!("{I_NODE} {ver}")));
    }
    let sep = format!(" {} ", paint(OVERLAY, "", "│"));
    [l1, l2].iter().filter(|l| !l.is_empty()).map(|l| l.join(&sep)).collect::<Vec<_>>().join("\n")
}

fn git(branch: &str, c: &Counts) -> String {
    let mut out = paint(TEAL, "3;", format!("{I_GIT} {branch}"));
    let mut add = |on: bool, text: String| {
        if on {
            out.push(' ');
            out.push_str(&text);
        }
    };
    add(c.staged > 0, paint(GREEN, "", format!("{I_STAGED} {}", c.staged)));
    add(c.modified > 0, paint(PEACH, "", format!("{I_MODIFIED} {}", c.modified)));
    add(c.untracked > 0, paint(OVERLAY, "", format!("{I_UNTRACKED} {}", c.untracked)));
    add(
        c.added > 0 || c.removed > 0,
        format!("{}{}", paint(GREEN, "", format!("+{}", c.added)), paint(RED, "", format!("-{}", c.removed))),
    );
    add(c.ahead > 0, paint(GREEN, "", format!("⇡{}", c.ahead)));
    add(c.behind > 0, paint(RED, "", format!("⇣{}", c.behind)));
    out
}

/// 目录怎么写:项目名加项目里的相对路径;不在项目里的写全,家目录写成 ~
fn place(dir: &str, project: Option<&str>, home: &str) -> String {
    let under = |base: &str| dir.strip_prefix(base).filter(|rest| !base.is_empty() && (rest.is_empty() || rest.starts_with('/')));
    if let Some(p) = project.map(|p| p.trim_end_matches('/'))
        && let Some(rel) = under(p)
    {
        return format!("{}{rel}", p.rsplit('/').next().unwrap_or(p));
    }
    under(home.trim_end_matches('/')).map_or_else(|| dir.to_string(), |rest| format!("~{rest}"))
}

/// 账号名和它还没到重置时间的窗口(过了重置时间的数是上一个窗口的,不写),窗口之间一个点;用到八成以上的带上还有多久重置
fn quota(q: &Quota, now: i64) -> Option<String> {
    let live: Vec<&Win> = q.wins.iter().filter(|w| w.resets.is_none_or(|r| r > now)).collect();
    if live.is_empty() {
        return None;
    }
    let mut out = paint(SUBTEXT, "", &q.who);
    for (i, w) in live.into_iter().enumerate() {
        if i > 0 {
            out.push_str(&paint(OVERLAY, "", " ·"));
        }
        out.push_str(&format!(" {} {}", paint(OVERLAY, "", &w.label), paint(level(w.used), "", format!("{:.0}%", w.used))));
        if w.used >= 80.0
            && let Some(r) = w.resets
        {
            out.push_str(&paint(OVERLAY, "", format!(" {}", left(r - now))));
        }
    }
    Some(out)
}

/// 多长一段时间,只写最大的两级:3d4h / 2h10m / 4m5s / 45s
fn span(secs: u64) -> String {
    match (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60, secs % 60) {
        (0, 0, 0, s) => format!("{s}s"),
        (0, 0, m, s) => format!("{m}m{s}s"),
        (0, h, m, _) => format!("{h}h{m}m"),
        (d, h, _, _) => format!("{d}d{h}h"),
    }
}

/// 还有多久重置:同上,不到一小时的不写秒(45m)
fn left(secs: i64) -> String {
    let s = secs.max(0) as u64;
    if s < 3600 { format!("{}m", s / 60) } else { span(s) }
}

/// 从这个目录往上找 `.git`,回(工作区的根, 它的 HEAD 文件)。`.git` 是文件的(worktree、子模块)里面写着真的在哪
fn repo(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    for d in dir.ancestors() {
        let g = d.join(".git");
        let Ok(meta) = std::fs::metadata(&g) else { continue };
        if meta.is_dir() {
            return Some((d.to_path_buf(), g.join("HEAD")));
        }
        let text = std::fs::read_to_string(&g).ok()?;
        let real = Path::new(text.trim().strip_prefix("gitdir:")?.trim());
        return Some((d.to_path_buf(), if real.is_absolute() { real.to_path_buf() } else { d.join(real) }.join("HEAD")));
    }
    None
}

/// 这个文件多少秒没动了;没有这个文件、读不到 = None
fn idle_secs(p: &Path) -> Option<u64> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs())
}

/// git 的数这一回认多久以内算过的:对话记录多久没动了(`idle`)。闲着的会话不为自己重算 ——
/// 同一个工作区里有会话在干活,缓存就是它 5 秒内写的;都闲着,一分钟一次也看不出差别。
/// 输入里没给对话记录(刚开的会话)的照原来的 5 秒
fn git_ttl(idle: Option<u64>) -> u64 {
    if idle.is_some_and(|s| s >= ACTIVE) { GIT_IDLE_TTL } else { GIT_TTL }
}

/// HEAD 和它旁边的 index 各自的修改时间:提交、暂存、切分支、变基都会动其中一个,对不上就不等到期。
/// 光改工作区里的文件动不到它们,那个靠到期。自己起的 git 带 `--no-optional-locks`,不会去写 index
fn stamp(head: &Path) -> String {
    let at = |p: &Path| {
        let t = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
    };
    format!("{}:{}", at(head), at(&head.with_file_name("index")))
}

/// HEAD 里写的:分支名;没在分支上(变基到一半、检出了某个提交)是 `@` 加短提交号
fn branch(head: &Path) -> Option<String> {
    let text = std::fs::read_to_string(head).ok()?;
    let text = text.trim();
    match text.strip_prefix("ref:").map(str::trim) {
        Some(r) => Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string()),
        None => (text.len() >= 7 && text.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("@{}", &text[..7])),
    }
}

/// 两个 git 一起跑:状态(哪些文件、领先落后)和没暂存的改动行数
fn count(dir: &Path) -> Counts {
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        c.arg("--no-optional-locks").arg("-C").arg(dir).args(args);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()
    };
    let (status, numstat) = (git(&["status", "--porcelain=v2", "--branch"]), git(&["diff", "--numstat"]));
    let text = |c: Option<Child>| c.and_then(|c| c.wait_with_output().ok()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let mut c = Counts::default();
    parse_status(&text(status).unwrap_or_default(), &mut c);
    parse_numstat(&text(numstat).unwrap_or_default(), &mut c);
    c
}

/// `git status --porcelain=v2 --branch`:`# branch.ab +领先 -落后`;`1` / `2` / `u` 开头的是跟踪着的文件,
/// 后面两个字符是暂存区和工作区各自的状态(`.` = 没动);`?` 开头的没跟踪
fn parse_status(text: &str, c: &mut Counts) {
    for l in text.lines() {
        if let Some(ab) = l.strip_prefix("# branch.ab ") {
            let mut it = ab.split(' ').map(|x| x.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0));
            (c.ahead, c.behind) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
        } else if l.starts_with("? ") {
            c.untracked += 1;
        } else if matches!(l.as_bytes(), [b'1' | b'2' | b'u', b' ', ..])
            && let [x, y] = l.as_bytes()[2..].get(..2).unwrap_or_default()
        {
            c.staged += u32::from(*x != b'.');
            c.modified += u32::from(*y != b'.');
        }
    }
}

/// `git diff --numstat`:每行「加 减 文件」,二进制文件是 `-`,不算
fn parse_numstat(text: &str, c: &mut Counts) {
    for l in text.lines() {
        let mut it = l.split('\t').map(|x| x.parse::<u64>().unwrap_or(0));
        c.added += it.next().unwrap_or(0);
        c.removed += it.next().unwrap_or(0);
    }
}

/// 用的哪个 Python 环境:(名字, 版本)。开着的(VIRTUAL_ENV、conda)先算,再是这个目录、项目目录里的 `.venv`
fn venv(active: Option<PathBuf>, conda: Option<String>, dir: Option<&Path>, project: Option<&Path>) -> Option<(String, String)> {
    if let Some(v) = active {
        return Some((v.file_name()?.to_string_lossy().into_owned(), py_version(&v)));
    }
    if let Some(c) = conda.filter(|c| !c.is_empty() && c != "base") {
        return Some((c, String::new()));
    }
    let v = [dir, project].into_iter().flatten().map(|d| d.join(".venv")).find(|v| v.join("bin/python").is_file())?;
    Some((".venv".into(), py_version(&v)))
}

/// 环境是哪版 Python 建的:`pyvenv.cfg` 里的 `version = 3.11.4`(venv、uv 写 `version_info`,virtualenv 写成 `3.11.4.final.0`)
fn py_version(venv: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(venv.join("pyvenv.cfg")) else { return String::new() };
    text.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| matches!(k.trim(), "version" | "version_info"))
        .map(|(_, v)| {
            v.trim().split('.').take_while(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).take(3).collect::<Vec<_>>().join(".")
        })
        .unwrap_or_default()
}

/// PATH 里那个 node 的版本。问一次要起一个 node,所以按程序的真实路径和修改时间记着,换了版本(路径或文件变了)才重问
fn node(cache: &Path) -> Option<String> {
    let bin = std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("node")).find(|p| p.is_file())?;
    let real = std::fs::canonicalize(&bin).ok()?;
    let mtime =
        std::fs::metadata(&real).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())?.as_secs();
    let ver = cached(cache, "node", &format!("{}:{mtime}", real.display()), "", u64::MAX, || {
        let out = Command::new(&bin).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok();
        out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    });
    Some(ver).filter(|v| !v.is_empty())
}

/// 存下来的一份:`key` 也写进去,读的时候对一遍(文件名只是它的散列);`stamp` 是算的时候它依据的东西的样子
#[derive(Serialize, Deserialize)]
struct Entry<T> {
    key: String,
    #[serde(default)]
    stamp: String,
    value: T,
}

/// `ttl` 秒内算过这个 `key`、`stamp` 也还是那时的,就用算过的,不然算一次存下来。`stamp` 要在算之前取:
/// 算的途中又变了的,下回对不上再算一次。一个 key 一个文件,放系统的临时目录:丢了只是重算
fn cached<T: Serialize + DeserializeOwned>(dir: &Path, kind: &str, key: &str, stamp: &str, ttl: u64, compute: impl FnOnce() -> T) -> T {
    // FNV-1a:只用来起文件名
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    let path = dir.join(format!("{kind}-{hash:016x}.json"));
    let fresh =
        std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < ttl);
    if fresh
        && let Some(e) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Entry<T>>(&b).ok())
        && e.key == key
        && e.stamp == stamp
    {
        return e.value;
    }
    let e = Entry { key: key.to_string(), stamp: stamp.to_string(), value: compute() };
    if let Ok(text) = serde_json::to_string(&e) {
        let _ = crate::state::write_atomic(&path, &text);
    }
    e.value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Agent;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aca-statusline-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn quota_of(wins: &[(&str, f64, i64)]) -> Quota {
        let wins = wins.iter().map(|(l, u, r)| Win { label: l.to_string(), used: *u, resets: Some(*r) }).collect();
        Quota { agent: Agent::Claude, who: "sub".into(), plan: String::new(), wins, updated: 0, note: String::new() }
    }

    #[test]
    fn both_lines_carry_every_segment_in_order() {
        let v = serde_json::json!({
            "model": {"display_name": "Opus"},
            "workspace": {"current_dir": "/w/app/src", "project_dir": "/w/app"},
            "context_window": {"used_percentage": 42.7},
            "cost": {"total_cost_usd": 1.5, "total_duration_ms": 65000}
        });
        let f = Facts {
            branch: Some("dev_ye".into()),
            git: Counts { staged: 2, modified: 11, untracked: 12, added: 3845, removed: 1816, ahead: 10, behind: 1 },
            venv: Some((".venv".into(), "3.11.4".into())),
            node: Some("v22.1.0".into()),
            quota: Some(quota_of(&[("5h", 9.0, 5000), ("7d", 87.0, 1000 + 2 * 3600 + 600)])),
            home: "/home/me".into(),
            now: 1000,
        };
        let out = render(&v, &f);
        assert_eq!(
            out.lines().map(crate::ansi::strip).collect::<Vec<_>>(),
            [
                format!(
                    "━━━━━━╌╌╌╌╌╌╌╌╌ 42% │ {I_MODEL} Opus │ {I_GIT} dev_ye {I_STAGED} 2 {I_MODIFIED} 11 {I_UNTRACKED} 12 +3845-1816 ⇡10 ⇣1 │ {I_DIR} app/src"
                ),
                format!("sub 5h 9% · 7d 87% 2h10m │ $1.500 1m5s │ {I_PY} .venv(3.11.4) │ {I_NODE} v22.1.0"),
            ]
        );
        // 分支是斜体的青色,模型是粗体的紫色
        assert!(out.contains(&format!("\x1b[3;38;2;23;146;153m{I_GIT} dev_ye\x1b[0m")), "{out:?}");
        assert!(out.contains(&format!("\x1b[1;38;2;136;57;239m{I_MODEL} Opus\x1b[0m")), "{out:?}");
    }

    #[test]
    fn what_is_missing_is_left_out_and_nothing_at_all_prints_nothing() {
        assert_eq!(render(&Value::Null, &Facts::default()), "");
        // 输入读不出来但账号有额度:只有额度那一行,前面不空一行
        let only_quota = Facts { quota: Some(quota_of(&[("5h", 9.0, 5000)])), now: 1000, ..Facts::default() };
        assert_eq!(crate::ansi::strip(&render(&Value::Null, &only_quota)), "sub 5h 9%");
        // 没花钱、没额度、没环境:只有一行;干净的仓库只写分支
        let v = serde_json::json!({"workspace": {"current_dir": "/home/me/x"}, "cost": {"total_cost_usd": 0}});
        let f = Facts { branch: Some("main".into()), home: "/home/me".into(), ..Facts::default() };
        assert_eq!(crate::ansi::strip(&render(&v, &f)), format!("{I_GIT} main │ {I_DIR} ~/x"));
        // 上下文超过一百也只画满
        let full = crate::ansi::strip(&render(&serde_json::json!({"context_window": {"used_percentage": 130}}), &Facts::default()));
        assert_eq!(full, format!("{} 130%", "━".repeat(15)));
    }

    #[test]
    fn a_long_session_reads_in_days_and_hours_not_thousands_of_minutes() {
        assert_eq!([0, 45, 65, 3600, 3725, 86400, 875655].map(span), ["0s", "45s", "1m5s", "1h0m", "1h2m", "1d0h", "10d3h"]);
        let v = serde_json::json!({"cost": {"total_cost_usd": 106.107, "total_duration_ms": 875655000u64}});
        assert_eq!(crate::ansi::strip(&render(&v, &Facts::default())), "$106.107 10d3h");
    }

    #[test]
    fn the_directory_reads_from_the_project_down() {
        assert_eq!(place("/w/app", Some("/w/app"), "/home/me"), "app");
        assert_eq!(place("/w/app/src/x", Some("/w/app/"), "/home/me"), "app/src/x");
        // 名字只是前缀一样的不算在项目里
        assert_eq!(place("/w/app-two", Some("/w/app"), "/w"), "~/app-two");
        assert_eq!(place("/elsewhere/x", Some("/w/app"), "/home/me"), "/elsewhere/x");
        assert_eq!(place("/home/me", None, "/home/me"), "~");
    }

    #[test]
    fn a_window_past_its_reset_is_dropped_and_a_nearly_full_one_shows_the_time_left() {
        let q = |wins: &[(&str, f64, i64)]| quota(&quota_of(wins), 1000).map(|s| crate::ansi::strip(&s));
        assert_eq!(q(&[("5h", 9.0, 999), ("7d", 11.4, 5000)]).as_deref(), Some("sub 7d 11%"));
        assert_eq!(
            q(&[("5h", 80.0, 1000 + 45 * 60), ("7d", 99.0, 1000 + 3 * 86400 + 4 * 3600)]).as_deref(),
            Some("sub 5h 80% 45m · 7d 99% 3d4h")
        );
        assert_eq!(q(&[("5h", 9.0, 1000)]), None);
        // 八成半以上红,六成以上黄
        let colored = quota(&quota_of(&[("5h", 85.0, 5000), ("7d", 60.0, 5000)]), 1000).unwrap();
        assert!(colored.contains("\x1b[38;2;210;15;57m85%") && colored.contains("\x1b[38;2;223;142;29m60%"), "{colored:?}");
    }

    #[test]
    fn the_branch_comes_straight_from_head_in_repos_worktrees_and_detached() {
        let root = tmp("head");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        let at = |d: &Path| super::repo(d).and_then(|(_, h)| branch(&h));
        let root_of = |d: &Path| super::repo(d).map(|(r, _)| r);
        assert_eq!(at(&repo.join("src/deep")).as_deref(), Some("feature/x"));
        // 在哪一层目录里,工作区的根都是同一个(git 的数按它缓存);worktree 的根是它自己,不是主仓库
        assert_eq!(root_of(&repo.join("src/deep")), Some(repo.clone()));
        assert_eq!(root_of(&repo), Some(repo.clone()));
        // worktree:.git 是个文件,指到主仓库里它自己的那份 HEAD(绝对的、相对的都有)
        let wt = root.join("wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/comms\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display())).unwrap();
        assert_eq!(at(&wt.join("sub")).as_deref(), Some("comms"));
        assert_eq!(root_of(&wt.join("sub")), Some(wt.clone()));
        std::fs::write(wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
        assert_eq!(at(&wt).as_deref(), Some("comms"));
        std::fs::write(repo.join(".git/HEAD"), "8ebb433c0ffee0000000000000000000000000aa\n").unwrap();
        assert_eq!(at(&repo).as_deref(), Some("@8ebb433"));
        let bare = root.join("plain");
        std::fs::create_dir_all(&bare).unwrap();
        // 临时目录的上级里要是有仓库,这条就测不了
        if !root.ancestors().any(|d| d.join(".git").exists()) {
            assert_eq!(at(&bare), None);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn git_output_is_counted_the_way_the_bar_shows_it() {
        let mut c = Counts::default();
        let status = "# branch.oid 8ebb433\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +10 -3\n\
                      1 .M N... 100644 100644 100644 aaa bbb src/a.rs\n\
                      1 M. N... 100644 100644 100644 aaa bbb src/b.rs\n\
                      1 MM N... 100644 100644 100644 aaa bbb src/c.rs\n\
                      2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\told.rs\n\
                      u UU N... 100644 100644 100644 100644 aaa bbb ccc both.rs\n\
                      ? notes.md\n? tmp/\n! ignored.log\n";
        parse_status(status, &mut c);
        parse_numstat("12\t3\tsrc/a.rs\n-\t-\tlogo.png\n5\t0\tsrc/c.rs\n", &mut c);
        assert_eq!(c, Counts { staged: 4, modified: 3, untracked: 2, added: 17, removed: 3, ahead: 10, behind: 3 });
        // 没有上游的分支没有 branch.ab 那行
        let mut none = Counts::default();
        parse_status("# branch.oid 8ebb433\n# branch.head main\n", &mut none);
        assert_eq!(none, Counts::default());
    }

    #[test]
    fn two_directories_inside_the_ttl_each_keep_their_own_numbers() {
        let dir = tmp("cache");
        let of = |n: u32| Counts { modified: n, ..Counts::default() };
        assert_eq!(cached(&dir, "git", "/w/a", "1:1", 5, || of(1)), of(1));
        assert_eq!(cached(&dir, "git", "/w/b", "1:1", 5, || of(2)), of(2));
        // 还没过期:不重算,也不是另一个目录的
        assert_eq!(cached(&dir, "git", "/w/a", "1:1", 5, || -> Counts { panic!("重算了") }), of(1));
        assert_eq!(cached(&dir, "git", "/w/b", "1:1", 5, || -> Counts { panic!("重算了") }), of(2));
        // 过期了(ttl 0)重算;文件里记的 key 对不上的(散列撞了)不认
        assert_eq!(cached(&dir, "git", "/w/a", "1:1", 0, || of(3)), of(3));
        let file = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| std::fs::read_to_string(p).unwrap().contains("/w/b"))
            .unwrap();
        std::fs::write(&file, std::fs::read_to_string(&file).unwrap().replace("/w/b", "/w/z")).unwrap();
        assert_eq!(cached(&dir, "git", "/w/b", "1:1", 5, || of(4)), of(4));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_commit_or_a_checkout_recomputes_at_once_and_an_idle_session_waits_longer() {
        let dir = tmp("stamp");
        let of = |n: u32| Counts { modified: n, ..Counts::default() };
        // 还没到期,但 HEAD / index 动过了(记号对不上):重算,存的是新记号,同一个文件(不是一动就多一个)
        assert_eq!(cached(&dir, "git", "/w/a", "1:1", 60, || of(1)), of(1));
        assert_eq!(cached(&dir, "git", "/w/a", "1:2", 60, || of(2)), of(2));
        assert_eq!(cached(&dir, "git", "/w/a", "1:2", 60, || -> Counts { panic!("重算了") }), of(2));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        // 旧版存的没有记号那一项:读得了,当成空的记号(node 的版本一直这么存)
        let file = std::fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
        std::fs::write(
            &file,
            r#"{"key":"/w/a","value":{"staged":0,"modified":7,"untracked":0,"added":0,"removed":0,"ahead":0,"behind":0}}"#,
        )
        .unwrap();
        assert_eq!(cached(&dir, "git", "/w/a", "", 60, || -> Counts { panic!("重算了") }), of(7));
        assert_eq!(cached(&dir, "git", "/w/a", "1:2", 60, || of(8)), of(8));

        // 对话记录 30 秒内动过 = 在对话,5 秒;再久 = 闲着,60 秒;输入里没给的照 5 秒
        assert_eq!([None, Some(0), Some(29), Some(30), Some(72000)].map(git_ttl), [5, 5, 5, 60, 60]);
        let log = dir.join("t.jsonl");
        assert_eq!(idle_secs(&log), None);
        std::fs::write(&log, "{}\n").unwrap();
        assert!(idle_secs(&log).is_some_and(|s| s < 5));
        let ago = |p: &Path, secs: u64| {
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs)).unwrap();
        };
        ago(&log, 100);
        assert!(idle_secs(&log).is_some_and(|s| (100..110).contains(&s)));

        // 记号 = HEAD 和它旁边 index 的修改时间:哪个动了都变;还没有 index 的新仓库也有记号
        let git = dir.join("repo/.git");
        std::fs::create_dir_all(&git).unwrap();
        let (head, index) = (git.join("HEAD"), git.join("index"));
        std::fs::write(&head, "ref: refs/heads/main\n").unwrap();
        let new = stamp(&head);
        assert!(new.ends_with(":0"), "{new}");
        std::fs::write(&index, "").unwrap();
        let a = stamp(&head);
        ago(&index, 50);
        let b = stamp(&head);
        ago(&head, 50);
        let c = stamp(&head);
        assert!(new != a && a != b && b != c, "{new} {a} {b} {c}");
        assert_eq!(c, stamp(&head));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_python_version_is_read_from_the_environment_itself() {
        let root = tmp("venv");
        let mk = |name: &str, cfg: &str| {
            let v = root.join(name);
            std::fs::create_dir_all(v.join("bin")).unwrap();
            std::fs::write(v.join("bin/python"), "").unwrap();
            std::fs::write(v.join("pyvenv.cfg"), cfg).unwrap();
            v
        };
        assert_eq!(py_version(&mk("a", "home = /usr/bin\nversion = 3.11.4\n")), "3.11.4");
        assert_eq!(py_version(&mk("b", "home = /usr/bin\nversion_info = 3.12.3.final.0\nuv = 0.4\n")), "3.12.3");
        assert_eq!(py_version(&root.join("none")), "");
        // 开着的环境先算;conda 的 base 不算;再是目录里的 .venv,没有才看项目的
        let (proj, sub) = (root.join("proj"), root.join("proj/sub"));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(proj.join(".venv/bin")).unwrap();
        std::fs::write(proj.join(".venv/bin/python"), "").unwrap();
        std::fs::write(proj.join(".venv/pyvenv.cfg"), "version_info = 3.13.0\n").unwrap();
        assert_eq!(venv(Some(root.join("a")), Some("ml".into()), Some(&sub), Some(&proj)), Some(("a".into(), "3.11.4".into())));
        assert_eq!(venv(None, Some("ml".into()), Some(&sub), Some(&proj)), Some(("ml".into(), String::new())));
        assert_eq!(venv(None, Some("base".into()), Some(&sub), Some(&proj)), Some((".venv".into(), "3.13.0".into())));
        assert_eq!(venv(None, None, Some(&sub), None), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_idle_session_nobody_watches_keeps_its_last_bar() {
        // 在说话的:照常重算,不去问有没有人在看
        assert!(!still(Some(ACTIVE - 1), || panic!("不该问")));
        assert!(!still(None, || panic!("不该问")));
        // 闲着的:没人看才原样打
        assert!(still(Some(ACTIVE), || false));
        assert!(!still(Some(3600), || true));
        assert!(plain("78c6b838-1895-4f3b-b41f-d9308d892247"));
        assert!(!plain("") && !plain("../x") && !plain("a/b"));
    }
}
