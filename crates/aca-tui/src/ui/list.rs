//! The list itself: each row's segments, tree marks and badges, group headers and folding, and the preview beside it.

use super::*;

/// Bottom bar: several groups of keys, • within a group, │ between groups; group number action is the main action, in the accent color.
/// The whole line is centered. When it doesn't fit, drop by priority from low to high until it does
pub(super) fn menu_bar(groups: &[Vec<MenuItem>], action: Option<usize>, w: usize) -> Line<'static> {
    menu_bar_in(groups, action, w, theme::pal().accent)
}

/// Same as `menu_bar`, with the main action's group in color main (green in multi-select)
pub(super) fn menu_bar_in(groups: &[Vec<MenuItem>], action: Option<usize>, w: usize, main: Color) -> Line<'static> {
    let gap = if w >= 60 { "  │  " } else { " │ " };
    let mut keep: Vec<Vec<MenuItem>> = groups.to_vec();
    let width = |gs: &[Vec<MenuItem>]| -> usize {
        let gs: Vec<&Vec<MenuItem>> = gs.iter().filter(|g| !g.is_empty()).collect();
        let items: usize = gs.iter().flat_map(|g| g.iter()).map(|(k, d, _)| k.width() + 1 + d.width()).sum();
        let seps: usize = gs.iter().map(|g| g.len().saturating_sub(1) * SEP.width()).sum();
        items + seps + gs.len().saturating_sub(1) * gap.width()
    };
    while width(&keep) > w {
        let worst = keep.iter().flatten().map(|it| it.2).max().unwrap_or(0);
        if worst == 0 {
            break;
        }
        for g in &mut keep {
            if let Some(i) = g.iter().rposition(|it| it.2 == worst) {
                g.remove(i);
                break;
            }
        }
    }
    let mut spans = vec![];
    for (gi, g) in keep.iter().enumerate().filter(|(_, g)| !g.is_empty()) {
        if !spans.is_empty() {
            spans.push(Span::styled(gap, theme::faint()));
        }
        let on = action == Some(gi);
        for (i, (k, d, _)) in g.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(SEP, theme::faint()));
            }
            let st = Style::new().fg(main);
            let (ks, ds) = if on { (st.add_modifier(Modifier::BOLD), st) } else { (key_style(), dim()) };
            spans.push(Span::styled(k.to_string(), ks));
            spans.push(Span::styled(format!(" {d}"), ds));
        }
    }
    Line::from(spans).centered()
}

/// One session's icon: ☾ if aca stopped it for being idle (gray, unobtrusive like ■; `[idle] stop_after`), otherwise by state
pub fn session_icon(s: &Session) -> (&'static str, Style) {
    if reaped(s) {
        return (aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted));
    }
    icon(s.state)
}

/// Icon of a subagent row: running looks the same as a session; finished gets a check mark (sessions have none), errored and stopped
/// ones are drawn like a session's
pub(super) fn agent_icon(p: Phase) -> (&'static str, Style) {
    let pal = theme::pal();
    match p {
        Phase::Running => icon(State::Running),
        Phase::Done => ("✓", Style::new().fg(pal.muted)),
        Phase::Failed => ("✕", Style::new().fg(pal.red)),
        Phase::Stopped => ("■", Style::new().fg(pal.muted)),
    }
}

/// Key of a subagent row: "session key \u{2} subagent id" (pane rows use \u{1})
pub(super) const AGENT_SEP: char = '\u{2}';

/// The k-th subagent sent out by session parent, made into a row (only there after `l` unfolds, see `App::lay_agents`). It has no pane
/// and isn't a session: `sub` records whom it hangs under (so it doesn't count as a session, comes and goes with that family, and `h`
/// gets back); the rest is copied from the parent session (group, workspace, pinned follow it);
/// both numbers in the last column hold how long it has run — up to now if running, the total if finished
pub(super) fn agent_row(parent: &Session, k: usize, a: &Subagent) -> Session {
    let secs = (a.dur.max(0) / 1000) as u64;
    Session {
        name: format!("{}{AGENT_SEP}{}", parent.name, a.id),
        title: a.label(),
        locked: true,
        state: if a.running() { State::Running } else { State::Idle },
        question: String::new(),
        idle_for: secs,
        ran: secs,
        active_at: (a.t0.max(0) / 1000) as u64 + secs,
        attached: false,
        screen: a.screen.clone(),
        noted: false,
        group_noted: false,
        pid: 0,
        deck_id: String::new(),
        info: String::new(),
        worktree: false,
        pane: String::new(),
        sub: Some(Sub { parent: parent.name.clone(), parent_title: parent.title.clone(), win: 0, pane: k as u32, split: false }),
        subagents: vec![],
        subagent: Some(a.clone()),
        due: vec![],
        ..parent.clone()
    }
}

/// The account column in the list is at most this wide; longer gets cut
const WHO_MAX: usize = 8;

/// The list's account column: the account cut to `WHO_MAX`, then `@<sandbox>` when it runs in one (`team@work`; a shell inside is
/// just `@work`), so the sandbox always shows
fn who_cell(s: &Session) -> String {
    let who = clip(&s.who, s.who.width().min(WHO_MAX));
    if s.within.is_empty() { who } else { format!("{who}@{}", s.within) }
}

/// One list item: a session (its index in the visible list) or a section header (pos = None)
pub struct Row {
    pub pos: Option<usize>,
    /// Unfolded group header: index in the visible list of the group's first session (the highlight can stop on headers, see `App::head`)
    pub head: Option<usize>,
    pub lines: Vec<Vec<(String, Style)>>,
}

/// Hint letters for jump mode, same as agent-deck (Vimium's default, home row)
const HINT_CHARS: &str = "sadfjklewcmpgh";

/// Hints for n rows, copying agent-deck's (Vimium's) algorithm: one letter where possible, all single letters up to 13 rows;
/// two-letter ones are inserted among the single letters. The same row count gives exactly agent-deck's hints
pub(super) fn jump_hints(n: usize) -> Vec<String> {
    if n == 0 {
        return vec![];
    }
    let mut hints = vec![String::new()];
    let mut offset = 0;
    while hints.len() - offset < n || hints.len() == 1 {
        let h = hints[offset].clone();
        offset += 1;
        hints.extend(HINT_CHARS.chars().map(|c| format!("{c}{h}")));
    }
    let mut r = hints[offset..offset + n].to_vec();
    r.sort();
    r.into_iter().map(|s| s.chars().rev().collect()).collect()
}

/// Where the highlight can stop (jumps and j / k all go by this): a session (index in the visible list), or an unfolded group header
/// (recorded as the group's first session)
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Jump {
    Row(usize),
    Head(usize),
}

/// Jump targets, ordered top to bottom as on screen: in sections, an unfolded group header comes before its sessions.
/// The pinned block's header doesn't count; a fold row is an item anyway (jumping to it unfolds)
pub(super) fn jump_targets(view: &[&Session], sections: bool, folds: &Folds) -> Vec<Jump> {
    let mut v = vec![];
    let mut cur = None;
    for (pos, s) in view.iter().enumerate() {
        if sections && cur != Some(section_key(s)) {
            cur = Some(section_key(s));
            if !s.pinned && !folds.contains_key(&s.group) {
                v.push(Jump::Head(pos));
            }
        }
        // An empty group has only its header (a folded one has the fold row)
        if !s.blank || folds.contains_key(&s.group) {
            v.push(Jump::Row(pos));
        }
    }
    v
}

pub(super) enum HintMatch {
    /// Matches exactly one row
    Hit(usize),
    /// Still the start of some hint; wait for the next letter
    Prefix,
    Miss,
}

pub(super) fn match_hint(hints: &[String], typed: &str) -> HintMatch {
    let exact = hints.iter().position(|h| h == typed);
    match (exact, hints.iter().filter(|h| h.starts_with(typed)).count()) {
        (Some(p), 1) => HintMatch::Hit(p),
        (_, 0) => HintMatch::Miss,
        _ => HintMatch::Prefix,
    }
}

/// A folded section is a single row in the list (held by the section's first session):
/// group → (how many folded, how many of them asking you)
pub type Folds = HashMap<String, (usize, usize)>;

/// Numbers are written only if the list is this wide (on a phone those 3 cells go to the title; digits still work, and jumps have hint
/// letters)
const NUM_MIN_W: usize = 56;

/// The whole list. In sections, each group gets a header row first (pinned ones at the very top, header 「★ 置顶」 ("★ Pinned"));
/// one row per session, and one asking you gets a row below with what it asks.
/// desk = desktop: always write numbers, a blank line between groups (directory and branch are in the preview header, not in the list).
/// In jump mode (jump = letters already typed), matching sessions show the hint in place of the number, and so does the ▾ of group headers.
/// Shared by the TUI and `aca -l`, so both look the same.
pub fn list_rows(view: &[&Session], sections: bool, width: usize, jump: Option<&str>, folds: &Folds, who_w: usize, desk: bool) -> Vec<Row> {
    let mut rows = vec![];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in view.iter().filter(|s| !s.pinned && s.sub.is_none() && !s.blank) {
        *counts.entry(s.group.as_str()).or_default() += 1;
    }
    let nums = desk || width >= NUM_MIN_W;
    let trees = tree_marks(view, sections);
    // On the desktop the title column is as wide as the longest title, with account and time right after it, not pushed to the far right
    let now = epoch();
    let fit = if desk {
        view.iter()
            .zip(&trees)
            .map(|(s, (t, _))| {
                t.width() + row_loc(s).width() + s.title.width() + agents_badge(s).width() + due_badge(s, now).width() + marks(s).width()
            })
            .max()
            .unwrap_or(0)
    } else {
        usize::MAX
    };
    // Subagent rows get no number: they come and go, and numbering them would make other sessions' numbers jump
    let mut n = 0;
    let numbers: Vec<Option<usize>> = view
        .iter()
        .map(|s| {
            (s.subagent.is_none() && !s.blank).then(|| {
                n += 1;
                n
            })
        })
        .collect();
    let targets = if jump.is_some() { jump_targets(view, sections, folds) } else { vec![] };
    let hints = jump_hints(targets.len());
    let hint_of = |t: Jump| {
        let typed = jump?;
        let h = &hints[targets.iter().position(|&x| x == t)?];
        h.starts_with(typed).then(|| hint_badge(h, typed))
    };
    let mut cur: Option<(bool, &str)> = None;
    for (pos, s) in view.iter().enumerate() {
        let hint = hint_of(Jump::Row(pos));
        let key = section_key(s);
        let new_sec = sections && cur != Some(key);
        if new_sec {
            if desk && cur.is_some() {
                rows.push(Row { pos: None, head: None, lines: vec![vec![]] });
            }
            cur = Some(key);
        }
        if let (false, Some(&(n, asks))) = (s.pinned, folds.get(&s.group)) {
            rows.push(Row { pos: Some(pos), head: None, lines: vec![folded_line(&s.group, n, asks, s.group_noted, width, hint)] });
            continue;
        }
        if new_sec {
            let (line, head) = match s.pinned {
                true => (pinned_line(), None),
                false => {
                    let n = counts.get(s.group.as_str()).copied().unwrap_or(0);
                    (section_line(&s.group, n, s.group_noted, width, hint_of(Jump::Head(pos))), Some(pos))
                }
            };
            rows.push(Row { pos: None, head, lines: vec![line] });
        }
        if s.blank {
            continue;
        }
        let lead = match (hint, nums) {
            (Some(h), true) => [vec![(" ".into(), Style::new())], h, vec![(" ".into(), Style::new())]].concat(),
            (Some(h), false) => h,
            (None, true) => vec![(numbers[pos].map_or_else(|| "    ".to_string(), |n| format!(" {n:>2} ")), dim())],
            (None, false) => vec![("  ".into(), Style::new())],
        };
        let (tree, cont) = &trees[pos];
        rows.push(Row { pos: Some(pos), head: None, lines: row_segments(lead, s, width, who_w, tree, cont, fit) });
    }
    rows
}

/// Section key: the pinned block is one section, the rest go by group
fn section_key(s: &Session) -> (bool, &str) {
    (s.pinned, if s.pinned { "" } else { s.group.as_str() })
}

/// Tree branches are drawn at most this many levels deep (two cells each); deeper ones are drawn level with this one: titles on narrow
/// screens don't get squeezed out
const TREE_DEEP: usize = 4;

/// The branch before each row's icon and the vertical bar on its continuation line (what it asks), joined like the tree command:
/// level 0 = a top-level session; a session's windows / subagents / child sessions are one level deeper ("├ " "└ "), preceded by "│ " for
/// each level above. Any depth works the same, drawn up to TREE_DEEP levels.
/// Hung under the parent session only if the parent is in that chain above (top level if the parent is filtered out / in the pinned block)
fn tree_marks(view: &[&Session], sections: bool) -> Vec<(String, String)> {
    let mut levels = Vec::with_capacity(view.len());
    let mut secs = Vec::with_capacity(view.len());
    // The chain the previous row is in: from the top-level session down, level by level, to its own session
    let mut chain: Vec<&str> = vec![];
    let (mut cur, mut sec): (Option<(bool, &str)>, usize) = (None, 0);
    for s in view {
        if sections && cur != Some(section_key(s)) {
            cur = Some(section_key(s));
            chain.clear();
            sec += 1;
        }
        let at = |n: &str| chain.iter().rposition(|c| *c == n);
        let lvl = match &s.sub {
            // A session: hangs under its parent if the parent is in this chain, otherwise top level
            None => {
                let lvl = s.under().and_then(at).map_or(0, |p| p + 1);
                chain.truncate(lvl);
                chain.push(&s.name);
                lvl
            }
            // A window / subagent: one level deeper than its session
            Some(x) => {
                let lvl = at(&x.parent).map_or(1, |p| p + 1);
                chain.truncate(lvl);
                lvl
            }
        };
        levels.push(lvl.min(TREE_DEEP));
        secs.push(sec);
    }
    // After pos, before going back to a shallower level, is there another one at level lvl (if so draw ├, otherwise └)
    let more =
        |pos: usize, lvl: usize| (pos + 1..view.len()).take_while(|&j| secs[j] == secs[pos] && levels[j] >= lvl).any(|j| levels[j] == lvl);
    // The next row hangs under it
    let kids = |pos: usize| pos + 1 < view.len() && secs[pos + 1] == secs[pos] && levels[pos + 1] > levels[pos];
    let bar = |on: bool| if on { "│ " } else { "  " };
    let fork = |on: bool| if on { "├ " } else { "└ " };
    (0..view.len())
        .map(|pos| match levels[pos] {
            0 => (String::new(), bar(kids(pos)).to_string()),
            lvl => {
                // Vertical bars for the levels above: only where that level has more siblings after it
                let ups: String = (1..lvl).map(|l| bar(more(pos, l))).collect();
                let on = more(pos, lvl);
                (format!("{ups}{}", fork(on)), format!("{ups}{}{}", bar(on), bar(kids(pos))))
            }
        })
        .collect()
}

/// Style of the group name in section headers: bold, uncolored (color is kept for states)
fn group_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A folded section: " ▸ work  3 ◆1 ✎", same as an unfolded header except ▾ becomes ▸; selectable, Enter / o unfolds
fn folded_line(group: &str, n: usize, asks: usize, noted: bool, width: usize, hint: Option<Vec<(String, Style)>>) -> Vec<(String, Style)> {
    let mut v = hint.unwrap_or_else(|| vec![(" ".into(), Style::new())]);
    let count = format!("  {n}");
    let ask = if asks > 0 { format!(" ◆{asks}") } else { String::new() };
    let mark = note_mark(noted);
    let name = clip(group_label(group), width.saturating_sub(4 + count.width() + ask.width() + mark.0.width())).trim_end().to_string();
    v.extend([("▸ ".into(), theme::faint()), (name, group_style()), (count, dim()), (ask, icon(State::Ask).1), mark]);
    v
}

/// The ✎ at the end of group headers and fold rows: the group has a note
fn note_mark(noted: bool) -> (String, Style) {
    (if noted { " ✎" } else { "" }.into(), theme::warn())
}

/// Hint letters: already typed on gray, still to type on yellow (as in agent-deck), padded to two cells
fn hint_badge(hint: &str, typed: &str) -> Vec<(String, Style)> {
    let p = theme::pal();
    let badge = |bg| Style::new().fg(p.on_hint).bg(bg).add_modifier(Modifier::BOLD);
    let n = typed.chars().count();
    let mut v: Vec<(String, Style)> =
        hint.chars().enumerate().map(|(i, c)| (c.to_string(), badge(if i < n { p.hint_typed } else { p.hint }))).collect();
    let len = hint.chars().count();
    if len < 2 {
        v.push((" ".repeat(2 - len), Style::new()));
    }
    v
}

/// Name in the section header: a plain tmux session's group is recorded as "-"
/// Which workspace a session is in: its own assignment → a child session follows its parent (if the parent has none either, keep going
/// up, any depth) → the topmost one's group → 1. Windows follow their own session.
/// mine = false ignores its own assignment, i.e. "where it would be if not recorded"
pub(super) fn space_of(all: &[Session], sp: &Spaces, boxed: &Boxed, s: &Session, mine: bool) -> char {
    let head = |n: &str| all.iter().find(|x| x.sub.is_none() && x.name == n);
    let mut s = head(s.family()).unwrap_or(s);
    if let Some(c) = sp.of(&notes::key(s)).filter(|_| mine) {
        return c;
    }
    for _ in 0..DEEP {
        let Some(p) = s.up.parent().and_then(head) else { break };
        if let Some(c) = sp.of(&notes::key(p)) {
            return c;
        }
        s = p;
    }
    boxed.of(s).or_else(|| sp.group_of(&s.profile, &s.group)).unwrap_or(spaces::HOME)
}

/// The workspace a sandbox's sessions go to unless sent elsewhere (`[sandbox.<name>] space`, see aca_core::sandbox): ahead of
/// their group's. Read from config.toml once per use
pub(super) struct Boxed(pub(super) aca_core::config::Config);

impl Boxed {
    pub(super) fn load() -> Boxed {
        Boxed(aca_core::config::load())
    }

    fn of(&self, s: &Session) -> Option<char> {
        if self.0.sandbox.is_empty() || s.tool.is_empty() {
            return None;
        }
        aca_core::sandbox::space(&self.0, &s.tool, &s.account).filter(|c| spaces::is_key(*c))
    }
}

/// Which letter each group gets in the group list: the first not-yet-taken letter of the group name (last level first), else the next one
/// in the jump alphabet;
/// q is kept for closing the list, space is all groups. As long as the group order doesn't change, neither do the letters; once learned,
/// no need to look
pub(super) fn group_keys(groups: &[String]) -> Vec<char> {
    let mut used = vec!['q'];
    groups
        .iter()
        .map(|g| {
            let name = group_label(g);
            let last = name.rsplit('/').next().unwrap_or(name);
            let free = |c: &char| c.is_ascii_lowercase() && !used.contains(c);
            let c = last
                .chars()
                .chain(name.chars())
                .map(|c| c.to_ascii_lowercase())
                .find(free)
                .or_else(|| HINT_CHARS.chars().chain('a'..='z').find(free));
            let c = c.unwrap_or(' ');
            used.push(c);
            c
        })
        .collect()
}

/// " ▾ group name  4": the section's count follows the name, then a ✎ if the group has a note. In jump mode the hint letters go before ▾
/// (as on fold rows)
fn section_line(group: &str, n: usize, noted: bool, width: usize, hint: Option<Vec<(String, Style)>>) -> Vec<(String, Style)> {
    let mut v = match hint {
        Some(h) => [h, vec![("▾ ".into(), theme::faint())]].concat(),
        None => vec![(" ▾ ".into(), theme::faint())],
    };
    let count = format!("  {n}");
    let mark = note_mark(noted);
    let name = clip(group_label(group), width.saturating_sub(4 + count.width() + mark.0.width())).trim_end().to_string();
    v.extend([(name, group_style()), (count, dim()), mark]);
    v
}

/// Header of the pinned block
fn pinned_line() -> Vec<(String, Style)> {
    vec![(" ★ ".into(), theme::warn()), (t!("置顶", "Pinned").into(), group_style())]
}

/// One session: (text, style) spans, one row; one asking you gets another row below with what it asks, aligned with the title.
/// lead = the number / indent at the start of the row (or the jump hint)
/// `who_w`: account column width, 0 = not shown (when there isn't a single registered session)
/// `tree` / `cont`: the branch before the icon, the vertical bar on the continuation line (see tree_marks)
/// `fit`: the title column is at most this wide (branch and window number included), usize::MAX = take it all
fn row_segments(
    lead: Vec<(String, Style)>,
    s: &Session,
    width: usize,
    who_w: usize,
    tree: &str,
    cont: &str,
    fit: usize,
) -> Vec<Vec<(String, Style)>> {
    let lead_w: usize = lead.iter().map(|(t, _)| t.width()).sum();
    let (ic, ic_style) = s.subagent.as_ref().map_or_else(|| session_icon(s), |a| agent_icon(a.phase));
    // A subagent row: not a session, no note or account, text in gray
    let own = s.subagent.is_none();
    let flags: String = [(s.noted, '✎'), (s.attached, '⇄')].iter().filter(|f| f.0 && own).map(|f| f.1).collect();
    let flag_style = if s.noted { theme::warn() } else { dim() };
    let when = s.when();
    let age_w = crate::data::age_w();
    let when = format!("{}{when}", " ".repeat(age_w.saturating_sub(when.width())));
    let who_cols = if who_w > 0 { who_w + 1 } else { 0 };
    let tail_w = 2 + 1 + who_cols + age_w + 1;
    let pre = tree;
    let loc = row_loc(s);
    let badge = agents_badge(s);
    let due = due_badge(s, epoch());
    // The minimum of 8 cells only matters on very narrow screens; the width set by the longest title isn't stretched per row any more,
    // or else the row with a long window number would have its time out of line
    let title_w =
        width.saturating_sub(lead_w + pre.width() + 2 + tail_w + loc.width()).max(8).min(fit.saturating_sub(pre.width() + loc.width()));
    let mut first = lead;
    if !pre.is_empty() {
        first.push((pre.into(), theme::faint()));
    }
    first.extend([(ic.to_string(), ic_style), (" ".into(), Style::new()), (loc, dim())]);
    // Three things follow the title: a subagent +N (green), scheduled ◷ (accent color), plugin text (gray); the title comes first,
    // keeping at least two thirds
    let marks = marks(s);
    let tail = badge.width() + due.width() + marks.width();
    if tail == 0 {
        first.push((clip(&s.title, title_w), if own { Style::new() } else { dim() }));
    } else {
        let tw = s.title.width().min(title_w.saturating_sub(tail).max(title_w * 2 / 3));
        let shown = clip(&s.title, tw).trim_end().to_string();
        let room = title_w.saturating_sub(shown.width());
        let badge = clip(&badge, room).trim_end().to_string();
        // If the whole "◷ 08:57" doesn't fit, keep only ◷: a half-cut time would be misread
        let left = room.saturating_sub(badge.width());
        let due = match due.width() {
            w if w <= left => due,
            _ if left >= 2 => format!(" {DUE}"),
            _ => String::new(),
        };
        // Plugin text fills this column (spaces if none), so the times after it line up
        let marks = clip(&marks, room.saturating_sub(badge.width() + due.width()));
        first.extend([
            (shown, if own { Style::new() } else { dim() }),
            (badge, Style::new().fg(theme::pal().green)),
            (due, Style::new().fg(theme::pal().accent)),
            (marks, theme::faint()),
        ]);
    }
    let who = if own { who_cell(s) } else { String::new() };
    first.extend([
        (format!("{flags:<2}"), flag_style),
        (" ".into(), Style::new()),
        (if who_cols > 0 { format!("{} ", clip(&who, who_w)) } else { String::new() }, agent_style(s.agent)),
        (when, dim()),
        (" ".into(), Style::new()),
    ]);
    let mut lines = vec![first];
    if !s.question.is_empty() {
        let q = clip(&s.question, width.saturating_sub(lead_w + cont.width() + 1)).trim_end().to_string();
        lines.push(vec![(" ".repeat(lead_w), Style::new()), (cont.into(), theme::faint()), (q, Style::new().fg(theme::pal().red))]);
    }
    // Stopped for running out of quota / not being logged in: the original message, followed by what you can do
    if can_rehome(s) {
        let hint = t!("  X 换账号", "  X switch account");
        let room = width.saturating_sub(lead_w + cont.width() + 1 + hint.width());
        let e = clip(&s.error, room).trim_end().to_string();
        lines.push(vec![
            (" ".repeat(lead_w), Style::new()),
            (cont.into(), theme::faint()),
            (e, Style::new().fg(theme::pal().red)),
            (hint.into(), theme::accent().add_modifier(Modifier::BOLD)),
        ]);
    }
    lines
}

/// A registered Claude / Codex session stopped because of its account (out of quota, not logged in, unpaid): switching accounts lets it
/// carry on (`X`)
fn can_rehome(s: &Session) -> bool {
    s.state == State::Error
        && s.error_kind == "account"
        && s.sub.is_none()
        && !s.deck_id.is_empty()
        && matches!(s.agent, Agent::Claude | Agent::Codex)
}

/// The short bit before the title: windows / panes get their tmux number (#2 / #2.1); subagents have no number, one started by another
/// subagent gets a ↳
fn row_loc(s: &Session) -> String {
    match (&s.subagent, &s.sub) {
        (Some(a), _) if a.depth > 0 => format!("{}↳ ", "  ".repeat(a.depth as usize - 1)),
        (Some(_), _) | (None, None) => String::new(),
        (None, Some(x)) => format!("{} ", x.loc()),
    }
}

/// The " +2" after a session's title: how many of the subagents it sent out are still running (nothing if none are; unfold with `l` to
/// see which)
fn agents_badge(s: &Session) -> String {
    match s.subagents.iter().filter(|a| a.running()).count() {
        0 => String::new(),
        n => format!(" +{n}"),
    }
}

/// The ◷ after the title: things it scheduled for itself that aren't due yet (see `Session::due`), with the time of the earliest one;
/// +N if there are several
fn due_badge(s: &Session, now: u64) -> String {
    let Some(first) = s.due.first() else { return String::new() };
    let when = first.next.map_or_else(|| t!("定时", "scheduled").to_string(), |t| local_when(t, now));
    let more = if s.due.len() > 1 { format!(" +{}", s.due.len() - 1) } else { String::new() };
    format!(" {DUE} {when}{more}")
}

/// Scheduled for itself and not due yet (after the title in the list, in the preview, in the legend)
const DUE: &str = "◷";

/// Local time: today's as "08:57", other days as "10/16 08:57"
pub(super) fn local_when(t: u64, now: u64) -> String {
    aca_core::schedule::clock(t, now)
}

/// One line each in the preview: kind, cron, next time, the message handed to it when it's due
fn due_line(d: &aca_core::schedule::Due, now: u64) -> String {
    let when = d.next.map_or_else(|| t!("无法识别时间", "time unknown").to_string(), |t| local_when(t, now));
    let what = match (d.cron.is_empty(), d.recurring) {
        _ if d.inbox => tr!("定时消息 {when}", "scheduled message at {when}"),
        (true, _) => tr!("/loop 下一次 {when}", "/loop next at {when}"),
        (false, true) => tr!("循环定时 {} · 下次 {when}", "recurring {} · next {when}", d.cron),
        (false, false) => tr!("一次性定时 {} · {when}", "one-shot {} · at {when}", d.cron),
    };
    let prompt = d.prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if prompt.is_empty() { format!("{DUE} {what}") } else { format!("{DUE} {what} · {prompt}") }
}

/// Gray text after the title: a fork first gets " fork of X", then what plugins attach (`aca meta set`): " [pr #12] [ci ✓]".
/// Not repeated if the title already contains the original session's name (the default 「X (分叉)」 ("X (fork)"))
fn marks(s: &Session) -> String {
    let fork = match &s.fork {
        Fork::Of(t) if s.sub.is_none() && !s.title.contains(t.as_str()) => tr!(" 分叉自 {t}", " fork of {t}"),
        _ => String::new(),
    };
    s.tokens.iter().fold(fork, |out, (k, v)| out + " " + &mark(k, v))
}

/// A worktree session's branch (the Branch line in info)
fn branch_of(s: &Session) -> Option<&str> {
    s.info.lines().find_map(|l| l.strip_prefix("Branch: ")).filter(|b| !b.is_empty())
}

/// Which session the preview draws: the highlighted one; while swiping, stay on the last one drawn (see SETTLE)
pub(super) fn preview_name(app: &mut App) -> Option<String> {
    let cur = app.current().map(|s| s.name.clone());
    let keep = app.sliding() && app.shown.as_ref().is_some_and(|n| app.all.iter().any(|s| &s.name == n));
    if !keep {
        app.shown = cur;
    }
    app.shown.clone()
}

/// How long ago: just now / 5 min ago
fn ago(sec: u64) -> String {
    if sec == 0 { t!("刚刚", "just now").into() } else { tr!("{}前", "{} ago", words::dur(sec)) }
}

/// State in words: needs you / running · 12 min / unread · 5 min ago …
pub(super) fn state_word(s: &Session) -> String {
    if let Some(a) = &s.subagent {
        let took = words::dur((a.dur.max(0) / 1000) as u64);
        return match a.phase {
            Phase::Running => tr!("运行中 · {took}", "running · {took}"),
            Phase::Done => tr!("已完成 · 用时 {took}", "finished · took {took}"),
            Phase::Failed => tr!("出错 · 用时 {took}", "failed · took {took}"),
            Phase::Stopped => tr!("已中断 · 用时 {took}", "interrupted · ran {took}"),
        };
    }
    match s.state {
        State::Ask => t!("等待回应", "needs you").to_string(),
        State::Running if s.ran > 0 => tr!("运行中 · {}", "running · {}", words::dur(s.ran)),
        State::Running => t!("运行中", "running").into(),
        State::Unread => tr!("未读 · {}", "unread · {}", ago(s.idle_for)),
        State::Idle => tr!("空闲 · {}", "idle · {}", ago(s.idle_for)),
        State::Error if s.error_kind == "account" => t!("出错 · 额度 / 登录", "error · quota / login").into(),
        State::Error => t!("出错", "error").into(),
        State::Dead | State::Stopped if s.idle_stopped > 0 => {
            tr!("自动停止 · {}", "auto-stopped · {}", ago(epoch().saturating_sub(s.idle_stopped)))
        }
        State::Stopped => t!("已停止", "stopped").into(),
        State::Exited | State::Dead => tr!("已停止 · {}", "stopped · {}", ago(s.idle_for)),
        State::Crashed => t!("出错 · 已停止", "error · stopped").into(),
    }
}

/// Uncommitted changes: 「  3 个文件 +12 −4」 ("  3 files +12 −4"); nothing when clean
fn stat_spans(r: &diff::Stat) -> Vec<(String, Style)> {
    if r.files == 0 {
        return vec![];
    }
    let (k, w) = (r.files, noun(r.files as u64, "file", "files"));
    let mut v = vec![(tr!("  {k} 个文件", "  {k} {w}"), dim())];
    if r.add > 0 {
        v.push((format!(" +{}", r.add), Style::new().fg(theme::pal().green)));
    }
    if r.del > 0 {
        v.push((format!(" −{}", r.del), Style::new().fg(theme::pal().red)));
    }
    v
}

/// Spans laid one after another into a line, cut beyond w
fn fit_line(pieces: Vec<(String, Style)>, w: usize) -> Line<'static> {
    let (mut v, mut used) = (vec![], 0);
    for (t, st) in pieces {
        if used >= w {
            break;
        }
        let t = if used + t.width() > w { clip(&t, w - used).trim_end().to_string() } else { t };
        used += t.width();
        v.push(Span::styled(t, st));
    }
    Line::from(v)
}

/// Directory entries get columns at most this many cells wide (longer ones are cut)
const LS_COL: usize = 28;

/// Preview when the highlight is on a group: the group's path, branch, changes; which session the path is borrowed from (when the group
/// has no default path); the group's note; then a blank line and what's in the directory
/// (subdirectories first, with /, laid out in columns; what doesn't fit is only counted, dotfiles aren't listed). Before g is read back,
/// only the path and note.
/// The note matters more than the directory: it can take up to half the preview even when not editing; while editing, same as a session's
pub(super) fn group_pane(sp: &Spot, g: Option<&Glance>, note: Option<&str>, editing: bool, w: usize, h: usize) -> Vec<Line<'static>> {
    let repo = g.and_then(|g| g.repo.as_ref());
    let mut tail: Vec<(String, Style)> = vec![];
    if let Some(r) = repo {
        if !r.branch.is_empty() {
            tail.push((format!("  {}", r.branch), dim()));
        }
        tail.extend(stat_spans(r));
    }
    // Once the branch and changes fit, the path gets the remaining room, cut from the left
    let room = w.saturating_sub(tail.iter().map(|(t, _)| t.width()).sum::<usize>());
    let dir = words::tilde(&sp.dir);
    let dir = if room >= 8 { clip_left(&dir, room) } else { dir };
    let mut out = vec![fit_line([vec![(dir, Style::new().add_modifier(Modifier::BOLD))], tail].concat(), w)];
    if let Some(t) = &sp.from {
        out.push(Line::styled(clip(&tr!("「{t}」的目录", "“{t}”'s directory"), w).trim_end().to_string(), dim()));
    }
    if let Some(text) = note {
        out.extend(group_note(text, editing, w, h, out.len()));
    }
    let Some(f) = g.and_then(|g| g.files.as_ref()) else { return out };
    out.push(Line::default());
    if let Some(e) = &f.err {
        out.push(Line::styled(e.clone(), theme::warn()));
        return out;
    }
    let dir_style = Style::new().fg(theme::pal().accent);
    let items: Vec<(String, Style)> =
        f.dirs.iter().map(|d| (format!("{d}/"), dir_style)).chain(f.files.iter().map(|x| (x.clone(), Style::new()))).collect();
    if items.is_empty() {
        if f.hidden == 0 {
            out.push(Line::styled(t!("空目录", "empty directory"), dim()));
        }
        return out;
    }
    let cw = items.iter().map(|(t, _)| t.width()).max().unwrap_or(1).min(LS_COL) + 2;
    let cols = (w / cw).max(1);
    // The last line is kept for "how many more"
    let room = h.saturating_sub(out.len());
    let need = items.len().div_ceil(cols);
    let rows = if need <= room { need } else { room.saturating_sub(1) };
    for r in 0..rows {
        let row: Vec<(String, Style)> = items.iter().skip(r * cols).take(cols).map(|(t, st)| (clip(t, cw - 2) + "  ", *st)).collect();
        out.push(fit_line(row, w));
    }
    let left = items.len().saturating_sub(rows * cols);
    if left > 0 && out.len() < h {
        out.push(Line::styled(tr!("还有 {left} 个", "{left} more"), dim()));
    }
    out
}

/// The group's note (the preview already has used rows above it): up to half the preview, both while editing and otherwise
pub(super) fn group_note(text: &str, editing: bool, w: usize, h: usize, used: usize) -> Vec<Line<'static>> {
    let room = (h / 2).saturating_sub(used);
    note_lines(text, editing, w, room, room.saturating_sub(1).max(3))
}

/// The preview's second line from the top: how many of the subagents it sent out are running and how many finished, followed by the keys
/// that work right now (folded: l unfolds; unfolded: h folds). None if it sent none
pub(super) fn agents_line(s: &Session, open: bool, w: usize) -> Option<Line<'static>> {
    if s.subagent.is_some() || s.subagents.is_empty() {
        return None;
    }
    let run = s.subagents.iter().filter(|a| a.running()).count();
    let rest = s.subagents.len() - run;
    let many = noun(s.subagents.len() as u64, "subagent", "subagents");
    let key = if open { t!("h 收起", "h folds") } else { t!("l 展开", "l unfolds") };
    let text = match (run, rest) {
        (r, 0) => tr!("子代理 {r} 个运行中 · {key}", "{r} {many} running · {key}"),
        (0, d) => tr!("子代理 {d} 个已完成 · {key}", "{d} {many} finished · {key}"),
        (r, d) => tr!("子代理 {r} 个运行中 · {d} 个已完成 · {key}", "subagents: {r} running · {d} finished · {key}"),
    };
    Some(Line::styled(clip(&text, w).trim_end().to_string(), dim()))
}

/// The preview's top lines: state (in the icon's color), changes, directory, branch; the note; then a blank line before the screen content.
/// The note only takes room when written (at most three lines, the rest summed up as how many more lines); while editing it's shown whole,
/// with a cursor, taking at most half the preview (the screen content must always show)
/// g: the directory it really works in, branch, uncommitted changes, read in the background (see `diff::glance`); until read, use the
/// registered directory and branch
pub(super) fn preview_head(s: &Session, g: Option<&Glance>, note: Option<&str>, editing: bool, w: usize, h: usize) -> Vec<Line<'static>> {
    // A subagent row: state and how many tools it used; the second line says who sent it and how to look closer. It has no directory,
    // changes or note
    if let (Some(a), Some(x)) = (&s.subagent, &s.sub) {
        let (n, steps) = (a.tools, noun(a.tools as u64, "step", "steps"));
        let steps = if n == 0 { String::new() } else { tr!("   {n} 步", "   {n} {steps}") };
        return vec![
            fit_line(vec![(state_word(s), agent_icon(a.phase).1), (steps, dim())], w),
            Line::styled(
                clip(&tr!("「{}」的子代理 · ⏎ 查看记录", "subagent of “{}” · ⏎ opens its record", x.parent_title), w)
                    .trim_end()
                    .to_string(),
                dim(),
            ),
        ];
    }
    let (_, sty) = session_icon(s);
    let word = state_word(s);
    let used = word.width();
    let dir = words::tilde(g.map(|g| g.work.dir.as_str()).filter(|d| !d.is_empty()).unwrap_or(&s.dir));
    let repo = g.and_then(|g| g.repo.as_ref());
    let branch = repo.map(|r| r.branch.clone()).filter(|b| !b.is_empty()).or_else(|| branch_of(s).map(String::from));
    // The diff stats follow the state right away (so they survive when narrow), the directory after them
    let stats = repo.map(stat_spans).unwrap_or_default();
    let mut tail: Vec<(String, Style)> = vec![];
    if let Some(b) = branch {
        tail.push((format!("  {b}"), dim()));
    }
    let width = |v: &[(String, Style)]| v.iter().map(|(t, _)| t.width()).sum::<usize>();
    // Once the rest fits, the directory gets the remaining room, cut from the left (the last level is the most useful: …/app)
    let room = w.saturating_sub(used + width(&stats) + 3 + width(&tail));
    let mut pieces = stats;
    if room >= 5 {
        pieces.push((format!("   {}", clip_left(&dir, room)), Style::new()));
    } else {
        pieces.push((format!("   {dir}"), Style::new()));
    }
    pieces.extend(tail);
    let mut out = vec![fit_line([vec![(word, sty)], pieces].concat(), w)];
    if let Some(x) = &s.sub {
        let kind = if x.split { t!("拆分的窗格", "split pane") } else { t!("窗口", "window") };
        out.push(Line::styled(
            clip(&tr!("「{}」里的{kind} {}", "{kind} {1} in “{0}”", x.parent_title, x.loc()), w).trim_end().to_string(),
            dim(),
        ));
    } else {
        match &s.up {
            Up::Under(_, t) | Up::Apart(_, t) => {
                out.push(Line::styled(clip(&tr!("「{t}」的子会话", "child of “{t}”"), w).trim_end().to_string(), dim()))
            }
            Up::Gone => out.push(Line::styled(t!("子会话 · 父会话已删除", "child session · parent deleted"), dim())),
            Up::Top => {}
        }
        match &s.fork {
            Fork::Of(t) => out.push(Line::styled(clip(&tr!("分叉自「{t}」", "forked from “{t}”"), w).trim_end().to_string(), dim())),
            Fork::Gone => out.push(Line::styled(t!("分叉 · 原会话已删除", "a fork · original deleted"), dim())),
            Fork::No => {}
        }
    }
    // Scheduled for itself and not due yet: one line each (at most three), then a line on what stopping it would do — visible before D, R
    if !s.due.is_empty() {
        let now = epoch();
        for d in s.due.iter().take(3) {
            let l = clip(&due_line(d, now), w).trim_end().to_string();
            out.push(match l.strip_prefix(DUE) {
                Some(rest) => Line::from(vec![Span::styled(DUE, Style::new().fg(theme::pal().accent)), Span::raw(rest.to_string())]),
                None => Line::raw(l),
            });
        }
        if s.due.len() > 3 {
            out.push(Line::styled(tr!("  还有 {} 项", "  and {} more", s.due.len() - 3), dim()));
        }
    }
    // Stopped on an error: the original message (at most two lines); stopped because of the account: offer X
    if s.state == State::Error && !s.error.is_empty() {
        let red = Style::new().fg(theme::pal().red);
        for (i, l) in wrap_width(&s.error, w.saturating_sub(2)).into_iter().take(2).enumerate() {
            out.push(Line::from(vec![Span::styled(if i == 0 { "✕ " } else { "  " }, red), Span::styled(l, red)]));
        }
        if can_rehome(s) {
            out.push(Line::from(vec![button(t!("X 换账号", "X switch account"), theme::pal().accent)]));
        }
    }
    if let Some(text) = note {
        let room = (h / 2).saturating_sub(out.len() + 1).max(1);
        out.extend(note_lines(text, editing, w, room, 3));
    }
    if s.state.gone() {
        out.push(Line::raw(""));
        out.push(Line::styled(t!("⏎ 启动 · d 删除", "⏎ start · d delete"), dim()));
    }
    out.push(Line::raw(""));
    out
}

/// The note in the preview: while editing it's shown whole, with a cursor, taking room lines including the 「✎ 便签」 ("✎ Note") line
/// (if longer, the last lines are kept, where the cursor is);
/// otherwise at most cap lines, the rest summed up as how many more lines
fn note_lines(text: &str, editing: bool, w: usize, room: usize, cap: usize) -> Vec<Line<'static>> {
    let warn = theme::warn();
    let mut body: Vec<String> = text.split('\n').flat_map(|l| wrap_width(l, w.saturating_sub(2))).collect();
    let mut out = vec![];
    if editing {
        out.push(Line::styled(t!("✎ 便签", "✎ Note"), warn.add_modifier(Modifier::BOLD)));
        let room = room.saturating_sub(1).max(1);
        // The cursor is at the end
        match body.last_mut() {
            Some(l) if l.width() < w.saturating_sub(2) => l.push('▏'),
            _ => body.push("▏".into()),
        }
        if body.len() > room {
            let more = body.len() - room + 1;
            body.drain(..more);
            body.insert(0, tr!("… 上面还有 {more} 行", "… {more} more lines above"));
        } else {
            body.resize(room, String::new());
        }
        out.extend(body.into_iter().map(|l| Line::raw(format!("  {l}"))));
    } else {
        let more = body.len().saturating_sub(cap);
        body.truncate(cap);
        for (i, l) in body.into_iter().enumerate() {
            let mark = if i == 0 { Span::styled("✎ ", warn) } else { Span::raw("  ") };
            out.push(Line::from(vec![mark, Span::raw(l)]));
        }
        if more > 0 {
            let w = noun(more as u64, "line", "lines");
            out.push(Line::styled(tr!("  … 还有 {more} 行", "  … {more} more {w}"), dim()));
        }
    }
    out
}

/// The preview drops Claude's input box and bottom status bar, keeping only the conversation. Not cut while it waits for you: that box is
/// exactly what to look at.
pub(super) fn preview_lines(s: &Session) -> Vec<Styled> {
    // A subagent's "screen" is a few lines written from its record (see subagent.rs); there's no input box to cut
    if s.subagent.is_some() {
        return ansi::parse_lines(&s.screen);
    }
    let lines = strip_prompt(ansi::parse_lines(&s.screen), s.state == State::Ask);
    if s.agent == Agent::Claude { quiet_claude(lines) } else { lines }
}

/// Lines on Claude's screen meant for its own terminal, useless in the preview, yet often taking the most visible last lines:
/// hints like "(ctrl+o to expand)" that can't be pressed in the preview; the compacted-while-idle line ("Compacted while idle …",
/// folded into two lines when narrow, the second holding only settings.json); files and skills listed as brought back after compaction
/// ("⎿  Referenced file …"
/// "⎿  Read <path> (12 lines)" "⎿  Skills restored (…)"; the file-reading tool's own result is "⎿  Read 12 lines", without path and
/// parentheses, left alone);
/// a hook's success line ("PostCompact […aca hook claude] completed successfully", mostly aca's own hook)
fn quiet_claude(lines: Vec<Styled>) -> Vec<Styled> {
    const HINTS: [&str; 3] = ["(ctrl+o to expand)", "(ctrl+r to expand)", "(ctrl+o to see full summary)"];
    let mut out: Vec<Styled> = Vec::with_capacity(lines.len());
    let mut idle_note = false;
    for mut l in lines {
        let text = ansi::plain(&l);
        let t = text.trim();
        let attached = t.strip_prefix("⎿").map(str::trim_start).is_some_and(|r| {
            r.starts_with("Referenced file ")
                || r.starts_with("Skills restored (")
                || (r.starts_with("Read ") && r.ends_with(" lines)") && r.contains(" ("))
        });
        let hook_ok = t.ends_with("] completed successfully") && t.contains(" [");
        // "⎿  Tip: Run /install-github-app …": a tip for its own terminal, can't be acted on in the preview
        let tip = t.strip_prefix("⎿").map(str::trim_start).is_some_and(|r| r.starts_with("Tip: "));
        let tail_of_note = std::mem::take(&mut idle_note) && t.ends_with("settings.json") && !t.contains(' ');
        if t.contains("Compacted while idle") {
            idle_note = true;
            continue;
        }
        if attached || hook_ok || tail_of_note || tip {
            continue;
        }
        for h in HINTS {
            // The hint is often a span of its own (gray), with the space before it in the previous span: after removing the hint, also
            // drop that space
            if let Some(i) = l.iter().rposition(|g| g.text.contains(h)) {
                l[i].text = l[i].text.replacen(h, "", 1);
                for j in (0..=i).rev() {
                    let t = l[j].text.trim_end().to_string();
                    let empty = t.is_empty();
                    l[j].text = t;
                    if !empty {
                        break;
                    }
                }
            }
        }
        out.push(l);
    }
    while out.last().is_some_and(|l| ansi::plain(l).trim().is_empty()) {
        out.pop();
    }
    out
}

/// Drop the input box at the bottom and the status bar below it; not while it waits for you (that box is exactly what to look at)
pub(super) fn strip_prompt(mut styled: Vec<Styled>, asking: bool) -> Vec<Styled> {
    let lines: Vec<String> = styled.iter().map(ansi::plain).collect();
    if !asking {
        // Input box = separator line + "❯ …"; below it there may also be a status bar or a background task / agent list, so search up
        // 20 lines
        // The top border may have the session name embedded: "────── project-main-session ─"
        let is_bar = |l: &str| {
            let t = l.trim();
            t.starts_with("───") && t.chars().filter(|&c| c == '─').count() * 2 > t.chars().count()
        };
        let from = lines.len().saturating_sub(20).max(1);
        if let Some(p) = (from..lines.len()).rev().find(|&i| lines[i].trim_start().starts_with('❯') && is_bar(&lines[i - 1])) {
            styled.truncate(p - 1);
        }
    }
    while styled.last().is_some_and(|l| ansi::plain(l).trim().is_empty()) {
        styled.pop();
    }
    styled
}

impl App {
    /// Whether the pos-th item of the visible list is a fold row (placeholder of a folded section)
    pub(super) fn is_fold_row(&self, pos: usize) -> bool {
        self.view.get(pos).is_some_and(|&i| {
            let s = &self.all[i];
            !s.pinned && self.folds.contains_key(&s.group)
        })
    }

    pub(super) fn on_fold_row(&self) -> bool {
        self.list.selected().is_some_and(|p| self.is_fold_row(p))
    }

    /// Account column width: set by the longest among all sessions (not just the ones shown),
    /// so headers don't resize when switching scope / group
    pub fn who_width(&self) -> usize {
        self.all.iter().map(|s| who_cell(s).width()).max().unwrap_or(0).min(WHO_MAX * 2)
    }

    pub fn fold_info(&self) -> &Folds {
        &self.folds
    }

    /// o: fold / unfold the current section (on a fold row: unfold). After folding the highlight lands on the fold row
    pub(super) fn toggle_fold(&mut self) {
        let Some(s) = self.current() else { return };
        if !self.sectioned() {
            return self.flash(t!("单个分组中不能折叠", "can't fold inside a single group"));
        }
        if s.pinned {
            return self.flash(t!("置顶项不能折叠", "pinned ones can't fold"));
        }
        let g = s.group.clone();
        match self.folded.contains(&g) {
            false => {
                // The placeholder is this section's first session; the highlight goes with it
                let first = self.view.iter().map(|&i| &self.all[i]).find(|x| !x.pinned && x.group == g).map(|x| x.name.clone());
                self.folded.insert(g.clone());
                self.selected = first;
            }
            true => {
                self.folded.remove(&g);
            }
        }
        self.rebuild();
        // Unfolded: the highlight stays on this group (on its header)
        if let Some(p) = self.head_pos(&g).filter(|_| !self.folded.contains(&g)) {
            self.select_head(p);
        }
    }

    /// h / ←: one level out. Window / subagent / child session → the session it hangs off; on a session with subagents unfolded = fold
    /// them; on a session or an unfolded header = fold the group (only h folds, only l unfolds); viewing a single group: back to all groups
    /// (highlight stays put). Fold rows and pinned-block items have no outer level; do nothing
    pub(super) fn step_out(&mut self) {
        if self.head.is_some() {
            return self.toggle_fold();
        }
        let Some(s) = self.current() else { return };
        // A session with subagents unfolded: fold those first; only the next press gets to the group
        if s.sub.is_none() && self.agents_open.contains(&s.name) {
            let name = s.name.clone();
            self.agents_open.remove(&name);
            self.lay_agents();
            self.apply_pins();
            return self.rebuild();
        }
        let up = s.sub.as_ref().map(|x| x.parent.clone()).or_else(|| s.under().map(String::from));
        let (group, pinned) = (s.group.clone(), s.pinned);
        if let Some(p) = up.and_then(|n| self.view.iter().position(|&i| self.all[i].name == n)) {
            return self.select(Some(p));
        }
        if !self.sectioned() {
            return self.set_group(None);
        }
        if self.head_pos(&group).is_some() && !pinned {
            self.toggle_fold();
        }
    }

    /// l / →: one level in. On a fold row = unfold (highlight on the header); on a group header = go to the group's first session; on a
    /// session: if it spawned subagents, unfold them into rows (highlight stays), if already unfolded go to the first; none, do nothing
    pub(super) fn step_in(&mut self) {
        if self.on_fold_row() {
            self.toggle_fold();
        } else if self.head.is_some() {
            let p = self.list.selected();
            self.select(p);
        } else if let Some(name) = self.current().filter(|s| s.sub.is_none() && !s.subagents.is_empty()).map(|s| s.name.clone()) {
            if self.agents_open.insert(name.clone()) {
                self.lay_agents();
                self.apply_pins();
                self.rebuild();
            } else if let Some(p) = self.view.iter().position(|&i| self.all[i].subagent.is_some() && self.all[i].family() == name) {
                self.select(Some(p));
            }
        }
    }

    /// O: unfold everything if anything is folded, else fold every section
    pub(super) fn toggle_fold_all(&mut self) {
        if !self.sectioned() {
            return self.flash(t!("单个分组中不能折叠", "can't fold inside a single group"));
        }
        if self.folded.is_empty() {
            self.folded.extend(self.view.iter().map(|&i| &self.all[i]).filter(|s| !s.pinned).map(|s| s.group.clone()));
        } else {
            self.folded.clear();
        }
        self.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{
        at, ctrl, deck_groups, fixture, fun_binned, jump_to_group, keys, layout, life_and_work, mk, order, pin, press, reader_rows, select,
        sessions, shift, shows, titles, typ, without_fun,
    };

    #[test]
    fn claude_preview_drops_its_own_terminal_chatter() {
        let lines = ansi::parse_lines(
            &"⏺ Read(src/a.rs)\n  ⎿  Read 12 lines (ctrl+o to expand)\n\n  Thought for 10s (ctrl+o to expand)\n\n⏺ 改好了\n\n\
             ✻ Crunched for 7m 41s\n  ⎿  Referenced file ../.claude/x.py\n  ⎿  Read ../criteria.md (59 lines)\n\n\
             ❯ /compact\n  ⎿  Compacted (ctrl+o to see full summary)\n     PostCompact ['/u/aca' hook claude] completed successfully\n\
               ⎿  Skills restored (sync-weekly)\n\n\
             ⏺ Compacted while idle, before the prompt cache expired · turn off with \"idleCompaction\": false in\n  settings.json"
                .split('\n')
                .collect::<Vec<_>>(),
        );
        let got: Vec<String> = quiet_claude(lines).iter().map(ansi::plain).collect();
        assert_eq!(
            got,
            [
                "⏺ Read(src/a.rs)",
                "  ⎿  Read 12 lines",
                "",
                "  Thought for 10s",
                "",
                "⏺ 改好了",
                "",
                "✻ Crunched for 7m 41s",
                "",
                "❯ /compact",
                "  ⎿  Compacted"
            ]
        );
    }

    #[test]
    fn a_session_aca_stopped_for_idling_is_drawn_apart_from_one_stopped_by_hand() {
        let mut v = sessions();
        for (title, at) in [("闲", epoch() - 7300), ("错", 0)] {
            let s = v.iter_mut().find(|s| s.title == title).unwrap();
            (s.state, s.idle_stopped, s.idle_for) = (State::Dead, at, 7300);
        }
        let mut app = fixture();
        app.set_sessions(v);
        let by = |t: &str| app.all.iter().find(|s| s.title == t).unwrap().clone();
        // Stopped by aca for being idle: ☾, gray, saying it was auto-stopped and how long ago; stopped by a person: ■ "stopped" as before
        assert_eq!(session_icon(&by("闲")), ("☾", Style::new().fg(theme::pal().muted)));
        assert_eq!(state_word(&by("闲")), "自动停止 · 2 小时前");
        assert_eq!((session_icon(&by("错")).0, state_word(&by("错")).as_str()), ("■", "已停止 · 2 小时前"));
        // Back up again (the moment before the mark is cleared): drawn by its state
        let mut up = by("闲");
        up.state = State::Idle;
        assert_eq!(session_icon(&up).0, "○");
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 100, 30);
        shows(&rows, &["☾", "自动停止 · 2 小时前"]);
    }

    #[test]
    fn a_session_with_its_own_schedule_shows_when_next_and_the_preview_lists_them() {
        use aca_core::schedule::Due;
        let now = epoch();
        let hm = |t: u64| {
            let d = aca_core::schedule::local(t);
            format!("{:02}:{:02}", d.h, d.mi)
        };
        let (soon, later) = (now + 1200, now + 3 * 86400);
        let loop_ = Due { cron: String::new(), recurring: false, prompt: "接着看\n部署".into(), next: Some(soon), inbox: false };
        let daily = Due { cron: "57 8 * * *".into(), recurring: true, prompt: "看一眼".into(), next: Some(later), inbox: false };
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "闲").unwrap().due = vec![loop_.clone(), daily.clone()];
        let mut app = fixture();
        app.set_sessions(v);
        let by = |t: &str| app.all.iter().find(|s| s.title == t).unwrap().clone();
        // After the title: the time of the earliest due one (today's: hour and minute only; other days add month and day), +N
        // when there are several; nothing when none is scheduled
        assert_eq!(due_badge(&by("闲"), now), format!(" ◷ {} +1", hm(soon)));
        assert_eq!(due_badge(&by("跑"), now), "");
        let day = aca_core::schedule::local(later);
        assert_eq!(local_when(later, now), format!("{}/{} {}", day.m, day.d, hm(later)));
        // One row each: kind, cron, time, the prompt (newlines joined into one line); one whose time can't be read says so
        assert_eq!(due_line(&loop_, now), format!("◷ /loop 下一次 {} · 接着看 部署", hm(soon)));
        assert_eq!(due_line(&daily, now), format!("◷ 循环定时 57 8 * * * · 下次 {} · 看一眼", local_when(later, now)));
        let odd = Due { cron: "@daily".into(), recurring: false, prompt: String::new(), next: None, inbox: false };
        assert_eq!(due_line(&odd, now), "◷ 一次性定时 @daily · 无法识别时间");
        // A scheduled message in the inbox (aca msg --in): Codex and pi have them too
        let msg = Due { cron: String::new(), recurring: false, prompt: "回来看看 CI".into(), next: Some(soon), inbox: true };
        assert_eq!(due_line(&msg, now), format!("◷ 定时消息 {} · 回来看看 CI", hm(soon)));
        // The row of a subagent it spawned isn't marked along with it
        let mut parent = by("闲");
        parent.subagents = vec![];
        let a = Subagent {
            id: "a1".into(),
            kind: "Explore".into(),
            desc: "找".into(),
            phase: Phase::Running,
            t0: 0,
            dur: 0,
            tools: 0,
            depth: 0,
            screen: vec![],
            reported: false,
        };
        assert!(agent_row(&parent, 0, &a).due.is_empty());
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 100, 30);
        shows(&rows, &[&format!("闲 ◷ {} +1", hm(soon)), &format!("◷ /loop 下一次 {} · 接着看 部署", hm(soon)), "◷ 循环定时 57 8 * * *"]);
    }

    /// What's read back after the write: groups in the order given
    fn regrouped(v: Vec<Session>, groups: &[&str]) -> Vec<Session> {
        v.into_iter()
            .map(|mut s| {
                s.slot.0 = groups.iter().position(|g| *g == s.group).unwrap_or(usize::MAX);
                s
            })
            .collect()
    }

    #[test]
    fn shift_arrows_on_a_group_header_move_the_whole_group() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        jump_to_group(&mut app, "work");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        assert_eq!(app.head.as_deref(), Some("work")); // the highlight follows the group
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::GroupOrder { group: "work".into(), by: "fun".into(), after: false });
        shift(&mut app, KeyCode::Up);
        assert!(app.msg.is_none());
        // A periodic refresh from before the write finished still reads the old order: doesn't jump back
        app.set_sessions(sessions());
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        app.job_done(&job, Outcome::Done(String::new()), regrouped(sessions(), &["work", "fun"]));
        assert_eq!(order(&app), ["跑", "闲", "错", "问", "未读"]);
        assert!(app.take_job().is_none());
        // Move it back: a folded group (fold row) can move too
        jump_to_group(&mut app, "fun");
        press(&mut app, KeyCode::Enter);
        assert!(app.on_fold_row());
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.take_job().unwrap(), Job::GroupOrder { group: "fun".into(), by: "work".into(), after: false });
        assert!(app.on_fold_row() && app.current().unwrap().group == "fun");
    }

    #[test]
    fn a_group_takes_its_subgroups_along_and_subgroups_stay_under_the_parent() {
        let mut v = sessions();
        v.push(mk("甲", "work/a", State::Idle, 10));
        v.push(mk("乙", "work/b", State::Idle, 10));
        let mut other = mk("壳", "-", State::Idle, 10); // plain tmux session: sinks to the bottom, doesn't move along
        other.deck_id.clear();
        v.push(other);
        let v = regrouped(v, &["fun", "work", "work/a", "work/b"]);
        let mut app = App::new(String::new(), v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错", "甲", "乙", "壳"]);
        jump_to_group(&mut app, "fun");
        shift(&mut app, KeyCode::Down); // jumps over the whole of work
        assert_eq!(order(&app), ["跑", "闲", "错", "甲", "乙", "问", "未读", "壳"]);
        assert_eq!(app.take_job().unwrap(), Job::GroupOrder { group: "fun".into(), by: "work".into(), after: true });
        shift(&mut app, KeyCode::Down);
        assert!(app.msg.is_none());
        jump_to_group(&mut app, "work/b");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["跑", "闲", "错", "乙", "甲", "问", "未读", "壳"]);
        shift(&mut app, KeyCode::Up); // can't leave work
        assert!(app.msg.is_none());
        jump_to_group(&mut app, "-");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话区域不能移动");
    }

    #[test]
    fn a_failed_group_write_puts_the_groups_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        jump_to_group(&mut app, "work");
        shift(&mut app, KeyCode::Up);
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed("移动分组失败:x".into()), sessions());
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        assert_eq!(app.msg.as_ref().unwrap().0, "移动分组失败:x");
        assert!(app.take_job().is_none());
    }

    #[test]
    fn the_tree_stops_indenting_after_a_few_levels() {
        let up = |i: usize| if i == 0 { Up::Top } else { Up::Under(format!("s{}", i - 1), String::new()) };
        let chain: Vec<Session> = (0..7).map(|i| Session { up: up(i), ..mk(&format!("s{i}"), "work", State::Idle, 10) }).collect();
        let view: Vec<&Session> = chain.iter().collect();
        let marks: Vec<String> = tree_marks(&view, true).into_iter().map(|m| m.0).collect();
        // From level TREE_DEEP down, drawn side by side, no further indent
        assert_eq!(marks, ["", "└ ", "  └ ", "    └ ", "      ├ ", "      ├ ", "      └ "]);
        // The parent isn't in the slice above (filtered out): top level counts from there
        let marks: Vec<String> = tree_marks(&view[2..5], true).into_iter().map(|m| m.0).collect();
        assert_eq!(marks, ["", "└ ", "  └ "]);
    }

    #[test]
    fn h_folds_from_a_session_or_header_and_l_unfolds_then_steps_in() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a')); // All: fun[问 未读] work[跑 闲 错]
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('h')); // On a session = fold its group right away, highlight lands on the fold row
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[▸ work  3]"]);
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('h')); // The fold row is the outermost level: no move, no group change
        assert_eq!((layout(&app).len(), app.group.as_deref()), (4, None));
        press(&mut app, KeyCode::Right); // → same as l: unfolds, stops on the header
        assert_eq!(layout(&app), ["[fun]", "问", "未读", "[work]", "跑", "闲", "错"]);
        assert_eq!(app.group_row(), Some("work"));
        press(&mut app, KeyCode::Left); // ← / h on an unfolded header folds it too
        assert!(app.on_fold_row());
        keys(&mut app, "ll"); // Unfold, into the group's first one
        assert_eq!((app.group_row(), at(&app)), (None, "跑".to_string()));
        press(&mut app, KeyCode::Char('l')); // No move on a session (only h folds)
        assert_eq!((app.folds.len(), app.group.as_deref(), at(&app)), (0, None, "跑".to_string()));
        pin(&mut app, "跑"); // The pinned block isn't in a section: h does nothing
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.folds.len(), at(&app)), (0, "跑".to_string()));
    }

    #[test]
    fn jump_hints_follow_agent_decks_algorithm() {
        assert_eq!(jump_hints(1), ["s"]);
        assert_eq!(jump_hints(5), ["a", "d", "f", "j", "s"]);
        assert!(jump_hints(13).iter().all(|h| h.len() == 1));
        for n in [20, 60, 200] {
            let h = jump_hints(n);
            assert_eq!(h.len(), n);
            let uniq: std::collections::HashSet<&String> = h.iter().collect();
            assert_eq!(uniq.len(), n);
            assert!(h.iter().all(|x| x.chars().all(|c| HINT_CHARS.contains(c))));
            // None is the start of another, or pressing halfway through would hit the wrong one
            assert!(h.iter().all(|x| h.iter().filter(|y| y.starts_with(x.as_str())).count() == 1));
        }
        let h = jump_hints(20);
        assert!(h.iter().any(|x| x.len() == 1) && h.iter().any(|x| x.len() == 2));
    }

    #[test]
    fn space_then_letter_enters_that_session() {
        let mut app = fixture(); // 1 day: [fun] 问 [work] 跑 闲 → hints a d f j s
        press(&mut app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), &Folds::new(), 0, false);
        let badges: Vec<&str> = rows.iter().filter(|r| r.pos.is_some()).map(|r| r.lines[0][0].0.as_str()).collect();
        assert_eq!(badges, ["d", "j", "s"]); // Hints take the place of the numbers
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.attach.take().as_deref(), Some("跑"));
        assert!(app.jump.is_none());
        // Esc / a letter that matches nothing: leave, don't enter
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Esc);
        assert!(app.jump.is_none() && !app.quit);
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('z'));
        assert!(app.jump.is_none() && app.attach.is_none());
        // Any other key: leave the jump and handle the key as usual
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('3'));
        assert!(app.jump.is_none());
        assert_eq!(at(&app), "闲");
    }

    #[test]
    fn two_letter_hints_wait_for_the_second_letter() {
        let v: Vec<Session> = (0..20).map(|i| mk(&format!("会话{i:02}"), "work", State::Idle, 10)).collect();
        let mut app = App::new(String::new(), v);
        let hints = jump_hints(21); // The group header takes the first
        let (i, two) = hints.iter().enumerate().find(|(i, h)| *i > 0 && h.len() == 2).unwrap();
        let want = app.visible().0[i - 1].name.clone();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(two.chars().next().unwrap()));
        assert!(app.jump.is_some() && app.attach.is_none());
        press(&mut app, KeyCode::Char(two.chars().nth(1).unwrap()));
        assert_eq!(app.attach, Some(want));
    }

    #[test]
    fn group_headers_get_letters_and_the_highlight_stops_on_them() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char(' '));
        let view = app.visible().0;
        let rows = list_rows(&view, true, 46, Some(""), &Folds::new(), 0, false);
        let heads: Vec<String> =
            rows.iter().filter(|r| r.head.is_some()).map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert_eq!(heads, ["a ▾ fun  1", "f ▾ work  2"]);
        press(&mut app, KeyCode::Char('f'));
        assert!(app.jump.is_none() && app.attach.is_none());
        assert_eq!(app.head.as_deref(), Some("work"));
        assert_eq!(at(&app), "跑"); // Behind it, the group's first one is selected
        // Drawn: highlight on the header row
        let mut t = Terminal::new(TestBackend::new(46, 20)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        let bg = |y: u16| buf[(4, y)].bg;
        // Row 0 top bar, 1 [fun], 2 问, 3 [work], 4 跑
        assert_eq!((bg(3), bg(4) == theme::pal().sel), (theme::pal().sel, false));
        // Still on the header after a refresh
        app.set_sessions(sessions());
        assert_eq!(app.head.as_deref(), Some("work"));
        // Showing only one group, there's no header to stop on
        press(&mut app, KeyCode::Char('W'));
        assert!(app.head.is_none());
        // The topmost group's header can be stopped on too; without sections no header gets a letter
        press(&mut app, KeyCode::Char('W'));
        jump_to_group(&mut app, "fun");
        assert_eq!(app.head.as_deref(), Some("fun"));
    }

    #[test]
    fn group_pane_shows_the_path_where_it_came_from_and_whats_inside() {
        let text = |l: &[Line]| {
            l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect::<Vec<_>>()
        };
        let sp = Spot { group: "work".into(), dir: "/p/work".into(), from: None };
        // Not read back yet: path only (the group's own default path gets no extra note)
        assert_eq!(text(&group_pane(&sp, None, None, false, 60, 20)), ["/p/work"]);
        let files = diff::Files {
            dirs: vec!["src".into(), "spec".into()],
            files: vec!["Cargo.toml".into(), "README.md".into(), "a".into()],
            hidden: 2,
            err: None,
        };
        let g = Glance {
            repo: Some(diff::Stat { branch: "main".into(), files: 3, add: 12, del: 4 }),
            files: Some(files),
            ..Default::default()
        };
        let t = text(&group_pane(&sp, Some(&g), None, false, 60, 20));
        assert_eq!(t[0], "/p/work  main  3 个文件 +12 −4");
        // Column width = the widest, Cargo.toml's 10 cells, + 2: 60 cells fit 5 columns, directories first; dotfiles neither listed
        // nor counted
        assert_eq!(t[1..], ["", "src/        spec/       Cargo.toml  README.md   a"]);
        // Doesn't fit: the last line counts how many more
        let t = text(&group_pane(&sp, Some(&g), None, false, 24, 4));
        assert_eq!(t[2..], ["src/        spec/", "还有 3 个"]);
        // Following a session's directory; the directory is gone
        let sp = Spot { from: Some("跑".into()), ..sp };
        let gone =
            Glance { files: Some(diff::Files { err: Some("目录已不存在".into()), ..Default::default() }), ..Default::default() };
        assert_eq!(text(&group_pane(&sp, Some(&gone), None, false, 60, 20)), ["/p/work", "「跑」的目录", "", "目录已不存在"]);
    }

    #[test]
    fn t_on_a_group_opens_a_terminal_in_the_groups_dir() {
        let mut v = sessions();
        v[3].dir = "/p/闲".into(); // The most recently active one in work is 「跑」
        let mut app = App::new(String::new(), v);
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        let r = app.term_req.take().unwrap();
        assert_eq!((r.key.as_str(), r.family.as_str(), r.title.as_str(), r.w.dir.as_str()), ("group:work", "", "work", "/p/work"));
        assert_eq!(app.head.as_deref(), Some("work"));
        // Equally recent: take the one higher in the list
        let mut v = sessions();
        (v[3].active_at, v[3].dir) = (v[2].active_at, "/p/闲".into());
        let mut t = App::new(String::new(), v);
        jump_to_group(&mut t, "work");
        press(&mut t, KeyCode::Char('t'));
        assert_eq!(t.term_req.take().unwrap().w.dir, "/p/work");
        // Opened before: goes back to that one
        app.terms.insert(r.key.clone(), crate::term::Term::spawn("/", &[], 5, 20).unwrap());
        press(&mut app, KeyCode::Char('t'));
        assert!(app.term_req.is_none() && app.term_on.as_deref() == Some("group:work"));
        app.terms.clear();
        app.term_on = None;
        // The group has a default path (directory still there): use it
        let tmp = || std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        let mut app = fixture().with_group_hints(|| crate::data::GroupHints {
            groups: vec![("work".into(), std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string())],
        });
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().w.dir, tmp());
        // Default path gone: fall back to the session's directory; a worktree uses its original repository
        let mut v = sessions();
        (v[2].worktree, v[2].info) = (true, "Repo: /repo\nPath: /p/work".into()); // 跑 (running)
        let mut app = App::new(String::new(), v)
            .with_group_hints(|| crate::data::GroupHints { groups: vec![("work".into(), "/没有这个目录".into())] });
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().w.dir, "/repo");
        // A fold row is a group too
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.term_req.take().unwrap().key, "group:work");
    }

    #[test]
    fn new_on_a_group_starts_in_the_groups_dir_and_group() {
        let mut v = sessions();
        v[2].tool = "codex".into(); // 跑: the most recently active one in work
        v[2].account = "team".into();
        v[3].dir = "/p/闲".into();
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('N'));
        let d = app.new_draft.as_ref().unwrap();
        assert_eq!((d.text.as_str(), d.group()), ("/p/work", ("work".into(), false)));
        press(&mut app, KeyCode::Enter);
        let r = app.create.take().unwrap();
        assert_eq!((r.dir.as_str(), r.group.as_str(), r.tool.as_str(), r.account.as_str()), ("/p/work", "work", "codex", "team"));
    }

    #[test]
    fn account_column_sits_before_the_age_with_a_fixed_width() {
        let mut v = sessions();
        v[2].who = "team".into(); // 跑 (running)
        v[4].who = "alt".into(); // 错 (error)
        v[4].agent = Agent::Codex;
        let text = |app: &App| -> Vec<String> {
            let view = app.visible().0;
            list_rows(&view, app.sectioned(), 46, None, app.fold_info(), app.who_width(), false)
                .iter()
                .filter(|r| r.pos.is_some())
                .map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect::<String>())
                .collect()
        };
        let mut app = App::new(String::new(), v.clone());
        assert_eq!(app.who_width(), 4);
        let rows = text(&app);
        assert!(rows.iter().all(|r| r.width() == 46), "{rows:?}");
        let run = rows.iter().find(|r| r.contains("跑")).unwrap();
        assert!(run.ends_with(" team  5秒 "), "{run}"); // 5秒 right-aligned to 4 cells
        // In English the column is 3 cells
        let en = i18n::with(Lang::En, || text(&app));
        assert!(en.iter().all(|r| r.width() == 46) && en.iter().any(|r| r.ends_with(" team  5s ")), "{en:?}");
        // Only sessions without an account left on show: the column width stays (computed over all sessions), titles don't shift
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["未读", "问"]);
        assert!(text(&app)[0].ends_with(&format!("{}11天 ", " ".repeat(2 + 1 + 4 + 1))), "{:?}", text(&app)); // Mark col + empty account col
        // Too long is cut; when no session has an account the column takes no space
        v[2].who = "a-very-long-slot".into();
        assert_eq!(App::new(String::new(), v.clone()).who_width(), 8);
        assert_eq!(fixture().who_width(), 0);
        // One in a sandbox says which after its account, a shell inside only that; a long account is cut, the sandbox kept
        (v[2].who, v[2].within) = ("team".into(), "work".into());
        (v[3].agent, v[3].who, v[3].within) = (Agent::None, String::new(), "work".into());
        let app = App::new(String::new(), v.clone());
        assert_eq!(app.who_width(), 9);
        let rows = text(&app);
        assert!(rows.iter().all(|r| r.width() == 46), "{rows:?}");
        assert!(rows.iter().find(|r| r.contains("跑")).unwrap().ends_with(" team@work  5秒 "), "{rows:?}");
        assert!(rows.iter().find(|r| r.contains("闲")).unwrap().ends_with(" @work     10秒 "), "{rows:?}");
        v[2].who = "a-very-long-slot".into();
        assert_eq!((App::new(String::new(), v.clone()).who_width(), who_cell(&v[2]).as_str()), (13, "a-very-…@work"));
    }

    #[test]
    fn shift_o_folds_all_pins_stay_and_filter_unfolds() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(layout(&app), ["[置顶]", "跑", "[▸ fun  2 ◆1]", "[▸ work  2]"]); // Pinned ones aren't in sections
        let saved = app.view_state();
        assert_eq!(saved.folded, ["fun", "work"]);
        assert_eq!(fixture().restore(saved).folds.len(), 2); // Remembered
        typ(&mut app, "/错"); // Filtering unfolds everything, so search misses nothing
        assert_eq!(order(&app), ["错"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(layout(&app).len(), 8);
        press(&mut app, KeyCode::Char('}')); // Only one group: no sections to fold
        press(&mut app, KeyCode::Char('o'));
        assert!(app.folds.is_empty());
    }

    #[test]
    fn sections_get_a_header_each_and_pins_sit_above_them() {
        let mut app = fixture();
        assert_eq!(layout(&app), ["[fun]", "问", "[work]", "跑", "闲"]);
        // Header: the group name followed by how many this section has
        let view = app.visible().0;
        let head: String =
            list_rows(&view, true, 30, None, app.fold_info(), 0, false)[2].lines[0].iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(head, " ▾ work  2");
        // On desktop it's one session per row too, but always numbered, with a blank line between groups (the blank line is an
        // item too, not selectable)
        let desk = list_rows(&view, true, 46, None, app.fold_info(), 0, true);
        assert_eq!(desk.iter().map(|r| r.lines.len()).collect::<Vec<_>>(), [1, 1, 1, 1, 1, 1]);
        assert!(desk[2].pos.is_none() && desk[2].lines[0].is_empty());
        assert_eq!(desk[1].lines[0][0].0, "  1 ");
        pin(&mut app, "闲");
        assert_eq!(layout(&app), ["[置顶]", "闲", "[fun]", "问", "[work]", "跑"]);
        pin(&mut app, "问"); // fun has only the pinned one left; an empty section draws no header
        assert_eq!(layout(&app), ["[置顶]", "闲", "问", "[work]", "跑"]);
        press(&mut app, KeyCode::Char('}')); // Only one group: no sections
        assert_eq!(layout(&app), ["问"]);
    }

    fn sess(state: State, screen: &[&str]) -> Session {
        Session {
            name: "s".into(),
            tmux: "$1".into(),
            title: "t".into(),
            locked: false,
            group: "g".into(),
            state,
            question: String::new(),
            idle_for: 0,
            active_at: 0,
            ran: 0,
            attached: false,
            screen: screen.iter().map(|s| s.to_string()).collect(),
            pinned: false,
            noted: false,
            group_noted: false,
            space: spaces::HOME,
            dir: String::new(),
            profile: String::new(),
            tool: String::new(),
            account: String::new(),
            agent: Agent::None,
            who: String::new(),
            within: String::new(),
            pid: 0,
            deck_id: String::new(),
            info: String::new(),
            worktree: false,
            slot: (0, 0, 0),
            pane: String::new(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: vec![],
            error: String::new(),
            error_kind: String::new(),
            idle_stopped: 0,
            due: vec![],
            blank: false,
        }
    }

    #[test]
    fn preview_cuts_input_box_even_with_panels_below() {
        let bar = "─".repeat(60);
        let mut screen = vec!["⏺ 结果", "· Choreographing… (9m 10s)", "  ⎿  Done", "", &bar, "❯\u{a0}", &bar];
        let panels = [
            "  ━━╌╌ 50% │ 模型",
            "  $1 100m",
            "  ⏵⏵ bypass permissions on",
            "  ◯ task 1",
            "  ◯ task 2",
            "  ◯ task 3",
            "  ◯ task 4",
            "  ◯ task 5",
            "  ◯ task 6",
        ];
        screen.extend(panels);
        let got = preview_lines(&sess(State::Running, &screen));
        assert_eq!(ansi::plain(got.last().unwrap()), "  ⎿  Done");
    }

    #[test]
    fn preview_cuts_input_box_with_session_name_in_border() {
        let top = format!("{} 项目-主会话 ─", "─".repeat(80));
        let bar = "─".repeat(90);
        let screen = ["⏺ 结果", "· Swooping… (14m 28s)", "", top.as_str(), "❯\u{a0}", bar.as_str(), "  ━━╌ 50%"];
        assert_eq!(ansi::plain(preview_lines(&sess(State::Running, &screen)).last().unwrap()), "· Swooping… (14m 28s)");
    }

    #[test]
    fn preview_keeps_dialog_when_asking() {
        let screen = ["问题?", "❯ 1. 是", "  2. 否", "Enter to select"];
        assert_eq!(preview_lines(&sess(State::Ask, &screen)).len(), 4);
    }

    #[test]
    fn group_pane_puts_the_note_under_the_path_and_gives_it_half_the_pane() {
        let sp = Spot { group: "work".into(), dir: "/p/work".into(), from: None };
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        let long = (1..=20).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        let got = text(&group_pane(&sp, None, Some(&long), false, 30, 20));
        assert_eq!(got[..3], ["/p/work", "✎ 第1行", "  第2行"]);
        assert_eq!(got.len(), 10, "{got:#?}");
        assert_eq!(got[9], "  … 还有 12 行");
        // While writing: cursor at the end, still half
        let got = text(&group_pane(&sp, None, Some(&long), true, 30, 20));
        assert_eq!((got.len(), got[1].as_str(), got[9].as_str()), (10, "✎ 便签", "  第20行▏"));
    }

    #[test]
    fn a_deleted_groups_note_goes_to_the_trash_and_comes_back_with_it() {
        let mut app = App::new(String::new(), life_and_work()).with_groups(deck_groups);
        app.notes.set("group:fun", "整体的想法").unwrap();
        let bin = |app: &mut App| {
            jump_to_group(app, "fun");
            keys(app, "dy");
            let job = app.take_job().unwrap();
            app.job_done(&job, fun_binned(), without_fun());
        };
        let restore = |app: &mut App| {
            ctrl(app, 'z');
            let job = app.take_job().unwrap();
            app.job_done(&job, Outcome::Done("已恢复分组 fun · 4 个会话".into()), life_and_work());
        };
        bin(&mut app);
        // Kept in the Trash entry and cleared from the notes: a new fun starts empty
        assert_eq!(app.trash.get("x1").unwrap().groups.iter().map(|g| g.note.as_str()).collect::<Vec<_>>(), ["整体的想法", ""]);
        assert_eq!(app.notes.get("group:fun"), None);
        restore(&mut app);
        assert_eq!(app.notes.get("group:fun"), Some("整体的想法"));
        // A fun created in the meantime got a note: the restored one is appended after it
        bin(&mut app);
        app.notes.set("group:fun", "新的").unwrap();
        restore(&mut app);
        assert_eq!(app.notes.get("group:fun"), Some("新的\n\n整体的想法"));
    }

    #[test]
    fn preview_head_states_the_facts_and_caps_long_notes() {
        let mut s = mk("闲", "work", State::Idle, 70);
        s.dir = "/p/work".into();
        s.info = "Repo: /r\nPath: /p/work\nBranch: feat".into();
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        // State · how long ago, directory, branch, then a blank line before the screen content
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10)), ["空闲 · 1 分钟前   /p/work  feat", ""]);
        // Session id and open-elsewhere aren't in the preview (B copies the id; ⇄ is on the list row)
        s.info.push_str("\nSession: 0000aaaa-1111-2222-3333-444455556666");
        s.attached = true;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10)), ["空闲 · 1 分钟前   /p/work  feat", ""]);
        // Narrow: cut from the end
        assert_eq!(text(&preview_head(&s, None, None, false, 24, 10))[0], "空闲 · 1 分钟前   /p/wo…");
        // Child session: who dispatched it; also says so when the parent is deleted
        s.up = Up::Apart("p".into(), "派活的".into());
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "「派活的」的子会话");
        s.up = Up::Gone;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "子会话 · 父会话已删除");
        s.up = Up::Top;
        // A fork: forked from whom; also says so when the original is deleted
        s.fork = Fork::Of("原来的".into());
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "分叉自「原来的」");
        s.fork = Fork::Gone;
        assert_eq!(text(&preview_head(&s, None, None, false, 60, 10))[1], "分叉 · 原会话已删除");
        s.fork = Fork::No;
        // A note gets at most three lines; beyond that, says how many more
        let long = (1..=20).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        let got = text(&preview_head(&s, None, Some(&long), false, 30, 20));
        assert_eq!(got[1..], ["✎ 第1行", "  第2行", "  第3行", "  … 还有 17 行", ""]);
        // While writing: even a short one takes the fixed top half; a long one keeps its last lines (where the cursor is)
        let got = text(&preview_head(&s, None, Some("一行"), true, 30, 20));
        assert_eq!(got.len(), 10, "{got:#?}");
        assert_eq!((got[1].as_str(), got[2].as_str()), ("✎ 便签", "  一行▏"));
        let got = text(&preview_head(&s, None, Some(&long), true, 30, 20));
        assert_eq!((got[2].as_str(), got[8].as_str()), ("  … 上面还有 14 行", "  第20行▏"));
    }

    #[test]
    fn preview_head_puts_the_change_count_first_and_trims_the_dir_from_the_left() {
        let mut s = mk("闲", "work", State::Idle, 70);
        s.dir = "/p/ws".into();
        s.info = "Path: /p/ws".into();
        let text = |l: &[Line]| l.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        let g = Glance {
            work: aca_records::workdir::Found { dir: "/p/ws/some-long-project-name/app".into(), how: "Claude 现在在这" },
            repo: Some(diff::Stat { branch: "main".into(), files: 3, add: 12, del: 4 }),
            files: None,
        };
        // Uses the directory it really works in and the branch there
        assert_eq!(
            text(&preview_head(&s, Some(&g), None, false, 80, 10))[0],
            "空闲 · 1 分钟前  3 个文件 +12 −4   /p/ws/some-long-project-name/app  main"
        );
        // Narrow: stats and branch stay, the directory is cut from the left
        assert_eq!(text(&preview_head(&s, Some(&g), None, false, 46, 10))[0], "空闲 · 1 分钟前  3 个文件 +12 −4   …/app  main");
        let clean = Glance { repo: Some(diff::Stat { branch: "main".into(), ..Default::default() }), ..g };
        assert_eq!(
            text(&preview_head(&s, Some(&clean), None, false, 80, 10))[0],
            "空闲 · 1 分钟前   /p/ws/some-long-project-name/app  main"
        );
    }
}
