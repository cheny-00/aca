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
    let label = app.group.as_ref().map_or_else(|| app.space_label(), |g| group_label(g).to_string());
    format!(" {} ", clip(&label, 14).trim_end())
}

/// Top bar: left is what you're looking at (accent-colored tag + days / all / open, or the search term), right is the count per state. When
/// narrow, shorten the counts first (◆1), then keep only the current scope
pub(super) fn top_bar(app: &App, w: usize) -> Line<'static> {
    let on = theme::accent().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let chip = vec![Span::styled(chip_text(app), theme::chip()), Span::raw("  ")];
    let tabs = |all: bool| -> Vec<Span<'static>> {
        if !app.filter.is_empty() || app.filtering {
            let cursor = if app.filtering { "▏" } else { "" };
            return vec![
                Span::styled("/ ", theme::accent()),
                Span::styled(format!("{}{cursor}", app.filter), Style::new().add_modifier(Modifier::BOLD)),
            ];
        }
        let mut v = vec![];
        let mut tab = |t: String, active: bool| {
            if all || active {
                if !v.is_empty() {
                    v.push(Span::raw("  "));
                }
                v.push(Span::styled(t, if active { on } else { dim() }));
            }
        };
        for d in DAYS {
            tab(tr!("{d} 天", "{d}d"), app.scope == Scope::Recent && app.days == d);
        }
        tab(t!("全部", "All").into(), app.scope == Scope::All);
        // "Open" is a toggle stacked on the scope, not one of its steps:
        // set apart, a tag when on, not mixed with the scope's underline
        if all || app.open {
            if !v.is_empty() {
                v.push(Span::styled("  │  ", theme::faint()));
            }
            v.push(if app.open {
                Span::styled(t!(" 未停止 ", " Open "), theme::chip())
            } else {
                Span::styled(t!("未停止", "Open"), dim())
            });
        }
        if app.fell_back {
            v.push(Span::styled(t!("  近期无活动，显示全部", "  nothing active, showing all"), dim()));
        }
        v
    };
    let mut extra = vec![];
    if !app.digits.is_empty() && app.digits_at.elapsed() <= DIGIT_GAP {
        extra.push(Span::styled(format!("  #{}", app.digits), theme::accent().add_modifier(Modifier::BOLD)));
    }
    // Status picked with ! @ # & ^ %: those cells get the selection background (search ignores status, so no background then). Idle, ■, ☾
    // aren't counted normally; listed when picked, even at 0
    let pick = app.status.filter(|_| app.filter.is_empty());
    // Like i, count only what's visible in the list (folded groups' placeholder rows don't count); the ■ cell counts everything ^ picks
    // (exited, errored and not running, ☾ too), the ☾ cell only auto-stopped ones
    let count = |hit: &dyn Fn(&Session) -> bool| {
        app.view
            .iter()
            .enumerate()
            .filter(|&(p, &i)| !app.is_fold_row(p) && app.all[i].subagent.is_none() && !app.all[i].blank && hit(&app.all[i]))
            .count()
    };
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
        // Broken plugins: how many plugins, not how many places
        let mut broken: Vec<&str> = app.plugin_alerts.iter().map(|p| p.plugin.as_str()).collect();
        broken.dedup();
        if !broken.is_empty() {
            if !v.is_empty() {
                v.push(Span::raw(if long { "    " } else { "  " }));
            }
            v.push(Span::styled("⚠", theme::warn()));
            v.push(Span::styled(
                if long { format!(" {}", broken.len()) } else { broken.len().to_string() },
                Style::new().add_modifier(Modifier::BOLD),
            ));
            if long {
                v.push(Span::styled(t!(" 插件出错(P)", " plugin errors (P)"), dim()));
            }
        }
        if v.is_empty() {
            v.push(Span::styled(t!("暂无待处理会话", "nothing to look at"), dim()));
        }
        v
    };
    let wid = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    let mut best = None;
    for (all, long) in [(true, true), (true, false), (false, false)] {
        let left: Vec<Span> = [chip.clone(), tabs(all), extra.clone()].concat();
        let right = counts(long);
        if wid(&left) + 2 + wid(&right) <= w || (!all && !long) {
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

/// Divider when the preview is below: a faint line with the title set in it — "─ ● title ───── Claude·team ─"
pub(super) fn title_rule(ic: (&'static str, Style), title: &str, label: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let line = theme::faint();
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

fn draw_screen(f: &mut Frame, app: &mut App) {
    let full = f.area();
    app.screen = full;
    if app.bill.is_some() {
        return draw_bill(f, app);
    }
    if app.trace.is_some() {
        return draw_trace(f, app);
    }
    if app.reader.as_ref().is_some_and(|r| !r.help) {
        return draw_reader(f, app);
    }
    let roomy = roomy(full);
    let area = if roomy { Rect { x: full.x + 3, y: full.y + 1, width: full.width - 6, height: full.height - 2 } } else { full };
    let side = preview_side(app.mode(), full.width, full.height).filter(|_| !app.view.is_empty());
    // List only: keep one line at the bottom saying what the highlighted one is doing
    let summary = side.is_none() && app.mode() == Preview::Off && !app.view.is_empty();
    // Note being written in the preview (with cursor): the bottom bar doesn't repeat its content, only the keys
    let note_in_preview = side.is_some() && matches!(&app.prompt, Some(Prompt { ask: Ask::Note { .. }, .. }));
    let foot_h = 1;
    let gap = u16::from(roomy);
    let [head, _, body, _, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Fill(1),
        Constraint::Length(gap),
        Constraint::Length(foot_h),
    ])
    .areas(area);
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
            let list_w = (body.width * 2 / 5).clamp(44, 72);
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
    app.list_area = list_area;
    app.preview_area = prev_area.filter(|_| !summary).unwrap_or_default();
    let mut rows = {
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        list_rows(&view, app.sectioned(), list_area.width as usize, app.jump.as_deref(), &app.folds, app.who_width(), roomy)
    };
    // Rows selected with v (like yazi): a green mark at the start, the whole row on a light green background
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
    if app.hidden > 0 && !app.view.is_empty() {
        if roomy {
            rows.push(Row { pos: None, head: None, lines: vec![vec![]] });
        }
        rows.push(Row {
            pos: None,
            head: None,
            lines: vec![vec![
                (tr!(" 还有 {} 个更早的 · ", " {} more, older · ", app.hidden), dim()),
                ("*".into(), key_style()),
                (t!(" 放宽", " widen").into(), dim()),
            ]],
        });
    }
    app.drawn = rows.iter().map(|r| (r.pos, r.head, r.lines.len() as u16)).collect();

    f.render_widget(Paragraph::new(top_bar(app, head.width as usize)), head);
    app.chip_area = Rect { width: (chip_text(app).width() as u16).min(head.width), height: 1, ..head };

    // List
    if app.view.is_empty() {
        let msg = if !app.filter.is_empty() {
            tr!(" 没有匹配「{}」的会话", " nothing matches “{}”", app.filter)
        } else if let Some(p) = app.status {
            let (key, none) = p.label();
            match app.hidden {
                0 => tr!(" {none} · {key} 返回", " {none} · {key} back"),
                n => tr!(" {none} · 还有 {n} 个更早的 · * 放宽", " {none} · {n} more, older · * widens"),
            }
        } else if app.open {
            match app.hidden {
                0 => t!(" 暂无未停止的会话 · w 返回", " nothing open · w back").into(),
                n => tr!(" 暂无未停止的会话 · 还有 {n} 个更早的 · * 放宽", " nothing open · {n} more, older · * widens"),
            }
        } else {
            t!(" tmux 里没有会话", " no sessions in tmux").into()
        };
        f.render_widget(Paragraph::new(msg).style(dim()), list_area);
    } else {
        let sel = app.list.selected();
        let sel_row = match app.head {
            Some(_) => rows.iter().position(|r| r.head.is_some() && r.head == sel),
            None => rows.iter().position(|r| r.pos.is_some() && r.pos == sel),
        };
        app.disp.select(sel_row);
        let items: Vec<ListItem> = rows
            .into_iter()
            .zip(ticked)
            .map(|(r, on)| {
                let lines: Vec<Line> = r
                    .lines
                    .into_iter()
                    .map(|segs| Line::from(segs.into_iter().map(|(t, s)| Span::styled(t, s)).collect::<Vec<_>>()))
                    .collect();
                let item = ListItem::new(Text::from(lines));
                if on { item.style(Style::new().bg(theme::pal().pick)) } else { item }
            })
            .collect();
        // The selected item gets a light purple background, keeping each part's color (state and account still readable at a glance). Keep
        // one item of padding: scrolling to a group's first session also shows the section header above it
        let list = List::new(items).highlight_style(Style::new().bg(theme::pal().sel)).scroll_padding(1);
        f.render_stateful_widget(list, list_area, &mut app.disp);
    }

    // Preview: the bottom of the highlighted session's screen. On the right it's the only box on the screen (accent-colored, title set in
    // the top border); below, it's an accent-colored line with the title in it, which also separates list and preview
    let spot = if app.on_group_row() { app.spot() } else { None };
    let shown = if app.on_group_row() { None } else { preview_name(app).and_then(|n| app.all.iter().position(|s| s.name == n)) };
    if let Some(pa) = prev_area {
        let (ic, title, label) = match shown.map(|i| &app.all[i]) {
            Some(s) => {
                // Clip the account name when too long; the title comes first
                let label = clip(&s.agent_label(), (pa.width as usize / 3).max(12)).trim_end().to_string();
                let label = if label.is_empty() { vec![] } else { vec![Span::styled(label, agent_style(s.agent))] };
                match &s.subagent {
                    Some(a) => (agent_icon(a.phase), s.title.clone(), vec![Span::styled(t!("子代理", "subagent"), dim())]),
                    None => (session_icon(s), s.title.clone(), label),
                }
            }
            None => (("▸", dim()), app.current().map(|s| group_label(&s.group).to_string()).unwrap_or_default(), vec![]),
        };
        // When little of the list shows (mostly preview / list only), the line says which number the highlighted one is
        let at = Span::styled(format!("{}/{}", app.list.selected().map_or(0, |p| p + 1), app.view.len()), dim());
        let label = if side == Some(Side::Focus) { [label, vec![Span::raw("  "), at.clone()]].concat() } else { label };
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
            let t = clip(&title, (pa.width as usize).saturating_sub(12 + label.iter().map(|s| s.content.width()).sum::<usize>()).max(6));
            // A faint box: no big block of strong color standing next to the list; the title stays dark bold (without a foreground color it
            // would take the border's color)
            let bold = Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD);
            let mut block =
                Block::bordered().border_type(BorderType::Rounded).border_style(theme::faint()).padding(Padding::horizontal(1)).title_top(
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled(ic.0, ic.1),
                        Span::raw(" "),
                        Span::styled(t.trim_end().to_string(), bold),
                        Span::raw(" "),
                    ]),
                );
            if !label.is_empty() {
                block = block.title_top(Line::from([vec![Span::raw(" ")], label, vec![Span::raw(" ")]].concat()).right_aligned());
            }
            let inner = block.inner(pa);
            f.render_widget(block, pa);
            inner
        } else {
            f.render_widget(Paragraph::new(title_rule(ic, &title, label, pa.width as usize)), Rect { height: 1, ..pa });
            Rect { x: pa.x + 1, y: pa.y + 1, width: pa.width.saturating_sub(2), height: pa.height.saturating_sub(1) }
        };
        match shown {
            _ if inner.height == 0 => {}
            None => {
                // Group: if its note is being written, show what's being written.
                // A group without a directory yet (empty group) has only the note
                let key = app.group_row().map(group_key);
                let editing = app.prompt.as_ref().and_then(|p| match &p.ask {
                    Ask::Note { key: k, .. } if Some(k) == key.as_ref() => Some(p.text.as_str()),
                    _ => None,
                });
                let note = editing.or_else(|| key.as_ref().and_then(|k| app.notes.get(k)));
                let (w, h) = (inner.width as usize, inner.height as usize);
                let lines = match &spot {
                    Some(sp) => group_pane(sp, app.glances.get(&group_key(&sp.group)), note, editing.is_some(), w, h),
                    None => note.map(|t| group_note(t, editing.is_some(), w, h, 0)).unwrap_or_default(),
                };
                f.render_widget(Paragraph::new(lines), inner);
            }
            Some(i) => {
                let s = &app.all[i];
                // This session's note is being written: show what's being written
                let editing = app.prompt.as_ref().and_then(|p| match &p.ask {
                    Ask::Note { key, .. } if *key == notes::key(s) => Some(p.text.as_str()),
                    _ => None,
                });
                let note = editing.or_else(|| app.notes.get(&notes::key(s)));
                let mut head =
                    preview_head(s, app.glances.get(&s.name), note, editing.is_some(), inner.width as usize, inner.height as usize);
                if let Some(l) = agents_line(s, app.agents_open.contains(&s.name), inner.width as usize) {
                    head.insert(1.min(head.len()), l);
                }
                let hh = (head.len() as u16).min(inner.height);
                f.render_widget(Paragraph::new(head), Rect { height: hh, ..inner });
                let body = Rect { y: inner.y + hh, height: inner.height - hh, ..inner };
                let lines = ansi::wrap(&preview_lines(s), body.width as usize);
                let h = body.height as usize;
                // Show the bottom of the screen (latest at the bottom), moved up by what the wheel scrolled back; a subagent that has
                // reported back shows from the top (its report comes first)
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

    // Bottom bar: key hints, or a message shown briefly.
    // New, rename, confirm, tool pick and keys are all popups, on top of this whole layer
    let w = foot.width as usize;
    let layout_strip = app.layout_at.filter(|t| t.elapsed() < Duration::from_secs(3)).map(|_| {
        let mut v = vec![Span::styled(t!("布局  ", "layout  "), dim())];
        for (i, (p, name)) in layouts(narrow(full.width)).into_iter().enumerate() {
            if i > 0 {
                v.push(Span::styled(" → ", dim()));
            }
            v.push(if p == app.mode() { Span::styled(format!(" {name} "), theme::chip()) } else { Span::styled(name, dim()) });
        }
        Line::from(v).centered()
    });
    let doing = app.doing();
    let foot_line = match &app.msg {
        _ if note_in_preview => menu_bar(
            &[vec![("Esc", t!("保存", "done"), 0), ("⏎", t!("换行", "newline"), 0), ("^G", t!("编辑器", "editor"), 1)]],
            Some(0),
            w,
        ),
        // q / Q pressed: more urgent than the hint before
        _ if app.pending.is_some() => pending_bar(app, w),
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ if term_area.is_some() => menu_bar(&[vec![("Esc", t!("收起", "hide"), 0), ("exit", t!("关闭", "close"), 0)]], Some(0), w),
        _ if layout_strip.is_some() => layout_strip.unwrap_or_default(),
        // Multi-selecting: more urgent than background work (the keys pressed are all here)
        _ if app.picking() && !app.filtering => pick_bar(app, w),
        _ if doing.is_some() => Line::styled(doing.clone().unwrap_or_default(), theme::warn()).centered(),
        _ if app.filtering => {
            menu_bar(&[vec![("⏎", t!("确定", "ok"), 0), ("↑↓", t!("选择", "select"), 0), ("Esc", t!("清除", "clear"), 0)]], Some(0), w)
        }
        _ if app.jump.is_some() => {
            Line::styled(t!("按字母打开或定位到分组 · Esc 取消", "press a letter to open / land on a group · Esc cancels"), theme::warn())
                .centered()
        }
        // Picking (`aca ui --ref`, or the reference key here): only the keys that work here; started here, where it goes
        _ if app.insert_to.is_some() => {
            let into = app.insert_back.as_ref().map(|(_, t)| tr!("插入到「{}」", "insert into “{}”", clip(t, 12).trim_end()));
            let enter = match () {
                _ if app.head.is_some() => t!("折叠", "fold"),
                _ if app.on_fold_row() => t!("展开", "unfold"),
                _ => into.as_deref().unwrap_or(t!("插入", "insert")),
            };
            menu_bar(&[vec![("⏎", enter, 0), ("Esc", t!("取消", "cancel"), 0)], vec![("/", t!("搜索", "search"), 3)]], Some(0), w)
        }
        _ => {
            // Three groups: manage sessions │ act on the highlighted one (main actions) │ view. Only the most used, the rest are in ?; when
            // it doesn't fit, drop the later ones first, ? and ⏎ always stay. On a subagent row: only view its record or go back to its
            // session; the session-management keys aren't listed
            let on_agent = app.head.is_none() && app.current().is_some_and(|s| s.subagent.is_some());
            let enter = match () {
                _ if app.head.is_some() => t!("折叠", "fold"),
                _ if app.on_fold_row() => t!("展开", "unfold"),
                _ if on_agent => t!("查看记录", "record"),
                // Following: it's already beside
                _ if app.following.is_some() => t!("留下", "stay"),
                _ => t!("打开", "open"),
            };
            let mut act: Vec<MenuItem> = vec![];
            if !app.filter.is_empty() {
                act.push(("Esc", t!("清除", "clear"), 0));
            } else if app.following.is_some() && app.status.is_none() {
                act.push(("Esc", t!("返回", "go back"), 0));
            }
            act.push(("⏎", enter, 0));
            if app.view.iter().any(|&i| waiting(&app.all[i])) {
                act.push(("i", t!("下一个", "next"), 1));
            }
            if on_agent {
                act.push(("h", t!("返回会话", "its session"), 6));
            } else {
                act.push(("e", t!("便签", "note"), 6));
            }
            // s only when there's a note
            if !app.on_group_row() && !on_agent && app.current().is_some_and(|s| s.noted) {
                act.push(("s", t!("发送", "send"), 6));
            }
            menu_bar(
                &[
                    if on_agent { vec![] } else { vec![("n", t!("新建", "new"), 2)] },
                    act,
                    vec![("/", t!("搜索", "search"), 3), ("v", t!("多选", "select"), 4), ("?", t!("快捷键", "keys"), 0)],
                ],
                Some(1),
                w,
            )
        }
    };
    f.render_widget(Paragraph::new(foot_line), foot);
    if let (Some(key), Some(ta)) = (app.term_on.clone(), term_area) {
        draw_term(f, app, &key, ta);
    }

    // Popups
    if app.reader.as_ref().is_some_and(|r| r.help) {
        draw_help(f, app);
    } else if let Some(d) = &app.new_draft {
        draw_draft(f, d);
    } else if let Some(p) = app.prompt.as_ref().filter(|_| !note_in_preview) {
        draw_prompt(f, p);
    } else if let Some((menu, sel)) = &app.tool_menu {
        let set = match app.tool_special {
            true => Set::Special { back: app.tool_back.is_some(), relogin: app.tool_relogin },
            false => Set::Here(!app.tool_more.is_empty()),
        };
        let logins = app.tool_logins.lock().unwrap_or_else(|e| e.into_inner()).clone();
        draw_tools(f, menu, *sel, app.handoff.as_ref(), &app.tool_quotas, &logins, set);
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
    } else if let Some(sel) = app.group_menu {
        draw_groups(f, app, sel);
    } else if let Some(sel) = app.only_menu {
        draw_only(f, app, sel);
    } else if let Some(m) = &app.theme_menu {
        draw_themes(f, m);
    } else if let Some(st) = &app.settings {
        draw_settings(f, st);
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
        Some(_) => vec![("Esc", t!("确定", "keep"), 0)],
        None => vec![
            ("v", t!("连选", "range"), 0),
            ("V", t!("连续取消", "range unselect"), 1),
            (t!("空格", "Space"), if on { t!("取消选中", "unselect") } else { t!("选中", "select") }, 1),
            ("Esc", t!("清空选择", "clear"), 0),
        ],
    };
    let used: usize = head.iter().map(|s| s.content.width()).sum();
    let mut l = menu_bar_in(&[acts, tail], Some(0), w.saturating_sub(used), theme::pal().green);
    l.spans.splice(0..0, head);
    l
}

/// After q / Q, waiting for the next key: the bottom bar lists the workspaces you can go to (not h, nor the current one)
fn pending_bar(app: &App, w: usize) -> Line<'static> {
    let names: Vec<(String, String)> =
        app.space_list().into_iter().filter(|&c| c != app.space).map(|c| (c.to_string(), app.spaces.name(c))).collect();
    let mut items: Vec<MenuItem> = names.iter().enumerate().map(|(i, (k, n))| (k.as_str(), n.as_str(), (i + 1).min(200) as u8)).collect();
    let (what, tail): (&str, Vec<MenuItem>) = match app.pending {
        Some(Pending::Send) => (t!("移到", "Send to"), vec![("Esc", t!("取消", "cancel"), 0)]),
        _ => (t!("工作区", "Workspace"), vec![("q", t!("退出", "quit"), 0), ("Esc", t!("取消", "cancel"), 0)]),
    };
    if items.is_empty() {
        items = tail;
        let mut l = menu_bar(&[items], None, w.saturating_sub(what.width() + 2));
        l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
        return l;
    }
    let mut l = menu_bar(&[items, tail], Some(0), w.saturating_sub(what.width() + 2));
    l.spans.insert(0, Span::styled(format!("{what}  "), theme::warn()));
    l
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
            let key_st = if c == app.space { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut line = menu_row(&c.to_string(), key_st, &shown(c), name_w, &heads);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("切换", "switch")), ("^R", t!("重命名", "rename"))], vec![], w));
        out
    });
}

/// A row of the workspace / group list: key, name, how many sessions, count per state
fn menu_row(key: &str, key_st: Style, name: &str, name_w: usize, heads: &[&Session]) -> Line<'static> {
    let mut spans = vec![
        Span::styled(key.to_string(), key_st),
        Span::raw("  "),
        Span::styled(clip(name, name_w), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {:>3}", heads.len()), dim()),
    ];
    for st in [State::Ask, State::Running, State::Unread, State::Error] {
        let n = heads.iter().filter(|s| s.state == st).count();
        if n > 0 {
            let (ic, sty) = icon(st);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(ic, sty));
            spans.push(Span::raw(n.to_string()));
        }
    }
    Line::from(spans)
}

/// Tab: this workspace's group list. One per row: letter (␣ for all groups), group name, how many sessions, count per state; the viewed
/// group's letter in accent color, the highlighted row gets a background
fn draw_groups(f: &mut Frame, app: &App, sel: usize) {
    let rows = app.group_rows();
    let name = |g: &Option<String>| g.as_deref().map_or(t!("全部", "all"), group_label).to_string();
    // Write group names in full (a long one widens the box), else groups with the same start get clipped to look alike
    let name_w = rows.iter().map(|(_, g)| name(g).width()).max().unwrap_or(0).min(40);
    modal(f, 48 + name_w.saturating_sub(20) as u16, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("分组", "Groups"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        for (i, (c, g)) in rows.iter().enumerate() {
            let heads: Vec<&Session> = app
                .all
                .iter()
                .filter(|s| s.space == app.space && s.sub.is_none() && !s.blank && g.as_ref().is_none_or(|g| &s.group == g))
                .collect();
            let key_st = if *g == app.group { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let key = if g.is_none() { "␣".to_string() } else { c.to_string() };
            let mut line = menu_row(&key, key_st, &name(g), name_w, &heads);
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("切换", "switch")), ("␣", t!("全部", "all"))], vec![], w));
        out
    });
}

/// A state's icon (as in the list) and name (as in the top bar) in `F`'s small list
fn pick_look(p: Pick) -> (Vec<(&'static str, Style)>, &'static str) {
    match p {
        Pick::Run => (vec![icon(State::Running)], t!("运行中", "running")),
        Pick::Wait => (vec![icon(State::Ask), icon(State::Unread)], t!("等待回应 · 未读", "needs you · unread")),
        Pick::Idle => (vec![icon(State::Idle)], t!("空闲", "idle")),
        Pick::Err => (vec![icon(State::Error)], t!("出错", "error")),
        Pick::Off => (vec![icon(State::Exited)], t!("已停止", "stopped")),
        Pick::Reaped => (vec![(aca_core::status::IDLE_STOPPED, Style::new().fg(theme::pal().muted))], t!("自动停止", "auto-stopped")),
    }
}

/// F: small list to view only one state. One state per row: first the key that picks it directly (the picked one in accent color), icon,
/// name, how many in this workspace; row 0 is all (Space). The highlighted row gets a background
fn draw_only(f: &mut Frame, app: &App, sel: usize) {
    modal(f, 44, theme::pal().accent, |w| {
        let mut out = vec![modal_title(t!("按状态筛选", "Only"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let row = |key: &str, on: bool, icons: Vec<(&'static str, Style)>, name: &str, n: usize| -> Line<'static> {
            let key_st = if on { theme::accent().add_modifier(Modifier::BOLD) } else { key_style() };
            let mut spans = vec![Span::styled(key.to_string(), key_st), Span::raw("  ")];
            let iw: usize = icons.iter().map(|(ic, _)| ic.width()).sum();
            spans.extend(icons.into_iter().map(|(ic, st)| Span::styled(ic, st)));
            spans.push(Span::raw(" ".repeat(4usize.saturating_sub(iw))));
            spans.push(Span::styled(name.to_string(), Style::new().add_modifier(Modifier::BOLD)));
            spread(spans, vec![Span::styled(n.to_string(), dim())], w)
        };
        let mut rows = vec![row("␣", app.status.is_none(), vec![], t!("全部", "all"), app.pick_count(None))];
        for p in PICKS {
            let (icons, name) = pick_look(p);
            rows.push(row(&p.label().0.to_string(), app.status == Some(p), icons, name, app.pick_count(Some(p))));
        }
        for (i, mut line) in rows.into_iter().enumerate() {
            if i == sel {
                let pad = w.saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(pad)));
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("筛选", "only")), ("␣", t!("全部", "all"))], vec![], w));
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

/// A popup's backdrop and frame: the whole layer below goes grey (text faint grey, background and weight removed), on desktop a ░ shadow
/// offset one cell down-right, the panel cleared and given a rounded border (accent color normally, red for delete). Returns the inside of
/// the panel (minus border and padding)
pub(super) fn modal_frame(f: &mut Frame, w: u16, h: u16, border: Color) -> Rect {
    let full = f.area();
    let (r, float) = modal_area(full, w, h);
    let pal = theme::pal();
    let buf = f.buffer_mut();
    for y in full.top()..full.bottom() {
        for x in full.left()..full.right() {
            buf[(x, y)].set_style(Style::reset().fg(pal.fade));
        }
    }
    if float {
        let mut shade = |x: u16, y: u16| {
            if x < full.right() && y < full.bottom() {
                split_wide(buf, x, y);
                buf[(x, y)].set_symbol("░").set_fg(pal.shadow);
            }
        };
        for y in r.y + 1..=r.bottom() {
            shade(r.right(), y);
        }
        for x in r.x + 1..r.right() {
            shade(x, r.bottom());
        }
    }
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
