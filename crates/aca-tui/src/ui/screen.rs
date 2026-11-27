//! Drawing the screen: the top bar, where the list and preview go, the menus over them, and the frames dialogs and side
//! panels sit in.

use super::*;

/// Desktop (wide and tall enough): margins all round (3 columns left and right, 1 line top and bottom, a blank line below the top bar and
/// above the bottom bar); the list shows numbers and a blank line between groups
fn roomy(area: Rect) -> bool {
    area.width >= 100 && area.height >= 30
}

/// The tag at the left of the top bar: the workspace's name ("sessions" when only 1 is in use); viewing a single group, that group instead
fn chip_text(app: &App) -> String {
    let label = app.group.as_ref().map_or_else(|| app.space_label(), |g| app.shown(g).into_owned());
    format!(" {} ", clip(&label, 14).trim_end())
}

/// What the list lists, as the cells of the top row: the two scopes with their counts, the current one lit; while `0` lists
/// every session a third cell 「所有 N」 (every N) is the lit one. A state picked in F stands in for them all (it lists that
/// state whatever the scope), and so does a search. `all` false keeps only the lit cell (narrow)
fn scope_cells(app: &App, all: bool) -> Vec<Span<'static>> {
    let on = theme::accent().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    if !app.filter.is_empty() || app.filtering {
        let cursor = if app.filtering { "▏" } else { "" };
        return vec![
            Span::styled("/ ", theme::accent()),
            Span::styled(format!("{}{cursor}", app.filter), Style::new().add_modifier(Modifier::BOLD)),
        ];
    }
    let every = app.status == Some(Pick::Every);
    if let Some(p) = app.status.filter(|_| !every) {
        let (icons, name) = pick_look(p);
        let mut v = vec![Span::styled(t!("仅 ", "only "), on)];
        v.extend(icons.into_iter().map(|(ic, st)| Span::styled(ic, st)));
        v.push(Span::styled(format!(" {name} {}", app.status_count(p)), on));
        return v;
    }
    let mut cells: Vec<(&str, usize, bool)> =
        Scope::ALL.iter().map(|&s| (s.name(), app.scope_count(s), !every && s == app.scope)).collect();
    if every {
        cells.push((t!("所有", "Every"), app.status_count(Pick::Every), true));
    }
    let mut v = vec![];
    for (name, n, lit) in cells.into_iter().filter(|c| all || c.2) {
        if !v.is_empty() {
            v.push(Span::raw("   "));
        }
        v.extend(crate::kit::tab(name, n, lit));
    }
    v
}

/// The number being typed, while its digits may still go on: written in the top row, `gap` cells after what's before it
fn typed_number(app: &App, gap: usize) -> Vec<Span<'static>> {
    match !app.digits.is_empty() && app.digits_at.elapsed() <= DIGIT_GAP {
        true => vec![Span::raw(" ".repeat(gap)), Span::styled(format!("#{}", app.digits), theme::accent().add_modifier(Modifier::BOLD))],
        false => vec![],
    }
}

/// Broken plugins, at the right end of the top row: how many plugins, not how many places. Nothing when none is
fn plugin_cell(app: &App, long: bool) -> Vec<Span<'static>> {
    let mut broken: Vec<&str> = app.plugin_alerts.iter().map(|p| p.plugin.as_str()).collect();
    broken.dedup();
    if broken.is_empty() {
        return vec![];
    }
    let mut v = vec![
        Span::styled("⚠", theme::warn()),
        Span::styled(if long { format!(" {}", broken.len()) } else { broken.len().to_string() }, Style::new().add_modifier(Modifier::BOLD)),
    ];
    if long {
        v.push(Span::styled(t!(" 插件出错(P)", " plugin errors (P)"), dim()));
    }
    v
}

/// The top row beside a rail, over the middle column: what the list lists (`scope_cells`) and the number being typed, then a
/// faint rule to the column's right edge, broken plugins at its end. No tag for the group and no count per state: the rail
/// says both. The workspace's name leads the row when more than one is in use. Inside a group the left names its pages
/// (`page_cells`), so what's listed is written at the right end, and only on the sessions page; a search is typed at the
/// left there too. When narrow, shorten the plugins' cell first, then keep only the lit cell
pub(super) fn mid_bar(app: &App, w: usize) -> Line<'static> {
    // One cell in: the cells start under the list's two-digit numbers
    let lead = match app.lone_space() {
        true => vec![Span::raw(" ")],
        false => vec![Span::styled(format!(" {} ", clip(&app.spaces.name(app.space), 14).trim_end()), theme::chip()), Span::raw("  ")],
    };
    let searching = !app.filter.is_empty() || app.filtering;
    let at_end = app.group.is_some() && !searching;
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    // Inside a group the left names its pages; which sessions are listed is said only on the sessions page
    let pages = [lead.clone(), page_cells(app), typed_number(app, 2)].concat();
    let mut best = None;
    for (all, long) in [(true, true), (true, false), (false, false)] {
        let cells = if app.off_list() { vec![] } else { scope_cells(app, all) };
        let plugins = plugin_cell(app, long);
        let (left, right) = match at_end {
            true if plugins.is_empty() || cells.is_empty() => (pages.clone(), [cells, plugins].concat()),
            true => (pages.clone(), [cells, vec![Span::raw("   ")], plugins].concat()),
            false => ([lead.clone(), cells, typed_number(app, 2)].concat(), plugins),
        };
        if wid(&left) + 2 + wid(&right) < w || (!all && !long) {
            best = Some((left, right));
            break;
        }
    }
    let (mut left, right) = best.unwrap_or_default();
    // Nothing written at the left: the rule starts under the list's numbers. What's at the right ends a cell short of the
    // edge, where the rows' times end when the titles fill the column
    let before = usize::from(wid(&left) > wid(&lead));
    let after = if right.is_empty() { 0 } else { wid(&right) + 2 };
    left.push(Span::raw(" ".repeat(before)));
    left.push(Span::styled("─".repeat(w.saturating_sub(wid(&left) + after)), theme::rule()));
    if !right.is_empty() {
        left.push(Span::raw(" "));
        left.extend(right);
        left.push(Span::raw(" "));
    }
    Line::from(left)
}

/// Top bar: left is what you're looking at (accent-colored tag + the two scopes with their counts, the picked state, or the search
/// term), right is the count per state. When narrow, shorten the counts first (◆1), then keep only the current scope
pub(super) fn top_bar(app: &App, w: usize) -> Line<'static> {
    let chip = vec![Span::styled(chip_text(app), theme::chip()), Span::raw("  ")];
    let extra = typed_number(app, 2);
    // State picked in F: those cells get the selection background (search ignores status, so no background then). Idle, ■, ☾
    // aren't counted normally; listed when picked, even at 0. All picked is no one state: no cell gets it
    let pick = app.status.filter(|&p| app.filter.is_empty() && p != Pick::Every);
    // Count what the list lists (the group, scope, state picked, search). The ■ cell counts everything F's ■ picks (exited, errored and not running, ☾ too), the ☾ cell only auto-stopped ones
    let count = |hit: &dyn Fn(&Session) -> bool| app.view.iter().filter(|&&i| counted(&app.all[i]) && hit(&app.all[i])).count();
    let counts = |long: bool| -> Vec<Span<'static>> {
        let mut v = vec![];
        for (st, name) in [
            (Some(State::Ask), t!("等待回应", "needs you")),
            (Some(State::Running), t!("运行中", "running")),
            (Some(State::Unread), t!("未读", "unread")),
            (Some(State::Error), t!("出错", "error")),
            (Some(State::Idle), t!("空闲", "idle")),
            (Some(State::Stopped), t!("已停止", "stopped")),
            (None, t!("自动停止", "auto-stopped")),
        ] {
            let (n, on, (ic, sty)) = match st {
                Some(State::Stopped) => (count(&|s| Pick::Off.covers(s.state)), pick == Some(Pick::Off), icon(State::Stopped)),
                Some(st) => (count(&|s| s.state == st), pick.is_some_and(|p| p.covers(st)), icon(st)),
                None => (count(&reaped), pick == Some(Pick::Reaped), (aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted))),
            };
            if !on && (n == 0 || !matches!(st, Some(State::Ask | State::Running | State::Unread | State::Error))) {
                continue;
            }
            if !v.is_empty() {
                v.push(Span::raw(if long { "    " } else { "  " }));
            }
            let mut cell = vec![
                Span::styled(ic, sty),
                Span::styled(if long { format!(" {n}") } else { n.to_string() }, Style::new().add_modifier(Modifier::BOLD)),
            ];
            if long {
                cell.push(Span::styled(format!(" {name}"), if on { Style::new() } else { dim() }));
            }
            if on {
                let bg = Style::new().bg(theme::pal().sel);
                cell.insert(0, Span::styled(" ", bg));
                cell.push(Span::styled(" ", bg));
                cell.iter_mut().for_each(|s| s.style = s.style.patch(bg));
            }
            v.extend(cell);
        }
        let broken = plugin_cell(app, long);
        if !broken.is_empty() {
            if !v.is_empty() {
                v.push(Span::raw(if long { "    " } else { "  " }));
            }
            v.extend(broken);
        }
        if v.is_empty() {
            v.push(Span::styled(t!("暂无待处理会话", "nothing to look at"), dim()));
        }
        v
    };
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    // Inside a group its pages are named after the tag; its notes page has no scopes and no states to count. When even the
    // lit scope doesn't fit beside them, the sessions page gives up the pages' names (which sessions are listed matters
    // more there); the notes page keeps them: they say which page this is
    let named = page_cells(app);
    let mut best = None;
    for (all, long, paged) in [(true, true, true), (true, false, true), (false, false, true), (false, false, false)] {
        let last = !paged || (named.is_empty() && !all && !long);
        let pages = if paged || app.off_list() { named.clone() } else { vec![] };
        let cells = if app.off_list() { vec![] } else { scope_cells(app, all) };
        let gap = if pages.is_empty() || cells.is_empty() { vec![] } else { vec![Span::raw("   ")] };
        let left: Vec<Span> = [chip.clone(), pages, gap, cells, extra.clone()].concat();
        let right = if app.off_list() { plugin_cell(app, long) } else { counts(long) };
        if wid(&left) + 2 + wid(&right) <= w || last {
            best = Some((left, right));
            break;
        }
    }
    let (mut left, right) = best.unwrap_or_default();
    let pad = w.saturating_sub(wid(&left) + wid(&right)).max(1);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    Line::from(left)
}

/// Wipe a part of the screen that lies on the ground: cleared, and the ground laid again
fn wipe(f: &mut Frame, r: Rect) {
    f.render_widget(ratatui::widgets::Clear, r);
    f.buffer_mut().set_style(r, theme::ground());
}

/// The row the cursor is on in the middle column (`rows`, one item's lines): on the terminal's own background from the cell
/// before the column, where a bar in the accent color marks it, to `to`, where the column beside it starts — so the row runs
/// into what it shows there. With no cell before the column the bar takes the row's first, a blank — unless the row is
/// one selected with v, whose green mark is there: that stays, and the row's own background says the cursor is on it
fn cursor_row(f: &mut Frame, rows: Rect, to: u16) {
    let full = f.area();
    let before = rows.x > full.x;
    let x = if before { rows.x - 1 } else { rows.x };
    let accent = theme::pal().accent;
    let buf = f.buffer_mut();
    buf.set_style(Rect { x, width: to.max(rows.right()) - x, ..rows }, theme::raised());
    for y in rows.top()..rows.bottom() {
        if before || buf[(x, y)].symbol() != "▌" {
            buf[(x, y)].set_symbol("▌").set_fg(accent);
        }
    }
}

/// A rule across the column beside the list, under its head
fn pane_rule(w: usize) -> Line<'static> {
    Line::styled("─".repeat(w), theme::rule())
}

/// The first line of the column beside the list: the mark and the title in bold, what `label` says at the right end
fn pane_title(ic: (&'static str, Style), title: &str, label: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let label_w: usize = label.iter().map(|s| s.content.width()).sum();
    let room = w.saturating_sub(2 + if label_w > 0 { label_w + 2 } else { 0 }).max(6);
    let left = vec![
        Span::styled(ic.0, ic.1),
        Span::raw(" "),
        Span::styled(clip(title, room).trim_end().to_string(), Style::new().add_modifier(Modifier::BOLD)),
    ];
    spread(left, label, w)
}

/// Divider when the preview is below: a faint line with the title set in it — "─ ● title ───── Claude·team ─"
pub(super) fn title_rule(ic: (&'static str, Style), title: &str, label: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let line = theme::rule();
    let label_w: usize = label.iter().map(|s| s.content.width()).sum();
    let title = clip(title, w.saturating_sub(4 + 2 + label_w + 5).max(4)).trim_end().to_string();
    let used = 2 + 2 + title.width() + 1 + if label_w > 0 { 1 + label_w } else { 0 } + 2;
    let mut v = vec![
        Span::styled("─ ", line),
        Span::styled(ic.0, ic.1),
        Span::raw(" "),
        Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ];
    v.push(Span::styled("─".repeat(w.saturating_sub(used)), line));
    if label_w > 0 {
        v.push(Span::raw(" "));
        v.extend(label);
    }
    v.push(Span::styled(" ─", line));
    Line::from(v)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    draw_screen(f, app);
    // `?` goes over whatever page it was pressed on; too long for one column on a wide screen, it widens into columns side by side
    if let Some(s) = &app.sheet {
        let full = f.area();
        let w = match floating(full) {
            true => {
                let h = full.height.saturating_sub(4).min(FILL_H).saturating_sub(modal_chrome(full));
                s.inner_width(full.width.saturating_sub(4 + 6) as usize, h as usize) as u16 + 6
            }
            false => s.width(),
        };
        modal_fill(f, w, s.fill_rows(), theme::pal().accent, |w, h| s.lines_in(w, h));
    }
    mark_torn_tails(&app.last_frame, f.buffer_mut());
    app.last_frame = f.buffer_mut().clone();
}

/// When a line of Chinese shifts left one cell as a whole (say closing the top bar's 「 未停止 」 (" Open ") tag), new chars cover the old
/// chars' left halves; ratatui leaves the right halves for the terminal to clear and doesn't write them, but tmux keeps the old char's
/// background in that cell, leaving a small colored block. When the old char has a background (or an underline, visible on spaces too),
/// mark that cell as must-write
pub(super) fn mark_torn_tails(prev: &Buffer, next: &mut Buffer) {
    if prev.area != next.area {
        return;
    }
    let seen_on_blank = Modifier::REVERSED | Modifier::UNDERLINED | Modifier::CROSSED_OUT;
    let w = next.area.width as usize;
    for row in (0..next.content.len()).step_by(w.max(1)) {
        let mut x = 0;
        while x < w {
            let end = x + next.content[row + x].cell_width().max(1) as usize;
            if end - x > 1 && end < w {
                let old = &prev.content[row + end - 1];
                let ow = old.cell_width() as usize;
                if ow > 1 && (old.bg != Color::Reset || old.modifier.intersects(seen_on_blank)) {
                    for c in &mut next.content[row + end..row + (end + ow - 1).min(w)] {
                        c.set_diff_option(CellDiffOption::AlwaysUpdate);
                    }
                }
            }
            x = end;
        }
    }
}

/// The list as items to draw, and what each one is (see `App::drawn`: the session's position, the header's, how many lines).
/// Rows selected with v (like yazi) get a green mark at the start and the whole row a light green background; in jump mode
/// the stops handed in show their letters
fn list_items(app: &App, width: usize, desk: bool, jump: Option<(&str, &[Jump])>) -> (Vec<Drawn>, Vec<ListItem<'static>>) {
    let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
    // The groups' own letters go on the rail; with none on screen, on their headers
    let keys = if jump.is_some() && !app.rail_on_screen() { app.group_keys() } else { vec![] };
    let mut rows = list_rows_keyed(&view, app.sectioned(), width, jump, &keys, &app.group_routers(), app.who_width(), desk);
    let mut ticked = vec![false; rows.len()];
    if app.picking() {
        let chosen = app.chosen();
        for (r, on) in rows.iter_mut().zip(&mut ticked) {
            if let (Some(p), None) = (r.pos, r.head)
                && app.pickable(p)
                && chosen.contains(&app.all[app.view[p]].deck_id)
            {
                r.lines.iter_mut().for_each(tick_row);
                *on = true;
            }
        }
    }
    let drawn = rows.iter().map(|r| (r.pos, r.head, r.lines.len() as u16)).collect();
    let items = rows
        .into_iter()
        .zip(ticked)
        .map(|(r, on)| {
            let lines: Vec<Line> =
                r.lines.into_iter().map(|segs| Line::from(segs.into_iter().map(|(t, s)| Span::styled(t, s)).collect::<Vec<_>>())).collect();
            let item = ListItem::new(Text::from(lines));
            if on { item.style(Style::new().bg(theme::pal().pick)) } else { item }
        })
        .collect();
    (drawn, items)
}

fn draw_screen(f: &mut Frame, app: &mut App) {
    let full = f.area();
    app.screen = full;
    if app.bill.is_some() {
        return draw_bill(f, app);
    }
    if app.trace.is_some() {
        return draw_trace(f, app);
    }
    if app.reader.is_some() {
        return draw_reader(f, app);
    }
    let roomy = roomy(full);
    // The ground: under the rail, the list, the bars and the margins round them. The column beside the list is set back on
    // the terminal's own background below, and so is the row the cursor is on
    f.buffer_mut().set_style(full, theme::ground());
    let area = if roomy { Rect { x: full.x + 3, y: full.y + 1, width: full.width - 6, height: full.height - 2 } } else { full };
    // A note of a group's notebook being written: (the key it's written under, what's written so far). It takes the place of
    // the preview, wherever the cursor is; so does the note under the cursor on a group's notes page
    let writing = app.prompt.as_ref().and_then(|p| match &p.ask {
        Ask::Note { key, .. } if notes::in_book(key) => Some((key.clone(), p.text.clone())),
        _ => None,
    });
    let note_side = writing.is_some() || app.in_notes();
    // On a group's tasks page it's the ticket under the cursor that's beside the list
    let task_side = !note_side && app.in_tasks();
    // A group's rail row has its directory and pinned notes to show even when none of its sessions is listed; the row for all
    // groups has the desk's thread, once there is a desk
    let on_group = app.on_rail() && (app.on_group_row() || app.row_router().is_some());
    let side =
        preview_side(app.mode(), full.width, full.height).filter(|_| !app.view.is_empty() || app.off_list() || note_side || on_group);
    // List only: keep one line at the bottom saying what the highlighted one is doing
    let summary = side.is_none() && app.mode() == Preview::Off && !app.view.is_empty() && !app.off_list();
    // Note / reply being written in the preview (with cursor): the bottom bar doesn't repeat its content, only the keys
    let note_in_preview = side.is_some() && matches!(&app.prompt, Some(Prompt { ask: Ask::Note { .. } | Ask::Reply { .. }, .. }));
    let reply_in_preview = note_in_preview && matches!(&app.prompt, Some(Prompt { ask: Ask::Reply { .. }, .. }));
    let foot_h = 1;
    let gap = u16::from(roomy);
    let [main, _, foot] = Layout::vertical([Constraint::Fill(1), Constraint::Length(gap), Constraint::Length(foot_h)]).areas(area);
    // A wide screen keeps a rail down the left, from the top bar's line to above the bottom bar; the top bar, list and preview
    // share what's right of it
    let main = match rail_shown(full) {
        true => {
            let [rail, _, rest] =
                Layout::horizontal([Constraint::Length(RAIL_W + 1), Constraint::Length(1), Constraint::Fill(1)]).areas(main);
            draw_rail(f, app, rail);
            rest
        }
        false => {
            // No rail of its own on this screen: one is laid over its left while the cursor is on it (drawn below, over the list)
            app.rail_area = Rect::default();
            main
        }
    };
    let [head, _, body] = Layout::vertical([Constraint::Length(1), Constraint::Length(gap), Constraint::Fill(1)]).areas(main);
    // t's terminal: takes a bit over the lower half; the list / preview stays above as usual
    let term = app.term_on.clone().filter(|k| app.terms.contains_key(k));
    let (body, term_area) = match term {
        Some(_) => {
            let h = (body.height * 11 / 20).max(5).min(body.height.saturating_sub(3));
            let [b, t] = Layout::vertical([Constraint::Fill(1), Constraint::Length(h)]).areas(body);
            (b, Some(t))
        }
        None => (body, None),
    };
    // Split ratio is fixed, not resized by the number of sessions — so the preview's edge doesn't jump up and down as sessions come and go
    let (list_area, prev_area) = match side {
        Some(Side::Below) => {
            let [l, p] = Layout::vertical([Constraint::Percentage(50), Constraint::Fill(1)]).areas(body);
            (l, Some(p))
        }
        Some(Side::Right) => {
            // Next to the rail the list gets a little under half of what's left: the rail already took its share of the width,
            // and titles are what you find a session by
            let list_w = if rail_shown(full) { body.width * 12 / 25 } else { body.width * 2 / 5 }.clamp(44, 72);
            let [l, _, p] = Layout::horizontal([Constraint::Length(list_w), Constraint::Length(3), Constraint::Fill(1)]).areas(body);
            (l, Some(p))
        }
        Some(Side::Focus) => {
            let [l, p] = Layout::vertical([Constraint::Length(FOCUS_ROWS), Constraint::Fill(1)]).areas(body);
            (l, Some(p))
        }
        None if summary => {
            let [l, s] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(body);
            (l, Some(s))
        }
        None => (body, None),
    };
    // Beside a rail the top row is the middle column's alone: it ends where the list does, and the preview starts level with it
    let (head, prev_area) = match (rail_shown(full), side, prev_area) {
        (true, Some(Side::Right), Some(p)) => {
            (Rect { width: list_area.width, ..head }, Some(Rect { y: head.y, height: p.height + (p.y - head.y), ..p }))
        }
        _ => (head, prev_area),
    };
    app.list_area = list_area;
    app.preview_area = prev_area.filter(|_| !summary).unwrap_or_default();
    // A screen with no rail of its own: called out (h), it lies over the left for as long as the cursor is on it, this
    // wide. With less room than a rail and a list's worth beside it (a phone), it takes the whole width
    let laid = (!rail_shown(full) && app.on_rail()).then_some(if main.width < 2 * (RAIL_W + 1) + 6 { main.width } else { RAIL_W + 1 });
    // Where it covers the preview, what's being written (e, s on a group's row) goes in a popup over it instead
    let covered = laid.is_some_and(|w| prev_area.is_some_and(|pa| pa.x < main.x + w));
    let (note_in_preview, reply_in_preview) = (note_in_preview && !covered, reply_in_preview && !covered);
    let (drawn, items) = list_items(app, list_area.width as usize, roomy, None);
    app.drawn = drawn;

    // A click on the tag calls the rail out; beside a rail there is none, and under one laid over it nothing to click
    let (bar, chip_w) = match rail_shown(full) {
        true => (mid_bar(app, head.width as usize), 0),
        false if app.on_rail() => (top_bar(app, head.width as usize), 0),
        false => (top_bar(app, head.width as usize), (chip_text(app).width() as u16).min(head.width)),
    };
    f.render_widget(Paragraph::new(bar), head);
    app.chip_area = Rect { width: chip_w, height: u16::from(chip_w > 0), ..head };

    // The cursor's row runs on to where the column beside the list starts, when there is one
    let reach = match (side, prev_area) {
        (Some(Side::Right), Some(p)) => p.x,
        _ => list_area.right(),
    };
    // List: a group on its notes page lists its notes there instead, one on its tasks page its tickets
    if app.on_notes() {
        app.drawn.clear();
        app.on_screen = Some((app.view_mark(), vec![]));
        if let Some(rows) = draw_notes(f, app, list_area, roomy) {
            cursor_row(f, rows, reach);
        }
    } else if app.on_tasks() {
        app.drawn.clear();
        app.on_screen = Some((app.view_mark(), vec![]));
        if let Some(rows) = draw_tasks(f, app, list_area, roomy) {
            cursor_row(f, rows, reach);
        }
    } else if app.view.is_empty() {
        let msg = if !app.filter.is_empty() {
            tr!(" 没有匹配「{}」的会话", " nothing matches “{}”", app.filter)
        } else if let Some(p) = app.status {
            tr!(" {} · Esc 清除", " {} · Esc clear", p.label().1)
        } else if app.hidden == 0 && app.group.is_some() {
            t!(" 这一组没有会话", " nothing in this group").into()
        } else if app.hidden == 0 {
            t!(" 没有会话", " no sessions").into()
        } else if app.scope == Scope::Open && app.scope_count(Scope::Week) > 0 {
            t!(" 暂无未停止的会话 · w 近 7 天", " nothing open · w 7 days").into()
        } else {
            // Ones stopped over a week ago are only in F's ■
            match app.scope {
                Scope::Open => t!(" 暂无未停止的会话 · F 查看已停止的", " nothing open · F shows stopped ones").into(),
                Scope::Week => t!(" 近 7 天暂无会话 · F 查看已停止的", " nothing in 7 days · F shows stopped ones").into(),
            }
        };
        f.render_widget(Paragraph::new(msg).style(dim()), list_area);
        app.on_screen = Some((app.view_mark(), vec![]));
    } else {
        let sel = app.list.selected();
        let sel_row = match app.head {
            Some(_) => app.drawn.iter().position(|d| d.1.is_some() && d.1 == sel),
            None => app.drawn.iter().position(|d| d.0.is_some() && d.0 == sel),
        };
        // The cursor is on the rail: no row of the list is highlighted
        app.disp.select(sel_row.filter(|_| !app.on_rail()));
        // The selected item stands out from the ground, keeping each part's color (state and account still readable at a glance);
        // `cursor_row` marks it once the list has settled. Keep one item of padding: scrolling to a group's first session also
        // shows the section header above it
        let listed = |items: Vec<ListItem<'static>>| List::new(items).highlight_style(theme::raised()).scroll_padding(1);
        let list = listed(items);
        // Doesn't all fit: the bottom line says how many more are below, and once scrolled, the top line how many above. Whether
        // it's scrolled is known only after drawing (the list scrolls to keep the highlight in view), so draw again if that changed.
        // The second try starts from a wiped list: it sits one line off the first, and the list writes nothing where a line is
        // empty or short (the blank before a group, its header), so the first try's rows showed there for a frame, a row drawn twice
        let total: usize = app.drawn.iter().map(|d| d.2 as usize).sum();
        let over = total > list_area.height as usize && list_area.height >= 5;
        let mut top = over && app.disp.offset() > 0;
        let mut area = list_area;
        for again in [false, true] {
            area = Rect { y: list_area.y + u16::from(top), height: list_area.height - u16::from(top) - u16::from(over), ..list_area };
            if again {
                wipe(f, list_area);
            }
            f.render_stateful_widget(list.clone(), area, &mut app.disp);
            if (over && app.disp.offset() > 0) == top {
                break;
            }
            top = !top;
        }
        app.list_area = area;
        // The sessions with a row on screen, now that the list has scrolled to where it stays (the list draws the rows that fit
        // whole, from its offset on). Jump mode gives its letters to these alone: a row not on screen has nowhere to show one,
        // and the fewer there are, the more of them are one letter (a group's header has none: the group has a letter of its
        // own). Drawn over the same rows, which only swap their number for it
        let mut room = area.height;
        let stops: Vec<Jump> = app
            .drawn
            .iter()
            .skip(app.disp.offset())
            .take_while(|d| {
                let fits = d.2 <= room;
                room = room.saturating_sub(d.2);
                fits
            })
            .filter_map(|d| d.0.filter(|_| d.1.is_none()).map(Jump::Row))
            .collect();
        if let Some(typed) = app.jump.clone() {
            let (_, items) = list_items(app, list_area.width as usize, roomy, Some((&typed, &stops)));
            wipe(f, area);
            f.render_stateful_widget(listed(items), area, &mut app.disp);
        }
        app.on_screen = Some((app.view_mark(), stops));
        // The rows the highlighted item ended up on, counted down from the first item drawn
        let off = app.disp.offset();
        if let Some(at) = app.disp.selected().filter(|&at| at >= off && at < app.drawn.len()) {
            let y = area.y + app.drawn[off..at].iter().map(|d| d.2).sum::<u16>();
            let rows = Rect { y, height: app.drawn[at].2.min(area.bottom().saturating_sub(y)), ..area };
            cursor_row(f, rows, reach);
        }
        if over {
            let (above, below) = beyond(app, area.height);
            let edge = |n: usize, up: bool| match n {
                0 => Line::default(),
                n if up => Line::styled(tr!(" ↑ 还有 {n} 个", " ↑ {n} more"), dim()),
                n => Line::styled(tr!(" ↓ 还有 {n} 个", " ↓ {n} more"), dim()),
            };
            if top {
                wipe(f, Rect { height: 1, ..list_area });
                f.render_widget(Paragraph::new(edge(above, true)), Rect { height: 1, ..list_area });
            }
            let last = Rect { y: list_area.y + list_area.height - 1, height: 1, ..list_area };
            wipe(f, last);
            f.render_widget(Paragraph::new(edge(below, false)), last);
        }
    }

    // Preview: the bottom of the highlighted session's screen. On the right it's a block off the ground with no frame, the title on
    // its first line; below, a faint line with the title in it parts list and preview
    let shown = preview_name(app).and_then(|n| app.all.iter().position(|s| s.name == n));
    // Beside a ticket: the session doing it, under what the ticket says
    let shown = match task_side {
        true => app.task_doer().map(|s| s.name.clone()).and_then(|n| app.all.iter().position(|s| s.name == n)),
        false => shown,
    };
    // On the row of a group that has a router: the column is the router's thread; on the rail's row for all groups, the
    // desk's. Which thread is showing is followed whether or not there's room to draw it, so leaving the row marks it seen
    let thread_seen = app.follow_thread();
    let router_at = app.row_router().map(|r| r.name.clone()).and_then(|n| app.all.iter().position(|s| s.name == n));
    let beside = app.on_group_row() || router_at.is_some();
    let spot = if shown.is_none() && beside { app.spot() } else { None };
    // The cursor on a rail row that is no group (all groups without a desk, the Trash): nothing to draw beside. Nor on a
    // notes page with no note on it yet
    let no_note = (note_side && writing.is_none() && app.note_cur().is_none()) || (task_side && app.task_cur().is_none());
    if let Some(pa) = prev_area.filter(|_| !no_note && (note_side || !app.on_rail() || beside)) {
        let (ic, title, label) = match shown.map(|i| &app.all[i]) {
            _ if note_side => note_head(app, writing.as_ref()),
            _ if task_side => task_head(app),
            Some(s) => {
                // Clip the account name when too long; the title comes first
                let label = clip(&s.agent_label(), (pa.width as usize / 3).max(12)).trim_end().to_string();
                let label = if label.is_empty() { vec![] } else { vec![Span::styled(label, agent_style(s.agent))] };
                match &s.subagent {
                    Some(a) => (agent_icon(a.phase), s.title.clone(), vec![Span::styled(t!("子代理", "subagent"), dim())]),
                    None => (session_icon(s), s.title.clone(), label),
                }
            }
            None => match router_at.map(|i| &app.all[i]) {
                // Its router stands for the group: the head is the router's, as a session's is
                Some(r) => {
                    let label = clip(&r.agent_label(), (pa.width as usize / 3).max(12)).trim_end().to_string();
                    let label = if label.is_empty() { vec![] } else { vec![Span::styled(label, agent_style(r.agent))] };
                    (session_icon(r), r.title.clone(), label)
                }
                None => {
                    let group = app.group_row().or_else(|| app.current().map(|s| s.group.as_str()));
                    // The scratch group's mark leads its name here as on its row
                    let mark = if group.is_some_and(|g| app.is_scratch(g)) { SCRATCH } else { "▸" };
                    ((mark, dim()), group.map(|g| app.shown(g).into_owned()).unwrap_or_default(), vec![])
                }
            },
        };
        // When little of the list shows (mostly preview / list only), the line says which number the highlighted one is
        let at = Span::styled(format!("{}/{}", app.list.selected().map_or(0, |p| p + 1), app.view.len()), dim());
        let label = if side == Some(Side::Focus) { [label, vec![Span::raw("  "), at.clone()]].concat() } else { label };
        // Beside the list: the rule under the column's head is still to draw, under the first line of what the column shows
        let mut rule_due = false;
        let inner = if summary {
            // List only: the preview shrinks to one line, showing state and directory
            let what = match (shown.map(|i| &app.all[i]), &spot) {
                (Some(s), _) => format!("{} · {}", state_word(s), words::tilde(&s.dir)),
                (None, Some(sp)) => format!("{title} · {}", words::tilde(&sp.dir)),
                (None, None) => title.clone(),
            };
            f.render_widget(Paragraph::new(title_rule(ic, &what, vec![at], pa.width as usize)), pa);
            Rect { height: 0, ..pa }
        } else if side == Some(Side::Right) {
            // No frame: a block on the terminal's own background, from the screen's top row to the row above the bottom bar
            // where there are margins to take those from. Its head is the title's line, a line under it and a rule
            let up = u16::from(roomy && pa.y > full.y);
            let down = u16::from(roomy && pa.bottom() + 1 == foot.y);
            f.buffer_mut().set_style(Rect { y: pa.y - up, height: pa.height + up + down, ..pa }, theme::raised());
            let inner = Rect { x: pa.x + 2, width: pa.width.saturating_sub(4), ..pa };
            let w = inner.width as usize;
            // The line under the title: who wrote a note (so not said at the title's end), the session doing a ticket. The
            // other kinds have it as their own first line (state and directory), and the rule goes in under that
            let under: Option<Vec<Line>> = match () {
                _ if note_side => Some(if label.is_empty() { vec![] } else { vec![Line::from(label.clone())] }),
                _ if task_side => {
                    let doer = app.task_doer().map(|s| Line::styled(clip(&s.title, w).trim_end().to_string(), dim()));
                    Some(doer.into_iter().collect())
                }
                _ => None,
            };
            let at_end = if note_side { vec![] } else { label };
            let top = 1.min(inner.height);
            f.render_widget(Paragraph::new(pane_title(ic, &title, at_end, w)), Rect { height: top, ..inner });
            let inner = Rect { y: inner.y + top, height: inner.height - top, ..inner };
            match under {
                Some(mut lines) => {
                    // A blank line after the rule, as the other kinds have after theirs
                    lines.extend([pane_rule(w), Line::default()]);
                    let h = (lines.len() as u16).min(inner.height);
                    f.render_widget(Paragraph::new(lines), Rect { height: h, ..inner });
                    Rect { y: inner.y + h, height: inner.height - h, ..inner }
                }
                None => {
                    rule_due = true;
                    inner
                }
            }
        } else {
            // Under the list: that block is on the terminal's background too, the line with its title parts the two
            f.buffer_mut().set_style(pa, theme::raised());
            f.render_widget(Paragraph::new(title_rule(ic, &title, label, pa.width as usize)), Rect { height: 1, ..pa });
            Rect { x: pa.x + 1, y: pa.y + 1, width: pa.width.saturating_sub(2), height: pa.height.saturating_sub(1) }
        };
        // A ticket: what it says leads the column, the session doing it has what's left below
        let inner = match task_side && inner.height > 0 {
            true => {
                let block = task_block(app, inner.width as usize);
                let h = (block.len() as u16).min(inner.height);
                f.render_widget(Paragraph::new(block), Rect { height: h, ..inner });
                Rect { y: inner.y + h, height: inner.height - h, ..inner }
            }
            false => inner,
        };
        match shown {
            _ if inner.height == 0 => {}
            None if task_side => {}
            _ if note_side => {
                let lines = note_body(app, writing.as_ref(), inner.width as usize);
                // What's written last stays in sight: a note longer than the column shows its end while it's written
                let skip = if writing.is_some() { lines.len().saturating_sub(inner.height as usize) } else { 0 };
                f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), inner);
            }
            None => {
                // Group: the notes pinned in its notebook (one being written has the whole column, see `note_side`). A group
                // without a directory yet (empty group) has only those
                let pins: Vec<list::Pin> =
                    app.route_group().map(|g| app.book.of(g).filter(|n| n.pinned).map(list::pin_of).collect()).unwrap_or_default();
                // One row is the rule's: under the line that says where the group works, or first when there's no such line
                let (w, h) = (inner.width as usize, (inner.height as usize).saturating_sub(usize::from(rule_due)));
                let rule_at = usize::from(router_at.is_some() || spot.is_some());
                // A router's column ends with the line a reply goes on: these many rows at the bottom
                let mut pen = 0;
                let mut lines = match (router_at.map(|i| &app.all[i]), &spot) {
                    (Some(r), _) => {
                        let repo = spot.as_ref().and_then(|sp| app.glances.get(&group_key(&sp.group))).and_then(|g| g.repo.as_ref());
                        // A project's router: the line under its title says how the project stands, in the numbers its
                        // row on the rail has
                        let tally = (!app.is_scratch(&r.group)).then(|| {
                            let row = app.rail_rows().into_iter().find(|x| x.kind == RailKind::Group(r.group.clone()));
                            row.map_or((None, [0; 4]), |x| (x.tasks, x.marks))
                        });
                        let pane = RouterPane {
                            router: r,
                            dir: spot.as_ref().map(|sp| sp.dir.as_str()),
                            branch: repo.map(|r| r.branch.as_str()),
                            pins: &pins,
                            tally,
                            seen: thread_seen.unwrap_or(u64::MAX),
                            now: epoch(),
                            // A reply to it being written (s): on the line it goes out from
                            writing: app.prompt.as_ref().and_then(|p| match &p.ask {
                                Ask::Reply { key, .. } if *key == notes::key(r) => Some(p.text.as_str()),
                                _ => None,
                            }),
                            kept: app.notes.get(&notes::key(r)),
                        };
                        // The sessions a thread names go by today's titles; one that is gone, by its id
                        let name = |id: &str| {
                            let s = app.all.iter().find(|s| s.deck_id == id && s.sub.is_none() && s.subagent.is_none());
                            s.map_or_else(|| id.to_string(), |s| s.title.clone())
                        };
                        pen = pen_lines(&pane, w).len() as u16;
                        router_pane(&pane, &name, w, h)
                    }
                    (None, Some(sp)) => group_pane(sp, app.glances.get(&group_key(&sp.group)), &pins, w, h),
                    (None, None) => pinned_lines(&pins, w, h / 2),
                };
                if rule_due {
                    lines.insert(rule_at.min(lines.len()), pane_rule(w));
                }
                // The column fills its rows exactly, that line the last of them; more than fit (a short column with pins,
                // what's scheduled and a long reply being written) and the bottom rows are something else: no band
                let fills = lines.len() == inner.height as usize;
                f.render_widget(Paragraph::new(lines), inner);
                // That line lies on the sunk ground, a band a cell wider than the words either side, as an input field
                if pen > 0 && fills {
                    let band = Rect { x: inner.x.saturating_sub(1), y: inner.bottom() - pen, width: inner.width + 2, height: pen };
                    f.buffer_mut().set_style(band.intersection(full), theme::ground());
                }
            }
            Some(i) => {
                let s = &app.all[i];
                // This session's note is being written: show what's being written
                let editing = app.prompt.as_ref().and_then(|p| match &p.ask {
                    Ask::Note { key, .. } if *key == notes::key(s) => Some((p.text.as_str(), Pen::Note)),
                    Ask::Reply { key, .. } if *key == notes::key(s) => Some((p.text.as_str(), Pen::Reply)),
                    _ => None,
                });
                let pen = editing.map_or(Pen::Off, |e| e.1);
                let note = editing.map(|e| e.0).or_else(|| app.notes.get(&notes::key(s)));
                let room = (inner.height as usize).saturating_sub(usize::from(rule_due));
                let mut head = preview_head(s, app.glances.get(&s.name), note, pen, inner.width as usize, room);
                if let Some(l) = agents_line(s, app.agents_open.contains(&s.name), inner.width as usize) {
                    head.insert(1.min(head.len()), l);
                }
                // The rule goes under its first line: state, directory, branch
                if rule_due {
                    head.insert(1.min(head.len()), pane_rule(inner.width as usize));
                }
                let hh = (head.len() as u16).min(inner.height);
                f.render_widget(Paragraph::new(head), Rect { height: hh, ..inner });
                let body = Rect { y: inner.y + hh, height: inner.height - hh, ..inner };
                // Nothing on screen (its pane is gone): its last turn, from the top, once read
                let turn = app.glances.get(&s.name).and_then(|g| g.turn.as_ref()).filter(|_| shows_turn(s));
                if let Some(t) = turn {
                    f.render_widget(Paragraph::new(turn_lines(t, body.width as usize, body.height as usize)), body);
                    (app.preview_for, app.preview_up, app.preview_max) = (s.name.clone(), 0, 0);
                } else {
                    let lines = ansi::wrap(&preview_lines(s), body.width as usize);
                    let h = body.height as usize;
                    // Show the bottom of the screen (latest at the bottom), moved up by what the wheel scrolled back; a subagent that
                    // has reported back shows from the top (its report comes first)
                    let top_first = s.subagent.as_ref().is_some_and(|a| a.reported);
                    let max = if top_first { 0 } else { lines.len().saturating_sub(h) };
                    let up = if app.preview_for == s.name { app.preview_up.min(max) } else { 0 };
                    let tail = match top_first {
                        true => &lines[..lines.len().min(h)],
                        false => &lines[lines.len().saturating_sub(h + up)..lines.len() - up],
                    };
                    f.render_widget(Paragraph::new(tail.iter().map(ansi::to_line).collect::<Vec<_>>()), body);
                    ansi::paint_links(f.buffer_mut(), body, tail);
                    // Scrolled up: the bottom row says how many lines are below (like the reader)
                    if up > 0 {
                        below_tag(f, body, up);
                    }
                    let name = s.name.clone();
                    (app.preview_for, app.preview_up, app.preview_max) = (name, up, max);
                }
            }
        }
    }

    // Bottom bar: key hints, or a message shown briefly.
    // New, rename, confirm, tool pick and keys are all popups, on top of this whole layer
    let w = foot.width as usize;
    let layout_strip = app.layout_at.filter(|t| t.elapsed() < Duration::from_secs(3)).map(|_| {
        let mut v = vec![Span::styled(t!("布局  ", "layout  "), dim())];
        // The layout now raised like a key cap, the others in the minor color
        for (i, (p, name)) in layouts(narrow(full.width)).into_iter().enumerate() {
            if i > 0 {
                v.push(Span::styled(" → ", theme::faint()));
            }
            v.push(match p == app.mode() {
                true => Span::styled(format!(" {name} "), key_style().bg(Color::Reset)),
                false => Span::styled(name, dim()),
            });
        }
        Line::from(v).centered()
    });
    let doing = app.doing();
    let foot_line = match &app.msg {
        // A reply: what was just said (why it can't be sent yet) comes before its keys
        Some((m, t)) if reply_in_preview && t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ if reply_in_preview => match app.prompt.as_ref().filter(|p| !p.hint.is_empty()) {
            Some(p) => Line::styled(p.hint.clone(), theme::warn()).centered(),
            None => menu_bar(&[reply_keys().iter().zip(0..).map(|(&(k, d), i)| (k, d, i)).collect()], w),
        },
        _ if note_in_preview => {
            menu_bar(&[vec![("Esc", t!("保存", "done"), 0), ("⏎", t!("换行", "newline"), 0), ("^G", t!("编辑器", "editor"), 1)]], w)
        }
        // q / Q pressed: more urgent than the hint before
        _ if app.pending.is_some() => pending_bar(app, w),
        // The reference key's first press (tmux's prefix key): what the next one does
        _ if app.ref_got > 0 => {
            let next = app.ref_keys.get(app.ref_got).copied().map(key_label).unwrap_or_default();
            menu_bar(&[vec![(next.as_str(), t!("插入引用", "insert a reference"), 0), ("Esc", t!("取消", "cancel"), 0)]], w)
        }
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ if term_area.is_some() => menu_bar(&[vec![("Esc", t!("收起", "hide"), 0), ("exit", t!("关闭", "close"), 0)]], w),
        _ if layout_strip.is_some() => layout_strip.unwrap_or_default(),
        // Multi-selecting: more urgent than background work (the keys pressed are all here)
        _ if app.picking() && !app.filtering => pick_bar(app, w),
        _ if doing.is_some() => Line::styled(doing.clone().unwrap_or_default(), theme::warn()).centered(),
        _ if app.filtering => {
            menu_bar(&[vec![("⏎", t!("确定", "ok"), 0), ("↑↓", t!("选择", "select"), 0), ("Esc", t!("清除", "clear"), 0)]], w)
        }
        // The groups' letters for `: a letter puts the cursor on that group's row; the row's own keys work from where it is
        _ if app.letters.is_some() => {
            let keys = [
                (t!("字母", "letter"), t!("到分组", "to group"), 0),
                ("⏎", t!("进入", "go in"), 2),
                ("o", "router", 1),
                ("`", t!("返回", "back"), 0),
            ];
            split_bar(&keys, &[("?", t!("快捷键", "keys"), 0)], w)
        }
        // The letters are showing: what they do (a session's opens it, unless nothing is listed or a rail laid over the list
        // covers them; a group's goes into it), and a when there is somewhere to go back to (before Space: a phone keeps two)
        _ if app.jump.is_some() => {
            let mut keys: Vec<MenuItem> = vec![];
            if !app.view.is_empty() && !app.off_list() && !(app.on_rail() && !rail_shown(full)) {
                keys.push((t!("字母", "letter"), t!("打开", "open"), 0));
            }
            if !app.group_keys().is_empty() {
                keys.push((t!("⇧字母", "⇧letter"), t!("进分组", "to group"), 0));
            }
            if app.hop_back.is_some() {
                keys.push(("a", t!("上一处", "back"), 0));
            }
            keys.push((t!("空格", "Space"), t!("取消", "cancel"), 0));
            split_bar(&keys, &[("?", t!("快捷键", "keys"), 0)], w)
        }
        // Picking (`aca ui --ref`, or the reference key here): only the keys that work here; started here, where it goes
        _ if app.insert_to.is_some() => {
            let into = app.insert_back.as_ref().map(|(_, t)| tr!("插入到「{}」", "insert into “{}”", clip(t, 12).trim_end()));
            let enter = match () {
                _ if app.head.is_some() => t!("进入", "go in"),
                _ => into.as_deref().unwrap_or(t!("插入", "insert")),
            };
            menu_bar(&[vec![("⏎", enter, 0), ("Esc", t!("取消", "cancel"), 0)], vec![("/", t!("搜索", "search"), 3)]], w)
        }
        _ => list_bar(app, w),
    };
    f.render_widget(Paragraph::new(foot_line), foot);
    // The rail laid over the left (see `laid`)
    if let Some(w) = laid {
        let lay = Rect { width: w, ..main };
        wipe(f, lay);
        draw_rail(f, app, lay);
    }
    if let (Some(key), Some(ta)) = (app.term_on.clone(), term_area) {
        draw_term(f, app, &key, ta);
    }

    // Popups
    if let Some((tools, selected)) = &app.router_picker {
        super::draft::draw_router_choices(f, tools, *selected);
    } else if let Some(d) = &app.new_draft {
        draw_draft(f, d);
    } else if let Some(p) = app.prompt.as_ref().filter(|_| !note_in_preview) {
        draw_prompt(f, p);
    } else if let Some((menu, sel)) = &app.tool_menu {
        let set = match app.tool_special {
            true => Set::Special { back: app.tool_back.is_some() },
            false => Set::Here(!app.tool_more.is_empty()),
        };
        let logins = app.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).clone();
        draw_tools(f, menu, *sel, app.handoff.as_ref(), &app.tool_quotas, &logins, set, app.tool_relogin);
    } else if let Some(o) = app.overlays.last() {
        // The bottom bar's line (just said, or else the background work) also goes at the bottom of the popup: on a phone the popup covers
        // the bottom bar, so results of things done with a popup open would be invisible
        let note = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.clone()).or_else(|| app.doing());
        if o.fill() {
            // Box size stays: the last line is kept for that line (two if long), blank when nothing to say; page lines don't jump
            modal_fill(f, o.width(), o.fill_rows(), theme::pal().accent, |w, h| {
                let mut said: Vec<Line> = note
                    .as_ref()
                    .map_or(vec![], |n| wrap_width(n, w).into_iter().take(2).map(|l| Line::styled(l, theme::warn())).collect());
                if said.is_empty() {
                    said.push(Line::raw(""));
                }
                let mut lines = o.lines_in(w, h.saturating_sub(said.len()));
                lines.extend(said);
                lines
            });
        } else {
            modal(f, o.width(), theme::pal().accent, |w| {
                let mut lines = o.lines(w);
                if let Some(n) = &note {
                    lines.push(Line::raw(""));
                    lines.extend(wrap_width(n, w).into_iter().map(|l| Line::styled(l, theme::warn())));
                }
                lines
            });
        }
    } else if let Some(sel) = app.space_menu {
        draw_spaces(f, app, sel);
    } else if let Some(sel) = app.only_menu {
        draw_only(f, app, sel);
    } else if let Some(m) = &app.give_menu {
        draw_give(f, app, m);
    } else if let Some(m) = &app.theme_menu {
        draw_themes(f, m);
    } else if let Some(st) = &app.settings {
        let note = app.msg.as_ref().filter(|(_, t)| t.elapsed() < app.msg_ttl).map(|(m, _)| m.as_str());
        draw_settings(f, st, note);
    } else if let Some(Confirm::Answer { t, question, shown, offer }) = &app.confirm {
        // Beside the preview, not over it: the preview is that pane's real screen (the command it wants to run)
        let list = (side == Some(Side::Right) || side == Some(Side::Below)).then_some(list_area);
        draw_answer(f, list, prev_area.is_some() && !summary, t, question, shown.as_ref(), offer);
    } else if let Some(c) = &app.confirm {
        draw_confirm(f, c, &app.all);
    }
}

/// A row selected with v (drawn for every row): the blank cell at the start becomes a green "▌" (like yazi's mark),
/// the number turns green too
fn tick_row(segs: &mut Vec<(String, Style)>) {
    let Some((text, style)) = segs.first_mut() else { return };
    let Some(rest) = text.strip_prefix(' ') else { return };
    let green = Style::new().fg(theme::pal().green);
    *text = rest.to_string();
    if !text.trim().is_empty() {
        *style = green.add_modifier(Modifier::BOLD);
    }
    segs.insert(0, ("▌".into(), green));
}

/// Bottom bar while multi-selecting (like yazi): first a green block saying how many are selected (during a range select, first whether the
/// range selects or unselects), then the keys acting on all of them together (main actions; p / u say what pressing does), last range /
/// select / set / none
fn pick_bar(app: &App, w: usize) -> Line<'static> {
    let rows = app.chosen_rows();
    let n = rows.len();
    let count = tr!("已选 {n}", "{n} selected");
    let badge = match app.anchor {
        Some(_) if app.unpick => button(&format!("{} · {count}", t!("取消选中", "unselect")), theme::pal().red),
        Some(_) => button(&format!("{} · {count}", t!("连选", "range")), theme::pal().green),
        None => button(&count, theme::pal().green),
    };
    let head = vec![badge, Span::raw(" ")];
    let pin = if rows.iter().any(|s| app.pins.rank(&s.name).is_none()) { t!("置顶", "pin") } else { t!("取消置顶", "unpin") };
    let read = if rows.iter().any(|s| s.state == State::Unread) || !rows.iter().any(|s| s.state == State::Idle) {
        t!("已读", "read")
    } else {
        t!("未读", "unread")
    };
    // Nothing selected (V pressed with nothing ticked): nothing to act on together
    let acts: Vec<MenuItem> = if n == 0 {
        vec![]
    } else {
        vec![
            ("d", t!("删除", "delete"), 0),
            ("m", t!("移到分组", "move"), 2),
            ("D", t!("停止", "stop"), 3),
            ("u", read, 4),
            ("R", t!("重启", "restart"), 5),
            ("p", pin, 6),
            ("Q", t!("移动到工作区", "to space"), 7),
        ]
    };
    // Whether the highlighted row is ticked: whether Space ticks or unticks.
    // When it doesn't fit drop Space first, then V (a phone keeps only v, Esc)
    let on = app.list.selected().is_some_and(|p| app.head.is_none() && app.picked.contains(&app.all[app.view[p]].deck_id));
    let tail: Vec<MenuItem> = match app.anchor {
        Some(_) => vec![("Esc", t!("结束", "end"), 0)],
        None => vec![
            ("v", t!("连选", "range"), 0),
            ("V", t!("连续取消", "range unselect"), 1),
            (t!("空格", "Space"), if on { t!("取消选中", "unselect") } else { t!("选中", "select") }, 1),
            ("Esc", t!("清空选择", "clear"), 0),
        ],
    };
    let used: usize = head.iter().map(|s| s.content.width()).sum();
    let mut l = menu_bar_in(&[acts, tail], w.saturating_sub(used), Some(theme::pal().pick));
    l.spans.splice(0..0, head);
    l
}

/// How many sessions are scrolled out of view above and below, the list drawn h lines high (ratatui draws only rows that fit
/// whole). Rows of sessions count; headers and the closing hint don't
fn beyond(app: &App, h: u16) -> (usize, usize) {
    let session = |d: &Drawn| matches!(*d, (Some(_), None, _));
    let off = app.disp.offset().min(app.drawn.len());
    let (mut used, mut end) = (0, off);
    for d in &app.drawn[off..] {
        if used + d.2 > h {
            break;
        }
        used += d.2;
        end += 1;
    }
    (app.drawn[..off].iter().filter(|d| session(d)).count(), app.drawn[end..].iter().filter(|d| session(d)).count())
}

/// The keys of the router a row stands for (`g`: its group; None = the rail's row for all groups with no scratch group yet):
/// s by the state the router is in, as on a session's row; o opens it, starts one that isn't running, or makes one. The
/// scratch group's is the desk, and is called that
fn route_keys(app: &App, g: Option<&str>) -> Vec<(&'static str, &'static str)> {
    let desk = g.is_none_or(|g| app.is_scratch(g));
    let word = |desk_word: &'static str, router_word: &'static str| if desk { desk_word } else { router_word };
    let mut keys = vec![];
    match g.and_then(|g| app.router_of(g)) {
        Some(r) if starts(r) => {
            if list::wakes_on_send(r) {
                keys.push(("s", t!("发话", "send")));
            }
            keys.push(("o", word(t!("启动总台", "start desk"), t!("启动 router", "start router"))));
        }
        Some(r) => {
            match r.state {
                State::Ask => keys.push(("s", t!("回答", "answer"))),
                _ if cant_fill(r).is_none() => keys.push(("s", t!("回话", "reply"))),
                _ => {}
            }
            keys.push(("o", word(t!("打开总台", "open desk"), t!("打开 router", "open router"))));
        }
        None if desk || g.is_some_and(router::can_route) => {
            keys.push(("o", word(t!("新建总台", "new desk"), t!("新建 router", "new router"))));
        }
        None => {}
    }
    keys
}

/// The list's bottom bar when nothing more urgent is showing: the highlighted row's keys on the left (see `row_keys`; a group's
/// heading has its own), the keys that work anywhere on the right. Clearing a search, or going back while following, comes first
fn list_bar(app: &App, w: usize) -> Line<'static> {
    if app.in_notes() {
        return notes_bar(app, w);
    }
    if app.in_tasks() {
        return tasks_bar(app, w);
    }
    let mut left: Vec<(&str, &str)> = vec![];
    if !app.filter.is_empty() || app.status.is_some() {
        left.push(("Esc", t!("清除", "clear")));
    } else if app.following.is_some() {
        left.push(("Esc", t!("返回", "go back")));
    }
    let on_agent = !app.on_rail() && app.head.is_none() && app.current().is_some_and(|s| s.subagent.is_some());
    // A group's row: N and r come last, and are the first to go as the bar narrows
    let mut last: Vec<(&str, &str)> = vec![];
    match app.group_row() {
        _ if app.rail == Some(RailAt::Trash) => left.push(("⏎", t!("打开", "open"))),
        Some(g) => {
            left.push(("⏎", t!("进入", "go in")));
            left.extend(route_keys(app, Some(g)));
            // On the rail, on the row of the group the middle column shows: its other page
            if app.on_rail() && app.group.is_some() {
                left.push(("Tab", t!("换页", "page")));
            }
            left.push(("e", t!("写笔记", "write a note")));
            // The ordinary tmux sessions' section isn't a group you can add to or rename
            if g != "-" {
                last.extend([("N", t!("在组里新建", "new in group")), ("r", t!("改名", "rename"))]);
            }
        }
        // The rail's row for all groups: the desk's keys
        None if app.on_rail() => {
            left.push(("⏎", t!("进入", "go in")));
            left.extend(route_keys(app, app.scratch.as_deref()));
        }
        None => {
            let mut keys = app.current().map(|s| row_keys(s, s.noted)).unwrap_or_default();
            // Following: it's already beside
            if let Some(k) = keys.first_mut().filter(|_| app.following.is_some() && !on_agent) {
                k.1 = t!("留下", "stay");
            }
            left.extend(keys);
            // Inside a group: its other page
            if app.group.is_some() {
                left.push(("Tab", t!("换页", "page")));
            }
        }
    }
    let mut left: Vec<MenuItem> =
        left.into_iter().enumerate().map(|(i, (k, d))| (k, d, [0, 0, 6, 7].get(i).copied().unwrap_or(7))).collect();
    // Dropped before anything else on the bar
    left.extend(last.into_iter().map(|(k, d)| (k, d, 10)));
    let mut right: Vec<MenuItem> = vec![];
    // a: what it does from here. In all groups with nowhere to go back to it only shows the letters, as Space does: not listed
    match (&app.group, app.on_rail(), &app.hop_back) {
        (Some(_), _, _) => right.push(("a", t!("全部", "all"), 6)),
        (None, true, _) => right.push(("a", t!("跳转", "jump"), 6)),
        (None, false, Some(_)) => right.push(("a", t!("上一处", "back"), 6)),
        (None, false, None) => {}
    }
    if app.view.iter().any(|&i| waiting(&app.all[i])) {
        right.push(("i", t!("下一个", "next"), 5));
    }
    if !on_agent {
        right.push(("n", t!("新建", "new"), 8));
    }
    right.extend([("/", t!("搜索", "search"), 9), ("?", t!("快捷键", "keys"), 0)]);
    split_bar(&left, &right, w)
}

/// A key as the bottom bar names it: `^B`, `M-x`, `F6`, `b`
fn key_label((code, mods): (KeyCode, KeyModifiers)) -> String {
    let base = match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        _ => String::new(),
    };
    match () {
        _ if mods.contains(KeyModifiers::CONTROL) => format!("^{}", base.to_uppercase()),
        _ if mods.contains(KeyModifiers::ALT) => format!("M-{base}"),
        _ => base,
    }
}

/// c pressed: what the next key copies. On a phone the later ones drop first
pub(super) fn copy_bar(w: usize) -> Line<'static> {
    let what = t!("复制", "Copy");
    let mut items: Vec<MenuItem> = vec![("c", t!("最后回复", "last reply"), 0), ("b", t!("引用", "reference"), 1)];
    let names = crate::copies::names().map(|(k, n)| (k.to_string(), n));
    items.extend(names.iter().enumerate().map(|(i, (k, n))| (k.as_str(), *n, i as u8 + 2)));
    let mut l = menu_bar(&[items, vec![("Esc", t!("取消", "cancel"), 0)]], w.saturating_sub(what.width() + 2));
    l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
    l
}

/// After q / Q / c, waiting for the next key: after q, that q again quits; after Q, the workspaces it can go to (not h, nor the
/// current one); after c, what to copy
fn pending_bar(app: &App, w: usize) -> Line<'static> {
    let cancel = ("Esc", t!("取消", "cancel"), 0);
    if app.pending == Some(Pending::Quit) {
        return menu_bar(&[vec![("q", t!("退出", "quit"), 0), cancel]], w);
    }
    if app.pending == Some(Pending::Copy) {
        return copy_bar(w);
    }
    let names: Vec<(String, String)> =
        app.space_list().into_iter().filter(|&c| c != app.space).map(|c| (c.to_string(), app.spaces.name(c))).collect();
    let mut items: Vec<MenuItem> = names.iter().enumerate().map(|(i, (k, n))| (k.as_str(), n.as_str(), (i + 1).min(200) as u8)).collect();
    let (what, tail) = (t!("移到", "Send to"), vec![cancel]);
    if items.is_empty() {
        items = tail;
        let mut l = menu_bar(&[items], w.saturating_sub(what.width() + 2));
        l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
        return l;
    }
    let mut l = menu_bar(&[items, tail], w.saturating_sub(what.width() + 2));
    l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
    l
}

/// S's keys, for its bottom bar and `?`
pub(super) fn space_keys() -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    vec![
        key("⏎", t!("切换", "switch")),
        key("^R", t!("重命名", "rename")),
        more(t!("字母 数字", "letter digit"), t!("切到这个工作区", "go to that workspace")),
        more("↑↓", t!("选择", "select")),
        more("Esc", t!("关闭", "close")),
    ]
}

/// F's keys, for its bottom bar and `?`
pub(super) fn only_keys() -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    vec![
        key("⏎", t!("筛选", "only")),
        key("0", t!("所有", "every")),
        more("1-6", t!("只看这一种", "only this state")),
        more("↑↓", t!("选择", "select")),
        more("Esc", t!("关闭", "close")),
    ]
}

/// S: workspace list. One per row: key, name, how many sessions, count per state; the current one's key in accent color, the highlighted
/// row gets a background
fn draw_spaces(f: &mut Frame, app: &App, sel: usize) {
    let list = app.space_list();
    modal(f, 48, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("工作区", "Workspace"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        // Unnamed ones show only the key, without repeating the key as the name
        let shown = |c: char| if app.spaces.renamed(c) { app.spaces.name(c) } else { String::new() };
        let name_w = list.iter().map(|&c| shown(c).width()).max().unwrap_or(0).min(16);
        for (i, &c) in list.iter().enumerate() {
            let heads: Vec<&Session> = app.all.iter().filter(|s| s.space == c && s.sub.is_none() && !s.blank).collect();
            let marks = TALLY_MARKS.map(|st| heads.iter().filter(|s| s.state == st).count());
            let key_st = if c == app.space { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut line = menu_row(&c.to_string(), key_st, &shown(c), name_w, (heads.len(), marks));
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line_help(&crate::sheet::foot(&space_keys()), vec![], w));
        out
    });
}

/// A row of the workspace list: key, name, how many sessions, count per state
fn menu_row(key: &str, key_st: Style, name: &str, name_w: usize, (n, marks): (usize, [usize; 4])) -> Line<'static> {
    let mut spans = vec![
        Span::styled(key.to_string(), key_st),
        Span::raw("  "),
        Span::styled(clip(name, name_w), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {n:>3}"), dim()),
    ];
    for (&st, &n) in TALLY_MARKS.iter().zip(&marks) {
        if n > 0 {
            let (ic, sty) = icon(st);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(ic, sty));
            spans.push(Span::raw(n.to_string()));
        }
    }
    Line::from(spans)
}

/// A state's icon (as in the list) and name (as in the top bar) in `F`'s small list
fn pick_look(p: Pick) -> (Vec<(&'static str, Style)>, &'static str) {
    match p {
        Pick::Every => (vec![], t!("所有", "every")),
        Pick::Run => (vec![icon(State::Running)], t!("运行中", "running")),
        Pick::Wait => (vec![icon(State::Ask), icon(State::Unread)], t!("等待回应 · 未读", "needs you · unread")),
        Pick::Idle => (vec![icon(State::Idle)], t!("空闲", "idle")),
        Pick::Err => (vec![icon(State::Error)], t!("出错", "error")),
        Pick::Off => (vec![icon(State::Exited)], t!("已停止", "stopped")),
        Pick::Reaped => (vec![(aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted))], t!("自动停止", "auto-stopped")),
    }
}

/// F: small list to view only one state. One state per row: first the key that picks it directly (the picked one in accent color), icon,
/// name, how many in this workspace; row 0 is every session (0). ☾ is a part of ■, so its icon and name sit two cells further in. The
/// highlighted row gets a background
fn draw_only(f: &mut Frame, app: &App, sel: usize) {
    modal(f, 44, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("按状态筛选", "Only"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let row = |key: &str, on: bool, icons: Vec<(&'static str, Style)>, name: &str, n: usize, inner: bool| -> Line<'static> {
            let key_st = if on { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut spans = vec![Span::styled(key.to_string(), key_st), Span::raw(if inner { "    " } else { "  " })];
            let iw: usize = icons.iter().map(|(ic, _)| ic.width()).sum();
            spans.extend(icons.into_iter().map(|(ic, st)| Span::styled(ic, st)));
            spans.push(Span::raw(" ".repeat(4usize.saturating_sub(iw))));
            spans.push(Span::styled(name.to_string(), Style::new().add_modifier(Modifier::BOLD)));
            spread(spans, vec![Span::styled(n.to_string(), dim())], w)
        };
        let rows = PICKS.map(|p| {
            let (icons, name) = pick_look(p);
            row(&p.label().0.to_string(), app.status == Some(p), icons, name, app.pick_count(p), p == Pick::Reaped)
        });
        for (i, mut line) in rows.into_iter().enumerate() {
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line_help(&crate::sheet::foot(&only_keys()), vec![], w));
        out
    });
}

/// t's terminal: the top line says where it's open, below is the shell's screen, the cursor on its cursor
fn draw_term(f: &mut Frame, app: &mut App, key: &str, area: Rect) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let Some(t) = app.terms.get_mut(key) else { return };
    t.resize(body.height, body.width);
    let w = head.width as usize;
    let dir = words::tilde(&t.dir);
    let label = tr!(" 终端 · {} ", " terminal · {} ", clip(&t.title, 16).trim_end());
    let rest = w.saturating_sub(label.width() + 1);
    let head_line = Line::from(vec![Span::styled(label, theme::chip()), Span::raw(" "), Span::styled(clip_left(&dir, rest), dim())]);
    f.render_widget(Paragraph::new(head_line), head);
    f.render_widget(ratatui::widgets::Clear, body);
    // With a popup on top (the login link one), no cursor: keys go to the popup
    let covered = !app.overlays.is_empty();
    if let Some(pos) = app.terms.get(key).and_then(|t| t.render(body, f.buffer_mut())).filter(|_| !covered) {
        f.set_cursor_position(pos);
    }
}

/// Where a popup goes: wide and tall enough (desktop) floats centered with a shadow; narrow (phone) rises from the bottom at full width
pub(super) fn modal_area(full: Rect, w: u16, h: u16) -> (Rect, bool) {
    if full.width >= 70 && full.height >= 20 {
        let (w, h) = (w.min(full.width.saturating_sub(4)), h.min(full.height.saturating_sub(2)));
        (Rect { x: full.x + (full.width - w) / 2, y: full.y + (full.height - h) / 2, width: w, height: h }, true)
    } else {
        let h = h.min(full.height);
        (Rect { x: full.x, y: full.y + full.height - h, width: full.width, height: h }, false)
    }
}

/// Whether a popup floats on this screen (desktop); on a phone it's a full-width strip at the bottom
pub(super) fn floating(full: Rect) -> bool {
    modal_area(full, 1, 1).1
}

/// Before turning cell (x, y) into a narrow char: if it's the second half of a wide char, or a wide char itself, clear the other half to a
/// space so chars don't misalign
fn split_wide(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    let area = buf.area;
    if x > area.left() && buf[(x - 1, y)].symbol().width() > 1 {
        buf[(x - 1, y)].set_symbol(" ");
    }
    if buf[(x, y)].symbol().width() > 1 && x + 1 < area.right() {
        buf[(x + 1, y)].set_symbol(" ");
    }
}

/// What a popup lies over goes grey: text faint grey, weight removed, and all of it on the ground (the column beside the
/// list too), so the popup — on the terminal's own background — is the one thing standing out
fn fade(buf: &mut Buffer, area: Rect) {
    let pal = theme::pal();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_style(Style::reset().fg(pal.fade).bg(pal.back));
        }
    }
}

/// A popup's backdrop and frame: the whole layer below goes grey (`fade`), the panel cleared and given a rounded border (accent
/// color normally, red for delete). Returns the inside of the panel (minus border and padding)
pub(super) fn modal_frame(f: &mut Frame, w: u16, h: u16, border: Color) -> Rect {
    let full = f.area();
    let (r, float) = modal_area(full, w, h);
    fade(f.buffer_mut(), full);
    panel(f, r, float, border)
}

/// The panel itself: split wide chars sitting on the edge, clear, draw a rounded border. Returns the inside (minus border and padding)
fn panel(f: &mut Frame, r: Rect, float: bool, border: Color) -> Rect {
    let full = f.area();
    let buf = f.buffer_mut();
    for y in r.top()..r.bottom() {
        split_wide(buf, r.left(), y);
        if r.right() < full.right() {
            split_wide(buf, r.right(), y);
        }
    }
    f.render_widget(ratatui::widgets::Clear, r);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(border)).padding(if float {
        Padding::new(2, 2, 1, 1)
    } else {
        Padding::new(1, 1, 0, 0)
    });
    let inner = block.inner(r);
    f.render_widget(block, r);
    inner
}

/// How wide a side panel is: want on desktop (a bit less on a narrow screen), full width on a phone;
/// the second value is the usable inner width
pub(super) fn side_width(full: Rect, want: u16) -> (u16, usize) {
    let float = floating(full);
    let outer = if float { want.min(full.width.saturating_sub(4)) } else { full.width };
    (outer, outer.saturating_sub(if float { 6 } else { 4 }) as usize)
}

/// A popup that leaves the preview in view: on desktop centered over the list (`list`, the preview beside or below it isn't greyed
/// out) when it fits there, else centered on the screen as usual; on a phone with a preview (`preview`) a strip at the top, leaving
/// the bottom of the preview — where an agent asks — in view
pub(super) fn modal_beside(
    f: &mut Frame,
    list: Option<Rect>,
    preview: bool,
    w: u16,
    border: Color,
    lines: impl Fn(usize) -> Vec<Line<'static>>,
) {
    let full = f.area();
    if !floating(full) {
        if !preview {
            return modal(f, w, border, lines);
        }
        let body = lines(full.width.saturating_sub(4) as usize);
        let r = Rect { height: (body.len() as u16 + modal_chrome(full)).min(full.height), ..full };
        let inner = panel(f, r, false, border);
        return f.render_widget(Paragraph::new(body), inner);
    }
    let Some(l) = list.filter(|l| l.width >= 40) else { return modal(f, w, border, lines) };
    let inside = w.min(l.width.saturating_sub(2));
    let body = lines(inside.saturating_sub(6) as usize);
    let h = body.len() as u16 + modal_chrome(full);
    if h + 1 > l.height {
        return modal(f, w, border, lines);
    }
    let r = Rect { x: l.x + (l.width - inside) / 2, y: l.y + (l.height - h) / 2, width: inside, height: h };
    fade(f.buffer_mut(), l);
    let inner = panel(f, r, true, border);
    f.render_widget(Paragraph::new(body), inner);
}

/// A panel that doesn't grey out what's below (the color theme list: the aca below stays visible as the preview). On desktop it floats in
/// the bottom-left corner, on a phone it rises from the bottom
pub(super) fn side_panel(f: &mut Frame, outer: u16, body: Vec<Line<'static>>) {
    let full = f.area();
    let float = floating(full);
    let h = body.len() as u16 + modal_chrome(full);
    let r = if float {
        let h = h.min(full.height.saturating_sub(2));
        // Two cells off the left, two above the bottom bar
        Rect { x: full.x + 2, y: full.bottom().saturating_sub(h + 2), width: outer, height: h }
    } else {
        modal_area(full, outer, h).0
    };
    let inner = panel(f, r, float, theme::pal().accent);
    f.render_widget(Paragraph::new(body), inner);
}

/// How many rows the popup's frame plus padding take (by where the popup ends up)
pub(super) fn modal_chrome(full: Rect) -> u16 {
    if floating(full) { 4 } else { 2 }
}

/// Input box: "> text▏", only the end when too long, the cursor always visible;
/// a part can hang on the right (like 「新目录」 ("new directory"))
pub(super) fn input_row(text: &str, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let right_w: usize = right.iter().map(|s| s.content.width()).sum();
    let room = w.saturating_sub(2 + 1 + if right_w > 0 { right_w + 2 } else { 0 });
    let mut shown = text.to_string();
    if shown.width() > room {
        let mut tail: Vec<char> = vec![];
        let mut n = 1; // room for …
        for c in text.chars().rev() {
            n += c.width().unwrap_or(0);
            if n > room {
                break;
            }
            tail.push(c);
        }
        shown = format!("…{}", tail.into_iter().rev().collect::<String>());
    }
    let left = vec![Span::styled("> ", theme::accent().add_modifier(Modifier::BOLD)), Span::raw(shown), Span::styled("▏", theme::accent())];
    spread(left, right, w)
}

/// The line under the input box
pub(super) fn input_rule(w: usize) -> Line<'static> {
    Line::styled("─".repeat(w), dim())
}

/// Draw a popup: work out its height (content lines + frame), draw the frame, then fill in the lines
pub(super) fn modal(f: &mut Frame, w: u16, border: Color, lines: impl Fn(usize) -> Vec<Line<'static>>) {
    let full = f.area();
    let (probe, float) = modal_area(full, w, full.height);
    let inner_w = probe.width.saturating_sub(if float { 6 } else { 4 }) as usize;
    let body = lines(inner_w);
    let h = body.len() as u16 + modal_chrome(full);
    let inner = modal_frame(f, w, h, border);
    f.render_widget(Paragraph::new(body), inner);
}

/// Max height of fixed-size popups (they don't fill a large screen)
const FILL_H: u16 = 44;

/// Fixed-size popups (plugins page, accounts page): on desktop w wide, full screen height (two lines off top and bottom,
/// at most FILL_H; a page that asks for only `rows` lines gets that, plus one line for the message at the bottom),
/// full screen on a phone; exactly h lines inside
fn modal_fill(f: &mut Frame, w: u16, rows: Option<u16>, border: Color, lines: impl Fn(usize, usize) -> Vec<Line<'static>>) {
    let full = f.area();
    let most = full.height.saturating_sub(4).min(FILL_H);
    let h = match rows {
        _ if !floating(full) => full.height,
        Some(r) => most.min(r + 1 + modal_chrome(full)),
        None => most,
    };
    let inner = modal_frame(f, w, h, border);
    let mut body = lines(inner.width as usize, inner.height as usize);
    body.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(body), inner);
}
