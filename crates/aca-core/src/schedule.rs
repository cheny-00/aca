//! agent 在会话里给自己定的「到点再来」:Claude 的 CronCreate(定时)和 ScheduleWakeup(`/loop` 的下一次)。
//! 两样都只在那个 Claude 进程的内存里(CronCreate 的 `durable` 写明了没用),进程一退就没了,接着对话再起也不回来。
//! 所以还有没到点的,闲着也不能停(见 `session::stop_idle`)。
//!
//! 从对话记录里认,只算这次起来(`Run::started`)之后的:
//! - CronCreate:输入 `{cron, prompt, recurring}`,结果「Scheduled one-shot task 0df328a0 (17 3 1 1 *). Next run: …」里是任务号;
//!   CronDelete `{id}` 删掉的不算。循环的(不写 recurring 就是)建了 7 天后 Claude 自己删掉;一次性的到点跑一次就没了,
//!   几点按 cron 在本地时区算,认不出来的写法当它 7 天。认不出任务号的删不掉,一直算着 —— 宁可不停
//! - ScheduleWakeup:最后那一次不是 `{stop: true}` 的话,`delaySeconds`(60–3600)之后叫它
//!
//! 到点之后再多算 [`SLACK`]:它忙着的时候不跑,循环的还会往后挪(最多 30 分钟)。
//! 照 Claude Code 2.1.300 写进记录的样子(2026-10-15 在本机建了一个再删掉看的)。
//!
//! 界面每次刷新都问(列表上标 ◷):记录一份一百多 MB,所以每份读过的记着读到哪了,下回只读后面新写的(见 [`scan`])。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use serde_json::Value;

use crate::runtime::Run;

/// 循环的定时建了多久之后 Claude 自己删掉
const RECURRING_LIFE: u64 = 7 * 86400;
/// 到点之后再多算这么久
const SLACK: u64 = 3600;
/// 记录里这几样字都没有的行不用解析(大的记录一百多 MB)
const NEEDLES: &[&str] = &["\"CronCreate\"", "\"CronDelete\"", "\"ScheduleWakeup\"", "Scheduled "];
static JOB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Scheduled\b[^.]*?\btask ([0-9A-Za-z_-]+)").unwrap());

/// 还没到点的一样
#[derive(Clone, Debug, PartialEq)]
pub struct Due {
    /// cron;`/loop` 的下一次是空的
    pub cron: String,
    /// 循环的(cron 才有)
    pub recurring: bool,
    /// 到点交给它的那句话
    pub prompt: String,
    /// 下一次到点;cron 认不出来是 None
    pub next: Option<u64>,
}

/// 这次起来之后定下、还没到点的,先到点的在前
pub fn due(r: &Run, now: u64) -> Vec<Due> {
    let mut seen = Seen::default();
    for f in records(r) {
        // 这次起来之前就没再写过的,里面不会有这次定的
        let written = f.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok());
        if written.is_some_and(|w| w.as_secs() >= r.started) {
            // 起的时候就有的那份:它那时的长度之前的内容里不会有这次定的,从那往后读
            let from = if f == Path::new(&r.started_file) { r.started_len } else { 0 };
            seen.merge(&scan(&f, from, now));
        }
    }
    seen.due(r.started, now, &|cron, at| next_fire(cron, at, &local, &at_local))
}

/// 有没有还没到点的
pub fn pending(r: &Run, now: u64) -> bool {
    !due(r, now).is_empty()
}

/// 一份记录读到哪了(读到的最后一个换行之后)、那时候认出来的。文件换了一个(inode 变了)、变短了就从头读
struct Scan {
    ino: u64,
    read: u64,
    seen: Seen,
}

static SCANS: LazyLock<Mutex<HashMap<PathBuf, Scan>>> = LazyLock::new(Default::default);

/// 一份记录里认出来的:读过的部分记着,只读后面新写的;第一次读从 `from` 起(文件比它短 = 换过了,从头)。
/// 两个线程同时读同一份也没事,各读各的,后放回去的留下
fn scan(path: &Path, from: u64, now: u64) -> Seen {
    let Ok(meta) = path.metadata() else { return Seen::default() };
    let (ino, len) = (meta.ino(), meta.len());
    let kept = SCANS.lock().ok().and_then(|mut c| c.remove(path));
    let first = Scan { ino, read: if from <= len { from } else { 0 }, seen: Seen::default() };
    let mut s = kept.filter(|s| s.ino == ino && s.read <= len).unwrap_or(first);
    if s.read < len {
        s.read += read(path, s.read, now, &mut s.seen);
    }
    let out = s.seen.clone();
    if let Ok(mut c) = SCANS.lock() {
        c.insert(path.to_path_buf(), s);
    }
    out
}

/// 这段对话和这次起来之后换过的每一段(/clear 换了对话,进程和它的定时还在)的记录
fn records(r: &Run) -> Vec<PathBuf> {
    if r.transcript.is_empty() {
        return vec![];
    }
    let t = PathBuf::from(&r.transcript);
    let mut v = vec![t.clone()];
    if let Some(dir) = t.parent() {
        for c in r.convs.iter().chain([&r.conv]).filter(|c| !c.is_empty()) {
            let p = dir.join(format!("{c}.jsonl"));
            if !v.contains(&p) {
                v.push(p);
            }
        }
    }
    v
}

/// CronCreate 的一次调用
#[derive(Clone, Debug)]
struct Create {
    at: u64,
    cron: String,
    recurring: bool,
    prompt: String,
}

/// 记录里认出来的
#[derive(Clone, Debug, Default)]
struct Seen {
    /// CronCreate 那次调用的 id → 它
    creates: HashMap<String, Create>,
    /// CronCreate 那次调用的 id → 结果里的任务号
    jobs: HashMap<String, String>,
    /// CronDelete 删掉的任务号
    deleted: HashSet<String>,
    /// 最后一次 ScheduleWakeup:(什么时候, 隔几秒和那句话;`stop` = None)
    wake: Option<(u64, Option<(u64, String)>)>,
}

impl Seen {
    /// 几份记录(/clear 之前那几段)认出来的合在一起
    fn merge(&mut self, o: &Seen) {
        for (call, c) in &o.creates {
            self.creates.entry(call.clone()).and_modify(|e| e.at = e.at.min(c.at)).or_insert_with(|| c.clone());
        }
        self.jobs.extend(o.jobs.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.deleted.extend(o.deleted.iter().cloned());
        if let Some(w) = &o.wake
            && self.wake.as_ref().is_none_or(|(t, _)| w.0 >= *t)
        {
            self.wake = Some(w.clone());
        }
    }

    /// `fire`:cron 在这之后第一次到点是几时(认不出来 = None)
    fn due(&self, since: u64, now: u64, fire: &dyn Fn(&str, u64) -> Option<u64>) -> Vec<Due> {
        let mut v = vec![];
        for (call, c) in &self.creates {
            if c.at < since || self.jobs.get(call).is_some_and(|j| self.deleted.contains(j)) {
                continue;
            }
            let first = fire(&c.cron, c.at);
            let until = if c.recurring { c.at + RECURRING_LIFE } else { first.unwrap_or(c.at + RECURRING_LIFE) };
            if now < until + SLACK {
                let next = if c.recurring { fire(&c.cron, now.max(c.at)) } else { first };
                v.push(Due { cron: c.cron.clone(), recurring: c.recurring, prompt: c.prompt.clone(), next });
            }
        }
        if let Some((at, Some((d, prompt)))) = &self.wake {
            let next = at + (*d).clamp(60, 3600);
            if *at >= since && now < next + SLACK {
                v.push(Due { cron: String::new(), recurring: false, prompt: prompt.clone(), next: Some(next) });
            }
        }
        v.sort_by_key(|d| d.next.unwrap_or(u64::MAX));
        v
    }

    /// 一行记录。没带时刻的当成现在(宁可多留);同一次调用记了两行的,取早的那个时刻
    fn line(&mut self, v: &Value, now: u64) {
        let at = v["timestamp"].as_str().and_then(iso).unwrap_or(now);
        let Some(items) = v["message"]["content"].as_array() else { return };
        for it in items {
            let input = &it["input"];
            match (it["type"].as_str(), it["name"].as_str()) {
                (Some("tool_use"), Some("CronCreate")) => {
                    let id = it["id"].as_str().unwrap_or_default().to_string();
                    let cron = input["cron"].as_str().unwrap_or_default().to_string();
                    let recurring = input["recurring"].as_bool().unwrap_or(true);
                    let prompt = input["prompt"].as_str().unwrap_or_default().to_string();
                    self.creates.entry(id).and_modify(|e| e.at = e.at.min(at)).or_insert(Create { at, cron, recurring, prompt });
                }
                (Some("tool_use"), Some("CronDelete")) => {
                    if let Some(job) = input["id"].as_str() {
                        self.deleted.insert(job.to_string());
                    }
                }
                (Some("tool_use"), Some("ScheduleWakeup")) => {
                    let next = if input["stop"].as_bool() == Some(true) {
                        None
                    } else {
                        let prompt = input["prompt"].as_str().unwrap_or_default().to_string();
                        Some((input["delaySeconds"].as_f64().unwrap_or(0.0) as u64, prompt))
                    };
                    if self.wake.as_ref().is_none_or(|(t, _)| at >= *t) {
                        self.wake = Some((at, next));
                    }
                }
                (Some("tool_result"), _) => {
                    let Some(call) = it["tool_use_id"].as_str().filter(|c| self.creates.contains_key(*c)) else { continue };
                    let text = match &it["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
                        _ => String::new(),
                    };
                    if let Some(c) = JOB.captures(text.trim_start()) {
                        self.jobs.insert(call.to_string(), c[1].to_string());
                    }
                }
                _ => {}
            }
        }
    }
}

/// 从 `from` 读到最后一个换行,返回读了几个字节。还没写完的最后一行(没有换行)留到下回
fn read(path: &Path, from: u64, now: u64, seen: &mut Seen) -> u64 {
    let Ok(mut f) = std::fs::File::open(path) else { return 0 };
    if f.seek(SeekFrom::Start(from)).is_err() {
        return 0;
    }
    let (mut r, mut line, mut done) = (BufReader::new(f), Vec::new(), 0u64);
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(n) if n > 0 && line.ends_with(b"\n") => done += n as u64,
            _ => return done,
        }
        // 有一行不是 UTF-8 也只跳过那一行
        let Ok(text) = std::str::from_utf8(&line) else { continue };
        if !NEEDLES.iter().any(|n| text.contains(n)) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            seen.line(&v, now);
        }
    }
}

/// `2026-10-15T08:00:36.843Z` → 秒。只认 UTC 的(Claude 写的就是)
fn iso(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[10] != b'T' || !s.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?);
    u64::try_from(days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?).ok()
}

/// 公历日期 → 1970-01-01 起第几天(Howard Hinnant 的算法;日子超出那个月的照样往后数)
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 本地时间的一刻:年、月(1–12)、日、星期几(0 = 星期天)、时、分
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Day {
    pub y: i64,
    pub m: u32,
    pub d: u32,
    pub w: u32,
    pub h: u32,
    pub mi: u32,
}

/// 秒 → 本地时间(跟着 TZ 走,夏令时也对)
pub fn local(t: u64) -> Day {
    let tt = t as libc::time_t;
    // SAFETY:tm 是纯数据,全 0 合法;localtime_r 只写我们给的那一个
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&tt, &mut tm) };
    Day {
        y: tm.tm_year as i64 + 1900,
        m: tm.tm_mon as u32 + 1,
        d: tm.tm_mday as u32,
        w: tm.tm_wday as u32,
        h: tm.tm_hour as u32,
        mi: tm.tm_min as u32,
    }
}

/// 本地时间那一天(日子超出那个月的往后数)的几点几分 → 秒;夏令时由 mktime 自己定
fn at_local(y: i64, m: u32, d: i64, h: u32, mi: u32) -> u64 {
    // SAFETY:同上
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (y - 1900) as i32;
    tm.tm_mon = m as i32 - 1;
    tm.tm_mday = d as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = mi as i32;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) }.max(0) as u64
}

/// 五段的 cron(分 时 日 月 星期):每段能写 `*`、`5`、`1-5`、`*/15`、`0-30/10`、`1,15`,星期的 7 也是星期天。
/// 日和星期两段都不是 `*` 开头的,对上一样就算;有一段是的,两段都要对上(和 Vixie cron 一样)
#[derive(Debug)]
struct Cron {
    min: Vec<u32>,
    hour: Vec<u32>,
    dom: Vec<u32>,
    mon: Vec<u32>,
    dow: Vec<u32>,
    any_dom: bool,
    any_dow: bool,
}

fn parse(expr: &str) -> Option<Cron> {
    let f: Vec<&str> = expr.split_whitespace().collect();
    let [mi, h, dom, mon, dow] = f.as_slice() else { return None };
    let dow = field(dow, 0, 7)?.into_iter().map(|d| d % 7).collect();
    Some(Cron {
        min: field(mi, 0, 59)?,
        hour: field(h, 0, 23)?,
        dom: field(dom, 1, 31)?,
        mon: field(mon, 1, 12)?,
        dow,
        any_dom: dom.starts_with('*'),
        any_dow: f[4].starts_with('*'),
    })
}

fn field(s: &str, lo: u32, hi: u32) -> Option<Vec<u32>> {
    let mut v = vec![];
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, n)) => (r, n.parse::<u32>().ok().filter(|n| *n > 0)?),
            None => (part, 1),
        };
        let (a, b) = match range {
            "*" => (lo, hi),
            r => match r.split_once('-') {
                Some((a, b)) => (a.parse().ok()?, b.parse().ok()?),
                // `5/10`:从 5 起每 10 个
                None if part.contains('/') => (r.parse().ok()?, hi),
                None => (r.parse().ok()?, r.parse().ok()?),
            },
        };
        if a < lo || b > hi || a > b {
            return None;
        }
        v.extend((a..=b).step_by(step as usize));
    }
    Some(v)
}

/// `cron` 在 `after` 之后第一次到点是几时(一年里找不到、写法认不出来 = None)。按天往后看,那天对得上再看几点几分
fn next_fire(cron: &str, after: u64, local: &dyn Fn(u64) -> Day, at: &dyn Fn(i64, u32, i64, u32, u32) -> u64) -> Option<u64> {
    let c = parse(cron)?;
    let start = local(after);
    for k in 0..=400 {
        // 那天的中午:日子超出那个月的由 at 往后数,再读回来是哪年哪月几号、星期几
        let day = local(at(start.y, start.m, start.d as i64 + k, 12, 0));
        let dom = c.dom.contains(&day.d);
        let dow = c.dow.contains(&day.w);
        let on = if c.any_dom || c.any_dow { dom && dow } else { dom || dow };
        if !on || !c.mon.contains(&day.m) {
            continue;
        }
        for &h in &c.hour {
            for &mi in &c.min {
                let t = at(day.y, day.m, day.d as i64, h, mi);
                if t > after {
                    return Some(t);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 测试里拿 UTC 当本地时间,和机器的时区无关
    fn utc(t: u64) -> Day {
        let days = (t / 86400) as i64;
        let (z, w) = (days + 719468, ((days + 4).rem_euclid(7)) as u32);
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let (h, mi) = ((t % 86400 / 3600) as u32, (t % 3600 / 60) as u32);
        Day { y: yoe + era * 400 + i64::from(m <= 2), m, d, w, h, mi }
    }

    fn at_utc(y: i64, m: u32, d: i64, h: u32, mi: u32) -> u64 {
        (days_from_civil(y, m as i64, 1) + d - 1) as u64 * 86400 + h as u64 * 3600 + mi as u64 * 60
    }

    fn t(s: &str) -> u64 {
        iso(s).unwrap()
    }

    fn next(cron: &str, after: &str) -> Option<u64> {
        next_fire(cron, t(after), &utc, &at_utc)
    }

    #[test]
    fn claude_timestamps_and_dates_turn_into_seconds() {
        assert_eq!(t("1970-01-01T00:00:00.000Z"), 0);
        assert_eq!(t("2026-10-15T08:00:36.843Z"), 1_792_051_236);
        assert_eq!(utc(t("2026-10-15T08:00:36.843Z")), Day { y: 2026, m: 10, d: 15, w: 4, h: 8, mi: 0 });
        assert_eq!(utc(t("2024-02-29T23:59:59Z")), Day { y: 2024, m: 2, d: 29, w: 4, h: 23, mi: 59 });
        assert_eq!((iso("2026-10-15 08:00:36"), iso("2026-10-15T08:00:36+08:00"), iso("")), (None, None, None));
    }

    #[test]
    fn a_cron_fires_next_where_its_fields_say() {
        // 一次性的:Claude 让它把几号几点写死
        assert_eq!(next("17 3 1 1 *", "2026-10-15T08:00:00Z"), Some(t("2027-01-01T03:17:00Z")));
        assert_eq!(next("30 14 15 10 *", "2026-10-15T08:00:00Z"), Some(t("2026-10-15T14:30:00Z")));
        // 每 5 分钟、每小时第 7 分、工作日早上(10-17 是星期六)
        assert_eq!(next("*/5 * * * *", "2026-10-15T08:01:30Z"), Some(t("2026-10-15T08:05:00Z")));
        assert_eq!(next("7 * * * *", "2026-10-15T08:07:00Z"), Some(t("2026-10-15T09:07:00Z")));
        assert_eq!(next("57 8 * * 1-5", "2026-10-16T09:00:00Z"), Some(t("2026-10-19T08:57:00Z")));
        // 星期的 7 是星期天;日和星期都限定了:对上一样就算
        assert_eq!(next("0 9 * * 7", "2026-10-15T00:00:00Z"), Some(t("2026-10-18T09:00:00Z")));
        assert_eq!(next("0 9 20 * 6", "2026-10-15T00:00:00Z"), Some(t("2026-10-17T09:00:00Z")));
        // 有一段是 * 开头的:两段都要对上(隔天的 9 点,16 号不是单数)
        assert_eq!(next("0 9 */2 * *", "2026-10-15T10:00:00Z"), Some(t("2026-10-17T09:00:00Z")));
        assert_eq!(next("0 9 */2 * 1", "2026-10-15T10:00:00Z"), Some(t("2026-10-19T09:00:00Z"))); // 单数日、星期一
        // 列表、带起点的步长、跨年、2 月 29
        assert_eq!(next("0,45 10 * * *", "2026-10-15T10:10:00Z"), Some(t("2026-10-15T10:45:00Z")));
        assert_eq!(next("5/20 * * * *", "2026-10-15T10:30:00Z"), Some(t("2026-10-15T10:45:00Z")));
        assert_eq!(next("0 0 29 2 *", "2026-10-15T00:00:00Z"), None); // 2028 年的在一年多以后
        // 认不出来的写法
        for bad in ["", "* * * *", "61 * * * *", "* * * JAN *", "*/0 * * * *", "5-1 * * * *", "@daily"] {
            assert_eq!(next(bad, "2026-10-15T00:00:00Z"), None, "{bad}");
        }
    }

    fn rec(lines: &[&str]) -> Seen {
        let mut s = Seen::default();
        for l in lines {
            s.line(&serde_json::from_str(l).unwrap(), t("2026-10-15T12:00:00Z"));
        }
        s
    }

    fn create(at: &str, call: &str, cron: &str, recurring: Option<bool>) -> String {
        let r = recurring.map_or(String::new(), |r| format!(",\"recurring\":{r}"));
        format!(
            r#"{{"timestamp":"{at}","message":{{"content":[{{"type":"tool_use","id":"{call}","name":"CronCreate","input":{{"cron":"{cron}","prompt":"看一眼"{r}}}}}]}}}}"#
        )
    }

    fn result(call: &str, text: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-10-15T08:00:01Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"{call}","content":"{text}"}}]}}}}"#
        )
    }

    fn tool(at: &str, name: &str, input: &str) -> String {
        format!(r#"{{"timestamp":"{at}","message":{{"content":[{{"type":"tool_use","id":"t-{at}","name":"{name}","input":{input}}}]}}}}"#)
    }

    #[test]
    fn a_job_scheduled_since_it_started_holds_it_until_it_is_deleted_fires_or_expires() {
        let since = t("2026-10-15T07:00:00Z");
        let pending = |s: &Seen, now: &str| !s.due(since, t(now), &|c, at| next_fire(c, at, &utc, &at_utc)).is_empty();
        // 循环的(不写 recurring 就是):建了 7 天后没了,再多算一小时
        let daily = rec(&[&create("2026-10-15T08:00:00Z", "c1", "57 8 * * *", None)]);
        assert!(pending(&daily, "2026-10-21T08:00:00Z"));
        assert!(pending(&daily, "2026-10-22T08:59:00Z"));
        assert!(!pending(&daily, "2026-10-22T09:01:00Z"));
        // 一次性的:到点(14:30)再多算一小时
        let once = rec(&[&create("2026-10-15T08:00:00Z", "c1", "30 14 15 10 *", Some(false))]);
        assert!(pending(&once, "2026-10-15T15:29:00Z"));
        assert!(!pending(&once, "2026-10-15T15:31:00Z"));
        // 删掉了:结果里的任务号对上 CronDelete 的
        let gone = [
            create("2026-10-15T08:00:00Z", "c1", "57 8 * * *", None),
            result("c1", "Scheduled recurring task 0df328a0 (57 8 * * *). Next run: Oct 16 8:57am."),
            tool("2026-10-15T09:00:00Z", "CronDelete", r#"{"id":"0df328a0"}"#),
        ];
        assert!(!pending(&rec(&gone.iter().map(String::as_str).collect::<Vec<_>>()), "2026-10-15T10:00:00Z"));
        // 删的是别的任务、结果里认不出任务号:都还算着
        let other = [gone[0].clone(), gone[1].clone(), tool("2026-10-15T09:00:00Z", "CronDelete", r#"{"id":"ffff0000"}"#)];
        assert!(pending(&rec(&other.iter().map(String::as_str).collect::<Vec<_>>()), "2026-10-15T10:00:00Z"));
        let odd = [gone[0].clone(), result("c1", "Created job #7"), gone[2].clone()];
        assert!(pending(&rec(&odd.iter().map(String::as_str).collect::<Vec<_>>()), "2026-10-15T10:00:00Z"));
        // 这次起来之前定的:进程早换了,不算
        let before = rec(&[&create("2026-10-15T06:00:00Z", "c1", "57 8 * * *", None)]);
        assert!(!pending(&before, "2026-10-15T10:00:00Z"));
        // 没带时刻的那一行当成现在,同一次调用有带时刻的就取早的
        let dup = rec(&[
            r#"{"message":{"content":[{"type":"tool_use","id":"c1","name":"CronCreate","input":{"cron":"30 14 15 10 *","recurring":false}}]}}"#,
            &create("2026-10-15T08:00:00Z", "c1", "30 14 15 10 *", Some(false)),
        ]);
        assert!(!pending(&dup, "2026-10-15T15:31:00Z"));
        // 写法认不出来:当它 7 天
        let weird = rec(&[&create("2026-10-15T08:00:00Z", "c1", "@daily", Some(false))]);
        assert!(pending(&weird, "2026-10-21T08:00:00Z"));
    }

    #[test]
    fn what_is_due_says_its_cron_prompt_and_next_run_soonest_first() {
        let since = t("2026-10-15T07:00:00Z");
        let s = rec(&[
            &create("2026-10-15T08:00:00Z", "c1", "57 8 * * *", None),
            &create("2026-10-15T08:01:00Z", "c2", "@daily", Some(false)),
            &tool("2026-10-15T09:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":1200,"prompt":"接着看部署"}"#),
        ]);
        let due = s.due(since, t("2026-10-15T09:05:00Z"), &|c, at| next_fire(c, at, &utc, &at_utc));
        let show: Vec<_> = due.iter().map(|d| (d.cron.as_str(), d.recurring, d.prompt.as_str(), d.next)).collect();
        assert_eq!(
            show,
            [
                // /loop 的下一次:9:20;循环的每天 8:57:今天的过了,明天;认不出来的写法没有下一次,排最后
                ("", false, "接着看部署", Some(t("2026-10-15T09:20:00Z"))),
                ("57 8 * * *", true, "看一眼", Some(t("2026-10-16T08:57:00Z"))),
                ("@daily", false, "看一眼", None),
            ]
        );
    }

    #[test]
    fn a_record_is_read_once_and_then_only_what_was_added() {
        let dir = std::env::temp_dir().join(format!("aca-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c1.jsonl");
        let line = create("2026-10-15T08:00:00Z", "c1", "57 8 * * *", None);
        // 最后一行还没写完(没有换行):这回不算,也不记成读过
        std::fs::write(&f, format!("{{\"type\":\"user\"}}\n{}", &line[..40])).unwrap();
        assert!(scan(&f, 0, 0).creates.is_empty());
        std::fs::write(&f, format!("{{\"type\":\"user\"}}\n{line}\n")).unwrap();
        assert!(scan(&f, 0, 0).creates.contains_key("c1"));
        // 后面又写了一行:读到的是两样;读过的那部分不再读(改掉它也看不出来)
        let mut text = std::fs::read_to_string(&f).unwrap().replace("57 8", "58 8");
        text.push_str(&tool("2026-10-15T09:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":600}"#));
        text.push('\n');
        std::fs::OpenOptions::new().write(true).open(&f).unwrap().write_all(text.as_bytes()).unwrap();
        let s = scan(&f, 0, 0);
        assert_eq!((s.creates["c1"].cron.as_str(), s.wake.as_ref().map(|w| w.0)), ("57 8 * * *", Some(t("2026-10-15T09:00:00Z"))));
        // 换了一份(变短了、inode 变了):从头读
        std::fs::remove_file(&f).unwrap();
        std::fs::write(&f, "{\"type\":\"user\"}\n").unwrap();
        let s = scan(&f, 0, 0);
        assert!(s.creates.is_empty() && s.wake.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_last_wakeup_holds_it_until_due_and_a_stop_lets_it_go() {
        let since = t("2026-10-15T07:00:00Z");
        let pending = |s: &Seen, now: &str| !s.due(since, t(now), &|_, _| None).is_empty();
        let loop_ = rec(&[&tool("2026-10-15T08:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":1200,"prompt":"接着看"}"#)]);
        assert!(pending(&loop_, "2026-10-15T09:19:00Z"));
        assert!(!pending(&loop_, "2026-10-15T09:21:00Z"));
        // 最后一次是 stop:不再叫它
        let stopped = rec(&[
            &tool("2026-10-15T08:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":1200}"#),
            &tool("2026-10-15T08:10:00Z", "ScheduleWakeup", r#"{"stop":true}"#),
        ]);
        assert!(!pending(&stopped, "2026-10-15T08:11:00Z"));
        // 隔多久照 Claude 的夹在 60–3600 秒
        let long = rec(&[&tool("2026-10-15T08:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":86400}"#)]);
        assert!(!pending(&long, "2026-10-15T10:01:00Z"));
    }

    #[test]
    fn records_are_found_for_every_conversation_since_it_started_and_lines_are_read_one_by_one() {
        let dir = std::env::temp_dir().join(format!("aca-schedule-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = crate::paths::now();
        let stamp = |secs: u64| {
            let d = utc(secs);
            let s = secs % 86400;
            format!("{}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z", d.y, d.m, d.d, s / 3600, s / 60 % 60, s % 60)
        };
        // /clear 之前那段对话里定的(进程还是这个),中间夹着一行不是 UTF-8 的
        let old = dir.join("c0.jsonl");
        let mut bytes = b"{\"type\":\"user\"}\n\xff\xfe broken\n".to_vec();
        bytes.extend(create(&stamp(now - 60), "c1", "57 8 * * *", None).as_bytes());
        bytes.push(b'\n');
        std::fs::write(&old, bytes).unwrap();
        std::fs::write(dir.join("c1.jsonl"), "{\"type\":\"user\"}\n").unwrap();
        let r = Run {
            transcript: dir.join("c1.jsonl").to_string_lossy().into_owned(),
            conv: "c1".into(),
            convs: vec!["c0".into(), "c1".into()],
            started: now - 120,
            ..Default::default()
        };
        assert_eq!(records(&r), [dir.join("c1.jsonl"), dir.join("c0.jsonl")]);
        assert!(pending(&r, now));
        // 这次起来之前就没再写过的记录不看
        assert!(!pending(&Run { started: now + 10, ..r.clone() }, now));
        assert!(!pending(&Run::default(), now));
        // 起的时候记录已经有的那段里定的不算(上一个进程定的,它退了就没了):从记下的长度往后读,后面新写的照常认。
        // 读过的偏移按文件记着(SCANS),所以每种情况各用一份文件
        let before = format!("{}\n", create(&stamp(now - 60), "old", "57 8 * * *", None));
        let run = |name: &str, file: &str, len: u64| {
            std::fs::write(dir.join(name), &before).unwrap();
            Run {
                transcript: dir.join(name).to_string_lossy().into_owned(),
                conv: name.trim_end_matches(".jsonl").into(),
                started: now - 120,
                started_file: dir.join(file).to_string_lossy().into_owned(),
                started_len: len,
                ..Default::default()
            }
        };
        let r2 = run("c2.jsonl", "c2.jsonl", before.len() as u64);
        assert!(!pending(&r2, now));
        std::fs::write(dir.join("c2.jsonl"), format!("{before}{}\n", create(&stamp(now - 30), "new", "58 8 * * *", None))).unwrap();
        assert!(pending(&r2, now));
        // 记下的长度比文件还长 = 文件换过了,从头读;记的是别的文件(/clear 之后的新对话)的也不管它
        assert!(pending(&run("c3.jsonl", "c3.jsonl", before.len() as u64 + 1), now));
        assert!(pending(&run("c4.jsonl", "c9.jsonl", before.len() as u64), now));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
