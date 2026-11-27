//! Small parts for drawing lines, shared by the list, the pages and the popups: cutting and wrapping by display width, key
//! hints, the state icons and account colors, buttons and title lines.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use aca_core::t;
use aca_records::agent::Agent;

use crate::data::State;
use crate::theme;

/// Secondary text. Not the terminal's DIM: it fades to unreadable on a light background (see theme.rs)
pub fn dim() -> Style {
    theme::minor()
}

/// Style of the "key" in key hints: bold, the description dim, so the keys catch the eye
pub fn key_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// Separator between key hints
pub(crate) const SEP: &str = " • ";

/// Room between two items of the screen's bottom bar, in place of a separator
pub(crate) const CAP_GAP: usize = 3;

/// One item of the screen's bottom bar: the key as a cap, a cell of room either side, bold, on the terminal's own ground so it
/// stands up from the bar's `back`; then a space and what it does in the minor color. `ground`: the cap's ground instead of
/// the terminal's (the main actions while multi-selecting)
pub(crate) fn key_cap(k: &str, d: &str, ground: Option<Color>) -> [Span<'static>; 2] {
    let cap = key_style().bg(ground.unwrap_or(Color::Reset));
    [Span::styled(format!(" {k} "), cap), Span::styled(format!(" {d}"), dim())]
}

/// The width of a [`key_cap`]
pub(crate) fn cap_width(k: &str, d: &str) -> usize {
    k.width() + 3 + d.width()
}

/// One cell of the top row's tabs: a page's (or a scope's) name and how many. The lit one's name is in the body color, bold,
/// underlined in the accent color (in the body color where the terminal can't color an underline), its count minor and not
/// underlined; the others are minor throughout
pub(crate) fn tab(name: &str, count: impl std::fmt::Display, lit: bool) -> Vec<Span<'static>> {
    let on = Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED).underline_color(theme::pal().accent);
    match lit {
        true => vec![Span::styled(name.to_string(), on), Span::styled(format!(" {count}"), dim())],
        false => vec![Span::styled(format!("{name} {count}"), dim())],
    }
}

/// A run of "key description" items separated by •, laid out front to back until room is used up (later ones are less common; what
/// doesn't fit is left out); returns the spans and the width taken
pub(crate) fn key_spans(items: &[(&str, &str)], room: usize) -> (Vec<Span<'static>>, usize) {
    let mut spans = vec![];
    let mut used = 0;
    for (k, d) in items {
        let add = k.width() + 1 + d.width() + if spans.is_empty() { 0 } else { SEP.width() };
        if used + add > room {
            break;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(SEP, theme::faint()));
        }
        spans.push(Span::styled(k.to_string(), key_style()));
        spans.push(Span::styled(format!(" {d}"), dim()));
        used += add;
    }
    (spans, used)
}

/// State icons: ● running, ◆ needs you (the screen is asking you), ◐ unread (finished, waiting for your next message), ○ idle,
/// ✕ error, ■ stopped.
/// Needs you and unread get two glyphs (agent-deck uses ◐ for both, told apart only by red vs yellow), so monochrome terminals
/// and color-blind users can tell them apart.
/// Colors are in theme.rs; colored ones are bold, gray ones aren't (the glyph alone already differs from the rest)
pub fn icon(s: State) -> (&'static str, Style) {
    let p = theme::pal();
    let bold = |c| Style::new().fg(c).add_modifier(Modifier::BOLD);
    match s {
        State::Ask => ("◆", bold(p.red)),
        State::Running => ("●", bold(p.green)),
        State::Unread => ("◐", bold(p.amber)),
        State::Idle => ("○", Style::new().fg(p.muted)),
        State::Error => ("✕", bold(p.red)),
        // An error that isn't running: red, not bold — it doesn't count as open; don't steal the eye from errors that are running
        State::Crashed => ("✕", Style::new().fg(p.red)),
        State::Exited | State::Stopped | State::Dead => ("■", Style::new().fg(p.muted)),
    }
}

/// Color of the account name: Claude orange, Codex blue (see theme.rs)
pub fn agent_style(a: Agent) -> Style {
    match a {
        Agent::Claude => Style::new().fg(theme::pal().claude),
        Agent::Codex => Style::new().fg(theme::pal().codex),
        Agent::Pi => Style::new().fg(theme::pal().pi),
        Agent::Opencode => Style::new().fg(theme::pal().opencode),
        Agent::Agy => Style::new().fg(theme::pal().agy),
        Agent::Other => dim(),
        Agent::None => Style::new(),
    }
}

/// The ▪ before an account name in the list and the legend: the tool's color (gray for one with no color of its own), so the
/// account name itself can stay gray
pub fn swatch(a: Agent) -> (&'static str, Style) {
    match a {
        Agent::Claude | Agent::Codex | Agent::Pi | Agent::Opencode | Agent::Agy => ("▪", agent_style(a)),
        _ => ("▪", dim()),
    }
}

/// Cut and pad to a display width (Chinese takes two cells)
pub fn clip(s: &str, w: usize) -> String {
    if s.width() <= w {
        return format!("{s}{}", " ".repeat(w - s.width()));
    }
    let (mut out, mut n) = (String::new(), 0);
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if n + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        n += cw;
    }
    format!("{out}…{}", " ".repeat(w.saturating_sub(1 + n)))
}

/// Cut from the left (a path keeps its last few levels: …/fun/app), no padding
pub fn clip_left(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let (mut tail, mut n) = (vec![], 0);
    for ch in s.chars().rev() {
        let cw = ch.width().unwrap_or(0);
        if n + cw > w.saturating_sub(1) {
            break;
        }
        tail.push(ch);
        n += cw;
    }
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

pub(crate) fn group_label(group: &str) -> &str {
    match group {
        "-" => t!("其他 tmux 会话", "Other tmux sessions"),
        crate::data::UNGROUPED => t!("未分组", "No group"),
        _ => group,
    }
}

/// The scratch group's mark: before its own name, and for its path in the names of the groups under it
pub(crate) const SCRATCH: &str = "⌂";

/// How many bytes of a group's path are the scratch group's: 0 when it is neither that group nor under it
pub(crate) fn scratch_len(group: &str, scratch: Option<&str>) -> usize {
    scratch.filter(|s| crate::groups::within(group, s)).map_or(0, str::len)
}

/// A group's name as written on screen. One under the scratch group has the scratch mark for that part of its path
/// (fun/manga → ⌂/manga); the scratch group itself keeps its name. `scratch`: see [`scratch_len`]. What you type and what
/// the command line prints stay the real path
pub(crate) fn group_shown(group: &str, scratch: usize) -> std::borrow::Cow<'_, str> {
    match group.get(scratch..) {
        Some(rest) if scratch > 0 && rest.starts_with('/') => format!("{SCRATCH}{rest}").into(),
        _ => group_label(group).into(),
    }
}

/// Wrap by display width (Chinese takes two cells); an empty line stays an empty line
pub fn wrap_width(s: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        let cur = out.last_mut().unwrap();
        if cur.width() + cw > w.max(1) {
            // English words aren't broken in the middle: when halfway through a word, carry that half to the next line (a word wider than a
            // line is still broken hard)
            let word = |x: char| x.is_ascii_alphanumeric() || matches!(x, '-' | '_' | '.' | ',' | ';' | ':' | ')' | '\'' | '/');
            let half = match cur.rfind(' ') {
                Some(i) if word(c) && i > 0 && cur[i + 1..].chars().all(word) => cur.split_off(i + 1),
                _ => String::new(),
            };
            let kept = cur.trim_end().len();
            cur.truncate(kept);
            if c == ' ' && half.is_empty() {
                out.push(String::new());
                continue;
            }
            out.push(half);
        }
        out.last_mut().unwrap().push(c);
    }
    out
}

/// One line: a left part, a right part (right-aligned), spaces in between; when it doesn't fit, drop the right part first
pub fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    let (lw, rw) = (wid(&left), wid(&right));
    let mut v = left;
    if lw + 2 + rw <= w {
        v.push(Span::raw(" ".repeat(w - lw - rw)));
        v.extend(right);
    }
    Line::from(v)
}

/// A popup's title line: the title in accent bold, a part on the right (key to close / page number)
pub fn modal_title(title: &str, st: Style, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    spread(vec![Span::styled(clip(title, w).trim_end().to_string(), st.add_modifier(Modifier::BOLD))], right, w)
}

pub fn esc_hint(what: &str) -> Vec<Span<'static>> {
    vec![Span::styled("Esc", key_style()), Span::styled(format!(" {what}"), dim())]
}

/// A button with an accent background, like " ⏎ create "
pub fn button(t: &str, bg: Color) -> Span<'static> {
    Span::styled(format!(" {t} "), Style::new().fg(theme::pal().on_accent).bg(bg).add_modifier(Modifier::BOLD))
}

/// One line of key hints (at the bottom of a popup)
pub fn keys_line(items: &[(&str, &str)], right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let (keys, _) = key_spans(items, w.saturating_sub(if right_w > 0 { right_w + 2 } else { 0 }));
    spread(keys, right, w)
}

/// `? 快捷键` ("? keys"), the key every page and popup with a `?` ends its key line with
pub fn help_key() -> (&'static str, &'static str) {
    ("?", t!("快捷键", "keys"))
}

/// A popup's line of keys, for one that has a `?`: `? 快捷键` at its right end, after anything else there; short of room it goes
/// first, before any of the keys
pub fn keys_line_help(items: &[(&str, &str)], mut right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    let (help, help_w) = key_spans(&[help_key()], usize::MAX);
    let keys_w = key_spans(items, usize::MAX).1;
    let right_w = wid(&right);
    if keys_w + 2 + right_w + if right_w > 0 { 2 } else { 0 } + help_w <= w {
        if right_w > 0 {
            right.push(Span::raw("  "));
        }
        right.extend(help);
    }
    keys_line(items, right, w)
}

/// A full-screen page's bottom bar, laid out like the list's: this page's keys on the left, the most used first (what doesn't fit
/// goes from their right end; main = the ones in the accent color), `Esc 返回 • ? 快捷键` ("Esc back • ? keys") always at the
/// right end
pub fn page_bar(items: &[(&str, &str, bool)], w: usize) -> Line<'static> {
    let (right, right_w) = key_spans(&[("Esc", t!("返回", "back")), help_key()], usize::MAX);
    let room = w.saturating_sub(right_w + 2);
    let main = Style::new().fg(theme::pal().accent);
    let (mut left, mut used) = (vec![], 0);
    for &(k, d, on) in items {
        let add = k.width() + 1 + d.width() + if left.is_empty() { 0 } else { SEP.width() };
        if used + add > room {
            break;
        }
        if !left.is_empty() {
            left.push(Span::styled(SEP, theme::faint()));
        }
        let (ks, ds) = if on { (main.add_modifier(Modifier::BOLD), main) } else { (key_style(), dim()) };
        left.push(Span::styled(k.to_string(), ks));
        left.push(Span::styled(format!(" {d}"), ds));
        used += add;
    }
    spread(left, right, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_under_the_scratch_group_is_named_by_the_scratch_mark_and_nothing_else_is() {
        let shown = |g: &str, scratch: Option<&str>| group_shown(g, scratch_len(g, scratch)).into_owned();
        assert_eq!(shown("fun/manga", Some("fun")), "⌂/manga");
        assert_eq!(shown("fun/manga/raw", Some("fun")), "⌂/manga/raw");
        // The scratch group itself keeps its name (its mark stands before it, where there's a place for one)
        assert_eq!(shown("fun", Some("fun")), "fun");
        // A scratch group that is itself under another: the whole of its path is the mark
        assert_eq!(shown("home/fun/manga", Some("home/fun")), "⌂/manga");
        // Not under it: a name that only starts the same, a group above it, any group when there's no scratch group
        assert_eq!(shown("funny/manga", Some("fun")), "funny/manga");
        assert_eq!(shown("home", Some("home/fun")), "home");
        assert_eq!(shown("fun/manga", None), "fun/manga");
        // The two that aren't paths keep their own names
        assert_eq!(shown("-", Some("fun")), group_label("-"));
        assert_eq!(shown(crate::data::UNGROUPED, Some("fun")), group_label(crate::data::UNGROUPED));
    }

    #[test]
    fn status_icons_use_the_theme_colors_not_dim() {
        let p = theme::pal();
        for (st, fg) in [
            (State::Ask, p.red),
            (State::Running, p.green),
            (State::Unread, p.amber),
            (State::Idle, p.muted),
            (State::Error, p.red),
            (State::Crashed, p.red),
            (State::Stopped, p.muted),
        ] {
            let sty = icon(st).1;
            assert_eq!(sty.fg, Some(fg), "{st:?}");
            assert!(!sty.add_modifier.contains(Modifier::DIM), "{st:?}");
        }
    }

    #[test]
    fn clip_pads_by_display_width() {
        assert_eq!(clip_left("~/workspace/fun/app", 9), "…/fun/app");
        assert_eq!(clip_left("~/a", 9), "~/a");
        assert_eq!(clip("中文标题", 10).width(), 10);
        assert_eq!(clip("一个很长很长的中文标题", 10).width(), 10);
        assert!(clip("一个很长很长的中文标题", 10).contains('…'));
    }
}
