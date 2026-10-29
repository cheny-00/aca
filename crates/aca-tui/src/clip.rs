//! Copy to the clipboard.
//!
//! - Connected over ssh (phone): hand it to the terminal with OSC 52; Moshi puts it into the phone's clipboard
//! - Running in tmux (desktop): `tmux load-buffer -w`; tmux itself passes it on to the outer terminal with OSC 52
//! - Running directly on this machine: pbcopy

use std::io::Write;
use std::process::{Command, Stdio};

use aca_core::tr;

/// Some terminals drop an OSC 52 that's too long; the last reply is usually far smaller than this
const MAX: usize = 64 * 1024;

/// Returns "how it was copied", for the bottom bar hint
pub fn copy(text: &str) -> Result<&'static str, String> {
    let text = cut(text, MAX);
    let remote = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    if std::env::var_os("TMUX").is_some() {
        pipe(Command::new("tmux").args(["load-buffer", "-w", "-"]), text)?;
        if !remote {
            let _ = pipe(&mut Command::new("pbcopy"), text);
        }
        return Ok("tmux");
    }
    if remote {
        let mut out = std::io::stdout().lock();
        write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes())).and_then(|_| out.flush()).map_err(|e| e.to_string())?;
        return Ok("OSC 52");
    }
    pipe(&mut Command::new("pbcopy"), text)?;
    Ok("pbcopy")
}

/// Truncate by bytes, but never in the middle of a character
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn pipe(cmd: &mut Command, text: &str) -> Result<(), String> {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    child.stdin.take().unwrap().write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    let st = child.wait().map_err(|e| e.to_string())?;
    st.success().then_some(()).ok_or_else(|| tr!("退出码 {st}", "{st}"))
}

pub(crate) fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("中文".as_bytes()), "5Lit5paH");
    }

    #[test]
    fn cut_keeps_whole_chars() {
        assert_eq!(cut("中文", 4), "中");
        assert_eq!(cut("abc", 10), "abc");
    }
}
