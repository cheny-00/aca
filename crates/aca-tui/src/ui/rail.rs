//! The rail down the left of a wide screen: where you can go. One row for everything, then the pinned ones, the groups as a
//! tree, plain tmux sessions and the Trash. Each row says what going there would list: how many sessions and how many of them
//! are in each of the states the top bar counts. A narrower screen keeps none: `h` lays it over the left while the cursor is
//! on it.

use super::*;

/// Wide enough for the rail next to the list and the preview; under this they keep the whole width
pub(super) const RAIL_MIN_W: u16 = 130;
/// Columns of a rail row. A line and a blank column follow before the list
pub(super) const RAIL_W: u16 = 28;

pub(super) fn rail_shown(screen: Rect) -> bool {
    screen.width >= RAIL_MIN_W
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum RailKind {
    /// Every group
    All,
    /// The pinned block of the list (not somewhere to go on its own)
    Pinned,
    Group(String),
    /// A level of a group's path that is no group itself: only says what the rows under it share
    Label,
    Gap,
    Trash,
}

impl RailKind {
    /// Whether the rail's cursor can stop on a row of this kind
    fn stop(&self) -> bool {
        matches!(self, RailKind::All | RailKind::Group(_) | RailKind::Trash)
    }
}

/// Where the rail's cursor is
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RailAt {
    /// On the row of the place the list shows (every group, or the group viewed)
    Here,
    Trash,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RailRow {
    pub kind: RailKind,
    /// Levels in: a subgroup sits one further in than its parent
    pub depth: usize,
    pub name: String,
    /// Sessions listed on going there (0 = no number written)
    pub n: usize,
    /// How many of them are in each of [`TALLY_MARKS`]
    pub marks: [usize; 4],
    /// A group that has a router: the color of its router's state, for the @ before the name. The row for all groups has
    /// the desk's; the scratch group's own row has none (the desk is its router)
    pub router: Option<Style>,
    /// The scratch group's own row: its mark leads the name
    pub scratch: bool,
    /// A group with task tickets: how many are done, and how many it has, written after its name (6/11)
    pub tasks: Option<(usize, usize)>,
}

/// The order marks are written in: what waits for you first (◆ ◐ ✕), running ones last. Indexes into [`TALLY_MARKS`]
const MARK_ORDER: [usize; 4] = [0, 2, 3, 1];

impl RailRow {
    fn plain(kind: RailKind, name: &str) -> Self {
        Self { kind, depth: 0, name: name.to_string(), n: 0, marks: [0; 4], router: None, scratch: false, tasks: None }
    }
}

impl App {
    /// The rail's rows top to bottom. Numbers follow the scope, the state picked and the search, as the list does; a pinned
    /// session counts in its own group (going there lists it), and in the pinned row
    pub(super) fn rail_rows(&self) -> Vec<RailRow> {
        let view = self.listing(None).1;
        let mut tally: HashMap<&str, (usize, [usize; 4])> = HashMap::new();
        let mut pinned = 0;
        for s in view.iter().map(|&i| &self.all[i]).filter(|s| counted(s)) {
            let e = tally.entry(s.group.as_str()).or_default();
            e.0 += usize::from(s.sub.is_none());
            if let Some(k) = TALLY_MARKS.iter().position(|&st| st == s.state) {
                e.1[k] += 1;
            }
            pinned += usize::from(s.pinned && s.sub.is_none());
        }
        // Groups in list order (a parent is followed right by its subgroups, see `group_ranks`). Empty ones are rows too, at
        // their own place in the tree: the list sinks their headings to the bottom, keeping the rank last in the slot
        let rank = |s: &Session| if s.blank { s.slot.2 as usize } else { s.slot.0 };
        let mut groups: Vec<(usize, &str)> =
            self.all.iter().filter(|s| s.space == self.space).map(|s| (rank(s), s.group.as_str())).collect();
        groups.sort();
        groups.dedup();
        let known: HashSet<&str> = groups.iter().map(|g| g.1).collect();

        let (n, marks) = self.group_tally(None);
        // The desk dispatches for every group: its @ leads this row
        let desk = self.desk().map(|s| session_icon(s).1);
        let mut rows = vec![RailRow { n, marks, router: desk, ..RailRow::plain(RailKind::All, t!("全部", "All")) }];
        let mut labelled: HashSet<String> = HashSet::new();
        // A group's row, after rows for the levels of its path that are no groups. `base`: how many levels of the path aren't
        // drawn (the scratch group stands flush left whatever it's under)
        let routers = self.group_routers();
        let scratch = self.scratch.as_deref();
        let mut push = |rows: &mut Vec<RailRow>, g: &str, base: usize| {
            let (n, marks) = tally.get(g).copied().unwrap_or_default();
            // Plain tmux sessions and ungrouped ones have a name of their own, not a path
            if group_label(g) != g {
                return rows.push(RailRow { n, marks, ..RailRow::plain(RailKind::Group(g.to_string()), group_label(g)) });
            }
            let router = routers.iter().find(|(x, _)| x == g).map(|(_, st)| *st);
            let parts: Vec<&str> = g.split('/').collect();
            for i in base + 1..parts.len() {
                let up = parts[..i].join("/");
                if !known.contains(up.as_str()) && labelled.insert(up) {
                    rows.push(RailRow { depth: i - 1 - base, ..RailRow::plain(RailKind::Label, parts[i - 1]) });
                }
            }
            let name = parts.last().copied().unwrap_or(g).to_string();
            let (kind, depth) = (RailKind::Group(g.to_string()), parts.len() - 1 - base);
            let tasks = Some(self.tickets.tally(g)).filter(|t| t.1 > 0);
            rows.push(RailRow { kind, depth, name, n, marks, router, scratch: scratch == Some(g), tasks });
        };
        // The scratch group and what's under it come right after all groups, before the pinned ones (the list's order too)
        let under_scratch = |g: &str| scratch.is_some_and(|s| groups::within(g, s));
        let base = scratch.map_or(0, |s| s.split('/').count() - 1);
        // A blank row between all groups and the scratch group, as between the scratch group and the rest
        if groups.iter().any(|(_, g)| under_scratch(g)) {
            rows.push(RailRow::plain(RailKind::Gap, ""));
        }
        for (_, g) in groups.iter().filter(|(_, g)| under_scratch(g)) {
            push(&mut rows, g, base);
        }
        rows.push(RailRow::plain(RailKind::Gap, ""));
        if pinned > 0 {
            rows.push(RailRow { n: pinned, ..RailRow::plain(RailKind::Pinned, t!("置顶", "Pinned")) });
        }
        for (_, g) in groups.iter().filter(|(_, g)| !under_scratch(g)) {
            push(&mut rows, g, 0);
        }
        rows.push(RailRow::plain(RailKind::Gap, ""));
        rows.push(RailRow::plain(RailKind::Trash, t!("回收站", "Trash")));
        rows
    }

    /// The rail row for where the list is now: every group, or the one group viewed
    pub(super) fn rail_at(&self, rows: &[RailRow]) -> Option<usize> {
        rows.iter().position(|r| match (&r.kind, &self.group) {
            (RailKind::All, None) => true,
            (RailKind::Group(g), Some(cur)) => g == cur,
            _ => false,
        })
    }

    /// The rail row the bar is drawn on: the Trash while the cursor is on it, otherwise where the list is
    fn rail_cursor(&self, rows: &[RailRow]) -> Option<usize> {
        match self.rail {
            Some(RailAt::Trash) => rows.iter().position(|r| r.kind == RailKind::Trash),
            _ => self.rail_at(rows),
        }
    }

    pub(super) fn on_rail(&self) -> bool {
        self.rail.is_some()
    }

    /// A rail is on screen: a wide screen always has one; a narrower one while the cursor is on it (see `out_to_rail`)
    pub(super) fn rail_on_screen(&self) -> bool {
        rail_shown(self.screen) || self.rail.is_some()
    }

    /// Out of the list onto the rail (h / ← at the list's outermost level): the cursor lands on the row of the place the list
    /// shows. A screen too narrow to keep a rail gets it laid over its left for as long as the cursor is on it; where the list
    /// was is remembered, for leaving without going anywhere (`rail_back`)
    pub(super) fn out_to_rail(&mut self) {
        self.rail_from = (!rail_shown(self.screen)).then(|| self.group.clone());
        self.hop_back = None;
        self.rail = Some(RailAt::Here);
    }

    /// h / Esc on a rail laid over a narrow screen: put it away, the list back on the place it showed when the rail was called
    /// out, the cursor in the list. False = this rail is the screen's own (wide): nothing to put away
    fn rail_back(&mut self) -> bool {
        if rail_shown(self.screen) {
            return false;
        }
        self.rail = None;
        let from = self.rail_from.take().filter(|g| g.as_deref().is_none_or(|g| self.group_listed(g)));
        match from {
            Some(g) => self.set_group(g),
            // Nowhere to go back to (the rail wasn't called out on this screen, or that group is gone since): the list stays
            // on what it shows, unless that lists nothing
            None if self.group.as_deref().is_some_and(|g| !self.group_listed(g)) => self.set_group(None),
            None => {}
        }
        true
    }

    /// The cursor moves d stops along the rail (every group, each group, the Trash). The list shows the place it lands on at
    /// once; on the Trash the list stays as it was
    pub(super) fn rail_step(&mut self, d: isize) {
        let rows = self.rail_rows();
        let stops: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].kind.stop()).collect();
        let cur = self.rail_cursor(&rows).and_then(|at| stops.iter().position(|&i| i == at)).unwrap_or(0) as isize;
        let Some(&to) = stops.get((cur + d).clamp(0, stops.len() as isize - 1) as usize) else { return };
        self.rail_go(&rows[to].kind);
    }

    /// Put the rail's cursor on this row
    pub(super) fn rail_go(&mut self, kind: &RailKind) {
        if kind.stop() {
            self.hop_back = None;
        }
        match kind {
            RailKind::All => {
                self.rail = Some(RailAt::Here);
                self.set_group(None);
            }
            RailKind::Group(g) => {
                self.rail = Some(RailAt::Here);
                self.set_group(Some(g.clone()));
            }
            RailKind::Trash => self.rail = Some(RailAt::Trash),
            RailKind::Pinned | RailKind::Label | RailKind::Gap => {}
        }
    }

    /// ⏎ / l / → on the rail: into the place, the cursor on the row it was on when the place was left (see `set_group`); on the
    /// Trash its page opens. A place that lists nothing has no row to go to
    fn rail_enter(&mut self) {
        match self.rail {
            Some(RailAt::Trash) => self.open_trash(),
            // Its notes page and its tasks page have somewhere to be even with nothing on them yet: where n makes the first
            _ if self.view.is_empty() && !self.off_list() => {}
            _ => self.rail = None,
        }
    }

    /// A key with the cursor on the rail. False = not taken here: the keys that mean the same wherever the cursor is, and on a
    /// group's row the group's own keys, which act on it as on its heading in the list (see `group_row`); on the row for all
    /// groups, the two that reach the desk. Keys that act on a session do nothing: no session is under the cursor
    pub(super) fn on_rail_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let on_group = self.on_group_row();
        let on_all = self.on_all_row();
        match k.code {
            // ^G writes the group's note in the editor
            KeyCode::Char('g') if ctrl => return !on_group,
            _ if ctrl => return false,
            // ⇧↑ ⇧↓ move the group
            KeyCode::Up | KeyCode::Down if shift => return !on_group,
            KeyCode::Up | KeyCode::Char('k') => self.rail_step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.rail_step(1),
            KeyCode::PageUp => self.rail_step(-5),
            KeyCode::PageDown => self.rail_step(5),
            KeyCode::Home | KeyCode::Char('g') => self.rail_step(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.rail_step(10_000),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.rail_enter(),
            // On a group's row: the other of its pages shows in the middle column
            KeyCode::Tab if on_group && self.group.is_some() => self.flip_page(),
            // Nothing is further out than the rail; one laid over a narrow screen is put away
            KeyCode::Left | KeyCode::Char('h') => {
                self.rail_back();
            }
            // A search or a state picked goes first, as in the list; then a rail laid over is put away
            KeyCode::Esc if self.filter.is_empty() && self.status.is_none() && self.rail_back() => {}
            KeyCode::Esc => return false,
            KeyCode::Char(c) if c.is_ascii_digit() || " i/?qwFaSb$MPLnN".contains(c) => {
                // The ones that act on the list of sessions: on a group showing another of its pages, its sessions come back
                // first. Not Space on a rail laid over the list, which covers the sessions' letters: the groups' own show on it
                let covered = c == ' ' && !rail_shown(self.screen);
                if (c.is_ascii_digit() || " i/wF".contains(c)) && self.off_list() && !covered {
                    self.show_sessions();
                }
                return false;
            }
            // Rename, delete, note, a terminal in its directory, send it to a workspace; its router: open it, reply to it
            KeyCode::Char('r' | 'd' | 'e' | 't' | 'Q' | 'o' | 's') if on_group => return false,
            // The desk: open it, reply to it
            KeyCode::Char('o' | 's') if on_all => return false,
            // On any row ` shows the groups' letters
            KeyCode::Char('`') => return false,
            _ => {}
        }
        true
    }

    /// A click on the rail: the cursor goes to that row; on the row it is already on, into the place (as ⏎)
    pub(super) fn rail_click(&mut self, row: u16) {
        let rows = self.rail_rows();
        let Some(i) = Some(self.rail_top + (row - self.rail_area.y) as usize).filter(|&i| i < rows.len() && rows[i].kind.stop()) else {
            return;
        };
        if self.on_rail() && self.rail_cursor(&rows) == Some(i) {
            return self.rail_enter();
        }
        self.rail_go(&rows[i].kind);
    }
}

/// Cut in the middle and pad to a display width: long names often differ only at the end (`…-runtime`, `…-runtime-v3`), and
/// cut at the end they'd read the same
fn clip_mid(s: &str, w: usize) -> String {
    if s.width() <= w || w < 5 {
        return clip(s, w);
    }
    let tail_w = (w - 1) / 2;
    let head_w = w - 1 - tail_w;
    let take = |chars: &mut dyn Iterator<Item = char>, room: usize| -> (Vec<char>, usize) {
        let (mut out, mut n) = (vec![], 0);
        for ch in chars {
            let cw = ch.width().unwrap_or(0);
            if n + cw > room {
                break;
            }
            out.push(ch);
            n += cw;
        }
        (out, n)
    };
    let (head, hn) = take(&mut s.chars(), head_w);
    let (tail, tn) = take(&mut s.chars().rev(), tail_w);
    let (head, tail): (String, String) = (head.into_iter().collect(), tail.into_iter().rev().collect());
    format!("{head}…{tail}{}", " ".repeat(w - hn - 1 - tn))
}

/// One rail row, `w` columns: a bar on the row the cursor or the list is on (`here`; `lit` = the cursor is on the rail, so the
/// row is highlighted as the list's is), the name (further in per level, cut when long), then the marks and the number at the
/// right edge. A row that would list nothing is gray and has no number. `key`: the group's letter in jump mode, written
/// like a session's in the two columns before the names (so no name moves or is cut shorter for it)
/// How many sessions are in each of [`TALLY_MARKS`], as the rail writes them: each mark with how many after it (none for
/// one), what waits for you first, a space between
pub(super) fn mark_spans(marks: &[usize; 4]) -> Vec<Span<'static>> {
    let mut v: Vec<Span<'static>> = vec![];
    for k in MARK_ORDER {
        let n = marks[k];
        if n == 0 {
            continue;
        }
        let (ic, sty) = icon(TALLY_MARKS[k]);
        if !v.is_empty() {
            v.push(Span::raw(" "));
        }
        v.push(Span::styled(ic, sty));
        if n > 1 {
            v.push(Span::raw(n.to_string()));
        }
    }
    v
}

fn rail_line(r: &RailRow, here: bool, lit: bool, key: Option<char>, w: usize) -> Line<'static> {
    if r.kind == RailKind::Gap {
        return Line::default();
    }
    let mut right = mark_spans(&r.marks);
    // The number has its own 4 columns at the edge, so numbers line up down the rail whatever the marks
    let count = if r.n > 0 { format!("{:>4}", r.n) } else { " ".repeat(4) };
    right.push(Span::styled(count, dim()));
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let name_st = match r.kind {
        RailKind::Label => theme::faint(),
        _ if here => Style::new().add_modifier(Modifier::BOLD),
        RailKind::Trash => dim(),
        RailKind::Group(_) | RailKind::All if r.n == 0 => dim(),
        _ => Style::new(),
    };
    // A router's @ stands in the two cells right before the group's name: the last of its indent, or at the top level the
    // cells a jump letter takes while the letters show. The scratch group's mark stands there too, written as its name is
    let at = |st: Style| Span::styled(format!("{} ", list::ROUTER), st);
    let lead: Vec<Span<'static>> = match (key, &r.kind, r.router) {
        (Some(c), _, _) => hint_badge(&c.to_string(), "").into_iter().map(|(t, st)| Span::styled(t, st)).collect(),
        (None, RailKind::Pinned, _) => vec![Span::styled("★ ", Style::new().fg(theme::pal().amber))],
        (None, _, _) if r.scratch => vec![Span::styled(format!("{SCRATCH} "), name_st)],
        (None, _, Some(st)) if r.depth == 0 => vec![at(st)],
        (None, _, _) => vec![Span::raw("  ")],
    };
    let indent = 2 * r.depth;
    let room = w.saturating_sub(2 + 2 + indent + right_w + usize::from(right_w > 4));
    // The bar is the cursor's, in the accent color like the list's; faint when the cursor is in the list and the row only
    // says which group the list is in
    let bar = match (here, lit) {
        (true, true) => Span::styled("▌ ", theme::accent()),
        (true, false) => Span::styled("▌ ", theme::faint()),
        (false, _) => Span::raw("  "),
    };
    let mut v = vec![bar];
    v.extend(lead);
    match r.router.filter(|_| r.depth > 0) {
        Some(st) => v.extend([Span::raw(" ".repeat(indent - 2)), at(st)]),
        None => v.push(Span::raw(" ".repeat(indent))),
    }
    // A group with tickets: how many are done of how many, after its name — when the name still has room beside it
    let tally = r.tasks.map(|(done, all)| format!("  {done}/{all}")).filter(|t| r.name.width() + t.width() <= room);
    match tally {
        // Right after the name: the name fits whole (see the filter above)
        Some(t) => v.extend([Span::styled(r.name.clone(), name_st), Span::styled(t, dim())]),
        None => v.push(Span::styled(clip_mid(&r.name, room), name_st)),
    }
    let used: usize = v.iter().map(|s| s.content.width()).sum();
    v.push(Span::raw(" ".repeat(w.saturating_sub(used + right_w))));
    v.extend(right);
    let line = Line::from(v);
    if here && lit { line.style(theme::raised()) } else { line }
}

/// Draw the rail into `area` (its last column is the line that parts it from the list). More rows than fit: scrolled just far
/// enough to keep the row with the bar in view
pub(super) fn draw_rail(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.rail_rows();
    let at = app.rail_cursor(&rows);
    let h = area.height as usize;
    let off = at.map_or(0, |p| (p + 1).saturating_sub(h));
    let w = area.width.saturating_sub(1) as usize;
    let lit = app.on_rail();
    // In jump mode, and while ` shows them, each group's row leads with its letter
    let keys = if app.jump.is_some() || app.letters.is_some() { app.group_keys() } else { vec![] };
    let key_of = |r: &RailRow| match &r.kind {
        RailKind::Group(g) => keys.iter().find(|(_, x)| x == g).map(|(c, _)| *c),
        _ => None,
    };
    let lines: Vec<Line> = rows.iter().enumerate().skip(off).take(h).map(|(i, r)| rail_line(r, at == Some(i), lit, key_of(r), w)).collect();
    f.render_widget(Paragraph::new(lines), Rect { width: area.width.saturating_sub(1), ..area });
    let edge = Rect { x: area.x + area.width.saturating_sub(1), width: 1, ..area };
    f.render_widget(Paragraph::new(vec![Line::styled("│", theme::rule()); h]), edge);
    app.rail_area = Rect { width: area.width.saturating_sub(1), height: area.height.min(rows.len().saturating_sub(off) as u16), ..area };
    app.rail_top = off;
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;

    /// Sessions in groups ranked as the registry would: a parent right before its subgroups
    fn grouped(groups: &[&str], v: Vec<(&str, &str, State, u64)>) -> Vec<Session> {
        let rank = |g: &str| groups.iter().position(|x| *x == g).unwrap_or(usize::MAX);
        v.into_iter()
            .enumerate()
            .map(|(i, (title, group, state, idle))| {
                let mut s = mk(title, group, state, idle);
                s.slot = (rank(group), i as i64, 0);
                s
            })
            .collect()
    }

    fn shape(app: &App) -> Vec<String> {
        app.rail_rows()
            .iter()
            .map(|r| match r.kind {
                RailKind::Gap => String::new(),
                _ => format!("{}{}{}", "  ".repeat(r.depth), r.name, if r.n > 0 { format!(" {}", r.n) } else { String::new() }),
            })
            .collect()
    }

    #[test]
    fn the_rail_lists_groups_as_the_tree_their_paths_make() {
        let groups = ["fun", "fun/probe", "lab/x", "lab/y", "work", "空"];
        let mut v = grouped(
            &groups,
            vec![
                ("问", "fun", State::Ask, 10),
                ("探", "fun/probe", State::Running, 10),
                ("甲", "lab/x", State::Idle, 10),
                ("乙", "lab/y", State::Unread, 10),
                ("跑", "work", State::Running, 10),
                ("错", "work", State::Error, 10),
                ("旧", "work", State::Stopped, 9 * 86_400),
                ("壳", "-", State::Idle, 10),
            ],
        );
        v.push(crate::data::blank_row("空", 5));
        let mut app = App::new(String::new(), v);
        // 「lab」 is no group itself: a label over its two subgroups, written once. An empty group is a row without a number,
        // plain tmux sessions come last
        let want = ["全部 7", "", "fun 1", "  probe 1", "lab", "  x 1", "  y 1", "work 2", "空", "其他 tmux 会话 1", "", "回收站"];
        assert_eq!(shape(&app), want);
        let rows = app.rail_rows();
        assert_eq!(rows.iter().filter(|r| r.kind == RailKind::Label).count(), 1);
        let work = rows.iter().find(|r| r.kind == RailKind::Group("work".into())).unwrap();
        assert_eq!(work.marks, [0, 1, 0, 1]); // ● 跑, ✕ 错
        // The numbers are what going there would list: they follow w and F
        press(&mut app, KeyCode::Char('F'));
        press(&mut app, KeyCode::Char('0'));
        assert!(shape(&app).contains(&"work 3".to_string()), "{:?}", shape(&app));
        press(&mut app, KeyCode::Esc);
        // A pinned one has its own row, and still counts in its group
        pin(&mut app, "跑");
        let rail = shape(&app);
        assert_eq!(&rail[..4], ["全部 7", "", "置顶 1", "fun 1"]);
        assert!(rail.contains(&"work 2".to_string()), "{rail:?}");
        // Where the list is: every group, or the one viewed
        assert_eq!(app.rail_at(&app.rail_rows()), Some(0));
        app.set_group(Some("lab/y".into()));
        let rows = app.rail_rows();
        assert_eq!(app.rail_at(&rows).map(|p| rows[p].name.clone()).as_deref(), Some("y"));
    }

    /// 「随手问」 works in /h and has a project under it; the other groups have no directory
    fn scratch_hints() -> crate::data::GroupHints {
        let g = |n: &str, d: &str| (n.to_string(), d.to_string());
        crate::data::GroupHints { groups: vec![g("fun", ""), g("work", ""), g("随手问", "/h"), g("随手问/rss", "/h/rss")] }
    }

    #[test]
    fn the_scratch_group_comes_right_after_all_groups_and_takes_what_is_started_off_any_project() {
        let groups = ["fun", "work", "随手问", "随手问/rss"];
        let v = || {
            grouped(
                &groups,
                vec![
                    ("问", "fun", State::Ask, 10),
                    ("跑", "work", State::Running, 10),
                    ("查", "随手问", State::Idle, 10),
                    ("订", "随手问/rss", State::Idle, 10),
                    ("壳", "-", State::Idle, 10),
                ],
            )
        };
        // No scratch directory set: a group like any other, where the registry puts it
        let mut plain = App::new(String::new(), v()).with_group_hints(scratch_hints);
        assert_eq!(shape(&plain), ["全部 5", "", "fun 1", "work 1", "随手问 1", "  rss 1", "其他 tmux 会话 1", "", "回收站"]);
        assert_eq!(order(&plain), ["问", "跑", "查", "订", "壳"]);
        select(&mut plain, "壳");
        press(&mut plain, KeyCode::Char('N'));
        assert_eq!(plain.new_draft.take().unwrap().text, "/p/-");
        // Set: it and what is under it come right after all groups, before the pinned ones, on the rail and in the list alike
        let mut app = App::new(String::new(), v())
            .with_group_hints(scratch_hints)
            .with_scratch(|| "/h".into(), |_| Ok("随手问".into()))
            .with_tools(|| Ok(vec![tool("codex", "codex", "", Agent::Codex)]));
        reader_rows(&mut app, 150, 40);
        pin(&mut app, "跑");
        // A blank row on either side of the scratch group
        assert_eq!(shape(&app), ["全部 5", "", "随手问 1", "  rss 1", "", "置顶 1", "fun 1", "work 1", "其他 tmux 会话 1", "", "回收站"]);
        assert_eq!(order(&app), ["查", "订", "跑", "问", "壳"]);
        // N with no project under the cursor (a plain tmux session, the rail's row for all groups): in its directory, into it
        let started = |app: &mut App| app.new_draft.take().map(|d| (d.text.clone(), d.group().0)).unwrap();
        select(&mut app, "壳");
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(started(&mut app), ("/h".to_string(), "随手问".to_string()));
        // n: the same place, with the tool picked from its menu
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.new_draft.as_ref().map(|d| d.tool.as_str()), Some("codex"));
        assert_eq!(started(&mut app), ("/h".to_string(), "随手问".to_string()));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(rail_on(&app).as_deref(), Some("全部"));
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(started(&mut app), ("/h".to_string(), "随手问".to_string()));
        // On a session of a project: another one like it, as always
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(started(&mut app), ("/p/fun".to_string(), "fun".to_string()));
        // Its row on the rail: d is refused, and so is deleting what it's in; r still renames
        keys(&mut app, "hj");
        assert_eq!(rail_on(&app).as_deref(), Some("随手问"));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm.is_none());
        assert_eq!(app.msg.take().map(|m| m.0).as_deref(), Some("无法删除随手问所在的分组"));
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::GroupName(g)) if g == "随手问"));
    }

    /// The fixture (fun[问 未读] work[跑 闲 错]) on a screen wide enough for the rail: it goes by the screen last drawn
    fn wide() -> App {
        let mut app = fixture();
        reader_rows(&mut app, 150, 40);
        app
    }

    /// The rail row the cursor is on (None = the cursor is in the list)
    fn rail_on(app: &App) -> Option<String> {
        app.rail.map(|r| match r {
            RailAt::Trash => "回收站".to_string(),
            RailAt::Here => app.group.clone().unwrap_or_else(|| "全部".into()),
        })
    }

    #[test]
    fn h_goes_out_to_the_rail_and_l_comes_back_to_the_row_left_there() {
        let mut app = wide();
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((rail_on(&app).as_deref(), app.group.as_deref()), (Some("全部"), None));
        // No row of the list is highlighted while the cursor is on the rail, and beside all groups nothing is drawn: the
        // whole screen is the ground but the rail's row, which stands out from it as the list's highlighted row does
        // (the terminal's own background, a bar in the accent color before it)
        let mut t = Terminal::new(TestBackend::new(150, 40)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        assert!(app.disp.selected().is_none());
        let lit = |x: u16, y: u16| buf[(x, y)].bg == Color::Reset;
        let (rail, list, pre) = (app.rail_area, app.list_area, app.preview_area);
        assert!(lit(rail.x + 4, rail.y) && !lit(rail.x + 4, rail.y + 2));
        assert_eq!((buf[(rail.x, rail.y)].symbol(), buf[(rail.x, rail.y)].fg), ("▌", theme::pal().accent));
        assert!(!(list.y..list.y + list.height).any(|y| lit(list.x + 6, y)));
        assert!(!(0..40).any(|y| lit(pre.x + 4, y)), "nothing beside all groups");
        assert_eq!((buf[(0, 0)].bg, buf[(149, 39)].bg), (theme::pal().back, theme::pal().back));
        // j: the next place, and the list shows it at once; beside it, the group
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(rail_on(&app).as_deref(), Some("fun"));
        assert_eq!(order(&app), ["问", "未读"]);
        let rows = reader_rows(&mut app, 150, 40);
        assert!(app.disp.selected().is_none() && rows.iter().any(|r| r.contains("▸ fun")), "{rows:#?}");
        press(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["跑", "闲", "错"]);
        // l: into it, on its first row the first time
        press(&mut app, KeyCode::Char('l'));
        assert_eq!((rail_on(&app), at(&app)), (None, "跑".to_string()));
        // Left on another row: coming back lands there
        keys(&mut app, "jhkj");
        press(&mut app, KeyCode::Enter);
        assert_eq!((rail_on(&app), at(&app)), (None, "闲".to_string()));
        // And all groups is back on the row it was left on
        keys(&mut app, "hg");
        press(&mut app, KeyCode::Right);
        assert_eq!((app.group.as_deref(), rail_on(&app), at(&app)), (None, None, "未读".to_string()));
        // The window got too narrow to keep a rail: the cursor is still on it, the rail now laid over the list; h puts it away,
        // the list on what it showed (this rail wasn't called out from anywhere to go back to)
        keys(&mut app, "hj");
        assert_eq!(rail_on(&app).as_deref(), Some("fun"));
        let rows = reader_rows(&mut app, 120, 30);
        assert!(app.on_rail() && rows.iter().any(|r| r.contains("▌   fun")) && rows.iter().any(|r| r.contains("回收站")), "{rows:#?}");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.on_rail(), app.group.as_deref()), (false, Some("fun")));
        assert!(!reader_rows(&mut app, 120, 30).iter().any(|r| r.contains("回收站")));
    }

    #[test]
    fn the_rail_stops_on_an_empty_group_and_deleting_the_group_viewed_goes_back_to_all() {
        let mut v = sessions();
        v.push(crate::data::blank_row("空", 5));
        let mut app = App::new(String::new(), v).with_groups(deck_groups);
        reader_rows(&mut app, 150, 40);
        keys(&mut app, "hGk");
        assert_eq!(rail_on(&app).as_deref(), Some("空"));
        // Still on it after a refresh; it lists nothing, so there is no row to go to
        app.set_sessions(app.all.clone());
        press(&mut app, KeyCode::Enter);
        assert_eq!((rail_on(&app).as_deref(), order(&app).len()), (Some("空"), 0));
        // d y on the group viewed: its row goes, and the cursor is on all groups
        keys(&mut app, "gj");
        assert_eq!(rail_on(&app).as_deref(), Some("fun"));
        keys(&mut app, "dy");
        assert_eq!((rail_on(&app).as_deref(), app.group.as_deref()), (Some("全部"), None));
    }

    #[test]
    fn h_on_a_child_session_goes_to_its_parent_before_it_leaves_the_list() {
        let mut v = sessions();
        v.push(Session { up: Up::Under("闲".into(), "闲".into()), ..mk("子", "work", State::Idle, 10) });
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 150, 40);
        select(&mut app, "子");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((rail_on(&app), at(&app)), (None, "闲".to_string()));
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(rail_on(&app).as_deref(), Some("全部"));
    }

    #[test]
    fn the_rail_stops_on_places_only_and_the_trash_opens_its_page() {
        let mut app = wide();
        pin(&mut app, "跑");
        press(&mut app, KeyCode::Char('h'));
        let mut seen = vec![];
        for _ in 0..5 {
            seen.push(rail_on(&app).unwrap());
            press(&mut app, KeyCode::Char('j'));
        }
        // Not on the pinned block's row or the blank lines; at the last one it stays
        assert_eq!(seen, ["全部", "fun", "work", "回收站", "回收站"]);
        // On the Trash the list still shows the place before it
        assert_eq!(app.group.as_deref(), Some("work"));
        press(&mut app, KeyCode::Enter);
        assert!(!app.overlays.is_empty());
    }

    #[test]
    fn on_a_groups_rail_row_its_keys_work_and_a_sessions_keys_do_nothing() {
        let mut app = wide().with_groups(deck_groups);
        select(&mut app, "跑");
        keys(&mut app, "hj");
        assert_eq!(rail_on(&app).as_deref(), Some("fun"));
        // Keys that act on a session: no session is under the cursor
        for c in ['s', 'z', 'x', 'p', 'm', 'D', 'R', 'W', 'c', 'v'] {
            press(&mut app, KeyCode::Char(c));
            assert!(app.prompt.is_none() && app.confirm.is_none() && app.reader.is_none() && app.pending.is_none(), "{c}");
            assert_eq!((rail_on(&app).as_deref(), app.picking()), (Some("fun"), false), "{c}");
        }
        assert!(app.pins.rank("问").is_none());
        // The group's own: its note, deleting it, moving it
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::Note { key, .. }) if key == "group:fun"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Group(g)) if g.group == "fun"));
        press(&mut app, KeyCode::Esc);
        shift(&mut app, KeyCode::Down);
        assert_eq!(app.take_job().unwrap(), Job::GroupOrder { group: "fun".into(), by: "work".into(), after: true });
        // All groups is no group: nothing to delete
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.confirm.is_none() && rail_on(&app).as_deref() == Some("全部"));
    }

    #[test]
    fn n_and_r_on_a_groups_rail_row_are_the_groups_and_a_session_come_back_from_gets_the_cursor() {
        fn hints() -> crate::data::GroupHints {
            crate::data::GroupHints { groups: vec![("work".into(), "/w/work".into()), ("空".into(), String::new())] }
        }
        let mut v = sessions();
        v.push(crate::data::blank_row("空", 5));
        let mut app = App::new(String::new(), v).with_group_hints(hints).with_new_group_dir(|| "/p".into());
        reader_rows(&mut app, 150, 40);
        // r on a group's row edits the group, as on its heading
        keys(&mut app, "hjj");
        assert_eq!(rail_on(&app).as_deref(), Some("work"));
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::GroupName(g)) if g == "work"));
        press(&mut app, KeyCode::Esc);
        // N: a new session in this group
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.new_draft.take().unwrap().group().0, "work");
        // On an empty group too: into it, starting from the directory new groups get
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(rail_on(&app).as_deref(), Some("空"));
        press(&mut app, KeyCode::Char('N'));
        let d = app.new_draft.take().unwrap();
        assert_eq!((d.text.as_str(), d.group()), ("/p/", ("空".into(), false)));
        // Back from a session gone into from here: the cursor is on it, in the list
        app.came_back("跑", "");
        assert!(!app.on_rail());
    }

    #[test]
    fn esc_in_a_group_steps_out_to_the_rail_and_keys_for_the_list_take_the_cursor_back() {
        let mut app = wide();
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.group.as_deref(), rail_on(&app)), (Some("work"), None));
        press(&mut app, KeyCode::Esc);
        assert_eq!((rail_on(&app).as_deref(), app.quit), (Some("work"), false));
        // On the rail Esc is what it is on all groups: it doesn't leave the place
        press(&mut app, KeyCode::Esc);
        assert_eq!((rail_on(&app).as_deref(), said(&app)), (Some("work"), "q q 退出"));
        // A row's number puts the cursor on that row
        press(&mut app, KeyCode::Char('2'));
        assert_eq!((rail_on(&app), at(&app)), (None, "闲".to_string()));
        // i goes into the one waiting, from the rail too
        keys(&mut app, "hk");
        assert_eq!(rail_on(&app).as_deref(), Some("fun"));
        press(&mut app, KeyCode::Char('i'));
        assert_eq!((rail_on(&app), app.attach.as_deref()), (None, Some("问")));
    }

    #[test]
    fn a_click_on_the_rail_goes_there_and_a_second_one_goes_in() {
        let mut app = wide();
        let rows = app.rail_rows();
        let row = |name: &str| app.rail_area.y + rows.iter().position(|r| r.name == name).unwrap() as u16;
        let (work, gap, x) = (row("work"), row("全部") + 1, app.rail_area.x + 6);
        let click = |app: &mut App, row: u16| {
            app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row, modifiers: KeyModifiers::NONE })
        };
        click(&mut app, gap); // a blank line: nowhere to go
        assert_eq!(rail_on(&app), None);
        click(&mut app, work);
        assert_eq!(rail_on(&app).as_deref(), Some("work"));
        assert_eq!(order(&app), ["跑", "闲", "错"]);
        click(&mut app, work);
        assert_eq!((rail_on(&app), at(&app)), (None, "跑".to_string()));
    }

    fn frame(app: &mut App, w: u16) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(w, 40)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        t.backend().buffer().clone()
    }

    /// The top row over the list's column in a frame this wide: its text, and the part of it that is underlined (the lit cell)
    fn top_row(app: &mut App, w: u16) -> (String, String) {
        let buf = frame(app, w);
        let (mut row, mut lit, mut x) = (String::new(), String::new(), app.list_area.x);
        while x < app.list_area.x + app.list_area.width {
            let c = &buf[(x, 1)];
            row.push_str(c.symbol());
            if c.modifier.contains(Modifier::UNDERLINED) {
                lit.push_str(c.symbol());
            }
            x += c.cell_width().max(1);
        }
        (row, lit)
    }

    #[test]
    fn beside_the_rail_the_top_row_is_the_middle_columns_own() {
        let mut app = wide();
        // What the list lists, then a rule: no tag for the group and no count per state, the rail has both
        let (row, lit) = top_row(&mut app, 150);
        assert!(row.starts_with(" 未停止 5   近 7 天 5 ─") && row.ends_with('─'), "{row}");
        // Only the lit one's name is underlined, not its count
        assert!(!row.contains("全部分组") && !row.contains("运行中") && lit == "未停止", "{row} / {lit}");
        // The rule ends where the list does, and the column beside it starts level with the row: no frame, a block on the
        // terminal's own background from the screen's top row down, its title two cells in
        let buf = frame(&mut app, 150);
        let (list, pre) = (app.list_area, app.preview_area);
        assert_eq!((buf[(list.x + list.width, 1)].symbol(), pre.y, buf[(pre.x, 1)].symbol()), (" ", 1, " "));
        assert_eq!((buf[(pre.x, 0)].bg, buf[(pre.x, 1)].bg, buf[(pre.x - 1, 1)].bg), (Color::Reset, Color::Reset, theme::pal().back));
        assert_eq!(buf[(pre.x + 2, 1)].symbol(), "◆", "the highlighted session's mark leads its title");
        // No tag to click: the rail is the group list
        assert_eq!(app.chip_area.width, 0);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: list.x + 2,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!app.on_rail() && !app.modal_open());
        // 0: a third cell, the lit one
        press(&mut app, KeyCode::Char('0'));
        let (row, lit) = top_row(&mut app, 150);
        assert!(row.starts_with(" 未停止 5   近 7 天 5   所有 5 ─") && lit == "所有", "{row} / {lit}");
        press(&mut app, KeyCode::Char('0'));
        // A state picked in F, a search and a number being typed are written where the cells are
        only(&mut app, '1');
        assert!(top_row(&mut app, 150).0.starts_with(" 仅 ● 运行中 1 ─"));
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "/跑");
        assert!(top_row(&mut app, 150).0.starts_with(" / 跑▏ ─"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('2'));
        assert!(top_row(&mut app, 150).0.starts_with(" 未停止 5   近 7 天 5  #2 ─"));
        // Broken plugins end the row; a column too narrow for it all shortens them first
        let p =
            |plugin: &str| aca_core::plugin::health::Problem { plugin: plugin.into(), what: "钩子".into(), detail: "1".into(), at: 0 };
        app.plugin_alerts = vec![p("aca.bark"), p("me.todo")];
        app.digits.clear();
        press(&mut app, KeyCode::Char('0'));
        let row = top_row(&mut app, 150).0;
        assert!(row.contains("所有 5 ─") && row.ends_with("─ ⚠ 2 插件出错(P) "), "{row}");
        let row = top_row(&mut app, 130).0;
        assert!(row.contains("所有 5 ─") && row.ends_with("─ ⚠2 "), "{row}");
        // More than one workspace in use: this one's name leads the row
        press(&mut app, KeyCode::Char('0'));
        select(&mut app, "问");
        keys(&mut app, "Ql");
        let row = top_row(&mut app, 150).0;
        assert!(!app.lone_space() && row.starts_with(&format!(" {}   未停止 4", app.spaces.name(app.space))), "{row}");
        // The preview below: the row runs the width of what's right of the rail
        press(&mut app, KeyCode::Char('L'));
        let row = top_row(&mut app, 150).0;
        assert!(app.preview_area.y > 1 && row.width() > 100 && row.ends_with("─ ⚠ 2 插件出错(P) "), "{row}");
    }

    #[test]
    fn inside_a_group_what_is_listed_is_at_the_right_end_of_the_top_row() {
        let mut app = wide();
        app.set_group(Some("work".into()));
        // The names of the group's pages at the left (the one showing lit), the scopes at the end, a cell short of the edge
        // as the rows' times are when the titles fill the column
        let (row, lit) = top_row(&mut app, 150);
        assert!(row.starts_with(" 任务 0   会话 3   笔记 0 ─") && row.ends_with("─ 未停止 3   近 7 天 3 "), "{row}");
        assert_eq!(lit, "会话未停止");
        // The same on the group's rail row, the cursor not yet in it
        keys(&mut app, "h");
        assert!(top_row(&mut app, 150).0.ends_with("─ 未停止 3   近 7 天 3 "));
        press(&mut app, KeyCode::Char('l'));
        // 0, w and F do there what they do on all groups
        press(&mut app, KeyCode::Char('0'));
        let (row, lit) = top_row(&mut app, 150);
        // (the three pages' names leave room for the lit scope only, once there are three scopes)
        assert!(row.ends_with("─ 所有 3 ") && lit == "会话所有", "{row} / {lit}");
        press(&mut app, KeyCode::Char('w'));
        assert_eq!((app.scope, top_row(&mut app, 150).1.as_str()), (Scope::Week, "会话近 7 天"));
        only(&mut app, '1');
        let row = top_row(&mut app, 150).0;
        assert!(row.starts_with(" 任务 0   会话 1   笔记 0 ─") && row.ends_with("─ 仅 ● 运行中 1 "), "{row}");
        // On its notes page, and on its tasks page, the row says nothing of which sessions are listed
        press(&mut app, KeyCode::Tab);
        let (row, lit) = top_row(&mut app, 150);
        assert!(row.starts_with(" 任务 0   会话 1   笔记 0 ─") && row.ends_with('─') && lit == "笔记", "{row} / {lit}");
        press(&mut app, KeyCode::Tab);
        let (row, lit) = top_row(&mut app, 150);
        assert!(row.ends_with('─') && lit == "任务", "{row} / {lit}");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Esc);
        // A search is typed at the left, as on all groups, and so is a row's number
        keys(&mut app, "/跑");
        let row = top_row(&mut app, 150).0;
        assert!(row.starts_with(" / 跑▏ ─") && row.ends_with('─'), "{row}");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('2'));
        let row = top_row(&mut app, 150).0;
        assert!(row.starts_with(" 任务 0   会话 3   笔记 0  #2 ─") && row.ends_with("─ 未停止 3   近 7 天 3 "), "{row}");
        // Broken plugins come last
        let p =
            |plugin: &str| aca_core::plugin::health::Problem { plugin: plugin.into(), what: "钩子".into(), detail: "1".into(), at: 0 };
        app.plugin_alerts = vec![p("aca.bark")];
        app.digits.clear();
        let row = top_row(&mut app, 213).0;
        assert!(row.ends_with("─ 未停止 3   近 7 天 3   ⚠ 1 插件出错(P) "), "{row}");
        // A column too narrow for it all shortens the plugins' cell first, then keeps only the lit scope; the pages stay
        let row = top_row(&mut app, 150).0;
        assert!(row.ends_with("─ 未停止 3   近 7 天 3   ⚠1 "), "{row}");
        press(&mut app, KeyCode::Char('0'));
        let row = top_row(&mut app, 130).0;
        assert!(row.starts_with(" 任务 0   会话 3   笔记 0 ─") && row.ends_with("─ 所有 3   ⚠1 "), "{row}");
    }

    #[test]
    fn zero_works_from_the_rail_and_the_cursor_stays_there() {
        let mut app = wide();
        keys(&mut app, "h0");
        assert_eq!((rail_on(&app).as_deref(), app.status), (Some("全部"), Some(Pick::Every)));
        // Esc first takes that off, and only then is what it is on the rail
        press(&mut app, KeyCode::Esc);
        assert_eq!((rail_on(&app).as_deref(), app.status, app.msg.is_none()), (Some("全部"), None, true));
        // On a group's row it lists every session of that group
        keys(&mut app, "jj0");
        assert_eq!((rail_on(&app).as_deref(), app.status), (Some("work"), Some(Pick::Every)));
        assert_eq!(order(&app), ["跑", "闲", "错"]);
        press(&mut app, KeyCode::Char('0'));
        assert_eq!((rail_on(&app).as_deref(), app.status), (Some("work"), None));
    }

    #[test]
    fn capital_w_goes_into_the_highlighted_sessions_group_and_back_and_the_rails_bar_follows() {
        let mut app = wide();
        select(&mut app, "闲");
        // The rail row the bar is on
        let bar_on = |app: &App| {
            let rows = app.rail_rows();
            app.rail_at(&rows).map(|p| rows[p].name.clone())
        };
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), rail_on(&app), at(&app)), (Some("work"), None, "闲".to_string()));
        assert_eq!(bar_on(&app).as_deref(), Some("work"));
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), rail_on(&app), at(&app)), (None, None, "闲".to_string()));
        assert_eq!(bar_on(&app).as_deref(), Some("全部"));
        // Tab opens nothing: not in all groups, not in a group, not on the rail
        for before in ["", "W", "h"] {
            keys(&mut app, before);
            let was = (app.group.clone(), app.rail, at(&app));
            press(&mut app, KeyCode::Tab);
            assert!(!app.modal_open() && (app.group.clone(), app.rail, at(&app)) == was, "{before}");
        }
    }

    /// The bottom bar of a 150×40 frame
    fn bar(app: &mut App) -> String {
        reader_rows(app, 150, 40).into_iter().rev().find(|r| !r.trim().is_empty()).unwrap_or_default()
    }

    #[test]
    fn a_goes_to_all_groups_with_the_letters_showing_and_a_again_goes_back() {
        let mut app = wide();
        select(&mut app, "未读");
        app.set_group(Some("work".into()));
        select(&mut app, "错");
        assert!(bar(&mut app).contains(&caps("a 全部")));
        // From a group's third row: all groups, on the row it was left on, the letters showing
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.group.as_deref(), rail_on(&app), at(&app), app.jump.as_deref()), (None, None, "未读".to_string(), Some("")));
        let b = bar(&mut app);
        assert!(b.contains(&caps("字母 打开 • ⇧字母 进分组 • a 上一处 • 空格 取消")) && b.ends_with(&caps("? 快捷键")), "{b}");
        // a again: back in the group, on its third row, the letters gone
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(
            (app.group.as_deref(), at(&app), app.jump.is_none(), app.hop_back.is_none()),
            (Some("work"), "错".to_string(), true, true)
        );
        // a then a letter goes into that session; back on the list, a still goes back to the group
        press(&mut app, KeyCode::Char('a'));
        reader_rows(&mut app, 150, 40);
        press(&mut app, KeyCode::Char('d')); // d 问, f 未读, j 跑 … (a group's header has no letter here)
        assert_eq!((app.attach.take().as_deref(), app.jump.is_none(), app.group.as_deref()), (Some("问"), true, None));
        // (the interface in a terminal of its own: the same list is back once the session is left, and reread)
        app.came_back("问", "");
        app.set_sessions(sessions());
        assert!(app.hop_back.is_some() && bar(&mut app).contains(&caps("a 上一处")));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.group.as_deref(), at(&app)), (Some("work"), "错".to_string()));
        // Space puts the letters away and stays in all groups; a from there goes back
        keys(&mut app, "a ");
        assert_eq!((app.group.as_deref(), app.jump.is_none(), app.hop_back.is_some()), (None, true, true));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.group.as_deref(), Some("work"));
        // Esc does what Space does there
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.group.as_deref(), app.jump.is_none(), app.hop_back.is_some(), app.quit), (None, true, true, false));
        // All groups left another way: nowhere to go back to any more, and a from the new place remembers that one
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), app.hop_back.is_none()), (Some("fun"), true));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.hop_back, Some((Some("fun".to_string()), None)));
        keys(&mut app, " h");
        assert_eq!((rail_on(&app).as_deref(), app.hop_back.is_none()), (Some("全部"), true));
    }

    #[test]
    fn a_works_from_the_rail_and_comes_back_onto_it() {
        let mut app = wide();
        // On a group's rail row
        keys(&mut app, "hjj");
        assert_eq!(rail_on(&app).as_deref(), Some("work"));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.group.as_deref(), rail_on(&app), app.jump.is_some()), (None, None, true));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.group.as_deref(), rail_on(&app).as_deref(), app.jump.is_none()), (Some("work"), Some("work"), true));
        // On the row of all groups the list is already that: the cursor goes into it with the letters, and back out
        keys(&mut app, "kk");
        assert_eq!(rail_on(&app).as_deref(), Some("全部"));
        assert!(bar(&mut app).contains(&caps("a 跳转")));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((rail_on(&app), app.jump.is_some()), (None, true));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((rail_on(&app).as_deref(), app.jump.is_none()), (Some("全部"), true));
        // In the list of all groups with nowhere to go back to, a only shows the letters, as Space does, and isn't in the bar
        press(&mut app, KeyCode::Char('l'));
        assert!(!bar(&mut app).contains("a "));
        press(&mut app, KeyCode::Char('a'));
        let b = bar(&mut app);
        assert!(
            app.jump.is_some() && app.hop_back.is_none() && b.contains(&caps("字母 打开 • ⇧字母 进分组 • 空格 取消")) && !b.contains("a "),
            "{b}"
        );
        // a there has nowhere to go: the letters go away
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.jump.is_none(), app.group.as_deref(), rail_on(&app)), (true, None, None));
    }

    /// With the letters showing, ⇧ and a group's letter goes into that group, from wherever the list is
    #[test]
    fn a_capital_letter_in_jump_mode_goes_into_the_group_that_has_it() {
        let mut app = wide(); // [fun] 问 未读 [work] 跑 闲 错
        select(&mut app, "未读");
        app.set_group(Some("work".into()));
        select(&mut app, "错");
        app.set_group(None);
        assert_eq!(app.group_keys(), [('F', "fun".to_string()), ('W', "work".to_string())]);
        // Space: each group's own letter on its rail row, the sessions' in the list (one each, a header has none)
        press(&mut app, KeyCode::Char(' '));
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("F fun")) && rows.iter().any(|r| r.contains("W work")), "{rows:#?}");
        shows(&rows, &[&caps("字母 打开 • ⇧字母 进分组 • 空格 取消")]);
        assert_eq!(app.jump_stops().len(), 5);
        assert!(!rows.iter().any(|r| r.contains("F  fun") || r.contains("W  work")), "{rows:#?}");
        // ⇧W: in work, the cursor in its list on the row it was left on; it wasn't taken as the list's own W
        press(&mut app, KeyCode::Char('W'));
        let now = |app: &App| (app.group.clone().unwrap_or_default(), rail_on(app), at(app), app.jump.is_none(), app.hop_back.is_none());
        assert_eq!(now(&app), ("work".to_string(), None, "错".to_string(), true, true));
        // From inside a group (fun was never left on a row: its first), and after a
        keys(&mut app, " F");
        assert_eq!(now(&app), ("fun".to_string(), None, "问".to_string(), true, true));
        keys(&mut app, "j");
        keys(&mut app, "aW");
        assert_eq!(now(&app), ("work".to_string(), None, "错".to_string(), true, true));
        // The letter of the group the list is in: the letters go, nothing else
        keys(&mut app, " W");
        assert_eq!(now(&app), ("work".to_string(), None, "错".to_string(), true, true));
        // From the rail the cursor goes into the list as well, on the row that group was left on
        keys(&mut app, "h F");
        assert_eq!(now(&app), ("fun".to_string(), None, "未读".to_string(), true, true));
        // A capital no group has puts the letters away and isn't taken as the key it is elsewhere (J: the next group, V: a
        // range of rows, N: a new session)
        app.set_group(None);
        select(&mut app, "问");
        for c in ['J', 'V', 'N', 'Z'] {
            press(&mut app, KeyCode::Char(' '));
            press(&mut app, KeyCode::Char(c));
            assert!(app.jump.is_none() && !app.picking() && !app.modal_open() && app.reader.is_none() && at(&app) == "问", "{c}");
        }
        // On a group's notes page Space shows the groups' letters alone, and a group comes back on the page it was left on
        app.set_group(Some("work".into()));
        press(&mut app, KeyCode::Tab);
        assert!(app.in_notes());
        press(&mut app, KeyCode::Char(' '));
        let b = bar(&mut app);
        assert!(app.jump.is_some() && b.contains(&caps("⇧字母 进分组 • 空格 取消")) && !b.contains(&caps("字母 打开")), "{b}");
        press(&mut app, KeyCode::Char('F'));
        assert_eq!((app.group.as_deref(), app.on_notes()), (Some("fun"), false));
        keys(&mut app, " W");
        assert!(app.group.as_deref() == Some("work") && app.in_notes());
    }

    /// What the list lists doesn't change a group's letter: only the groups there are, and their order
    #[test]
    fn a_groups_letter_stays_the_same_whatever_the_list_lists() {
        let mut v = sessions();
        v.push(crate::data::blank_row("lab", 2));
        v.push(mk("旧", "fast", State::Dead, 9 * 86400));
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 150, 40);
        let keys_now = app.group_keys();
        // fun and fast both start with f: the first in the rail's order has it, the other its next free letter; an empty
        // group and one with only stopped sessions have theirs too
        let of = |g: &str| keys_now.iter().find(|(_, x)| x == g).map(|(c, _)| *c);
        assert_eq!((of("fun"), of("work"), of("lab")), (Some('F'), Some('W'), Some('L')));
        assert!(of("fast").is_some_and(|c| c != 'F'), "{keys_now:?}");
        // The other scope, a state picked, a search, inside a group, on the rail: the same letters
        keys(&mut app, "w");
        assert_eq!(app.group_keys(), keys_now);
        keys(&mut app, "F1");
        assert_eq!(app.group_keys(), keys_now);
        press(&mut app, KeyCode::Esc);
        keys(&mut app, "/问");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.group_keys(), keys_now);
        press(&mut app, KeyCode::Esc);
        app.set_group(Some("work".into()));
        assert_eq!(app.group_keys(), keys_now);
        keys(&mut app, "h");
        assert_eq!(app.group_keys(), keys_now);
        // More groups than letters: the first 26 have one each, all different
        let many: Vec<String> = (0..30).map(|i| format!("g{i:02}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let got = group_letters(&many);
        let given: std::collections::HashSet<char> = got.iter().flatten().copied().collect();
        assert_eq!((got.iter().flatten().count(), given.len(), got[26..].iter().all(Option::is_none)), (26, 26, true));
        // The row of plain tmux sessions has a letter as well, from what the row is called
        let plain = group_letters(&["fun", "-"]);
        assert!(plain[0] == Some('F') && plain[1].is_some_and(|c| c != 'F'), "{plain:?}");
    }

    /// A screen with no rail: the groups' letters lead their headers in all groups, and work unseen inside a group; on a
    /// rail laid over the list they show on it, and going into a group puts it away
    #[test]
    fn with_no_rail_on_screen_a_groups_letter_is_on_its_header_or_on_the_rail_laid_over() {
        let mut v = sessions();
        v.push(crate::data::blank_row("lab", 2));
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 120, 30);
        press(&mut app, KeyCode::Char(' '));
        let rows = reader_rows(&mut app, 120, 30);
        assert!(rows.iter().any(|r| r.contains("F  fun")) && rows.iter().any(|r| r.contains("W  work")), "{rows:#?}");
        assert_eq!(app.jump_stops().len(), 5);
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), app.on_rail(), app.jump.is_none()), (Some("work"), false, true));
        // Inside a group nothing has the letters written on it; they work all the same
        press(&mut app, KeyCode::Char(' '));
        let rows = reader_rows(&mut app, 120, 30);
        assert!(!rows.iter().any(|r| r.contains("F  fun") || r.contains("F fun")), "{rows:#?}");
        press(&mut app, KeyCode::Char('F'));
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), false));
        // h lays the rail over the list; Space there shows the letters on it, and ⇧W goes into work and puts it away
        keys(&mut app, "h ");
        let rows = reader_rows(&mut app, 120, 30);
        assert!(app.on_rail() && rows.iter().any(|r| r.contains("W work")) && rows.iter().any(|r| r.contains("L lab")), "{rows:#?}");
        press(&mut app, KeyCode::Char('W'));
        assert_eq!((app.group.as_deref(), app.on_rail(), app.jump.is_none()), (Some("work"), false, true));
        // A group with no session at all has no list to be in without a rail: the rail is laid over, the cursor on its row
        keys(&mut app, " L");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("lab"), true));
        app.set_sessions({
            let mut v = sessions();
            v.push(crate::data::blank_row("lab", 2));
            v
        });
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("lab"), true));
        // On a phone, in all groups: the letter before the header's mark. The bottom bar has just said where the list went:
        // with the letters showing it says what they do instead
        press(&mut app, KeyCode::Esc);
        app.set_group(None);
        reader_rows(&mut app, 46, 30);
        assert!(app.msg.is_some());
        press(&mut app, KeyCode::Char(' '));
        let rows = reader_rows(&mut app, 46, 30);
        assert!(rows.iter().any(|r| r.starts_with("F ▾ fun")) && rows.iter().any(|r| r.starts_with("W ▾ work")), "{rows:#?}");
        shows(&rows, &[&caps("字母 打开 • ⇧字母 进分组")]);
        assert!(rows.iter().all(|r| r.width() <= 46), "{rows:#?}");
        // Inside a group there the bar alone says a group's letter can be pressed
        keys(&mut app, "W ");
        shows(&reader_rows(&mut app, 46, 30), &[&caps("字母 打开 • ⇧字母 进分组")]);
    }

    /// A group with no session at all has no row to put the cursor on: its letter leaves the cursor on its rail row, where
    /// N starts a session in it. One whose sessions just aren't listed now is gone into like any other
    #[test]
    fn a_capital_letter_goes_to_a_group_that_lists_nothing_too() {
        let mut v = sessions();
        v.push(crate::data::blank_row("lab", 2));
        v.push(mk("旧", "old", State::Dead, 10 * 86400));
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 150, 40);
        keys(&mut app, " L");
        assert_eq!((app.group.as_deref(), rail_on(&app).as_deref(), app.jump.is_none()), (Some("lab"), Some("lab"), true));
        assert!(bar(&mut app).contains(&caps("N 在组里新建")), "{}", bar(&mut app));
        keys(&mut app, " O");
        assert_eq!((app.group.as_deref(), rail_on(&app), app.view.is_empty()), (Some("old"), None, true));
        // Multi-selecting, or picking a session to refer to: the letters go where they go, and neither ends
        let mut app = wide();
        select(&mut app, "问");
        keys(&mut app, "vaW");
        assert!(app.picking() && app.group.as_deref() == Some("work"));
        // Nothing listed at all (every session stopped long ago): Space still shows the groups' letters
        let mut app = App::new(String::new(), vec![mk("旧", "old", State::Dead, 10 * 86400)]);
        reader_rows(&mut app, 150, 40);
        assert!(app.view.is_empty());
        press(&mut app, KeyCode::Char(' '));
        let b = bar(&mut app);
        assert!(app.jump.is_some() && b.contains(&caps("⇧字母 进分组 • 空格 取消")) && !b.contains(&caps("字母 打开")), "{b}");
        press(&mut app, KeyCode::Char('O'));
        assert_eq!((app.group.as_deref(), app.jump.is_none()), (Some("old"), true));
    }

    #[test]
    fn a_after_its_group_is_gone_shows_the_letters() {
        let mut app = wide();
        app.set_group(Some("work".into()));
        keys(&mut app, "a ");
        let mut v = sessions();
        v.retain(|s| s.group == "fun");
        app.set_sessions(v);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!((app.group.as_deref(), app.jump.is_some(), app.hop_back.is_none()), (None, true, true));
    }

    #[test]
    fn the_jump_letters_go_to_the_rows_on_screen_only() {
        // Sixty sessions in one group, two of them with a second line (what they ask): more than a screen of 40 lines holds
        let mut v: Vec<Session> = (0..60).map(|i| mk(&format!("s{i:02}"), "work", State::Idle, 10)).collect();
        for i in [5, 33] {
            v[i].state = State::Ask;
            v[i].question = "Allow edit?".into();
        }
        let mut app = App::new(String::new(), v);
        // The letters showing in a frame, top to bottom: what's written where a row's number is
        let letters = |app: &mut App| -> Vec<String> {
            let buf = frame(app, 150);
            let list = app.list_area;
            (list.y..list.y + list.height)
                .map(|y| (list.x..list.x + 3).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim().to_string())
                .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase()))
                .collect()
        };
        reader_rows(&mut app, 150, 40);
        press(&mut app, KeyCode::Char(' '));
        let shown = letters(&mut app);
        let stops = app.jump_stops();
        // As many letters as stops on screen, far fewer than the list has, and so more of them a single letter
        assert_eq!(shown, jump_hints(stops.len()));
        assert!(stops.len() < 40 && stops.len() > 20 && app.stops().len() == 61, "{}", stops.len());
        let one = |n: usize| jump_hints(n).iter().filter(|h| h.len() == 1).count();
        assert!(one(stops.len()) > one(61));
        // The last letter on screen is the last row on screen, not the list's last
        let last = shown.last().unwrap().clone();
        let want = match *stops.last().unwrap() {
            Jump::Row(p) => app.all[app.view[p]].title.clone(),
            Jump::Head(_) => unreachable!(),
        };
        keys(&mut app, &last);
        assert_eq!(app.attach.take(), Some(want.clone()));
        assert!(want.as_str() < "s40", "{want}");
        // Scrolled to the end: the letters are the rows there now
        press(&mut app, KeyCode::Char('G'));
        reader_rows(&mut app, 150, 40);
        press(&mut app, KeyCode::Char(' '));
        let shown = letters(&mut app);
        assert_eq!(shown, jump_hints(app.jump_stops().len()));
        keys(&mut app, &shown[shown.len() - 1].clone());
        assert_eq!(app.attach.take().as_deref(), Some("s59"));
        // A list another frame drew (it changed, and the key came before it was drawn): every stop has a letter
        app.set_group(Some("work".into()));
        assert_eq!(app.jump_stops().len(), 60);
    }

    #[test]
    fn long_names_are_cut_in_the_middle_so_their_ends_still_tell_them_apart() {
        assert_eq!(clip_mid("probe", 8), "probe   ");
        assert_eq!(clip_mid("agent-factory-runtime", 20), "agent-fact…y-runtime");
        assert_eq!(clip_mid("agent-factory-runtime-v3", 20), "agent-fact…untime-v3");
        // Chinese takes two cells: never over the width, padded when a wide one doesn't fit the last cell
        assert_eq!(clip_mid("四方小仓的周报和月报", 10), "四方…月报 ");
        assert_eq!(clip_mid("四方小仓的周报和月报", 10).width(), 10);
    }

    #[test]
    fn a_wide_screen_has_the_rail_and_a_narrower_one_is_as_before() {
        let mut app = fixture();
        let wide = reader_rows(&mut app, 150, 40);
        // The screen's left margin, the rail's columns, then its line (all one cell wide here but the Chinese names)
        let rail: Vec<String> = wide.iter().map(|r| r.split_inclusive('│').next().unwrap_or_default().to_string()).collect();
        assert!(rail.iter().any(|r| r.contains("▌   全部") && r.ends_with("   5│")), "{rail:#?}");
        assert!(rail.iter().any(|r| r.contains("fun") && r.contains("◆")), "{rail:#?}");
        assert!(rail.iter().any(|r| r.contains("work") && r.contains("●")), "{rail:#?}");
        assert!(rail.iter().any(|r| r.contains("回收站")), "{rail:#?}");
        // The list and its top bar start right of the rail
        assert!(wide[1].contains("未停止 5"), "{}", wide[1]);
        assert!(!wide[1].starts_with("    全部分组"), "{}", wide[1]);
        // Under the width the rail needs, nothing of it
        let narrow = reader_rows(&mut app, 120, 30);
        assert!(!narrow.iter().any(|r| r.contains("回收站")), "{narrow:#?}");
        assert!(narrow[1].starts_with("    全部分组") && narrow[3].starts_with("      fun"), "{narrow:#?}");
    }
}
