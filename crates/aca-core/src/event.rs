//! Events: everything that happens to a session is written as one line to `events.jsonl` and handed to the plugins
//! subscribed to it.
//!
//! This is the one channel the interface and plugins share: the interface watches this file (refreshing when it changes
//! instead of waiting for the next poll), and plugins subscribe in their manifest with `[[events]] on = "turn.done"`, or
//! read it themselves with `aca events -f`.
//!
//! One JSON object per line: `{"ts": milliseconds, "event": name, "session": session id, …the event's own fields}`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::paths;

/// Events that get emitted (a plugin manifest naming anything else gets a warning when it's linked)
pub const KNOWN: &[&str] = &[
    "session.created",
    "session.started",
    "session.stopped",
    "session.removed",
    "session.archived",
    "session.renamed",
    "session.titled",
    "session.moved",
    "session.rehomed",
    "turn.started",
    "turn.done",
    "turn.failed",
    "turn.interrupted",
    "agent.waiting",
    "agent.status",
    "agent.exited",
    "conversation.changed",
    "message.sent",
    "message.delivered",
    "message.throttled",
    "ask.answered",
    "limits.changed",
    "task.added",
    "task.given",
    "task.done",
    "task.changed",
    "task.removed",
];

/// Each event's own fields (not counting the common `ts` `event` `session`). This is part of the plugin interface (the
/// "Events" table in docs/plugins.md follows it, and a test checks): within one interface version fields are only added,
/// never removed. An emitted event carrying a field not listed here crashes a debug build — register it here and in the
/// docs first
pub const FIELDS: &[(&str, &[&str])] = &[
    ("session.created", &["title", "tool", "account", "dir", "group", "parent", "by", "fork", "fork_of", "adopted", "tmux"]),
    ("session.started", &["how"]),
    ("session.stopped", &["idle"]),
    ("session.removed", &["title", "released"]),
    ("session.archived", &[]),
    ("session.renamed", &["title", "tmux"]),
    ("session.titled", &["title", "tmux"]),
    ("session.moved", &["group", "from", "by"]),
    ("session.rehomed", &["tool", "account", "from"]),
    ("turn.started", &["prompt"]),
    ("turn.done", &["last", "bg", "secs", "routers", "via"]),
    ("turn.failed", &["error", "kind", "routers", "via"]),
    ("turn.interrupted", &["by"]),
    ("agent.waiting", &["ask", "kind", "options", "routers", "via"]),
    ("agent.status", &["state", "kind", "msg"]),
    ("agent.exited", &["reason"]),
    ("conversation.changed", &["conv", "from"]),
    ("message.sent", &["id", "from", "kind", "what", "wake", "text", "at", "via"]),
    ("message.delivered", &["id", "how"]),
    ("message.throttled", &["from", "count", "secs"]),
    ("ask.answered", &["by", "answer", "ask", "kind"]),
    ("limits.changed", &["tool", "name", "dir", "wins"]),
    ("task.added", &["n", "group", "title", "by"]),
    ("task.given", &["n", "group", "title", "by", "say"]),
    ("task.done", &["n", "group", "title", "by", "result", "routers"]),
    ("task.changed", &["n", "group", "title", "by"]),
    ("task.removed", &["n", "group", "title", "by"]),
];

/// This event's own fields (no such event = empty)
pub fn fields(name: &str) -> &'static [&'static str] {
    FIELDS.iter().find(|(n, _)| *n == name).map_or(&[], |(_, f)| f)
}

/// When the file reaches this size, start a new one (the old one is kept as .1)
const ROTATE: u64 = 4 * 1024 * 1024;

pub fn line(name: &str, session: &str, data: Value) -> String {
    if cfg!(debug_assertions)
        && let Value::Object(d) = &data
        && let Some(k) = d.keys().find(|k| !fields(name).contains(&k.as_str()))
    {
        panic!("事件 {name} 带了没登记的字段 {k}:写进 event::FIELDS 和 docs/plugins.md");
    }
    let mut v = json!({ "ts": paths::now_ms(), "event": name, "session": session });
    if let (Some(o), Value::Object(d)) = (v.as_object_mut(), data) {
        o.extend(d);
    }
    v.to_string()
}

pub fn emit(name: &str, session: &str, data: Value) {
    let line = line(name, session, data);
    let path = paths::events_log();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    append(&path, &line);
    crate::plugin::dispatch(name, session, &line);
}

/// How long [`emit`] waits for `events.lock` before going on without it
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// Rotating and appending take turns on `events.lock` next to the file: appends share it, a rotation holds it alone and
/// looks at the size again once it has it. So of two processes that both saw the file over the limit only the first
/// renames it (the second finds the new, small one and leaves it, instead of renaming it over the old log), and no line is
/// written into a file after it was renamed, when a reader following the live file (`aca events -f`) may be done with it.
///
/// Waiting never blocks: at most [`LOCK_WAIT`], trying again every 2 ms. Either side holds the lock only for one rename or
/// one write, so running out means a holder is stuck (stopped with ^Z, a disk that hangs). A rotation that runs out is
/// skipped (the next event looks again); an append that runs out writes anyway, as before there was a lock: losing the
/// event would be worse than the rare reader that misses it
fn append(path: &Path, line: &str) {
    let over = || std::fs::metadata(path).is_ok_and(|m| m.len() > ROTATE);
    if over()
        && let Ok(_alone) = lock(path, false)
        && over()
    {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    let _shared = lock(path, true);
    // One write per line, opened for append: several processes writing at once don't interleave
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// Takes `events.lock`, shared or alone, held until the file returned is dropped. Ok(None) = the lock file can't be opened
/// (a directory that can't be written): go on without it, as before there was one. Err = someone held it for all of
/// [`LOCK_WAIT`]
fn lock(path: &Path, shared: bool) -> Result<Option<File>, ()> {
    let Ok(f) = OpenOptions::new().create(true).append(true).open(path.with_extension("lock")) else { return Ok(None) };
    let t = Instant::now();
    loop {
        match if shared { f.try_lock_shared() } else { f.try_lock() } {
            Ok(()) => return Ok(Some(f)),
            Err(TryLockError::WouldBlock) if t.elapsed() < LOCK_WAIT => std::thread::sleep(Duration::from_millis(2)),
            Err(TryLockError::WouldBlock) => return Err(()),
            // Locking isn't supported where the file is: as if there were no lock file
            Err(TryLockError::Error(_)) => return Ok(None),
        }
    }
}

/// Whether a manifest's `on` matches this event: exactly, a prefix like `turn.*`, or `*`
pub fn matches(on: &str, name: &str) -> bool {
    match on.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => on == name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every event registers its fields, and each row of the event tables in docs/plugins.md (English) and plugins.zh-CN.md
    /// lists all of them
    #[test]
    fn every_event_declares_its_fields_and_the_docs_list_them() {
        let names: Vec<&str> = FIELDS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, KNOWN);
        for (file, docs) in [
            ("plugins.md", include_str!("../../../docs/plugins.md")),
            ("plugins.zh-CN.md", include_str!("../../../docs/plugins.zh-CN.md")),
        ] {
            for (name, fields) in FIELDS {
                let row = docs
                    .lines()
                    .find(|l| l.starts_with('|') && l.contains(&format!("`{name}`")))
                    .unwrap_or_else(|| panic!("{file} 的事件表里没有 {name}"));
                for f in *fields {
                    assert!(row.contains(&format!("`{f}`")), "{file} 里 {name} 那一行没写 {f}:{row}");
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "没登记的字段")]
    fn a_field_nobody_declared_is_caught_in_debug_builds() {
        line("turn.done", "ab12", json!({ "surprise": 1 }));
    }

    #[test]
    fn an_event_line_carries_its_own_fields_next_to_the_common_ones() {
        let v: Value = serde_json::from_str(&line("turn.done", "ab12", json!({ "last": "好了", "bg": 0 }))).unwrap();
        assert_eq!((v["event"].as_str(), v["session"].as_str(), v["last"].as_str()), (Some("turn.done"), Some("ab12"), Some("好了")));
        assert!(v["ts"].as_u64().unwrap() > 1_700_000_000_000);
    }

    /// Several processes find the log just over the limit at the same moment: it's renamed once — the 4 MB it held are still
    /// in `.1`, not replaced by a new file renamed over them — and every event they wrote is in one of the two files. Each
    /// round the processes spin on a go file, so they all look at the size within microseconds of each other
    #[test]
    fn processes_rotating_at_once_rename_the_log_once_and_lose_no_event() {
        use std::path::PathBuf;
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};
        const NAME: &str = "event::tests::processes_rotating_at_once_rename_the_log_once_and_lose_no_event";
        const PROCS: usize = 8;
        const ROUNDS: usize = 12;
        let wait_for = |what: &dyn Fn() -> bool| {
            let t = Instant::now();
            while !what() {
                if t.elapsed() > Duration::from_secs(30) {
                    return false;
                }
                std::hint::spin_loop();
            }
            true
        };
        if let Some(sync) = std::env::var_os("ACA_TEST_EMITTER") {
            // One of the processes: each round, wait for the go, emit one event, say it's written
            let (sync, me) = (PathBuf::from(sync), std::env::var("ACA_TEST_EMITTER_N").unwrap());
            for round in 0..ROUNDS {
                if !wait_for(&|| sync.join(format!("go-{round}")).exists()) {
                    return;
                }
                emit("agent.status", "ab12", json!({ "msg": format!("p{me}-r{round}") }));
                std::fs::write(sync.join(format!("done-{me}-{round}")), "").unwrap();
            }
            return;
        }
        struct Reap(Vec<Child>);
        impl Drop for Reap {
            fn drop(&mut self) {
                for c in &mut self.0 {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
        let home = std::env::temp_dir().join(format!("aca-unit-rotate-{}", paths::short_id()));
        let (sync, state) = (home.join("sync"), home.join("state"));
        std::fs::create_dir_all(&sync).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let (live, rotated) = (state.join("events.jsonl"), state.join("events.jsonl.1"));
        let mut procs = Reap(vec![]);
        for n in 0..PROCS {
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME])
                .env("ACA_TEST_EMITTER", &sync)
                .env("ACA_TEST_EMITTER_N", n.to_string())
                .env("HOME", &home)
                .env("ACA_HOME", &home)
                .env_remove("ACA_ROUTER_SHIFT")
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            procs.0.push(child);
        }
        for round in 0..ROUNDS {
            // Just over the limit, written as the old log: its own lines, this round's
            let mut old = String::new();
            let mut n = 0;
            while old.len() as u64 <= ROTATE {
                old += &format!("{{\"event\":\"old\",\"round\":{round},\"n\":{n},\"pad\":\"{}\"}}\n", "x".repeat(200));
                n += 1;
            }
            std::fs::write(&live, &old).unwrap();
            std::fs::write(sync.join(format!("go-{round}")), "").unwrap();
            let all_done = || (0..PROCS).all(|p| sync.join(format!("done-{p}-{round}")).exists());
            assert!(wait_for(&all_done), "第 {round} 轮有进程没写完");
            let (now, before) = (std::fs::read_to_string(&live).unwrap_or_default(), std::fs::read_to_string(&rotated).unwrap());
            assert!(before == old, "第 {round} 轮 .1 不是原来那 {} 字节的日志了(现在 {} 字节)", old.len(), before.len());
            for p in 0..PROCS {
                let mark = format!("\"msg\":\"p{p}-r{round}\"");
                assert_eq!(now.matches(&mark).count() + before.matches(&mark).count(), 1, "第 {round} 轮 p{p} 的事件");
            }
        }
        for c in procs.0.drain(..) {
            let o = c.wait_with_output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
        }
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn subscriptions_match_exactly_or_by_prefix() {
        assert!(matches("turn.done", "turn.done") && !matches("turn.done", "turn.started"));
        assert!(matches("turn.*", "turn.started") && !matches("turn.*", "session.created"));
        assert!(matches("*", "agent.waiting"));
    }
}
