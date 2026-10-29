//! What the `$` billing panel draws. Top to bottom: subscription quotas; what each of the four periods cost (in USD at API prices,
//! with the change vs. the previous period and the trend; the selected one gets its daily average underneath);
//! the trend by hour / day / month; split by account / model / session.
//! Things that wouldn't change how you use it (the overview of calls, tokens and cache, where the money went, weekday × hour,
//! by directory) are hidden by default; `m` shows them (`more`).
//! Narrow screens get one column running down (phones); wide ones (≥ TWO_COL cells) get two: "quota | periods" on top,
//! "trend | breakdowns" below.
//! Data comes from `usage`; the title, footer and paging are drawn by ui.
//! Only two color families: each account has its own color (theme's series); every other bar and scale is a shade of the theme's
//! accent (total / parts / heat). The only grays are accounts without a color (other) and the unused part of a quota bar (track).
//! Secondary text is `dim()`, separator lines `theme::faint()`

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use aca_core::i18n::{Lang, Tr, lang};
use aca_core::{t, tr};

use crate::data::Agent;
use crate::theme;
use crate::ui::{clip, dim};
use crate::usage::{Quota, RANGES, Range, Report, Row, WEEK, civil_from_days, weekday};

/// Widest a column gets
const MAX_W: usize = 96;
/// From this width on, two columns, each at most COL_W wide, GAP apart
const TWO_COL: usize = 108;
const COL_W: usize = 78;
const GAP: usize = 3;
/// The money column
const MONEY_W: usize = 9;
/// At most this many rows by session / directory; the rest merge into one row
const TOP_SESSIONS: usize = 15;
const TOP_DIRS: usize = 8;
/// Vertical bars in eighths of a cell
const TICKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Horizontal bars are thin, half a cell high (the background shows between rows, so they don't merge into a block), and their
/// length goes down to half a cell: a full cell, the closing left half, the right half that continues after it
const BAR: &str = "▄";
const BAR_END: char = '▖';
const BAR_START: char = '▗';
/// The parts the money goes to
const PARTS: [Tr; 5] = [
    Tr::new("输入", "input"),
    Tr::new("缓存写", "cache write"),
    Tr::new("缓存读", "cache read"),
    Tr::new("输出", "output"),
    Tr::new("搜索", "search"),
];

/// $12.34 / $1,234
pub fn money(v: f64) -> String {
    if v >= 1000.0 {
        format!("${}", count(v.round() as u64))
    } else if v == 0.0 || v >= 0.005 {
        format!("${v:.2}")
    } else {
        "<$0.01".into()
    }
}

/// 16,234
fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 980 / 1.2 万 / 3.4 亿 (Chinese units of 10^4 / 10^8); in English 980 / 1.2K / 3.4M / 5.6B
pub fn tokens(n: u64) -> String {
    let short = |v: f64, unit: &str| format!("{} {unit}", format!("{v:.1}").trim_end_matches(".0"));
    if lang() == Lang::En {
        let short = |v: f64, unit: &str| format!("{}{unit}", format!("{v:.1}").trim_end_matches(".0"));
        return match n {
            0..=999 => n.to_string(),
            1_000..=999_999 => short(n as f64 / 1e3, "K"),
            1_000_000..=999_999_999 => short(n as f64 / 1e6, "M"),
            _ => short(n as f64 / 1e9, "B"),
        };
    }
    match n {
        0..=9_999 => n.to_string(),
        10_000..=99_999_999 => short(n as f64 / 1e4, "万"),
        _ => short(n as f64 / 1e8, "亿"),
    }
}

/// Share: 53% / <1%
fn pct(v: f64, total: f64) -> String {
    match if total > 0.0 { v / total * 100.0 } else { 0.0 } {
        p if p > 0.0 && p < 0.5 => "<1%".into(),
        p => format!("{p:.0}%"),
    }
}

/// A subheading followed by a thin line out to w; the line can end with a gray note
fn head(t: &str, note: &str, w: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" "), Span::styled(t.to_string(), theme::accent().add_modifier(Modifier::BOLD))];
    let used = 1 + t.width();
    let tail = if note.is_empty() { 0 } else { note.width() + 2 };
    let (rule, note) = if used + 3 + tail <= w { (w - used - 1 - tail, note) } else { (w.saturating_sub(used + 1), "") };
    if rule > 0 {
        spans.push(Span::styled(format!(" {}", "─".repeat(rule)), theme::faint()));
    }
    if !note.is_empty() {
        spans.push(Span::styled(format!(" {note} "), dim()));
    }
    Line::from(spans)
}

/// Right-align to n cells
fn rpad(s: &str, n: usize) -> String {
    format!("{}{s}", " ".repeat(n.saturating_sub(s.width())))
}

/// Left-align to n cells
fn lpad(s: &str, n: usize) -> String {
    format!("{s}{}", " ".repeat(n.saturating_sub(s.width())))
}

/// Clip from the left: …/fun/app (what you look at in a directory is its tail), pad with spaces to w
fn clip_left(s: &str, w: usize) -> String {
    if s.width() <= w {
        return lpad(s, w);
    }
    let mut tail: Vec<char> = vec![];
    let mut n = 1;
    for ch in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if n + cw > w {
            break;
        }
        tail.push(ch);
        n += cw;
    }
    lpad(&format!("…{}", tail.into_iter().rev().collect::<String>()), w)
}

/// Unregistered conversations are named "<dir> · 9/24": when it doesn't fit, cut the directory and keep the date (otherwise every
/// row shows the same prefix). Pad with spaces to w
fn clip_name(s: &str, w: usize) -> String {
    match s.rsplit_once(" · ") {
        Some((dir, day)) if s.width() > w && w >= day.width() + 3 + 4 => {
            lpad(&format!("{} · {day}", clip(dir, w - day.width() - 3).trim_end()), w)
        }
        _ => clip(s, w),
    }
}

/// How many half cells a bar gets: v's fraction of max fills that fraction of w; anything nonzero gets at least half a cell
fn halves(v: f64, max: f64, w: usize) -> usize {
    if max <= 0.0 || v <= 0.0 || w == 0 {
        return 0;
    }
    ((v / max).min(1.0) * (w * 2) as f64).round().max(1.0) as usize
}

/// A bar of e half cells
fn bar(e: usize) -> String {
    let mut s = BAR.repeat(e / 2);
    if e % 2 == 1 {
        s.push(BAR_END);
    }
    s
}

fn hbar(v: f64, max: f64, w: usize) -> String {
    bar(halves(v, max, w))
}

/// Quota meter: the used part and the unused part (each in its own color), always half a cell apart, w cells in all
fn meter(used: f64, w: usize) -> (String, String) {
    let e = if used > 0.0 { halves(used, 100.0, w) } else { 0 };
    let rest = w - e.div_ceil(2);
    let track = match e {
        0 => BAR.repeat(rest),
        _ if e % 2 == 1 => BAR.repeat(rest),
        _ if rest > 0 => format!("{BAR_START}{}", BAR.repeat(rest - 1)),
        _ => String::new(),
    };
    (bar(e), track)
}

/// A row of small vertical bars, one cell per value; 0 is a space
fn spark(vals: &[f64]) -> String {
    let max = vals.iter().copied().fold(0.0, f64::max);
    vals.iter().map(|&v| if v <= 0.0 || max <= 0.0 { ' ' } else { TICKS[((v / max * 8.0).ceil() as usize).clamp(1, 8) - 1] }).collect()
}

/// Squeeze into n cells: average neighboring values (when it doesn't fit you still see the trend of the whole span)
fn squeeze(vals: &[f64], n: usize) -> Vec<f64> {
    if vals.len() <= n || n == 0 {
        return vals.to_vec();
    }
    (0..n)
        .map(|i| {
            let part = &vals[i * vals.len() / n..(i + 1) * vals.len() / n];
            part.iter().sum::<f64>() / part.len().max(1) as f64
        })
        .collect()
}

/// Share w cells out in proportion: anything nonzero gets at least one cell, leftovers go to the largest remainders
fn split(parts: &[f64], w: usize) -> Vec<usize> {
    let total: f64 = parts.iter().sum();
    if total <= 0.0 || w == 0 {
        return vec![0; parts.len()];
    }
    let exact: Vec<f64> = parts.iter().map(|p| p / total * w as f64).collect();
    let mut n: Vec<usize> = exact.iter().zip(parts).map(|(e, p)| if *p > 0.0 { (e.floor() as usize).max(1) } else { 0 }).collect();
    loop {
        let sum: usize = n.iter().sum();
        if sum == w {
            break n;
        }
        let pick = if sum < w {
            (0..n.len()).filter(|&i| parts[i] > 0.0).max_by(|&a, &b| (exact[a] - n[a] as f64).total_cmp(&(exact[b] - n[b] as f64)))
        } else {
            // Take the excess from the ones that got too much; with fewer cells than items, take from the smallest
            (0..n.len())
                .filter(|&i| n[i] > 1)
                .max_by(|&a, &b| (n[a] as f64 - exact[a]).total_cmp(&(n[b] as f64 - exact[b])))
                .or_else(|| (0..n.len()).filter(|&i| n[i] > 0).min_by(|&a, &b| exact[a].total_cmp(&exact[b])))
        };
        let Some(i) = pick else { break n };
        if sum < w { n[i] += 1 } else { n[i] -= 1 }
    }
}

/// Wrap a run of small segments to the width, sep between segments, each line indented 2
fn flow(out: &mut Vec<Line<'static>>, items: Vec<Vec<Span<'static>>>, sep: &str, w: usize) {
    let mut line = vec![Span::raw("  ")];
    let mut used = 2;
    for it in items {
        let iw: usize = it.iter().map(|s| s.content.width()).sum();
        if used > 2 && used + sep.width() + iw > w {
            out.push(Line::from(std::mem::replace(&mut line, vec![Span::raw("  ")])));
            used = 2;
        }
        if used > 2 {
            line.push(Span::styled(sep.to_string(), dim()));
            used += sep.width();
        }
        line.extend(it);
        used += iw;
    }
    if used > 2 {
        out.push(Line::from(line));
    }
}

/// Gray text + number + gray text
fn fact(pre: &str, val: String, post: &str) -> Vec<Span<'static>> {
    [Span::styled(pre.to_string(), dim()), Span::raw(val), Span::styled(post.to_string(), dim())]
        .into_iter()
        .filter(|s| !s.content.is_empty())
        .collect()
}

/// (day number, hour, minute) in local time
fn local(ts: i64, off: i64) -> (i64, i64, i64) {
    let t = ts + off;
    (t.div_euclid(86400), t.rem_euclid(86400) / 3600, t.rem_euclid(3600) / 60)
}

/// Time of day: today 14:30 / tomorrow 09:00 / Fri 09:00 / 10/12 09:00
pub(crate) fn when(ts: i64, now: i64, off: i64) -> String {
    let (d, h, m) = local(ts, off);
    let today = local(now, off).0;
    let day = match d - today {
        0 => t!("今天", "today").to_string(),
        1 => t!("明天", "tomorrow").to_string(),
        2..=6 => WEEK[weekday(d)].to_string(),
        _ => {
            let (_, mo, dd) = civil_from_days(d);
            format!("{mo}/{dd}")
        }
    };
    format!("{day} {h:02}:{m:02}")
}

/// Only once quota numbers are this old (seconds) do we say when they're from (same as the switch-account box)
const STALE: i64 = 3600;

/// just now / 3m ago
pub(crate) fn ago(ts: i64, now: i64) -> String {
    match (now - ts).max(0) as u64 {
        0..=59 => t!("刚刚", "just now").into(),
        s => tr!("{}前", "{} ago", crate::data::dur(s)),
    }
}

// ---------- Subscription quotas ----------

/// How long a quota window is: 5h / 7d / 300m → seconds
fn window_secs(label: &str) -> Option<i64> {
    let (n, unit) = label.split_at(label.len().checked_sub(1)?);
    let n: i64 = n.parse().ok()?;
    Some(n * [("m", 60), ("h", 3600), ("d", 86400)].iter().find(|x| x.0 == unit)?.1)
}

/// Going on at the pace up to `at`, when it runs out before the reset (None if it won't)
fn runs_out(used: f64, resets: i64, len: i64, at: i64) -> Option<i64> {
    let elapsed = len - (resets - at);
    // Early in the window the pace can't be told yet; one that's already used up needs no guess
    if !(0.0..100.0).contains(&used) || used == 0.0 || elapsed < len / 10 || elapsed > len {
        return None;
    }
    let t = at + ((100.0 - used) / used * elapsed as f64) as i64;
    (t < resets).then_some(t)
}

/// Write an account's full name ("Claude sub") in two parts: the agent in its color (as account names in the list), the name in
/// the `rest` style. On the billing page an account has its own color (trend, bars), a different scheme from the list's
/// orange = Claude, blue = Codex: coloring the agent name ties the two together
fn family_spans(name: &str, agent: Option<Agent>, rest: Style) -> Vec<Span<'static>> {
    let (fam, tail) = name.split_once(' ').map_or((name, ""), |(f, t)| (f, t));
    let agent = agent.unwrap_or(if fam == "Codex" { Agent::Codex } else { Agent::Claude });
    let mut v = vec![Span::styled(fam.to_string(), crate::ui::agent_style(agent).patch(rest))];
    if !tail.is_empty() {
        v.push(Span::styled(format!(" {tail}"), rest));
    }
    v
}

/// The account's full name in the quota block (the one colors are looked up by)
fn quota_name(q: &Quota) -> String {
    match q.agent {
        Agent::Claude | Agent::Codex => crate::usage::account_name(q.agent, &q.who),
        _ => q.who.clone(),
    }
}

/// swatch: this account's color in the trend and bars, drawn as a block before the name so you can tell which series is whose;
/// the name itself is in the body text color.
/// label_w: cells to align window names to (the longest in the whole block, see `label_width`)
fn quota_lines(out: &mut Vec<Line<'static>>, q: &Quota, swatch: Color, (now, off): (i64, i64), w: usize, label_w: usize) {
    let name = crate::usage::account_shown(&quota_name(q)).to_string();
    let mut left = vec![Span::raw(" "), Span::styled("■ ", Style::new().fg(swatch))];
    left.extend(family_spans(&name, Some(q.agent), Style::new().add_modifier(Modifier::BOLD)));
    let mut used_w = 3 + name.width();
    for extra in [&q.plan, &q.note].into_iter().filter(|x| !x.is_empty()) {
        left.push(Span::styled(format!(" · {extra}"), dim()));
        used_w += 3 + extra.width();
    }
    // Say when it's from only once it's stale
    let right = if q.updated > 0 && now - q.updated > STALE { tr!("{}更新", "updated {}", ago(q.updated, now)) } else { String::new() };
    if !right.is_empty() && used_w + 2 + right.width() <= w {
        left.push(Span::raw(" ".repeat(w - used_w - right.width())));
        left.push(Span::styled(right, dim()));
    }
    out.push(Line::from(left));
    let bar_w = match w {
        50.. => 14,
        42.. => 10,
        _ => 5,
    };
    let p = theme::pal();
    for win in &q.wins {
        let label = format!("   {} ", rpad(&win.label, label_w));
        // Past the reset time and no new numbers: we don't know current usage, so say just that
        if win.resets.is_some_and(|r| r <= now) {
            out.push(Line::from(vec![
                Span::raw(label),
                Span::styled(BAR.repeat(bar_w), Style::new().fg(p.track)),
                Span::styled(t!("      已重置", "      reset"), dim()),
            ]));
            continue;
        }
        // Status colors only when it needs attention (yellow over 70%, red over 90%); otherwise the color that belongs to no account:
        // green is an account color in the palette
        let status = match win.used {
            u if u >= 90.0 => Some(p.red),
            u if u >= 70.0 => Some(p.amber),
            _ => None,
        };
        let (bar, track) = meter(win.used.clamp(0.0, 100.0), bar_w);
        let pct_style = status.map_or(Style::new(), |c| Style::new().fg(c)).add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::raw(label),
            Span::styled(bar, Style::new().fg(status.unwrap_or(p.total))),
            Span::styled(track, Style::new().fg(p.track)),
            Span::styled(format!(" {}", rpad(&format!("{:.0}%", win.used), 4)), pct_style),
        ];
        if let Some(r) = win.resets {
            let at = when(r, now, off);
            let room = w.saturating_sub(4 + label_w + bar_w + 5);
            let s = if tr!("  {at} 重置", "  resets {at}").width() <= room {
                tr!("  {at} 重置", "  resets {at}")
            } else {
                format!("  {at}")
            };
            if s.width() <= room {
                spans.push(Span::styled(s, dim()));
            }
        }
        out.push(Line::from(spans));
        // At this pace it runs out before the reset: say so ahead of time
        let base = if q.updated > 0 { q.updated } else { now };
        // Claude's per-model ones (Fable) are all weekly quotas
        let len = window_secs(&win.label).or((q.agent == Agent::Claude).then_some(7 * 86400));
        if let (Some(r), Some(len)) = (win.resets, len)
            && let Some(t) = runs_out(win.used, r, len, base).filter(|&t| t > now)
        {
            let s = tr!("       按当前速度 {} 用完", "       at this pace, used up {}", when(t, now, off));
            if s.width() <= w {
                out.push(Line::styled(s, Style::new().fg(if win.used >= 70.0 { p.red } else { p.amber })));
            }
        }
    }
}

/// Width of window names in the quota block: the longest across all accounts, so the bars start in the same column (Claude's
/// per-model ones are model names, Fable)
fn label_width(qs: &[Quota]) -> usize {
    qs.iter().flat_map(|q| &q.wins).map(|w| w.label.width()).max().unwrap_or(0).max(3)
}

fn quota_block(r: &Report, w: usize) -> Vec<Line<'static>> {
    if r.quotas.is_empty() && r.quota_note.is_empty() {
        return vec![];
    }
    let mut out = vec![head(t!("订阅额度", "Subscription quotas"), "", w)];
    if !r.quota_note.is_empty() {
        out.push(Line::from(Span::styled(format!("   {}", r.quota_note), dim())));
    }
    let (pal, label_w) = (account_palette(r), label_width(&r.quotas));
    for q in &r.quotas {
        quota_lines(&mut out, q, color_of(&pal, &quota_name(q)), (r.now, r.off), w, label_w);
    }
    out
}

// ---------- The four periods ----------

/// Compared with the span before: yesterday's amount / up or down how much vs. the previous 7 days / All's daily average.
/// long = add 「比前 7 天」 ("vs prev 7 days")
fn compare(g: &Range, i: usize, long: bool) -> String {
    match i {
        0 if g.prev > 0.0 => tr!("昨天 {}", "yesterday {}", money(g.prev)),
        1 | 2 if g.prev > 0.0 => {
            let d = (g.cost - g.prev) / g.prev * 100.0;
            let s = if d.abs() < 0.5 {
                t!("持平", "flat").to_string()
            } else {
                format!("{}{:.0}%", if d > 0.0 { "↑" } else { "↓" }, d.abs())
            };
            if long { tr!("比{} {s}", "vs {} {s}", ["", t!("前 7 天", "prev 7 days"), t!("前 30 天", "prev 30 days")][i]) } else { s }
        }
        3 if g.days > 0 && g.cost > 0.0 => tr!("日均 {}", "{}/day", money(g.cost / g.days as f64)),
        _ => String::new(),
    }
}

/// Trend: oldest first
fn oldest_first(g: &Range) -> Vec<f64> {
    g.series.iter().rev().map(|x| x.1).collect()
}

/// The line under the selected period: daily average over the last 7 / 30 days (All has its daily average on its own row, Today
/// has none), and the day All starts from
fn picked_note(r: &Report, g: &Range, sel: usize) -> String {
    match (sel, r.first) {
        (1 | 2, _) if g.days > 0 && g.cost > 0.0 => tr!("日均 {}", "{}/day", money(g.cost / g.days as f64)),
        (3, Some(f)) => {
            let (y, m, d) = civil_from_days(local(f, r.off).0);
            tr!("{y}/{m}/{d} 起", "since {y}/{m}/{d}")
        }
        _ => String::new(),
    }
}

fn totals_block(r: &Report, sel: usize, w: usize) -> Vec<Line<'static>> {
    let mut out = vec![head(t!("按 API 价格", "At API prices"), "", w)];
    let fixed = 3 + 8 + MONEY_W;
    let pick = |long: bool| -> Vec<String> { r.ranges.iter().enumerate().map(|(i, g)| compare(g, i, long)).collect() };
    let fits = |c: &[String]| fixed + 2 + c.iter().map(|x| x.width()).max().unwrap_or(0) <= w;
    let cmps = [pick(true), pick(false)].into_iter().find(|c| fits(c)).unwrap_or_default();
    let cmp_w = cmps.iter().map(|x| x.width()).max().unwrap_or(0);
    for (i, g) in r.ranges.iter().enumerate() {
        let on = i == sel;
        let bold = if on { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
        let mut spans = vec![
            Span::styled(if on { " ▸ " } else { "   " }, theme::accent()),
            Span::styled(lpad(RANGES[i].get(), 8), bold),
            Span::styled(rpad(&money(g.cost), MONEY_W), bold),
        ];
        let mut used = fixed;
        if cmp_w > 0 {
            spans.push(Span::styled(format!("  {}", lpad(&cmps[i], cmp_w)), dim()));
            used += 2 + cmp_w;
        }
        let room = w.saturating_sub(used + 2);
        let vals = oldest_first(g);
        if room >= 6 && vals.iter().any(|&v| v > 0.0) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(spark(&squeeze(&vals, room)), Style::new().fg(theme::pal().total)));
        }
        out.push(Line::from(spans));
        let note = if on { picked_note(r, g, i) } else { String::new() };
        if !note.is_empty() {
            // Line up with the comparison column; shift left if it doesn't fit
            let at = (fixed + 2).min(w.saturating_sub(note.width())).max(3);
            out.push(Line::styled(format!("{}{note}", " ".repeat(at)), dim()));
        }
    }
    out
}

// ---------- The selected period ----------

/// What `m` shows: the selected period's calls, tokens, cache and so on, and where the money went (the daily average is in the
/// periods block)
fn usage_block(g: &Range, sel: usize, w: usize) -> Vec<Line<'static>> {
    let mut out = vec![head(t!("用量", "Usage"), "", w)];
    let mut facts = vec![fact(t!("调用 ", ""), count(g.calls), t!(" 次", " calls")), fact("", tokens(g.tokens), t!(" token", " tokens"))];
    if sel > 0 && g.days > 0 {
        facts.push(fact(&tr!("{} 天里用了 ", "of {} days, active on ", g.days), g.active.to_string(), t!(" 天", "")));
    }
    if g.calls > 0 {
        facts.push(fact(t!("平均每次 ", "per call "), money(g.cost / g.calls as f64), ""));
    }
    let ctx = g.toks[0] + g.toks[1] + g.toks[2];
    if ctx > 0 {
        facts.push(fact(t!("缓存命中 ", "cache hits "), format!("{:.0}%", g.toks[2] as f64 / ctx as f64 * 100.0), ""));
    }
    if g.saved >= 0.005 {
        facts.push(fact(t!("缓存节省 ", "cache saved "), money(g.saved), ""));
    }
    flow(&mut out, facts, " · ", w);
    if g.parts.iter().sum::<f64>() <= 0.0 {
        return out;
    }
    // Where the money went: one segmented bar + legend. Cache reads are often the biggest part
    let p = theme::pal();
    let colors = p.parts;
    let cells = split(&g.parts, w.saturating_sub(3));
    let mut bar = vec![Span::raw("  ")];
    let last = cells.iter().rposition(|n| *n > 0);
    for (i, (n, c)) in cells.iter().zip(colors).enumerate() {
        if *n > 0 {
            // Leave a sliver of background between segments
            let end = if Some(i) == last { "█" } else { "▉" };
            bar.push(Span::styled(format!("{}{end}", "█".repeat(*n - 1)), Style::new().fg(c)));
        }
    }
    out.push(Line::from(bar));
    let legend = (0..PARTS.len())
        .filter(|&i| g.parts[i] > 0.0)
        .map(|i| {
            vec![
                Span::styled("■ ", Style::new().fg(colors[i])),
                Span::styled(format!("{} ", PARTS[i]), dim()),
                Span::raw(money(g.parts[i])),
                Span::styled(format!(" {}", pct(g.parts[i], g.cost)), dim()),
            ]
        })
        .collect();
    flow(&mut out, legend, "   ", w);
    out
}

/// The text on a tick: "10:00" → "10", "9/30 Wed" → "9/30"
fn short_label(l: &str) -> &str {
    l.split([' ', ':']).next().unwrap_or(l)
}

/// What goes on the time axis: by hour, one every 3 / 6 hours; 7 days, one a day; 30 days, one each Monday
fn tick(label: &str, sel: usize, dense: bool) -> Option<String> {
    match sel {
        0 => {
            let h: usize = label.get(..2).and_then(|x| x.parse().ok()).unwrap_or(99);
            h.is_multiple_of(if dense { 3 } else { 6 }).then(|| h.to_string())
        }
        1 => Some(short_label(label).to_string()),
        // Compare against the same table the labels came from, so the weekday spelling matches in any language
        _ => label.ends_with(WEEK[1].get()).then(|| short_label(label).to_string()),
    }
}

/// Trend split by account: accounts with a color and at least 3% of this period each get their own segment, stacked by color slot
/// (neighbors in the stack are neighbors in the palette, pairs verified to be told apart); the rest merge into a gray 「其他」 ("other").
/// If there can't be two segments, there's just one (the color that belongs to no account).
/// vals[bucket][segment], oldest first; each bucket's segments add up to that bucket's total
struct Stacks {
    names: Vec<(String, Color)>,
    vals: Vec<Vec<f64>>,
}

/// Account colors: palette entries (theme's series) by the slot recorded in slots, sorted by slot. Accounts without a slot
/// aren't in it and are drawn gray.
/// The trend segments, quotas and the 「按账号」 ("By account") / 「按会话」 ("By session") bars all use this one set, so an
/// account has the same color everywhere
fn account_palette(r: &Report) -> Vec<(String, Color)> {
    let p = theme::pal();
    let mut v: Vec<&(String, usize)> = r.colors.iter().filter(|x| x.1 < p.series.len()).collect();
    v.sort_by_key(|x| x.1);
    v.into_iter().map(|(n, s)| (n.clone(), p.series[*s])).collect()
}

fn color_of(pal: &[(String, Color)], name: &str) -> Color {
    pal.iter().find(|x| x.0 == name).map_or(theme::pal().other, |x| x.1)
}

/// n = how many buckets to draw (Today fills all 24 hours; hours not reached yet are 0)
fn stacks(g: &Range, n: usize, pal: &[(String, Color)]) -> Stacks {
    let pad = |v: Vec<f64>| -> Vec<f64> { (0..n).map(|i| v.get(i).copied().unwrap_or(0.0)).collect() };
    let total = pad(g.series.iter().rev().map(|x| x.1).collect());
    let p = theme::pal();
    let names: Vec<(String, Color)> = pal
        .iter()
        .filter(|(name, _)| {
            g.cost > 0.0 && g.src_series.iter().find(|x| &x.0 == name).is_some_and(|x| x.2.iter().sum::<f64>() >= g.cost * 0.03)
        })
        .cloned()
        .collect();
    let cols: Vec<Vec<f64>> = names
        .iter()
        .map(|(name, _)| g.src_series.iter().find(|x| &x.0 == name).map_or(vec![0.0; n], |x| pad(x.2.iter().rev().copied().collect())))
        .collect();
    let (mut names, mut cols) = (names, cols);
    let other: Vec<f64> = (0..n).map(|i| (total[i] - cols.iter().map(|c| c[i]).sum::<f64>()).max(0.0)).collect();
    if other.iter().sum::<f64>() > g.cost * 0.005 {
        names.push((t!("其他", "other").into(), p.other));
        cols.push(other);
    }
    // Only one segment: a single account (say, when only it is shown) uses its own color
    if names.len() < 2 {
        let c = names.first().map_or(p.total, |x| x.1);
        return Stacks { names: vec![(String::new(), c)], vals: total.into_iter().map(|t| vec![t]).collect() };
    }
    Stacks { vals: (0..n).map(|i| cols.iter().map(|c| c[i]).collect()).collect(), names }
}

/// The "lower part" of a cell growing upward: 1/8 … 7/8 of a cell
const LOWER: [char; 8] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇'];

/// Draw one segmented vertical bar bottom-up as a run of cells: vals are the segment values, the whole bar is e eighths high.
/// A cell holding a boundary between two segments uses a "lower part" glyph, foreground the lower segment's color, background
/// the upper one's, so boundaries are also precise to 1/8 cell;
/// when the bar's last cell isn't full, it takes the color of the segment that fills most of that cell
fn cells(vals: &[f64], e: usize, colors: &[Color]) -> Vec<(char, Style)> {
    let total: f64 = vals.iter().sum();
    if e == 0 || total <= 0.0 {
        return vec![];
    }
    let mut acc = 0.0;
    let bounds: Vec<usize> = vals
        .iter()
        .map(|v| {
            acc += v;
            (acc / total * e as f64).round() as usize
        })
        .collect();
    let seg = |x: usize| bounds.iter().position(|&b| x < b).unwrap_or(bounds.len() - 1);
    (0..e.div_ceil(8))
        .map(|c| {
            let (a, z) = (c * 8, (c * 8 + 8).min(e));
            if z - a < 8 {
                let most = (a..z).map(seg).fold(HashMap::new(), |mut m, k| {
                    *m.entry(k).or_insert(0) += 1;
                    m
                });
                let k = most.into_iter().max_by_key(|&(k, n)| (n, std::cmp::Reverse(k))).map_or(0, |x| x.0);
                return (LOWER[z - a], Style::new().fg(colors[k]));
            }
            let (first, last) = (seg(a), seg(z - 1));
            if first == last {
                return ('█', Style::new().fg(colors[first]));
            }
            (LOWER[bounds[first] - a], Style::new().fg(colors[first]).bg(colors[last]))
        })
        .collect()
}

/// One segmented horizontal bar, e half cells long; each cell takes the color of the segment covering its left half (a thin bar
/// can't change color within a cell, so boundaries fall on whole cells)
fn hcells(vals: &[f64], e: usize, colors: &[Color]) -> Vec<(char, Style)> {
    let total: f64 = vals.iter().sum();
    if e == 0 || total <= 0.0 {
        return vec![];
    }
    let mut acc = 0.0;
    let bounds: Vec<usize> = vals
        .iter()
        .map(|v| {
            acc += v;
            (acc / total * e as f64).round() as usize
        })
        .collect();
    let seg = |x: usize| bounds.iter().position(|&b| x < b).unwrap_or(bounds.len() - 1);
    bar(e).chars().enumerate().map(|(c, ch)| (ch, Style::new().fg(colors[seg(c * 2)]))).collect()
}

/// Merge a run of (char, style) into a few Spans
fn spans_of(cs: impl IntoIterator<Item = (char, Style)>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = vec![];
    for (ch, st) in cs {
        match out.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push(ch),
            _ => out.push(Span::styled(ch.to_string(), st)),
        }
    }
    out
}

/// Horizontal bar chart: one row per bucket (newest first), bars split by account, the bucket's total at the end of the row
fn hbars(out: &mut Vec<Line<'static>>, labels: &[String], st: &Stacks, w: usize) {
    let label_w = labels.iter().map(|l| l.width()).max().unwrap_or(0);
    let bar_w = w.saturating_sub(2 + label_w + 2 + 1 + MONEY_W);
    let sums: Vec<f64> = st.vals.iter().map(|v| v.iter().sum()).collect();
    let max = sums.iter().copied().fold(0.0, f64::max);
    let colors: Vec<Color> = st.names.iter().map(|x| x.1).collect();
    for i in (0..labels.len()).rev() {
        let bar = hcells(&st.vals[i], halves(sums[i], max, bar_w), &colors);
        let style = if sums[i] > 0.0 { Style::new() } else { dim() };
        let pad = bar_w - bar.len() + 1;
        let mut line = vec![Span::styled(format!("  {}  ", lpad(&labels[i], label_w)), style)];
        line.extend(spans_of(bar));
        line.push(Span::raw(" ".repeat(pad)));
        line.push(Span::styled(rpad(&money(sums[i]), MONEY_W), style));
        out.push(Line::from(line));
    }
}

/// Vertical bar chart: one bar per bucket (oldest first), stacked upward by account; a baseline and ticks below; when too many to
/// fit, keep only the newest
fn columns(out: &mut Vec<Line<'static>>, labels: &[String], st: &Stacks, sel: usize, w: usize) {
    let rows = if w >= 60 { 8 } else { 6 };
    let avail = w.saturating_sub(3);
    let n0 = labels.len();
    let (cell, gap) = match avail {
        a if a >= n0 * 3 => (2, 1),
        a if a >= n0 * 2 => (1, 1),
        _ => (1, 0),
    };
    let step = cell + gap;
    let skip = n0.saturating_sub((avail + gap) / step);
    let (labels, vals) = (&labels[skip..], &st.vals[skip..]);
    let sums: Vec<f64> = vals.iter().map(|v| v.iter().sum()).collect();
    let max = sums.iter().copied().fold(0.0, f64::max);
    let colors: Vec<Color> = st.names.iter().map(|x| x.1).collect();
    // Each bar's cells, bottom up
    let bars: Vec<Vec<(char, Style)>> = vals
        .iter()
        .zip(&sums)
        .map(|(v, &t)| {
            let e = if max > 0.0 && t > 0.0 { ((t / max) * (rows * 8) as f64).round().max(1.0) as usize } else { 0 };
            cells(v, e, &colors)
        })
        .collect();
    for row in (0..rows).rev() {
        let mut cs = vec![(' ', Style::new()), (' ', Style::new())];
        for (i, b) in bars.iter().enumerate() {
            let c = b.get(row).copied().unwrap_or((' ', Style::new()));
            cs.extend(std::iter::repeat_n(c, cell));
            if gap > 0 && i + 1 < bars.len() {
                cs.push((' ', Style::new()));
            }
        }
        out.push(Line::from(spans_of(cs)));
    }
    out.push(Line::styled(format!("  {}", "─".repeat(bars.len() * step - gap)), theme::faint()));
    let mut ticks = String::from("  ");
    let mut end = 0;
    for (i, label) in labels.iter().enumerate() {
        let Some(text) = tick(label, sel, step >= 2) else { continue };
        let pos = (2 + i * step).min(w.saturating_sub(text.width()));
        if (end == 0 || pos > end) && pos >= ticks.width() {
            ticks.push_str(&" ".repeat(pos - ticks.width()));
            ticks.push_str(&text);
            end = pos + text.width();
        }
    }
    if end > 0 {
        out.push(Line::styled(ticks, dim()));
    }
}

fn trend_block(g: &Range, sel: usize, pal: &[(String, Color)], w: usize) -> Vec<Line<'static>> {
    if g.series.is_empty() {
        return vec![];
    }
    let mut labels: Vec<String> = g.series.iter().rev().map(|x| x.0.clone()).collect();
    // Today is drawn as all 24 hours; hours not reached yet stay blank
    if sel == 0 {
        labels.extend((labels.len()..24).map(|h| format!("{h:02}:00")));
    }
    let note = match g.series.iter().filter(|x| x.1 > 0.0).max_by(|a, b| a.1.total_cmp(&b.1)) {
        Some((l, c)) if sel == 0 => tr!("最高 {} 点 {}", "peak at {}:00 {}", short_label(l).parse::<u32>().unwrap_or(0), money(*c)),
        Some((l, c)) => tr!("最高 {} {}", "peak {} {}", short_label(l), money(*c)),
        None => String::new(),
    };
    let mut out = vec![head([t!("按小时", "By hour"), t!("按天", "By day"), t!("按天", "By day"), t!("按月", "By month")][sel], &note, w)];
    // 7 days and All (few buckets) are horizontal bars with the amount at the end of the row; Today and 30 days (many buckets) are
    // vertical bars
    let st = stacks(g, labels.len(), pal);
    if sel == 1 || sel == 3 {
        hbars(&mut out, &labels, &st, w);
    } else {
        columns(&mut out, &labels, &st, sel, w);
    }
    if st.names.len() > 1 {
        let items = st
            .names
            .iter()
            .map(|(name, c)| {
                let mut v = vec![Span::styled("■ ", Style::new().fg(*c))];
                v.extend(family_spans(crate::usage::account_shown(name), None, dim()));
                v
            })
            .collect();
        flow(&mut out, items, "   ", w);
    }
    out
}

/// Weekday × hour: one solid cell per hour, darker the more was spent (theme's four heat levels; the square root spreads them
/// out, or only the few peak cells would show)
fn heat_block(g: &Range, sel: usize, w: usize) -> Vec<Line<'static>> {
    let max = g.heat.iter().flatten().copied().fold(0.0, f64::max);
    if sel == 0 || max <= 0.0 {
        return vec![];
    }
    let (pw, ph) =
        (0..7).flat_map(|d| (0..24).map(move |h| (d, h))).max_by(|a, b| g.heat[a.0][a.1].total_cmp(&g.heat[b.0][b.1])).unwrap_or((0, 0));
    let mut out = vec![head(t!("按星期和钟点", "By weekday and hour"), &tr!("{} {ph} 点最多", "most on {} at {ph}:00", WEEK[pw]), w)];
    let cell = if w >= 7 + 48 { 2 } else { 1 };
    let every = if cell == 2 { 3 } else { 6 };
    let mut axis = " ".repeat(7);
    for h in (0..24).step_by(every) {
        axis.push_str(&lpad(&h.to_string(), every * cell));
    }
    out.push(Line::styled(axis.trim_end().to_string(), dim()));
    let ramp = theme::pal().heat;
    for d in [1, 2, 3, 4, 5, 6, 0] {
        let mut spans = vec![Span::styled(format!("  {} ", WEEK[d]), dim())];
        for v in g.heat[d] {
            // Hours with no spend are a cell too (background color), so the seven rows form a neat block; cells are a hair short, so the
            // background shows between rows
            let lv = if v > 0.0 { (((v / max).sqrt() * 4.0).ceil() as usize).clamp(1, 4) } else { 0 };
            let c = if lv == 0 { theme::pal().track } else { ramp[lv - 1] };
            spans.push(Span::styled("▇".repeat(cell), Style::new().fg(c)));
        }
        out.push(Line::from(spans));
    }
    out
}

// ---------- By account / model / session / directory ----------

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Src,
    Model,
    Session,
    Dir,
}

/// Name (+ gray note), a bar scaled to the largest row, amount, share
/// By account and By session bars use the account colors (palette; accounts without a slot are gray); By model and By
/// directory aren't split by account and use theme's total.
/// Names are all in the body text color: color is only on the bars, names aren't painted to match
/// Not drawn when there's only one row (a single account or model: it's all theirs)
fn rows_block(title: &str, rows: &[Row], total: f64, top: usize, kind: Kind, palette: &[(String, Color)], w: usize) -> Vec<Line<'static>> {
    if rows.len() < 2 {
        return vec![];
    }
    // The count isn't written: the extra last row says 「其余 N 个」 ("N more")
    let mut out = vec![head(title, "", w)];
    let shown = &rows[..rows.len().min(top)];
    let rest = &rows[shown.len()..];
    // If the gray note (a session's account) is the same on every row (only one account), leave it out
    let same_note = shown.iter().all(|r| r.note == shown[0].note);
    let tail = 1 + MONEY_W + 5;
    // Name, gray note (a session's account, its own column) and bar share the remaining space. Name first (at least NAME_MIN),
    // then the note, the bar last
    const NAME_MIN: usize = 14;
    const BAR_MIN: usize = 6;
    let base = w.saturating_sub(2 + tail);
    let name_need = shown.iter().map(|r| r.name.width()).max().unwrap_or(0).max(if rest.is_empty() { 0 } else { 9 });
    let note_most = if same_note { 0 } else { shown.iter().map(|r| r.note.width()).max().unwrap_or(0) };
    let note_need = if note_most > 0 { note_most + 2 } else { 0 };
    let (name_col, note_col, bar_w) = if name_need + note_need + 1 + BAR_MIN <= base {
        (name_need, note_most, base - name_need - note_need - 1)
    } else if base >= NAME_MIN + note_need + 1 + BAR_MIN {
        (base - note_need - 1 - BAR_MIN, note_most, BAR_MIN)
    } else if base >= NAME_MIN + note_need {
        (base - note_need, note_most, 0)
    } else {
        (base.max(6), 0, 0)
    };
    let max = shown.iter().map(|r| r.cost).fold(0.0, f64::max);
    let line = |r: &Row, faint: bool| -> Line<'static> {
        let name_style = if faint { dim() } else { Style::new() };
        let name = match kind {
            Kind::Dir => clip_left(&r.name, name_col),
            Kind::Session => clip_name(&r.name, name_col),
            Kind::Src => clip(crate::usage::account_shown(&r.name), name_col),
            Kind::Model => clip(&r.name, name_col),
        };
        let mut spans = vec![Span::raw("  ")];
        if kind == Kind::Src && !faint {
            spans.extend(family_spans(&name, Some(r.agent), name_style));
        } else {
            spans.push(Span::styled(name, name_style));
        }
        if note_col > 0 {
            spans.push(Span::styled(format!("  {}", lpad(&r.note, note_col)), dim()));
        }
        spans.push(Span::raw(" "));
        if bar_w > 0 {
            let bar = if faint { String::new() } else { hbar(r.cost, max, bar_w) };
            let bar_style = match kind {
                Kind::Src | Kind::Session => {
                    // A session's gray note is its account name; join it back into "Claude <name>" to look it up
                    let who = if kind == Kind::Src {
                        r.name.clone()
                    } else {
                        format!("{} {}", if r.agent == Agent::Codex { "Codex" } else { "Claude" }, r.note)
                    };
                    Style::new().fg(color_of(palette, &who))
                }
                Kind::Model | Kind::Dir => Style::new().fg(theme::pal().total),
            };
            spans.push(Span::styled(bar.clone(), bar_style));
            spans.push(Span::raw(" ".repeat(bar_w - bar.width())));
        }
        spans.push(Span::styled(rpad(&money(r.cost), MONEY_W), if faint { dim() } else { Style::new() }));
        spans.push(Span::styled(rpad(&pct(r.cost, total), 5), dim()));
        Line::from(spans)
    };
    out.extend(shown.iter().map(|r| line(r, false)));
    if !rest.is_empty() {
        let r = Row { name: tr!("其余 {} 个", "{} more", rest.len()), cost: rest.iter().map(|r| r.cost).sum(), ..Default::default() };
        out.push(line(&r, true));
    }
    out
}

fn unpriced_block(g: &Range, w: usize) -> Vec<Line<'static>> {
    if g.unpriced.is_empty() {
        return vec![];
    }
    let mut out = vec![head(t!("未计价的模型", "Unpriced models"), "", w)];
    for (m, t) in &g.unpriced {
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::raw(clip(m, w.saturating_sub(18).max(8))),
            Span::styled(format!(" {} token", tokens(*t)), dim()),
        ]));
    }
    out
}

// ---------- Layout ----------

/// Stack blocks downward, a blank line between blocks
fn stack(blocks: Vec<Vec<Line<'static>>>) -> Vec<Line<'static>> {
    let mut out = vec![];
    for b in blocks.into_iter().filter(|b| !b.is_empty()) {
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(b);
    }
    out
}

/// Two columns side by side: pad the left one with spaces to cw
fn side_by_side(left: Vec<Line<'static>>, right: Vec<Line<'static>>, cw: usize) -> Vec<Line<'static>> {
    let n = left.len().max(right.len());
    let mut l = left.into_iter();
    let mut r = right.into_iter();
    (0..n)
        .map(|_| {
            let mut spans = l.next().map(|x| x.spans).unwrap_or_default();
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            if let Some(rl) = r.next().filter(|x| !x.spans.is_empty()) {
                spans.push(Span::raw(" ".repeat(cw.saturating_sub(used) + GAP)));
                spans.extend(rl.spans);
            }
            Line::from(spans)
        })
        .collect()
}

/// The whole panel's content (without title and footer). sel = the selected period (index into RANGES); more = `m` shows the
/// hidden blocks
pub fn lines(r: &Report, sel: usize, more: bool, w: usize) -> Vec<Line<'static>> {
    if r.ranges.is_empty() {
        let w = w.min(MAX_W);
        return stack(vec![quota_block(r, w), vec![head(t!("按 API 价格", "At API prices"), "", w)]]);
    }
    let sel = sel.min(r.ranges.len() - 1);
    let g = &r.ranges[sel];
    let pal = account_palette(r);
    let usage = |w| if more { usage_block(g, sel, w) } else { vec![] };
    let heat = |w| if more { heat_block(g, sel, w) } else { vec![] };
    let dirs = |w| if more { rows_block(t!("按目录", "By directory"), &g.by_dir, g.cost, TOP_DIRS, Kind::Dir, &[], w) } else { vec![] };
    if w >= TWO_COL {
        let cw = ((w - GAP) / 2).min(COL_W);
        let quota = quota_block(r, cw);
        let (tl, tr) = if quota.is_empty() {
            (totals_block(r, sel, cw), usage(cw))
        } else {
            (quota, stack(vec![totals_block(r, sel, cw), usage(cw)]))
        };
        // Left: the trend with 「按模型」 ("By model") below it (not split by account, same kind as By directory); right: the two
        // account-colored blocks. The two columns come out about as long
        let left = stack(vec![
            trend_block(g, sel, &pal, cw),
            rows_block(t!("按模型", "By model"), &g.by_model, g.cost, usize::MAX, Kind::Model, &[], cw),
            heat(cw),
            dirs(cw),
            unpriced_block(g, cw),
        ]);
        let right = stack(vec![
            rows_block(t!("按账号", "By account"), &g.by_src, g.cost, usize::MAX, Kind::Src, &pal, cw),
            rows_block(t!("按会话", "By session"), &g.by_session, g.cost, TOP_SESSIONS, Kind::Session, &pal, cw),
        ]);
        let mut out = side_by_side(tl, tr, cw);
        out.push(Line::default());
        out.extend(side_by_side(left, right, cw));
        return out;
    }
    let w = w.min(MAX_W);
    stack(vec![
        quota_block(r, w),
        totals_block(r, sel, w),
        usage(w),
        trend_block(g, sel, &pal, w),
        heat(w),
        rows_block(t!("按账号", "By account"), &g.by_src, g.cost, usize::MAX, Kind::Src, &pal, w),
        rows_block(t!("按模型", "By model"), &g.by_model, g.cost, usize::MAX, Kind::Model, &[], w),
        rows_block(t!("按会话", "By session"), &g.by_session, g.cost, TOP_SESSIONS, Kind::Session, &pal, w),
        dirs(w),
        unpriced_block(g, w),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Win;

    fn text(ls: &[Line]) -> Vec<String> {
        ls.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn money_and_tokens_read_at_a_glance() {
        assert_eq!(money(0.0), "$0.00");
        assert_eq!(money(0.001), "<$0.01");
        assert_eq!(money(12.345), "$12.35");
        assert_eq!(money(999.99), "$999.99");
        assert_eq!(money(1234.5), "$1,235");
        assert_eq!(money(1234567.0), "$1,234,567");
        assert_eq!(count(16234), "16,234");
        assert_eq!(tokens(980), "980");
        assert_eq!(tokens(12_000), "1.2 万");
        assert_eq!(tokens(10_000), "1 万");
        assert_eq!(tokens(340_000_000), "3.4 亿");
        assert_eq!((pct(1.0, 400.0), pct(0.0, 1.0), pct(53.0, 100.0)), ("<1%".into(), "0%".into(), "53%".into()));
    }

    #[test]
    fn bars_are_precise_to_half_a_cell_and_never_vanish() {
        assert_eq!(hbar(50.0, 100.0, 4), "▄▄");
        assert_eq!(hbar(40.0, 100.0, 4), "▄▖");
        assert_eq!(hbar(0.01, 100.0, 4), "▖");
        assert_eq!(hbar(0.0, 100.0, 4), "");
        assert_eq!(hbar(200.0, 100.0, 4), "▄▄▄▄");
        // Quota meter: always half a cell between used and unused, the two parts adding up to exactly that width
        let m = |u| meter(u, 4);
        assert_eq!(m(0.0), ("".into(), "▄▄▄▄".into()));
        assert_eq!(m(1.0), ("▖".into(), "▄▄▄".into()));
        assert_eq!(m(50.0), ("▄▄".into(), "▗▄".into()));
        assert_eq!(m(60.0), ("▄▄▖".into(), "▄".into()));
        assert_eq!(m(100.0), ("▄▄▄▄".into(), "".into()));
        assert_eq!(spark(&[0.0, 1.0, 4.0, 8.0]), " ▁▄█");
        assert_eq!(clip_left("~/workspace/fun/app", 10), "…e/fun/app");
        assert_eq!(clip_name("some-long-project-name · 9/24", 17), "some-long… · 9/24");
        assert_eq!(clip_name("一个很长很长的标题", 10), "一个很长… ");
        assert_eq!(short_label("09:00"), "09");
        assert_eq!(short_label("9/30 周三"), "9/30");
    }

    #[test]
    fn a_stacked_bar_uses_every_cell_and_keeps_tiny_parts_visible() {
        let n = split(&[1.0, 50.0, 100.0, 59.0, 0.0], 40);
        assert_eq!(n.iter().sum::<usize>(), 40);
        assert!(n[0] >= 1 && n[4] == 0 && n[2] > n[3] && n[3] > n[1]);
        assert_eq!(split(&[0.0; 5], 40), vec![0; 5]);
        assert_eq!(split(&[1.0, 1.0, 1.0], 2).iter().sum::<usize>(), 2);
    }

    #[test]
    fn quota_warns_only_when_the_pace_runs_out_before_the_reset() {
        assert_eq!(window_secs("5h"), Some(5 * 3600));
        assert_eq!(window_secs("7d"), Some(7 * 86400));
        assert_eq!(window_secs("300m"), Some(300 * 60));
        assert_eq!(window_secs("?"), None);
        let day = 86400;
        // 7-day window half gone with 60% used: at this pace it runs out in 5.83 days, before the reset
        assert_eq!(runs_out(60.0, 1000 + 7 * day / 2, 7 * day, 1000), Some(1000 + (40.0 / 60.0 * (7 * day / 2) as f64) as i64));
        // Half gone with 30% used: enough
        assert_eq!(runs_out(30.0, 1000 + 7 * day / 2, 7 * day, 1000), None);
        // Too early to tell, or already used up: no guess
        assert_eq!(runs_out(5.0, 1000 + 7 * day - 60, 7 * day, 1000), None);
        assert_eq!(runs_out(100.0, 1000 + day, 7 * day, 1000), None);
    }

    #[test]
    fn a_per_model_week_lines_up_with_5h_and_7d_and_gets_a_pace_too() {
        let (day, now) = (86400, 1_000_000);
        let win = |label: &str, used, resets| Win { label: label.into(), used, resets: Some(resets) };
        let wins = vec![win("5h", 12.0, now + 3600), win("7d", 40.0, now + 3 * day), win("Fable", 80.0, now + 3 * day)];
        let q = Quota { agent: Agent::Claude, who: "sub".into(), plan: String::new(), updated: now, wins, note: String::new() };
        let mut out = vec![];
        quota_lines(&mut out, &q, Color::Reset, (now, 0), 80, label_width(std::slice::from_ref(&q)));
        let t = text(&out);
        // Bars start in the same column; Fable is 80% used four days in, runs out before the reset
        assert_eq!(t.len(), 5, "{t:#?}");
        assert!(t[1].starts_with("      5h ") && t[2].starts_with("      7d ") && t[3].starts_with("   Fable "), "{t:#?}");
        assert!(t[4].contains("按当前速度"), "{t:#?}");
    }

    fn report() -> Report {
        let now = crate::usage::parse_ts("2026-09-30T02:00:00Z");
        let row = |name: &str, note: &str, agent, cost| Row { name: name.into(), note: note.into(), agent, cost, tokens: 1, calls: 1 };
        let mut sessions: Vec<Row> = (0..20).map(|i| row(&format!("会话{i}"), "work", Agent::Claude, 20.0 - i as f64)).collect();
        sessions[0].name = "一个特别特别特别特别特别特别长的会话标题".into();
        let dirs: Vec<Row> =
            (0..10).map(|i| row(&format!("~/workspace/some/very/deep/project-{i}"), "", Agent::None, 30.0 - i as f64)).collect();
        let mut heat = [[0.0; 24]; 7];
        heat[2][15] = 80.0;
        heat[2][9] = 5.0;
        heat[5][22] = 20.0;
        let today = Range {
            cost: 210.0,
            parts: [1.0, 50.0, 100.0, 59.0, 0.0],
            tokens: 340_000_000,
            toks: [1_000_000, 9_000_000, 300_000_000, 30_000_000],
            calls: 1234,
            saved: 1500.0,
            prev: 323.3,
            days: 1,
            active: 1,
            series: vec![("10:00".into(), 200.0), ("09:00".into(), 10.0)]
                .into_iter()
                .chain((0..9).rev().map(|h| (format!("{h:02}:00"), 0.0)))
                .collect(),
            heat,
            src_series: vec![
                ("Claude work".into(), Agent::Claude, [180.0, 10.0].into_iter().chain([0.0; 9]).collect()),
                ("Codex cx".into(), Agent::Codex, [20.0].into_iter().chain([0.0; 10]).collect()),
            ],
            by_src: vec![row("Claude work", "", Agent::Claude, 190.0), row("Codex cx", "", Agent::Codex, 20.0)],
            by_model: vec![row("claude-opus-5-5", "", Agent::Claude, 200.0), row("gpt-6-astra", "", Agent::Codex, 10.0)],
            by_session: sessions,
            by_dir: dirs,
            unpriced: vec![("gpt-5.5-codex".into(), 12_000)],
        };
        let days: Vec<(String, f64)> =
            (0..30).map(|i| (format!("9/{} {}", 30 - i, WEEK[(3 + 7 * 5 - i) % 7]), (i * 37 % 11) as f64 * 10.0)).collect();
        let week = Range {
            cost: 1500.0,
            prev: 1650.0,
            days: 7,
            active: 6,
            calls: 9000,
            series: days[..7].to_vec(),
            heat,
            by_src: today.by_src.clone(),
            ..Default::default()
        };
        let month = Range { cost: 13000.0, prev: 10000.0, days: 30, active: 26, series: days.clone(), heat, ..Default::default() };
        let all = Range {
            cost: 42000.0,
            days: 208,
            active: 190,
            series: vec![("2026-09".into(), 13000.0), ("2026-08".into(), 0.0), ("2026-07".into(), 29000.0)],
            heat,
            ..Default::default()
        };
        Report {
            now,
            off: 8 * 3600,
            quotas: vec![
                Quota {
                    agent: Agent::Claude,
                    who: "work".into(),
                    plan: String::new(),
                    wins: vec![
                        Win { label: "5h".into(), used: 12.0, resets: Some(now + 3 * 3600) },
                        Win { label: "7d".into(), used: 74.0, resets: Some(now + 2 * 86400) },
                    ],
                    updated: now - 180,
                    note: String::new(),
                },
                Quota {
                    agent: Agent::Codex,
                    who: "cx".into(),
                    plan: "plus".into(),
                    wins: vec![Win { label: "7d".into(), used: 24.0, resets: Some(now - 60) }],
                    updated: now - 86400,
                    note: String::new(),
                },
            ],
            ranges: vec![today, week, month, all],
            first: Some(crate::usage::parse_ts("2026-03-06T01:00:00Z")),
            colors: vec![("Claude work".into(), 0), ("Codex cx".into(), 1)],
            ..Default::default()
        }
    }

    fn find<'a>(t: &'a [String], s: &str) -> (usize, &'a str) {
        let i = t.iter().position(|l| l.contains(s)).unwrap_or_else(|| panic!("没有「{s}」:\n{}", t.join("\n")));
        (i, t[i].as_str())
    }

    #[test]
    fn the_panel_reads_top_down_quota_then_totals_then_breakdowns() {
        let t = text(&lines(&report(), 0, false, 60));
        let at = |s: &str| find(&t, s).0;
        assert!(at("订阅额度") < at("按 API 价") && at("按 API 价") < at("按小时") && at("按小时") < at("按账号"));
        assert!(at("按账号") < at("按模型") && at("按模型") < at("按会话"));
        // A freshly updated quota doesn't say when it was updated; only a stale one does
        assert_eq!(find(&t, "Claude work").1, " ■ Claude work");
        assert!(find(&t, "   5h").1.contains("12%") && find(&t, "   5h").1.contains("今天 13:00 重置"));
        let (i7, l7) = find(&t, "   7d ▄▄");
        assert!(l7.contains("74%") && l7.contains("周五 10:00 重置"));
        // 7 days, 74% used, two days left: at this pace it runs out early
        assert!(t[i7 + 1].contains("按当前速度") && t[i7 + 1].contains("用完"), "{}", t[i7 + 1]);
        assert!(!t[find(&t, "   5h").0 + 1].contains("按当前速度"));
        // Past the reset time: don't make things up
        assert!(find(&t, "已重置").1.ends_with("      已重置"));
        assert!(find(&t, "Codex cx · plus").1.ends_with("1 天前更新"));
        assert!(find(&t, " ▸ 今天").1.contains("$210.00") && find(&t, " ▸ 今天").1.contains("昨天 $323.30"));
        assert!(find(&t, "   近 7 天").1.contains("比前 7 天 ↓9%"));
        assert!(find(&t, "   近 30 天").1.contains("比前 30 天 ↑30%"));
        assert!(find(&t, "   全部").1.contains("$42,000") && find(&t, "   全部").1.contains("日均 $201.92"));
        assert!(find(&t, "按小时").1.contains("最高 10 点 $200.00"));
        // The count is said only once, in the last row; the session's account is the same on every row, so it's left out
        assert!(find(&t, "其余 5 个").1.contains("$15.00"));
        assert!(find(&t, "按会话").1.trim_end().ends_with('─'));
        assert!(!find(&t, "会话3").1.contains("work"));
        assert!(find(&t, "未计价的模型").0 < find(&t, "gpt-5.5-codex").0);
        assert!(find(&t, "gpt-5.5-codex").1.contains("1.2 万 token"));
        // Hidden: usage, where the money went, By directory; Today never draws weekday × hour anyway
        for gone in ["调用 1,234 次", "缓存命中", "■ 缓存读", "按目录", "其余 2 个", "按星期和钟点"] {
            assert!(!t.iter().any(|l| l.contains(gone)), "{gone}");
        }
        // `m` shows them
        let t = text(&lines(&report(), 0, true, 60));
        let at = |s: &str| find(&t, s).0;
        assert!(at("按 API 价") < at("用量") && at("用量") < at("按小时") && at("按会话") < at("按目录"));
        assert!(find(&t, "调用 1,234 次").1.contains("3.4 亿 token"));
        assert!(t.iter().any(|l| l.contains("缓存命中 97%")) && t.iter().any(|l| l.contains("缓存节省 $1,500")));
        assert!(t.iter().any(|l| l.contains("■ 缓存读 $100.00 48%")) && t.iter().any(|l| l.contains("■ 输入 $1.00 <1%")));
        assert!(!t.iter().any(|l| l.contains("搜索")));
        assert!(find(&t, "其余 2 个").1.contains("$43.00"));
        assert!(!t.iter().any(|l| l.contains("按星期和钟点")));
    }

    #[test]
    fn the_picked_range_says_its_daily_average_under_its_row() {
        let t = text(&lines(&report(), 1, false, 60));
        let (i, _) = find(&t, " ▸ 近 7 天");
        assert_eq!(t[i + 1].trim(), "日均 $214.29");
        // All has its daily average on its own row and the start day below it; Today doesn't
        let t = text(&lines(&report(), 3, false, 60));
        assert_eq!(t[find(&t, " ▸ 全部").0 + 1].trim(), "2026/3/6 起");
        let t = text(&lines(&report(), 0, false, 60));
        assert!(t[find(&t, " ▸ 今天").0 + 1].starts_with("   近 7 天"));
    }

    #[test]
    fn a_block_of_one_row_and_a_default_account_name_are_left_out() {
        let mut r = report();
        let g = &mut r.ranges[0];
        g.by_src = vec![Row { name: "Claude claude".into(), agent: Agent::Claude, cost: 210.0, ..Default::default() }];
        g.by_model.truncate(1);
        r.quotas[0].who = "claude".into();
        let t = text(&lines(&r, 0, false, 60));
        assert!(!t.iter().any(|l| l.contains("按账号") || l.contains("按模型")), "{t:#?}");
        assert_eq!(find(&t, "■ Claude").1, " ■ Claude");
        assert_eq!(crate::usage::account_shown("Codex codex"), "Codex");
        assert_eq!(crate::usage::account_shown("Codex cx"), "Codex cx");
    }

    #[test]
    fn today_by_hour_is_24_columns_split_by_account() {
        let t = text(&lines(&report(), 0, false, 80));
        let ls = lines(&report(), 0, false, 80);
        let (i, _) = find(&t, "按小时");
        // 8 rows of vertical bars + baseline + ticks + legend
        assert!(t[i + 9].trim().chars().all(|c| c == '─'));
        let ticks = &t[i + 10];
        for h in ["0", "3", "9", "12", "21"] {
            assert!(ticks.split_whitespace().any(|x| x == h), "刻度没有 {h}:「{ticks}」");
        }
        assert!(t[i + 11].contains("■ Claude work") && t[i + 11].contains("■ Codex cx") && !t[i + 11].contains("其他"));
        // Stacked bottom-up by color slot: the bottom row is all Claude (slot 0); the 10 o'clock bar reaches the top, its top cell
        // half Claude below, half Codex above
        let p = theme::pal();
        let bars = |row: usize| ls[i + row].spans.iter().filter(|s| !s.content.trim().is_empty()).map(|s| s.style).collect::<Vec<_>>();
        assert!(bars(8).iter().all(|st| st.fg == Some(p.series[0]) && st.bg.is_none()), "{:?}", bars(8));
        assert_eq!(bars(1), vec![Style::new().fg(p.series[0]).bg(p.series[1])]);
    }

    #[test]
    fn a_stacked_bar_splits_at_an_eighth_of_a_cell() {
        let (a, b) = (Color::Red, Color::Blue);
        // A vertical bar 12 eighths high: lower segment 4, upper 8 → first cell lower half a, upper half b; second cell half a cell of b
        let c = cells(&[1.0, 2.0], 12, &[a, b]);
        assert_eq!(c, vec![('▄', Style::new().fg(a).bg(b)), ('▄', Style::new().fg(b))]);
        assert_eq!(cells(&[3.0], 16, &[a]), vec![('█', Style::new().fg(a)); 2]);
        assert_eq!(cells(&[0.0], 16, &[a]), vec![]);
        // A horizontal bar of 7 half cells: first segment 2, second 5 → one cell a, two cells b, a closing half cell b
        let st = |c| Style::new().fg(c);
        assert_eq!(hcells(&[2.0, 5.0], 7, &[a, b]), vec![('▄', st(a)), ('▄', st(b)), ('▄', st(b)), ('▖', st(b))]);
        assert_eq!(hcells(&[0.0], 4, &[a]), vec![]);
        assert_eq!(spans_of(vec![('█', Style::new().fg(a)), ('█', Style::new().fg(a)), ('▌', Style::new().fg(b))]).len(), 2);
    }

    #[test]
    fn accounts_keep_their_color_and_small_or_uncolored_ones_fold_into_other() {
        let acct = |name: &str, agent, v: f64| (name.to_string(), agent, vec![v, 0.0]);
        let g = Range {
            cost: 100.0,
            series: vec![("b".into(), 100.0), ("a".into(), 0.0)],
            src_series: vec![
                acct("Claude a", Agent::Claude, 50.0),
                acct("Codex b", Agent::Codex, 30.0),
                acct("Claude c", Agent::Claude, 10.0),
                acct("Codex d", Agent::Codex, 8.0),
                acct("Codex e", Agent::Codex, 2.0),
            ],
            ..Default::default()
        };
        let p = theme::pal();
        // b gets slot 0 first: the color follows the account, not how much it spent; d has no slot (palette full), e is under
        // 3%, both go into 「其他」 ("other")
        let r = Report {
            colors: vec![("Claude a".into(), 1), ("Codex b".into(), 0), ("Claude c".into(), 2), ("Codex e".into(), 3)],
            ..Default::default()
        };
        let pal = account_palette(&r);
        assert_eq!(pal.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["Codex b", "Claude a", "Claude c", "Codex e"]);
        let st = stacks(&g, 2, &pal);
        assert_eq!(
            st.names,
            vec![
                ("Codex b".into(), p.series[0]),
                ("Claude a".into(), p.series[1]),
                ("Claude c".into(), p.series[2]),
                ("其他".into(), p.other)
            ]
        );
        // Oldest first: bucket a is all 0, bucket b is 30 / 50 / 10 / other 8 + 2
        assert_eq!(st.vals, vec![vec![0.0; 4], vec![30.0, 50.0, 10.0, 10.0]]);
        // The By account bars use the same colors; no slot means gray
        assert_eq!((color_of(&pal, "Claude a"), color_of(&pal, "Codex d")), (p.series[1], p.other));
        // Only one account: one segment, no legend
        let one = Range {
            cost: 5.0,
            series: vec![("a".into(), 5.0)],
            src_series: vec![("Claude a".into(), Agent::Claude, vec![5.0])],
            ..Default::default()
        };
        assert_eq!(stacks(&one, 1, &pal).names.len(), 1);
    }

    #[test]
    fn color_means_an_account_and_everything_else_is_the_theme_hue() {
        let p = theme::pal();
        let ls = lines(&report(), 1, true, 60);
        let t = text(&ls);
        let fgs = |i: usize, ch: char| ls[i].spans.iter().filter(|s| s.content.contains(ch)).filter_map(|s| s.style.fg).collect::<Vec<_>>();
        // Quota meter: under 70% it's the color that belongs to no account, the unused part is track; status colors only past 70%
        let (i, _) = find(&t, "   5h");
        assert_eq!((fgs(i, '▖'), fgs(i, '▄')), (vec![p.total], vec![p.total, p.track]));
        assert_eq!(fgs(find(&t, "   7d ▄▄").0, '▄')[0], p.amber);
        // Weekday × hour: hours with no spend are track, the busiest cell is the darkest level
        let (h, _) = find(&t, "按星期和钟点");
        let tue = fgs(h + 3, '▇');
        assert!(tue.contains(&p.track) && tue.contains(&p.heat[3]) && tue.contains(&p.heat[0]), "{tue:?}");
        // By account: each its own color; the small trend chart isn't split by account
        assert_eq!(fgs(find(&t, "  Claude work").0, '▄'), vec![p.series[0]]);
        assert_eq!(fgs(find(&t, "  Codex cx").0, '▄'), vec![p.series[1]]);
        assert_eq!(fgs(find(&t, " ▸ 近 7 天").0, '█'), vec![p.total]);
        // By model isn't split by account; where the money went is the parts segments
        let ls = lines(&report(), 0, true, 60);
        let t = text(&ls);
        let fgs = |i: usize, ch: char| ls[i].spans.iter().filter(|s| s.content.contains(ch)).filter_map(|s| s.style.fg).collect::<Vec<_>>();
        assert_eq!(fgs(find(&t, "  claude-opus-5-5").0, '▄'), vec![p.total]);
        // One color per segment, a sliver of background at the end of each (not the last)
        let bar = &ls[find(&t, "■ 输入").0 - 1].spans[1..];
        assert_eq!(bar.iter().filter_map(|s| s.style.fg).collect::<Vec<_>>(), p.parts[..4].to_vec());
        assert_eq!(bar.iter().map(|s| s.content.chars().last()).collect::<Vec<_>>(), [Some('▉'), Some('▉'), Some('▉'), Some('█')]);
    }

    #[test]
    fn longer_ranges_show_days_and_when_in_the_week() {
        let t = text(&lines(&report(), 2, false, 60));
        let (i, head) = find(&t, "按天");
        assert!(head.contains("最高"));
        let ticks = &t[i + t[i..].iter().position(|l| !l.trim().is_empty() && l.trim().chars().all(|c| c == '─')).unwrap() + 1];
        assert!(ticks.contains("9/28"), "30 天的刻度写周一:{ticks}");
        // Months only have totals (no per-account numbers), so the trend has no legend (the swatches before names in the quota block
        // don't count)
        assert!(!t.iter().any(|l| l.contains("按星期和钟点")), "星期 × 钟点收着");
        let t = text(&lines(&report(), 2, true, 60));
        let (h, hl) = find(&t, "按星期和钟点");
        assert!(!t[i..h].iter().any(|l| l.contains("■ ")));
        assert!(hl.contains("周二 15 点最多"));
        assert!(t[h + 1].trim_start().starts_with('0'));
        assert!(t[h + 3].starts_with("  周二") && t[h + 3].contains('▇'));
        assert!(t[h + 8].starts_with("  周日"));
        assert!(find(&t, "30 天里用了 26 天").0 > find(&t, "日均 $433.33").0);
        // 7 days and All are horizontal bars, newest first, amount at the end of the row
        let t = text(&lines(&report(), 1, false, 60));
        assert!(find(&t, "  9/30 周三").1.ends_with("$0.00") && find(&t, "  9/29 周二").1.contains('▄'));
        let t = text(&lines(&report(), 3, false, 60));
        let (m, _) = find(&t, "按月");
        assert!(t[m + 1].contains("2026-09") && t[m + 2].contains("2026-08") && t[m + 2].contains("$0.00"));
        assert!(find(&t, "2026/3/6 起").0 == find(&t, " ▸ 全部").0 + 1);
    }

    #[test]
    fn nothing_overflows_from_a_phone_to_a_wide_screen() {
        for w in [34, 40, 46, 60, 80, 96, 107, 108, 120, 140, 160, 220] {
            for (sel, more) in (0..4).flat_map(|s| [(s, false), (s, true)]) {
                for l in text(&lines(&report(), sel, more, w)) {
                    assert!(l.width() <= w, "{w} 宽放不下:「{l}」({})", l.width());
                }
            }
        }
    }

    #[test]
    fn a_wide_screen_puts_two_columns_side_by_side() {
        let t = text(&lines(&report(), 1, false, 140));
        assert!(t[0].starts_with(" 订阅额度") && t[0].contains("按 API 价"));
        let (_, l) = find(&t, "按天");
        assert!(l.contains("按账号"), "走势和按账号并排:{l}");
        // Narrow means one column
        assert!(!text(&lines(&report(), 1, false, 100))[0].contains("按 API 价"));
    }

    #[test]
    #[ignore]
    fn show() {
        let w: usize = std::env::var("W").ok().and_then(|x| x.parse().ok()).unwrap_or(60);
        let sel: usize = std::env::var("SEL").ok().and_then(|x| x.parse().ok()).unwrap_or(0);
        // REPORT=<file saved by aca ui --cost-json> to see real data; THEME=mocha for a dark theme (a theme file in THEMES=<dir> works
        // too); ANSI=1 for colors (render it to an image to check the palette); MORE=1 is like pressing m
        let r = std::env::var("REPORT").ok().map_or_else(report, |f| serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap());
        if let Ok(t) = std::env::var("THEME") {
            let dir = std::env::var("THEMES").unwrap_or_default();
            theme::set(Box::leak(Box::new(theme::from_dir(&t, std::path::Path::new(&dir)).unwrap())));
        }
        let ls = lines(&r, sel, std::env::var("MORE").is_ok(), w);
        if std::env::var("ANSI").is_err() {
            return println!("{}", text(&ls).join("\n"));
        }
        let code = |c: Color, base: u8| match c {
            Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
            _ => (base + 9).to_string(),
        };
        for l in ls {
            let mut s = String::new();
            for sp in l.spans {
                let mut sgr = vec!["0".to_string()];
                if sp.style.add_modifier.contains(Modifier::BOLD) {
                    sgr.push("1".into());
                }
                sgr.extend(sp.style.fg.map(|c| code(c, 30)));
                sgr.extend(sp.style.bg.map(|c| code(c, 40)));
                s.push_str(&format!("\x1b[{}m{}", sgr.join(";"), sp.content));
            }
            println!("{s}\x1b[0m");
        }
    }

    #[test]
    fn switching_range_changes_the_breakdown() {
        let t = text(&lines(&report(), 1, false, 60));
        assert!(t.iter().any(|l| l.starts_with(" ▸ 近 7 天") && l.contains("$1,500")));
        assert!(!t.iter().any(|l| l.starts_with(" 近 7 天 ─")), "选中的时段不再起一个标题");
        assert!(!t.iter().any(|l| l.contains("按会话")));
    }
}
