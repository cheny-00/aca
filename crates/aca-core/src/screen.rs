//! Reading the screen to tell the state: waiting for you (with the question it asks) or running (with seconds run so far).
//! Each tool's screen looks different, and how to read it lives in its own `agent/<name>.rs`; this file only has the shared
//! result type and the spinner character in the pane title.
//!
//! Hooks know things more precisely, but there are two things Claude's hooks don't know: pressing Esc to interrupt sends
//! no event, and neither does approving a permission prompt. Both show on the screen, so the list goes by the screen, and
//! hooks fill in what the screen can't show (unread, error, conversation id).

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spinner_at_the_start_of_the_pane_title_means_working() {
        assert!(spinning("⠋ 修登录") && spinning("⠸ | api") && !spinning("✳ 修登录") && !spinning(""));
    }
}
