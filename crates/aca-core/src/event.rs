//! Events: everything that happens to a session is written as one line to `events.jsonl` and handed to the plugins
//! subscribed to it.
//!
//! This is the one channel the interface and plugins share: the interface watches this file (refreshing when it changes
//! instead of waiting for the next poll), and plugins subscribe in their manifest with `[[events]] on = "turn.done"`, or
//! read it themselves with `aca events -f`.
//!
//! One JSON object per line: `{"ts": milliseconds, "event": name, "session": session id, …the event's own fields}`.

use std::io::Write;

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
    "agent.exited",
    "conversation.changed",
    "message.sent",
    "message.delivered",
    "message.throttled",
    "ask.answered",
    "limits.changed",
];

/// Each event's own fields (not counting the common `ts` `event` `session`). This is part of the plugin interface (the
/// "Events" table in docs/plugins.md follows it, and a test checks): within one interface version fields are only added,
/// never removed. An emitted event carrying a field not listed here crashes a debug build — register it here and in the
/// docs first
pub const FIELDS: &[(&str, &[&str])] = &[
    ("session.created", &["title", "tool", "account", "dir", "group", "parent", "fork", "fork_of", "adopted", "tmux"]),
    ("session.started", &["how"]),
    ("session.stopped", &["idle"]),
    ("session.removed", &["title", "released"]),
    ("session.archived", &[]),
    ("session.renamed", &["title", "tmux"]),
    ("session.titled", &["title", "tmux"]),
    ("session.moved", &["group"]),
    ("session.rehomed", &["tool", "account", "from"]),
    ("turn.started", &["prompt"]),
    ("turn.done", &["last", "bg", "secs"]),
    ("turn.failed", &["error", "kind"]),
    ("turn.interrupted", &["by"]),
    ("agent.waiting", &["ask", "kind", "options"]),
    ("agent.exited", &["reason"]),
    ("conversation.changed", &["conv", "from"]),
    ("message.sent", &["id", "from", "kind", "what", "wake", "text", "at"]),
    ("message.delivered", &["id", "how"]),
    ("message.throttled", &["from", "count", "secs"]),
    ("ask.answered", &["by", "answer", "ask", "kind"]),
    ("limits.changed", &["tool", "name", "dir", "wins"]),
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
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE) {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
    }
    // One write per line, opened for append: several processes writing at once don't interleave
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
    crate::plugin::dispatch(name, session, &line);
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

    #[test]
    fn subscriptions_match_exactly_or_by_prefix() {
        assert!(matches("turn.done", "turn.done") && !matches("turn.done", "turn.started"));
        assert!(matches("turn.*", "turn.started") && !matches("turn.*", "session.created"));
        assert!(matches("*", "agent.waiting"));
    }
}
