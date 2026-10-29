//! `C`: pick one thing to copy: aca id (for commands like `aca move <id>`), session id (Claude's / Codex's own conversation id,
//! for `claude --resume`), path (the session's directory). Press the key at the start of a row to copy it, or pick with ↑↓ and ⏎.
//! One that's missing (a plain tmux session has no aca id; one that hasn't talked yet has no conversation id) is still listed,
//! saying why it's missing.
//! `B` as before: path and conversation id, two lines together.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::{t, tr};

use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

/// One row: key, name, value (empty = missing), what to show when missing
struct Row {
    key: char,
    name: &'static str,
    value: String,
    none: &'static str,
}

pub struct Menu {
    title: String,
    rows: Vec<Row>,
    sel: usize,
}

impl Menu {
    /// id: aca's session id (not registered = empty); info: what the list gives `B` (`Path: …`, `Session: …`, one per line)
    pub fn new(title: &str, id: &str, info: &str) -> Menu {
        let line = |k: &str| info.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("").trim().to_string();
        let rows = vec![
            Row { key: 'i', name: "aca id", value: id.to_string(), none: t!("A 接管后可用", "only after adopting with A") },
            Row { key: 's', name: "session id", value: line("Session: "), none: t!("暂无对话", "no conversation yet") },
            Row { key: 'p', name: "path", value: line("Path: "), none: t!("目录未知", "directory unknown") },
        ];
        let sel = rows.iter().position(|r| !r.value.is_empty()).unwrap_or(0);
        Menu { title: title.to_string(), rows, sel }
    }

    fn copy(&self, i: usize, fx: &mut Fx) -> Flow {
        let r = &self.rows[i];
        if r.value.is_empty() {
            fx.say(tr!("没有 {}:{}", "no {}: {}", r.name, r.none));
            return Flow::Stay;
        }
        fx.copy = Some(r.value.clone());
        Flow::Close
    }

    /// Move the highlight, skipping missing ones
    fn step(&mut self, d: usize) {
        let n = self.rows.len();
        for k in 1..n {
            let i = (self.sel + k * d) % n;
            if !self.rows[i].value.is_empty() {
                self.sel = i;
                return;
            }
        }
    }
}

impl Overlay for Menu {
    fn width(&self) -> u16 {
        72
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let title = kit::clip(&tr!("复制 · {}", "Copy · {}", self.title), w.saturating_sub(12)).trim_end().to_string();
        let mut out = vec![kit::modal_title(&title, theme::accent(), kit::esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let name_w = self.rows.iter().map(|r| r.name.width()).max().unwrap_or(0);
        // "i  " + name + "  "
        let room = w.saturating_sub(3 + name_w + 2);
        for (i, r) in self.rows.iter().enumerate() {
            let (text, st) = if r.value.is_empty() { (r.none, kit::dim()) } else { (r.value.as_str(), ratatui::style::Style::new()) };
            let line = Line::from(vec![
                Span::styled(r.key.to_string(), if r.value.is_empty() { kit::dim() } else { kit::key_style() }),
                Span::raw(format!("  {}{}  ", r.name, " ".repeat(name_w - r.name.width()))),
                Span::styled(kit::clip(text, room).trim_end().to_string(), st),
            ]);
            out.push(kit::picked(line, w, i == self.sel && !r.value.is_empty()));
        }
        out.push(Line::raw(""));
        out.push(kit::keys_line(&[("⏎", t!("复制", "copy"))], vec![], w));
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q' | 'C') => Flow::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.step(1);
                Flow::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.step(self.rows.len() - 1);
                Flow::Stay
            }
            KeyCode::Enter => self.copy(self.sel, fx),
            KeyCode::Char(c) => match self.rows.iter().position(|r| r.key == c) {
                Some(i) => self.copy(i, fx),
                None => Flow::Stay,
            },
            _ => Flow::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(m: &mut Menu, code: KeyCode) -> (Flow, Fx) {
        let mut fx = Fx::default();
        let f = m.on_key(KeyEvent::from(code), &mut fx);
        (f, fx)
    }

    fn text(m: &Menu, w: usize) -> Vec<String> {
        m.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn each_row_copies_its_own_value_by_key_or_enter() {
        let mut m = Menu::new("修登录", "3f2a9c1e", "Path: /w/app\nSession: 5f0c-uuid");
        let rows = text(&m, 66);
        for want in ["复制 · 修登录", "i  aca id      3f2a9c1e", "s  session id  5f0c-uuid", "p  path        /w/app", "⏎ 复制"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        let (f, fx) = press(&mut m, KeyCode::Char('s'));
        assert_eq!((f, fx.copy.as_deref()), (Flow::Close, Some("5f0c-uuid")));
        // ↓ then ⏎: path
        press(&mut m, KeyCode::Down);
        press(&mut m, KeyCode::Down);
        let (f, fx) = press(&mut m, KeyCode::Enter);
        assert_eq!((f, fx.copy.as_deref()), (Flow::Close, Some("/w/app")));
        assert_eq!(press(&mut m, KeyCode::Esc).0, Flow::Close);
    }

    #[test]
    fn missing_values_say_why_and_the_highlight_skips_them() {
        // Plain tmux session that hasn't talked yet: only the directory
        let mut m = Menu::new("裸", "", "Path: /w/x");
        assert_eq!(m.sel, 2);
        let rows = text(&m, 66);
        assert!(rows.iter().any(|r| r.contains("A 接管后可用")) && rows.iter().any(|r| r.contains("暂无对话")), "{rows:#?}");
        let (f, fx) = press(&mut m, KeyCode::Char('i'));
        assert_eq!((f, fx.copy, fx.flash.as_deref()), (Flow::Stay, None, Some("没有 aca id:A 接管后可用")));
        press(&mut m, KeyCode::Up);
        assert_eq!(m.sel, 2);
        // Phone width: no line too wide
        assert!(text(&Menu::new(&"长".repeat(40), "id", &format!("Path: /{}", "a".repeat(90))), 40).iter().all(|r| r.width() <= 40));
    }
}
