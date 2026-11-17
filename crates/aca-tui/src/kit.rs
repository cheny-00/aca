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
        Agent::Other => dim(),
        Agent::None => Style::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
