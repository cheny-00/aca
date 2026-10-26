//! Claude 的订阅额度,带「这份数是什么时候的」。
//!
//! 只记「谁最后写的」不行:每个会话的 statusLine 手里是它自己最后一次调 API 时的 rate_limits,
//! 闲着的会话一重画状态栏(改一下 settings.json 所有会话都会重画)就把旧数写回去,
//! 面板上的百分比就往回跳。rate_limits 里只有百分比和重置时间,没有时间,所以 statusLine 先过
//! `aca limits -- <原来的命令>`(或者用 `aca statusline`,它自己画,记法一样),按会话各记一份,每个窗口带上数据的时间:
//! - 这个会话第一次报这个窗口:取它对话记录里最后一条回复的时间(状态栏刷新时这条已经写进去了,
//!   闲着的老会话报的正是那时候的数);一条回复都没有 = 刚开的会话刚调完,就是现在
//! - 数变了:就是现在(一个会话的数只在它自己调完 API 时才会变)
//! - 没变:取原来的和最后一条回复里晚的那个
//!
//! 面板上每个账号每个窗口取时间最新的那份,额度提前重置、数往下走也跟得上。
//! 记在 ~/.local/state/aca/ui/limits/<会话 id>.json;八天没动的删掉。
//!
//! 还有一条不过状态栏的路:aca 起 Claude 时带进去的 mod(`aca_core::agent::claude_mod`,`[tools.claude] mod = true` 才带),
//! Claude 每次量出用量它就交给 `aca limits --mod`。它只在数是新的时候交(一起来、每轮结束、额度每动一个点;
//! 接着旧对话起的会话一起来交的也是账号现在的数,2.1.295 上实测),所以时间就是交来的那一刻,不用翻对话记录。
//! 各记各的文件(`mod-<aca 的会话 id>.json`):同一个会话两条路都报时,数差一位小数也不会互相当成「变了」。
//! 所有会话每次刷新都跑这个程序(hook 也是每个事件跑一次),所以装新版 aca 要拷成临时名再改名换过去,
//! 别直接往原文件上拷:拷到一半时起来的那些进程读到的是半个程序。
//!
//! 上面两条路都只有 5h、7d,按模型分的周额度(Fable)只有 Claude 自己的 `/usage` 有。所以还有第三条:
//! 一个账号有会话报来新数、离上次问过了 [`ASK_EVERY`],就在后台起 `aca limits --ask` 问 Claude 自己(见 [`ask`]),
//! 记在 `ask-<账号>.json`,时间也是问的那一刻。
//!
//! 给外面的程序(比如菜单栏)用的两样,都不联网、不轮询:`aca limits --json` 打一份现在的(每个账号合好的,
//! Codex 的读它最近的对话记录,见 [`usage::codex_now`]);数动了往事件流里写一条 `limits.changed`(见 [`announce`]),
//! 订了它的不用隔一会儿来问一次。Codex 不过状态栏,它一轮跑完时 hook 起一个 `aca limits --codex` 看一眼。

use std::collections::HashMap;
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aca_core::agent::claude_mod::Verdict;
use aca_core::config::Tool;
use aca_core::{t, tr};
use memchr::memmem::Finder;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use serde::{Deserialize, Serialize};

use crate::data::Agent;
use crate::overlay::{Fx, Setting};
use crate::usage::{self, Quota, Source, Win, parse_ts};

/// 一个会话记下的
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Rec {
    /// 账号目录(对话记录在它的 projects/ 下)
    dir: String,
    wins: Vec<RWin>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RWin {
    label: String,
    used: f64,
    resets: Option<i64>,
    /// 这份数是什么时候的
    at: i64,
}

pub fn dir() -> Option<PathBuf> {
    crate::state::state_file("limits")
}

/// `aca limits [-- 命令…]`:记下这次 statusLine 交来的额度,再把原样的输入交给后面的命令,
/// 它的输出和退出码照转。自己这边出什么错都不吭声:状态栏命令一报错,Claude Code 就把状态栏画成空的
pub fn run(args: &[String]) -> i32 {
    // 这两条没有输入:不读标准输入(在终端里敲 --json 时读它就卡住了)
    match args.first().map(String::as_str) {
        Some("--json") => return print_json(),
        Some("--codex") => return codex_turn(),
        _ => {}
    }
    let mut payload = vec![];
    let _ = std::io::stdin().read_to_end(&mut payload);
    if args.first().is_some_and(|a| a == "--ask") {
        return args.get(1).map_or(0, |acct| ask(acct));
    }
    if args.first().is_some_and(|a| a == "--mod") {
        return from_mod(&payload);
    }
    if let Some(d) = dir() {
        record(&payload, &d, now());
    }
    let cmd: Vec<&String> = args.iter().skip_while(|a| *a != "--").skip(1).collect();
    let Some((prog, rest)) = cmd.split_first() else { return 0 };
    let child = Command::new(prog).args(rest).stdin(Stdio::piped()).spawn();
    let Ok(mut child) = child else { return 1 };
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(&payload);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Deserialize)]
struct Payload {
    session_id: Option<String>,
    transcript_path: Option<String>,
    rate_limits: Option<serde_json::Map<String, serde_json::Value>>,
}

fn label_of(key: &str) -> String {
    match key {
        "five_hour" => "5h".into(),
        "seven_day" => "7d".into(),
        _ => key.into(),
    }
}

/// 窗口排个固定的序:5h、7d,其余按名字
fn order(label: &str) -> (u8, String) {
    (
        match label {
            "5h" => 0,
            "7d" => 1,
            _ => 2,
        },
        label.to_string(),
    )
}

/// 文件名里用得的 id
fn plain(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 一个窗口:(名字, 用了百分之几, 什么时候重置)
type Got = (String, f64, Option<i64>);

/// 记一次:会话文件里每个窗口的数和时间按文件头的规则更新,没变就不写
pub fn record(payload: &[u8], dir: &Path, now: i64) {
    let Ok(p) = serde_json::from_slice::<Payload>(payload) else { return };
    let (Some(sid), Some(rl)) = (p.session_id, p.rate_limits) else { return };
    if !plain(&sid) {
        return;
    }
    let got: Vec<Got> = rl
        .iter()
        .filter_map(|(k, v)| Some((label_of(k), v.get("used_percentage")?.as_f64()?, v.get("resets_at").and_then(|x| x.as_i64()))))
        .collect();
    let transcript = p.transcript_path.map(PathBuf::from);
    let acct = account_dir(transcript.as_deref());
    let moved = update(&dir.join(format!("{sid}.json")), got, &acct, transcript.as_deref(), now, false);
    if moved {
        announce(dir, &acct);
    }
    ask_if_due(dir, &acct, moved, now);
}

/// mod 交来的 `session.measure`(Claude 2.1.295 上的样子):
/// `{"rateLimits": [{"kind": "five_hour", "percentUsed": 6, "resetsAt": "2026-10-09T11:40:00.000Z"}, …], "context": …, "cost": …}`。
/// 不是订阅的账号 `rateLimits` 是空的。连这个键都没有 / 不是这个形状 = 认不出(None):接口变了
fn measured(payload: &[u8]) -> Option<Vec<Got>> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    v.get("rateLimits")?
        .as_array()?
        .iter()
        .map(|w| {
            let resets = w.get("resetsAt").and_then(|x| x.as_str()).map(parse_ts).filter(|&t| t > 0);
            Some((label_of(w.get("kind")?.as_str()?), w.get("percentUsed")?.as_f64()?, resets))
        })
        .collect()
}

/// `aca limits --mod`:aca 的 mod 在 Claude 里交来一次用量。是哪个会话看环境里的 `ACA_SESSION_ID`,哪个账号看
/// `CLAUDE_CONFIG_DIR`(mod 起的子进程带着会话的环境)。同 hook:永远退出 0、一个字不写
fn from_mod(payload: &[u8]) -> i32 {
    let Some(id) = std::env::var("ACA_SESSION_ID").ok().filter(|id| plain(id)) else { return 0 };
    // Claude 的后台会话继承了原窗格的环境,不是这个会话本人(同 `aca hook`)
    if std::env::var_os("CLAUDE_JOB_DIR").is_some() {
        return 0;
    }
    let got = measured(payload);
    aca_core::agent::claude_mod::heard(&id, got.is_some());
    if let (Some(got), Some(d)) = (got, dir()) {
        let (acct, now) = (account_dir(None), now());
        let moved = update(&d.join(format!("mod-{id}.json")), got, &acct, None, now, true);
        if moved {
            announce(&d, &acct);
        }
        ask_if_due(&d, &acct, moved, now);
    }
    0
}

/// 一个账号多久问一次 Claude 自己(秒)
const ASK_EVERY: i64 = 300;
/// 问 Claude 最多等多久(秒):实测两三秒答完
const ASK_WAIT: u64 = 20;

/// 一个账号问 Claude 自己记下的那份(`ask-<账号>.json`,`quotas` 和会话的一起读)和它的锁(`.lock`,里面写上次问的时间)
fn ask_files(dir: &Path, acct: &str) -> (PathBuf, PathBuf) {
    let mut h = DefaultHasher::new();
    std::fs::canonicalize(acct).unwrap_or_else(|_| PathBuf::from(acct)).hash(&mut h);
    let key = format!("ask-{:016x}", h.finish());
    (dir.join(format!("{key}.json")), dir.join(format!("{key}.lock")))
}

/// 状态栏 / mod 刚记了一次:这个账号的数动了(有会话刚调过 API)、离上次问过了 [`ASK_EVERY`],或者从没问过,
/// 就在后台起一个 `aca limits --ask`。这里只看锁文件的修改时间,几十个会话一起刷新也不多花什么;
/// 一起起了好几个的,真去问的只有一个(见 [`ask`])
fn ask_if_due(dir: &Path, acct: &str, moved: bool, now: i64) {
    let (_, lock) = ask_files(dir, acct);
    let last = std::fs::metadata(&lock).and_then(|m| m.modified()).ok();
    let age = last.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| now - d.as_secs() as i64);
    // 测试里的「自己」是测试程序
    if !due(age, moved) || cfg!(test) {
        return;
    }
    // 自己一个进程组、不接管道:状态栏的命令等的是它自己的输出,不等这个
    let _ = aca_core::proc::detach(
        Command::new(aca_core::agent::self_exe())
            .args(["limits", "--ask", acct])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

/// age = 上次问到现在几秒,没问过是 None
fn due(age: Option<i64>, moved: bool) -> bool {
    age.is_none_or(|a| moved && a >= ASK_EVERY)
}

/// `aca limits --ask <账号目录>`:问 Claude 自己这个账号的额度(SDK 的 `get_usage`,和 `/usage` 一个来源),
/// 按模型分的周额度(Fable)只有这里有。带着这个账号的环境起一个 `claude -p`(同 `aca run`:不带 hook),
/// 不读设置、不连 MCP、不留对话记录,问完就关。只问登记了配置目录的账号:没登记的带着的可能是外层会话的账号。
/// 先锁住锁文件、再看一次上次是什么时候问的,一起起来的几个只有一个真问。同 hook:永远退出 0、一个字不写
fn ask(acct: &str) -> i32 {
    let Some(dir) = dir() else { return 0 };
    let (file, lock) = ask_files(&dir, acct);
    let _ = std::fs::create_dir_all(&dir);
    let Ok(held) = std::fs::OpenOptions::new().create(true).append(true).open(&lock) else { return 0 };
    if held.try_lock().is_err() {
        return 0;
    }
    let now = now();
    let last: Option<i64> = std::fs::read_to_string(&lock).ok().and_then(|s| s.trim().parse().ok());
    if last.is_some_and(|t| now - t < ASK_EVERY) {
        return 0;
    }
    // 问不到(没登录、限流)也算问过:不然下一次刷新又来
    let _ = std::fs::write(&lock, now.to_string());
    let cfg = aca_core::config::load();
    let Some(spec) = spec_of(&cfg, acct) else { return 0 };
    if let Some(got) = asked(&cfg, &spec)
        && update(&file, got, acct, None, self::now(), true)
    {
        announce(&dir, acct);
    }
    0
}

/// 账号目录 → `aca run` 的写法(`claude:work`):登记了配置目录的才有
fn spec_of(cfg: &aca_core::config::Config, acct: &str) -> Option<String> {
    let real = std::fs::canonicalize(acct).ok()?;
    let same = |home: &str| !home.is_empty() && std::fs::canonicalize(home).is_ok_and(|h| h == real);
    let name = cfg.accounts.get("claude")?.iter().find(|(_, a)| same(&a.home))?.0;
    Some(format!("claude:{name}"))
}

/// 起 Claude 问一次,它答了就是那几个窗口;没答、起不来、不是订阅的(rate_limits 是 null)= None / 空的
fn asked(cfg: &aca_core::config::Config, spec: &str) -> Option<Vec<Got>> {
    use std::os::unix::process::CommandExt;
    let args = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--no-session-persistence",
    ]
    .map(String::from);
    let l = aca_core::agent::direct(cfg, spec, &args).ok()?;
    let mut child = Command::new(&l.argv[0])
        .args(&l.argv[1..])
        .envs(l.env)
        .current_dir(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    // 发完就关:它答完这一条自己退出(2.1.303 上实测)
    let ask =
        serde_json::json!({"type": "control_request", "request_id": "aca", "request": {"subtype": "get_usage", "skip_behaviors": true}});
    if let Some(mut i) = child.stdin.take() {
        let _ = writeln!(i, "{ask}");
    }
    let began = std::time::Instant::now();
    while child.try_wait().ok()?.is_none() {
        if began.elapsed().as_secs() >= ASK_WAIT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    usage_of(&child.wait_with_output().ok()?.stdout)
}

/// `get_usage` 的回答(Claude 2.1.303 上的样子,SDK 里标着 experimental):
/// `{"type":"control_response","response":{"subtype":"success","request_id":"aca","response":{"rate_limits":{"five_hour":{"utilization":47,"resets_at":"2026-10-19T11:30:00.428198+00:00",…},"seven_day":{…},…,"model_scoped":[{"display_name":"Fable","utilization":48,"resets_at":"…"}]},…}}}`。
/// model_scoped 里的窗口名字是 Claude 给的(Fable),照着记
fn usage_of(out: &[u8]) -> Option<Vec<Got>> {
    out.split(|&b| b == b'\n').find_map(|line| {
        let v: serde_json::Value = serde_json::from_slice(line).ok()?;
        let r = v.get("response").filter(|r| r.get("request_id").and_then(|x| x.as_str()) == Some("aca"))?;
        let rl = r.pointer("/response/rate_limits")?;
        let win = |label: String, w: &serde_json::Value| {
            let resets = w.get("resets_at").and_then(|x| x.as_str()).map(parse_ts).filter(|&t| t > 0);
            Some((label, w.get("utilization")?.as_f64()?, resets))
        };
        let mut got: Vec<Got> = ["five_hour", "seven_day"].iter().filter_map(|k| win(label_of(k), rl.get(*k)?)).collect();
        let scoped = rl.get("model_scoped").and_then(|x| x.as_array()).into_iter().flatten();
        got.extend(scoped.filter_map(|m| win(m.get("display_name")?.as_str()?.to_string(), m)));
        Some(got)
    })
}

/// 计费面板额度那一段顶上的一句:开了 mod(`[tools.<claude 那一家的>] mod = true`),现在这一版 Claude 上却没带。
/// 没开、带着、还没起过会话(没验过)= 空的。为什么没带写在设置页 mod 那一行
pub fn mod_note(cfg: &aca_core::config::Config) -> String {
    let on = cfg.names().iter().filter_map(|n| cfg.tool(n.split(':').next()?)).any(|t| t.kind == "claude" && t.mods);
    match on.then(aca_core::agent::claude_mod::verdict).flatten() {
        Some(v) if !v.ok => t!("额度可能不是最新数据", "quotas may be out of date").into(),
        _ => String::new(),
    }
}

/// `^S` 里 mod 那一行读写的口子(测试里换成假的)
#[derive(Clone, Copy)]
pub struct ModIo {
    /// `[tools.claude]` 现在怎么写的
    tool: fn() -> Tool,
    /// 现在这一版 Claude 带不带(还没验过 = None)
    verdict: fn() -> Option<Verdict>,
    set: fn(&str, &str, toml::Value) -> Result<(), String>,
}

/// 设置页里的一行:「m  Claude mod  开」,开着却不生效的后面写为什么。⏎ 当场切,改的是 `[tools.claude] mod`;
/// 开着的会话不动,下次起(新建、重启)才跟着变
pub struct ModRow {
    io: ModIo,
}

impl ModRow {
    pub fn real() -> ModRow {
        ModRow {
            io: ModIo {
                tool: || aca_core::config::load().tool("claude").unwrap_or_default(),
                verdict: aca_core::agent::claude_mod::verdict,
                set: crate::config::set,
            },
        }
    }

    /// 开着却不生效:行上括号里写的、开它时底栏说的为什么。带得上(还没验过的也算)= None
    fn not_carried(&self, tool: &Tool) -> Option<(String, String)> {
        // 自己包了一层命令:不知道里面跑的是哪一版 Claude,不带
        if !tool.cmd.is_empty() {
            return Some((
                t!("已自定义命令", "custom command").into(),
                t!("已自定义命令,mod 不生效", "a custom command is set; the mod has no effect").into(),
            ));
        }
        match (self.io.verdict)() {
            Some(v) if !v.ok => {
                Some((format!("Claude {}", v.build), tr!("Claude {} 未加载 mod:{}", "Claude {} won't load the mod: {}", v.build, v.why)))
            }
            _ => None,
        }
    }
}

impl Setting for ModRow {
    fn key(&self) -> char {
        'm'
    }

    fn name(&self) -> &'static str {
        "Claude mod"
    }

    fn value(&self) -> Vec<Span<'static>> {
        let tool = (self.io.tool)();
        let bold = Style::new().add_modifier(Modifier::BOLD);
        if !tool.mods {
            return vec![Span::styled(t!("关", "off"), bold)];
        }
        let mut out = vec![Span::styled(t!("开", "on"), bold)];
        if let Some((short, _)) = self.not_carried(&tool) {
            out.push(Span::styled(tr!(" · 不生效({short})", " · no effect ({short})"), crate::theme::warn()));
        }
        out
    }

    fn mark(&self) -> &'static str {
        ""
    }

    fn enter(&self, fx: &mut Fx) {
        let tool = (self.io.tool)();
        let on = !tool.mods;
        if let Err(e) = (self.io.set)("tools.claude", "mod", toml::Value::Boolean(on)) {
            return fx.say(e);
        }
        // 开着的会话不动,新起、重启的才跟着变
        fx.say(match self.not_carried(&tool) {
            Some((_, why)) if on => why,
            _ => t!("已保存,新会话生效", "saved; new sessions use it").into(),
        });
    }
}

/// 一个会话这次报的几个窗口 → 它的文件,acct 是它的账号目录。`live` = 这些数就是现在的(mod 交来的、问来的);
/// 不是的话时间照文件头的规则从对话记录里推。返回有没有哪个窗口的数变了(头一回报的也算)
fn update(path: &Path, mut got: Vec<Got>, acct: &str, transcript: Option<&Path>, now: i64, live: bool) -> bool {
    if got.is_empty() {
        return false;
    }
    got.sort_by_key(|w| order(&w.0));
    let old: Rec = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let last = if live { None } else { transcript.and_then(last_reply) };
    let mut rec = Rec { dir: acct.to_string(), wins: old.wins.clone() };
    let mut moved = false;
    for (label, used, resets) in got {
        let before = old.wins.iter().find(|w| w.label == label);
        moved |= before.is_none_or(|w| w.used != used || w.resets != resets);
        let at = match before {
            _ if live => now,
            Some(w) if w.used == used && w.resets == resets => w.at.max(last.unwrap_or(0)),
            Some(_) => now,
            None => last.unwrap_or(now),
        };
        let w = RWin { label: label.clone(), used, resets, at };
        match rec.wins.iter_mut().find(|w| w.label == label) {
            Some(x) => *x = w,
            None => rec.wins.push(w),
        }
    }
    rec.wins.sort_by_key(|w| order(&w.label));
    if rec != old
        && let Ok(text) = serde_json::to_string(&rec)
    {
        let _ = crate::state::write_atomic(path, &text);
    }
    moved
}

/// 对话记录是 `<账号目录>/projects/<项目>/<会话>.jsonl`;对不上就按 Claude Code 自己的规矩猜
fn account_dir(transcript: Option<&Path>) -> String {
    let from_path =
        transcript.and_then(|t| t.parent()?.parent()).filter(|p| p.file_name().is_some_and(|n| n == "projects")).and_then(Path::parent);
    match from_path {
        Some(d) => d.to_string_lossy().into_owned(),
        None => std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| format!("{}/.claude", std::env::var("HOME").unwrap_or_default())),
    }
}

static F_REPLY: std::sync::LazyLock<Finder<'static>> = std::sync::LazyLock::new(|| Finder::new(b"\"type\":\"assistant\""));

/// 对话记录里最后一条回复的时间:从尾巴往前一块一块读,最多读 8 MB(工具输出一行能有好几 MB)
fn last_reply(path: &Path) -> Option<i64> {
    const CHUNK: u64 = 256 << 10;
    const LIMIT: u64 = 8 << 20;
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut end = len;
    // 后一块开头那半行,接到前一块后面
    let mut carry: Vec<u8> = vec![];
    while end > 0 && len - end < LIMIT {
        let start = end.saturating_sub(CHUNK);
        let mut buf = vec![0; (end - start) as usize];
        f.seek(SeekFrom::Start(start)).ok()?;
        f.read_exact(&mut buf).ok()?;
        buf.extend_from_slice(&carry);
        let cut = if start == 0 { 0 } else { memchr::memchr(b'\n', &buf).map_or(buf.len(), |i| i + 1) };
        for line in buf[cut..].rsplit(|&b| b == b'\n') {
            if F_REPLY.find(line).is_none() {
                continue;
            }
            #[derive(Deserialize)]
            struct L {
                #[serde(rename = "type")]
                kind: Option<String>,
                timestamp: Option<String>,
            }
            if let Ok(L { kind: Some(k), timestamp: Some(ts) }) = serde_json::from_slice::<L>(line)
                && k == "assistant"
            {
                return Some(parse_ts(&ts)).filter(|&t| t > 0);
            }
        }
        carry = buf[..cut].to_vec();
        end = start;
    }
    None
}

/// 登记的 Claude 账号:(真实目录, 名字)
fn names(srcs: &[Source]) -> Vec<(PathBuf, &str)> {
    srcs.iter().filter(|s| s.agent == Agent::Claude).filter_map(|s| Some((std::fs::canonicalize(&s.dir).ok()?, s.who.as_str()))).collect()
}

/// 账号目录 → 计费里的账号名:对得上登记的用登记的名字,对不上用目录名
fn who_of(names: &[(PathBuf, &str)], dir: &str) -> String {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| PathBuf::from(dir));
    names
        .iter()
        .find(|(d, _)| *d == real)
        .map(|(_, w)| w.to_string())
        .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or("").trim_start_matches('.').to_string())
}

/// 状态栏用:这段对话记录所在账号的那份(同 `quotas`,取的是这个账号所有会话里最新的,不是这个会话自己手里的)。
/// 这个账号一个窗口都没记过 = None
pub fn quota_for(dir: &Path, srcs: &[Source], transcript: Option<&Path>, now: i64) -> Option<Quota> {
    let who = who_of(&names(srcs), &account_dir(transcript));
    quotas(dir, srcs, now).into_iter().find(|q| q.who == who)
}

/// 一个账号合好的那份
struct Merged {
    /// 账号名(同计费)
    who: String,
    /// 账号目录(记下它的会话报的)
    dir: String,
    wins: Vec<RWin>,
}

/// 每个账号每个窗口取时间最新的那份(一样新的取重置更晚、用得更多的),按账号名排。
/// 账号名跟计费的一样(按账号目录对上 srcs);顺带删掉八天没动的会话文件
fn merged(dir: &Path, srcs: &[Source]) -> Vec<Merged> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let names = names(srcs);
    let mut best: HashMap<String, (String, HashMap<String, RWin>)> = HashMap::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let age = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a.as_secs() > 8 * 86400) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let Some(rec) = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Rec>(&b).ok()) else { continue };
        let (_, wins) = best.entry(who_of(&names, &rec.dir)).or_insert_with(|| (rec.dir.clone(), HashMap::new()));
        for w in rec.wins {
            let newer = |o: &RWin| (w.at, w.resets, w.used) > (o.at, o.resets, o.used);
            if wins.get(&w.label).is_none_or(newer) {
                wins.insert(w.label.clone(), w);
            }
        }
    }
    let mut out: Vec<Merged> = best
        .into_iter()
        .map(|(who, (dir, m))| {
            let mut wins: Vec<RWin> = m.into_values().collect();
            wins.sort_by_key(|w| order(&w.label));
            Merged { who, dir, wins }
        })
        .collect();
    out.sort_by(|a, b| a.who.cmp(&b.who));
    out
}

/// 面板用:每个账号合好的那份(见 [`merged`])
pub fn quotas(dir: &Path, srcs: &[Source], now: i64) -> Vec<Quota> {
    merged(dir, srcs)
        .into_iter()
        .map(|m| Quota {
            agent: Agent::Claude,
            who: m.who,
            plan: String::new(),
            updated: m.wins.iter().map(|w| w.at).max().unwrap_or(0).min(now),
            wins: m.wins.into_iter().map(|w| Win { label: w.label, used: w.used, resets: w.resets }).collect(),
            note: String::new(),
        })
        .collect()
}

// ---------- 给外面的程序:--json 和 limits.changed ----------

/// 一个账号在 `--json` / `limits.changed` 里的样子:`{"tool", "name", "dir", "wins": [{"label", "used", "resets", "at"}]}`
fn account_json(tool: &str, name: &str, dir: &str, wins: &[RWin]) -> serde_json::Value {
    let wins: Vec<serde_json::Value> =
        wins.iter().map(|w| serde_json::json!({ "label": w.label, "used": w.used, "resets": w.resets, "at": w.at })).collect();
    serde_json::json!({ "tool": tool, "name": name, "dir": dir, "wins": wins })
}

/// Codex 一个账号的那份 → 窗口(同一条记录里的,时间都是它记下的那一刻)
fn codex_wins(q: &Quota) -> Vec<RWin> {
    q.wins.iter().map(|w| RWin { label: w.label.clone(), used: w.used, resets: w.resets, at: q.updated }).collect()
}

/// Codex 每个订阅账号现在的额度:各读最近几份对话记录的尾巴(`usage::codex_now`,不扫整个目录),
/// 用 API key 登录的不要(`usage::subscribed`)。(账号目录, 那份)
fn codex_all(accounts: &[(Agent, String, String)], home: &str) -> Vec<(PathBuf, Quota)> {
    usage::sources(accounts, home)
        .into_iter()
        .filter(|s| s.agent == Agent::Codex)
        .filter_map(|s| {
            let q = usage::codex_now(&s)?;
            Some((s.dir, usage::subscribed(vec![q], accounts, home).pop()?))
        })
        .collect()
}

/// `aca limits --json`:现在每个账号的额度,一个 JSON 打在标准输出(`{"now", "accounts": […]}`)。
/// Claude 的是记下的那些合好的(同计费面板),Codex 的读它最近的对话记录。没记到过的账号不列
fn print_json() -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = crate::data::accounts(&aca_core::config::load());
    let srcs = usage::sources(&accounts, &home);
    let mut out: Vec<serde_json::Value> = vec![];
    if let Some(d) = dir() {
        out.extend(merged(&d, &srcs).iter().map(|m| account_json("claude", &m.who, &m.dir, &m.wins)));
    }
    for (dir, q) in codex_all(&accounts, &home) {
        out.push(account_json("codex", &q.who, &dir.to_string_lossy(), &codex_wins(&q)));
    }
    println!("{}", serde_json::json!({ "now": now(), "accounts": out }));
    0
}

/// 一个账号上回发出去的数记在哪:`seen-<哈希>.last`(不是 .json,合的时候不会当成会话的)
fn seen_file(dir: &Path, tool: &str, acct: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    tool.hash(&mut h);
    std::fs::canonicalize(acct).unwrap_or_else(|_| PathBuf::from(acct)).hash(&mut h);
    dir.join(format!("seen-{:016x}.last", h.finish()))
}

/// 这个账号的数跟上回发出去的比:每个窗口的名字、用了多少、什么时候重置(时间不算:数没动只是又报了一次)。
/// 变了就记下这回的、返回 true
fn differs(dir: &Path, tool: &str, acct: &str, wins: &[RWin]) -> bool {
    let key: String = wins.iter().map(|w| format!("{} {} {}\n", w.label, w.used, w.resets.unwrap_or(0))).collect();
    let file = seen_file(dir, tool, acct);
    if std::fs::read_to_string(&file).is_ok_and(|old| old == key) {
        return false;
    }
    let _ = crate::state::write_atomic(&file, &key);
    true
}

/// Claude 一个账号刚记了一次、某个会话的数动了:合好的那份要是跟上回发出去的不一样,返回 `limits.changed` 的字段。
/// 会话自己的数动了不一定是账号的数动了(闲着的老会话头一回报它手里的旧数,合的时候输给新的)
fn changed(dir: &Path, srcs: &[Source], acct: &str) -> Option<serde_json::Value> {
    let who = who_of(&names(srcs), acct);
    let m = merged(dir, srcs).into_iter().find(|m| m.who == who)?;
    differs(dir, "claude", acct, &m.wins).then(|| account_json("claude", &m.who, &m.dir, &m.wins))
}

/// 事件是哪个会话报的:aca 起的会话环境里有 `ACA_SESSION_ID`,状态栏、mod、hook 起的子进程都带着;别的是空的
fn reporter() -> String {
    std::env::var("ACA_SESSION_ID").ok().filter(|id| plain(id)).unwrap_or_default()
}

/// 记完一次、这个会话的数动了(状态栏每 10 秒刷新一次,数没动的根本走不到这里):账号合好的那份也变了,
/// 往事件流写一条 `limits.changed`。同 hook:出什么错都不吭声
fn announce(dir: &Path, acct: &str) {
    // 测试里写的是假账号,别发到真的事件流里
    if cfg!(test) {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let srcs = usage::sources(&crate::data::accounts(&aca_core::config::load()), &home);
    if let Some(data) = changed(dir, &srcs, acct) {
        aca_core::event::emit("limits.changed", &reporter(), data);
    }
}

/// `aca limits --codex`:Codex 一轮跑完时 hook 在后台起的(Codex 不过状态栏)。各个 Codex 账号的数跟上回发出去的比,
/// 变了的发 `limits.changed`。同 hook:永远退出 0、一个字不写
fn codex_turn() -> i32 {
    let Some(dir) = dir() else { return 0 };
    // 这一轮最后那条用量(带着额度)是 Codex 在后台写的,等它落盘
    std::thread::sleep(std::time::Duration::from_secs(1));
    let home = std::env::var("HOME").unwrap_or_default();
    let accounts = crate::data::accounts(&aca_core::config::load());
    let _ = std::fs::create_dir_all(&dir);
    for (acct, q) in codex_all(&accounts, &home) {
        let (acct, wins) = (acct.to_string_lossy(), codex_wins(&q));
        if differs(&dir, "codex", &acct, &wins) {
            aca_core::event::emit("limits.changed", &reporter(), account_json("codex", &q.who, &acct, &wins));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(sid: &str, transcript: &Path, five: f64, seven: f64) -> Vec<u8> {
        serde_json::json!({
            "session_id": sid,
            "transcript_path": transcript,
            "rate_limits": {
                "five_hour": {"used_percentage": five, "resets_at": 5000},
                "seven_day": {"used_percentage": seven, "resets_at": 9000}
            }
        })
        .to_string()
        .into_bytes()
    }

    fn reply(ts: &str) -> String {
        format!("{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{}}}}\n")
    }

    #[test]
    fn an_idle_session_redrawing_old_numbers_loses_to_the_fresh_one() {
        let root = std::env::temp_dir().join(format!("aca-limits-{}", std::process::id()));
        let acct = root.join(".claude-work");
        let proj = acct.join("projects/-x");
        std::fs::create_dir_all(&proj).unwrap();
        let store = root.join("limits");
        let fresh = proj.join("a.jsonl");
        let idle = proj.join("b.jsonl");
        let t = |s: &str| parse_ts(s);
        // b 最后一次回复 12:47(那时 80 / 39),a 13:48(98 / 41)
        std::fs::write(&idle, reply("2026-09-30T04:47:40Z") + "{\"type\":\"system\",\"timestamp\":\"2026-09-30T05:50:00Z\"}\n").unwrap();
        std::fs::write(&fresh, reply("2026-09-30T05:48:00Z")).unwrap();
        record(&payload("a", &fresh, 98.0, 41.0), &store, t("2026-09-30T05:48:01Z"));
        // 13:50 b 重画:数是旧的,时间也是旧的
        record(&payload("b", &idle, 80.0, 39.0), &store, t("2026-09-30T05:50:43Z"));
        let srcs = vec![Source { agent: Agent::Claude, who: "work".into(), dir: acct.clone() }];
        let q = quotas(&store, &srcs, t("2026-09-30T06:00:00Z"));
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].who, "work");
        assert_eq!(q[0].wins.iter().map(|w| w.used).collect::<Vec<_>>(), vec![98.0, 41.0]);
        assert_eq!(q[0].updated, t("2026-09-30T05:48:00Z"));

        // 额度提前重置:b 调了一次 API,数变小了,但它是新的 → 跟上
        record(&payload("b", &idle, 2.0, 30.0), &store, t("2026-09-30T06:10:00Z"));
        let q = quotas(&store, &srcs, t("2026-09-30T06:10:00Z"));
        assert_eq!(q[0].wins.iter().map(|w| w.used).collect::<Vec<_>>(), vec![2.0, 30.0]);
        assert_eq!(q[0].updated, t("2026-09-30T06:10:00Z"));

        // a 数没变、又回复了一次:时间跟着最后一条回复走;a 重画不会把 b 的新数盖掉
        std::fs::write(&fresh, reply("2026-09-30T05:48:00Z") + &reply("2026-09-30T05:55:00Z")).unwrap();
        record(&payload("a", &fresh, 98.0, 41.0), &store, t("2026-09-30T06:20:00Z"));
        let a: Rec = serde_json::from_slice(&std::fs::read(store.join("a.json")).unwrap()).unwrap();
        assert_eq!(a.wins[0].at, t("2026-09-30T05:55:00Z"));
        let q = quotas(&store, &srcs, t("2026-09-30T06:20:00Z"));
        assert_eq!(q[0].wins.iter().map(|w| w.used).collect::<Vec<_>>(), vec![2.0, 30.0]);

        // 刚开的会话还没有回复行:就是现在
        let new = proj.join("c.jsonl");
        std::fs::write(&new, "{\"type\":\"user\"}\n").unwrap();
        record(&payload("c", &new, 3.0, 31.0), &store, t("2026-09-30T06:30:00Z"));
        let q = quotas(&store, &srcs, t("2026-09-30T06:30:00Z"));
        assert_eq!((q[0].wins[0].used, q[0].updated), (3.0, t("2026-09-30T06:30:00Z")));

        // 没有额度 / 会话 id 不像样的:不记
        record(br#"{"session_id":"d","transcript_path":"/x/projects/p/d.jsonl"}"#, &store, 1);
        record(&payload("../e", &new, 1.0, 1.0), &store, 1);
        let names: Vec<String> =
            std::fs::read_dir(&store).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn what_the_mod_hands_over_reads_like_the_status_line() {
        // Claude 2.1.295 的 session.measure 原样交来的;同一刻状态栏给的是 resets_at 1791546000 / 1792047600
        let e = br#"{"context":{"tokens":42868,"window":1000000,"percent":4},"rateLimits":[{"kind":"seven_day","percentUsed":17.5,"resetsAt":"2026-10-15T07:00:00.000Z"},{"kind":"five_hour","percentUsed":6,"resetsAt":"2026-10-09T11:40:00.000Z"}],"cost":{"usd":0.00379882},"changed":["context","cost"]}"#;
        let got = measured(e).unwrap();
        assert_eq!(got, vec![("7d".to_string(), 17.5, Some(1792047600)), ("5h".to_string(), 6.0, Some(1791546000))]);
        // 不是订阅的账号:空的,但认得出
        assert_eq!(measured(br#"{"context":{"window":200000},"rateLimits":[]}"#), Some(vec![]));
        // 形状变了(键没了、窗口里的字段改了名)、不是 JSON:认不出
        assert_eq!(measured(br#"{"context":{},"limits":[]}"#), None);
        assert_eq!(measured(br#"{"rateLimits":[{"kind":"five_hour","used":6}]}"#), None);
        assert_eq!(measured(b"Usage: aca"), None);

        // 记在它自己的文件里;交来的就是那一刻的数,没变时间也跟着走(状态栏那条路没变就不动);空的不写
        let root = std::env::temp_dir().join(format!("aca-limits-mod-{}", std::process::id()));
        let file = root.join("mod-ab12.json");
        // 头一回报算动了,数一样的再报不算
        let first = update(&file, got.clone(), "/x/.claude", None, 700, true);
        let again = update(&file, got, "/x/.claude", None, 900, true);
        assert!(!update(&root.join("mod-none.json"), vec![], "/x/.claude", None, 900, true));
        let rec: Rec = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let left = std::fs::read_dir(&root).unwrap().count();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(first && !again);
        assert_eq!(
            rec.wins.iter().map(|w| (w.label.as_str(), w.used, w.at)).collect::<Vec<_>>(),
            vec![("5h", 6.0, 900), ("7d", 17.5, 900)]
        );
        assert_eq!(left, 1);
    }

    #[test]
    fn what_claude_says_when_asked_adds_the_per_model_week() {
        // Claude 2.1.303 对 get_usage 的回答(删短了):Fable 是按模型分的周额度,状态栏里没有
        let out = br#"{"type":"system","subtype":"init"}
{"type":"control_response","response":{"subtype":"success","request_id":"aca","response":{"subscription_type":"max","rate_limits_available":true,"rate_limits":{"five_hour":{"utilization":12,"resets_at":"2026-10-19T11:30:00.608788+00:00"},"seven_day":{"utilization":40,"resets_at":"2026-10-22T06:59:59.609015+00:00"},"seven_day_opus":null,"limits":[],"model_scoped":[{"display_name":"Fable","utilization":48,"resets_at":"2026-10-22T06:59:59.609039+00:00"}]},"behaviors":null}}}
"#;
        let (five, week) = (parse_ts("2026-10-19T11:30:00Z"), parse_ts("2026-10-22T06:59:59Z"));
        let got = usage_of(out).unwrap();
        assert_eq!(got, vec![("5h".into(), 12.0, Some(five)), ("7d".into(), 40.0, Some(week)), ("Fable".into(), 48.0, Some(week))]);
        // 不是订阅的账号:rate_limits 是 null,什么都不记;别的请求的回答、不是 JSON 的不认
        let api_key =
            br#"{"type":"control_response","response":{"request_id":"aca","response":{"rate_limits_available":false,"rate_limits":null}}}"#;
        assert_eq!(usage_of(api_key), Some(vec![]));
        assert_eq!(usage_of(br#"{"type":"control_response","response":{"request_id":"other","response":{}}}"#), None);
        assert_eq!(usage_of(b"Error: not logged in\n"), None);

        // 没问过的就问;问过的要等这个账号的数又动了、隔了五分钟
        assert!(due(None, false));
        assert!(!due(Some(ASK_EVERY + 1), false) && !due(Some(ASK_EVERY - 1), true));
        assert!(due(Some(ASK_EVERY), true));

        // 问来的和状态栏记的按账号合在一起:Fable 排在 5h、7d 后面
        let root = std::env::temp_dir().join(format!("aca-limits-ask-{}", std::process::id()));
        let acct = root.join(".claude-sub");
        std::fs::create_dir_all(&acct).unwrap();
        let store = root.join("limits");
        let acct_s = acct.to_string_lossy().into_owned();
        update(&store.join("s1.json"), vec![("5h".into(), 13.0, Some(five)), ("7d".into(), 40.0, Some(week))], &acct_s, None, 2000, true);
        update(&ask_files(&store, &acct_s).0, got, &acct_s, None, 1000, true);
        let srcs = vec![Source { agent: Agent::Claude, who: "sub".into(), dir: acct.clone() }];
        let q = quotas(&store, &srcs, 3000);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(
            q[0].wins.iter().map(|w| (w.label.as_str(), w.used)).collect::<Vec<_>>(),
            vec![("5h", 13.0), ("7d", 40.0), ("Fable", 48.0)]
        );
    }

    thread_local! {
        /// 假的设置行:`[tools.claude]` 现在怎么写的、这一版的结论、写进 config.toml 的
        static TOOL: std::cell::RefCell<Tool> = std::cell::RefCell::new(Tool::default());
        static VERDICT: std::cell::RefCell<Option<Verdict>> = const { std::cell::RefCell::new(None) };
        static WROTE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(vec![]) };
    }

    #[test]
    fn the_settings_row_flips_the_mod_and_says_whether_this_claude_takes_it() {
        let row = ModRow {
            io: ModIo {
                tool: || TOOL.with(|t| t.borrow().clone()),
                verdict: || VERDICT.with(|v| v.borrow().clone()),
                set: |sec, key, v| {
                    WROTE.with(|w| w.borrow_mut().push(format!("[{sec}] {key} = {v}")));
                    // 写进去了下次读到的就是新的
                    TOOL.with(|t| t.borrow_mut().mods = v.as_bool().unwrap());
                    Ok(())
                },
            },
        };
        let shown = |row: &ModRow| row.value().iter().map(|s| s.content.to_string()).collect::<String>();
        let enter = |row: &ModRow| {
            let mut fx = Fx::default();
            row.enter(&mut fx);
            fx.flash.unwrap()
        };
        assert_eq!((row.key(), row.mark(), shown(&row).as_str()), ('m', "", "关"));
        // 开:还没起过 Claude,没验过 —— 只写开
        assert_eq!(enter(&row), "已保存,新会话生效");
        assert_eq!(WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()), ["[tools.claude] mod = true"]);
        assert_eq!(shown(&row), "开");
        // 这一版验过了,带得上:还是只写开
        let verdict = |ok: bool, why: &str| Some(Verdict { build: "2.1.295".into(), ok, why: why.into(), at: 1 });
        VERDICT.with(|v| *v.borrow_mut() = verdict(true, ""));
        assert_eq!(shown(&row), "开");
        // 这一版不带:行上标出来;关了再开,底栏说为什么
        VERDICT.with(|v| *v.borrow_mut() = verdict(false, "它交来的用量认不出了"));
        assert_eq!((shown(&row).as_str(), row.value()[1].style), ("开 · 不生效(Claude 2.1.295)", crate::theme::warn()));
        assert_eq!(enter(&row), "已保存,新会话生效");
        assert_eq!(shown(&row), "关");
        assert_eq!(enter(&row), "Claude 2.1.295 未加载 mod:它交来的用量认不出了");
        assert_eq!(
            WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()),
            ["[tools.claude] mod = false", "[tools.claude] mod = true"]
        );
        // 工具自己包了一层命令:哪一版都不带
        TOOL.with(|t| t.borrow_mut().cmd = vec!["wrap.sh".into()]);
        assert_eq!(shown(&row), "开 · 不生效(已自定义命令)");
    }

    #[test]
    fn limits_changed_goes_out_only_when_the_accounts_merged_numbers_move() {
        let root = std::env::temp_dir().join(format!("aca-limits-changed-{}", std::process::id()));
        let acct = root.join(".claude-sub");
        let proj = acct.join("projects/-x");
        std::fs::create_dir_all(&proj).unwrap();
        let store = root.join("limits");
        let (fresh, idle) = (proj.join("a.jsonl"), proj.join("b.jsonl"));
        let t = |s: &str| parse_ts(s);
        std::fs::write(&fresh, reply("2026-10-26T01:00:00Z")).unwrap();
        std::fs::write(&idle, reply("2026-10-25T20:00:00Z")).unwrap();
        let srcs = vec![Source { agent: Agent::Claude, who: "sub".into(), dir: acct.clone() }];
        let acct_s = acct.to_string_lossy().into_owned();
        let used = |v: &serde_json::Value| v["wins"].as_array().unwrap().iter().map(|w| w["used"].as_f64().unwrap()).collect::<Vec<_>>();

        // 头一回:发,带着账号名、目录、每个窗口的数和时间
        record(&payload("a", &fresh, 7.0, 48.0), &store, t("2026-10-26T01:00:05Z"));
        let first = changed(&store, &srcs, &acct_s).unwrap();
        assert_eq!(
            (first["tool"].as_str(), first["name"].as_str(), first["dir"].as_str()),
            (Some("claude"), Some("sub"), Some(acct_s.as_str()))
        );
        assert_eq!(used(&first), vec![7.0, 48.0]);
        assert_eq!((first["wins"][0]["label"].as_str(), first["wins"][0]["resets"].as_i64()), (Some("5h"), Some(5000)));
        assert_eq!(first["wins"][0]["at"].as_i64(), Some(t("2026-10-26T01:00:00Z")));
        // 同样的数又报一次:不发
        assert_eq!(changed(&store, &srcs, &acct_s), None);
        // 闲着的老会话头一回报它手里的旧数:它自己的数动了,账号合好的没动 —— 不发
        record(&payload("b", &idle, 3.0, 40.0), &store, t("2026-10-26T01:01:00Z"));
        assert_eq!(changed(&store, &srcs, &acct_s), None);
        // 真动了:发新的
        record(&payload("a", &fresh, 9.0, 48.0), &store, t("2026-10-26T01:10:00Z"));
        assert_eq!(changed(&store, &srcs, &acct_s).map(|v| used(&v)), Some(vec![9.0, 48.0]));
        // 记上回发了什么的文件不是 .json:合的时候不会被当成一个会话
        let q = quotas(&store, &srcs, t("2026-10-26T01:10:00Z"));
        let seen = std::fs::read_dir(&store).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".last")).count();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!((q.len(), seen), (1, 1));
    }

    #[test]
    fn codex_quotas_come_from_the_tail_of_its_latest_record_and_go_out_once_per_change() {
        let home = std::env::temp_dir().join(format!("aca-limits-codex-{}", std::process::id()));
        let day = home.join(".codex/sessions/2026/10/26");
        std::fs::create_dir_all(&day).unwrap();
        let rate = |used: f64, ts: &str| {
            format!(
                r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"primary":{{"used_percent":{used},"window_minutes":300,"resets_at":1793000000}},"secondary":{{"used_percent":5.0,"window_minutes":10080,"resets_at":1793500000}},"plan_type":"plus"}}}}}}"#
            ) + "\n"
        };
        let file = day.join("rollout-2026-10-26T01-00-00-a.jsonl");
        std::fs::write(&file, rate(30.0, "2026-10-26T01:00:00Z") + &rate(33.0, "2026-10-26T01:05:00Z")).unwrap();
        let h = home.to_string_lossy().into_owned();
        // 没登记的默认目录:名字是目录名去掉点;两个窗口都是最后那条的数和时间
        let got = codex_all(&[], &h);
        assert_eq!(got.len(), 1);
        let (dir, q) = &got[0];
        let v = account_json("codex", &q.who, &dir.to_string_lossy(), &codex_wins(q));
        let wins: Vec<(&str, f64, i64, i64)> = v["wins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (w["label"].as_str().unwrap(), w["used"].as_f64().unwrap(), w["resets"].as_i64().unwrap(), w["at"].as_i64().unwrap()))
            .collect();
        let at = parse_ts("2026-10-26T01:05:00Z");
        assert_eq!((v["tool"].as_str(), v["name"].as_str()), (Some("codex"), Some("codex")));
        assert_eq!(wins, vec![("5h", 33.0, 1793000000, at), ("7d", 5.0, 1793500000, at)]);
        // 一轮跑完数没变:不发;变了:发一次
        let store = home.join("limits");
        std::fs::create_dir_all(&store).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert!(differs(&store, "codex", &d, &codex_wins(q)));
        assert!(!differs(&store, "codex", &d, &codex_wins(q)));
        std::fs::write(&file, rate(33.0, "2026-10-26T01:05:00Z") + &rate(41.0, "2026-10-26T01:20:00Z")).unwrap();
        let (_, q2) = codex_all(&[], &h).pop().unwrap();
        assert!(differs(&store, "codex", &d, &codex_wins(&q2)));
        // 同一个目录,Claude 和 Codex 记的是两份
        assert!(differs(&store, "claude", &d, &codex_wins(&q2)));
        // 用 API key 登录的:没有订阅额度,不列
        std::fs::write(home.join(".codex/auth.json"), r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-x"}"#).unwrap();
        let none = codex_all(&[], &h);
        std::fs::remove_dir_all(&home).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn the_last_reply_is_found_behind_a_huge_tool_output() {
        let p = std::env::temp_dir().join(format!("aca-last-{}.jsonl", std::process::id()));
        let big = format!("{{\"type\":\"user\",\"x\":\"{}\"}}\n", "a".repeat(700 << 10));
        let nested = "{\"type\":\"user\",\"toolUseResult\":{\"type\":\"assistant\",\"timestamp\":\"2030-01-01T00:00:00Z\"}}\n";
        std::fs::write(&p, reply("2026-09-30T01:00:00Z") + &reply("2026-09-30T02:00:00Z") + &big + nested + &big).unwrap();
        let got = last_reply(&p);
        std::fs::write(&p, "{\"type\":\"user\"}\n").unwrap();
        let none = last_reply(&p);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(got, Some(parse_ts("2026-09-30T02:00:00Z")));
        assert_eq!(none, None);
    }
}
