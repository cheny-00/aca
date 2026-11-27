//! The legend at the top of the main list's `?` sheet: the state icons in the list's colors and the accounts' colors.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::i18n::{Lang, lang};
use aca_core::t;
use aca_records::agent::Agent;

use crate::data::State;
use crate::kit::{dim, icon, swatch};
use crate::theme;

fn head(t: &str) -> Line<'static> {
    Line::styled(t.to_string(), Style::new().add_modifier(Modifier::BOLD))
}

/// The legend: state icons in the list's colors, three per row (two when narrow); then the accounts' colors, a ▪ in each tool's
/// color before its name as in the list
pub fn legend(w: usize) -> Vec<Line<'static>> {
    let items: Vec<(Span<'static>, &str)> = [
        (State::Ask, t!("等待回应", "needs you")),
        (State::Running, t!("运行中", "running")),
        (State::Unread, t!("未读", "unread")),
        (State::Idle, t!("空闲", "idle")),
        (State::Error, t!("出错", "error")),
        (State::Exited, t!("已停止", "stopped")),
    ]
    .into_iter()
    .map(|(st, what)| {
        let (ic, sty) = icon(st);
        (Span::styled(ic, sty), what)
    })
    .chain([
        (Span::styled(aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted)), t!("自动停止", "auto-stopped")),
        (Span::styled("◷", Style::new().fg(theme::pal().accent)), t!("有定时", "scheduled")),
        (Span::styled("✎", theme::warn()), t!("有便签", "has a draft")),
        (Span::styled("★", theme::warn()), t!("置顶", "pinned")),
        (Span::styled("⇄", dim()), t!("别处打开", "open elsewhere")),
    ])
    .collect();
    // Width of one "icon + label" cell: the English labels are longer
    let cell = if lang() == Lang::En { 16 } else { 13 };
    let per = (w / cell).clamp(1, 3);
    let mut out = vec![head(t!("图例", "Legend"))];
    for chunk in items.chunks(per) {
        let mut spans = vec![];
        for (i, (ic, what)) in chunk.iter().enumerate() {
            spans.push(ic.clone());
            let pad = if i + 1 < chunk.len() { cell.saturating_sub(3 + what.width()) } else { 0 };
            spans.push(Span::raw(format!("  {what}{}", " ".repeat(pad))));
        }
        out.push(Line::from(spans));
    }
    // Account colors: what doesn't fit on one line wraps to the next, aligned with the first one
    let label = t!("账号  ", "accounts  ");
    let mut row = vec![Span::styled(label, dim())];
    let mut used = label.width();
    for (i, (name, a)) in
        [("Claude", Agent::Claude), ("Codex", Agent::Codex), ("Pi", Agent::Pi), ("OpenCode", Agent::Opencode), ("Antigravity", Agent::Agy)].into_iter().enumerate()
    {
        if i > 0 && used + 2 + 1 + name.width() > w {
            out.push(Line::from(std::mem::take(&mut row)));
            row.push(Span::raw(" ".repeat(label.width())));
            used = label.width();
        } else if i > 0 {
            row.push(Span::raw("  "));
            used += 2;
        }
        let (mark, st) = swatch(a);
        row.push(Span::styled(mark, st));
        row.push(Span::raw(name));
        used += 1 + name.width();
    }
    out.push(Line::from(row));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legend_fits_a_phone_and_wraps_the_accounts() {
        for lg in [Lang::Zh, Lang::En] {
            aca_core::i18n::with(lg, || {
                for w in [40, 66] {
                    let ls = legend(w);
                    assert!(ls.iter().all(|l| l.width() <= w), "{w}: {ls:?}");
                    assert!(ls.iter().any(|l| l.spans.iter().any(|s| s.content == "Claude")));
                }
            });
        }
    }

    #[test]
    fn each_account_name_in_the_legend_has_a_swatch_in_its_tools_color_before_it() {
        let ls = legend(66);
        let spans: Vec<&Span> = ls.last().unwrap().spans.iter().collect();
        for (name, color) in [("Claude", theme::pal().claude), ("Codex", theme::pal().codex), ("Pi", theme::pal().pi)] {
            let i = spans.iter().position(|s| s.content == name).unwrap();
            assert_eq!((spans[i - 1].content.as_ref(), spans[i - 1].style.fg), ("▪", Some(color)));
            assert_eq!(spans[i].style, Style::new(), "{name}");
        }
    }
}
