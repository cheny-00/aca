//! 会话记录排成字:左边对话里的一轮、右边一轮的过程、整屏的详情、e 用编辑器看的那份。
//! 两家(`trace_claude` / `trace_codex`)把一个节点的内容读回来整理成一样的 —— 提问(`asked`)、想的和说的(`said`)、
//! 工具的 `Io`(命令、改动、读到的文件、输入、输出)—— 这里只管怎么排。

use std::hash::{DefaultHasher, Hash, Hasher};

use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use aca_core::i18n::noun;
use aca_core::{t, tr};

use crate::ansi::Styled;
use crate::data::Agent;
use crate::theme;
use crate::trace::{
    Change, Chat, Kind, Para, St, Step, Trace, View, asked, blank, body, body_hl, call_note, clean, clock, fail_text, first_line, flat,
    fmt_cost, fmt_dur, fmt_tok, hang, heading, io, md, minor, said, seg, short_model, short_path, thousands, tool_name, unmark,
};
use crate::ui::dim;

/// 收着的时候改动最多露几行、输出最多露几行
const PEEK_DIFF: usize = 8;
const PEEK_OUT: usize = 3;
/// 收着的思考最多几行:本机的摘要桌面右栏(57 格)八成六放得下,手机(39 格)放得下三成,其余露前 5 行
const PEEK_THOUGHT: usize = 5;

/// 改动排成段落:行号淡,删的、加的那行铺淡红 / 淡绿底、记号红 / 绿,代码按文件的语言上色
/// (认不出语言的:删的字红、加的绿、上下文淡);两处改动中间一行点线;折下去的行跟内容对齐
fn change_paras(ch: &[&Change], pre: &Styled, path: &str) -> Vec<Para> {
    let p = theme::pal();
    let nw = ch.iter().filter_map(|c| c.no).max().map_or(0, |n| n.to_string().len());
    let gutter = if nw == 0 { 0 } else { nw + 1 };
    // 改之前、改之后各是一段连着的代码,分开上色(上下文两边都喂)
    let first_line = ch.iter().find(|c| c.c != '~').map(|c| c.text.as_str());
    let (mut old, mut new) = (crate::hl::Hl::for_path(path, first_line), crate::hl::Hl::for_path(path, first_line));
    ch.iter()
        .map(|c| {
            if c.c == '~' {
                old.as_mut().map(crate::hl::Hl::reset);
                new.as_mut().map(crate::hl::Hl::reset);
                return Para::under(pre.clone(), vec![seg("┈".repeat(gutter + 2), theme::faint())]);
            }
            let text = clean(&c.text);
            let (mark, bg) = match c.c {
                '-' => (Style::new().fg(p.red), Some(p.del_bg)),
                '+' => (Style::new().fg(p.green), Some(p.add_bg)),
                _ => (Style::new(), None),
            };
            let base = bg.map_or(Style::new(), |b| Style::new().bg(b));
            let body = match c.c {
                '-' => old.as_mut().map(|h| h.line(&text, base)),
                '+' => new.as_mut().map(|h| h.line(&text, base)),
                _ => {
                    old.as_mut().map(|h| h.line(&text, base));
                    new.as_mut().map(|h| h.line(&text, base))
                }
            };
            let body = body.unwrap_or_else(|| vec![seg(text, if c.c == ' ' { dim() } else { mark.patch(base) })]);
            let no = match c.no {
                Some(n) if nw > 0 => format!("{n:>nw$} "),
                _ => " ".repeat(gutter),
            };
            let first = [pre.clone(), vec![seg(no, theme::faint()), seg(format!("{} ", c.c), mark.patch(base))]].concat();
            let rest = [pre.clone(), vec![seg(" ".repeat(gutter), Style::new()), seg("  ", base)]].concat();
            Para { pad_bg: bg, ..Para::hung(first, rest, body) }
        })
        .collect()
}

fn more(pre: &Styled, k: usize) -> Para {
    Para::under(pre.clone(), vec![seg(tr!("… 还有 {} 行", "… {} more lines", thousands(k as u64)), theme::faint())])
}

/// 几个文件的改动:改了不止一个文件时,每个文件先写一行路径。open = 带上下文全露(最多 200 行),
/// 收着只露改动的行(最多 8 行)。第二个值:还有没露的
fn edit_paras(t: &Trace, edits: &[(String, Vec<Change>)], open: bool, pre: &Styled) -> (Vec<Para>, bool) {
    let cap = if open { crate::trace::OUT_LINES } else { PEEK_DIFF };
    let (mut out, mut used, mut left, mut hidden) = (vec![], 0, 0, false);
    for (path, ch) in edits {
        let rows: Vec<&Change> = if open { ch.iter().collect() } else { ch.iter().filter(|c| matches!(c.c, '-' | '+')).collect() };
        hidden |= rows.len() < ch.len();
        let take = rows.len().min(cap - used);
        if edits.len() > 1 && used < cap {
            out.push(Para::under(pre.clone(), vec![seg(short_path(path, &t.cwd), dim().add_modifier(Modifier::BOLD))]));
        }
        out.extend(change_paras(&rows[..take], pre, path));
        used += take;
        left += rows.len() - take;
    }
    if left > 0 {
        out.push(more(pre, left));
        hidden = true;
    }
    (out, hidden)
}

/// 读到的文件内容排成段落:行号淡,代码按文件的语言上色
fn file_paras(rows: &[(u64, String)], path: &str, pre: &Styled) -> Vec<Para> {
    let nw = rows.last().map_or(1, |r| r.0.to_string().len());
    let mut hl = crate::hl::Hl::for_path(path, rows.first().map(|r| r.1.as_str()));
    rows.iter()
        .map(|(no, l)| {
            let l = clean(l);
            let body = match hl.as_mut() {
                Some(h) => h.line(&l, Style::new()),
                None => vec![seg(l, dim())],
            };
            Para::hung(
                [pre.clone(), vec![seg(format!("{no:>nw$}  "), theme::faint())]].concat(),
                [pre.clone(), vec![seg(" ".repeat(nw + 2), Style::new())]].concat(),
                body,
            )
        })
        .collect()
}

/// 命令排成段落:第一行前面写 $,按 shell 上色
fn cmd_paras(cmds: &[String], pre: &Styled) -> Vec<Para> {
    let mut out = vec![];
    for cmd in cmds {
        let mut hl = crate::hl::Hl::for_lang("sh");
        for (k, l) in cmd.trim().lines().enumerate() {
            let mark = seg(if k == 0 { "$ " } else { "  " }, theme::faint());
            let l = clean(l);
            let body = match hl.as_mut() {
                Some(h) => h.line(&l, Style::new()),
                None => vec![seg(l, Style::new())],
            };
            out.push(Para::hung([pre.clone(), vec![mark]].concat(), [pre.clone(), vec![seg("  ", Style::new())]].concat(), body));
        }
    }
    out
}

/// 工具那一行:▸ Edit  src/bill.rs ……… 靠右 +3 -1 · 12.3s(在跑 / 被拒 / 被打断 / 出错也写在靠右那截)。
/// mark:▸ 收着还有没露的、▾ 展开了、· 全露了;extra:靠右先写的(改了几行、读了几行、几条命令)
fn tool_head(t: &Trace, i: usize, live: bool, now: i64, mark: &str, extra: Styled) -> Para {
    let n = &t.nodes[i];
    let running = t.running(i, live);
    let dur = t.dur(i, running, now);
    let p = theme::pal();
    let first = vec![
        seg(format!("{mark} "), theme::faint()),
        seg(format!("{:<5} ", tool_name(&n.name)), theme::accent().add_modifier(Modifier::BOLD)),
    ];
    let body = vec![seg(n.label.clone(), if n.copied { dim() } else { Style::new() })];
    let state = match (running, n.state, n.t1) {
        (true, ..) => Some((t!("运行中", "running"), Style::new().fg(p.green).add_modifier(Modifier::BOLD))),
        (_, St::Denied, _) => Some((t!("已拒绝", "denied"), Style::new().fg(p.red))),
        (_, St::Err, _) => Some((t!("出错", "error"), Style::new().fg(p.red))),
        (_, St::Stopped, _) => Some((t!("已中断", "interrupted"), dim())),
        (_, _, 0) => Some((t!("无结果", "no result"), dim())),
        _ => None,
    };
    let mut parts: Vec<Styled> = vec![];
    if let Some((s, st)) = state {
        parts.push(vec![seg(s, st)]);
    }
    if !extra.is_empty() {
        parts.push(extra);
    }
    if (n.t1 > 0 || running) && dur >= 1000 {
        parts.push(vec![seg(fmt_dur(dur), dim())]);
    }
    let mut right = vec![];
    for (k, x) in parts.into_iter().enumerate() {
        if k > 0 {
            right.push(seg(" · ", theme::faint()));
        }
        right.extend(x);
    }
    Para { first, body, right, ..Para::default() }
}

/// 右边的一个工具:一行头 + 底下的内容。收着露一点(改动的行、输出开头;读到的文件内容不露),
/// open 了看全(改动带上下文、文件内容带行号、整条命令、输出前 200 行)。第二个值:能不能 o
pub(crate) fn tool_item(t: &Trace, i: usize, live: bool, now: i64, open: bool) -> (Vec<Para>, bool) {
    if t.sub_of(i).is_some() {
        return sub_item(t, i, live, now, open);
    }
    let n = &t.nodes[i];
    let p = theme::pal();
    let pre = vec![seg("  ", Style::new())];
    let io = io(t, i);
    let (mut out, mut extra, mut hidden) = (vec![], vec![], false);
    'body: {
        if !io.edits.is_empty() {
            let all = io.edits.iter().flat_map(|e| &e.1);
            let (add, del) = all.fold((0, 0), |(a, d), c| (a + usize::from(c.c == '+'), d + usize::from(c.c == '-')));
            if add > 0 {
                extra.push(seg(format!("+{add}"), Style::new().fg(p.green)));
            }
            if del > 0 {
                extra.push(seg(format!("{}-{del}", if add > 0 { " " } else { "" }), Style::new().fg(p.red)));
            }
            let (ps, h) = edit_paras(t, &io.edits, open, &pre);
            out.extend(ps);
            hidden = h;
            // 改好了:输出只是「改好了」,不露
            if n.state == St::Ok && io.cmds.is_empty() {
                break 'body;
            }
        }
        if let Some((path, rows)) = &io.read {
            match (rows.first(), rows.last()) {
                (Some(a), Some(b)) if a.0 > 1 => extra.push(seg(tr!("第 {}–{} 行", "lines {}–{}", a.0, b.0), dim())),
                (Some(_), Some(b)) => {
                    let (k, w) = (thousands(b.0), noun(b.0, "line", "lines"));
                    extra.push(seg(tr!("{k} 行", "{k} {w}"), dim()))
                }
                _ => {}
            }
            if !open {
                hidden = !rows.is_empty();
                break 'body;
            }
            let cap = crate::trace::OUT_LINES;
            out.extend(file_paras(&rows[..rows.len().min(cap)], path, &pre));
            if rows.len() > cap {
                out.push(more(&pre, rows.len() - cap));
            }
            break 'body;
        }
        if io.cmds.len() > 1 {
            if !extra.is_empty() {
                extra.push(seg(" · ", theme::faint()));
            }
            let (k, w) = (io.cmds.len(), noun(io.cmds.len() as u64, "command", "commands"));
            extra.push(seg(tr!("{k} 条命令", "{k} {w}"), dim()));
        }
        // 命令:头一行只写了第一条的第一行,展开了先写全部命令
        hidden |= io.cmds.len() > 1 || io.cmds.iter().any(|c| c.trim().lines().count() > 1);
        if open {
            out.extend(cmd_paras(&io.cmds, &pre));
        }
        let Some(text) = &io.out else { break 'body };
        let mut lines: Vec<&str> = text.lines().map(str::trim_end).skip_while(|l| l.is_empty()).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let st = if n.state == St::Ok { dim() } else { Style::new().fg(p.red) };
        let cap = if open { crate::trace::OUT_LINES } else { PEEK_OUT };
        out.extend(lines.iter().take(cap).map(|l| Para::under(pre.clone(), vec![seg(clean(l), st)])));
        if lines.len() > cap {
            out.push(more(&pre, lines.len() - cap));
        }
        hidden |= !open && lines.len() > cap;
    }
    let mark = match (open, hidden) {
        (true, _) => "▾",
        (false, true) => "▸",
        (false, false) => "·",
    };
    let mut ps = vec![tool_head(t, i, live, now, mark, extra)];
    ps.extend(out);
    (ps, open || hidden)
}

/// 子代理最后说的(它交回来的结果)
fn sub_reply(t: &Trace) -> String {
    t.last_turn().and_then(|r| last_said(t, r)).map(|c| said(t, c).1.join("\n\n")).unwrap_or_default()
}

/// 子代理的数字:在跑 / 出错 / 被打断、几次调用、钱、从头到尾多久(后台跑的 Agent 工具早就返回了,真正的耗时在这)
fn sub_numbers(t: &Trace, i: usize, live: bool, now: i64) -> Styled {
    let Some(s) = t.sub_of(i) else { return vec![] };
    let p = theme::pal();
    let sum = s.t.total(false, 0).2;
    let mut parts: Vec<crate::ansi::Seg> = vec![];
    match (t.sub_live(i, live, now), t.sub_state(i)) {
        (true, _) => parts.push(seg(t!("运行中", "running"), Style::new().fg(p.green).add_modifier(Modifier::BOLD))),
        (_, St::Err) => parts.push(seg(t!("出错", "error"), Style::new().fg(p.red))),
        (_, St::Stopped) => parts.push(seg(t!("已中断", "interrupted"), dim())),
        _ => {}
    }
    if sum.calls > 0 {
        let (k, w) = (sum.calls, noun(sum.calls as u64, "call", "calls"));
        parts.push(seg(tr!("{k} 调用", "{k} {w}"), dim()));
        parts.push(seg(fmt_cost(sum.cost), dim()));
    }
    let d = t.sub_dur(i, live, now);
    if d >= 1000 {
        parts.push(seg(fmt_dur(d), dim()));
    }
    let mut out = vec![];
    for (k, x) in parts.into_iter().enumerate() {
        if k > 0 {
            out.push(seg(" · ", theme::faint()));
        }
        out.push(x);
    }
    out
}

/// 起了子代理的那一步:头一行照工具写,靠右换成子代理的数字;底下是它最后说的(收着前 3 行,展开全文)。
/// ⏎ 进去看它自己的过程(界面管)
fn sub_item(t: &Trace, i: usize, live: bool, now: i64, open: bool) -> (Vec<Para>, bool) {
    let n = &t.nodes[i];
    let pre = vec![seg("  ", Style::new())];
    let reply = t.sub_of(i).map(|s| sub_reply(&s.t)).unwrap_or_default();
    let lines: Vec<&str> = reply.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    let hidden = !open && lines.len() > PEEK_OUT;
    let mark = match (open, hidden) {
        (true, _) => "▾",
        (false, true) => "▸",
        (false, false) => "·",
    };
    let first = vec![
        seg(format!("{mark} "), theme::faint()),
        seg(format!("{:<5} ", tool_name(&n.name)), theme::accent().add_modifier(Modifier::BOLD)),
    ];
    let mut ps =
        vec![Para { first, body: vec![seg(n.label.clone(), Style::new())], right: sub_numbers(t, i, live, now), ..Para::default() }];
    if open {
        let mut body = md(&reply, Style::new());
        let cap = crate::trace::OUT_LINES;
        let left = body.len().saturating_sub(cap);
        body.truncate(cap);
        ps.extend(hang(body, &pre, &pre));
        if left > 0 {
            ps.push(more(&pre, left));
        }
    } else {
        ps.extend(lines.iter().take(PEEK_OUT).map(|l| Para::under(pre.clone(), vec![seg(clean(&unmark(l)), dim())])));
        if hidden {
            ps.push(more(&pre, lines.len() - PEEK_OUT));
        }
    }
    (ps, open || hidden)
}

/// 一次调用开头一条分隔线,线上写数字:── 4.5s · 1.1k · $0.01 ───(模型换了写上模型)
fn call_rule(t: &Trace, i: usize) -> Para {
    Para {
        body: vec![seg("── ", theme::faint()), seg(call_numbers(t, i), dim()), seg(" ", Style::new())],
        fill: Some(seg("─", theme::faint())),
        ..Para::default()
    }
}

/// 一次调用的数字:多久 · 上下文 · 钱,4.5s · 1.1k · $0.01。模型和上一次调用的不一样时写上模型
/// (头一次调用跟出现最多的那个比)
fn call_numbers(t: &Trace, i: usize) -> String {
    let n = &t.nodes[i];
    let cost = if n.copied { t!("不计入", "not counted").to_string() } else { t.cost(i).map_or("–".into(), fmt_cost) };
    let mut s = format!("{} · {} · {cost}", fmt_dur(t.dur(i, false, 0)), fmt_tok(n.context()));
    let prev = t.nodes[..i].iter().rev().find(|x| x.kind == Kind::Call).map_or_else(|| t.main_model(), |x| x.model);
    if n.model != prev {
        s += &format!(" · {}", short_model(&t.models[n.model]));
    }
    s
}

/// 这一轮最后说了话的那次调用(它说的就是这一轮的回复,左边写它)
pub(crate) fn last_said(t: &Trace, turn: usize) -> Option<usize> {
    t.nodes[turn].kids.iter().rev().copied().find(|&k| t.nodes[k].kind == Kind::Call && !t.nodes[k].label.is_empty())
}

/// 右边的一次调用:分隔线、想的(收着摊成一段,最多 PEEK_THOUGHT 行)、说的(全文;brief = 这是回复,左边有全文,收着只写第一句)。
/// 第二个值:能不能 o(想的截没截到,要折好才知道,见 `wrap_clipped`)
fn call_item(t: &Trace, i: usize, brief: bool, open: bool) -> (Vec<Para>, bool) {
    let n = &t.nodes[i];
    let mut out = vec![call_rule(t, i)];
    let (thought, text) = said(t, i);
    let label = |s: &str| vec![seg(s.to_string(), theme::faint())];
    let pad = vec![seg("     ", Style::new())];
    let mut hidden = false;
    // 收着的一句:第一行,后面还有就写 …
    let mut one = |all: &str, limit: usize| {
        let mut line = first_line(&unmark(all), limit);
        if all.lines().filter(|l| !l.trim().is_empty()).count() > 1 || unmark(all).trim().chars().count() > line.chars().count() {
            line.push('…');
            hidden = true;
        }
        line
    };
    if !thought.is_empty() {
        let all = thought.join("\n\n");
        if open {
            out.extend(hang(md(&all, dim()), &label(t!("思考 ", "think ")), &pad));
        } else {
            out.push(Para { max: PEEK_THOUGHT, ..Para::hung(label(t!("思考 ", "think ")), pad.clone(), vec![seg(flat(&all), dim())]) });
        }
    }
    let st = if n.copied { dim() } else { Style::new() };
    if brief && !open {
        let all = text.join("\n");
        out.push(Para::hung(label(t!("回复 ", "reply ")), pad, vec![seg(one(&all, 80), dim())]));
        return (out, hidden);
    }
    for (k, tx) in text.iter().enumerate() {
        if k > 0 {
            out.push(blank());
        }
        out.extend(md(tx, st));
    }
    (out, open || hidden)
}

/// 一轮的对话里有哪几块:这一轮自己(提问)、每次调用、它调的每个工具、出错
pub(crate) fn flow_nodes(t: &Trace, turn: usize) -> Vec<usize> {
    let mut out = vec![turn];
    for &k in &t.nodes[turn].kids {
        match t.nodes[k].kind {
            Kind::Call => {
                out.push(k);
                out.extend(t.nodes[k].kids.iter().copied());
            }
            Kind::Fail => out.push(k),
            _ => {}
        }
    }
    out
}

/// 一轮的对话,一块一个节点(见 flow_nodes)。brief = 最后那句回复只留第一句;open:哪些块展开了
pub(crate) fn flow(t: &Trace, turn: usize, live: bool, now: i64, brief: bool, open: &dyn Fn(usize) -> bool) -> Vec<Step> {
    let list = flow_nodes(t, turn);
    (0..list.len()).map(|k| flow_item(t, turn, &list, k, live, now, brief, open)).collect()
}

/// 第 k 块(单独排得出来:右边只给看得见的块上色,要一块块重排)。下一块是调用 / 出错的,这块末尾空一行;最后一块末尾写在跑 / 被打断
#[allow(clippy::too_many_arguments)]
fn flow_item(t: &Trace, turn: usize, list: &[usize], k: usize, live: bool, now: i64, brief: bool, open: &dyn Fn(usize) -> bool) -> Step {
    let n = &t.nodes[turn];
    let i = list[k];
    let kn = &t.nodes[i];
    let (mut paras, fold) = match kn.kind {
        Kind::Turn => {
            let (who, text) = asked(t, turn);
            let st = if n.copied || n.rolled { dim() } else { Style::new() };
            if who.is_empty() {
                (vec![minor(text)], false)
            } else {
                let label = vec![seg(format!("{who}  "), theme::accent().add_modifier(Modifier::BOLD))];
                let pad = vec![seg(" ".repeat(who.width() + 2), Style::new())];
                let body = md(&text, st);
                (hang(if body.is_empty() { vec![blank()] } else { body }, &label, &pad), false)
            }
        }
        Kind::Call => {
            let reply = if brief && !t.running(turn, live) { last_said(t, turn) } else { None };
            call_item(t, i, reply == Some(i), open(i))
        }
        Kind::Tool => tool_item(t, i, live, now, open(i)),
        _ => {
            let red = Style::new().fg(theme::pal().red);
            let mut ps =
                vec![Para::new(vec![seg(t!("✕ 出错 ", "✕ error "), red.add_modifier(Modifier::BOLD)), seg(kn.label.clone(), red)])];
            ps.push(Para::under(vec![seg("  ", Style::new())], vec![seg(first_line(&fail_text(t, i), 200), dim())]));
            (ps, false)
        }
    };
    if list.get(k + 1).is_some_and(|&x| matches!(t.nodes[x].kind, Kind::Call | Kind::Fail)) {
        paras.push(blank());
    }
    if k + 1 == list.len() {
        let end = if t.running(turn, live) {
            Some(Para::new(vec![seg(t!("● 运行中…", "● running…"), Style::new().fg(theme::pal().green).add_modifier(Modifier::BOLD))]))
        } else if n.state == St::Stopped {
            Some(Para::new(vec![seg(t!("■ 已中断", "■ interrupted"), dim())]))
        } else {
            None
        };
        if let Some(e) = end {
            paras.push(blank());
            paras.push(e);
        }
    }
    Step { node: i, paras, fold }
}

/// 一轮的头(整屏看一轮、e 写的那份):几点到几点、多久、出错 / 被打断;几次调用、几个工具、上下文最多多少、输出、钱
fn turn_head(t: &Trace, i: usize, live: bool, now: i64) -> Vec<Para> {
    let n = &t.nodes[i];
    let mut out = vec![minor(turn_span(t, i, live, now))];
    let s = t.sum(i);
    if s.calls > 0 {
        let mut nums = tr!("{} · 输出 {} · {}", "{} · output {} · {}", turn_counts(&s), fmt_tok(s.out), fmt_cost(s.cost));
        if s.subs > 0 {
            nums += &tr!(" · 子代理 {}", " · subagents {}", fmt_cost(s.sub_cost));
        }
        if s.unpriced {
            nums += t!(" · 部分模型未计价", " · some models not priced");
        }
        out.push(minor(nums));
    }
    if n.copied {
        out.push(minor(t!("上一段 · 不计入", "earlier · not counted")));
    }
    out
}

/// 右栏顶上一轮的头:时刻、时长、钱、出错 / 被打断左栏那一轮底下写了,这里只写几次调用、几个工具、上下文最多多少
fn side_head(t: &Trace, i: usize) -> Vec<Para> {
    let s = t.sum(i);
    let mut out = vec![];
    if s.calls > 0 {
        let mut nums = turn_counts(&s);
        if s.unpriced {
            nums += t!(" · 部分模型未计价", " · some models not priced");
        }
        out.push(minor(nums));
    }
    if t.nodes[i].copied {
        out.push(minor(t!("上一段 · 不计入", "earlier · not counted")));
    }
    out
}

/// 2 调用 · 1 工具 · 3 子代理 · 上下文最多 309k
fn turn_counts(s: &crate::trace::Sum) -> String {
    let (k, w) = (s.calls, noun(s.calls as u64, "call", "calls"));
    let mut parts = vec![tr!("{k} 调用", "{k} {w}")];
    if s.tools > 0 {
        let (k, w) = (s.tools, noun(s.tools as u64, "tool", "tools"));
        parts.push(tr!("{k} 工具", "{k} {w}"));
    }
    if s.subs > 0 {
        let (k, w) = (s.subs, noun(s.subs as u64, "subagent", "subagents"));
        parts.push(tr!("{k} 子代理", "{k} {w}"));
    }
    parts.push(tr!("上下文最多 {}", "context up to {}", fmt_tok(s.ctx)));
    parts.join(" · ")
}

/// 几点到几点、多久、出错 / 被打断 / 回退掉了
fn turn_span(t: &Trace, i: usize, live: bool, now: i64) -> String {
    let n = &t.nodes[i];
    let running = t.running(i, live);
    let dur = t.dur(i, running, now);
    // 记录里没写一轮到哪结束的(子代理、记录断在半截的):到最后一次有动静
    let end = if running {
        t!("运行中", "running").to_string()
    } else if n.t1 > 0 {
        clock(n.t1, t.tz, true)
    } else {
        clock(t.last_at(i), t.tz, true)
    };
    let d = if n.exact { fmt_dur(dur) } else { tr!("约 {}", "~{}", fmt_dur(dur)) };
    let mut first = format!("{} → {end} · {d}", clock(n.t0, t.tz, true));
    match n.state {
        St::Err => first += t!(" · 出错", " · error"),
        St::Stopped => first += t!(" · 已中断", " · interrupted"),
        _ => {}
    }
    if n.rolled {
        first += t!(" · 已回退", " · rolled back");
    }
    first
}

/// 左边对话里的一轮:你说的(全文)、Claude / Codex 最后说的(全文;在跑的还没有)、一行数字
pub(crate) fn chat(t: &Trace, i: usize, live: bool, now: i64) -> Chat {
    let n = &t.nodes[i];
    let running = t.running(i, live);
    let p = theme::pal();
    let st = if n.copied || n.rolled { dim() } else { Style::new() };
    let bold = |c| Style::new().fg(c).add_modifier(Modifier::BOLD);
    let some = |ps: Vec<Para>| if ps.is_empty() { vec![blank()] } else { ps };
    let (who, text) = asked(t, i);
    let asked = if who.is_empty() {
        vec![minor(text)]
    } else {
        hang(
            some(md(&text, st)),
            &vec![seg(format!("{who}  "), bold(if n.copied || n.rolled { p.muted } else { p.accent }))],
            &vec![seg(" ".repeat(who.width() + 2), st)],
        )
    };
    let (ai, color) = if t.agent == Agent::Codex { ("Codex  ", p.codex) } else { ("Claude  ", p.claude) };
    let reply = match last_said(t, i).filter(|_| !running) {
        Some(c) => {
            let (_, texts) = said(t, c);
            hang(
                some(md(&texts.join("\n\n"), st)),
                &vec![seg(ai, bold(if n.copied || n.rolled { p.muted } else { color }))],
                &vec![seg("  ", st)],
            )
        }
        None => vec![],
    };
    // 时刻 · 时长 · 钱;几次调用、几个工具在右栏
    let s = t.sum(i);
    let mut parts = vec![clock(n.t0, t.tz, false), fmt_dur(t.dur(i, running, now))];
    parts.push(if n.copied {
        t!("不计入", "not counted").into()
    } else if s.calls > 0 {
        fmt_cost(s.cost)
    } else {
        String::new()
    });
    parts.retain(|x| !x.is_empty());
    let mut foot = vec![];
    // 这一轮起的后台子代理还在干活
    let busy = !running && t.subs_in(i).iter().any(|&k| t.sub_live(k, live, now));
    match (running, n.state) {
        _ if n.rolled => foot.push(seg(t!("↶ 已回退 · ", "↶ rolled back · "), dim())),
        (true, _) => foot.push(seg(t!("● 运行中 · ", "● running · "), bold(p.green))),
        _ if busy => foot.push(seg(t!("● 子代理运行中 · ", "● subagent running · "), bold(p.green))),
        (_, St::Err) => foot.push(seg(t!("✕ 出错 · ", "✕ error · "), Style::new().fg(p.red))),
        (_, St::Stopped) => foot.push(seg(t!("■ 已中断 · ", "■ interrupted · "), dim())),
        _ => {}
    }
    foot.push(seg(parts.join(" · "), dim()));
    Chat { asked, reply, foot: Para::new(foot) }
}

/// 右边:一轮的中间过程。第一块是这一轮的头(节点是这一轮),后面每次调用、每个工具、出错一块;最后那句回复只留第一句
#[cfg(test)]
pub(crate) fn steps(t: &Trace, turn: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Vec<Step> {
    let list = flow_nodes(t, turn);
    (0..list.len()).map(|k| steps_item(t, turn, &list, k, live, now, open)).collect()
}

/// 右边第 k 块(list = flow_nodes)排出来的样子取决于的东西:都没变的块不用重排。
/// 在跑的会话每读到新的一截整轮都要重排,一轮几百次调用时大半的块是老样子
pub(crate) fn step_sig(t: &Trace, turn: usize, list: &[usize], k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> u64 {
    let i = list[k];
    let n = &t.nodes[i];
    let mut h = DefaultHasher::new();
    (i, open(i), n.t0, n.t1, n.state as u8, n.src.len(), n.kids.len(), n.tok[4], n.label.len()).hash(&mut h);
    let running = t.running(turn, live);
    let last = k + 1 == list.len();
    // 最后一块末尾写在跑 / 被打断;下一块是调用的空一行;是这一轮的回复的只留一句
    (last, last && running, last.then_some(t.nodes[turn].state as u8)).hash(&mut h);
    list.get(k + 1).map(|&x| matches!(t.nodes[x].kind, Kind::Call | Kind::Fail)).hash(&mut h);
    (n.kind == Kind::Call && !running && last_said(t, turn) == Some(i)).hash(&mut h);
    // 子代理:它的记录长了、干完了
    t.sub_of(i).map(|s| (s.t.grown(), t.sub_live(i, live, now), t.sub_state(i) as u8)).hash(&mut h);
    // 在跑的写着跑了多久;头是整轮的合计,每次都排
    if t.running(i, live) || t.sub_live(i, live, now) || k == 0 {
        (now, t.off).hash(&mut h);
    }
    h.finish()
}

/// 右边的第 k 块:第一块换成这一轮的头
pub(crate) fn step(t: &Trace, turn: usize, k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Option<Step> {
    let list = flow_nodes(t, turn);
    (k < list.len()).then(|| steps_item(t, turn, &list, k, live, now, open))
}

fn steps_item(t: &Trace, turn: usize, list: &[usize], k: usize, live: bool, now: i64, open: &dyn Fn(usize) -> bool) -> Step {
    if k > 0 {
        return flow_item(t, turn, list, k, live, now, true, open);
    }
    let mut head = side_head(t, turn);
    head.push(blank());
    Step { node: turn, paras: head, fold: false }
}

/// 改动的标题下面:改了不止一个文件时每个文件前面一行路径
fn edits_detail(t: &Trace, out: &mut Vec<Para>, edits: &[(String, Vec<Change>)]) {
    for (path, ch) in edits {
        if edits.len() > 1 {
            out.push(Para::new(vec![seg(short_path(path, &t.cwd), dim().add_modifier(Modifier::BOLD))]));
        }
        out.extend(change_paras(&ch.iter().collect::<Vec<_>>(), &vec![], path));
    }
}

/// 一次调用为什么停:正常的(要调工具、说完了)不写,别的说成话;不认得的照写
fn stop_why(stop: &str) -> Option<String> {
    Some(match stop {
        "" | "tool_use" | "end_turn" | "stop_sequence" | "pause_turn" => return None,
        "max_tokens" => t!("输出达到上限,已截断", "hit the output limit, cut off").into(),
        "model_context_window_exceeded" => t!("上下文已满", "the context is full").into(),
        "refusal" => t!("模型拒绝回答", "the model refused").into(),
        x => tr!("停止于 {x}", "stopped at {x}"),
    })
}

pub(crate) fn detail(t: &Trace, i: usize, live: bool, now: i64) -> Vec<Para> {
    let n = &t.nodes[i];
    let running = t.running(i, live);
    let dur = t.dur(i, running, now);
    let mut out: Vec<Para> = vec![];
    let span = |exact: bool| {
        let end = match n.t1 {
            _ if running => t!("运行中", "running").to_string(),
            0 if n.kind == Kind::Turn => clock(t.last_at(i), t.tz, true),
            0 => "?".into(),
            x => clock(x, t.tz, true),
        };
        let d = if exact || n.kind == Kind::Tool { fmt_dur(dur) } else { tr!("约 {}", "~{}", fmt_dur(dur)) };
        format!("{} → {end} · {d}", clock(n.t0, t.tz, true))
    };
    let state = |st: St| match st {
        St::Ok => None,
        St::Err => Some(t!("出错", "error")),
        St::Denied => Some(t!("已拒绝", "denied")),
        St::Stopped => Some(t!("已中断", "interrupted")),
    };
    match n.kind {
        Kind::Turn => {
            out.extend(turn_head(t, i, live, now));
            out.push(blank());
            for s in flow(t, i, live, now, false, &|_| false) {
                out.extend(s.paras);
            }
        }
        Kind::Call => {
            let model = &t.models[n.model];
            out.push(minor(if n.effort.is_empty() {
                model.clone()
            } else {
                tr!("{model} · 思考 {}", "{model} · thinking {}", n.effort)
            }));
            let mut when = span(false);
            if let Some(why) = stop_why(&n.stop) {
                when += &format!(" · {why}");
            }
            out.push(minor(when));
            let mut tok = tr!("上下文 {}:输入 {}", "context {}: input {}", thousands(n.context()), thousands(n.tok[0]));
            if n.tok[1] + n.tok[2] > 0 {
                tok += &tr!(" · 缓存写 {}", " · cache write {}", thousands(n.tok[1] + n.tok[2]));
            }
            tok += &tr!(" · 缓存读 {}", " · cache read {}", thousands(n.tok[3]));
            out.push(minor(tok));
            let mut outs = tr!("输出 {}", "output {}", thousands(n.tok[4]));
            if n.think > 0 {
                outs += &tr!("(思考 {})", " (thinking {})", thousands(n.think));
            }
            outs += &format!(" · {}", t.cost(i).map_or(t!("未计价", "not priced").into(), fmt_cost));
            if n.searches > 0 {
                outs += &tr!(" · 搜索 {} 次", " · {} searches", n.searches);
            }
            if n.copied {
                outs += t!(" · 上一段 · 不计入", " · earlier · not counted");
            }
            out.push(minor(outs));
            if let Some(note) = call_note(t, i) {
                out.push(minor(note));
            }
            let (thought, text) = said(t, i);
            if !thought.is_empty() {
                out.push(blank());
                out.push(heading(t!("思考", "Thinking")));
                for th in &thought {
                    out.extend(md(th, dim()));
                }
            }
            if !text.is_empty() {
                out.push(blank());
                out.push(heading(t!("回复", "Reply")));
                for (k, tx) in text.iter().enumerate() {
                    if k > 0 {
                        out.push(blank());
                    }
                    out.extend(md(tx, Style::new()));
                }
            }
            if !n.kids.is_empty() {
                out.push(blank());
                out.push(heading(t!("调用了", "Called")));
                out.extend(n.kids.iter().map(|&k| tool_head(t, k, live, now, "▸", vec![])));
            }
        }
        Kind::Tool => {
            out.push(Para::hung(
                vec![seg(format!("{} ", tool_name(&n.name)), theme::accent().add_modifier(Modifier::BOLD))],
                vec![],
                vec![seg(n.label.clone(), Style::new())],
            ));
            let io = io(t, i);
            let mut first = span(true);
            if let Some(w) = state(n.state) {
                first += &format!(" · {w}");
            } else if n.t1 == 0 && !running {
                first += t!(" · 无结果", " · no result");
            }
            if let Some(x) = io.exit.filter(|&x| x != 0) {
                first += &tr!(" · 退出码 {x}", " · exit code {x}");
            }
            if n.hook > 0 {
                first += &tr!(" · 钩子 {}", " · hooks {}", fmt_dur(n.hook as i64));
            }
            out.push(minor(first));
            if let Some(s) = t.sub_of(i) {
                out.push(blank());
                out.push(heading(t!("子代理", "Subagent")));
                let head = [s.kind.as_str(), s.desc.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
                out.push(Para::new([vec![seg(format!("{head}  "), Style::new())], sub_numbers(t, i, live, now)].concat()));
                let reply = sub_reply(&s.t);
                if !reply.is_empty() {
                    out.push(blank());
                    out.extend(md(&reply, Style::new()));
                }
            }
            if io.edits.iter().any(|e| !e.1.is_empty()) {
                out.push(blank());
                out.push(heading(t!("改动", "Changes")));
                edits_detail(t, &mut out, &io.edits);
            }
            if !io.cmds.is_empty() {
                out.push(blank());
                let text = io
                    .note
                    .iter()
                    .map(|d| format!("# {d}"))
                    .chain(io.cmds.iter().map(|c| c.trim().to_string()))
                    .collect::<Vec<_>>()
                    .join("\n");
                body_hl(&mut out, t!("命令", "Command"), &text, None, Style::new(), crate::hl::Hl::for_lang("sh"));
            } else if let Some((lang, text)) = io.input.as_ref().filter(|_| io.edits.is_empty()) {
                out.push(blank());
                body_hl(&mut out, t!("输入", "Input"), text, None, Style::new(), crate::hl::Hl::for_lang(lang));
            }
            if let Some(text) = &io.out
                && !(!io.edits.is_empty() && io.cmds.is_empty() && n.state == St::Ok)
            {
                out.push(blank());
                let st = if n.state == St::Ok { Style::new() } else { Style::new().fg(theme::pal().red) };
                match &io.read {
                    // 读到的文件内容:带行号、上色,后面跟的提示不要
                    Some((path, rows)) => {
                        let cap = crate::trace::OUT_LINES;
                        out.push(heading(&if rows.len() > cap {
                            tr!("文件内容 · 前 {cap} 行 / 共 {} 行", "File content · first {cap} of {} lines", thousands(rows.len() as u64))
                        } else {
                            t!("文件内容", "File content").into()
                        }));
                        out.extend(file_paras(&rows[..rows.len().min(cap)], path, &vec![]));
                    }
                    None => body(&mut out, t!("输出", "Output"), text, Some(crate::trace::OUT_LINES), st),
                }
            }
        }
        Kind::Fail => {
            out.push(minor(clock(n.t0, t.tz, true)));
            out.push(blank());
            body(&mut out, t!("出错", "error"), &fail_text(t, i), None, Style::new());
        }
        Kind::Mark | Kind::Copied => out.push(minor(clock(n.t0, t.tz, true))),
    }
    out
}

// ---------- e:用编辑器看 ----------

/// 围住一段的反引号:比里面最长的一串多一个(至少 3 个)
pub(crate) fn fenced(lang: &str, body: &str) -> String {
    let (mut run, mut longest) = (0, 0);
    for c in body.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let f = "`".repeat((longest + 1).max(3));
    format!("{f}{lang}\n{}\n{f}\n\n", body.trim_end_matches('\n'))
}

/// 改动写成 diff(编辑器按 diff 上色),每处开头写从第几行起;几个文件的,每个文件前面写 --- 路径
fn diff_text(t: &Trace, edits: &[(String, Vec<Change>)]) -> String {
    let mut out = String::new();
    for (path, ch) in edits {
        if edits.len() > 1 {
            out += &format!("--- {}\n", short_path(path, &t.cwd));
        }
        let mut head = true;
        for c in ch {
            if c.c == '~' {
                head = true;
                continue;
            }
            if std::mem::take(&mut head)
                && let Some(n) = c.no
            {
                out += &tr!("@@ 第 {n} 行 @@\n", "@@ line {n} @@\n");
            }
            out.push(c.c);
            out += &c.text;
            out.push('\n');
        }
    }
    out
}

/// 一个工具写成 Markdown:标题(名字 · 摘要 · 耗时 · 状态)、命令 / 输入、改动、输出(全部,不截)
fn tool_md(t: &Trace, i: usize, level: &str, live: bool, now: i64) -> String {
    let n = &t.nodes[i];
    let running = t.running(i, live);
    let mut head = format!("{level} {} · {}", tool_name(&n.name), n.label);
    let dur = t.dur(i, running, now);
    if dur >= 1000 {
        head += &format!(" · {}", fmt_dur(dur));
    }
    match (running, n.state) {
        (true, _) => head += t!(" · 运行中", " · running"),
        (_, St::Err) => head += t!(" · 出错", " · error"),
        (_, St::Denied) => head += t!(" · 被拒", " · denied"),
        (_, St::Stopped) => head += t!(" · 已中断", " · interrupted"),
        _ => {}
    }
    let mut s = format!("{head}\n\n");
    let io = io(t, i);
    if io.edits.iter().any(|e| !e.1.is_empty()) {
        s += &fenced("diff", &diff_text(t, &io.edits));
    }
    if !io.cmds.is_empty() {
        let text =
            io.note.iter().map(|d| format!("# {d}")).chain(io.cmds.iter().map(|c| c.trim().to_string())).collect::<Vec<_>>().join("\n");
        s += &fenced("sh", &text);
    } else if let Some((lang, text)) = io.input.as_ref().filter(|_| io.edits.is_empty() && io.file.is_none()) {
        s += &fenced(lang, text);
    }
    if let Some(text) = &io.out
        && !(!io.edits.is_empty() && io.cmds.is_empty() && n.state == St::Ok)
    {
        match &io.read {
            Some((path, rows)) => {
                let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
                s += &fenced(ext, &rows.iter().map(|r| r.1.as_str()).collect::<Vec<_>>().join("\n"));
            }
            None => s += &fenced("text", text),
        }
    }
    if let Some(sub) = t.sub_of(i) {
        let turns: Vec<usize> = sub.t.roots.iter().copied().filter(|&r| sub.t.nodes[r].kind == Kind::Turn).collect();
        // 子代理的每一轮,标题降一级
        for r in turns {
            s += &format!("\n{}", turn_md(&sub.t, r, t.sub_live(i, live, now), now)).replace("\n#", &format!("\n{level}#"));
        }
    }
    s
}

/// 一次调用写成 Markdown:数字、想的(引用)、说的、调的工具
fn call_md(t: &Trace, i: usize, level: &str, live: bool, now: i64) -> String {
    let n = &t.nodes[i];
    let mut s = format!("{level} {} · {}\n\n", crate::trace::where_of(t, i), call_numbers(t, i));
    let (thought, text) = said(t, i);
    for th in &thought {
        s += &th.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
        s += "\n\n";
    }
    for tx in &text {
        s += tx;
        s += "\n\n";
    }
    for &k in &n.kids {
        s += &tool_md(t, k, &format!("{level}#"), live, now);
    }
    s
}

/// 一轮写成 Markdown:头、你说的、每次调用(想的、说的、工具)、出错
fn turn_md(t: &Trace, i: usize, live: bool, now: i64) -> String {
    let n = &t.nodes[i];
    let head: Vec<String> = turn_head(t, i, live, now).iter().map(|p| crate::ansi::plain(&p.body)).collect();
    let (who, asked) = asked(t, i);
    let mut s = format!(
        "# {} · {}\n\n## {}\n\n{asked}\n\n",
        crate::trace::where_of(t, i),
        head.join(" · "),
        if who.is_empty() { t!("开头", "Start") } else { who }
    );
    for &k in &n.kids {
        let kn = &t.nodes[k];
        match kn.kind {
            Kind::Call => s += &call_md(t, k, "##", live, now),
            Kind::Fail => s += &tr!("## ✕ 出错 {}\n\n{}\n\n", "## ✕ Error {}\n\n{}\n\n", kn.label, fail_text(t, k)),
            _ => {}
        }
    }
    s
}

/// e 看什么:读 / 改文件的工具看那个文件本身(停在读的 / 改的那一行;文件不在了才看记录里的);
/// 别的写成一份 Markdown(一轮、一次调用、一个工具的全部,输出不截)
pub(crate) fn view(t: &Trace, i: usize, live: bool, now: i64) -> Option<View> {
    let n = &t.nodes[i];
    let text = |text: String| Some(View::Text { text, ext: "md".into(), line: 1 });
    match n.kind {
        Kind::Turn => text(turn_md(t, i, live, now)),
        Kind::Call => text(call_md(t, i, "#", live, now)),
        Kind::Tool => {
            if let Some((mut path, line)) = io(t, i).file.filter(|f| !f.0.is_empty()) {
                if !path.starts_with('/') && !t.cwd.is_empty() {
                    path = format!("{}/{path}", t.cwd);
                }
                if std::path::Path::new(&path).is_file() {
                    return Some(View::File { path, line: line.max(1) as usize });
                }
            }
            text(tool_md(t, i, "#", live, now))
        }
        Kind::Fail => text(tr!("# ✕ 出错 {}\n\n{}\n", "# ✕ Error {}\n\n{}\n", n.label, fail_text(t, i))),
        Kind::Mark | Kind::Copied => None,
    }
}
