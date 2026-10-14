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

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use aca_core::agent::claude_mod::Verdict;
use aca_core::config::Tool;
use aca_core::{t, tr};
use memchr::memmem::Finder;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use serde::{Deserialize, Serialize};

use crate::data::Agent;
use crate::overlay::{Fx, Setting, kit};
use crate::usage::{Quota, Source, Win, parse_ts};

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
    let mut payload = vec![];
    let _ = std::io::stdin().read_to_end(&mut payload);
    if args.first().is_some_and(|a| a == "--mod") {
        return from_mod(&payload);
    }
    if let Some(d) = dir() {
        record(&payload, &d, now());
    }
    let cmd: Vec<&String> = args.iter().skip_while(|a| *a != "--").skip(1).collect();
    let Some((prog, rest)) = cmd.split_first() else { return 0 };
    let child = std::process::Command::new(prog).args(rest).stdin(std::process::Stdio::piped()).spawn();
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
    update(&dir.join(format!("{sid}.json")), got, p.transcript_path.map(PathBuf::from).as_deref(), now, false);
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
        update(&d.join(format!("mod-{id}.json")), got, None, now(), true);
    }
    0
}

/// 计费面板额度那一段顶上的一句:开了 mod(`[tools.<claude 那一家的>] mod = true`),现在这一版 Claude 上却没带。
/// 没开、带着、还没起过会话(没验过)= 空的
pub fn mod_note(cfg: &aca_core::config::Config) -> String {
    let on = cfg.names().iter().filter_map(|n| cfg.tool(n.split(':').next()?)).any(|t| t.kind == "claude" && t.mods);
    match on.then(aca_core::agent::claude_mod::verdict).flatten() {
        Some(v) if !v.ok => tr!("aca 的 mod 没带(Claude {}):{}", "aca's mod not loaded (Claude {}): {}", v.build, v.why),
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

/// 设置页里的一行:「m  Claude mod  开 · 2.1.295 能带」。⏎ 当场切,改的是 `[tools.claude] mod`;
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

    /// 开着时这一版带不带:行上写的短的那句、要不要标成提醒、不带的原因(带着就是空的)
    fn carried(&self, tool: &Tool) -> (String, bool, String) {
        if !tool.cmd.is_empty() {
            return (
                t!("写了 cmd,不带", "cmd set, not loaded").into(),
                true,
                t!(
                    "[tools.claude] 写了 cmd:不知道里面跑的是哪一版 Claude,mod 不带",
                    "[tools.claude] sets cmd: no telling which Claude version runs inside, so no mod"
                )
                .into(),
            );
        }
        match (self.io.verdict)() {
            None => (t!("起 Claude 时验", "checked when Claude starts").into(), false, String::new()),
            Some(v) if v.ok => (tr!("{} 能带", "{} can load it", v.build), false, String::new()),
            Some(v) => (
                tr!("{} 不带", "{} can't load it", v.build),
                true,
                tr!("Claude {} 不带 mod:{}", "Claude {} won't load the mod: {}", v.build, v.why),
            ),
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
        let (short, warn, _) = self.carried(&tool);
        vec![Span::styled(t!("开", "on"), bold), Span::styled(format!(" · {short}"), if warn { crate::theme::warn() } else { kit::dim() })]
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
        let why = self.carried(&tool).2;
        fx.say(match (on, why.is_empty()) {
            (false, _) => t!(
                "新起、重启的 Claude 不带 mod 了(开着的会话不动)",
                "new and restarted Claudes go without the mod (open sessions unchanged)"
            )
            .to_string(),
            (true, true) => {
                t!("新起、重启的 Claude 带上 mod:额度不靠状态栏", "new and restarted Claudes load the mod: quotas without the status bar")
                    .to_string()
            }
            (true, false) => why,
        });
    }
}

/// 一个会话这次报的几个窗口 → 它的文件。`live` = 这些数就是现在的(mod 交来的);不是的话时间照文件头的规则从对话记录里推
fn update(path: &Path, mut got: Vec<Got>, transcript: Option<&Path>, now: i64, live: bool) {
    if got.is_empty() {
        return;
    }
    got.sort_by_key(|w| order(&w.0));
    let old: Rec = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let last = if live { None } else { transcript.and_then(last_reply) };
    let mut rec = Rec { dir: account_dir(transcript), wins: old.wins.clone() };
    for (label, used, resets) in got {
        let at = match old.wins.iter().find(|w| w.label == label) {
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

/// 面板用:每个账号每个窗口取时间最新的那份(一样新的取重置更晚、用得更多的)。
/// 账号名跟计费的一样(按账号目录对上 srcs);顺带删掉八天没动的会话文件
pub fn quotas(dir: &Path, srcs: &[Source], now: i64) -> Vec<Quota> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let names = names(srcs);
    let mut best: HashMap<String, HashMap<String, RWin>> = HashMap::new();
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
        let wins = best.entry(who_of(&names, &rec.dir)).or_default();
        for w in rec.wins {
            let newer = |o: &RWin| (w.at, w.resets, w.used) > (o.at, o.resets, o.used);
            if wins.get(&w.label).is_none_or(newer) {
                wins.insert(w.label.clone(), w);
            }
        }
    }
    let mut out: Vec<Quota> = best
        .into_iter()
        .map(|(who, m)| {
            let mut ws: Vec<RWin> = m.into_values().collect();
            ws.sort_by_key(|w| order(&w.label));
            Quota {
                agent: Agent::Claude,
                who,
                plan: String::new(),
                updated: ws.iter().map(|w| w.at).max().unwrap_or(0).min(now),
                wins: ws.into_iter().map(|w| Win { label: w.label, used: w.used, resets: w.resets }).collect(),
                note: String::new(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.who.cmp(&b.who));
    out
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
        update(&file, got.clone(), None, 700, true);
        update(&file, got, None, 900, true);
        update(&root.join("mod-none.json"), vec![], None, 900, true);
        let rec: Rec = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let left = std::fs::read_dir(&root).unwrap().count();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            rec.wins.iter().map(|w| (w.label.as_str(), w.used, w.at)).collect::<Vec<_>>(),
            vec![("5h", 6.0, 900), ("7d", 17.5, 900)]
        );
        assert_eq!(left, 1);
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
        // 开:还没起过 Claude,没验过
        assert!(enter(&row).contains("带上 mod"));
        assert_eq!(WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()), ["[tools.claude] mod = true"]);
        assert_eq!(shown(&row), "开 · 起 Claude 时验");
        // 这一版验过了
        let verdict = |ok: bool, why: &str| Some(Verdict { build: "2.1.295".into(), ok, why: why.into(), at: 1 });
        VERDICT.with(|v| *v.borrow_mut() = verdict(true, ""));
        assert_eq!(shown(&row), "开 · 2.1.295 能带");
        // 这一版不带:行上标出来;关了再开,底栏说为什么
        VERDICT.with(|v| *v.borrow_mut() = verdict(false, "它交来的用量认不出了"));
        assert_eq!((shown(&row).as_str(), row.value()[1].style), ("开 · 2.1.295 不带", crate::theme::warn()));
        assert!(enter(&row).contains("不带 mod 了"));
        assert_eq!(shown(&row), "关");
        assert_eq!(enter(&row), "Claude 2.1.295 不带 mod:它交来的用量认不出了");
        assert_eq!(
            WROTE.with(|w| w.borrow_mut().drain(..).collect::<Vec<_>>()),
            ["[tools.claude] mod = false", "[tools.claude] mod = true"]
        );
        // 工具自己包了一层命令:哪一版都不带
        TOOL.with(|t| t.borrow_mut().cmd = vec!["wrap.sh".into()]);
        assert_eq!(shown(&row), "开 · 写了 cmd,不带");
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
