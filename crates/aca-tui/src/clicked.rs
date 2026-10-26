//! 点了桌面通知。kitty 的通知(OSC 99)带 `a=report` 时,点一下 kitty 往发通知的那个窗口回一串
//! `ESC ] 99 ; i=<id> ; ESC \`。界面正在那个窗口的前台时,这串进的是 aca 的输入:crossterm 不认 OSC,
//! 拆成 Alt+] 加一个个字符再 Alt+\ —— 不拣出来就是一串按键(`i` `a` `c` 都有用处)。
//! id 是 `aca-<会话 id>` 的进那个会话;别的只拣掉。界面不在前台(进了会话,前台是 tmux)时由 tmux 的
//! user-keys 接住,见本机的 notify-jump 插件。

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// 一串回报最长多少字:再长就不是了,照按键处理
const MAX: usize = 256;

/// 拣出这一批里收了尾的 OSC(`ESC ]` 后面、`ESC \` 或 BEL 前面的那段字),剩下的照旧当按键。
/// kitty 一次写完一整串,读的时候在同一批里
pub fn take(batch: &mut Vec<Event>) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < batch.len() {
        if is(&batch[i], ']', KeyModifiers::ALT)
            && let Some((text, end)) = body(&batch[i + 1..])
        {
            batch.drain(i..=i + 1 + end);
            out.push(text);
            continue;
        }
        i += 1;
    }
    out
}

/// `ESC ]` 后面到收尾的字,和收尾那个在 rest 里的位置
fn body(rest: &[Event]) -> Option<(String, usize)> {
    let mut text = String::new();
    for (j, e) in rest.iter().enumerate() {
        // ST(ESC \,拆出来是 Alt+\)或 BEL(Ctrl+G)
        if is(e, '\\', KeyModifiers::ALT) || is(e, 'g', KeyModifiers::CONTROL) {
            return Some((text, j));
        }
        match e {
            Event::Key(k)
                if k.kind == KeyEventKind::Press
                    && !k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
                    && text.len() < MAX =>
            {
                let KeyCode::Char(c) = k.code else { return None };
                text.push(c);
            }
            _ => return None,
        }
    }
    None
}

fn is(e: &Event, c: char, m: KeyModifiers) -> bool {
    matches!(e, Event::Key(k) if k.kind == KeyEventKind::Press && k.code == KeyCode::Char(c) && k.modifiers == m)
}

/// 点的是哪个会话:`99;i=aca-<id>;`(点在按钮上是 `;1` `;2`,也算)。关掉了、问还在不在(带 `p=`)的不算
pub fn session(osc: &str) -> Option<&str> {
    let (meta, _) = osc.strip_prefix("99;")?.split_once(';')?;
    let mut id = None;
    for kv in meta.split(':') {
        match kv.split_once('=')? {
            ("i", v) => id = Some(v),
            ("p", _) => return None,
            _ => {}
        }
    }
    id?.strip_prefix("aca-").filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    /// 字节照 crossterm 的拆法变成事件:ESC 加一个字符 = Alt+它,0x01–0x1A = Ctrl+字母
    fn events(bytes: &str) -> Vec<Event> {
        let mut out = Vec::new();
        let mut it = bytes.chars();
        while let Some(c) = it.next() {
            let k = match c {
                '\x1b' => KeyEvent::new(KeyCode::Char(it.next().unwrap()), KeyModifiers::ALT),
                '\x01'..='\x1a' => KeyEvent::new(KeyCode::Char((c as u8 - 1 + b'a') as char), KeyModifiers::CONTROL),
                c => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            };
            out.push(Event::Key(k));
        }
        out
    }

    #[test]
    fn a_click_report_comes_out_of_the_keys_and_names_the_session() {
        let mut b = events("j\x1b]99;i=aca-3f2a9c01;\x1b\\k");
        let got = take(&mut b);
        assert_eq!(got, ["99;i=aca-3f2a9c01;"]);
        assert_eq!(b, events("jk"));
        assert_eq!(session(&got[0]), Some("3f2a9c01"));
    }

    #[test]
    fn bel_ends_it_too_and_two_in_one_batch_both_come_out() {
        let mut b = events("\x1b]99;i=aca-1;1\x07\x1b]99;i=aca-179fbabb-1781502897;\x1b\\");
        let got = take(&mut b);
        assert!(b.is_empty(), "{b:?}");
        assert_eq!(got.iter().map(|o| session(o)).collect::<Vec<_>>(), [Some("1"), Some("179fbabb-1781502897")]);
    }

    #[test]
    fn a_lone_alt_bracket_and_an_unfinished_one_stay_keys() {
        for s in ["\x1b]", "\x1b]99;i=aca-1;", "\x1b]99\x1b[x\x1b\\"] {
            let mut b = events(s);
            assert!(take(&mut b).is_empty(), "{s:?}");
            assert_eq!(b, events(s));
        }
    }

    #[test]
    fn only_a_click_on_an_aca_notification_names_a_session() {
        for s in ["99;i=aca-x:p=close;", "99;i=aca-x:p=close;untracked", "99;i=0;", "99;i=other;", "99;i=aca-;", "11;rgb:ffff/ffff/ffff"] {
            assert_eq!(session(s), None, "{s}");
        }
    }
}
