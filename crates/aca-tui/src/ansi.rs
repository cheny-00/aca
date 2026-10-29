//! Colors and links in the preview / full-screen reading.
//!
//! `tmux capture-pane -e` gives text with escape sequences: SGR (colors, bold…) and OSC 8 hyperlinks are all in there,
//! and **state carries across lines** (a color left on by one line isn't sent again at the start of the next), so the whole
//! block is parsed in order.
//! Parsed into "runs of styled text", then wrapped to the screen width and handed to ratatui to draw.
//!
//! Links: OSC 8 sent by the program in the session is kept as is; where none was sent, URLs recognized in the text become
//! links too. When drawing, every cell is wrapped with the full URL (see `paint_links`), so when a URL wraps over several
//! lines, clicking any piece gives the whole URL.

use std::sync::LazyLock;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub text: String,
    pub style: Style,
    pub link: Option<String>,
}

/// One line = a number of segments
pub type Styled = Vec<Seg>;

/// Where parsing has got to: carried across lines
#[derive(Default)]
struct State {
    style: Style,
    link: Option<String>,
}

/// Parse the whole block in order (state carries across lines), recognizing URLs on each line
pub fn parse_lines<S: AsRef<str>>(lines: &[S]) -> Vec<Styled> {
    let mut st = State::default();
    lines.iter().map(|l| linkify(parse_line(l.as_ref(), &mut st))).collect()
}

fn push(out: &mut Styled, c: char, st: &State) {
    match out.last_mut() {
        Some(s) if s.style == st.style && s.link == st.link => s.text.push(c),
        _ => out.push(Seg { text: c.to_string(), style: st.style, link: st.link.clone() }),
    }
}

fn parse_line(line: &str, st: &mut State) -> Styled {
    let mut out = vec![];
    let mut cs = line.chars().peekable();
    while let Some(c) = cs.next() {
        if c != '\x1b' {
            if !c.is_control() {
                push(&mut out, c, st);
            }
            continue;
        }
        match cs.next() {
            // CSI: parameters + a final letter; only m (SGR) is understood, the rest (cursor movement and such) is dropped
            Some('[') => {
                let mut params = String::new();
                for c in cs.by_ref() {
                    if c.is_ascii_alphabetic() || c == '~' || c == '@' {
                        if c == 'm' {
                            sgr(&params, &mut st.style);
                        }
                        break;
                    }
                    params.push(c);
                }
            }
            // OSC: up to BEL or ESC \; only 8 (hyperlink) is understood
            Some(']') => {
                let mut body = String::new();
                while let Some(c) = cs.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        if cs.peek() == Some(&'\\') {
                            cs.next();
                        }
                        break;
                    }
                    body.push(c);
                }
                if let Some(rest) = body.strip_prefix("8;") {
                    // 8;params;URL: empty URL = end of link
                    let url = rest.split_once(';').map_or("", |(_, u)| u);
                    st.link = (!url.is_empty()).then(|| url.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

fn color256(n: u8) -> Color {
    match n {
        0..=7 => [Color::Black, Color::Red, Color::Green, Color::Yellow, Color::Blue, Color::Magenta, Color::Cyan, Color::Gray][n as usize],
        8..=15 => [
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
        ][n as usize - 8],
        _ => Color::Indexed(n),
    }
}

/// SGR parameters → style. Semicolons separate groups, colons within a group separate subparameters (the 38:2::r:g:b form)
fn sgr(params: &str, style: &mut Style) {
    let groups: Vec<Vec<u16>> = if params.is_empty() {
        vec![vec![0]]
    } else {
        params.split(';').map(|g| g.split(':').map(|p| p.parse().unwrap_or(0)).collect()).collect()
    };
    let mut i = 0;
    while i < groups.len() {
        let g = &groups[i];
        let p = g[0];
        match p {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            9 => *style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            29 => *style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => *style = style.fg(color256((p - 30) as u8)),
            39 => style.fg = None,
            40..=47 => *style = style.bg(color256((p - 40) as u8)),
            49 => style.bg = None,
            90..=97 => *style = style.fg(color256((p - 90 + 8) as u8)),
            100..=107 => *style = style.bg(color256((p - 100 + 8) as u8)),
            38 | 48 | 58 => {
                let c = if g.len() > 1 {
                    // Colon form: 38:5:n / 38:2:r:g:b / 38:2:colorspace:r:g:b
                    match g[1] {
                        5 => g.get(2).map(|&n| color256(n as u8)),
                        2 if g.len() >= 5 => {
                            let rgb = &g[g.len() - 3..];
                            Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
                        }
                        _ => None,
                    }
                } else {
                    // Semicolon form: the following groups are its parameters, consumed with it
                    let at = |k: usize| groups.get(i + k).map(|g| g[0]);
                    match at(1) {
                        Some(5) => {
                            let c = at(2).map(|n| color256(n as u8));
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let c = match (at(2), at(3), at(4)) {
                                (Some(r), Some(g), Some(b)) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                                _ => None,
                            };
                            i += 4;
                            c
                        }
                        _ => None,
                    }
                };
                match (p, c) {
                    (38, Some(c)) => *style = style.fg(c),
                    (48, Some(c)) => *style = style.bg(c),
                    _ => {} // 58 = underline color, ignored
                }
            }
            _ => {}
        }
        i += 1;
    }
}

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`\u{3000}-\u{303f}\u{ff01}-\u{ff5e}]+"#).unwrap());

/// URLs in the text become links too (where the program sent OSC 8 itself is left alone). Trailing punctuation and extra
/// closing parens aren't part of the URL
fn linkify(line: Styled) -> Styled {
    let text = plain(&line);
    let spans: Vec<(usize, usize, String)> = URL
        .find_iter(&text)
        .map(|m| {
            let mut u = m.as_str();
            loop {
                let t = u.trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
                let t = if t.ends_with(')') && t.matches('(').count() < t.matches(')').count() { &t[..t.len() - 1] } else { t };
                let t = if t.ends_with(']') && !t.contains('[') { &t[..t.len() - 1] } else { t };
                if t.len() == u.len() {
                    break;
                }
                u = t;
            }
            (m.start(), m.start() + u.len(), u.to_string())
        })
        .collect();
    if spans.is_empty() {
        return line;
    }
    let mut out = vec![];
    let mut pos = 0; // byte position
    let st_of = |seg: &Seg| State { style: seg.style, link: seg.link.clone() };
    for seg in line {
        for c in seg.text.chars() {
            let mut st = st_of(&seg);
            if st.link.is_none() {
                st.link = spans.iter().find(|(a, b, _)| (*a..*b).contains(&pos)).map(|(_, _, u)| u.clone());
            }
            push(&mut out, c, &st);
            pos += c.len_utf8();
        }
    }
    out
}

pub fn plain(line: &Styled) -> String {
    line.iter().map(|s| s.text.as_str()).collect()
}

/// Wrap by display width (Chinese takes two cells). English breaks at spaces where it can; a URL that fits on one line is
/// moved whole to the next line rather than broken in the middle (on a phone you long-press a URL to open it, and a broken one
/// isn't recognized whole)
pub fn wrap(lines: &[Styled], width: usize) -> Vec<Styled> {
    wrap_rows(lines, width).0
}

/// Like `wrap`, also giving the wrapped row each original line starts at (for jumping by file in the diff)
pub fn wrap_rows(lines: &[Styled], width: usize) -> (Vec<Styled>, Vec<usize>) {
    let mut out = vec![];
    let mut starts = Vec::with_capacity(lines.len());
    if width == 0 {
        return (out, starts);
    }
    for line in lines {
        starts.push(out.len());
        // Spread into single characters, so it's easy to back up to a break point
        let cells: Vec<(char, &Seg)> = line.iter().flat_map(|s| s.text.chars().map(move |c| (c, s))).collect();
        let mut row: Vec<(char, &Seg)> = vec![];
        let mut w = 0;
        let width_of = |r: &[(char, &Seg)]| r.iter().map(|(c, _)| c.width().unwrap_or(0)).sum::<usize>();
        for (i, &(c, seg)) in cells.iter().enumerate() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width && !row.is_empty() {
                // Only spaces left (Claude pads diff lines to the pane width with background-colored spaces): no new row just for them
                if cells[i..].iter().all(|&(x, _)| x == ' ') {
                    break;
                }
                if c == ' ' {
                    out.push(collect(&row));
                    row.clear();
                    w = 0;
                    continue;
                }
                // In the middle of a URL: where in this row it starts, and how wide it is in total
                let link_start = seg.link.as_ref().and_then(|u| {
                    let p = row.iter().rposition(|(_, s)| s.link.as_ref() != Some(u)).map_or(0, |p| p + 1);
                    let tail = cells[i..].iter().take_while(|(_, s)| s.link.as_ref() == Some(u));
                    let total = width_of(&row[p..]) + tail.map(|(c, _)| c.width().unwrap_or(0)).sum::<usize>();
                    (p > 0 && total <= width).then_some(p)
                });
                let space = row.iter().rposition(|&(x, _)| x == ' ').filter(|&p| p > 0 && p * 2 >= row.len());
                let rest = match (link_start, space) {
                    (Some(p), _) => {
                        let rest = row.split_off(p);
                        while row.last().is_some_and(|(x, _)| *x == ' ') {
                            row.pop();
                        }
                        rest
                    }
                    (None, Some(p)) => {
                        let rest = row.split_off(p + 1);
                        row.pop();
                        rest
                    }
                    _ => vec![],
                };
                out.push(collect(&row));
                row = rest;
                w = width_of(&row);
            }
            row.push((c, seg));
            w += cw;
        }
        out.push(collect(&row));
    }
    (out, starts)
}

fn collect(row: &[(char, &Seg)]) -> Styled {
    let mut out: Styled = vec![];
    for &(c, seg) in row {
        let st = State { style: seg.style, link: seg.link.clone() };
        push(&mut out, c, &st);
    }
    out
}

pub fn to_line(l: &Styled) -> Line<'static> {
    Line::from(l.iter().map(|s| Span::styled(s.text.clone(), s.style)).collect::<Vec<_>>())
}

/// After the text is drawn, turn link cells into "OSC 8 open + char + OSC 8 close". Each cell is wrapped on its own: ratatui
/// only redraws cells that changed, so with open / close only at the ends, a middle cell redrawn alone would lose the link.
/// ForcedWidth tells ratatui the cell really only takes the character's width.
pub fn paint_links(buf: &mut Buffer, area: Rect, rows: &[Styled]) {
    for (dy, row) in rows.iter().enumerate().take(area.height as usize) {
        let y = area.y + dy as u16;
        let mut x = area.x;
        for seg in row {
            for c in seg.text.chars() {
                let cw = c.width().unwrap_or(0) as u16;
                if x >= area.x + area.width {
                    break;
                }
                if let (Some(url), Some(w)) = (&seg.link, std::num::NonZeroU16::new(cw)) {
                    let cell = &mut buf[(x, y)];
                    let sym = format!("\x1b]8;;{url}\x1b\\{}\x1b]8;;\x1b\\", cell.symbol());
                    cell.set_symbol(&sym);
                    cell.set_diff_option(CellDiffOption::ForcedWidth(w));
                }
                x += cw;
            }
        }
    }
}

/// Strip OSC 8 (for debug printing)
pub fn strip_osc8(s: &str) -> String {
    static OSC8: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\]8;[^\x1b\x07]*(\x1b\\|\x07)").unwrap());
    OSC8.replace_all(s, "").into_owned()
}

/// Plain text (for telling the state, finding the input box)
pub fn strip(s: &str) -> String {
    let mut st = State::default();
    plain(&parse_line(s, &mut st))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors_across_lines_and_osc8() {
        let lines = [
            "plain \x1b[31mred\x1b[39m \x1b[1m\x1b[38;2;10;200;30mrgb",
            "still rgb\x1b[0m \x1b]8;;https://e.x/a\x1b\\link\x1b]8;;\x1b\\ end",
        ];
        let v = parse_lines(&lines);
        assert_eq!(plain(&v[0]), "plain red rgb");
        assert_eq!(v[0][1], Seg { text: "red".into(), style: Style::new().fg(Color::Red), link: None });
        let rgb = Style::new().add_modifier(Modifier::BOLD).fg(Color::Rgb(10, 200, 30));
        assert_eq!(v[0][3].style, rgb);
        assert_eq!(v[1][0].style, rgb); // a color not turned off carries on into the next line
        let link: Vec<&Seg> = v[1].iter().filter(|s| s.link.is_some()).collect();
        assert_eq!((link[0].text.as_str(), link[0].link.as_deref()), ("link", Some("https://e.x/a")));
    }

    #[test]
    fn plain_urls_become_links_without_trailing_punctuation() {
        let v = parse_lines(&["see https://a.b/c?d=1). and （https://中.cn/x）"]);
        let links: Vec<(String, String)> = v[0].iter().filter_map(|s| s.link.clone().map(|l| (s.text.clone(), l))).collect();
        assert_eq!(links[0], ("https://a.b/c?d=1".into(), "https://a.b/c?d=1".into()));
        assert_eq!(links[1].1, "https://中.cn/x"); // full-width parens aren't part of the URL
    }

    #[test]
    fn wrap_keeps_a_url_whole_when_it_fits_on_a_line() {
        let v = parse_lines(&["go to https://ex.am/ple now"]);
        let w: Vec<String> = wrap(&v, 20).iter().map(plain).collect();
        assert_eq!(w, ["go to", "https://ex.am/ple", "now"]);
        // A URL longer than a line has to break, but every piece still links to the whole URL
        let v = parse_lines(&["x https://example.com/a/very/long/path"]);
        let rows = wrap(&v, 12);
        assert!(rows.len() > 2);
        assert!(rows[1..].iter().all(|r| r.iter().all(|s| s.link.as_deref() == Some("https://example.com/a/very/long/path"))));
    }

    #[test]
    fn padding_past_the_edge_does_not_get_a_row_of_its_own() {
        // Pane wider than the preview: the background-colored spaces padding to the pane's right edge mustn't wrap into a row of their own
        let v = parse_lines(&["\x1b[48;5;194m 12 + x      \x1b[49m", "next"]);
        let rows = wrap(&v, 8);
        assert_eq!(rows.iter().map(plain).collect::<Vec<_>>(), [" 12 + x ", "next"]);
        // The row that's kept still has its background running to the right
        assert_eq!(rows[0][0].style.bg, Some(Color::Indexed(194)));
        // Spaces with more text after them wrap as usual
        let v = parse_lines(&["ab   cd      "]);
        assert_eq!(wrap(&v, 4).iter().map(plain).collect::<Vec<_>>(), ["ab  ", "cd  "]);
    }

    #[test]
    fn sgr_256_and_colon_forms() {
        let mut s = Style::default();
        sgr("38;5;208;48;5;4", &mut s);
        assert_eq!((s.fg, s.bg), (Some(Color::Indexed(208)), Some(Color::Blue)));
        let mut s = Style::default();
        sgr("38:2::1:2:3", &mut s);
        assert_eq!(s.fg, Some(Color::Rgb(1, 2, 3)));
        sgr("0", &mut s);
        assert_eq!(s, Style::default());
    }

    #[test]
    fn strip_gives_plain_text() {
        assert_eq!(strip("\x1b[1m❯\x1b[0m \x1b]8;;u\x1b\\hi\x1b]8;;\x1b\\"), "❯ hi");
    }
}
