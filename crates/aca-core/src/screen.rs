//! 读屏幕判断状态:在等你操作(附它问的那句)、在跑(附已跑秒数)。
//! 每家的屏幕长得不一样,怎么认在各自的 `agent/<名字>.rs` 里;这里只有共用的结果类型和窗格标题那个转圈的字符。
//!
//! hook 知道的事更准,但有两样 Claude 的 hook 不知道:按 Esc 打断不发任何事件;权限框点了同意之后也没有事件。
//! 这两样屏幕上看得出来,所以列表里以屏幕为准、hook 补屏幕看不出的(跑完没看、出错、对话 id)。

/// agent 干活时窗格标题开头转的那个字符(Claude、Codex 都这么做)
const SPINNER: &str = "⠁⠂⠄⡀⢀⠠⠐⠈⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

#[derive(Clone, Debug, PartialEq)]
pub enum Seen {
    Ask(String),
    Running(u64),
    /// 看得懂这家的屏幕,上面既没在问也没在跑
    Quiet,
    /// 同 Quiet,只是正文的尾巴这几秒还在变(见 [`crate::status::look`]):Claude 2.1.300 往外吐正文时屏幕上没有别的记号。
    /// hook 说在跑时才算在跑 —— 跑完那一下最后几行也会重画
    Moving,
    /// 这家的屏幕看不出在不在跑(没有读法、声明式工具没写 `busy`):状态听 hook 的
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
