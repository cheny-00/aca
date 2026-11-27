//! A group's tasks page: its task tickets (tickets.rs), one a row of two lines — its state, title and number, then the
//! one thing that matters most about it now. The done ones come first, in the order they were finished, then the rest by
//! number; the cursor starts on the first still to finish. The right column shows the ticket under the cursor in full,
//! with the session doing it below.
//!
//! What waits for what is the group's router's to work out (aca-core's task.rs): a ticket's line is drawn as written.

use aca_core::store::{Task, TaskState};

use super::*;

impl App {
    /// The group being viewed shows its tasks page
    pub(super) fn on_tasks(&self) -> bool {
        self.page() == Page::Tasks
    }

    /// The cursor is among the tickets: the tasks page shows, and the cursor isn't out on the rail
    pub(super) fn in_tasks(&self) -> bool {
        self.on_tasks() && !self.on_rail()
    }

    /// The tickets the page lists
    pub(super) fn tasks_listed(&self) -> Vec<&Task> {
        self.group.as_ref().map(|g| self.tickets.of(g)).unwrap_or_default()
    }

    /// Which of them the cursor is on. Never put anywhere (or on one that's gone): the first still to finish; with all of
    /// them done, the last
    pub(super) fn task_pos(&self) -> Option<usize> {
        let v = self.tasks_listed();
        let first = || v.iter().position(|t| t.state() != TaskState::Done).unwrap_or(v.len().saturating_sub(1));
        (!v.is_empty()).then(|| self.task_at.and_then(|n| v.iter().position(|t| t.n == n)).unwrap_or_else(first))
    }

    pub(super) fn task_cur(&self) -> Option<&Task> {
        self.task_pos().and_then(|p| self.tasks_listed().into_iter().nth(p))
    }

    /// A registered session by its id, among the ones read (its own row, not a window inside it)
    pub(super) fn session_of(&self, id: &str) -> Option<&Session> {
        self.all.iter().find(|s| !id.is_empty() && s.deck_id == id && s.sub.is_none() && s.subagent.is_none())
    }

    /// The session doing the ticket under the cursor, while it has one that's still there
    pub(super) fn task_doer(&self) -> Option<&Session> {
        self.task_cur().and_then(|t| self.session_of(&t.session))
    }

    pub(super) fn task_step(&mut self, d: isize) {
        let ns: Vec<i64> = self.tasks_listed().iter().map(|t| t.n).collect();
        let Some(p) = self.task_pos() else { return };
        self.task_at = ns.get((p as isize + d).clamp(0, ns.len() as isize - 1) as usize).copied();
    }

    /// A number typed: the cursor goes to the row with that number (a digit that goes on no number starts a new one, as in
    /// the list). It's the row's number, not the ticket's
    fn task_number(&mut self, c: char) {
        if self.digits_at.elapsed() > DIGIT_GAP {
            self.digits.clear();
        }
        self.digits_at = Instant::now();
        self.digits.push(c);
        let ns: Vec<i64> = self.tasks_listed().iter().map(|t| t.n).collect();
        let at = |d: &str| d.parse::<usize>().ok().filter(|n| (1..=ns.len()).contains(n));
        if at(&self.digits).is_none() {
            self.digits = c.to_string();
        }
        match at(&self.digits) {
            Some(n) => self.task_at = Some(ns[n - 1]),
            None => self.digits.clear(),
        }
    }

    /// Where in `all` the session doing the ticket under the cursor is
    fn task_doer_at(&self) -> Option<usize> {
        let name = self.task_doer().map(|s| s.name.clone())?;
        self.all.iter().position(|s| s.name == name)
    }

    /// ⏎: into the session doing the ticket under the cursor
    fn task_open(&mut self) {
        match self.task_doer_at() {
            Some(i) => self.go(i),
            None => self.flash(t!("还没有会话在做 · m 交给", "no session has it yet · m hands it over")),
        }
    }

    /// s: say something to the session doing it, or answer what it asks, as on that session's own row
    fn task_reply(&mut self) {
        match self.task_doer_at() {
            Some(i) => self.reply_to(i),
            None => self.flash(t!("还没有会话在做 · m 交给", "no session has it yet · m hands it over")),
        }
    }

    /// n: a new ticket in this group; e: the one under the cursor rewritten. Its title is asked first, then what counts as
    /// done
    fn task_write(&mut self, new: bool) {
        let Some(group) = self.group.clone() else { return };
        let (n, text) = match self.task_cur().filter(|_| !new) {
            Some(t) => (Some(t.n), t.title.clone()),
            None if new => (None, String::new()),
            None => return,
        };
        self.prompt = Some(Prompt { ask: Ask::TaskTitle { n, group }, text, hint: String::new() });
        self.msg = None;
    }

    /// What the page does to a ticket runs in the background (it passes word to sessions): one thing at a time
    fn task_job(&mut self, act: TaskAct) {
        if let Some(b) = self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)) {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        self.job = Some(Job::Task(act));
    }

    /// c: mark the ticket under the cursor done
    fn task_done(&mut self) {
        let Some((n, done)) = self.task_cur().map(|t| (t.n, t.state() == TaskState::Done)) else { return };
        match done {
            true => self.flash(&tr!("任务 #{n} 已经完成", "task #{n} is done already")),
            false => self.task_job(TaskAct::Done { n }),
        }
    }

    /// d: ask before deleting the ticket under the cursor
    fn ask_task_gone(&mut self) {
        if let Some(t) = self.task_cur() {
            self.confirm = Some(Confirm::Task { n: t.n, title: format!("{}  #{}", t.title, t.n) });
        }
    }

    /// m: whom the ticket under the cursor can go to — the group's own sessions, and nobody while a session has it. The
    /// highlight starts on the one doing it now
    fn open_give(&mut self) {
        let Some(t) = self.task_cur() else { return };
        let mine = |s: &&Session| s.group == t.group && s.sub.is_none() && s.subagent.is_none() && !s.blank && !s.deck_id.is_empty();
        let mut rows: Vec<(String, String)> = self.all.iter().filter(mine).map(|s| (s.deck_id.clone(), s.title.clone())).collect();
        if t.state() == TaskState::Doing {
            rows.push((String::new(), t!("取回", "take it back").into()));
        }
        if rows.is_empty() {
            return self.flash(t!("这一组还没有会话 · N 新建", "no session in this group yet · N starts one"));
        }
        let sel = rows.iter().position(|(id, _)| !id.is_empty() && *id == t.session).unwrap_or(0);
        self.give_menu = Some(GiveMenu { n: t.n, sel, rows });
    }

    /// In that list: ↑↓ j k pick, ⏎ hands the ticket to the highlighted one, a row's number does it directly; Esc / q / m close
    pub(super) fn on_give_key(&mut self, k: KeyEvent) {
        let Some(m) = &mut self.give_menu else { return };
        let n = m.rows.len();
        let pick = match k.code {
            KeyCode::Down | KeyCode::Char('j') => {
                m.sel = (m.sel + 1) % n;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                m.sel = (m.sel + n - 1) % n;
                return;
            }
            KeyCode::Enter => Some(m.sel),
            KeyCode::Char(c @ '1'..='9') => Some(c as usize - '1' as usize).filter(|i| *i < n),
            KeyCode::Esc | KeyCode::Char('q' | 'm') => None,
            _ => return,
        };
        let Some(m) = self.give_menu.take() else { return };
        if let Some((to, name)) = pick.and_then(|i| m.rows.into_iter().nth(i)) {
            self.task_job(TaskAct::Give { n: m.n, to, name });
        }
    }

    /// A key with the cursor on the tasks page. False = not taken here: the keys that mean the same wherever the cursor
    /// is. Keys that act on a session do nothing: a ticket is under the cursor, not a session
    pub(super) fn on_tasks_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            _ if k.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Tab => self.flip_page(),
            KeyCode::Enter => self.task_open(),
            KeyCode::Char('s') => self.task_reply(),
            KeyCode::Char('n') => self.task_write(true),
            KeyCode::Char('e') => self.task_write(false),
            KeyCode::Char('m') => self.open_give(),
            KeyCode::Char('c') => self.task_done(),
            KeyCode::Char('d') => self.ask_task_gone(),
            KeyCode::Up | KeyCode::Char('k') => self.task_step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.task_step(1),
            KeyCode::PageUp => self.task_step(-5),
            KeyCode::PageDown => self.task_step(5),
            KeyCode::Home | KeyCode::Char('g') => self.task_step(-10_000),
            KeyCode::End | KeyCode::Char('G') => self.task_step(10_000),
            KeyCode::Char('?') => self.open_sheet(&tasks_keys()),
            KeyCode::Char(c) if c.is_ascii_digit() => self.task_number(c),
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

    /// A click on the tasks page: the cursor goes to that ticket
    pub(super) fn task_click(&mut self, row: u16) {
        if let Some(&(.., n)) = self.task_hits.iter().find(|(y, h, _)| (*y..*y + *h).contains(&row)) {
            self.rail = None;
            self.task_at = Some(n);
        }
    }

    /// Who a ticket names, as written on it: you, or the session by its title (by its id once it's no longer listed)
    fn task_who(&self, id: &str) -> String {
        match id {
            "" => t!("你", "you").into(),
            id => self.session_of(id).map_or_else(|| id.to_string(), |s| s.title.clone()),
        }
    }
}

/// `m`'s list: the ticket, the highlighted row, and whom it can go to — (a session's id, its title); an empty id takes the
/// ticket back
pub(super) struct GiveMenu {
    pub n: i64,
    pub sel: usize,
    pub rows: Vec<(String, String)>,
}

/// The tasks page's keys, for `?`
fn tasks_keys() -> Vec<crate::sheet::Key> {
    use crate::sheet::{key, more};
    vec![
        key("⏎", t!("打开做它的会话", "open the session doing it")),
        key("s", t!("回它 / 回答它", "reply to it / answer it")),
        key("n", t!("新任务", "new task")),
        key("e", t!("修改", "edit")),
        key("m", t!("交给", "hand over")),
        key("c", t!("完成", "done")),
        key("d", t!("删除", "delete")),
        key("Tab", t!("换页", "next page")),
        key("a", t!("到全部并跳转", "all groups, jump")),
        more("↑↓ jk", t!("选择", "pick")),
        more(t!("数字", "digits"), t!("按编号选择", "select by number")),
        more("h", t!("到左栏", "out to the rail")),
    ]
}

/// `m`'s list: a row for each session of the ticket's group (its number, its state, its title; the one doing the ticket
/// now says so), then one to take the ticket back
pub(super) fn draw_give(f: &mut Frame, app: &App, m: &GiveMenu) {
    let doing = app.tickets.get(m.n).map(|t| t.session.clone()).unwrap_or_default();
    modal(f, 52, theme::pal().accent, |w| {
        let title = tr!("交给 · #{}", "Hand over · #{}", m.n);
        let mut out = vec![modal_title(&title, theme::accent(), esc_hint(t!("取消", "cancel")), w), Line::raw("")];
        for (i, (id, name)) in m.rows.iter().enumerate() {
            let key = if i < 9 { (i + 1).to_string() } else { " ".into() };
            let (ic, st) = app.session_of(id).map_or(("·", dim()), session_icon);
            let now = if !id.is_empty() && *id == doing { t!("在做", "doing") } else { "" };
            let name = clip(name, w.saturating_sub(8 + now.width())).trim_end().to_string();
            let mut line = spread(
                vec![
                    Span::styled(key, key_style()),
                    Span::raw("  "),
                    Span::styled(format!("{ic} "), st),
                    Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
                ],
                vec![Span::styled(now, dim())],
                w,
            );
            if i == m.sel {
                line = line.patch_style(Style::new().bg(theme::pal().sel));
            }
            out.push(line);
        }
        out.push(Line::raw(""));
        out.push(keys_line(&[("⏎", t!("交给", "hand over")), ("↑↓", t!("选择", "select"))], vec![], w));
        out
    });
}

/// The tasks page's bottom bar: the keys for the ticket under the cursor on the left, the ones that work anywhere on the right
pub(super) fn tasks_bar(app: &App, w: usize) -> Line<'static> {
    let mut left: Vec<MenuItem> = vec![];
    if let Some(t) = app.task_cur() {
        if let Some(s) = app.task_doer() {
            left.push(("⏎", t!("打开", "open"), 0));
            left.push(("s", if s.state == State::Ask { t!("回答", "answer") } else { t!("回话", "reply") }, 0));
        }
        left.push(("m", t!("交给", "hand over"), 1));
        if t.state() != TaskState::Done {
            left.push(("c", t!("完成", "done"), 2));
        }
    }
    left.push(("n", t!("新任务", "new task"), 3));
    if app.task_cur().is_some() {
        left.extend([("e", t!("修改", "edit"), 6), ("d", t!("删除", "delete"), 7)]);
    }
    left.push(("Tab", t!("换页", "page"), 5));
    let mut right: Vec<MenuItem> = vec![("a", t!("全部", "all"), 6)];
    if app.view.iter().any(|&i| waiting(&app.all[i])) {
        right.push(("i", t!("下一个", "next"), 5));
    }
    right.extend([("/", t!("搜索", "search"), 9), ("?", t!("快捷键", "keys"), 0)]);
    split_bar(&left, &right, w)
}

/// A ticket's state as its row shows it: ✓ done, · waiting for someone; one being done has the mark of the session doing
/// it, as that session's own row has (a session that's gone: waiting)
fn task_icon(app: &App, t: &Task) -> (&'static str, Style) {
    match (t.state(), app.session_of(&t.session)) {
        (TaskState::Done, _) => ("✓", dim()),
        (TaskState::Doing, Some(s)) => session_icon(s),
        _ => ("·", dim()),
    }
}

/// How long since what last happened to a ticket: it was finished, the session doing it did something, it was made
fn task_age(app: &App, t: &Task, now: u64) -> u64 {
    let at = match (t.state(), app.session_of(&t.session)) {
        (TaskState::Done, _) => t.done,
        (TaskState::Doing, Some(s)) if s.active_at > 0 => s.active_at,
        (TaskState::Doing, _) => t.given,
        (TaskState::Todo, _) => t.created,
    };
    now.saturating_sub(at)
}

/// The one line under a ticket's title: what matters most about it now. Done: who did it and the result. Being done:
/// what its session asks you, else the ticket's line, else what was said handing it over. Waiting: its line, else what
/// counts as done, else who made it
fn task_line(app: &App, t: &Task) -> String {
    let doer = || app.task_who(&t.session);
    let join = |a: String, b: &str| if b.is_empty() { a } else { format!("{a}  {b}") };
    match (t.state(), app.session_of(&t.session)) {
        (TaskState::Done, _) if t.session.is_empty() => t.result.clone(),
        (TaskState::Done, _) => join(doer(), &t.result),
        (TaskState::Doing, Some(s)) if s.state == State::Ask && !s.question.is_empty() => {
            tr!("{} 问:{}", "{} asks: {}", s.title, s.question)
        }
        (TaskState::Doing, _) if !t.note.is_empty() => t.note.clone(),
        (TaskState::Doing, _) if !t.say.is_empty() => format!("{} ❯ {}", doer(), t.say),
        (TaskState::Doing, _) => doer(),
        (TaskState::Todo, _) if !t.note.is_empty() => t.note.clone(),
        (TaskState::Todo, _) if !t.criteria.is_empty() => t.criteria.clone(),
        (TaskState::Todo, _) => app.task_who(&t.by),
    }
}

/// The tasks page in the middle column: two lines a ticket (its row's number, its state, its title and #number, how long
/// since something happened to it; then the line of `task_line`). Returns the rows of the cursor's, for the caller to mark
/// as the cursor's row — none while the cursor is out on the rail. Tickets scrolled off are counted at the top (↑ 5) and
/// the bottom. `numbered`: the rows carry numbers, as the list's do on the desktop
pub(super) fn draw_tasks(f: &mut Frame, app: &mut App, area: Rect, numbered: bool) -> Option<Rect> {
    let tasks: Vec<Task> = app.tasks_listed().into_iter().cloned().collect();
    app.task_hits.clear();
    if tasks.is_empty() {
        let msg = t!(" 这一组还没有任务", " no tasks in this group yet");
        f.render_widget(Paragraph::new(msg).style(dim()), area);
        return None;
    }
    let mut cursor = None;
    let pos = app.task_pos().unwrap_or(0);
    // Not looked at yet: the cursor's ticket second, the one finished last above it
    if app.task_at.is_none() {
        app.task_top = pos.saturating_sub(1);
    }
    // Each takes two lines; a first row saying how many are above takes one more
    let rows = |top: usize| ((area.height as usize).saturating_sub(usize::from(top > 0)) / 2).max(1);
    app.task_top = app.task_top.min(pos);
    while pos >= app.task_top + rows(app.task_top) {
        app.task_top += 1;
    }
    let (w, now, top) = (area.width as usize, epoch(), app.task_top);
    let mut y = area.y;
    if top > 0 {
        f.render_widget(Paragraph::new(format!("   ↑ {top}")).style(dim()), Rect { y, height: 1, ..area });
        y += 1;
    }
    let shown = rows(top).min(tasks.len() - top);
    for (i, t) in tasks.iter().enumerate().skip(top).take(shown) {
        let num = if numbered { format!("{:>3} ", i + 1) } else { " ".to_string() };
        let (ic, ic_st) = task_icon(app, t);
        let indent = num.width() + 2;
        let right = format!("{:>4} ", crate::print::age(task_age(app, t, now)));
        let tag = format!("  #{}", t.n);
        let done = t.state() == TaskState::Done;
        let title = clip(&t.title, w.saturating_sub(indent + tag.width() + right.width() + 1)).trim_end().to_string();
        let head = spread(
            vec![
                Span::styled(num, dim()),
                Span::styled(format!("{ic} "), ic_st),
                Span::styled(title, if done { dim() } else { Style::new().add_modifier(Modifier::BOLD) }),
                Span::styled(tag, dim()),
            ],
            vec![Span::styled(right, dim())],
            w,
        );
        let line = clip(&task_line(app, t), w.saturating_sub(indent + 1)).trim_end().to_string();
        let more = Line::from(vec![Span::raw(" ".repeat(indent)), Span::styled(line, dim())]);
        let h = 2.min(area.y + area.height - y);
        let rows = Rect { y, height: h, ..area };
        f.render_widget(Paragraph::new(vec![head, more]), rows);
        if i == pos && !app.on_rail() {
            cursor = Some(rows);
        }
        app.task_hits.push((y, h, t.n));
        y += 2;
    }
    let below = tasks.len() - top - shown;
    if below > 0 && y < area.y + area.height {
        f.render_widget(Paragraph::new(format!("   ↓ {below}")).style(dim()), Rect { y, height: 1, ..area });
    }
    cursor
}

/// What the right column's title says of the ticket under the cursor: its state's mark and its title, and beside the
/// title its number and its state in words
pub(super) fn task_head(app: &App) -> ((&'static str, Style), String, Vec<Span<'static>>) {
    let Some(t) = app.task_cur() else { return (("·", dim()), String::new(), vec![]) };
    let state = match t.state() {
        TaskState::Todo => t!("待办", "todo"),
        TaskState::Doing => t!("在做", "doing"),
        TaskState::Done => t!("完成", "done"),
    };
    (task_icon(app, t), t.title.clone(), vec![Span::styled(format!("#{} · {state}{}", t.n, t.watch_label()), dim())])
}

/// The right column's lines about the ticket under the cursor, above the session doing it: its line, what counts as done,
/// who handed it to whom (how long ago) and what they said, and the result once it's done. A blank line ends them when a
/// session is drawn below
pub(super) fn task_block(app: &App, w: usize) -> Vec<Line<'static>> {
    let Some(t) = app.task_cur() else { return vec![] };
    let now = epoch();
    let wrap = |text: &str, lead: usize| -> Vec<String> {
        text.lines().flat_map(|l| if l.is_empty() { vec![String::new()] } else { wrap_width(l, w.saturating_sub(lead).max(1)) }).collect()
    };
    let mut v: Vec<Line<'static>> = vec![];
    v.extend(wrap(&t.note, 0).into_iter().map(Line::raw));
    if !t.criteria.is_empty() {
        let label = t!("完成标准", "done when");
        let lead = label.width() + 2;
        for (i, l) in wrap(&t.criteria, lead).into_iter().enumerate() {
            let head = if i == 0 { Span::styled(format!("{label}  "), dim()) } else { Span::raw(" ".repeat(lead)) };
            v.push(Line::from(vec![head, Span::raw(l)]));
        }
    }
    if !t.session.is_empty() {
        let who = format!("{} → {}", app.task_who(&t.given_by), app.task_who(&t.session));
        let age = crate::print::age(now.saturating_sub(t.given));
        let who = clip(&who, w.saturating_sub(age.width() + 2)).trim_end().to_string();
        v.push(spread(vec![Span::styled(who, dim())], vec![Span::styled(age, dim())], w));
        v.extend(wrap(&t.say, 2).into_iter().map(|l| Line::raw(format!("  {l}"))));
    } else if t.state() == TaskState::Todo {
        let age = crate::print::age(now.saturating_sub(t.created));
        v.push(spread(vec![Span::styled(app.task_who(&t.by), dim())], vec![Span::styled(age, dim())], w));
    }
    if t.state() == TaskState::Done {
        let age = crate::print::age(now.saturating_sub(t.done));
        let mut lines = wrap(&t.result, 2).into_iter();
        let first = lines.next().unwrap_or_default();
        let first = clip(&first, w.saturating_sub(age.width() + 4)).trim_end().to_string();
        v.push(spread(vec![Span::styled("✓ ", dim()), Span::raw(first)], vec![Span::styled(age, dim())], w));
        v.extend(lines.map(|l| Line::raw(format!("  {l}"))));
    }
    if app.task_doer().is_some() {
        v.push(Line::default());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{caps, fixture, keys, mk, press, reader_rows, said, sessions, shows, typ};

    fn ticket(n: i64, group: &str, title: &str) -> Task {
        Task { n, group: group.into(), title: title.into(), created: epoch() - 3600, ..Task::default() }
    }

    /// A wide screen (a rail); the group work has two more sessions (构建失败 running, 改登录页 asking) and five tickets: #1
    /// done by 构建失败, #2 being done by 改登录页, #3 waiting with a line, #5 being done by 构建失败 with words said
    /// handing it over, #6 waiting. #4 is in another group; fun has none
    fn with_tasks() -> App {
        let mut v = sessions();
        v.push(mk("构建失败", "work", State::Running, 5));
        v.push(Session { question: "kubectl apply -f alert.yaml".into(), ..mk("改登录页", "work", State::Ask, 120) });
        let mut app = App::new(String::new(), v);
        app.screen = Rect::new(0, 0, 150, 40);
        let (build, login) = ("id-构建失败".to_string(), "id-改登录页".to_string());
        let now = epoch();
        let done = Task { session: build.clone(), given: now - 7200, done: now - 3600, result: "回放 3 天,丢点 0".into(), ..ticket(1, "work", "ingest 丢点修复") };
        let asks = Task {
            session: login.clone(),
            given_by: build.clone(),
            given: now - 3000,
            say: "这块你昨天改过".into(),
            criteria: "造一次延迟,告警响;发版窗口里不响".into(),
            ..ticket(2, "work", "告警规则上测试集群")
        };
        let waits = Task { note: "等 #2:改的是同一份 alert.yaml".into(), by: build.clone(), ..ticket(3, "work", "告警阈值做成可配置的") };
        let other = ticket(4, "elsewhere", "别的组的");
        let doing = Task { session: build, given: now - 1800, say: "面板加三张:延迟、丢点、告警".into(), ..ticket(5, "work", "面板加三张图") };
        let plain = Task { criteria: "磁盘用到 90% 也告警".into(), ..ticket(6, "work", "磁盘快满也告警") };
        for t in [done, asks, waits, other, doing, plain] {
            app.tickets.put(t);
        }
        app
    }

    /// The middle column of a wide frame's rows
    fn mid(rows: &[String]) -> Vec<String> {
        rows.iter().map(|r| r.chars().skip(32).take(58).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn a_group_with_tickets_to_finish_opens_on_its_tasks_and_tab_goes_round_its_three_pages() {
        let mut app = with_tasks();
        // Down the rail: fun has no ticket and opens on its sessions, as before; work has tickets to finish, so its tasks show
        keys(&mut app, "hj");
        assert_eq!((app.group.as_deref(), app.page()), (Some("fun"), Page::Sessions));
        keys(&mut app, "j");
        assert_eq!((app.group.as_deref(), app.page()), (Some("work"), Page::Tasks));
        // Tab: tasks, sessions, notes, and round again; the top row names the three with what each holds
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("任务 4   会话 ") && r.contains("   笔记 0")), "{rows:#?}");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.page(), Page::Sessions);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.page(), Page::Notes);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.page(), Page::Tasks);
        // Left on its sessions, it still opens on its tasks next time: there's work to look at
        press(&mut app, KeyCode::Tab);
        app.set_group(None);
        app.set_group(Some("work".into()));
        assert_eq!(app.page(), Page::Tasks);
    }

    #[test]
    fn the_tasks_page_lists_the_done_ones_first_and_the_cursor_starts_on_the_first_to_finish() {
        let mut app = with_tasks();
        app.set_group(Some("work".into()));
        app.rail = None;
        let rows = mid(&reader_rows(&mut app, 150, 40));
        let at = |want: &str| rows.iter().position(|r| r.contains(want)).unwrap_or_else(|| panic!("缺「{want}」:{rows:#?}"));
        // Done first, then by number; each with its row's number, its mark, its title and its own number
        assert!(at("1 ✓ ingest 丢点修复  #1") < at("2 ◆ 告警规则上测试集群  #2"), "{rows:#?}");
        assert!(at("告警规则上测试集群") < at("3 · 告警阈值做成可配置的  #3") && at("#3") < at("面板加三张图  #5") && at("#5") < at("5 · 磁盘快满也告警  #6"));
        assert!(!rows.iter().any(|r| r.contains("别的组的")));
        // The line under each: the result, what its session asks, its line, what was said handing it over, what counts as done
        assert!(rows[at("#1") + 1].contains("构建失败  回放 3 天,丢点 0"), "{rows:#?}");
        assert!(rows[at("#2") + 1].contains("改登录页 问:"), "{rows:#?}");
        assert!(rows[at("#3") + 1].contains("等 #2:改的是同一份 alert.yaml"));
        assert!(rows[at("#5") + 1].contains("构建失败 ❯ 面板加三张:延迟、丢点、告警"));
        assert!(rows[at("#6") + 1].contains("磁盘用到 90% 也告警"));
        // The cursor: the first not done
        assert_eq!(app.task_cur().map(|t| t.n), Some(2));
        // j k, a row's number, g G; a click
        keys(&mut app, "jj");
        assert_eq!(app.task_cur().map(|t| t.n), Some(5));
        keys(&mut app, "k1");
        assert_eq!(app.task_cur().map(|t| t.n), Some(1));
        keys(&mut app, "G");
        assert_eq!(app.task_cur().map(|t| t.n), Some(6));
        keys(&mut app, "g5");
        assert_eq!(app.task_cur().map(|t| t.n), Some(6));
        let (y, ..) = app.task_hits[1];
        app.task_click(y + 1);
        assert_eq!(app.task_cur().map(|t| t.n), Some(2));
        // i and / are about sessions: the group's sessions page comes back for them
        keys(&mut app, "/");
        assert_eq!(app.page(), Page::Sessions);
    }

    #[test]
    fn the_column_beside_the_tasks_page_is_the_ticket_under_the_cursor_with_its_session_below() {
        let mut app = with_tasks();
        app.set_group(Some("work".into()));
        app.rail = None;
        let rows = reader_rows(&mut app, 150, 40);
        // Its title carries the ticket's mark, title, number and state, and the line under it names the session doing it;
        // under the rule what counts as done, who handed it to whom and what they said; then that session, as the column
        // shows a session
        let head = |app: &mut App| crate::ui::tests::beside(app, 150, 40)[..3].to_vec();
        let titled = |head: &[String], title: &str, state: &str| head[0].starts_with(title) && head[0].ends_with(state);
        let h = head(&mut app);
        assert!(titled(&h, "◆ 告警规则上测试集群", "#2 · 在做") && h[1] == "改登录页" && h[2].starts_with("───"), "{h:#?}");
        shows(&rows, &["完成标准  造一次延迟,告警响;发版窗口里不响", "构建失败 → 改登录页", "这块你昨天改过"]);
        // A waiting one: nobody is doing it, the rule is right under its title; its line, who made it; nothing below
        keys(&mut app, "j");
        let rows = reader_rows(&mut app, 150, 40);
        let h = head(&mut app);
        assert!(titled(&h, "· 告警阈值做成可配置的", "#3 · 待办") && h[1].starts_with("───"), "{h:#?}");
        shows(&rows, &["等 #2:改的是同一份 alert.yaml", "构建失败"]);
        // A done one: the result
        keys(&mut app, "g");
        let rows = reader_rows(&mut app, 150, 40);
        assert!(titled(&head(&mut app), "✓ ingest 丢点修复", "#1 · 完成"), "{:#?}", head(&mut app));
        shows(&rows, &["✓ 回放 3 天,丢点 0"]);
        // The cursor out on the rail's row for the group: the page stays in the middle with no cursor, the column beside is the
        // group's own
        keys(&mut app, "h");
        let rows = reader_rows(&mut app, 150, 40);
        assert!(mid(&rows).iter().any(|r| r.contains("告警规则上测试集群  #2")) && !rows.iter().any(|r| r.contains("#1 · 完成")), "{rows:#?}");
    }

    #[test]
    fn the_page_makes_rewrites_hands_over_finishes_and_deletes_tickets_in_the_background() {
        let mut app = with_tasks();
        app.set_group(Some("work".into()));
        app.rail = None;
        // What the page asked for, taken as the run loop takes it, and counted as finished
        let job = |app: &mut App| {
            let got = app.take_job();
            app.busy = None;
            match got {
                Some(Job::Task(a)) => Some(a),
                _ => None,
            }
        };
        // n: its title (not an empty one), then what counts as done, which may be left out
        keys(&mut app, "n⏎");
        assert!(matches!(app.prompt.as_ref().map(|p| (&p.ask, p.hint.as_str())), Some((Ask::TaskTitle { n: None, .. }, "名字不能为空"))));
        typ(&mut app, "写文档");
        keys(&mut app, "⏎");
        assert!(matches!(app.prompt.as_ref().map(|p| &p.ask), Some(Ask::TaskCriteria { n: None, title, .. }) if title == "写文档"));
        typ(&mut app, "README 有一节");
        keys(&mut app, "⏎");
        let made = TaskAct::Add { group: "work".into(), title: "写文档".into(), criteria: "README 有一节".into() };
        assert_eq!((job(&mut app), app.prompt.is_none()), (Some(made), true));
        keys(&mut app, "n");
        typ(&mut app, "别的");
        keys(&mut app, "⏎⏎");
        assert_eq!(job(&mut app), Some(TaskAct::Add { group: "work".into(), title: "别的".into(), criteria: String::new() }));
        // e: the ticket's own title and standard are in the boxes
        keys(&mut app, "e");
        assert_eq!(app.prompt.as_ref().map(|p| p.text.as_str()), Some("告警规则上测试集群"));
        typ(&mut app, "!");
        keys(&mut app, "⏎");
        assert_eq!(app.prompt.as_ref().map(|p| p.text.as_str()), Some("造一次延迟,告警响;发版窗口里不响"));
        keys(&mut app, "⏎");
        let set = TaskAct::Set { n: 2, title: "告警规则上测试集群!".into(), criteria: "造一次延迟,告警响;发版窗口里不响".into() };
        assert_eq!(job(&mut app), Some(set));
        // Esc in either box makes nothing
        keys(&mut app, "n⎋e⏎⎋");
        assert!(job(&mut app).is_none() && app.prompt.is_none());
        // c: done; not a second time
        keys(&mut app, "c");
        assert_eq!(job(&mut app), Some(TaskAct::Done { n: 2 }));
        keys(&mut app, "gc");
        assert!(job(&mut app).is_none() && said(&app).contains("已经完成"), "{}", said(&app));
        // d asks first: only y deletes
        keys(&mut app, "dn");
        assert!(job(&mut app).is_none() && app.confirm.is_none());
        keys(&mut app, "d");
        assert!(matches!(&app.confirm, Some(Confirm::Task { n: 1, title }) if title == "ingest 丢点修复  #1"));
        keys(&mut app, "y");
        assert_eq!(job(&mut app), Some(TaskAct::Remove { n: 1 }));
    }

    #[test]
    fn m_lists_the_groups_sessions_to_hand_a_ticket_to_and_enter_and_s_reach_the_one_doing_it() {
        let mut app = with_tasks();
        app.set_group(Some("work".into()));
        app.rail = None;
        // What the page asked for, taken as the run loop takes it, and counted as finished
        let job = |app: &mut App| {
            let got = app.take_job();
            app.busy = None;
            match got {
                Some(Job::Task(a)) => Some(a),
                _ => None,
            }
        };
        // m on a ticket being done: the group's sessions, the highlight on the one doing it, and a row to take it back
        keys(&mut app, "m");
        let m = app.give_menu.as_ref().unwrap();
        let titles: Vec<&str> = m.rows.iter().map(|r| r.1.as_str()).collect();
        assert_eq!((m.n, titles), (2, vec!["改登录页", "构建失败", "跑", "闲", "错", "取回"]));
        assert_eq!(m.rows[m.sel].1, "改登录页");
        let rows = reader_rows(&mut app, 150, 40);
        shows(&rows, &["交给 · #2", "1  ◆ 改登录页", "在做", "3  ● 跑", "6  · 取回"]);
        // ↓ and ⏎: the one below; a row's number picks it at once; the last row takes it back
        keys(&mut app, "j⏎");
        assert_eq!(job(&mut app), Some(TaskAct::Give { n: 2, to: "id-构建失败".into(), name: "构建失败".into() }));
        assert!(app.give_menu.is_none());
        keys(&mut app, "m4");
        assert_eq!(job(&mut app), Some(TaskAct::Give { n: 2, to: "id-闲".into(), name: "闲".into() }));
        keys(&mut app, "m6");
        assert_eq!(job(&mut app), Some(TaskAct::Give { n: 2, to: String::new(), name: "取回".into() }));
        // Esc hands it to nobody; a waiting ticket has no row to take it back
        keys(&mut app, "m⎋jm");
        assert!(job(&mut app).is_none());
        assert_eq!(app.give_menu.as_ref().map(|m| (m.n, m.rows.len(), m.sel)), Some((3, 5, 0)));
        keys(&mut app, "⎋");
        // ⏎ and s on a ticket nobody has: nothing to open, and the bar says how to hand it over
        keys(&mut app, "⏎");
        assert!(app.attach.is_none() && said(&app).contains("m 交给"), "{}", said(&app));
        keys(&mut app, "s");
        assert!(app.prompt.is_none() && app.confirm.is_none());
        app.msg = None;
        let b = bar_of(&mut app);
        assert!(b.contains(&caps("m 交给 • c 完成 • n 新任务")) && !b.contains(&caps("⏎ 打开")), "{b}");
        // On one a running session has: ⏎ goes into it, s writes to it
        keys(&mut app, "j");
        assert_eq!(app.task_cur().map(|t| t.n), Some(5));
        assert!(bar_of(&mut app).contains(&caps("⏎ 打开 • s 回话 • m 交给 • c 完成")));
        keys(&mut app, "s");
        assert!(matches!(&app.prompt.as_ref().unwrap().ask, Ask::Reply { t, .. } if t.name == "构建失败"));
        keys(&mut app, "⎋⏎");
        assert_eq!(app.attach.as_deref(), Some("构建失败"));
        // On one whose session asks: s answers it
        app.attach = None;
        keys(&mut app, "kk");
        assert_eq!(app.task_cur().map(|t| t.n), Some(2));
        assert!(bar_of(&mut app).contains(&caps("s 回答")));
        keys(&mut app, "s");
        assert!(matches!(&app.confirm, Some(Confirm::Answer { t, .. }) if t.name == "改登录页"));
    }

    #[test]
    fn a_sessions_row_names_its_ticket_and_a_groups_rail_row_says_how_many_of_its_tickets_are_done() {
        let mut app = with_tasks();
        app.tickets.put(Task { session: "id-闲".into(), done: epoch(), ..ticket(7, "work", "收尾") });
        // The rows take their tickets at a refresh
        app.apply_pins();
        let rows = reader_rows(&mut app, 150, 40);
        // The rail: done / all after the name of a group that has tickets, nothing after one that has none
        let rail: Vec<String> = rows.iter().map(|r| r.chars().take(30).collect()).collect();
        assert!(rail.iter().any(|r| r.contains("work  2/6")) && rail.iter().any(|r| r.contains("fun  ")), "{rail:#?}");
        assert!(!rail.iter().any(|r| r.contains("fun  0/")), "{rail:#?}");
        // A session's row: the ticket it's doing (not the one it finished before), or with none the one it finished last
        shows(&rows, &["改登录页  #2", "构建失败  #5", "闲  ✓ #7"]);
        assert!(!rows.iter().any(|r| r.contains("跑  #") || r.contains("跑  ✓")), "{rows:#?}");
        // `aca ui -l` prints the same rows
        let view: Vec<&Session> = app.view.iter().map(|&i| &app.all[i]).collect();
        let printed: Vec<String> =
            list_rows(&view, true, 60, None, 0, false).iter().map(|r| r.lines[0].iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert!(printed.iter().any(|r| r.contains("闲  ✓ #7")), "{printed:#?}");
    }

    /// The bottom row of a wide frame
    fn bar_of(app: &mut App) -> String {
        reader_rows(app, 150, 40).iter().rev().find(|r| !r.trim().is_empty()).cloned().unwrap_or_default()
    }

    #[test]
    fn o_and_backtick_work_from_the_tasks_page_and_the_notes_page_too() {
        let mut app = with_tasks();
        keys(&mut app, "hjjl");
        for page in [Page::Tasks, Page::Notes] {
            while app.page() != page {
                press(&mut app, KeyCode::Tab);
            }
            // o: this group's router; work has none yet, so the box for a new one in it, the cursor staying on the page
            press(&mut app, KeyCode::Char('o'));
            let d = app.new_draft.as_ref();
            assert!(d.is_some_and(|d| d.router && d.group_override.as_deref() == Some("work")), "{page:?}");
            press(&mut app, KeyCode::Esc);
            assert_eq!((app.new_draft.is_none(), app.on_rail(), app.group.as_deref(), app.page()), (true, false, Some("work"), page));
            // `: onto the rail with the letters showing, and ` again is back on the page
            press(&mut app, KeyCode::Char('`'));
            let rows = reader_rows(&mut app, 150, 40);
            assert!(app.letters.is_some() && app.on_rail() && rows.iter().any(|r| r.contains("W work")), "{page:?} {rows:#?}");
            press(&mut app, KeyCode::Char('`'));
            assert_eq!((app.letters.is_none(), app.on_rail(), app.group.as_deref(), app.page()), (true, false, Some("work"), page));
        }
    }

    #[test]
    fn a_page_longer_than_the_column_says_how_many_tickets_are_above_and_below() {
        let mut app = fixture();
        app.screen = Rect::new(0, 0, 150, 16);
        let now = epoch();
        for n in 1..=12 {
            let done = if n <= 6 { now - 100 + n as u64 } else { 0 };
            app.tickets.put(Task { done, session: if done > 0 { "x".into() } else { String::new() }, ..ticket(n, "work", &format!("第 {n} 张")) });
        }
        app.set_group(Some("work".into()));
        app.rail = None;
        // First shown: the cursor on the first to finish (#7), the last finished one above it, the five before counted
        let rows = mid(&reader_rows(&mut app, 150, 16));
        assert_eq!(app.task_cur().map(|t| t.n), Some(7));
        assert!(rows.iter().any(|r| r.trim() == "↑ 5") && rows.iter().any(|r| r.contains("第 6 张")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("第 5 张")) && rows.iter().any(|r| r.trim().starts_with("↓ ")), "{rows:#?}");
        // To the end: the last one shows, nothing counted below
        keys(&mut app, "G");
        let rows = mid(&reader_rows(&mut app, 150, 16));
        assert!(rows.iter().any(|r| r.contains("第 12 张")) && !rows.iter().any(|r| r.trim().starts_with("↓ ")), "{rows:#?}");
        // To the top: nothing counted above
        keys(&mut app, "g");
        let rows = mid(&reader_rows(&mut app, 150, 16));
        assert!(rows.iter().any(|r| r.contains("第 1 张")) && !rows.iter().any(|r| r.trim().starts_with("↑ ")), "{rows:#?}");
    }
}
