//! What an agent schedules for itself in a session to "come back when it's due": Claude's CronCreate (scheduled) and
//! ScheduleWakeup (the next `/loop` run).
//! Both live only in that Claude process's memory (CronCreate's `durable` is documented as having no effect): gone once the
//! process exits, and they don't come back when the conversation is resumed.
//! So while something isn't due yet, it can't be stopped even when idle (see `session::stop_idle`).
//!
//! Recognized from the conversation record, counting only what came after this start (`Run::started`):
//! - CronCreate: input `{cron, prompt, recurring}`; the result "Scheduled one-shot task 0df328a0 (17 3 1 1 *). Next run: …" holds
//!   the job id; ones deleted by CronDelete `{id}` don't count. A recurring one (the default without recurring) is deleted by
//!   Claude itself 7 days after it's created; a one-shot runs once when due and is gone, its time worked out from the cron in the
//!   local time zone; a form that isn't recognized counts as 7 days. One whose job id isn't recognized can't be deleted and keeps
//!   counting — better not to stop it
//! - ScheduleWakeup: unless the last one is `{stop: true}`, it's woken `delaySeconds` (60–3600) later
//!
//! [`SLACK`] more is counted past the due time: it doesn't run while it's busy, and a recurring one also gets pushed back (up to
//! 30 minutes).
//! Follows what Claude Code 2.1.300 writes into the record (seen on this machine on 2026-10-15 by creating one and deleting it).
//!
//! The interface asks on every refresh (marked ◷ in the list): a record can be over 100 MB, so for each one read, how far it got
//! is kept, and next time only what was written after is read (see [`scan`]).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use serde_json::Value;

use crate::runtime::Run;

/// How long after a recurring job is created Claude deletes it itself
const RECURRING_LIFE: u64 = 7 * 86400;
/// How much longer to count past the due time
const SLACK: u64 = 3600;
/// Lines of the record with none of these strings don't need parsing (a big record is over 100 MB)
const NEEDLES: &[&str] = &["\"CronCreate\"", "\"CronDelete\"", "\"ScheduleWakeup\"", "Scheduled "];
static JOB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Scheduled\b[^.]*?\btask ([0-9A-Za-z_-]+)").unwrap());

/// One thing not due yet
#[derive(Clone, Debug, PartialEq)]
pub struct Due {
    /// The cron; empty for the next `/loop` run
    pub cron: String,
    /// Recurring (only cron ones can be)
    pub recurring: bool,
    /// What it's handed when due
    pub prompt: String,
    /// When it's next due; None if the cron isn't recognized
    pub next: Option<u64>,
    /// A scheduled message in the aca inbox (`aca msg --in`), not one in its process
    pub inbox: bool,
}

/// Scheduled messages in the inbox that aren't due yet (see [`crate::inbox::scheduled`]), listed like the ones it scheduled itself
pub fn inbox(id: &str) -> Vec<Due> {
    crate::inbox::scheduled(id)
        .into_iter()
        .map(|m| Due { cron: String::new(), recurring: false, prompt: m.text, next: Some(m.at), inbox: true })
        .collect()
}

/// What was scheduled since this start and isn't due yet, soonest first
pub fn due(r: &Run, now: u64) -> Vec<Due> {
    let mut seen = Seen::default();
    for f in records(r) {
        // One not written since before this start can't hold anything scheduled this time
        let written = f.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok());
        if written.is_some_and(|w| w.as_secs() >= r.started) {
            // The one already there at start: nothing scheduled this time lies before its length then, so read from there on
            let from = if f == Path::new(&r.started_file) { r.started_len } else { 0 };
            seen.merge(&scan(&f, from, now));
        }
    }
    seen.due(r.started, now, &|cron, at| next_fire(cron, at, &local, &at_local))
}

/// Whether anything isn't due yet
pub fn pending(r: &Run, now: u64) -> bool {
    !due(r, now).is_empty()
}

/// How far a record has been read (just past the last newline read) and what was recognized by then. If the file was replaced
/// (inode changed) or got shorter, read from the start
struct Scan {
    ino: u64,
    read: u64,
    seen: Seen,
}

static SCANS: LazyLock<Mutex<HashMap<PathBuf, Scan>>> = LazyLock::new(Default::default);

/// What's recognized in a record: the part already read is kept, only what was written after is read; the first read starts at
/// `from` (file shorter than that = replaced, from the start).
/// Two threads reading the same one at once is fine: each reads on its own, the one put back last stays
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

/// The records of this conversation and of every one switched to since this start (/clear switches the conversation; the process
/// and its scheduled jobs stay)
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

/// One CronCreate call
#[derive(Clone, Debug)]
struct Create {
    at: u64,
    cron: String,
    recurring: bool,
    prompt: String,
}

/// What's recognized in a record
#[derive(Clone, Debug, Default)]
struct Seen {
    /// CronCreate call id → the call
    creates: HashMap<String, Create>,
    /// CronCreate call id → the job id in its result
    jobs: HashMap<String, String>,
    /// Job ids deleted by CronDelete
    deleted: HashSet<String>,
    /// ScheduleWakeup call id → (when, the delay in seconds and the prompt; `stop` = None). The latest one counts
    wakes: HashMap<String, (u64, Option<(u64, String)>)>,
}

impl Seen {
    /// Merge what's recognized in several records (the conversations before /clear)
    fn merge(&mut self, o: &Seen) {
        for (call, c) in &o.creates {
            self.creates.entry(call.clone()).and_modify(|e| e.at = e.at.min(c.at)).or_insert_with(|| c.clone());
        }
        self.jobs.extend(o.jobs.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.deleted.extend(o.deleted.iter().cloned());
        for (call, w) in &o.wakes {
            self.wakes.entry(call.clone()).and_modify(|e| e.0 = e.0.min(w.0)).or_insert_with(|| w.clone());
        }
    }

    /// `fire`: when the cron is first due after that time (not recognized = None)
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
                v.push(Due { cron: c.cron.clone(), recurring: c.recurring, prompt: c.prompt.clone(), next, inbox: false });
            }
        }
        if let Some((at, Some((d, prompt)))) = self.wakes.values().max_by_key(|w| w.0) {
            let next = at + (*d).clamp(60, 3600);
            if *at >= since && now < next + SLACK {
                v.push(Due { cron: String::new(), recurring: false, prompt: prompt.clone(), next: Some(next), inbox: false });
            }
        }
        v.sort_by_key(|d| d.next.unwrap_or(u64::MAX));
        v
    }

    /// One line of the record. One without a timestamp is taken as now (better to keep too much); a call recorded on two lines
    /// takes the earlier time
    /// (since 2.1.312 each message is first recorded as an `api-request-blob` line without a timestamp, then the line with one)
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
                    let id = it["id"].as_str().unwrap_or_default().to_string();
                    let next = if input["stop"].as_bool() == Some(true) {
                        None
                    } else {
                        let prompt = input["prompt"].as_str().unwrap_or_default().to_string();
                        Some((input["delaySeconds"].as_f64().unwrap_or(0.0) as u64, prompt))
                    };
                    self.wakes.entry(id).and_modify(|e| e.0 = e.0.min(at)).or_insert((at, next));
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

/// Read from `from` to the last newline and return how many bytes were read. A last line not fully written yet (no newline) is
/// left for next time
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
        // If a line isn't UTF-8, skip only that line
        let Ok(text) = std::str::from_utf8(&line) else { continue };
        if !NEEDLES.iter().any(|n| text.contains(n)) {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            seen.line(&v, now);
        }
    }
}

/// `2026-10-15T08:00:36.843Z` → seconds. Only UTC is recognized (it's what Claude writes)
fn iso(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[10] != b'T' || !s.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?);
    u64::try_from(days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?).ok()
}

/// Gregorian date → days since 1970-01-01 (Howard Hinnant's algorithm; a day past the end of the month just counts on)
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// A moment in local time: year, month (1–12), day, weekday (0 = Sunday), hour, minute
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Day {
    pub y: i64,
    pub m: u32,
    pub d: u32,
    pub w: u32,
    pub h: u32,
    pub mi: u32,
}

/// Seconds → local time (follows TZ, DST included)
pub fn local(t: u64) -> Day {
    let tt = t as libc::time_t;
    // SAFETY: tm is plain data, all zeros is valid; localtime_r writes only the one we pass
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

/// The time: today's as "08:57", other days as "10/16 08:57"
pub fn clock(t: u64, now: u64) -> String {
    let (a, b) = (local(t), local(now));
    let hm = format!("{:02}:{:02}", a.h, a.mi);
    if (a.y, a.m, a.d) == (b.y, b.m, b.d) { hm } else { format!("{}/{} {hm}", a.m, a.d) }
}

/// Hour and minute on that local day (a day past the end of the month counts on) → seconds; mktime works out DST itself
fn at_local(y: i64, m: u32, d: i64, h: u32, mi: u32) -> u64 {
    // SAFETY: as above
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (y - 1900) as i32;
    tm.tm_mon = m as i32 - 1;
    tm.tm_mday = d as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = mi as i32;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) }.max(0) as u64
}

/// A five-field cron (minute hour day month weekday): each field can be `*`, `5`, `1-5`, `*/15`, `0-30/10`, `1,15`; weekday 7 is
/// Sunday too.
/// If neither the day nor the weekday field starts with `*`, matching either is enough; if one does, both must match (as in
/// Vixie cron)
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
                // `5/10`: every 10th from 5 on
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

/// When `cron` is first due after `after` (none within a year, or a form not recognized = None). Goes forward day by day; on a day
/// that matches, looks at the hours and minutes
fn next_fire(cron: &str, after: u64, local: &dyn Fn(u64) -> Day, at: &dyn Fn(i64, u32, i64, u32, u32) -> u64) -> Option<u64> {
    let c = parse(cron)?;
    let start = local(after);
    for k in 0..=400 {
        // Noon of that day: `at` counts a day past the end of the month on, then read back the year, month, day and weekday
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

    /// Tests take UTC as local time, whatever the machine's time zone
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
        // One-shot: Claude hard-codes the day and time in it
        assert_eq!(next("17 3 1 1 *", "2026-10-15T08:00:00Z"), Some(t("2027-01-01T03:17:00Z")));
        assert_eq!(next("30 14 15 10 *", "2026-10-15T08:00:00Z"), Some(t("2026-10-15T14:30:00Z")));
        // Every 5 minutes, minute 7 of every hour, weekday mornings (10-17 is a Saturday)
        assert_eq!(next("*/5 * * * *", "2026-10-15T08:01:30Z"), Some(t("2026-10-15T08:05:00Z")));
        assert_eq!(next("7 * * * *", "2026-10-15T08:07:00Z"), Some(t("2026-10-15T09:07:00Z")));
        assert_eq!(next("57 8 * * 1-5", "2026-10-16T09:00:00Z"), Some(t("2026-10-19T08:57:00Z")));
        // Weekday 7 is Sunday; both day and weekday restricted: matching either is enough
        assert_eq!(next("0 9 * * 7", "2026-10-15T00:00:00Z"), Some(t("2026-10-18T09:00:00Z")));
        assert_eq!(next("0 9 20 * 6", "2026-10-15T00:00:00Z"), Some(t("2026-10-17T09:00:00Z")));
        // One field starts with *: both must match (9:00 every other day; the 16th isn't odd)
        assert_eq!(next("0 9 */2 * *", "2026-10-15T10:00:00Z"), Some(t("2026-10-17T09:00:00Z")));
        assert_eq!(next("0 9 */2 * 1", "2026-10-15T10:00:00Z"), Some(t("2026-10-19T09:00:00Z"))); // an odd day and a Monday
        // A list, a step with a start, into the next year, February 29
        assert_eq!(next("0,45 10 * * *", "2026-10-15T10:10:00Z"), Some(t("2026-10-15T10:45:00Z")));
        assert_eq!(next("5/20 * * * *", "2026-10-15T10:30:00Z"), Some(t("2026-10-15T10:45:00Z")));
        assert_eq!(next("0 0 29 2 *", "2026-10-15T00:00:00Z"), None); // the 2028 one is more than a year away
        // Forms that aren't recognized
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
        // Recurring (the default without recurring): gone 7 days after it's created, plus an hour
        let daily = rec(&[&create("2026-10-15T08:00:00Z", "c1", "57 8 * * *", None)]);
        assert!(pending(&daily, "2026-10-21T08:00:00Z"));
        assert!(pending(&daily, "2026-10-22T08:59:00Z"));
        assert!(!pending(&daily, "2026-10-22T09:01:00Z"));
        // One-shot: due (14:30), plus an hour
        let once = rec(&[&create("2026-10-15T08:00:00Z", "c1", "30 14 15 10 *", Some(false))]);
        assert!(pending(&once, "2026-10-15T15:29:00Z"));
        assert!(!pending(&once, "2026-10-15T15:31:00Z"));
        // Deleted: the job id in the result matches CronDelete's
        let gone = [
            create("2026-10-15T08:00:00Z", "c1", "57 8 * * *", None),
            result("c1", "Scheduled recurring task 0df328a0 (57 8 * * *). Next run: Oct 16 8:57am."),
            tool("2026-10-15T09:00:00Z", "CronDelete", r#"{"id":"0df328a0"}"#),
        ];
        assert!(!pending(&rec(&gone.iter().map(String::as_str).collect::<Vec<_>>()), "2026-10-15T10:00:00Z"));
        // Another job deleted, or no job id recognized in the result: both still count
        let other = [gone[0].clone(), gone[1].clone(), tool("2026-10-15T09:00:00Z", "CronDelete", r#"{"id":"ffff0000"}"#)];
        assert!(pending(&rec(&other.iter().map(String::as_str).collect::<Vec<_>>()), "2026-10-15T10:00:00Z"));
        let odd = [gone[0].clone(), result("c1", "Created job #7"), gone[2].clone()];
        assert!(pending(&rec(&odd.iter().map(String::as_str).collect::<Vec<_>>()), "2026-10-15T10:00:00Z"));
        // Scheduled before this start: the process has long been replaced, doesn't count
        let before = rec(&[&create("2026-10-15T06:00:00Z", "c1", "57 8 * * *", None)]);
        assert!(!pending(&before, "2026-10-15T10:00:00Z"));
        // The line without a timestamp is taken as now; if the same call has one with a timestamp, the earlier time is taken
        let dup = rec(&[
            r#"{"message":{"content":[{"type":"tool_use","id":"c1","name":"CronCreate","input":{"cron":"30 14 15 10 *","recurring":false}}]}}"#,
            &create("2026-10-15T08:00:00Z", "c1", "30 14 15 10 *", Some(false)),
        ]);
        assert!(!pending(&dup, "2026-10-15T15:31:00Z"));
        // Form not recognized: counts as 7 days
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
                // The next /loop run: 9:20; the recurring one daily at 8:57: today's has passed, so tomorrow; the form that isn't
                // recognized has no next run and goes last
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
        // The last line isn't fully written yet (no newline): not counted this time, nor recorded as read
        std::fs::write(&f, format!("{{\"type\":\"user\"}}\n{}", &line[..40])).unwrap();
        assert!(scan(&f, 0, 0).creates.is_empty());
        std::fs::write(&f, format!("{{\"type\":\"user\"}}\n{line}\n")).unwrap();
        assert!(scan(&f, 0, 0).creates.contains_key("c1"));
        // Another line written after it: both are read; the part already read isn't read again (changing it goes unnoticed)
        let mut text = std::fs::read_to_string(&f).unwrap().replace("57 8", "58 8");
        text.push_str(&tool("2026-10-15T09:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":600}"#));
        text.push('\n');
        std::fs::OpenOptions::new().write(true).open(&f).unwrap().write_all(text.as_bytes()).unwrap();
        let s = scan(&f, 0, 0);
        assert_eq!((s.creates["c1"].cron.as_str(), s.wakes.values().map(|w| w.0).max()), ("57 8 * * *", Some(t("2026-10-15T09:00:00Z"))));
        // Replaced (got shorter, inode changed): read from the start
        std::fs::remove_file(&f).unwrap();
        std::fs::write(&f, "{\"type\":\"user\"}\n").unwrap();
        let s = scan(&f, 0, 0);
        assert!(s.creates.is_empty() && s.wakes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_last_wakeup_holds_it_until_due_and_a_stop_lets_it_go() {
        let since = t("2026-10-15T07:00:00Z");
        let pending = |s: &Seen, now: &str| !s.due(since, t(now), &|_, _| None).is_empty();
        let loop_ = rec(&[&tool("2026-10-15T08:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":1200,"prompt":"接着看"}"#)]);
        assert!(pending(&loop_, "2026-10-15T09:19:00Z"));
        assert!(!pending(&loop_, "2026-10-15T09:21:00Z"));
        // The last one is a stop: no longer woken
        let stopped = rec(&[
            &tool("2026-10-15T08:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":1200}"#),
            &tool("2026-10-15T08:10:00Z", "ScheduleWakeup", r#"{"stop":true}"#),
        ]);
        assert!(!pending(&stopped, "2026-10-15T08:11:00Z"));
        // The delay is clamped to 60–3600 seconds, as Claude does
        let long = rec(&[&tool("2026-10-15T08:00:00Z", "ScheduleWakeup", r#"{"delaySeconds":86400}"#)]);
        assert!(!pending(&long, "2026-10-15T10:01:00Z"));
        // The same call first recorded on a line without a timestamp (api-request-blob, 12:00 when read): the timestamped line
        // counts, due at 8:02
        let blob = |id: &str, input: &str| {
            format!(
                r#"{{"type":"api-request-blob","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"ScheduleWakeup","input":{input}}}]}}}}"#
            )
        };
        let real = |at: &str, id: &str, input: &str| tool(at, "ScheduleWakeup", input).replace(&format!("t-{at}"), id);
        let twice = rec(&[&blob("w1", r#"{"delaySeconds":120}"#), &real("2026-10-15T08:00:00Z", "w1", r#"{"delaySeconds":120}"#)]);
        let due = twice.due(since, t("2026-10-15T08:01:00Z"), &|_, _| None);
        assert_eq!(due.iter().map(|d| d.next).collect::<Vec<_>>(), [Some(t("2026-10-15T08:02:00Z"))]);
        // Same for the later stop: its line without a timestamp doesn't bring the earlier one back
        let then_stop = rec(&[
            &blob("w1", r#"{"delaySeconds":1200}"#),
            &real("2026-10-15T08:00:00Z", "w1", r#"{"delaySeconds":1200}"#),
            &blob("w2", r#"{"stop":true}"#),
            &real("2026-10-15T08:10:00Z", "w2", r#"{"stop":true}"#),
        ]);
        assert!(!pending(&then_stop, "2026-10-15T08:11:00Z"));
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
        // Scheduled in the conversation before /clear (still this process), with a line that isn't UTF-8 in between
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
        // A record not written since before this start isn't looked at
        assert!(!pending(&Run { started: now + 10, ..r.clone() }, now));
        assert!(!pending(&Run::default(), now));
        // What's scheduled in the part the record already had at start doesn't count (the previous process scheduled it, gone
        // once it exited): read on from the recorded length, what's written after is recognized as usual.
        // Read offsets are kept per file (SCANS), so each case uses its own file
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
        // A recorded length longer than the file = the file was replaced, read from the start; one recorded for another file (the
        // new conversation after /clear) is ignored too
        assert!(pending(&run("c3.jsonl", "c3.jsonl", before.len() as u64 + 1), now));
        assert!(pending(&run("c4.jsonl", "c9.jsonl", before.len() as u64), now));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
