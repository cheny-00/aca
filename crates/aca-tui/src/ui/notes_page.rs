//! A group's notes page. Inside a group the middle column has two pages: its sessions, and the notes in its notebook
//! (book.rs). Tab goes from one to the other, and a group comes back on the page it was left on. On the notes page each note
//! is a row (pinned ones first, then the most recently changed) and the right column shows the one under the cursor in full
//! — or the one being written, as it's typed.

use aca_core::store::Note;

use super::*;

impl App {
    /// Which page the middle column shows: the sessions outside a group, and in one that was never turned to another
    pub(super) fn page(&self) -> Page {
        self.group.as_ref().and_then(|g| self.pages.get(g)).copied().unwrap_or(Page::Sessions)
    }

    /// Turn a group to this page
    pub(super) fn show_page(&mut self, group: &str, page: Page) {
        match page {
            Page::Sessions => self.pages.remove(group),
            page => self.pages.insert(group.to_string(), page),
        };
    }

    /// The group being viewed shows its notes page
    pub(super) fn on_notes(&self) -> bool {
        self.page() == Page::Notes
    }

    /// The middle column isn't the list of sessions: the group being viewed shows another of its pages
    pub(super) fn off_list(&self) -> bool {
        self.page() != Page::Sessions
    }

    /// The group being viewed turns to its sessions page (what's about to be done works on sessions)
    pub(super) fn show_sessions(&mut self) {
        if let Some(g) = self.group.clone() {
            self.show_page(&g, Page::Sessions);
            self.digits.clear();
        }
    }

    /// The cursor is among the notes: the notes page shows, and the cursor isn't out on the rail
    pub(super) fn in_notes(&self) -> bool {
        self.on_notes() && !self.on_rail()
    }

    /// The notes the page lists
    pub(super) fn notes_listed(&self) -> Vec<&Note> {
        self.group.as_ref().map(|g| self.book.of(g).collect()).unwrap_or_default()
    }

    /// Which of them the cursor is on
    pub(super) fn note_pos(&self) -> Option<usize> {
        let v = self.notes_listed();
        (!v.is_empty()).then(|| self.note_at.as_ref().and_then(|id| v.iter().position(|n| n.id == *id)).unwrap_or(0))
    }

    pub(super) fn note_cur(&self) -> Option<&Note> {
        self.note_pos().and_then(|p| self.notes_listed().into_iter().nth(p))
    }

    /// Tab: the group's next page, round and round: tasks, sessions, notes
    pub(super) fn flip_page(&mut self) {
        let Some(g) = self.group.clone() else { return };
        let next = match self.page() {
            Page::Tasks => Page::Sessions,
            Page::Sessions => Page::Notes,
            Page::Notes => Page::Tasks,
        };
        self.show_page(&g, next);
        self.digits.clear();
    }

    pub(super) fn note_step(&mut self, d: isize) {
        let ids: Vec<String> = self.notes_listed().iter().map(|n| n.id.clone()).collect();
        let Some(p) = self.note_pos() else { return };
        self.note_at = ids.get((p as isize + d).clamp(0, ids.len() as isize - 1) as usize).cloned();
    }

    /// A number typed: the cursor goes to the note with that number (a digit that goes on no number starts a new one, as in
    /// the list)
    fn note_number(&mut self, c: char) {
        if self.digits_at.elapsed() > DIGIT_GAP {
            self.digits.clear();
        }
        self.digits_at = Instant::now();
        self.digits.push(c);
        let ids: Vec<String> = self.notes_listed().iter().map(|n| n.id.clone()).collect();
        let at = |d: &str| d.parse::<usize>().ok().filter(|n| (1..=ids.len()).contains(n));
        if at(&self.digits).is_none() {
            self.digits = c.to_string();
        }
        match at(&self.digits) {
            Some(n) => self.note_at = Some(ids[n - 1].clone()),
            None => self.digits.clear(),
        }
    }

    /// p: pin the note under the cursor, or unpin it. It moves to where such ones are listed, the cursor with it
    fn note_pin(&mut self) {
        let Some((id, on, name)) = self.note_cur().map(|n| (n.id.clone(), !n.pinned, n.name().to_string())) else { return };
        let name = clip(&name, 16).trim_end().to_string();
        match self.book.pin(&id, on) {
            Ok(()) => {
                self.note_at = Some(id);
                self.flash(&match on {
                    true => tr!("已置顶「{name}」", "pinned “{name}”"),
                    false => tr!("已取消置顶「{name}」", "unpinned “{name}”"),
                });
            }
            Err(e) => self.flash(&tr!("保存失败:{e}", "not saved: {e}")),
        }
    }

    /// d: ask before deleting the note under the cursor
    fn ask_note_gone(&mut self) {
        if let Some(n) = self.note_cur() {
            self.confirm = Some(Confirm::Note { id: n.id.clone(), name: n.name().to_string() });
        }
    }

    /// y in that box: delete it; the cursor goes to the one after it (the one before, at the end). ^Z brings it back, as it
    /// undoes an edit: by writing its text again under its key
    pub(super) fn note_gone(&mut self, id: &str, name: &str) {
        let key = notes::note_key(id);
        let Some(text) = self.book.text(&key).map(String::from) else { return };
        let ids: Vec<String> = self.notes_listed().iter().map(|n| n.id.clone()).collect();
        let at = ids.iter().position(|x| x == id);
        let next = at.and_then(|p| ids.get(p + 1).or_else(|| p.checked_sub(1).and_then(|q| ids.get(q)))).cloned();
        match self.book.bin(id) {
            Ok(()) => {
                self.note_at = next;
                self.remember(Undo::Note { key, title: name.to_string(), text });
                self.apply_pins();
                self.flash(&tr!("已删除笔记「{}」 · ^Z 撤销", "deleted note “{}” · ^Z undoes", clip(name, 16).trim_end()));
            }
            Err(e) => self.flash(&tr!("删除失败:{e}", "delete failed: {e}")),
        }
    }

    /// e: rewrite the note under the cursor; n: start a new one. It's written in the right column (in a box where there's
    /// none), saved as it's typed
    fn note_write(&mut self, new: bool) {
        let Some(g) = self.group.clone() else { return };
        let (key, title, text) = match self.note_cur().filter(|_| !new) {
            Some(n) => (notes::note_key(&n.id), n.name().to_string(), n.text.clone()),
            None if new => (group_key(&g), self.shown(&g).into_owned(), String::new()),
            None => return,
        };
        self.prompt = Some(Prompt { ask: Ask::Note { key, title, orig: text.clone() }, text, hint: String::new() });
        // The bottom bar now belongs to the writing
        self.msg = None;
    }

    /// A key with the cursor on the notes page. False = not taken here: the keys that mean the same wherever the cursor is.
    /// Keys that act on a session do nothing: no session is under the cursor
    pub(super) fn on_notes_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            // ^G writes the note under the cursor in the editor; the other Ctrl keys are the list's
            _ if k.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Tab => self.flip_page(),
            KeyCode::Up | KeyCode::Char('k') => self.note_step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.note_step(1),
            KeyCode::PageUp => self.note_step(-5),
            KeyCode::PageDown => self.note_step(5),
            KeyCode::Home | KeyCode::Char('g') => self.note_step(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.note_step(10_000),
            KeyCode::Char('e') => self.note_write(false),
            KeyCode::Char('n') => self.note_write(true),
            KeyCode::Char('p') => self.note_pin(),
            KeyCode::Char('d') => self.ask_note_gone(),
            KeyCode::Char('?') => self.open_sheet(&notes_keys()),
            KeyCode::Char(c) if c.is_ascii_digit() => self.note_number(c),
            // Out of the group: onto the rail (one is laid over a screen with none), Esc back to all groups where there's none
            KeyCode::Left | KeyCode::Char('h') => self.out_to_rail(),
            KeyCode::Esc => match rail_shown(self.screen) {
                true => self.out_to_rail(),
                false => self.set_group(None),
            },
            // The next one waiting, and a search, are among the sessions: over to that page first
            KeyCode::Char('i' | '/') => {
                self.show_sessions();
                return false;
            }
            // Space shows the groups' letters (no session is listed here to have one), ` too for a row of the rail; o is this
            // group's router
            KeyCode::Char('a' | ' ' | 'o' | '`' | 'q' | 'S' | 'b' | '$' | 'M' | 'P' | 'L') => return false,
            _ => {}
        }
        true
    }

    /// A click on the notes page: the cursor goes to that note
    pub(super) fn note_click(&mut self, row: u16) {
        if let Some((.., id)) = self.note_hits.iter().find(|(y, h, _)| (*y..*y + *h).contains(&row)) {
            self.rail = None;
            self.note_at = Some(id.clone());
        }
    }
}

/// The notes page's keys, for `?`
fn notes_keys() -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    vec![
        key("e", t!("编辑", "edit")),
        key("n", t!("新笔记", "new note")),
        key("p", t!("置顶 / 取消", "pin / unpin")),
        key("d", t!("删除", "delete")),
        key("Tab", t!("换页", "next page")),
        key("a", t!("到全部并跳转", "all groups, jump")),
        more("↑↓ jk", t!("选择", "pick")),
        more(t!("数字", "digits"), t!("按编号选择", "select by number")),
        more("^G", t!("用编辑器写", "write in the editor")),
        more("^Z", t!("撤销", "undo")),
        more("h", t!("到左栏", "out to the rail")),
    ]
}

/// The names of a group's pages with how many each lists, for the top row: the one showing is lit. Nothing outside a group
pub(super) fn page_cells(app: &App) -> Vec<Span<'static>> {
    let Some(g) = &app.group else { return vec![] };
    let sessions = app.view.iter().filter(|&&i| counted(&app.all[i])).count();
    let page = app.page();
    // The tasks cell counts the tickets still to finish
    let cells = [
        (t!("任务", "tasks"), app.tickets.open(g), page == Page::Tasks),
        (t!("会话", "sessions"), sessions, page == Page::Sessions),
        (t!("笔记", "notes"), app.book.of(g).count(), page == Page::Notes),
    ];
    let mut v = vec![];
    for (name, n, lit) in cells {
        if !v.is_empty() {
            v.push(Span::raw("   "));
        }
        v.extend(crate::kit::tab(name, n, lit));
    }
    v
}

/// The notes page's bottom bar: the keys for the note under the cursor on the left, the ones that work anywhere on the right
pub(super) fn notes_bar(app: &App, w: usize) -> Line<'static> {
    let mut left: Vec<MenuItem> = match app.note_cur() {
        Some(n) => vec![
            ("e", t!("编辑", "edit"), 0),
            ("n", t!("新笔记", "new note"), 0),
            ("p", if n.pinned { t!("取消置顶", "unpin") } else { t!("置顶", "pin") }, 6),
            ("d", t!("删除", "delete"), 7),
        ],
        None => vec![("n", t!("新笔记", "new note"), 0)],
    };
    left.push(("Tab", t!("换页", "page"), 5));
    let mut right: Vec<MenuItem> = vec![("a", t!("全部", "all"), 6)];
    if app.view.iter().any(|&i| waiting(&app.all[i])) {
        right.push(("i", t!("下一个", "next"), 5));
    }
    right.extend([("/", t!("搜索", "search"), 9), ("?", t!("快捷键", "keys"), 0)]);
    split_bar(&left, &right, w)
}

/// Who wrote a note, as its row says: you, or the session by its title (by its id once it's no longer listed)
fn note_by(app: &App, n: &Note) -> String {
    match n.by.as_str() {
        "" => t!("你", "you").into(),
        id => app.all.iter().find(|s| s.sub.is_none() && s.deck_id == id).map_or_else(|| id.to_string(), |s| s.title.clone()),
    }
}

/// The notes page in the middle column: two lines a note (its number, ★ if pinned, its name, who wrote it, how long since it
/// changed; then how it goes on), a blank line between them. Returns the rows of the cursor's, for the caller to mark as the
/// cursor's row — none while the cursor is out on the rail. `numbered`: the rows carry numbers, as the list's do on the desktop
pub(super) fn draw_notes(f: &mut Frame, app: &mut App, area: Rect, numbered: bool) -> Option<Rect> {
    let notes: Vec<Note> = app.notes_listed().into_iter().cloned().collect();
    app.note_hits.clear();
    if notes.is_empty() {
        let msg = t!(" 这一组还没有笔记 · n 新笔记", " no notes in this group yet · n writes one");
        f.render_widget(Paragraph::new(msg).style(dim()), area);
        return None;
    }
    let mut cursor = None;
    let pos = app.note_pos().unwrap_or(0);
    // Each takes two lines and the blank after it: as many as fit whole, the cursor's among them
    let fit = (area.height as usize).div_ceil(3).max(1);
    app.note_top = app.note_top.min(pos).max((pos + 1).saturating_sub(fit));
    let (w, now) = (area.width as usize, epoch());
    let mut y = area.y;
    for (i, n) in notes.iter().enumerate().skip(app.note_top) {
        if y + 2 > area.y + area.height {
            break;
        }
        let num = if numbered { format!("{:>3} ", i + 1) } else { " ".to_string() };
        let indent = num.width() + 2;
        let right = format!("{}  {:>4} ", clip(&note_by(app, n), 12).trim_end(), crate::print::age(now.saturating_sub(n.updated)));
        let name = clip(n.name(), w.saturating_sub(indent + right.width() + 2)).trim_end().to_string();
        let head = spread(
            vec![
                Span::styled(num, dim()),
                Span::styled(if n.pinned { "★ " } else { "  " }, theme::accent()),
                Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
            ],
            vec![Span::styled(right, dim())],
            w,
        );
        // How it goes on: what follows its name, run together on one line
        let rest = n.text.lines().map(str::trim).filter(|l| !l.is_empty()).skip(1).collect::<Vec<_>>().join(" ");
        let rest = clip(&rest, w.saturating_sub(indent + 1)).trim_end().to_string();
        let more = Line::from(vec![Span::raw(" ".repeat(indent)), Span::styled(rest, dim())]);
        let rows = Rect { y, height: 2, ..area };
        f.render_widget(Paragraph::new(vec![head, more]), rows);
        if i == pos && !app.on_rail() {
            cursor = Some(rows);
        }
        app.note_hits.push((y, 2, n.id.clone()));
        y += 3;
    }
    cursor
}

/// What the right column says of a note: the sign and name its title carries, and who wrote it and how long ago beside the
/// title. `writing`: one is being written — (the key it's written under, what's written so far); a new one has no name yet
pub(super) fn note_head(app: &App, writing: Option<&(String, String)>) -> ((&'static str, Style), String, Vec<Span<'static>>) {
    let note = match writing {
        Some((key, _)) => app.book.id_of(key).and_then(|id| app.book.get(id)),
        None => app.note_cur(),
    };
    let new = writing.is_some_and(|(key, _)| notes::note_of(key).is_none());
    let title = match note {
        _ if new => t!("新笔记", "new note").to_string(),
        Some(n) => n.name().to_string(),
        None => String::new(),
    };
    let by = match note {
        Some(n) => format!("{} · {}", note_by(app, n), crate::print::age(epoch().saturating_sub(n.updated))),
        None if new => t!("你", "you").to_string(),
        None => String::new(),
    };
    let sign = if note.is_some_and(|n| n.pinned) { ("★", theme::accent()) } else { ("✎", dim()) };
    (sign, title, if by.is_empty() { vec![] } else { vec![Span::styled(by, dim())] })
}

/// The right column's lines: the note under the cursor from below its name on; while one is written, all that's written so
/// far with the cursor at its end (its first line is its name)
pub(super) fn note_body(app: &App, writing: Option<&(String, String)>, w: usize) -> Vec<Line<'static>> {
    let text = match writing {
        Some((_, text)) => format!("{text}▏"),
        None => {
            let Some(n) = app.note_cur() else { return vec![] };
            let mut lines = n.text.lines().skip_while(|l| l.trim().is_empty());
            lines.next();
            lines.collect::<Vec<_>>().join("\n").trim_matches('\n').to_string()
        }
    };
    text.lines().flat_map(|l| if l.is_empty() { vec![String::new()] } else { wrap_width(l, w.max(1)) }).map(Line::raw).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{caps, ctrl, fixture, keys, press, reader_rows, said, select, shows, typ};

    /// A wide screen (a rail), in the group work on its notes page with three notes: 发版窗口 (pinned), then 口径, 回放数据
    fn on_notes() -> App {
        let mut app = fixture();
        app.screen = Rect::new(0, 0, 150, 40);
        for (text, pin) in [
            ("回放数据\n3 天的回放在 /data/replay", false),
            ("口径\n不到 130 列不画左栏。\n\n窄屏按 h 叫出来。", false),
            ("发版窗口\n周二、周四", true),
        ] {
            let id = app.book.add("work", text).unwrap().id;
            app.book.pin(&id, pin).unwrap();
        }
        app.set_group(Some("work".into()));
        press(&mut app, KeyCode::Tab);
        app
    }

    fn names(app: &App) -> Vec<String> {
        app.notes_listed().iter().map(|n| n.name().to_string()).collect()
    }

    fn cur(app: &App) -> String {
        app.note_cur().map(|n| n.name().to_string()).unwrap_or_default()
    }

    #[test]
    fn tab_in_a_group_goes_to_its_notes_and_back_and_the_group_comes_back_on_the_page_it_was_left_on() {
        let mut app = on_notes();
        assert!(app.in_notes());
        let rows = reader_rows(&mut app, 150, 40);
        // The top row names the pages, the one showing lit; each note is two lines; the right column has the cursor's in full
        shows(
            &rows,
            &["任务 0   会话 3   笔记 3", "1 ★ 发版窗口", "周二、周四", "2   口径", "不到 130 列不画左栏。 窄屏按 h 叫出来。", "3   回放数据"],
        );
        shows(&rows, &["★ 发版窗口", "你 · "]);
        assert!(!rows.iter().any(|r| r.contains("○ 闲")), "{rows:#?}");
        let bar = ["e 编辑", "n 新笔记", "p 取消置顶", "d 删除", "Tab 换页", "a 全部"].map(caps);
        shows(&rows, &bar.each_ref().map(|s| s.as_str()));
        // Tab again: its tasks (none yet); once more: its sessions, the cursor where it was
        press(&mut app, KeyCode::Tab);
        assert!(app.on_tasks() && reader_rows(&mut app, 150, 40).iter().any(|r| r.contains("这一组还没有任务")));
        press(&mut app, KeyCode::Tab);
        let rows = reader_rows(&mut app, 150, 40);
        assert!(!app.on_notes() && rows.iter().any(|r| r.contains("闲")), "{rows:#?}");
        shows(&rows, &[&caps("Tab 换页")]);
        // Left on its notes page, gone elsewhere and come back: still its notes; another group starts on its sessions
        press(&mut app, KeyCode::Tab);
        app.set_group(Some("fun".into()));
        assert!(!app.on_notes());
        app.set_group(Some("work".into()));
        assert!(app.on_notes());
        // Tab means nothing in all groups
        app.set_group(None);
        press(&mut app, KeyCode::Tab);
        assert!(!app.on_notes() && app.group.is_none());
    }

    #[test]
    fn the_cursor_goes_from_note_to_note_and_the_right_column_follows() {
        let mut app = on_notes();
        assert_eq!((names(&app), cur(&app)), (vec!["发版窗口".to_string(), "口径".into(), "回放数据".into()], "发版窗口".into()));
        keys(&mut app, "j");
        assert_eq!(cur(&app), "口径");
        let rows = reader_rows(&mut app, 150, 40);
        // Its name is the title; below, what comes after the name, its blank line kept
        shows(&rows, &["✎ 口径", "不到 130 列不画左栏。", "窄屏按 h 叫出来。"]);
        keys(&mut app, "jjj");
        assert_eq!(cur(&app), "回放数据");
        keys(&mut app, "g");
        assert_eq!(cur(&app), "发版窗口");
        keys(&mut app, "3");
        assert_eq!(cur(&app), "回放数据");
        // Keys that act on a session do nothing here: none is under the cursor
        keys(&mut app, "sDRmx");
        assert!(app.prompt.is_none() && app.confirm.is_none() && app.reader.is_none() && cur(&app) == "回放数据");
        // h: out onto the rail, the notes still listed with none highlighted; Enter comes back in
        keys(&mut app, "h");
        assert!(app.on_rail() && app.on_notes() && !app.in_notes());
        press(&mut app, KeyCode::Enter);
        assert!(app.in_notes() && cur(&app) == "回放数据");
    }

    /// A key that acts on the list of sessions, pressed while the group's notes page is showing: its sessions come back
    /// first, so what the key does is on screen
    #[test]
    fn keys_for_the_list_of_sessions_bring_the_sessions_page_back() {
        // From the group's rail row: a search, a row's number
        for key in ['/', '2', 'w', 'i'] {
            let mut app = on_notes();
            keys(&mut app, "h");
            assert!(app.on_rail() && app.on_notes());
            press(&mut app, KeyCode::Char(key));
            assert!(!app.on_notes(), "{key}");
        }
        // From the notes page itself: the next one waiting and a search
        let mut app = on_notes();
        keys(&mut app, "/");
        assert!(!app.on_notes() && app.filtering);
        // W on a session in all groups: into its group, among its sessions, whichever page the group was left on
        let mut app = on_notes();
        app.set_group(None);
        select(&mut app, "闲");
        keys(&mut app, "W");
        assert_eq!((app.group.as_deref(), app.on_notes(), app.current().map(|s| s.title.as_str())), (Some("work"), false, Some("闲")));
        // a from the notes page: all groups with the letters showing; a again: back on the notes page, on the same note
        let mut app = on_notes();
        keys(&mut app, "jj");
        keys(&mut app, "a");
        assert!(app.group.is_none() && app.jump.is_some() && !app.on_notes());
        keys(&mut app, "a");
        assert!(app.in_notes() && cur(&app) == "回放数据", "{:?} {}", app.group, cur(&app));
    }

    /// Opened with e and left as it was: it wasn't changed, so it keeps its place in the list and nothing is there to undo
    #[test]
    fn a_note_opened_and_left_as_it_was_keeps_its_place() {
        let mut app = on_notes();
        let before = names(&app);
        keys(&mut app, "jj");
        keys(&mut app, "e");
        press(&mut app, KeyCode::Esc);
        assert_eq!((names(&app), cur(&app).as_str()), (before.clone(), "回放数据"));
        keys(&mut app, "e");
        typ(&mut app, "!");
        press(&mut app, KeyCode::Esc);
        assert!(app.note_cur().is_some_and(|n| n.text.ends_with('!')));
        // One ^Z undoes that rewrite (the look that changed nothing left no step of its own)
        ctrl(&mut app, 'z');
        assert!(app.note_cur().is_some_and(|n| n.text == "回放数据\n3 天的回放在 /data/replay"));
    }

    #[test]
    fn p_pins_and_unpins_and_d_deletes_after_asking_and_ctrl_z_brings_it_back() {
        let mut app = on_notes();
        keys(&mut app, "jj");
        keys(&mut app, "p");
        // Pinned ones come first; the cursor went up with it
        assert_eq!(names(&app), ["发版窗口", "回放数据", "口径"]);
        assert_eq!((cur(&app).as_str(), said(&app)), ("回放数据", "已置顶「回放数据」"));
        shows(&reader_rows(&mut app, 150, 40), &["2 ★ 回放数据"]);
        assert_eq!(crate::ui::tests::beside(&mut app, 150, 40)[0], "★ 回放数据");
        keys(&mut app, "p");
        assert_eq!((names(&app), cur(&app).as_str()), (vec!["发版窗口".to_string(), "口径".into(), "回放数据".into()], "回放数据"));
        // d asks; any key but y leaves it
        keys(&mut app, "d");
        shows(&reader_rows(&mut app, 150, 40), &["删除笔记？", "回放数据"]);
        keys(&mut app, "n");
        assert_eq!((names(&app).len(), said(&app)), (3, "未删除"));
        keys(&mut app, "dy");
        assert_eq!(names(&app), ["发版窗口", "口径"]);
        assert!(said(&app).starts_with("已删除笔记「回放数据」"), "{}", said(&app));
        // The cursor is on the one that took its place (here the last one, so the one before)
        assert_eq!(cur(&app), "口径");
        ctrl(&mut app, 'z');
        assert_eq!((names(&app).len(), cur(&app).as_str()), (3, "回放数据"));
    }

    /// A note deleted with d is on the trash page, with the deleted sessions and groups, and Enter there brings it back
    #[test]
    fn a_deleted_note_is_on_the_trash_page_and_enter_there_brings_it_back() {
        let mut app = on_notes();
        keys(&mut app, "jj");
        let id = app.note_cur().unwrap().id.clone();
        keys(&mut app, "d");
        shows(&reader_rows(&mut app, 150, 40), &["删除笔记？", "移入回收站，保留 1 天 · b 恢复"]);
        keys(&mut app, "yb");
        assert!(app.modal_open());
        shows(&reader_rows(&mut app, 150, 40), &["回收站", " ✎ 回放数据  work", "⏎ 恢复"]);
        shows(&reader_rows(&mut app, 46, 30), &["回收站", " ✎ 回放数据  work"]);
        press(&mut app, KeyCode::Enter);
        assert!(!app.modal_open());
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Restore { item, .. } if item.note.as_ref().is_some_and(|n| n.id == id && n.group == "work")));
        // What the job does in the registry; done, the note is listed again with the cursor on it
        keys(&mut app, "g");
        app.book.unbin(&id).unwrap();
        app.job_done(&job, Outcome::Done("已恢复笔记「回放数据」".into()), crate::ui::tests::sessions());
        assert_eq!((names(&app).len(), cur(&app).as_str(), said(&app)), (3, "回放数据", "已恢复笔记「回放数据」"));
        keys(&mut app, "b");
        shows(&reader_rows(&mut app, 150, 40), &["回收站为空"]);
    }

    #[test]
    fn e_rewrites_the_note_under_the_cursor_and_n_starts_a_new_one_in_the_right_column() {
        let mut app = on_notes();
        keys(&mut app, "j");
        keys(&mut app, "e");
        assert_eq!(app.prompt.as_ref().unwrap().text, "口径\n不到 130 列不画左栏。\n\n窄屏按 h 叫出来。");
        typ(&mut app, "!");
        let rows = reader_rows(&mut app, 150, 40);
        // Written in the right column, the cursor at the end; the bottom bar has only the writing's keys
        shows(&rows, &["窄屏按 h 叫出来。!▏", &caps("Esc 保存")]);
        press(&mut app, KeyCode::Esc);
        assert_eq!((said(&app), cur(&app).as_str()), ("笔记已保存", "口径"));
        assert!(app.note_cur().unwrap().text.ends_with("叫出来。!"));
        // n: a new one. It's in the list from its first character on, the cursor on it; its first line is its name
        keys(&mut app, "n");
        shows(&reader_rows(&mut app, 150, 40), &["新笔记", "笔记 3"]);
        typ(&mut app, "冻结 alert.yaml");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "周四 18:00 之后");
        let rows = reader_rows(&mut app, 150, 40);
        shows(&rows, &["笔记 4", "冻结 alert.yaml", "周四 18:00 之后▏"]);
        assert_eq!(cur(&app), "冻结 alert.yaml");
        press(&mut app, KeyCode::Esc);
        assert_eq!(names(&app).len(), 4);
        // Started and left empty: no note
        keys(&mut app, "n");
        press(&mut app, KeyCode::Esc);
        assert_eq!(names(&app).len(), 4);
        // ^Z takes the new one back out
        ctrl(&mut app, 'z');
        assert_eq!(names(&app).len(), 3);
    }

    #[test]
    fn a_group_with_no_notes_says_so_and_n_writes_the_first() {
        let mut app = fixture();
        app.screen = Rect::new(0, 0, 150, 40);
        app.set_group(Some("fun".into()));
        press(&mut app, KeyCode::Tab);
        let rows = reader_rows(&mut app, 150, 40);
        shows(&rows, &["笔记 0", "这一组还没有笔记 · n 新笔记"]);
        keys(&mut app, "epd");
        assert!(app.prompt.is_none() && app.confirm.is_none());
        keys(&mut app, "n");
        typ(&mut app, "第一条");
        press(&mut app, KeyCode::Esc);
        assert_eq!(names(&app), ["第一条"]);
    }
}
