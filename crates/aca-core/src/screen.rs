//! Reading the screen to tell the state: waiting for you (with the question it asks) or running (with seconds run so far).
//! Each tool's screen looks different, and how to read it lives in its own `agent/<name>.rs`; this file only has the shared
//! result type, the spinner character in the pane title, and the numbered-option prompts any tool can show ([`gate`]).
//!
//! Hooks know things more precisely, but there are two things Claude's hooks don't know: pressing Esc to interrupt sends
//! no event, and neither does approving a permission prompt. Both show on the screen, so the list goes by the screen, and
//! hooks fill in what the screen can't show (unread, error, conversation id).

use std::sync::LazyLock;

use regex::Regex;

/// The character that spins at the start of the pane title while an agent works (Claude and Codex both do this)
const SPINNER: &str = "⠁⠂⠄⡀⢀⠠⠐⠈⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

#[derive(Clone, Debug, PartialEq)]
pub enum Seen {
    Ask(String),
    Running(u64),
    /// We can read this tool's screen, and it is neither asking nor running
    Quiet,
    /// Like Quiet, except the tail of the text has been changing these last seconds (see [`crate::status::look`]): while
    /// Claude 2.1.300 streams text there's no other mark on the screen. It only counts as running when the hook says so —
    /// the last few lines are also redrawn right when a turn ends
    Moving,
    /// We can't tell from this tool's screen whether it's running (no reader, or a declared tool without `busy`): the state
    /// follows the hook
    Unsure,
}

pub fn spinning(pane_title: &str) -> bool {
    pane_title.chars().next().is_some_and(|c| SPINNER.contains(c))
}

/// A prompt recognized only from the screen (the gates at startup)
#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    /// The lines above the options ("Folder access · /p/api · Trust this folder? …")
    pub title: String,
    pub opts: Vec<String>,
    /// Which one is highlighted now (from 0)
    pub at: usize,
}

/// "› 1. Trust and continue", "  2. Quit": there may be a highlight mark in front
static OPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*([›❯>▶]\s*)?(\d)\.\s+(\S.*?)\s*$").unwrap());

/// The run of consecutive options at the very bottom of the screen, numbered in order from 1 (at least two), and the few
/// lines above it. None if it can't be recognized
pub fn gate(lines: &[String]) -> Option<Gate> {
    let rows: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let tail = &rows[rows.len().saturating_sub(16)..];
    let end = tail.iter().rposition(|l| OPT.is_match(l))? + 1;
    let mut start = end - 1;
    while start > 0 && OPT.is_match(tail[start - 1]) {
        start -= 1;
    }
    let (mut opts, mut at) = (vec![], 0);
    for (i, l) in tail[start..end].iter().enumerate() {
        let c = OPT.captures(l)?;
        if c[2].parse::<usize>().ok()? != i + 1 {
            return None;
        }
        if c.get(1).is_some() {
            at = i;
        }
        opts.push(c[3].to_string());
    }
    if opts.len() < 2 {
        return None;
    }
    let title: Vec<&str> = tail[start.saturating_sub(3)..start].iter().map(|l| l.trim()).collect();
    Some(Gate { title: title.join(" · "), opts, at })
}

/// Move to the nth (from 1), then press Enter
pub(crate) fn gate_keys(g: &Gate, n: usize) -> Vec<&'static str> {
    let to = n - 1;
    let mut keys = if to > g.at { vec!["Down"; to - g.at] } else { vec!["Up"; g.at - to] };
    keys.push("Enter");
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spinner_at_the_start_of_the_pane_title_means_working() {
        assert!(spinning("⠋ 修登录") && spinning("⠸ | api") && !spinning("✳ 修登录") && !spinning(""));
    }
}
