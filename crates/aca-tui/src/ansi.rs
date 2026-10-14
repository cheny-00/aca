//! 预览 / 全屏阅读里的颜色和链接。
//!
//! `tmux capture-pane -e` 给的是带转义序列的文本:SGR(颜色、粗体…)和 OSC 8 超链接都在里面,
//! 而且**状态跨行延续**(上一行开的颜色没关,下一行开头不会再发一遍),所以要按顺序整段解析。
//! 解析成「一段段带样式的文字」,再按屏幕宽度折行、交给 ratatui 画。
//!
//! 链接:会话里的程序自己发的 OSC 8 照搬;没发的,把文字里的网址认出来也当链接。画的时候
//! 每个格子都包上完整网址(见 `paint_links`),网址被折成几行时点哪一截都是整条网址。

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

/// 一行 = 若干段
pub type Styled = Vec<Seg>;

/// 解析到哪儿了:跨行带着走
#[derive(Default)]
struct State {
    style: Style,
    link: Option<String>,
}

/// 按顺序解析整段(状态跨行延续),每行认一遍网址
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
            // CSI:参数 + 结尾字母;只认 m(SGR),别的(光标移动之类)丢掉
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
            // OSC:到 BEL 或 ESC \ 为止;只认 8(超链接)
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
                    // 8;参数;网址 —— 网址空 = 链接结束
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

/// SGR 参数 → 样式。分号分组,组内冒号是子参数(38:2::r:g:b 这种写法)
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
                    // 冒号写法:38:5:n / 38:2:r:g:b / 38:2:色彩空间:r:g:b
                    match g[1] {
                        5 => g.get(2).map(|&n| color256(n as u8)),
                        2 if g.len() >= 5 => {
                            let rgb = &g[g.len() - 3..];
                            Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
                        }
                        _ => None,
                    }
                } else {
                    // 分号写法:后面的组是参数,一起吃掉
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
                    _ => {} // 58 = 下划线颜色,不管
                }
            }
            _ => {}
        }
        i += 1;
    }
}

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`\u{3000}-\u{303f}\u{ff01}-\u{ff5e}]+"#).unwrap());

/// 文字里的网址也当链接(程序自己发了 OSC 8 的地方不动)。结尾的标点、多出来的右括号不算网址
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
    let mut pos = 0; // 字节位置
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

/// 按显示宽度折行(中文占两格)。英文尽量在空格处断;放得下一整行的网址宁可整条挪到下一行,
/// 也不从中间断开(手机上长按网址打开,断开的认不全)
pub fn wrap(lines: &[Styled], width: usize) -> Vec<Styled> {
    wrap_rows(lines, width).0
}

/// 同 `wrap`,另外给出每行原文从折好的第几行开始(看改动时按文件跳要用)
pub fn wrap_rows(lines: &[Styled], width: usize) -> (Vec<Styled>, Vec<usize>) {
    let mut out = vec![];
    let mut starts = Vec::with_capacity(lines.len());
    if width == 0 {
        return (out, starts);
    }
    for line in lines {
        starts.push(out.len());
        // 摊成一个个字符,方便回退到断点
        let cells: Vec<(char, &Seg)> = line.iter().flat_map(|s| s.text.chars().map(move |c| (c, s))).collect();
        let mut row: Vec<(char, &Seg)> = vec![];
        let mut w = 0;
        let width_of = |r: &[(char, &Seg)]| r.iter().map(|(c, _)| c.width().unwrap_or(0)).sum::<usize>();
        for (i, &(c, seg)) in cells.iter().enumerate() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width && !row.is_empty() {
                // 后面只剩空格(Claude 的 diff 行拿带底色的空格补到窗格那么宽):不为它们另起一行
                if cells[i..].iter().all(|&(x, _)| x == ' ') {
                    break;
                }
                if c == ' ' {
                    out.push(collect(&row));
                    row.clear();
                    w = 0;
                    continue;
                }
                // 正在一条网址中间:这条网址从本行哪儿开始、一共多宽
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

/// 画完文字后,把链接格子改成「OSC 8 开 + 字 + OSC 8 关」。每格单独包一次:ratatui 只重画变了的格子,
/// 只在首尾开关的话,中间的格子单独重画时就不带链接了。ForcedWidth 告诉 ratatui 这格实际只占字宽。
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

/// 去掉 OSC 8(调试打印用)
pub fn strip_osc8(s: &str) -> String {
    static OSC8: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\]8;[^\x1b\x07]*(\x1b\\|\x07)").unwrap());
    OSC8.replace_all(s, "").into_owned()
}

/// 纯文字(判断状态、找输入框用)
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
        assert_eq!(v[1][0].style, rgb); // 没关的颜色延续到下一行
        let link: Vec<&Seg> = v[1].iter().filter(|s| s.link.is_some()).collect();
        assert_eq!((link[0].text.as_str(), link[0].link.as_deref()), ("link", Some("https://e.x/a")));
    }

    #[test]
    fn plain_urls_become_links_without_trailing_punctuation() {
        let v = parse_lines(&["see https://a.b/c?d=1). and （https://中.cn/x）"]);
        let links: Vec<(String, String)> = v[0].iter().filter_map(|s| s.link.clone().map(|l| (s.text.clone(), l))).collect();
        assert_eq!(links[0], ("https://a.b/c?d=1".into(), "https://a.b/c?d=1".into()));
        assert_eq!(links[1].1, "https://中.cn/x"); // 全角括号不算网址的一部分
    }

    #[test]
    fn wrap_keeps_a_url_whole_when_it_fits_on_a_line() {
        let v = parse_lines(&["go to https://ex.am/ple now"]);
        let w: Vec<String> = wrap(&v, 20).iter().map(plain).collect();
        assert_eq!(w, ["go to", "https://ex.am/ple", "now"]);
        // 比一行还长的网址只能断开,但每一截都还是整条网址的链接
        let v = parse_lines(&["x https://example.com/a/very/long/path"]);
        let rows = wrap(&v, 12);
        assert!(rows.len() > 2);
        assert!(rows[1..].iter().all(|r| r.iter().all(|s| s.link.as_deref() == Some("https://example.com/a/very/long/path"))));
    }

    #[test]
    fn padding_past_the_edge_does_not_get_a_row_of_its_own() {
        // 窗格比预览宽:底色补到窗格右边的那截空格,折过来不该是单独一行
        let v = parse_lines(&["\x1b[48;5;194m 12 + x      \x1b[49m", "next"]);
        let rows = wrap(&v, 8);
        assert_eq!(rows.iter().map(plain).collect::<Vec<_>>(), [" 12 + x ", "next"]);
        // 留下的那行底色照旧铺到右边
        assert_eq!(rows[0][0].style.bg, Some(Color::Indexed(194)));
        // 空格后面还有字的照常折
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
