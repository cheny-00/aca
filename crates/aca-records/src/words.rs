//! How a session is put in words, the same in the interface and on the command line: its vendor and account, its state, how
//! long, where.

use aca_core::{t, tr};

use crate::agent::Agent;

pub const ANTIGRAVITY: &str = "Antigravity";

/// "Claude·work": the vendor and who (the account, or the tool name); just one of them when the other is empty or they're
/// the same
pub fn agent_label(agent: Agent, who: &str) -> String {
    let name = match agent {
        Agent::Claude => "Claude",
        Agent::Codex => "Codex",
        Agent::Pi => "Pi",
        Agent::Opencode => "OpenCode",
        Agent::Agy => ANTIGRAVITY,
        Agent::None | Agent::Other => "",
    };
    match (name, who) {
        (n, "") => n.to_string(),
        ("", w) => w.to_string(),
        (n, "agy") if agent == Agent::Agy => n.to_string(),
        (n, w) if n.eq_ignore_ascii_case(w) => n.to_string(),
        (n, w) => format!("{n}·{w}"),
    }
}

/// Duration (long form, for the preview and hints): just now / 40 sec / 12 min / 2 hours / 3 days
pub fn dur(sec: u64) -> String {
    match sec {
        0 => t!("刚刚", "just now").into(),
        1..=59 => tr!("{sec} 秒", "{sec}s"),
        60..=3599 => tr!("{} 分钟", "{}m", sec / 60),
        3600..=86399 => tr!("{} 小时", "{}h", sec / 3600),
        _ => tr!("{} 天", "{}d", sec / 86400),
    }
}

pub use aca_core::status::state_name;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn home() -> String {
    env("HOME").unwrap_or_default()
}

/// /Users/me/proj → ~/proj
pub fn tilde(dir: &str) -> String {
    let h = home();
    match dir.strip_prefix(&h) {
        Some(rest) if !h.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => dir.to_string(),
    }
}
