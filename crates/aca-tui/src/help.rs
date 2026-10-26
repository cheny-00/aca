//! `?` 的快捷键弹窗:按用途分组的速查表 + 图例。够宽时排成两三栏(桌面一屏看完),手机上一栏往下翻。
//! 这里只写「按什么 → 干什么」几个字,不解释;细节在 `aca help keys`(keys.txt)和 README。
//! 标题和底下那行(怎么滚)是弹窗画的,不在这里。
//! 弹窗开着时按一个键,说明它的那几行就亮起来(见 `Hit`)。

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::i18n::{Lang, lang};
use aca_core::t;

use crate::data::{Agent, State};
use crate::theme;
use crate::ui::{agent_style, dim, icon, wrap_width};

/// 一组:标题 + (键, 干什么)。键那一列里的「空格」「数字」「前缀」是整个词,按键亮行时靠它对上(见 keys_of、hit)
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

/// 现在那种话的速查表。两份一样长、一样的键,只是字不同(测试查)
fn secs() -> &'static [Sec] {
    if lang() == Lang::En { SECS_EN } else { SECS }
}

/// 弹窗里按下的键:亮起说明它的行
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// 底栏里怎么称呼它:「s」「^R」「空格」
    pub label: String,
    keys: Vec<String>,
    /// 还没滚到它那儿(画的时候滚过去一次,之后随便翻)
    pub jump: bool,
}

/// 按下的键 → Hit。滚动、关闭的键不走这里(弹窗自己处理),认不出的键也给一个(说没有)
pub fn hit(k: KeyEvent) -> Hit {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let keys: Vec<String> = match k.code {
        // ^\ 在普通终端里到手是 ^4(0x1C),kitty 键盘协议里才是 ^\ 本身
        KeyCode::Char('4' | '\\') if ctrl => vec!["^\\".into()],
        // ^` 在弹窗里到手是 NUL(tmux 转的),读成 ^空格
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

/// 一行说明回应哪些键:键那一列(「↑↓ jk」拆成 ↑ ↓ j k;「Tab」「空格」「^R」「⇧⏎」是一个)
fn keys_of(k: &str) -> Vec<String> {
    let mut out = vec![];
    for t in k.split_whitespace() {
        // 中文的键名(空格、前缀)是汉字;英文的那几个(Space、prefix)单列
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

/// 栏与栏之间空几格
const GAP: usize = 3;
/// 一栏最宽这么多,再宽只是空着(说明本来就短)
const COL_MAX: usize = 40;

fn head(t: &str) -> Line<'static> {
    Line::styled(t.to_string(), Style::new().add_modifier(Modifier::BOLD))
}

/// 一组排成几行:键(强调色)占固定宽的一列,说明太长时折行、和上一行的说明对齐。
/// 按下的键对得上的行铺上列表选中时的底色、铺满这一栏;第二个返回值是这些行在块里第几行
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

/// 图例:状态图标照列表里的颜色画,一行三个(窄了两个);再是账号的颜色
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
    // 「图标 + 说明」一格的宽度:英文的说明长一些
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
    // 账号颜色:一行放不下就折到下一行,和第一家对齐
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

/// 整页:w 宽时排几栏、每栏多宽,各组按顺序往栏里填,尽量一样高。
/// 第二个返回值:亮起来的行在第几行(从上往下排好序),弹窗据此滚过去
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
        // 桌面弹窗里三栏并排:第一行就有三个标题,一屏放得下
        assert_eq!(text(&desk[0]).split_whitespace().collect::<Vec<_>>(), ["导航", "会话", "新建时"], "{}", text(&desk[0]));
        assert!(desk.len() <= 26, "{}", desk.len()); // 加上外框、标题也在 36 行的终端里放得下
        // 键单独上色(强调色),说明不上色;手机上说明不折行
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
        // 同一个键在不同界面里干不同的事:都亮
        assert_eq!(lit(&key(KeyCode::Char('n'), none)), ["n ^N 新建 / 新分组", "n p m 切换文件 / 连同提交"]);
        assert_eq!(lit(&key(KeyCode::Char('N'), KeyModifiers::SHIFT)), ["N 按当前配置新建"]);
        assert_eq!(lit(&key(KeyCode::Char('V'), KeyModifiers::SHIFT)), ["v V 多选 / 连续取消"]);
        assert_eq!(lit(&key(KeyCode::Char('L'), KeyModifiers::SHIFT)), ["L $ ^S 布局 / 计费 / 设置"]);
        assert_eq!(lit(&key(KeyCode::Char('q'), none)), ["? q q 快捷键 / 退出", "q Q +键 切换 / 移到工作区"]);
        assert_eq!(lit(&key(KeyCode::Char(']'), none)), ["[ ] ` 上 / 下 / 上次的工作区"]);
        assert_eq!(lit(&key(KeyCode::Char('`'), none)), ["[ ] ` 上 / 下 / 上次的工作区"]);
        assert_eq!(lit(&key(KeyCode::Char('F'), KeyModifiers::SHIFT)), ["F 按状态筛选"]);
        assert!(lit(&key(KeyCode::Char('^'), none)).is_empty()); // 不亮 ^N ^G 那些
        assert_eq!(lit(&key(KeyCode::Char('h'), none)), ["h l ← → 返回上层 / 进入下层"]);
        assert_eq!(lit(&key(KeyCode::Left, none)), ["h l ← → 返回上层 / 进入下层"]);
        assert_eq!(lit(&key(KeyCode::Char('}'), none)), ["{ } ⇧Tab 上 / 下 / 上次的分组"]);
        assert_eq!(lit(&key(KeyCode::BackTab, KeyModifiers::SHIFT)), ["{ } ⇧Tab 上 / 下 / 上次的分组"]);
        assert_eq!(lit(&key(KeyCode::Tab, none)), ["Tab W 分组列表 / 仅本组", "Tab 补全", "Tab 输出 ↔ 改动"]);
        assert_eq!(lit(&key(KeyCode::Enter, KeyModifiers::SHIFT)), ["E ⇧⏎ 附带便签打开"]);
        assert_eq!(lit(&key(KeyCode::Char('g'), KeyModifiers::CONTROL)), ["e ^G s 便签 / 编辑器 / 发送"]);
        assert_eq!(lit(&key(KeyCode::Char('7'), none)).len(), 1); // 数字那行
        // ^\\ 在普通终端里到手是 ^4
        let h = key(KeyCode::Char('4'), KeyModifiers::CONTROL);
        assert_eq!((h.label.as_str(), lit(&h)), ("^\\", vec!["前缀 a ^\\ 弹出 / 关闭 aca".to_string()]));
        let h = key(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert_eq!((h.label.as_str(), lit(&h)), ("^`", vec!["^` 返回上一个会话".to_string()]));
        assert!(lit(&key(KeyCode::Char('U'), none)).is_empty());
        // 亮的那行铺满这一栏的底色,键照样是强调色
        let (ls, at) = lines(40, Some(&key(KeyCode::Char('t'), none)));
        let row = &ls[at[0]];
        assert_eq!(width_of(row), 40);
        assert!(row.spans.iter().all(|s| s.style.bg == Some(theme::pal().sel)));
        assert_eq!(row.spans[0].style.fg, Some(theme::pal().accent));
        // 没按键时一行都不亮
        assert!(lines(40, None).1.is_empty());
    }

    #[test]
    fn the_english_sheet_has_the_same_keys_and_fits_the_same_widths() {
        use aca_core::i18n::with;
        // 两份一一对应:组数、每组的行数、键一样(除了「空格」「数字」这些键名本身)
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
            // prefix、Space 是整词:按 p、S 不会亮它们
            assert!(lit(&key(KeyCode::Char('p'))).iter().all(|l| !l.contains("prefix")));
            assert_eq!(keys_of("^Q prefix d"), ["^Q", "prefix", "d"]);
        });
    }
}
