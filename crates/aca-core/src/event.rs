//! 事件:会话上发生的每件事记一行到 `events.jsonl`,同时交给订了它的插件。
//!
//! 这是界面和插件共用的一条线:界面盯着这个文件(变了就刷新,不用干等下一次轮询),
//! 插件在清单里写 `[[events]] on = "turn.done"` 订,或者自己 `aca events -f` 一直读。
//!
//! 一行一个 JSON:`{"ts": 毫秒, "event": 名字, "session": 会话 id, …这件事自己的字段}`。

use std::io::Write;

use serde_json::{Value, json};

use crate::paths;

/// 会发出来的事件(插件清单里写了别的名字,链接时提醒一声)
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
];

/// 每个事件自己带的字段(公共的 `ts` `event` `session` 不算)。这是插件接口的一部分(docs/plugins.md「事件」那张表照它写,
/// 测试里对):同一版接口里只加不删。发出去的事件带了这里没写的字段,调试版直接崩 —— 先来这里登记、写进文档
pub const FIELDS: &[(&str, &[&str])] = &[
    ("session.created", &["title", "tool", "account", "dir", "group", "parent", "fork", "fork_of", "adopted", "tmux"]),
    ("session.started", &["how"]),
    ("session.stopped", &[]),
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
    ("message.sent", &["id", "from", "kind", "what", "wake", "text"]),
    ("message.delivered", &["id", "how"]),
    ("message.throttled", &["from", "count", "secs"]),
    ("ask.answered", &["by", "answer", "ask", "kind"]),
];

/// 这个事件自己的字段(没有这个事件 = 空)
pub fn fields(name: &str) -> &'static [&'static str] {
    FIELDS.iter().find(|(n, _)| *n == name).map_or(&[], |(_, f)| f)
}

/// 文件到这么大就换一个新的(旧的留一份 .1)
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
    // 一行一次 write、追加打开:几个进程同时写也不会串行
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
    crate::plugin::dispatch(name, session, &line);
}

/// 清单里的 `on` 对不对得上这个事件:原样、`turn.*` 这样的前缀、或者 `*`
pub fn matches(on: &str, name: &str) -> bool {
    match on.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => on == name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个事件都登记了字段,docs/plugins.md(英文)和 plugins.zh-CN.md 的事件表里每一行把它的字段都写了
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
