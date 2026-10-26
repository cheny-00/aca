//! `C`:挑一样复制 —— aca id(`aca move <id>` 这类命令用的)、session id(Claude / Codex 自己的对话 id,
//! `claude --resume` 用的)、path(会话的目录)。按行首的键直接复制,或者 ↑↓ 挑了回车。
//! 没有的那样(普通 tmux 会话没有 aca id、还没说过话的没有对话 id)照样列着,写为什么没有。
//! `B` 照旧:路径和对话 id 两行一起。

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::{t, tr};

use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

/// 一行:键、名字、值(空 = 没有)、没有时写的
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
    /// id:aca 的会话 id(没登记 = 空);info:列表里给 `B` 的那份(`Path: …`、`Session: …` 一行一个)
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

    /// 挪高亮,跳过没有的
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
        // 「i  」+ 名字 +「  」
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
        // ↓ 再回车:path
        press(&mut m, KeyCode::Down);
        press(&mut m, KeyCode::Down);
        let (f, fx) = press(&mut m, KeyCode::Enter);
        assert_eq!((f, fx.copy.as_deref()), (Flow::Close, Some("/w/app")));
        assert_eq!(press(&mut m, KeyCode::Esc).0, Flow::Close);
    }

    #[test]
    fn missing_values_say_why_and_the_highlight_skips_them() {
        // 普通 tmux 会话、还没说过话:只有目录
        let mut m = Menu::new("裸", "", "Path: /w/x");
        assert_eq!(m.sel, 2);
        let rows = text(&m, 66);
        assert!(rows.iter().any(|r| r.contains("A 接管后可用")) && rows.iter().any(|r| r.contains("暂无对话")), "{rows:#?}");
        let (f, fx) = press(&mut m, KeyCode::Char('i'));
        assert_eq!((f, fx.copy, fx.flash.as_deref()), (Flow::Stay, None, Some("没有 aca id:A 接管后可用")));
        press(&mut m, KeyCode::Up);
        assert_eq!(m.sel, 2);
        // 手机宽度:一行不超宽
        assert!(text(&Menu::new(&"长".repeat(40), "id", &format!("Path: /{}", "a".repeat(90))), 40).iter().all(|r| r.width() <= 40));
    }
}
