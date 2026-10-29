//! How the command line's output looks in a terminal (like cat → bat): `aca ls` / `show` / `output` / `help` / `names` /
//! `group ls` are colored with the interface's theme and aligned in columns, replies laid out as Markdown, code colored by
//! language (the same machinery as reading a record in the interface: `trace::md`, `hl`).
//!
//! Only printed this way when stdout is a terminal: each function here returns true = printed; false = not a terminal, left
//! alone, and the caller prints as before (the original lines in cli.rs, unchanged to the letter). Scripts and agents read that
//! version; for programs it's still `--json`.
//! In a terminal with `NO_COLOR` set it's laid out the same, just without color.

use std::io::{IsTerminal, Write};

use aca_core::config::Config;
use aca_core::i18n::{Lang, lang};
use aca_core::paths;
use aca_core::runtime::{Run, mark};
use aca_core::status::{Row, State};
use aca_core::store::{Group, Store};
use aca_core::{t, tr};
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::ansi::Styled;
use crate::trace::{self, seg};
use crate::{create, data, hl, theme, ui};

/// stdout is a terminal = a person is looking
fn human() -> bool {
    std::io::stdout().is_terminal()
}

/// Whether to color: not when NO_COLOR is set (non-empty) or the terminal is dumb
fn colored(no_color: Option<&str>, term: Option<&str>) -> bool {
    no_color.is_none_or(str::is_empty) && term != Some("dumb")
}

fn color() -> bool {
    colored(std::env::var("NO_COLOR").ok().as_deref(), std::env::var("TERM").ok().as_deref())
}

/// Terminal width (COLUMNS if it can't be queried, else 80)
fn width() -> usize {
    ratatui::crossterm::terminal::size()
        .ok()
        .map(|(w, _)| w as usize)
        .filter(|w| *w > 0)
        .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
        .unwrap_or(80)
}

fn put(lines: &[Styled]) {
    let color = color();
    let mut out = std::io::stdout().lock();
    for l in lines {
        let s: String = l.iter().map(|x| if color { crate::ansi(&x.text, x.style) } else { x.text.clone() }).collect();
        // Terminal closed, or piped into a pager that quit early: exit quietly, don't panic
        if writeln!(out, "{}", s.trim_end()).is_err() {
            return;
        }
    }
}

fn text(s: impl Into<String>) -> crate::ansi::Seg {
    seg(s, Style::new())
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// How long: 12s / 3m / 5h / 2d
pub fn age(sec: u64) -> String {
    match sec {
        0..60 => tr!("{sec}秒", "{sec}s"),
        60..3600 => tr!("{}分", "{}m", sec / 60),
        3600..86400 => tr!("{}时", "{}h", sec / 3600),
        _ => tr!("{}天", "{}d", sec / 86400),
    }
}

/// A state icon's color (same as the interface's list, see `ui::icon`) and its name
fn state(s: State) -> (Style, &'static str) {
    let p = theme::pal();
    let strong = |c| Style::new().fg(c).add_modifier(Modifier::BOLD);
    match s {
        State::Running => (strong(p.green), t!("运行中", "running")),
        State::Ask => (strong(p.red), t!("等待回应", "needs you")),
        State::Unread => (strong(p.amber), t!("未读", "unread")),
        State::Idle => (Style::new().fg(p.muted), t!("空闲", "idle")),
        State::Error => (strong(p.red), t!("出错", "error")),
        State::Exited | State::Stopped => (Style::new().fg(p.muted), t!("已停止", "stopped")),
    }
}

/// "tool:account": the account colored by its agent (Claude orange, Codex blue, see theme.rs), the tool name before it in the
/// minor color; with no account, just the tool name.
/// `brief` = print only the account (the color tells which agent, as in the interface's list): for a narrow terminal where
/// titles don't fit
fn who(cfg: &Config, tool: &str, account: &str, brief: bool) -> Styled {
    let st = ui::agent_style(data::agent_of_tool(cfg, tool));
    let name = seg(aca_core::agent::account_label(account), st);
    match (account.is_empty(), brief) {
        (true, _) => vec![seg(tool, st)],
        (false, true) => vec![name],
        (false, false) => vec![seg(format!("{tool}:"), theme::minor()), name],
    }
}

fn wide(l: &Styled) -> usize {
    l.iter().map(|x| x.text.width()).sum()
}

fn spaces(n: usize) -> crate::ansi::Seg {
    text(" ".repeat(n))
}

/// Which part of an id to print, and how many leading characters are enough to type. Print the first 8 (or more, until it's
/// told apart from the others); enough to type = the characters that tell it apart from every other registered id (`all`
/// includes archived ones: `aca show 12` looks the prefix up among them all), at least 2, so one new session doesn't make it
/// too few
fn short(id: &str, all: &[String]) -> (String, usize) {
    let n = id.chars().count();
    let same = |o: &String| o.chars().zip(id.chars()).take_while(|(a, b)| a == b).count();
    let need = all.iter().filter(|o| o.as_str() != id).map(same).max().map_or(1, |m| m + 1);
    let lit = need.max(2).min(n);
    (id.chars().take(lit.max(8).min(n)).collect(), lit)
}

/// All registered ids (archived included); if they can't be read, compare only with the ones listed
fn all_ids(store: &Store, rows: &[Row]) -> Vec<String> {
    match store.sessions(true) {
        Ok(v) if !v.is_empty() => v.into_iter().map(|s| s.id).collect(),
        _ => rows.iter().map(|r| r.id.clone()).collect(),
    }
}

/// The id cell: the characters enough to type as usual, the rest faint, padded to w columns
fn id_cell((shown, lit): &(String, usize), w: usize) -> Styled {
    let head: String = shown.chars().take(*lit).collect();
    let tail: String = shown.chars().skip(*lit).collect();
    let pad = w.saturating_sub(shown.width());
    vec![seg(head, bold()), seg(format!("{tail}{}", " ".repeat(pad)), theme::faint())]
}

/// Keep the title column at least this wide (if the terminal is narrower still, let it wrap)
const TITLE_MIN: usize = 12;

/// `aca ls`: one block per group, the count after the group name; one row = state, id, title, tool:account, how long idle,
/// each its own column; totals on the last line
fn list(cfg: &Config, rows: &[Row], all: &[String], w: usize, now: u64) -> Vec<Styled> {
    if rows.is_empty() {
        return vec![vec![seg(
            t!(
                "暂无会话。新建:aca new <工具[:账号]> [目录];接管已在运行的会话:aca adopt",
                "No sessions yet. Start one: aca new <TOOL[:ACCOUNT]> [DIR]; adopt running ones: aca adopt"
            ),
            theme::minor(),
        )]];
    }
    let ids: Vec<(String, usize)> = rows.iter().map(|r| short(&r.id, all)).collect();
    let ages: Vec<String> = rows.iter().map(|r| age(now.saturating_sub(r.active))).collect();
    let marks: Vec<String> = rows.iter().map(|r| r.tokens.iter().map(|(k, v)| format!(" {}", mark(k, v))).collect()).collect();
    let most = |it: &mut dyn Iterator<Item = usize>| it.max().unwrap_or(0);
    let idw = most(&mut ids.iter().map(|i| i.0.width()));
    let agew = most(&mut ages.iter().map(|a| a.width()));
    let longest = most(&mut rows.iter().map(|r| r.title.width()));
    // "  ● id  title  tool:account  age [tags]": titles as wide as the longest. If a row doesn't fit (on a phone): first drop
    // the tool name and keep only the account; if it still doesn't fit, cut titles to the terminal width
    let fit = |brief: bool| {
        let whos: Vec<Styled> = rows.iter().map(|r| who(cfg, &r.tool, &r.account, brief)).collect();
        let whow = most(&mut whos.iter().map(wide));
        let rest = 4 + idw + 2 + 2 + whow + 2 + agew + most(&mut marks.iter().map(|m| m.width()));
        (whos, whow, w.saturating_sub(rest))
    };
    let (whos, whow, room) = Some(fit(false)).filter(|f| f.2 >= longest).unwrap_or_else(|| fit(true));
    let titlew = longest.min(room.max(TITLE_MIN));
    let mut out = vec![];
    let mut group = None;
    for (i, r) in rows.iter().enumerate() {
        if group != Some(&r.group) {
            let name = if r.group.is_empty() { t!("(未分组)", "(no group)") } else { &r.group };
            let n = rows.iter().filter(|x| x.group == r.group).count();
            out.push(vec![seg(name, bold()), seg(format!("  {n}"), theme::minor())]);
            group = Some(&r.group);
        }
        // Not running: the whole row a shade fainter
        let off = matches!(r.state, State::Exited | State::Stopped);
        let mut line = vec![spaces(2), seg(r.glyph(), state(r.state).0), spaces(1)];
        line.extend(id_cell(&ids[i], idw));
        line.push(spaces(2));
        line.push(seg(ui::clip(&trace::clean(&r.title), titlew), if off { theme::minor() } else { Style::new() }));
        line.push(spaces(2));
        line.extend(whos[i].clone());
        line.push(seg(format!("{}{}", " ".repeat(whow - wide(&whos[i]) + 2 + agew - ages[i].width()), ages[i]), theme::minor()));
        line.push(seg(marks[i].clone(), theme::minor()));
        out.push(line);
        if !r.ask.is_empty() {
            // When it needs you, its question goes under the title
            let indent = 4 + idw + 2;
            let ask = ui::clip(&trace::clean(&r.ask), w.saturating_sub(indent + 2).max(TITLE_MIN));
            out.push(vec![spaces(indent), seg("└ ", theme::faint()), seg(ask.trim_end(), Style::new().fg(theme::pal().red))]);
        }
    }
    out.push(vec![]);
    out.push(summary(rows));
    out
}

/// The last line: how many in all, how many need you, unread, running, errored (no scrolling back when the list is longer
/// than a screen)
fn summary(rows: &[Row]) -> Styled {
    let mut line = vec![seg(tr!("{} 个会话", "sessions: {}", rows.len()), theme::minor())];
    for s in [State::Ask, State::Unread, State::Running, State::Error] {
        let n = rows.iter().filter(|r| r.state == s).count();
        if n > 0 {
            let (st, word) = state(s);
            line.extend([seg(" · ", theme::faint()), seg(s.glyph(), st), seg(format!(" {n} {word}"), theme::minor())]);
        }
    }
    line
}

/// A section heading: like `aca cost`'s, followed by a rule drawn to w columns
fn head(title: &str, w: usize) -> Styled {
    let rule = w.saturating_sub(title.width() + 2);
    vec![spaces(1), seg(title, theme::accent().add_modifier(Modifier::BOLD)), seg(format!(" {}", "─".repeat(rule)), theme::faint())]
}

/// Lay replies out at most this wide (however wide the terminal, overly long lines are hard to read)
const PROSE: usize = 100;

/// Lay the reply out as Markdown, wrapped to w columns (the interface's record-reading machinery); without color it's left as
/// is, marks kept
fn markdown(reply: &str, w: usize, color: bool) -> Vec<Styled> {
    if !color {
        return reply.lines().map(|l| vec![text(trace::clean(l))]).collect();
    }
    trace::wrap(&trace::md(reply, Style::new()), w.clamp(20, PROSE))
}

/// `aca show <id>`: state and title on the first line, then one field per line, finally the last reply laid out
fn card(cfg: &Config, row: &Row, run: &Run, all: &[String], w: usize, now: u64, color: bool) -> Vec<Styled> {
    let (st, word) = state(row.state);
    let word = if row.idle_stopped > 0 { t!("自动停止", "stopped by aca for idling") } else { word };
    let mut out = vec![vec![seg(row.glyph(), st), spaces(1), seg(trace::clean(&row.title), bold())]];
    // The name column: Chinese names are two characters (4 columns) plus two spaces; in English the longest, account, also gets two
    let col: usize = if lang() == Lang::En { 9 } else { 6 };
    let mut field = |name: &str, value: Styled| {
        let mut line = vec![spaces(2), seg(format!("{name}{}", " ".repeat(col.saturating_sub(name.width()))), theme::minor())];
        line.extend(value);
        out.push(line);
    };
    let none = |s: &str| vec![seg(s, theme::minor())];
    let since = match row.state {
        State::Running if row.ran > 0 => tr!(" · 已运行 {}", " · running for {}", age(row.ran)),
        _ if row.idle_stopped > 0 => tr!(" · {}前停止", " · {} ago", age(now.saturating_sub(row.idle_stopped))),
        _ => tr!(" · {} 无活动", " · quiet for {}", age(now.saturating_sub(row.active))),
    };
    field(t!("状态", "state"), vec![seg(word, Style::new().fg(st.fg.unwrap_or_default())), seg(since, theme::minor())]);
    if !row.ask.is_empty() {
        field(t!("提问", "asking"), vec![seg(trace::clean(&row.ask), Style::new().fg(theme::pal().red))]);
    }
    let (_, lit) = short(&row.id, all);
    field("id", id_cell(&(row.id.clone(), lit), 0));
    field(t!("账号", "account"), who(cfg, &row.tool, &row.account, false));
    field(t!("分组", "group"), if row.group.is_empty() { none(t!("(未分组)", "(no group)")) } else { vec![text(row.group.as_str())] });
    field(t!("目录", "dir"), vec![text(create::tilde(&row.dir))]);
    field(t!("对话", "conv"), if row.conv.is_empty() { none(t!("暂无", "none yet")) } else { vec![text(row.conv.as_str())] });
    field(
        "tmux",
        if row.tmux.is_empty() {
            none(t!("未运行", "not running"))
        } else {
            vec![text(row.tmux.as_str()), seg(format!(" {}", row.pane), theme::minor())]
        },
    );
    if !row.tokens.is_empty() {
        field(t!("标签", "tags"), vec![seg(row.tokens.iter().map(|(k, v)| mark(k, v)).collect::<Vec<_>>().join(" "), theme::minor())]);
    }
    if !run.last.is_empty() {
        out.push(vec![]);
        out.push(head(t!("最后一条回复", "Last reply"), w.min(PROSE)));
        out.extend(markdown(&run.last, w.saturating_sub(2), color).into_iter().map(|l| [vec![spaces(2)], l].concat()));
    }
    out
}

/// Color a piece of usage: commands and fixed words in the accent color (bold too with `strong`), what's inside `<…>` `[…]` and
/// after `--` in the minor color, `|` `…` as separators
fn usage(s: &str, strong: bool) -> Styled {
    let lit = if strong { theme::accent().add_modifier(Modifier::BOLD) } else { theme::accent() };
    let mut out: Styled = vec![];
    let mut push = |c: char, st: Style| match out.last_mut() {
        Some(last) if last.style == st => last.text.push(c),
        _ => out.push(seg(c.to_string(), st)),
    };
    let (mut depth, mut passed) = (0usize, false);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        // A lone --: what follows is handed on to something else as is
        let alone = |at: usize| at == 0 || chars[at - 1] == ' ';
        if depth == 0 && c == '-' && chars.get(i + 1) == Some(&'-') && alone(i) && chars.get(i + 2).is_none_or(|x| *x == ' ') {
            passed = true;
        }
        match c {
            '<' | '[' => {
                depth += 1;
                push(c, theme::minor());
            }
            '>' | ']' => {
                depth = depth.saturating_sub(1);
                push(c, theme::minor());
            }
            _ if depth > 0 || passed => push(c, theme::minor()),
            '|' | '…' => push(c, theme::faint()),
            _ => push(c, lit),
        }
    }
    out
}

/// Whether a line is usage: `aca`, or `aca ` followed by a command or option
fn is_usage(l: &str) -> bool {
    l == "aca" || l.strip_prefix("aca ").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '<' | '[' | '-')))
}

/// Whether a line is config.toml syntax: `[table]`, `key = value`, `# comment`
fn is_toml(body: &str) -> bool {
    if body.starts_with('#') {
        return true;
    }
    if let Some(rest) = body.strip_prefix('[') {
        return rest.split_once(']').is_some_and(|(_, after)| after.trim().is_empty() || after.trim_start().starts_with('#'));
    }
    let key = body.split(' ').next().unwrap_or("");
    !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && body[key.len()..].starts_with(" = ")
}

/// A line like "  name  description" (indented two, at least two spaces between name and description): the byte where the
/// description starts
fn gap(l: &str) -> Option<usize> {
    let body = l.strip_prefix("  ").filter(|b| !b.starts_with(' '))?;
    let at = body.find("  ")?;
    let more = body[at..].trim_start();
    (!more.is_empty()).then(|| l.len() - more.len())
}

/// The name column is at most this wide; anything wider is taken as a sentence (that happens to contain two spaces)
const TERM_MAX: usize = 40;

/// Split this line into name and description at column `col` (a line whose name is too long, only one space from its
/// description, is split where the other lines of the same paragraph are)
fn split_at_col(l: &str, col: usize) -> Option<usize> {
    l.strip_prefix("  ").filter(|b| !b.starts_with(' '))?;
    let at = l.char_indices().map(|(i, _)| i).find(|i| l[..*i].width() >= col)?;
    (l[..at].width() == col && l[..at].ends_with(' ') && !l[at..].starts_with(' ')).then_some(at)
}

/// Color a help page: usage, subheadings, each item's name (command, option, key), commands in examples, config.toml syntax
/// each get their own color. Not a letter or space changes (it's already laid out), only color
fn doc(page: &str) -> Vec<Styled> {
    let lines: Vec<&str> = page.lines().collect();
    let (mut sh, mut toml) = (hl::Hl::for_lang("sh"), hl::Hl::for_lang("toml"));
    let code = |h: &mut Option<hl::Hl>, l: &str| {
        let body = l.trim_start();
        let mut line = vec![spaces(l.len() - body.len())];
        match h {
            Some(h) => {
                h.reset();
                line.extend(h.line(body, Style::new()));
            }
            None => line.push(text(body)),
        }
        line
    };
    let named = |l: &str, at: usize| {
        let name = l[2..at].trim_end();
        let mut line = vec![spaces(2)];
        line.extend(usage(name, false));
        line.push(text(&l[2 + name.len()..]));
        line
    };
    let mut out = Vec::with_capacity(lines.len());
    let (mut lead, mut col) = (true, None);
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            (lead, col) = (false, None);
            out.push(vec![]);
            continue;
        }
        // The column where descriptions start in a paragraph (separated by blank lines): set by its first "name  description" line
        if col.is_none() {
            col = lines[i..].iter().take_while(|x| !x.trim().is_empty()).find_map(|x| gap(x).map(|at| x[..at].width()));
        }
        let body = l.trim_start();
        let at = gap(l).filter(|at| l[2..*at].trim_end().width() <= TERM_MAX).or_else(|| split_at_col(l, col?));
        let line = if lead && is_usage(l) {
            usage(l, true)
        } else if let Some(rest) = l.strip_prefix("aca — ").filter(|_| i == 0) {
            vec![seg("aca", bold()), text(format!(" — {rest}"))]
        } else if !l.starts_with(' ') {
            // Subheading: followed by an indented line; one ending in a period is a sentence and doesn't count
            let title = lines.get(i + 1).is_some_and(|n| n.starts_with("  ")) && !l.ends_with(['。', '.']);
            vec![seg(*l, if title { bold() } else { Style::new() })]
        } else if is_toml(body) {
            code(&mut toml, l)
        } else if body.starts_with("aca ") && at.is_none_or(|at| l[at..].starts_with('#')) {
            code(&mut sh, l)
        } else if let Some(at) = at {
            named(l, at)
        } else {
            vec![text(*l)]
        };
        out.push(line);
    }
    out
}

/// `aca ls`
pub fn rows(cfg: &Config, store: &Store, rows: &[Row]) -> bool {
    if human() {
        put(&list(cfg, rows, &all_ids(store, rows), width(), paths::now()));
    }
    human()
}

/// `aca show <id>`
pub fn show(cfg: &Config, store: &Store, row: &Row, run: &Run) -> bool {
    if human() {
        put(&card(cfg, row, run, &all_ids(store, std::slice::from_ref(row)), width(), paths::now(), color()));
    }
    human()
}

/// `aca output <id>`: the last reply
pub fn reply(reply: &str) -> bool {
    if human() {
        put(&markdown(reply, width(), color()));
    }
    human()
}

/// `aca help`, `aca help <NAME>`, `aca <COMMAND> --help`
pub fn help(page: &str) -> bool {
    if human() {
        put(&doc(page));
    }
    human()
}

/// `aca names`: one "tool" or "tool:account" per line
pub fn names(cfg: &Config, names: &[String]) -> bool {
    if human() {
        put(&names
            .iter()
            .map(|n| n.split_once(':').map_or_else(|| who(cfg, n, "", false), |(t, a)| who(cfg, t, a, false)))
            .collect::<Vec<_>>());
    }
    human()
}

/// `aca group ls`: order, path, default directory
fn group_lines(groups: &[Group]) -> Vec<Styled> {
    let pathw = groups.iter().map(|g| g.path.width()).max().unwrap_or(0);
    groups
        .iter()
        .map(|g| {
            let pad = pathw - g.path.width() + 2;
            vec![
                seg(format!("{:>3}  ", g.order), theme::minor()),
                seg(g.path.as_str(), bold()),
                spaces(pad),
                seg(create::tilde(&g.default_path), theme::minor()),
            ]
        })
        .collect()
}

pub fn groups(groups: &[Group]) -> bool {
    if human() {
        put(&group_lines(groups));
    }
    human()
}

/// `aca account ls`: one row per (claude / codex, account, config directory)
fn account_lines(all: &[(&str, &str, &str)]) -> Vec<Styled> {
    let whos: Vec<Styled> = all.iter().map(|(tool, name, _)| who(&Config::default(), tool, name, false)).collect();
    let w = whos.iter().map(wide).max().unwrap_or(0);
    let line = |(mut l, dir): (Styled, &str)| {
        l.extend([spaces(w - wide(&l) + 2), seg(dir, theme::minor())]);
        l
    };
    whos.into_iter().zip(all.iter().map(|a| a.2)).map(line).collect()
}

pub(crate) fn accounts(all: &[(&str, &str, &str)]) -> bool {
    if human() {
        put(&account_lines(all));
    }
    human()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(l: &Styled) -> String {
        l.iter().map(|x| x.text.as_str()).collect::<String>().trim_end().to_string()
    }

    fn page(lines: &[Styled]) -> String {
        lines.iter().map(plain).collect::<Vec<_>>().join("\n")
    }

    /// The style of the segment in this line containing `what`
    fn style_of(l: &Styled, what: &str) -> Style {
        l.iter().find(|x| x.text.contains(what)).unwrap_or_else(|| panic!("没有 {what}:{}", plain(l))).style
    }

    fn row(id: &str, title: &str, state: State, group: &str, who: &str, idle: u64) -> Row {
        let (tool, account) = who.split_once(':').unwrap_or((who, ""));
        Row {
            id: id.into(),
            title: title.into(),
            state,
            tool: tool.into(),
            account: account.into(),
            group: group.into(),
            dir: "/work/app".into(),
            parent: String::new(),
            fork_of: String::new(),
            tmux: String::new(),
            pane: String::new(),
            attached: 0,
            conv: String::new(),
            ask: String::new(),
            ran: 0,
            active: NOW - idle,
            bg: 0,
            locked: false,
            created: 0,
            order: 0,
            tokens: Default::default(),
            idle_stopped: 0,
        }
    }

    /// "Now" in the tests
    const NOW: u64 = 10_000_000;

    fn ids(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn no_color_and_dumb_terminals_get_no_color() {
        assert!(colored(None, Some("xterm-256color")));
        assert!(colored(Some(""), None));
        assert!(!colored(Some("1"), Some("xterm-256color")));
        assert!(!colored(None, Some("dumb")));
    }

    #[test]
    fn an_id_shows_eight_letters_and_lights_the_ones_you_need_to_type() {
        let all: Vec<String> =
            ["887f90d1", "88aa0000-1790000000", "c6eaa5b3-1791295731", "c6eaa5b3-1791295999", "ab"].map(String::from).to_vec();
        // Shares only its first two characters with another: three are enough
        assert_eq!(short("887f90d1", &all), ("887f90d1".to_string(), 3));
        // Nothing similar: still at least two; a long id prints only its first 8
        assert_eq!(short("ab", &all), ("ab".to_string(), 2));
        assert_eq!(short("88aa0000-1790000000", &all), ("88aa0000".to_string(), 3));
        // Can't be told apart in the first 8: print until it can
        assert_eq!(short("c6eaa5b3-1791295731", &all), ("c6eaa5b3-1791295731".chars().take(17).collect(), 17));
        // An archived one isn't in the list but must be told apart too (it's in `all`)
        assert_eq!(short("887f90d1", &["887f90d1".to_string()]), ("887f90d1".to_string(), 2));
        let cell = id_cell(&short("887f90d1", &all), 10);
        assert_eq!((cell[0].text.as_str(), cell[1].text.as_str()), ("887", "f90d1  "));
        assert_eq!((cell[0].style, cell[1].style), (bold(), theme::faint()));
    }

    #[test]
    fn the_list_lines_its_columns_up_and_counts_at_the_bottom() {
        let mut rows = vec![
            row("11111111-1790000000", "修登录", State::Running, "work/app", "claude:sub", 5),
            row("22222222", "a much longer title here", State::Ask, "work/app", "codex", 90),
            row("33333333", "停着的", State::Stopped, "", "claude:~/.claude-x", 86400 * 3),
            row("44444444", "闲着停的", State::Exited, "", "claude:~/.claude-x", 86400 * 3),
        ];
        rows[1].ask = "Allow Bash(rm -rf)?".into();
        // One aca stopped for idling is drawn ☾, apart from ■ stopped by a person
        rows[3].idle_stopped = NOW - 3600;
        rows[1].tokens.insert("pr".into(), "#12".into());
        let out = list(&Config::default(), &rows, &ids(&rows), 100, NOW);
        assert_eq!(
            page(&out),
            "work/app  2\n\
             \x20 ● 11111111  修登录                    claude:sub        5秒\n\
             \x20 ◆ 22222222  a much longer title here  codex             1分 [pr #12]\n\
             \x20             └ Allow Bash(rm -rf)?\n\
             (未分组)  2\n\
             \x20 ■ 33333333  停着的                    claude:.claude-x  3天\n\
             \x20 ☾ 44444444  闲着停的                  claude:.claude-x  3天\n\
             \n\
             4 个会话 · ◆ 1 等待回应 · ● 1 运行中"
        );
        let p = theme::pal();
        // State and account colors match the interface's list; titles of ones not running a shade fainter
        assert_eq!(style_of(&out[1], "●").fg, Some(p.green));
        assert_eq!(style_of(&out[2], "◆").fg, Some(p.red));
        assert_eq!(style_of(&out[1], "sub").fg, Some(p.claude));
        assert_eq!(style_of(&out[2], "codex").fg, Some(p.codex));
        assert_eq!(style_of(&out[3], "Allow").fg, Some(p.red));
        assert_eq!(style_of(&out[5], "停着的"), theme::minor());
        assert_eq!((style_of(&out[6], "闲着停的"), style_of(&out[6], "☾").fg), (theme::minor(), Some(p.muted)));
        assert_eq!(style_of(&out[1], "修登录"), Style::new());
        assert_eq!(style_of(&out[0], "work/app"), bold());
    }

    #[test]
    fn a_narrow_terminal_drops_the_tool_name_then_cuts_titles() {
        let rows = vec![
            row("11111111", "一个很长很长很长很长很长很长的标题", State::Idle, "g", "claude:sub", 5),
            row("22222222", "短的", State::Idle, "g", "codex", 5),
        ];
        let at = |w: usize| list(&Config::default(), &rows, &ids(&rows), w, NOW);
        // Fits: both tool name and account printed
        assert_eq!(plain(&at(65)[1]), "  ○ 11111111  一个很长很长很长很长很长很长的标题  claude:sub  5秒");
        // Just short: only the account kept (still in its agent's color), titles not cut
        let out = at(60);
        assert_eq!(plain(&out[1]), "  ○ 11111111  一个很长很长很长很长很长很长的标题  sub    5秒");
        assert_eq!(plain(&out[2]), "  ○ 22222222  短的                                codex  5秒");
        assert_eq!(style_of(&out[1], "sub").fg, Some(theme::pal().claude));
        // Narrower: titles cut to the terminal width, no row wraps
        let out = at(46);
        assert!(out.iter().all(|l| plain(l).width() <= 46), "{}", page(&out));
        assert_eq!(plain(&out[1]), "  ○ 11111111  一个很长很长很长很…   sub    5秒");
        // Too narrow to read: still leave a few characters of title (and let the terminal wrap)
        assert_eq!(plain(&at(20)[1]), "  ○ 11111111  一个很长很…   sub    5秒");
    }

    #[test]
    fn an_empty_list_says_how_to_start() {
        assert!(page(&list(&Config::default(), &[], &[], 80, 0)).contains("aca new"));
    }

    #[test]
    fn show_is_a_card_with_the_reply_laid_out_as_markdown() {
        let mut r = row("887f90d1", "修登录", State::Unread, "work", "claude:sub", 120);
        (r.conv, r.tmux, r.pane) = ("3f2a".into(), "修登录".into(), "%7".into());
        let run = Run { last: "改好了:\n\n- **原因**:`session` 过期\n\n```rust\nlet x = 1;\n```".into(), ..Default::default() };
        let out = card(&Config::default(), &r, &run, &ids(std::slice::from_ref(&r)), 60, NOW, true);
        assert_eq!(
            page(&out),
            "◐ 修登录\n\
             \x20 状态  未读 · 2分 无活动\n\
             \x20 id    887f90d1\n\
             \x20 账号  claude:sub\n\
             \x20 分组  work\n\
             \x20 目录  /work/app\n\
             \x20 对话  3f2a\n\
             \x20 tmux  修登录 %7\n\
             \n\
             \x20最后一条回复 ──────────────────────────────────────────────\n\
             \x20 改好了:\n\
             \n\
             \x20 • 原因:session 过期\n\
             \n\
             \x20 │ let x = 1;"
        );
        assert_eq!(out[9].iter().map(|x| x.text.width()).sum::<usize>(), 60);
        // Bold and inline code lose their marks for styles; code blocks colored by language
        let p = theme::pal();
        assert_eq!(style_of(&out[12], "原因"), bold());
        assert_eq!(style_of(&out[12], "session"), theme::accent());
        assert_eq!(style_of(&out[14], "let").fg, Some(p.syn_kw));
        // A running one says how long it's run; one that needs you gets an extra line with its question; one not started says so
        let mut r = row("887f90d1", "t", State::Running, "", "shell", 0);
        (r.ran, r.ask) = (125, "选哪个?".into());
        let out = card(&Config::default(), &r, &Run::default(), &[], 60, NOW, true);
        assert_eq!(
            page(&out),
            "● t\n  状态  运行中 · 已运行 2分\n  提问  选哪个?\n  id    887f90d1\n  账号  shell\n  分组  (未分组)\n  目录  /work/app\n  对话  暂无\n  tmux  未运行"
        );
    }

    #[test]
    fn without_color_the_reply_keeps_its_markdown_marks() {
        let reply = "- **原因**:`x`\n\x1b[31m红\x1b[0m";
        assert_eq!(page(&markdown(reply, 80, false)), "- **原因**:`x`\n红");
        assert_eq!(page(&markdown(reply, 80, true)), "• 原因:x\n红");
        // Overly long lines wrap at the terminal width, PROSE columns at most
        let long = "word ".repeat(60);
        assert!(markdown(&long, 40, true).iter().all(|l| plain(l).width() <= 40));
        assert!(markdown(&long, 400, true).iter().all(|l| plain(l).width() <= PROSE));
    }

    #[test]
    fn usage_marks_the_fixed_words_and_dims_what_you_fill_in() {
        let kinds = |s: &str| -> Vec<(String, &'static str)> {
            usage(s, false)
                .into_iter()
                .map(|x| {
                    let kind = match x.style {
                        st if st == theme::accent() => "词",
                        st if st == theme::minor() => "填",
                        st if st == theme::faint() => "隔",
                        _ => "?",
                    };
                    (x.text, kind)
                })
                .collect()
        };
        let own = |v: &[(&str, &'static str)]| v.iter().map(|(t, k)| (t.to_string(), *k)).collect::<Vec<_>>();
        assert_eq!(kinds("new <工具[:账号]> [目录]"), own(&[("new ", "词"), ("<工具[:账号]>", "填"), (" ", "词"), ("[目录]", "填")]));
        assert_eq!(kinds("start | stop <id>"), own(&[("start ", "词"), ("|", "隔"), (" stop ", "词"), ("<id>", "填")]));
        assert_eq!(kinds("remote run <机器> -- 命令"), own(&[("remote run ", "词"), ("<机器>", "填"), (" ", "词"), ("-- 命令", "填")]));
        assert_eq!(kinds("--all"), own(&[("--all", "词")]));
        assert_eq!(usage("aca ls", true)[0].style, theme::accent().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn a_help_page_keeps_every_letter_and_colors_by_what_a_line_is() {
        let text = "aca new <工具[:账号]> [目录] [-- 参数…]\naca new --hold\n\n新建会话并启动\n\n  工具[:账号]      认哪些名字:aca names\n                   不写账号用默认的\n  --as 工具[:账号] 照这个登记\n  --hold           只登记  不启动\n\n例:\n  aca new claude -g .            # 当前目录\n  aca new codex . -- --search\n\n不带参数是一张表。\n  ↑↓ 挑账号 · ←→ 换接法\n\n工具\n  [tools.claude]                   # 默认值\n  account = \"sub\"\n  没登记的目录不用写在这:aca new codex:~/.x";
        let out = doc(text);
        // Not a letter or a space changes
        assert_eq!(page(&out), text);
        let p = theme::pal();
        let strong = theme::accent().add_modifier(Modifier::BOLD);
        // Usage: commands in bold accent, what you fill in in the minor color
        assert_eq!((style_of(&out[0], "aca new"), style_of(&out[0], "目录")), (strong, theme::minor()));
        assert_eq!(style_of(&out[1], "--hold"), strong);
        assert_eq!(style_of(&out[3], "新建"), Style::new());
        // One item per line: name colored, description plain; the wrapped line, and the line whose too-long name is one space
        // from its description, split right too
        assert_eq!((style_of(&out[5], "工具"), style_of(&out[5], "认哪些")), (theme::accent(), Style::new()));
        assert_eq!(out[6].len(), 1);
        assert_eq!((style_of(&out[7], "--as"), style_of(&out[7], "照这个")), (theme::accent(), Style::new()));
        assert_eq!((style_of(&out[8], "--hold"), style_of(&out[8], "只登记  不启动")), (theme::accent(), Style::new()));
        // Subheadings bold; a sentence followed by indented lines isn't a subheading
        assert_eq!(style_of(&out[10], "例"), bold());
        assert_eq!(style_of(&out[17], "工具"), bold());
        assert_eq!(style_of(&out[14], "不带参数"), Style::new());
        assert_eq!(out[15].len(), 1);
        // Examples colored as shell, config.toml syntax as TOML; a sentence in between isn't colored
        assert_eq!(style_of(&out[11], "# 当前目录").fg, Some(p.syn_com));
        assert_eq!(style_of(&out[11], "aca").fg, Some(p.syn_fn));
        assert_eq!(style_of(&out[18], "# 默认值").fg, Some(p.syn_com));
        assert_eq!(style_of(&out[19], "\"sub\"").fg, Some(p.syn_str));
        assert_eq!(out[20].len(), 1);
    }

    #[test]
    fn the_overview_colors_commands_not_the_first_line() {
        let text = "aca — 一屏管住所有 agent 会话\n\n  aca                    打开界面\n  aca help <命令>        一条命令的细节\n\n会话\n  start | stop <id>      启动 / 停下\n\n<id> 可以只写开头几位。";
        let out = doc(text);
        assert_eq!(page(&out), text);
        assert_eq!((style_of(&out[0], "aca"), style_of(&out[0], "一屏")), (bold(), Style::new()));
        assert_eq!((style_of(&out[2], "aca"), style_of(&out[2], "打开界面")), (theme::accent(), Style::new()));
        assert_eq!((style_of(&out[3], "aca help"), style_of(&out[3], "<命令>")), (theme::accent(), theme::minor()));
        assert_eq!(style_of(&out[5], "会话"), bold());
        assert_eq!(
            (style_of(&out[6], "start"), style_of(&out[6], "|"), style_of(&out[6], "<id>")),
            (theme::accent(), theme::faint(), theme::minor())
        );
        assert_eq!(out[8].len(), 1);
    }

    #[test]
    fn small_lists_line_up_too() {
        let cfg = Config::default();
        let names = group_lines(&[
            Group { path: "work/app".into(), order: 1, default_path: "/srv/app".into() },
            Group { path: "杂".into(), order: 12, default_path: String::new() },
        ]);
        assert_eq!(page(&names), "  1  work/app  /srv/app\n 12  杂");
        let accounts = account_lines(&[("claude", "sub", "~/.claude-sub"), ("codex", "labs", "~/.labs")]);
        assert_eq!(page(&accounts), "claude:sub  ~/.claude-sub\ncodex:labs  ~/.labs");
        assert_eq!(style_of(&accounts[1], "labs").fg, Some(theme::pal().codex));
        // No account: just the tool name, in its agent's color; shell isn't colored
        assert_eq!(who(&cfg, "claude", "", false)[0].style.fg, Some(theme::pal().claude));
        assert_eq!(who(&cfg, "shell", "", true), vec![text("shell")]);
    }
}
