//! A desktop notification was clicked. When a kitty notification (OSC 99) carries `a=report`, a click makes kitty send
//! `ESC ] 99 ; i=<id> ; ESC \` back to the window that sent it. When the interface is in that window's foreground, this goes
//! into aca's input: crossterm doesn't know OSC and splits it into Alt+], the characters one by one, then Alt+\; if not
//! picked out it's a string of keypresses (`i` `a` `c` all do something).
//! An id of `aca-<session id>` goes into that session; anything else is just picked out. When the interface isn't in the
//! foreground (inside a session, tmux is in front) tmux's user-keys catch it, see this machine's notify-jump plugin.

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// The longest a report can be: anything longer isn't one, handled as keys
const MAX: usize = 256;

/// Pick out the terminated OSCs in this batch (the text after `ESC ]` and before `ESC \` or BEL); the rest stay keys as before.
/// kitty writes a whole report at once, so it's read in the same batch
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

/// The text after `ESC ]` up to the terminator, and the terminator's position in rest
fn body(rest: &[Event]) -> Option<(String, usize)> {
    let mut text = String::new();
    for (j, e) in rest.iter().enumerate() {
        // ST (ESC \, split into Alt+\) or BEL (Ctrl+G)
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

/// Which session was clicked: `99;i=aca-<id>;` (a click on a button is `;1` `;2`, that counts too). Closed, or asking whether
/// it's still there (with `p=`), doesn't count
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

    /// Bytes turned into events the way crossterm splits them: ESC plus a character = Alt+it, 0x01-0x1A = Ctrl+letter
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
