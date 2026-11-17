//! Session record (`Z`): the tree a conversation's record is read into comes from `aca_records::trace` (re-exported here);
//! this file finds a row's record and lays the tree out as text.
//! UI (ui.rs): on desktop the left is the conversation (`chat`: one block per turn — what you said, what Claude said last, a
//! line of numbers), the right is the selected turn's intermediate steps (`steps`: each call's numbers, thinking, interim text,
//! tools with their edits and output); on a phone each column takes the whole screen. `trace_page` lays out the blocks.

use std::path::PathBuf;

use aca_core::{t, tr};
use aca_records::agent::Agent;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub use aca_records::trace::*;

use crate::ansi::{Seg, Styled};
use crate::data::Session;
use crate::kit::{clip, dim};
use crate::theme;

/// Session → the record file of its current conversation
pub fn find(s: &Session) -> Result<PathBuf, String> {
    if !matches!(s.agent, Agent::Claude | Agent::Codex) {
        return Err(t!("不是 Claude / Codex 会话", "not a Claude / Codex session").into());
    }
    let conf = crate::mcp::home_of(s);
    let w = crate::data::where_of(s);
    aca_records::workdir::log_of(&w, &conf).ok_or_else(|| t!("找不到记录", "no record found").into())
}

/// Find and read this session's whole record
pub fn load(s: &Session) -> Result<Trace, String> {
    let t = Trace::open(find(s)?, s.agent).map_err(|_| t!("无法读取记录", "can't read the record").to_string())?;
    if t.cx.old {
        // The format before Codex 0.107
        return Err(t!("记录格式过旧,无法查看", "the record's format is too old to read").into());
    }
    Ok(t)
}

/// Assemble a line: the left spans + the summary in the middle (clipped to fit)
fn fit(left: Vec<Span<'static>>, label: &str, label_st: Style, w: usize) -> Line<'static> {
    let lw: usize = left.iter().map(|s| s.content.width()).sum();
    let mut v = left;
    v.push(Span::styled(clip(label, w.saturating_sub(lw)).trim_end().to_string(), label_st));
    Line::from(v)
}

/// One-line rows in the conversation: compaction, the copied block (expandable)
pub fn row(t: &Trace, i: usize, opened: bool, w: usize) -> Line<'static> {
    let n = &t.nodes[i];
    match n.kind {
        Kind::Copied => {
            let turns = n.kids.iter().filter(|&&k| t.nodes[k].kind == Kind::Turn).count();
            let fold = Span::styled(if opened { "▾ " } else { "▸ " }, theme::faint());
            fit(vec![Span::raw(" "), fold], &tr!("上一段 · {turns} 轮 · 不计入", "earlier · {turns} turns · not counted"), dim(), w)
        }
        _ => fit(vec![Span::raw("   "), Span::styled("── ", theme::faint())], &n.label, dim(), w),
    }
}

// ---------- Right side / details: paragraphs of text ----------

/// A paragraph: body wraps to the width, the first line prefixed with first, wrapped lines with rest (indent, a code block's
/// bar). One with right (a right-aligned part) or fill (text filling the middle, the ─ of a rule) takes one line only, clipped
/// if it doesn't fit
#[derive(Clone, Debug, Default)]
pub struct Para {
    pub first: Styled,
    pub rest: Styled,
    pub body: Styled,
    pub right: Styled,
    pub fill: Option<Seg>,
    /// Pad each wrapped line with spaces to the end, on this background (added / removed lines in an edit)
    pub pad_bg: Option<ratatui::style::Color>,
    /// Max wrapped lines (0 = no limit); extra ones are dropped and the last line ends in …: clipped by line count, so narrow and
    /// wide screens take the same height
    pub max: usize,
}

impl Para {
    pub fn new(body: Styled) -> Para {
        Para { body, ..Para::default() }
    }

    /// Every line gets the same prefix (indent / bar)
    pub fn under(pre: Styled, body: Styled) -> Para {
        Para { first: pre.clone(), rest: pre, body, ..Para::default() }
    }

    /// The first line gets first, wrapped lines get rest
    pub fn hung(first: Styled, rest: Styled, body: Styled) -> Para {
        Para { first, rest, body, ..Para::default() }
    }
}

/// Prefix a run of paragraphs with one more layer: the first paragraph's first line gets first, every other line gets rest
pub(crate) fn hang(ps: Vec<Para>, first: &Styled, rest: &Styled) -> Vec<Para> {
    ps.into_iter()
        .enumerate()
        .map(|(k, p)| Para {
            first: [if k == 0 { first } else { rest }.clone(), p.first].concat(),
            rest: [rest.clone(), p.rest].concat(),
            ..p
        })
        .collect()
}

fn width_of(s: &Styled) -> usize {
    s.iter().map(|x| x.text.width()).sum()
}

/// Clip to w cells, ending in … when clipped
fn cut(s: &Styled, w: usize) -> Styled {
    if width_of(s) <= w {
        return s.clone();
    }
    let (mut out, mut n) = (vec![], 0);
    for x in s {
        let mut text = String::new();
        for ch in x.text.chars() {
            let cw = ch.width().unwrap_or(0);
            if n + cw + 1 > w {
                break;
            }
            text.push(ch);
            n += cw;
        }
        let full = text.len() == x.text.len();
        out.push(Seg { text, ..x.clone() });
        if !full {
            break;
        }
    }
    if w > 0 {
        out.push(seg("…", theme::faint()));
    }
    out
}

/// Wrap to the width
pub fn wrap(ps: &[Para], w: usize) -> Vec<Styled> {
    wrap_clipped(ps, w).0
}

/// Like `wrap`, plus whether any paragraph lost lines to max (clipped at this width means that step has something to expand)
pub fn wrap_clipped(ps: &[Para], w: usize) -> (Vec<Styled>, bool) {
    let (mut out, mut clipped) = (vec![], false);
    for p in ps {
        if !p.right.is_empty() || p.fill.is_some() {
            // One line: the left is clipped to fit; the right-aligned part is dropped if it doesn't fit
            let room = w.saturating_sub(width_of(&p.first));
            let rw = width_of(&p.right);
            let right = rw > 0 && room >= rw + 8;
            let left = if right { room - rw - 1 } else { room };
            let body = cut(&p.body, left);
            let gap = left - width_of(&body);
            let mut line = [p.first.clone(), body].concat();
            match &p.fill {
                Some(f) => line.push(Seg { text: f.text.repeat(gap), ..f.clone() }),
                None if right => line.push(seg(" ".repeat(gap), Style::new())),
                None => {}
            }
            if right {
                line.push(seg(" ", Style::new()));
                line.extend(p.right.iter().cloned());
            }
            out.push(line);
            continue;
        }
        let room = w.saturating_sub(width_of(&p.first).max(width_of(&p.rest))).max(1);
        let mut rows = crate::ansi::wrap(std::slice::from_ref(&p.body), room);
        if p.max > 0 && rows.len() > p.max {
            rows.truncate(p.max);
            let last = rows.pop().unwrap_or_default();
            rows.push(cut(&[last, vec![seg("…", theme::faint())]].concat(), room));
            clipped = true;
        }
        for (k, l) in rows.into_iter().enumerate() {
            let mut line = if k == 0 { p.first.clone() } else { p.rest.clone() };
            line.extend(l);
            if let Some(bg) = p.pad_bg {
                line.push(seg(" ".repeat(w.saturating_sub(width_of(&line))), Style::new().bg(bg)));
            }
            out.push(line);
        }
    }
    (out, clipped)
}

pub(crate) fn seg(text: impl Into<String>, style: Style) -> Seg {
    Seg { text: text.into(), style, link: None }
}

pub(crate) fn minor(text: impl Into<String>) -> Para {
    Para::new(vec![seg(text, dim())])
}

pub(crate) fn blank() -> Para {
    Para::default()
}

/// A section heading: ── Reply ──
pub(crate) fn heading(text: &str) -> Para {
    Para::new(vec![seg("── ", theme::faint()), seg(text, Style::new().add_modifier(Modifier::BOLD)), seg(" ──", theme::faint())])
}

/// Max lines of tool output shown
pub const OUT_LINES: usize = 200;

/// Strip control characters from a line (\r, ESC color codes); Tabs become spaces
pub(crate) fn clean(l: &str) -> String {
    crate::ansi::strip(l).replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()
}

/// Add a whole text line by line; with limit = Some(n) only the first n lines, and the title gives the total
pub(crate) fn body(out: &mut Vec<Para>, title: &str, text: &str, limit: Option<usize>, st: Style) {
    body_hl(out, title, text, limit, st, None);
}

/// Like body, with code highlighted by hl (commands, JSON)
pub(crate) fn body_hl(out: &mut Vec<Para>, title: &str, text: &str, limit: Option<usize>, st: Style, mut hl: Option<crate::hl::Hl>) {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    match limit {
        Some(n) if total > n => {
            out.push(heading(&tr!("{title} · 前 {n} 行 / 共 {} 行", "{title} · first {n} of {} lines", thousands(total as u64))))
        }
        _ => out.push(heading(title)),
    }
    for l in lines.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let l = clean(l);
        out.push(Para::new(match hl.as_mut() {
            Some(h) => h.line(&l, st),
            None => vec![seg(l, st)],
        }));
    }
}

/// Lay Markdown out as paragraphs: headings, **bold**, `code`, lists, quotes, code blocks (with a bar), table separator rows
/// Code blocks are highlighted by their stated language (not inside a dim paragraph, which is dim throughout)
pub(crate) fn md(text: &str, st: Style) -> Vec<Para> {
    let mut out = vec![];
    let mut code = false;
    let mut hl: Option<crate::hl::Hl> = None;
    for raw in text.lines() {
        let line = clean(raw);
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            code = !code;
            hl = if code && st.fg.is_none() { crate::hl::Hl::for_lang(t.trim_start_matches(['`', '~'])) } else { None };
            continue;
        }
        if code {
            let body = match hl.as_mut() {
                Some(h) => h.line(&line, st),
                None => vec![seg(line.clone(), st)],
            };
            out.push(Para::under(vec![seg("│ ", theme::faint())], body));
            continue;
        }
        let pad = " ".repeat(line.len() - t.len());
        if let Some(h) = heading_text(t) {
            out.push(Para::new(inline(h, st.add_modifier(Modifier::BOLD))));
        } else if t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            out.push(Para::new(vec![seg(line.clone(), theme::faint())]));
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            out.push(Para::hung(vec![seg(format!("{pad}• "), theme::faint())], vec![seg(format!("{pad}  "), st)], inline(rest, st)));
        } else if let Some((num, rest)) = ordered(t) {
            let rest_pad = " ".repeat(pad.len() + num.len() + 1);
            out.push(Para::hung(vec![seg(format!("{pad}{num} "), dim())], vec![seg(rest_pad, st)], inline(rest, st)));
        } else if let Some(q) = t.strip_prefix('>') {
            out.push(Para::under(vec![seg(format!("{pad}│ "), theme::faint())], inline(q.trim_start(), dim())));
        } else {
            out.push(Para::new(inline(&line, st)));
        }
    }
    out
}

/// "## Title" → "Title"
fn heading_text(t: &str) -> Option<&str> {
    let h = t.trim_start_matches('#');
    let n = t.len() - h.len();
    ((1..=6).contains(&n) && h.starts_with(' ')).then(|| h.trim())
}

/// "12. Next step" → ("12.", "Next step")
fn ordered(t: &str) -> Option<(&str, &str)> {
    let d = t.find(|c: char| !c.is_ascii_digit())?;
    if d == 0 || d > 3 {
        return None;
    }
    let rest = t[d..].strip_prefix(". ").or_else(|| t[d..].strip_prefix(") "))?;
    Some((&t[..=d], rest))
}

/// Inline **bold** and `code` (marks removed)
fn inline(s: &str, st: Style) -> Styled {
    let mut out: Styled = vec![];
    let (mut bold, mut code) = (false, false);
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Styled, bold: bool, code: bool| {
        if !cur.is_empty() {
            let style = if code {
                theme::accent()
            } else if bold {
                st.add_modifier(Modifier::BOLD)
            } else {
                st
            };
            out.push(seg(std::mem::take(cur), style));
        }
    };
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            flush(&mut cur, &mut out, bold, code);
            code = !code;
            i += 1;
        } else if !code && c == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut cur, &mut out, bold, code);
            bold = !bold;
            i += 2;
        } else {
            cur.push(c);
            i += 1;
        }
    }
    flush(&mut cur, &mut out, bold, code);
    out
}

/// A node's details (the right column / the full-screen page)
pub fn detail(t: &Trace, i: usize, live: bool, now: i64) -> Vec<Para> {
    crate::trace_page::detail(t, i, live, now)
}

/// One turn in the conversation on the left
pub struct Chat {
    /// What you said (commands and notices as a summary)
    pub asked: Vec<Para>,
    /// What Claude said last (absent while running or if it said nothing)
    pub reply: Vec<Para>,
    /// A line of numbers: time, duration, calls, tools, cost (led by running / error / interrupted)
    pub foot: Para,
}

pub fn chat(t: &Trace, i: usize, live: bool, now: i64) -> Chat {
    crate::trace_page::chat(t, i, live, now)
}

/// One block on the right
pub struct Step {
    pub node: usize,
    pub paras: Vec<Para>,
    /// Can be expanded / collapsed with o (collapsed with something hidden, or already expanded)
    pub fold: bool,
}

/// Right side: a turn's intermediate steps, one block per node (the first block is the turn's head); open: which blocks are
/// expanded
#[cfg(test)]
pub fn steps(t: &Trace, turn: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Vec<Step> {
    crate::trace_page::steps(t, turn, live, now, open)
}

/// Which nodes the blocks on the right are
pub fn step_nodes(t: &Trace, turn: usize) -> Vec<usize> {
    crate::trace_page::flow_nodes(t, turn)
}

/// What the layout of the right side's k-th block depends on (unchanged means no relayout, see trace_page::step_sig)
pub fn step_sig(t: &Trace, turn: usize, list: &[usize], k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> u64 {
    crate::trace_page::step_sig(t, turn, list, k, live, now, open)
}

/// Lay out the right side's k-th block again on its own (to highlight it once it's visible)
pub fn step(t: &Trace, turn: usize, k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Option<Step> {
    crate::trace_page::step(t, turn, k, live, now, open)
}

/// What e shows in the editor: a real file (at a line), or text written to a temp file (ext = the temp file's extension, which
/// the editor highlights by)
#[derive(Debug, PartialEq)]
pub enum View {
    File { path: String, line: usize },
    Text { text: String, ext: String, line: usize },
}

pub fn view(t: &Trace, i: usize, live: bool, now: i64) -> Option<View> {
    crate::trace_page::view(t, i, live, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(ps: &[Para], w: usize) -> Vec<String> {
        wrap(ps, w).iter().map(crate::ansi::plain).collect()
    }

    #[test]
    fn markdown_loses_its_marks_and_keeps_its_shape() {
        let ps = md(
            "## 结论\n改成 **按宽度** 算 `width()`:\n- 第一条说得很长很长很长\n2. 第二步\n> 引用\n```\nlet w = 1;\n```\n| a | b |\n|---|---|",
            Style::new(),
        );
        let t = texts(&ps, 14);
        assert_eq!(
            t,
            [
                "结论",
                "改成 按宽度 算",
                "width():",
                "• 第一条说得很",
                "  长很长很长",
                "2. 第二步",
                "│ 引用",
                "│ let w = 1;",
                "| a | b |",
                "|---|---|"
            ]
        );
        // Bold is bold, code is the accent color, the marks all removed
        let line = &ps[1].body;
        assert!(line.iter().any(|s| s.text == "按宽度" && s.style.add_modifier.contains(Modifier::BOLD)));
        assert!(line.iter().any(|s| s.text == "width()" && s.style.fg == Some(theme::pal().accent)));
    }

    #[test]
    fn wrapped_lines_keep_their_hanging_indent() {
        let ps = hang(md("一二三四五六七八\n第二段", Style::new()), &vec![seg("你  ", Style::new())], &vec![seg("    ", Style::new())]);
        assert_eq!(texts(&ps, 12), ["你  一二三四", "    五六七八", "    第二段"]);
        // Even when too narrow, at least one cell per line, no infinite loop
        assert!(!texts(&ps, 2).is_empty());
    }
}

/// Claude records laid out on the `Z` page (reading them is tested in `aca_records::trace_claude`)
#[cfg(test)]
mod claude_records {
    use super::*;
    use crate::ansi::Styled;
    use crate::theme;
    use crate::trace::{Step, View};
    use crate::trace_page::{fenced, flow, step, steps, tool_item, view};
    use aca_records::agent::Agent;
    use aca_records::trace_claude::fixtures::{at, done, fixture, open, prompt, reply, with_subs};
    use serde_json::{Value, json};
    use std::io::Write;
    use unicode_width::UnicodeWidthStr;

    /// The text one tool on the right is laid out as (width 60)
    fn tool_text(t: &Trace, i: usize, open: bool) -> (Vec<String>, bool) {
        let (ps, fold) = tool_item(t, i, false, 0, open);
        (crate::trace::wrap(&ps, 60).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect(), fold)
    }

    #[test]
    fn subagents_hang_under_the_agent_that_started_them_and_their_money_counts_in_that_turn() {
        let (p, main, later) = with_subs("subs");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            writeln!(f, "{l}").unwrap();
        }
        let t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        let turn = t.roots[0];
        let call = t.nodes[turn].kids[0];
        let (ag1, ag3) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        let s1 = t.sub_of(ag1).expect("Explore 挂在 ag1 下");
        assert_eq!((s1.kind.as_str(), s1.desc.as_str(), s1.t.is_sub), ("Explore", "找 width", true));
        // The nested one goes into a1's own record, not the top level
        assert_eq!(t.subs.len(), 2);
        let ag2 = s1.t.nodes[s1.t.nodes[s1.t.roots[0]].kids[0]].kids[0];
        assert_eq!(s1.t.sub_of(ag2).map(|s| s.kind.as_str()), Some("general-purpose"));
        // The forked one starts with a 「上一段在父对话里」 (earlier: in the parent conversation) row
        let s3 = t.sub_of(ag3).unwrap();
        assert_eq!((s3.t.nodes[s3.t.roots[0]].kind, s3.t.nodes[s3.t.roots[0]].label.as_str()), (Kind::Mark, "上一段在父对话里"));
        // What the subagent receives is a task; finished and notified: not running
        assert_eq!(crate::trace::asked(&s1.t, s1.t.roots[0]), ("任务", "找出所有 width( 的调用".to_string()));
        assert!(!t.sub_live(ag1, true, ts_ms(&at(30_000))) && !t.sub_live(ag3, true, ts_ms(&at(30_000))));
        assert_eq!(t.sub_dur(ag1, false, 0), 9000 - 2050);
        // Cost: what $ reads from the four records, added up; a subagent's counts in the turn that started it
        let bill = |files: &[&str]| -> f64 {
            files
                .iter()
                .flat_map(|f| aca_records::usage::fixtures::claude_seen(&p.parent().unwrap().join(f)))
                .map(|(m, tok)| aca_records::usage::call_cost(&m, tok, false, false, 0).unwrap())
                .sum()
        };
        let subs = bill(&["s1/subagents/agent-a1.jsonl", "s1/subagents/agent-a2.jsonl", "s1/subagents/agent-a3.jsonl"]);
        let (turns, _, all) = t.total(false, 0);
        assert!((all.cost - bill(&["s1.jsonl"]) - subs).abs() < 1e-12, "{} vs {}", all.cost, bill(&["s1.jsonl"]) + subs);
        // First turn: its own two calls + the cost of three subagents (including the nested one)
        let s = t.sum(turn);
        assert_eq!((turns, s.calls, s.subs), (2, 2, 3));
        assert!((s.sub_cost - subs).abs() < 1e-12 && (s.cost + t.sum(t.roots[1]).cost - all.cost).abs() < 1e-12);
        // Right side: the Agent step shows the subagent's numbers, with the first 3 lines of what it said last below
        let plain = |ps: &[crate::trace::Para], w: usize| {
            crate::trace::wrap(ps, w).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>()
        };
        let (ps, fold) = tool_item(&t, ag1, false, 0, false);
        let text = plain(&ps, 80);
        assert!(fold && text[0].starts_with("▸ Agent Explore · 找 width") && text[0].contains("3 调用 · $"), "{text:#?}");
        assert_eq!(&text[1..], ["  找到 4 处:", "  src/bill.rs:12", "  src/ui.rs:88", "  … 还有 2 行"]);
        // The number of subagents is in the turn's heading in the right pane; under the left pane only time, duration, cost
        let head = plain(&steps(&t, turn, false, 0, &|_| false)[0].paras, 80);
        assert!(head[0].contains(" · 3 子代理 · "), "{head:?}");
        let c = crate::trace::chat(&t, turn, false, 0);
        assert!(!crate::ansi::plain(&c.foot.body).contains("子代理"), "{:?}", crate::ansi::plain(&c.foot.body));
        // e: the subagent's steps are written in too
        let Some(View::Text { text, .. }) = view(&t, ag1, false, 0) else { panic!() };
        assert!(text.contains("## 任务\n\n找出所有 width( 的调用") && text.contains("rg -n 'width\\('"), "{text}");
        drop(main);
    }

    #[test]
    fn a_background_subagent_runs_until_it_says_its_last_word_and_later_ones_are_found_as_the_record_grows() {
        let (p, main, later) = with_subs("subs-live");
        // a1 hasn't finished writing (stopped while running a command), and no notification either
        let a1 = p.with_extension("").join("subagents/agent-a1.jsonl");
        let full = std::fs::read_to_string(&a1).unwrap();
        let cut: String = full.lines().take(4).map(|l| format!("{l}\n")).collect();
        std::fs::write(&a1, &cut).unwrap();
        // a3's meta is written a bit later
        let a3 = p.with_extension("").join("subagents/agent-a3.meta.json");
        let meta3 = std::fs::read_to_string(&a3).unwrap();
        std::fs::remove_file(&a3).unwrap();
        let mut t = Trace::open_born(p.clone(), Agent::Claude, 0).unwrap();
        let call = t.nodes[t.roots[0]].kids[0];
        let (ag1, ag3) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert!(t.sub_live(ag1, true, ts_ms(&at(6000))) && !t.sub_live(ag1, false, ts_ms(&at(6000))));
        assert!(t.sub_of(ag3).is_none());
        let busy = crate::trace::chat(&t, t.roots[0], true, ts_ms(&at(6000)));
        assert!(crate::ansi::plain(&busy.foot.body).starts_with("● 子代理运行中 · "));
        // Writing continues: a1 finishes, a3 appears, the notification arrives
        std::fs::write(&a1, &full).unwrap();
        std::fs::write(&a3, meta3).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        for l in &later {
            writeln!(f, "{l}").unwrap();
        }
        assert!(t.tail().unwrap());
        assert!(!t.sub_live(ag1, true, ts_ms(&at(30_000))) && t.sub_of(ag3).is_some());
        assert_eq!(t.sub_of(ag1).unwrap().t.nodes.iter().filter(|n| n.kind == Kind::Call).count(), 3);
        drop(main);
    }

    #[test]
    fn huge_images_are_skipped_not_expanded_and_the_detail_reads_them_back() {
        let big = "A".repeat(2 << 20);
        let t = open(
            "detail",
            &[
                prompt("u1", None, 0, "看图"),
                reply(
                    "a1",
                    "u1",
                    1000,
                    "m1",
                    json!({"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/p/x.png"}}),
                    5,
                    Some("tool_use"),
                ),
                json!({"type": "user", "uuid": "r1", "parentUuid": "a1", "timestamp": at(1500), "sessionId": "s1",
                       "toolUseResult": {"type": "image", "file": {"base64": big}},
                       "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1",
                                   "content": [{"type": "image", "source": {"type": "base64", "data": big}}]}]}}),
            ],
        );
        let tool = t.nodes[t.nodes[t.roots[0]].kids[0]].kids[0];
        let text: Vec<String> = crate::trace::wrap(&crate::trace::detail(&t, tool, false, 0), 200).iter().map(crate::ansi::plain).collect();
        assert!(text.iter().any(|l| l == "[图片]"), "{text:?}");
        assert!(text.iter().any(|l| l.contains("/p/x.png")), "{text:?}");
        let call = t.nodes[t.roots[0]].kids[0];
        let text: Vec<String> = crate::trace::wrap(&crate::trace::detail(&t, call, false, 0), 200).iter().map(crate::ansi::plain).collect();
        assert!(text.iter().any(|l| l.contains(" · 缓存写 100 · ")), "{text:?}");
    }

    #[test]
    fn a_tool_folds_to_a_peek_and_o_opens_all_of_it() {
        let (t, _) = fixture("fold");
        let tool = |name: &str| (0..t.nodes.len()).find(|&i| t.nodes[i].name == name).unwrap();
        // Folded: the heading row has the name and relative path, lines changed on the right; below only the changed lines
        // (with line numbers), no context
        let (text, fold) = tool_text(&t, tool("Edit"), false);
        assert!(fold);
        assert!(text[0].starts_with("▸ Edit  src/bill.rs ") && text[0].ends_with("+2 -2"), "{text:?}");
        assert_eq!(
            text[1..],
            ["  12 -     let w = s.len();", "  12 +     let w = s.width();", "  41 -     pad(w)", "  41 +     pad(w.max(1))"]
        );
        // Coloring: code colored as .rs, added lines on a pale green background to the end of the line, removed on pale red,
        // line numbers without background
        let p = theme::pal();
        let lines = crate::trace::wrap(&tool_item(&t, tool("Edit"), false, 0, false).0, 60);
        let seg_of = |l: &Styled, w: &str| l.iter().find(|s| s.text.contains(w)).map(|s| s.style).unwrap();
        assert_eq!(seg_of(&lines[2], "let").fg, Some(p.syn_kw));
        assert_eq!(seg_of(&lines[2], "width").bg, Some(p.add_bg));
        assert_eq!(seg_of(&lines[1], "len").bg, Some(p.del_bg));
        assert_eq!(seg_of(&lines[2], "12").bg, None);
        assert_eq!(lines[2].last().unwrap().style.bg, Some(p.add_bg));
        assert_eq!(crate::ansi::plain(&lines[2]).width(), 60);
        // Unfolded: with context, a dotted line between two hunks
        let (text, _) = tool_text(&t, tool("Edit"), true);
        assert!(text[0].starts_with("▾ Edit"), "{text:?}");
        assert_eq!(
            text[1..6],
            [
                "  11   fn col(s: &str) -> usize {",
                "  12 -     let w = s.len();",
                "  12 +     let w = s.width();",
                "  13       w",
                "  ┈┈┈┈┈"
            ]
        );
        // Read folded shows nothing, lines read on the right; unfolded is the file content with line numbers, without the
        // trailing hint
        let (text, fold) = tool_text(&t, tool("Read"), false);
        assert!(fold && text.len() == 1 && text[0].starts_with("▸ Read  src/usage.rs") && text[0].ends_with("5 行"), "{text:?}");
        let (text, _) = tool_text(&t, tool("Read"), true);
        assert_eq!(text[1..], ["  1  use std::io;", "  2", "  3  fn main() {", "  4      run();", "  5  }"]);
        let lines = crate::trace::wrap(&tool_item(&t, tool("Read"), false, 0, true).0, 60);
        assert_eq!(seg_of(&lines[1], "use").fg, Some(p.syn_kw));
        // Bash: folded shows the first 3 lines of output; unfolded writes the whole command first, then all the output
        let (text, _) = tool_text(&t, tool("Bash"), false);
        assert_eq!(text[1..], ["  running 220 tests", "  test bill::cols ... ok", "  test ui::wide ... ok", "  … 还有 3 行"]);
        let (text, _) = tool_text(&t, tool("Bash"), true);
        assert_eq!(text[1], "  $ cargo test --release");
        let lines = crate::trace::wrap(&tool_item(&t, tool("Bash"), false, 0, true).0, 60);
        assert_ne!(seg_of(&lines[1], "cargo").fg, None, "命令上色:{:?}", lines[1]);
        assert_eq!(text.last().unwrap(), "  test result: ok. 220 passed");
        // One that shows everything can't be o'd
        let (text, fold) = tool_text(&t, tool("Grep"), false);
        assert!(fold && text.last().unwrap() == "  … 还有 1 行", "{text:?}");
    }

    #[test]
    fn coloring_never_changes_how_many_lines_a_block_takes() {
        // The interface measures heights uncolored first and colors once visible: both must be the same height
        let (t, _) = fixture("same-height");
        for &turn in t.roots.iter().filter(|&&r| t.nodes[r].kind == Kind::Turn) {
            for open in [false, true] {
                for w in [30, 43, 80] {
                    let heights = |ss: Vec<Step>| ss.iter().map(|s| crate::trace::wrap(&s.paras, w).len()).collect::<Vec<_>>();
                    let plain = heights(crate::hl::plain(|| steps(&t, turn, false, 0, &|_| open)));
                    assert_eq!(plain, heights(steps(&t, turn, false, 0, &|_| open)), "第 {turn} 轮 open={open} w={w}");
                    // Laying out block by block gives the same as laying out all at once
                    let one: Vec<Step> = (0..plain.len()).map(|k| step(&t, turn, k, false, 0, &|_| open).unwrap()).collect();
                    assert_eq!(plain, heights(one));
                }
            }
        }
    }

    #[test]
    fn e_shows_the_file_itself_or_a_markdown_copy_with_everything() {
        let (t, _) = fixture("view");
        let node = |name: &str| (0..t.nodes.len()).find(|&i| t.nodes[i].name == name).unwrap();
        let md = |i: usize| match view(&t, i, false, 0) {
            Some(View::Text { text, ext, line: 1 }) if ext == "md" => text,
            x => panic!("{x:?}"),
        };
        // The file is gone: changes written as a diff code block, each hunk with the line it starts at
        let edit = md(node("Edit"));
        for want in
            ["# Edit · src/bill.rs", "```diff", "@@ 第 11 行 @@", "-    let w = s.len();", "+    let w = s.width();", "@@ 第 40 行 @@"]
        {
            assert!(edit.contains(want), "缺「{want}」:{edit}");
        }
        // Content read is written in the file's language, without line numbers or the trailing hint
        let read = md(node("Read"));
        assert!(read.contains("```rs\nuse std::io;\n\nfn main() {") && !read.contains("system-reminder"), "{read}");
        // A turn: what you said, each call (numbers, thinking quoted, what was said), each tool's command and all its output
        let turn = md(t.roots.iter().copied().find(|&r| t.nodes[r].label == "看看两栏为什么对不齐").unwrap());
        for want in [
            "# 第 1 轮 · ",
            "## 你\n\n看看两栏为什么对不齐",
            "## 第 1 轮 · 第 1 次调用 · 4.0s",
            "> 先确认是不是宽字符的问题。",
            "### Grep · width(",
            "src/ui.rs:90:",
            "### Bash · cargo test --release · 8.8s",
            "```sh\n# run it\ncargo test --release",
            "test result: ok. 220 passed",
            "- 测试全过了",
        ] {
            assert!(turn.contains(want), "缺「{want}」:{turn}");
        }
        // The compaction row has nothing to view
        assert_eq!(view(&t, t.roots[0], false, 0), None);
        // Output containing ``` gets an outer fence one longer
        assert_eq!(fenced("text", "a\n```\nb"), "````text\na\n```\nb\n````\n\n");
        // The file is still there: view the file itself, at the line read / the first changed line
        let dir = std::env::temp_dir().join(format!("aca-trace-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.rs");
        std::fs::write(&f, "fn a() {}\n").unwrap();
        let fp = f.to_string_lossy().to_string();
        let t = open(
            "view-real",
            &[
                prompt("u1", None, 0, "看"),
                reply(
                    "a1",
                    "u1",
                    1000,
                    "m1",
                    json!({"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": fp, "offset": 30}}),
                    5,
                    None,
                ),
                reply(
                    "a2",
                    "a1",
                    1100,
                    "m1",
                    json!({"type": "tool_use", "id": "t2", "name": "Edit", "input": {"file_path": fp, "old_string": "a", "new_string": "b"}}),
                    9,
                    Some("tool_use"),
                ),
                done("r1", "a1", 1200, "t1", false),
                json!({"type": "user", "uuid": "r2", "parentUuid": "a2", "timestamp": at(1300), "sessionId": "s1",
                       "toolUseResult": {"structuredPatch": [{"oldStart": 7, "newStart": 7, "lines": [" x", "-a", "+b"]}]},
                       "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t2", "content": "ok"}]}}),
            ],
        );
        let call = t.nodes[t.roots[0]].kids[0];
        let (read, edit) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert_eq!(view(&t, read, false, 0), Some(View::File { path: fp.clone(), line: 30 }));
        assert_eq!(view(&t, edit, false, 0), Some(View::File { path: fp, line: 8 }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_call_names_its_model_only_where_the_model_changes() {
        let with = |mut v: Value, m: &str| {
            v["message"]["model"] = json!(m);
            v
        };
        let say = |x: &str| json!({"type": "text", "text": x});
        let t = open(
            "models",
            &[
                prompt("u1", None, 0, "一"),
                reply("a1", "u1", 1000, "m1", say("一"), 5, Some("end_turn")),
                prompt("u2", Some("a1"), 2000, "二"),
                with(reply("a2", "u2", 3000, "m2", say("二"), 5, Some("end_turn")), "claude-haiku-4-5"),
                prompt("u3", Some("a2"), 4000, "三"),
                with(reply("a3", "u3", 5000, "m3", say("三"), 5, Some("end_turn")), "claude-haiku-4-5"),
                prompt("u4", Some("a3"), 6000, "四"),
                reply("a4", "u4", 7000, "m4", say("四"), 5, Some("end_turn")),
                prompt("u5", Some("a4"), 8000, "五"),
                reply("a5", "u5", 9000, "m5", say("五"), 5, Some("end_turn")),
            ],
        );
        let rules: Vec<String> = t
            .roots
            .iter()
            .filter(|&&r| t.nodes[r].kind == Kind::Turn)
            .map(|&r| {
                let ls =
                    crate::trace::wrap(&flow(&t, r, false, 0, false, &|_| false).into_iter().flat_map(|s| s.paras).collect::<Vec<_>>(), 80);
                ls.iter().map(crate::ansi::plain).find(|l| l.starts_with("── ")).unwrap().trim_end_matches(['─', ' ']).to_string()
            })
            .collect();
        // The first is compared with the most used (opus); switching to haiku writes it once, switching back writes it again
        let model = |l: &String| l.split(" · ").nth(3).map(str::to_string);
        assert_eq!(
            rules.iter().map(model).collect::<Vec<_>>(),
            [None, Some("haiku-4-5".into()), None, Some("opus-5-5".into()), None],
            "{rules:#?}"
        );
    }

    #[test]
    fn a_call_keeps_the_first_line_of_what_it_thought_and_said_without_marks() {
        let t = open(
            "said",
            &[
                prompt("u1", None, 0, "问"),
                reply("a1", "u1", 1000, "m1", json!({"type": "thinking", "thinking": "\n先想想\n再说"}), 5, None),
                reply("a2", "a1", 1100, "m1", json!({"type": "text", "text": "## **结论**:用 `width()`"}), 9, Some("end_turn")),
            ],
        );
        let call = &t.nodes[t.nodes[t.roots[0]].kids[0]];
        assert_eq!((call.thought.as_str(), call.label.as_str()), ("先想想", "结论:用 width()"));
        // In the conversation: numbers on the separator, thinking flattened into one paragraph (lines without
        // sentence-ending punctuation followed by ·), what was said in full; unfolded, thinking keeps its original lines
        let text = |open: bool| -> Vec<String> {
            flow(&t, t.roots[0], false, 0, false, &|_| open)
                .iter()
                .flat_map(|s| crate::trace::wrap(&s.paras, 80))
                .map(|l| crate::ansi::plain(&l))
                .collect()
        };
        let closed = text(false);
        assert!(closed[0].starts_with("你  问"), "{closed:?}");
        let rule = closed.iter().find(|l| l.starts_with("── ")).expect("分隔线");
        assert!(rule.starts_with("── 1.1s · 1.1k · $0.00 ─") && rule.ends_with("──") && rule.width() == 80, "{rule}");
        assert!(closed.iter().any(|l| l == "思考 先想想 · 再说"), "{closed:?}");
        assert!(closed.iter().any(|l| l == "结论:用 width()"), "{closed:?}");
        let open = text(true);
        assert!(open.windows(2).any(|w| w[0] == "思考 先想想" && w[1] == "     再说"), "{open:?}");
    }

    #[test]
    fn a_long_thought_folds_to_five_lines_wherever_it_is_read_and_a_short_one_shows_whole() {
        let long = "先确认是不是宽字符的问题。".repeat(12);
        let t = open(
            "thought",
            &[
                prompt("u1", None, 0, "问"),
                reply("a1", "u1", 1000, "m1", json!({"type": "thinking", "thinking": long}), 5, None),
                reply("a2", "a1", 1100, "m2", json!({"type": "thinking", "thinking": "**看一眼**\n\n就改"}), 5, None),
                reply("a3", "a2", 1200, "m3", json!({"type": "text", "text": "好了"}), 9, Some("end_turn")),
            ],
        );
        let turn = t.roots[0];
        let call = |k: usize, open: bool| step(&t, turn, k, false, 0, &|_| open).unwrap();
        let plain = |ls: Vec<crate::ansi::Styled>| ls.iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>();
        // Both phone (39 columns) and desktop (57 columns) show only 5 lines, ending in …; this step can be o'd
        for w in [39, 57] {
            let (lines, clipped) = crate::trace::wrap_clipped(&call(1, false).paras, w);
            let thought: Vec<String> = plain(lines).into_iter().skip(1).filter(|l| !l.is_empty()).collect();
            assert_eq!(thought.len(), 5, "{w}: {thought:#?}");
            assert!(thought[0].starts_with("思考 先确认") && thought[4].ends_with('…') && clipped, "{w}: {thought:#?}");
            assert!(thought.iter().all(|l| l.width() <= w), "{w}: {thought:#?}");
        }
        // Unfolded isn't cut
        let (lines, clipped) = crate::trace::wrap_clipped(&call(1, true).paras, 39);
        assert!(lines.len() > 6 && !clipped);
        // A short one shows whole, the heading followed by ·, nothing to unfold
        let (lines, clipped) = crate::trace::wrap_clipped(&call(2, false).paras, 39);
        assert_eq!((plain(lines)[1].as_str(), clipped), ("思考 看一眼 · 就改", false));
    }
}

#[cfg(test)]
mod probe {
    use crate::trace::{Kind, Trace};
    use aca_records::agent::Agent;
    use std::path::PathBuf;

    /// Check subagents against this machine's real records (prints numbers only): ACA_TRACE_SUBS = a list of record files,
    /// one per line
    #[test]
    #[ignore]
    fn probe_subs() {
        let Ok(list) = std::env::var("ACA_TRACE_SUBS") else { return };
        fn count(t: &crate::trace::Trace) -> usize {
            t.subs.iter().map(|s| 1 + count(&s.t)).sum()
        }
        fn files(t: &crate::trace::Trace, out: &mut Vec<PathBuf>) {
            for s in &t.subs {
                out.push(s.t.path.clone());
                files(&s.t, out);
            }
        }
        fn copied(t: &crate::trace::Trace) -> (usize, usize) {
            let mut c = t
                .roots
                .iter()
                .filter(|&&r| t.nodes[r].kind == Kind::Copied)
                .map(|&r| t.nodes[r].kids.iter().filter(|&&k| t.nodes[k].kind == Kind::Turn).count())
                .sum::<usize>();
            let mut own = t.roots.iter().filter(|&&r| t.nodes[r].kind == Kind::Turn).count();
            for s in &t.subs {
                let (a, b) = copied(&s.t);
                c += a;
                own += b;
            }
            (c, own)
        }
        let price = |seen: Vec<(String, [u64; 5])>| -> f64 {
            seen.iter().filter_map(|(m, tok)| aca_records::usage::call_cost(m, *tok, false, false, 0)).sum()
        };
        for (k, p) in std::fs::read_to_string(list).unwrap().lines().enumerate() {
            let codex = p.contains("rollout-");
            let agent = if codex { Agent::Codex } else { Agent::Claude };
            let t0 = std::time::Instant::now();
            let Ok(t) = Trace::open(p.into(), agent) else { continue };
            let el = t0.elapsed();
            let mut fs = vec![];
            files(&t, &mut fs);
            let (s, rolled) = (t.total(false, 0).2, t.nodes.iter().filter(|n| n.rolled).count());
            let (cp, own) = copied(&t);
            if codex {
                // The parent thread's own $ + the child threads' cost in the record view (the child threads' $ also counts
                // copied lines, listed separately)
                let mine = price(aca_records::usage::fixtures::codex_seen(std::path::Path::new(p)));
                let kids_bill: f64 = fs.iter().map(|f| price(aca_records::usage::fixtures::codex_seen(f))).sum();
                let expected = std::fs::read_to_string(p).unwrap_or_default().matches("\"spawn_agent\"").count();
                println!(
                    "#{k} codex direct={} subs={} spawn_mentions={expected} want={} copied_turns={cp} own_turns={own} rolled={rolled} cost={:.4} own_bill={mine:.4} subs_cost={:.4} subs_bill_with_copies={kids_bill:.4} {:.0}ms",
                    t.subs.len(),
                    count(&t),
                    t.want.len(),
                    s.cost,
                    s.sub_cost,
                    el.as_secs_f64() * 1000.0
                );
            } else {
                let metas = std::fs::read_dir(std::path::Path::new(p).with_extension("").join("subagents"))
                    .map_or(0, |d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json")).count());
                let bill: f64 = price(aca_records::usage::fixtures::claude_seen(std::path::Path::new(p)))
                    + fs.iter().map(|f| price(aca_records::usage::fixtures::claude_seen(f))).sum::<f64>();
                // Not finished writing and not notified (however long it's been quiet)
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
                let live = (0..t.nodes.len()).filter(|&i| t.sub_live(i, true, i64::MIN / 2)).count();
                for i in (0..t.nodes.len()).filter(|&i| t.sub_live(i, true, i64::MIN / 2)) {
                    let sub = t.sub_of(i).unwrap();
                    let last = sub.t.roots.iter().map(|&r| sub.t.last_at(r)).max().unwrap_or(0);
                    let lc = sub.t.nodes.iter().rev().find(|n| n.kind == Kind::Call);
                    let open = sub.t.nodes.iter().filter(|n| n.kind == Kind::Tool && n.t1 == 0).count();
                    let fails = sub.t.nodes.iter().filter(|n| n.kind == Kind::Fail).count();
                    let parent_age = (now - t.roots.iter().map(|&r| t.last_at(r)).max().unwrap_or(0)) / 60_000;
                    println!(
                        "   live? age={}min parent_age={parent_age}min stop={:?} open_tools={open} fails={fails} tool_done={}",
                        (now - last) / 60_000,
                        lc.map(|c| c.stop.as_str()),
                        t.nodes[i].t1 > 0
                    );
                }
                // What drawing one screen computes: the whole record's total, each turn's total, each turn's conversation
                // block on the left
                let t1 = std::time::Instant::now();
                let _ = t.total(false, 0);
                for &r in &t.roots {
                    let _ = t.sum(r);
                }
                let sums = t1.elapsed();
                let t2 = std::time::Instant::now();
                for &r in t.roots.iter().filter(|&&r| t.nodes[r].kind == Kind::Turn) {
                    let _ = crate::trace::chat(&t, r, false, 0);
                }
                println!(
                    "#{k} claude subs={}/{metas} live_if_running={live} copied_turns={cp} cost={:.4} bill={bill:.4} diff={:.6} open={:.0}ms sums={:.2}ms chat_all={:.0}ms",
                    count(&t),
                    s.cost,
                    s.cost - bill,
                    el.as_secs_f64() * 1000.0,
                    sums.as_secs_f64() * 1000.0,
                    t2.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
    }

    /// Run by hand only: ACA_TRACE_PROBE=<record file> cargo test --release probe -- --ignored --nocapture. Prints only counts
    /// and timings
    #[test]
    #[ignore]
    fn probe_real_file() {
        let Ok(p) = std::env::var("ACA_TRACE_PROBE") else { return };
        let codex = p.contains("rollout-");
        let agent = if codex { aca_records::agent::Agent::Codex } else { aca_records::agent::Agent::Claude };
        let t0 = std::time::Instant::now();
        let t = crate::trace::Trace::open(p.clone().into(), agent).unwrap();
        let el = t0.elapsed();
        if codex {
            // Tools counted by name; what exec recognized; turns with no prompt; cost compared with $ computed from the same file
            let mut names = std::collections::BTreeMap::new();
            for n in t.nodes.iter().filter(|n| n.kind == crate::trace::Kind::Tool) {
                *names.entry(n.name.clone()).or_insert(0usize) += 1;
            }
            let no_prompt = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn && t.nodes[r].src.is_empty()).count();
            let steer = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn && t.nodes[r].src.len() > 1).count();
            let bill: f64 = aca_records::usage::fixtures::codex_seen(std::path::Path::new(&p))
                .iter()
                .filter_map(|(m, tok)| aca_records::usage::call_cost(m, *tok, false, false, 0))
                .sum();
            let calls = aca_records::usage::fixtures::codex_seen(std::path::Path::new(&p)).len();
            let s = t.total(false, 0).2;
            println!(
                "codex ver={:?} tools={names:?} no_prompt_turns={no_prompt} steer_turns={steer} calls={} bill_calls={calls} cost={:.4} bill={bill:.4}",
                t.cx.ver, s.calls, s.cost
            );
            // How long reading back and tidying each tool takes (unfolding on the right and the detail both read it)
            let t4 = std::time::Instant::now();
            let (mut with_cmd, mut with_edit, mut js, mut empty) = (0, 0, 0, 0);
            for i in (0..t.nodes.len()).filter(|&i| t.nodes[i].kind == crate::trace::Kind::Tool && t.nodes[i].name != "stdin") {
                let io = crate::trace::io(&t, i);
                match () {
                    _ if !io.cmds.is_empty() => with_cmd += 1,
                    _ if !io.edits.is_empty() => with_edit += 1,
                    _ if io.input.as_ref().is_some_and(|x| x.0 == "js") => js += 1,
                    _ => empty += 1,
                }
            }
            println!("io: cmd={with_cmd} edit={with_edit} js={js} other={empty} in {:?}", t4.elapsed());
        }
        let (turns, dur, s) = t.total(false, 0);
        let copied = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Copied).count();
        let marks = t.roots.iter().filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Mark).count();
        let fails = t.nodes.iter().filter(|n| n.kind == crate::trace::Kind::Fail).count();
        let open_tools = t.nodes.iter().filter(|n| n.kind == crate::trace::Kind::Tool && n.t1 == 0).count();
        let exact = t.roots.iter().filter(|&&r| t.nodes[r].exact).count();
        let mut durs: Vec<(i64, bool, usize, usize)> = t
            .roots
            .iter()
            .filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn)
            .map(|&r| (t.dur(r, false, 0) / 1000, t.nodes[r].exact, t.nodes[r].kids.len(), t.sum(r).tools))
            .collect();
        durs.sort_by_key(|d| -d.0);
        let ex: i64 = durs.iter().filter(|d| d.1).map(|d| d.0).sum();
        let nx: i64 = durs.iter().filter(|d| !d.1).map(|d| d.0).sum();
        println!("top turns (secs, exact, kids, tools): {:?}  exact_sum={ex}s inexact_sum={nx}s", &durs[..durs.len().min(8)]);
        // How long laying out and wrapping the biggest turn (most nodes) as a conversation takes
        let big = t
            .roots
            .iter()
            .copied()
            .filter(|&r| t.nodes[r].kind == crate::trace::Kind::Turn)
            .max_by_key(|&r| t.nodes[r].kids.len())
            .unwrap();
        // What the interface does: lay out the whole turn and the whole conversation uncolored (to measure heights), then
        // color one visible screen
        let t1 = std::time::Instant::now();
        let items = crate::hl::plain(|| crate::trace::steps(&t, big, false, 0, &|_| false));
        let lines: usize = items.iter().map(|s| crate::trace::wrap(&s.paras, 80).len()).sum();
        let el_flow = t1.elapsed();
        let t3 = std::time::Instant::now();
        let all: usize = crate::hl::plain(|| crate::trace::steps(&t, big, false, 0, &|_| true))
            .iter()
            .map(|s| crate::trace::wrap(&s.paras, 80).len())
            .sum();
        let el_all = t3.elapsed();
        let t2 = std::time::Instant::now();
        let chat: usize = crate::hl::plain(|| {
            t.roots
                .iter()
                .filter(|&&r| t.nodes[r].kind == crate::trace::Kind::Turn)
                .map(|&r| {
                    let c = crate::trace::chat(&t, r, false, 0);
                    crate::trace::wrap(&c.asked, 60).len() + crate::trace::wrap(&c.reply, 60).len()
                })
                .sum()
        });
        let el_chat = t2.elapsed();
        // Color one screen (50 lines): the blocks of the fully unfolded turn from the top up to 50 lines; the first time has
        // to compile the syntaxes used
        let screen = |open: bool| {
            let t5 = std::time::Instant::now();
            let (mut k, mut n) = (0, 0);
            while n < 50 {
                let Some(s) = crate::trace::step(&t, big, k, false, 0, &|_| open) else { break };
                n += crate::trace::wrap(&s.paras, 80).len();
                k += 1;
            }
            t5.elapsed()
        };
        let (cold, closed, open) = (screen(false), screen(false), crate::hl::upto(50, || screen(true)));
        println!(
            "biggest turn: kids={} items={} plain: steps={lines} lines {el_flow:?}, all open={all} lines {el_all:?}; whole chat lines={chat} in {el_chat:?}; color one screen: first {cold:?}, again {closed:?}, all open {open:?}",
            t.nodes[big].kids.len(),
            items.len()
        );
        println!(
            "bytes={} nodes={} turns={turns} exact_turns={exact} calls={} tools={} open_tools={open_tools} fails={fails} marks={marks} copied_groups={copied} models={} work={}s cost={:.2} parse={:?}",
            t.off,
            t.nodes.len(),
            s.calls,
            s.tools,
            t.models.len() - 1,
            dur / 1000,
            s.cost,
            el
        );
    }
}

/// Codex records laid out on the `Z` page (reading them is tested in `aca_records::trace_codex`)
#[cfg(test)]
mod codex_records {
    use super::*;
    use aca_records::agent::Agent;
    use aca_records::trace_codex::fixtures::{at, ev, family, file, fixture, fixture_lines, meta, row, say, tokens};
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn the_chat_steps_and_detail_read_back_what_codex_said_and_ran() {
        let (t, _) = fixture("page");
        let turn = t.roots[0];
        let plain = |ps: &[crate::trace::Para], w: usize| {
            crate::trace::wrap(ps, w).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>()
        };
        let c = crate::trace::chat(&t, turn, false, 0);
        assert_eq!(plain(&c.asked, 60), ["你  跑一下测试", "    顺便修掉", "    [图片]"]);
        assert_eq!(plain(&c.reply, 60)[0], "Codex  改好了:b 的断言写错了。");
        // Right side: a folded exec shows the start of its output and the command count at the right; changing files shows
        // the changed lines
        let steps: Vec<String> = crate::trace::steps(&t, turn, false, 0, &|_| false).iter().flat_map(|s| plain(&s.paras, 70)).collect();
        assert!(steps.iter().any(|l| l.starts_with("▸ Shell cargo test --release") && l.ends_with("2 条命令 · 1.5s")), "{steps:#?}");
        assert!(steps.iter().any(|l| l == "  running 3 tests"), "{steps:#?}");
        assert!(steps.iter().any(|l| l.starts_with("▸ Patch src/b.rs") && l.ends_with("+1 -1")), "{steps:#?}");
        assert!(steps.iter().any(|l| l == "  8 +     assert_eq!(x, 2);"), "{steps:#?}");
        assert!(steps.iter().any(|l| l == "思考 修 b · 改断言"), "{steps:#?}");
        // Unfolded: both commands written out
        let open: Vec<String> = crate::trace::steps(&t, turn, false, 0, &|_| true).iter().flat_map(|s| plain(&s.paras, 70)).collect();
        assert!(open.windows(2).any(|w| w[0] == "  $ cargo test --release" && w[1] == "  $ sleep 60"), "{open:#?}");
        // Details: commands, exit code; e writes Markdown
        let e1 = t.nodes[t.nodes[turn].kids[0]].kids[0];
        let d = plain(&crate::trace::detail(&t, e1, false, 0), 80);
        assert!(d.iter().any(|l| l == "cargo test --release") && d.iter().any(|l| l == "sleep 60"), "{d:#?}");
        let Some(crate::trace::View::Text { text, .. }) = crate::trace::view(&t, turn, false, 0) else { panic!() };
        for want in [
            "## 你\n\n跑一下测试",
            "### Shell · cargo test --release",
            "```sh\ncargo test --release\nsleep 60\n```",
            "```diff\n@@ 第 7 行 @@",
        ] {
            assert!(text.contains(want), "缺「{want}」:{text}");
        }
    }

    #[test]
    fn a_block_whose_signature_stays_the_same_looks_the_same_after_the_record_grows() {
        // A running session: the log read up to half of the first turn, then read on. Blocks whose signature didn't change are reused (the
        // UI doesn't re-lay out) and must be identical to re-laid-out ones
        let all = fixture_lines();
        let p = file("grow", &all[..13]);
        let mut t = Trace::open_born(p.clone(), Agent::Codex, 0).unwrap();
        let turn = t.roots[0];
        let now = ts_ms(&at(30_000));
        let snap = |t: &Trace, open: bool| -> Vec<(usize, u64, Vec<String>)> {
            let list = crate::trace::step_nodes(t, turn);
            let open = |_| open;
            (0..list.len())
                .map(|k| {
                    let s = crate::trace::step(t, turn, k, true, now, &open).unwrap();
                    (
                        list[k],
                        crate::trace::step_sig(t, turn, &list, k, true, now, &open),
                        crate::trace::wrap(&s.paras, 70).iter().map(crate::ansi::plain).collect(),
                    )
                })
                .collect()
        };
        for open in [false, true] {
            let mut before = snap(&t, open);
            let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
            for l in &all[13..22] {
                let (ts, k, pl) = (&l["timestamp"], &l["type"], &l["payload"]);
                writeln!(f, r#"{{"timestamp":{ts},"type":{k},"payload":{pl}}}"#).unwrap();
            }
            assert!(t.tail().unwrap());
            let after = snap(&t, open);
            let mut kept = 0;
            for (node, sig, text) in &after {
                if let Some(b) = before.iter().find(|b| b.0 == *node).filter(|b| b.1 == *sig) {
                    assert_eq!(&b.2, text, "签名没变、样子变了:节点 {node}");
                    kept += 1;
                }
            }
            // The head (whole-turn total), the exec whose background command finished, and the call that's no longer the last
            // get re-laid out; the first call stays as it was
            let c1 = t.nodes[turn].kids[0];
            assert!(before.iter().chain(&after).filter(|b| b.0 == c1).map(|b| b.1).collect::<std::collections::HashSet<_>>().len() == 1);
            assert!(kept >= 1 && kept < after.len(), "kept={kept} of {}", after.len());
            // Once more (the next round of open grows from the same starting point)
            before.clear();
            let _ = std::fs::write(
                &p,
                all[..13]
                    .iter()
                    .map(|l| format!("{{\"timestamp\":{},\"type\":{},\"payload\":{}}}\n", l["timestamp"], l["type"], l["payload"]))
                    .collect::<String>(),
            );
            t = Trace::open_born(p.clone(), Agent::Codex, 0).unwrap();
        }
    }

    #[test]
    fn child_threads_hang_under_spawn_agent_and_the_parent_history_they_copied_is_not_counted() {
        let (_h, parent) = family("kids", true);
        let t = Trace::open_born(parent, Agent::Codex, 0).unwrap();
        let call = t.nodes[t.roots[0]].kids[0];
        let (sp1, sp2) = (t.nodes[call].kids[0], t.nodes[call].kids[1]);
        assert_eq!((t.nodes[sp1].name.as_str(), t.nodes[sp1].label.as_str()), ("Agent", "explorer · 看看 b.rs 里的断言"));
        let k1 = t.sub_of(sp1).expect("kid1 按 sub_agent_activity 挂上");
        let k2 = t.sub_of(sp2).expect("kid2 按输出里的 agent_id 挂上");
        assert_eq!((k1.kind.as_str(), k1.desc.as_str(), k2.desc.as_str()), ("explorer", "Euclid", "Noether"));
        // The parent turn copied at the start is folded into one block, not counted; its own turn is turn 1, receiving the task
        for k in [k1, k2] {
            let roots: Vec<Kind> = k.t.roots.iter().map(|&r| k.t.nodes[r].kind).collect();
            assert_eq!(roots, [Kind::Copied, Kind::Turn], "{}", k.id);
            assert_eq!(k.t.turn_no(k.t.roots[1]), Some(1));
        }
        assert_eq!(crate::trace::asked(&k1.t, k1.t.roots[1]), ("任务", "看看 b.rs 里的断言".to_string()));
        // Money: a child thread counts only its own usage (not the copied call), added to the turn that started it
        let cost = |m: &str, tok: [u64; 5]| aca_records::usage::call_cost(m, tok, false, false, 0).unwrap();
        let own = cost("gpt-5.5", [100, 0, 0, 300, 70]) + cost("gpt-5.5", [100, 0, 0, 200, 60]);
        assert!((k1.t.total(false, 0).2.cost - own).abs() < 1e-12);
        let kid2 = cost("gpt-5.5", [300, 0, 0, 0, 40]);
        assert!((k2.t.total(false, 0).2.cost - kid2).abs() < 1e-12, "{}", k2.t.total(false, 0).2.cost);
        let s = t.sum(t.roots[0]);
        assert_eq!((s.calls, s.subs), (2, 2));
        assert!((s.sub_cost - own - kid2).abs() < 1e-12);
        // All done; the right side shows the child thread's numbers and what it said last
        assert!(!t.sub_live(sp1, true, ts_ms(&at(9000))));
        let plain = |ps: &[crate::trace::Para]| {
            crate::trace::wrap(ps, 80).iter().map(|l| crate::ansi::plain(l).trim_end().to_string()).collect::<Vec<_>>()
        };
        let (ps, _) = crate::trace_page::tool_item(&t, sp1, false, 0, false);
        let text = plain(&ps);
        assert!(text[0].starts_with("· Agent explorer · 看看 b.rs 里的断言") && text[0].contains("2 调用"), "{text:#?}");
        assert_eq!(&text[1..], ["  b.rs 第 8 行:断言写反了。", "  改成 2 就对了"]);
    }

    #[test]
    fn rolled_back_turns_are_greyed_and_unnumbered_but_their_money_stays() {
        let turn = |ms: u32, id: &str, ask: &str, total: u64| {
            vec![
                ev(ms, json!({"type": "task_started", "turn_id": id})),
                ev(ms + 10, json!({"type": "user_message", "message": ask, "images": [], "local_images": []})),
                say(ms + 500, "好", "final_answer"),
                tokens(ms + 600, total, 100, 0, 10),
                ev(ms + 700, json!({"type": "task_complete", "turn_id": id})),
            ]
        };
        let mut lines = vec![meta("0.150.0"), row(500, "turn_context", json!({"cwd": "/p/app", "model": "gpt-5.5"}))];
        lines.extend(turn(1000, "1", "一", 110));
        lines.extend(turn(2000, "2", "二", 220));
        lines.extend(turn(3000, "3", "三", 330));
        lines.push(ev(3900, json!({"type": "thread_rolled_back", "num_turns": 2})));
        lines.extend(turn(4000, "4", "改过的二", 440));
        let p = file("rollback", &lines);
        let t = Trace::open_born(p.clone(), Agent::Codex, 0).unwrap();
        let turns: Vec<usize> = t.roots.iter().copied().filter(|&r| t.nodes[r].kind == Kind::Turn).collect();
        let rolled: Vec<bool> = turns.iter().map(|&r| t.nodes[r].rolled).collect();
        assert_eq!(rolled, [false, true, true, false]);
        assert_eq!(turns.iter().map(|&r| t.turn_no(r)).collect::<Vec<_>>(), [Some(1), None, None, Some(2)]);
        assert_eq!(crate::trace::where_of(&t, turns[1]), "已回退");
        let (n, _, s) = t.total(false, 0);
        let bill: f64 = aca_records::usage::fixtures::codex_seen(&p)
            .into_iter()
            .map(|(m, tok)| aca_records::usage::call_cost(&m, tok, false, false, 0).unwrap())
            .sum();
        assert_eq!((n, s.calls), (2, 4));
        assert!((s.cost - bill).abs() < 1e-12);
        let c = crate::trace::chat(&t, turns[1], false, 0);
        assert!(crate::ansi::plain(&c.foot.body).starts_with("↶ 已回退 · "));
    }
}
