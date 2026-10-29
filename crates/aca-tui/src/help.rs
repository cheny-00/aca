//! The `?` keys popup: a cheat sheet grouped by purpose + a legend. When wide enough it's laid out in two or three columns (all
//! on one desktop screen); on a phone, one column you scroll down.
//! Only a few words of "press what → does what" here, no explanations; details are in `aca help keys` (keys.txt) and docs/keys.md.
//! The title and the line below it (how to scroll) are drawn by the popup, not here.
//! While the popup is open, pressing a key lights up the rows that explain it (see `Hit`).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::i18n::{Lang, lang};
use aca_core::t;

use crate::data::{Agent, State};
use crate::theme;
use crate::ui::{agent_style, dim, icon, wrap_width};

/// A section: title + (key, what it does). In the key column 「空格」「数字」「前缀」 (Space, 0-9, prefix) are whole words; a
/// pressed key is matched to its rows by them (see keys_of, hit)
type Sec = (&'static str, &'static [(&'static str, &'static str)]);

const SECS: &[Sec] = &[
    (
        "导航",
        &[
            ("↑↓ jk", "上下"),
            ("J K", "下一组 / 上一组"),
            ("h l ← →", "返回上层 / 进入下层"),
            ("数字", "按编号选择"),
            ("空格", "跳到会话 / 分组"),
            ("g G", "顶部 / 底部"),
            ("轻点", "选中，再点打开"),
        ],
    ),
    (
        "打开",
        &[
            ("⏎", "打开"),
            ("E ⇧⏎", "附带便签打开"),
            ("i", "下一个待处理"),
            ("z x Z", "全屏输出 / 改动 / 记录"),
            ("t T y", "终端 / 新窗口 / yazi"),
        ],
    ),
    (
        "筛选",
        &[
            ("/", "搜索"),
            ("w a *", "未停止 / 全部 / 天数"),
            ("F", "按状态筛选"),
            ("Tab W", "分组列表 / 仅本组"),
            ("{ } ⇧Tab", "上 / 下 / 上次的分组"),
            ("o O", "折叠 / 全部折叠"),
        ],
    ),
    (
        "会话",
        &[
            ("n ^N", "新建 / 新分组"),
            ("N", "按当前配置新建"),
            ("e ^G s", "便签 / 编辑器 / 发送"),
            ("p u", "置顶 / 已读·未读"),
            ("v V", "多选 / 连续取消"),
            ("r R D", "重命名 / 重启 / 停止"),
            ("m ⇧↑ ⇧↓", "移到分组 / 上移下移"),
            ("f H", "分叉 / 交接"),
            ("X A", "切换账号 / 接管"),
            ("M P :", "MCP / 插件 / 命令"),
            ("d ^Z b", "删除 / 撤销 / 回收站"),
            ("c B C", "复制回复 / 信息 / 选取"),
        ],
    ),
    (
        "视图",
        &[
            ("L $ ^S", "布局 / 计费 / 设置"),
            ("^R ^L", "刷新 / 重启 aca"),
            ("? q q", "快捷键 / 退出"),
            ("q Q", "+键 切换 / 移到工作区"),
            ("[ ] `", "上 / 下 / 上次的工作区"),
            ("S", "工作区列表 · ^R 重命名"),
        ],
    ),
    ("tmux 中", &[("^Q 前缀 d", "返回 aca"), ("前缀 a ^\\", "弹出 / 关闭 aca"), ("^`", "返回上一个会话")]),
    (
        "新建时",
        &[
            ("Tab", "补全"),
            ("→ /", "选择"),
            ("↓ ^F", "查找目录"),
            ("^W ^U", "删除一级 / 清空"),
            ("^T", "shell / 切回"),
            ("+", "添加账号(n 菜单中)"),
        ],
    ),
    (
        "全屏阅读",
        &[
            ("↑↓ jk", "滚动"),
            ("空格 b", "翻页"),
            ("u d", "半页"),
            ("g G", "顶部 / 底部"),
            ("J K", "切换会话"),
            ("Tab", "输出 ↔ 改动"),
            ("n p m", "切换文件 / 连同提交"),
            ("e l", "编辑器 / lazygit"),
        ],
    ),
];

const SECS_EN: &[Sec] = &[
    (
        "Navigate",
        &[
            ("↑↓ jk", "up / down"),
            ("J K", "next / prev group"),
            ("h l ← →", "out / in a level"),
            ("0-9", "select by number"),
            ("Space", "jump to a row"),
            ("g G", "top / bottom"),
            ("tap", "select, tap to open"),
        ],
    ),
    (
        "Open",
        &[
            ("⏎", "open"),
            ("E ⇧⏎", "open with the note"),
            ("i", "next to look at"),
            ("z x Z", "output/diff/record"),
            ("t T y", "shell/window/yazi"),
        ],
    ),
    (
        "Filter",
        &[
            ("/", "search"),
            ("w a *", "open / all / days"),
            ("F", "only one state"),
            ("Tab W", "groups / this group"),
            ("{ } ⇧Tab", "prev/next/last group"),
            ("o O", "fold / fold all"),
        ],
    ),
    (
        "Sessions",
        &[
            ("n ^N", "new / group"),
            ("N", "new with current"),
            ("e ^G s", "note/editor/send"),
            ("p u", "pin / read-unread"),
            ("v V", "select / unselect"),
            ("r R D", "rename/restart/stop"),
            ("m ⇧↑ ⇧↓", "move / up / down"),
            ("f H", "fork / hand off"),
            ("X A", "account / adopt"),
            ("M P :", "MCP / plugins / commands"),
            ("d ^Z b", "delete/undo/trash"),
            ("c B C", "copy reply/info/pick"),
        ],
    ),
    (
        "View",
        &[
            ("L $ ^S", "layout/bill/settings"),
            ("^R ^L", "refresh / restart"),
            ("? q q", "keys / quit"),
            ("q Q", "+key: go / send"),
            ("[ ] `", "prev/next/last space"),
            ("S", "spaces, ^R renames"),
        ],
    ),
    ("In tmux", &[("^Q prefix d", "back to aca"), ("prefix a ^\\", "pop up / close aca"), ("^`", "previous session")]),
    (
        "When creating",
        &[
            ("Tab", "complete"),
            ("→ /", "accept"),
            ("↓ ^F", "find a directory"),
            ("^W ^U", "del level / clear"),
            ("^T", "shell / back"),
            ("+", "add account (n menu)"),
        ],
    ),
    (
        "Full-screen reading",
        &[
            ("↑↓ jk", "scroll"),
            ("Space b", "page"),
            ("u d", "half page"),
            ("g G", "top / bottom"),
            ("J K", "switch session"),
            ("Tab", "output ↔ diff"),
            ("n p m", "next file / commits"),
            ("e l", "editor / lazygit"),
        ],
    ),
];

/// The cheat sheet in the current language. Both have the same length and the same keys, only the words differ (a test checks)
fn secs() -> &'static [Sec] {
    if lang() == Lang::En { SECS_EN } else { SECS }
}

/// A key pressed in the popup: lights up the rows that explain it
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// What the bottom bar calls it: s, ^R, 「空格」 (Space)
    pub label: String,
    keys: Vec<String>,
    /// Not scrolled to yet (drawing scrolls there once, then you scroll freely)
    pub jump: bool,
}

/// Pressed key → Hit. Scrolling and closing keys don't come here (the popup handles them); an unknown key gets one too (to say
/// there's none)
pub fn hit(k: KeyEvent) -> Hit {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let keys: Vec<String> = match k.code {
        // ^\ arrives as ^4 (0x1C) in a plain terminal; only with the kitty keyboard protocol is it ^\ itself
        KeyCode::Char('4' | '\\') if ctrl => vec!["^\\".into()],
        // In the popup ^` arrives as NUL (forwarded by tmux), read as ^Space
        KeyCode::Char(' ' | '`') if ctrl => vec!["^`".into()],
        KeyCode::Char(c) if ctrl => vec![format!("^{}", c.to_ascii_uppercase())],
        KeyCode::Char(' ') => vec![t!("空格", "Space").into()],
        KeyCode::Char(c) if c.is_ascii_digit() => vec![c.to_string(), t!("数字", "0-9").into()],
        KeyCode::Char(c) => vec![c.to_string()],
        KeyCode::Enter if k.modifiers.contains(KeyModifiers::SHIFT) => vec!["⇧⏎".into()],
        KeyCode::Enter => vec!["⏎".into()],
        KeyCode::Tab => vec!["Tab".into()],
        KeyCode::BackTab => vec!["⇧Tab".into()],
        KeyCode::Right => vec!["→".into()],
        KeyCode::Left => vec!["←".into()],
        KeyCode::Backspace => vec!["⌫".into()],
        _ => vec![],
    };
    let label = match k.code {
        KeyCode::Char(c) if c.is_ascii_digit() && !ctrl => c.to_string(),
        _ => keys.first().cloned().unwrap_or_else(|| t!("这个键", "this key").into()),
    };
    Hit { label, keys, jump: true }
}

/// Which keys a row answers to: its key column (`↑↓ jk` splits into ↑ ↓ j k; `Tab`, 「空格」 (Space), `^R`, `⇧⏎` are one key each)
fn keys_of(k: &str) -> Vec<String> {
    let mut out = vec![];
    for t in k.split_whitespace() {
        // Chinese key names (「空格」, 「前缀」) are all Han characters; the English ones (Space, prefix) are listed one by one
        let word = t.starts_with('^')
            || ["Tab", "⇧Tab", "Esc", "⇧⏎", "Space", "0-9", "tap", "prefix"].contains(&t)
            || t.chars().all(|c| !c.is_ascii() && c.is_alphabetic());
        if word {
            out.push(t.to_string());
        } else {
            out.extend(t.chars().map(String::from));
        }
    }
    out
}

/// Spaces between columns
const GAP: usize = 3;
/// A column is at most this wide; wider would only be blank (the descriptions are short anyway)
const COL_MAX: usize = 40;

fn head(t: &str) -> Line<'static> {
    Line::styled(t.to_string(), Style::new().add_modifier(Modifier::BOLD))
}

/// A section laid out in rows: the key (accent color) in a fixed-width column; a description too long wraps, aligned with the
/// description above. Rows matching the pressed key get the list's selection background across the whole column; the second
/// return value is those rows' line numbers within the block
fn block(sec: &Sec, key_w: usize, w: usize, hit: Option<&Hit>) -> (Vec<Line<'static>>, Vec<usize>) {
    let key = theme::accent().add_modifier(Modifier::BOLD);
    let mut out = vec![head(sec.0)];
    let mut lit = vec![];
    for (k, d) in sec.1 {
        let on = hit.is_some_and(|h| keys_of(k).iter().any(|x| h.keys.contains(x)));
        if on {
            lit.push(out.len());
        }
        let bg = if on { Style::new().bg(theme::pal().sel) } else { Style::new() };
        for (i, part) in wrap_width(d, w.saturating_sub(key_w).max(8)).into_iter().enumerate() {
            let k = if i == 0 { format!("{k}{}", " ".repeat(key_w.saturating_sub(k.width()))) } else { " ".repeat(key_w) };
            let pad = if on { w.saturating_sub(key_w + part.width()) } else { 0 };
            out.push(Line::from(vec![
                Span::styled(k, key.patch(bg)),
                Span::styled(part, bg.add_modifier(if on { Modifier::BOLD } else { Modifier::empty() })),
                Span::styled(" ".repeat(pad), bg),
            ]));
        }
    }
    (out, lit)
}

/// The legend: state icons in the list's colors, three per row (two when narrow); then the accounts' colors
fn legend(w: usize) -> Vec<Line<'static>> {
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
        (Span::styled("✎", theme::warn()), t!("有便签", "has a note")),
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
        [("Claude", Agent::Claude), ("Codex", Agent::Codex), ("Pi", Agent::Pi), ("OpenCode", Agent::Opencode)].into_iter().enumerate()
    {
        if i > 0 && used + 2 + name.width() > w {
            out.push(Line::from(std::mem::take(&mut row)));
            row.push(Span::raw(" ".repeat(label.width())));
            used = label.width();
        } else if i > 0 {
            row.push(Span::raw("  "));
            used += 2;
        }
        row.push(Span::styled(name, agent_style(a).add_modifier(Modifier::BOLD)));
        used += name.width();
    }
    out.push(Line::from(row));
    out
}

fn width_of(l: &Line) -> usize {
    l.spans.iter().map(|s| s.content.width()).sum()
}

/// The whole page: at width w, how many columns and how wide each; sections fill the columns in order, as evenly tall as
/// possible. Second return value: the line numbers of the lit rows (sorted top to bottom), which the popup scrolls to
pub fn lines(w: usize, hit: Option<&Hit>) -> (Vec<Line<'static>>, Vec<usize>) {
    let cols = match w {
        0..=69 => 1,
        70..=99 => 2,
        _ => 3,
    };
    let col_w = (w.saturating_sub(GAP * (cols - 1)) / cols).clamp(12, COL_MAX);
    let key_w = secs().iter().flat_map(|s| s.1.iter()).map(|(k, _)| k.width()).max().unwrap_or(0) + 2;
    let mut blocks: Vec<(Vec<Line<'static>>, Vec<usize>)> = secs().iter().map(|s| block(s, key_w, col_w, hit)).collect();
    blocks.push((legend(col_w), vec![]));
    let total: usize = blocks.iter().map(|b| b.0.len() + 1).sum();
    let target = total.div_ceil(cols);
    let mut columns: Vec<Vec<Line<'static>>> = vec![vec![]];
    let mut lit = vec![];
    for (b, on) in blocks {
        let cur = columns.last().unwrap();
        if !cur.is_empty() && cur.len() + b.len() > target && columns.len() < cols {
            columns.push(vec![]);
        }
        let cur = columns.last_mut().unwrap();
        if !cur.is_empty() {
            cur.push(Line::raw(""));
        }
        lit.extend(on.iter().map(|i| cur.len() + i));
        cur.extend(b);
    }
    lit.sort_unstable();
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    let out = (0..rows)
        .map(|y| {
            let mut spans = vec![];
            for (i, c) in columns.iter().enumerate() {
                let l = c.get(y).cloned().unwrap_or_default();
                let pad = if i + 1 < columns.len() { col_w + GAP - width_of(&l) } else { 0 };
                spans.extend(l.spans);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
            Line::from(spans)
        })
        .collect();
    (out, lit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn phone_gets_one_column_desktop_gets_three_and_nothing_overflows() {
        for w in [40, 44, 60, 100, 106, 150] {
            let ls = lines(w, None).0;
            assert!(ls.iter().all(|l| width_of(l) <= w), "{w} 宽时有行超出");
            let heads = ls.iter().filter(|l| text(l).contains("导航")).count();
            assert_eq!(heads, 1, "{w}");
        }
        let phone = lines(40, None).0;
        let desk = lines(106, None).0;
        assert!(desk.len() * 2 < phone.len(), "桌面分栏后矮得多:{} 对 {}", desk.len(), phone.len());
        // The desktop popup has three columns side by side: the first line already has three titles, it fits on one screen
        assert_eq!(text(&desk[0]).split_whitespace().collect::<Vec<_>>(), ["导航", "会话", "新建时"], "{}", text(&desk[0]));
        assert!(desk.len() <= 26, "{}", desk.len()); // fits a 36-line terminal even with the frame and title
        // Keys get their own color (accent), descriptions none; on a phone descriptions don't wrap
        let row = phone.iter().find(|l| text(l).contains("便签 / 编辑器 / 发送")).unwrap();
        assert_eq!(row.spans[0].content.trim(), "e ^G s");
        assert_eq!(row.spans[0].style.fg, Some(theme::pal().accent));
    }

    #[test]
    fn a_row_answers_to_the_keys_in_its_key_column() {
        assert_eq!(keys_of("↑↓ jk"), ["↑", "↓", "j", "k"]);
        assert_eq!(keys_of("{ } ⇧Tab"), ["{", "}", "⇧Tab"]);
        assert_eq!(keys_of("h l ← →"), ["h", "l", "←", "→"]);
        assert_eq!(keys_of("e ^G"), ["e", "^G"]);
        assert_eq!(keys_of("E ⇧⏎"), ["E", "⇧⏎"]);
        assert_eq!(keys_of("空格 b"), ["空格", "b"]);
        assert_eq!(keys_of("前缀 a"), ["前缀", "a"]);
        assert_eq!(keys_of("^\\"), ["^\\"]);
    }

    #[test]
    fn pressing_a_key_lights_up_every_row_that_explains_it() {
        let key = |c: KeyCode, m: KeyModifiers| hit(KeyEvent::new(c, m));
        let lit = |h: &Hit| {
            let (ls, lit) = lines(40, Some(h));
            lit.iter().map(|&y| text(&ls[y]).split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>()
        };
        let none = KeyModifiers::NONE;
        // The same key does different things in different views: all of them light up
        assert_eq!(lit(&key(KeyCode::Char('n'), none)), ["n ^N 新建 / 新分组", "n p m 切换文件 / 连同提交"]);
        assert_eq!(lit(&key(KeyCode::Char('N'), KeyModifiers::SHIFT)), ["N 按当前配置新建"]);
        assert_eq!(lit(&key(KeyCode::Char('V'), KeyModifiers::SHIFT)), ["v V 多选 / 连续取消"]);
        assert_eq!(lit(&key(KeyCode::Char('L'), KeyModifiers::SHIFT)), ["L $ ^S 布局 / 计费 / 设置"]);
        assert_eq!(lit(&key(KeyCode::Char('q'), none)), ["? q q 快捷键 / 退出", "q Q +键 切换 / 移到工作区"]);
        assert_eq!(lit(&key(KeyCode::Char(']'), none)), ["[ ] ` 上 / 下 / 上次的工作区"]);
        assert_eq!(lit(&key(KeyCode::Char('`'), none)), ["[ ] ` 上 / 下 / 上次的工作区"]);
        assert_eq!(lit(&key(KeyCode::Char('F'), KeyModifiers::SHIFT)), ["F 按状态筛选"]);
        assert!(lit(&key(KeyCode::Char('^'), none)).is_empty()); // doesn't light ^N, ^G and the like
        assert_eq!(lit(&key(KeyCode::Char('h'), none)), ["h l ← → 返回上层 / 进入下层"]);
        assert_eq!(lit(&key(KeyCode::Left, none)), ["h l ← → 返回上层 / 进入下层"]);
        assert_eq!(lit(&key(KeyCode::Char('}'), none)), ["{ } ⇧Tab 上 / 下 / 上次的分组"]);
        assert_eq!(lit(&key(KeyCode::BackTab, KeyModifiers::SHIFT)), ["{ } ⇧Tab 上 / 下 / 上次的分组"]);
        assert_eq!(lit(&key(KeyCode::Tab, none)), ["Tab W 分组列表 / 仅本组", "Tab 补全", "Tab 输出 ↔ 改动"]);
        assert_eq!(lit(&key(KeyCode::Enter, KeyModifiers::SHIFT)), ["E ⇧⏎ 附带便签打开"]);
        assert_eq!(lit(&key(KeyCode::Char('g'), KeyModifiers::CONTROL)), ["e ^G s 便签 / 编辑器 / 发送"]);
        assert_eq!(lit(&key(KeyCode::Char('7'), none)).len(), 1); // the 0-9 row
        // In a plain terminal ^\\ arrives as ^4
        let h = key(KeyCode::Char('4'), KeyModifiers::CONTROL);
        assert_eq!((h.label.as_str(), lit(&h)), ("^\\", vec!["前缀 a ^\\ 弹出 / 关闭 aca".to_string()]));
        let h = key(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert_eq!((h.label.as_str(), lit(&h)), ("^`", vec!["^` 返回上一个会话".to_string()]));
        assert!(lit(&key(KeyCode::Char('U'), none)).is_empty());
        // The lit row has the background across the whole column; the key is still in the accent color
        let (ls, at) = lines(40, Some(&key(KeyCode::Char('t'), none)));
        let row = &ls[at[0]];
        assert_eq!(width_of(row), 40);
        assert!(row.spans.iter().all(|s| s.style.bg == Some(theme::pal().sel)));
        assert_eq!(row.spans[0].style.fg, Some(theme::pal().accent));
        // With no key pressed, no row is lit
        assert!(lines(40, None).1.is_empty());
    }

    #[test]
    fn the_english_sheet_has_the_same_keys_and_fits_the_same_widths() {
        use aca_core::i18n::with;
        // The two match one to one: number of sections, rows per section, keys (except key names themselves like 「空格」「数字」)
        assert_eq!(SECS.len(), SECS_EN.len());
        for (zh, en) in SECS.iter().zip(SECS_EN) {
            assert_eq!(zh.1.len(), en.1.len(), "{}", zh.0);
            for ((kz, _), (ke, _)) in zh.1.iter().zip(en.1) {
                let named = |k: &str| k.replace("空格", "Space").replace("数字", "0-9").replace("轻点", "tap").replace("前缀", "prefix");
                assert_eq!(named(kz), *ke);
            }
        }
        with(Lang::En, || {
            for w in [40, 44, 60, 100, 106, 150] {
                let ls = lines(w, None).0;
                assert!(ls.iter().all(|l| width_of(l) <= w), "{w} 宽时有行超出");
            }
            let key = |c: KeyCode| hit(KeyEvent::new(c, KeyModifiers::NONE));
            let lit = |h: &Hit| {
                let (ls, lit) = lines(40, Some(h));
                lit.iter().map(|&y| text(&ls[y]).split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>()
            };
            assert_eq!(lit(&key(KeyCode::Char(' '))), ["Space jump to a row", "Space b page"]);
            assert_eq!(lit(&key(KeyCode::Char('7'))), ["0-9 select by number"]);
            // prefix and Space are whole words: pressing p or S doesn't light them
            assert!(lit(&key(KeyCode::Char('p'))).iter().all(|l| !l.contains("prefix")));
            assert_eq!(keys_of("^Q prefix d"), ["^Q", "prefix", "d"]);
        });
    }
}
