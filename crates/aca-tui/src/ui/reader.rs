//! Reading a session at length: the reader (`z`) over its screen and scrollback, the diff (`x`) of what it changed, the help
//! page, and the glance at a group's directory in the preview.

use super::*;

/// What to copy (the run loop fetches the content and writes the clipboard)
#[derive(Debug, PartialEq)]
pub enum Clip {
    /// c: the last reply; for a registered session the one the hook recorded, falling back to the text on screen
    Last { profile: String, id: String, title: String, screen: String },
    /// B on an unregistered or shell session (and `C`'s all): path / branch / session id / home
    Info { title: String, text: String },
    /// B on a registered agent session: the two-line reference another agent acts on (see `data::reference`)
    Ref { title: String, text: String },
    /// A piece of text handed back by a popup (`Fx::copy`): an address or such; the bottom bar says it as is
    Text(String),
}

/// Full-screen reading (`z`): doesn't go into the session, just shows its output, can scroll back through history
pub(super) struct Reader {
    /// tmux session name
    pub(super) name: String,
    /// History + screen, input box already removed
    pub(super) lines: Vec<Styled>,
    /// Rows from the bottom (after wrapping), 0 = follow the latest
    pub(super) scroll: usize,
    /// Cached wrap result, wrapped at wrap_w; dirty when the content changes
    pub(super) wrapped: Vec<Styled>,
    pub(super) wrap_w: usize,
    pub(super) dirty: bool,
    /// The key help opened by `?`, not a session
    pub(super) help: bool,
    /// Rows per screen when last drawn (the keys popup pages by this)
    pub(super) view_h: usize,
    /// `x`: showing changes (git diff), not output
    pub(super) diff: Option<DiffView>,
    /// The key just pressed in the keys popup: lights up its description row
    pub(super) hit: Option<crate::help::Hit>,
    /// Showing a plugin's output (a view, or a finished capability run): `r` reruns it
    pub(super) plugin: Option<crate::caps::Req>,
}

impl Reader {
    /// Which wrapped row is at the very top when a screen holds h rows (correct even if a key changed scroll before the redraw)
    fn top(&self, h: usize) -> usize {
        let total = self.wrapped.len();
        (total - self.scroll.min(total.saturating_sub(h))).saturating_sub(h)
    }
}

/// Full-screen reading switched to showing changes (see `diff`). git runs in the background in the run loop and `set_diff` sends the
/// result back when done
pub(super) struct DiffView {
    mode: Mode,
    /// Where to find the directory it really works in (see `workdir`)
    w: Where,
    /// Index in lines of each file's header, and its position after wrapping (n / p jump)
    files: Vec<usize>,
    rows: Vec<usize>,
    /// Which wrapped row each line of lines starts at (e finds which line is at the top of the screen)
    starts: Vec<usize>,
    /// (file number, line number) for each line, each file's path, repo root: see `diff::Diff`
    at: Vec<Option<(usize, usize)>>,
    paths: Vec<Option<String>>,
    top: String,
    /// No result yet
    loading: bool,
    /// Needs a reread: just opened / mode changed (timed refresh is separate)
    pub(super) want: bool,
    /// When the session last had activity and when it was read, as of the last read: timed refresh only rereads on new activity /
    /// after GLANCE_EVERY (as `glance_request`)
    seen: Option<(u64, Instant)>,
}

impl App {
    /// Full-screen reader on the highlighted session: show the screen content we already have, the run loop fetches history next
    pub(super) fn open_reader(&mut self) {
        if self.on_group(t!("查看全文", "read in full")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let (name, lines) = (s.name.clone(), preview_lines(s));
        self.trace = None;
        self.reader = Some(Reader {
            name,
            lines,
            scroll: 0,
            wrapped: vec![],
            wrap_w: 0,
            dirty: true,
            help: false,
            view_h: 0,
            diff: None,
            hit: None,
            plugin: None,
        });
        self.reader_load = true;
    }

    /// `x`: full-screen diff of the highlighted session's directory, from the top; the mode carries over from last time
    pub(super) fn open_diff(&mut self) {
        if self.on_group(t!("查看改动", "view the diff")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let (name, w) = (s.name.clone(), crate::data::where_of(s));
        self.trace = None;
        let lines = vec![vec![ansi::Seg { text: t!(" 正在读取 git…", " reading git…").into(), style: dim(), link: None }]];
        let diff = DiffView {
            mode: self.diff_mode,
            w,
            files: vec![],
            rows: vec![],
            starts: vec![],
            at: vec![],
            paths: vec![],
            top: String::new(),
            loading: true,
            want: true,
            seen: None,
        };
        self.reader = Some(Reader {
            name,
            lines,
            scroll: usize::MAX,
            wrapped: vec![],
            wrap_w: 0,
            dirty: true,
            help: false,
            view_h: 0,
            diff: Some(diff),
            hit: None,
            plugin: None,
        });
    }

    /// Whether the diff needs a git run: just opened, mode switched, or the periodic refresh is due. Gives the run loop
    /// (session name, directory, mode)
    pub fn diff_request(&mut self, refreshed: bool) -> Option<(String, Where, Mode)> {
        let act = self.reader.as_ref().and_then(|r| self.all.iter().find(|s| s.name == r.name)).map_or(0, |s| s.active_at);
        let r = self.reader.as_mut()?;
        let d = r.diff.as_mut()?;
        let fresh = refreshed && d.seen.is_none_or(|(a, t)| a != act || t.elapsed() >= GLANCE_EVERY);
        if !(std::mem::take(&mut d.want) || fresh) {
            return None;
        }
        d.seen = Some((act, Instant::now()));
        Some((r.name.clone(), d.w.clone(), d.mode))
    }

    /// A finished diff. Dropped if the session / mode has changed since; left alone if same as last time (no re-wrapping)
    pub fn set_diff(&mut self, name: &str, mode: Mode, d: diff::Diff) {
        let Some(r) = self.reader.as_mut().filter(|r| r.name == name) else { return };
        let Some(v) = r.diff.as_mut().filter(|v| v.mode == mode) else { return };
        if !v.loading && r.lines == d.lines {
            return;
        }
        v.loading = false;
        v.files = d.files;
        v.at = d.at;
        v.paths = d.paths;
        v.top = d.top;
        r.lines = d.lines;
        r.dirty = true;
    }

    /// Whether to read the diff stats at the top of the preview: the highlighted session changed; on a periodic refresh it has new
    /// activity (running: the pane has output; idle: a hook reported something, see `Session::active_at`), or GLANCE_EVERY passed.
    /// Counting on every refresh would mean five git processes every two seconds while sitting on a big repository.
    /// Not read while the preview is off or a full-screen view is open
    pub fn glance_request(&mut self, refreshed: bool) -> Option<(String, Where)> {
        if self.reader.is_some() || self.trace.is_some() || self.mode() == Preview::Off {
            return None;
        }
        if refreshed {
            self.spot = None;
        }
        // On a group: read the group's paths (key from group_key; by it the run loop also reads what's in the directories);
        // activity is the group's most recently active session
        let (key, w, act) = if self.on_group_row() {
            let sp = self.spot()?;
            let act = self.all.iter().filter(|s| s.group == sp.group && s.sub.is_none()).map(|s| s.active_at).max().unwrap_or(0);
            (group_key(&sp.group), Where { dir: sp.dir, ..Where::default() }, act)
        } else {
            let s = self.current().filter(|s| s.subagent.is_none())?;
            (s.name.clone(), crate::data::where_of(s), s.active_at)
        };
        let due = match &self.glance_for {
            Some((k, a, t)) if *k == key => refreshed && (*a != act || t.elapsed() >= GLANCE_EVERY),
            _ => true,
        };
        if !due {
            return None;
        }
        self.glance_for = Some((key.clone(), act, Instant::now()));
        Some((key, w))
    }

    pub fn set_glance(&mut self, name: String, g: Glance) {
        self.glances.insert(name, g);
    }

    /// Back from lazygit / the editor: the diff may have changed (committed, files changed), reread; say so if it didn't start
    pub fn external_done(&mut self, err: Option<String>) {
        if let Some(d) = self.reader.as_mut().and_then(|r| r.diff.as_mut()) {
            d.want = true;
        }
        self.glance_for = None;
        if let Some(e) = err {
            self.flash(&e);
        }
        self.reload_overlays();
        if let Some(top) = self.overlays.last_mut() {
            let mut fx = Fx::default();
            top.returned(&mut fx);
            self.apply(fx);
        }
        // An editor opened from a popup: back to the popup; once that closes, back to the settings page
        if self.overlays.is_empty() {
            self.back_to_settings();
        }
    }

    /// l: open lazygit in this repository
    fn diff_lazygit(&mut self) {
        let Some(v) = self.reader.as_ref().and_then(|r| r.diff.as_ref()) else { return };
        match () {
            _ if v.loading => self.flash(t!("正在读取 git,请稍候", "still reading git, wait a moment")),
            _ if v.top.is_empty() => self.flash(t!("不是 git 仓库", "not a git repository")),
            _ => self.external = Some(External::Lazygit(v.top.clone())),
        }
    }

    /// e: open the spot at the very top of the screen in the editor (top is the
    /// file list / a blank line: look down for the first one with a position)
    fn diff_edit(&mut self) {
        let Some(r) = self.reader.as_ref() else { return };
        let Some(v) = r.diff.as_ref() else { return };
        let top = r.top(r.view_h.max(1));
        let line = v.starts.partition_point(|&x| x <= top).saturating_sub(1);
        let Some((file, n)) = v.at.iter().skip(line).find_map(|a| *a) else {
            return self.flash(if v.loading {
                t!("正在读取 git,请稍候", "still reading git, wait a moment")
            } else {
                t!("没有可打开的文件", "no file to open")
            });
        };
        match v.paths.get(file).cloned().flatten() {
            None => self.flash(t!("该文件已被删除", "this file has been deleted")),
            Some(p) => {
                let path = std::path::Path::new(&v.top).join(p);
                if path.is_file() {
                    self.external = Some(External::Edit { path: path.to_string_lossy().into_owned(), line: n });
                } else {
                    self.flash(t!("文件已不存在", "the file is gone"));
                }
            }
        }
    }

    /// n / p: put the next / previous file's heading at the very top (p past the first goes back to the file list)
    fn diff_jump(&mut self, d: isize) {
        let Some(r) = self.reader.as_mut() else { return };
        let Some(v) = r.diff.as_ref() else { return };
        let (h, total) = (r.view_h.max(1), r.wrapped.len());
        let top = r.top(h);
        let to = if d > 0 {
            v.rows.iter().copied().find(|&x| x > top)
        } else {
            Some(v.rows.iter().copied().rev().find(|&x| x < top).unwrap_or(0))
        };
        let scroll = to.map(|t| total.saturating_sub(t + h));
        if let Some(s) = scroll.filter(|&s| s != r.scroll.min(total.saturating_sub(h))) {
            r.scroll = s;
        }
    }

    pub(super) fn open_help(&mut self) {
        self.reader = Some(Reader {
            name: t!("aca 按键", "aca keys").into(),
            lines: vec![],
            scroll: usize::MAX,
            wrapped: vec![],
            wrap_w: 0,
            dirty: true,
            help: true,
            view_h: 0,
            diff: None,
            hit: None,
            plugin: None,
        });
    }

    /// The cell the full-screen reader is showing (the run loop fetches history from
    /// it): the pane id, or the session name for a stopped session without one
    pub fn reader_target(&self) -> Option<String> {
        let r = self.reader.as_ref().filter(|r| !r.help && r.diff.is_none() && r.plugin.is_none())?;
        let pane = self.all.iter().find(|s| s.name == r.name).map(|s| s.pane.clone()).filter(|p| !p.is_empty());
        Some(pane.unwrap_or_else(|| format!("={}:", r.name)))
    }

    /// History fetched back. While scrolled up, new content growing at the bottom doesn't push the view away
    pub fn set_reader_lines(&mut self, lines: Vec<String>) {
        let Some(r) = self.reader.as_mut().filter(|r| r.diff.is_none() && r.plugin.is_none()) else { return };
        let asking = self.all.iter().any(|s| s.name == r.name && s.state == State::Ask);
        r.lines = strip_prompt(ansi::parse_lines(&lines), asking);
        r.dirty = true;
    }

    pub(super) fn on_reader_key(&mut self, k: KeyEvent) {
        let page = (self.screen.height.saturating_sub(3) as usize).max(1);
        let Some(r) = self.reader.as_mut() else { return };
        let help = r.help;
        let diff = r.diff.is_some();
        let page = if help && r.view_h > 0 { r.view_h } else { page };
        if help {
            // Keys popup: Esc / ? / q close, arrows and paging keys scroll; any other key
            // "looks it up" — lights up the row explaining it and scrolls there
            match k.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q') if !k.modifiers.contains(KeyModifiers::CONTROL) => self.reader = None,
                KeyCode::Up => r.scroll = r.scroll.saturating_add(1),
                KeyCode::Down => r.scroll = r.scroll.saturating_sub(1),
                KeyCode::PageUp => r.scroll = r.scroll.saturating_add(page),
                KeyCode::PageDown => r.scroll = r.scroll.saturating_sub(page),
                KeyCode::Home => r.scroll = usize::MAX,
                KeyCode::End => r.scroll = 0,
                _ => r.hit = Some(crate::help::hit(k)),
            }
            return;
        }
        if let Some(req) = r.plugin.clone() {
            // Plugin output: can page, copy, r reruns; it's not a session, so you can't go in or switch to another session
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?') => self.reader = None,
                KeyCode::Char('r') => {
                    self.flash(&tr!("正在重新运行 {}…", "re-running {}…", req.title()));
                    self.plugin_q.push(req);
                }
                KeyCode::Char('c') => {
                    let text: Vec<String> = r.lines.iter().map(ansi::plain).collect();
                    self.clip = Some(Clip::Text(text.join("\n")));
                }
                KeyCode::Up | KeyCode::Char('k') => r.scroll = r.scroll.saturating_add(1),
                KeyCode::Down | KeyCode::Char('j') => r.scroll = r.scroll.saturating_sub(1),
                KeyCode::PageUp | KeyCode::Char('b') => r.scroll = r.scroll.saturating_add(page),
                KeyCode::PageDown | KeyCode::Char(' ') => r.scroll = r.scroll.saturating_sub(page),
                KeyCode::Char('u') => r.scroll = r.scroll.saturating_add(page / 2),
                KeyCode::Char('d') => r.scroll = r.scroll.saturating_sub(page / 2),
                KeyCode::Home | KeyCode::Char('g') => r.scroll = usize::MAX,
                KeyCode::End | KeyCode::Char('G') => r.scroll = 0,
                _ => {}
            }
            return;
        }
        match k.code {
            // z for output, x for the diff: pressing the one already shown goes back
            KeyCode::Char('z') if diff => self.open_reader(),
            KeyCode::Char('x') if !diff => self.open_diff(),
            KeyCode::Tab if diff => self.open_trace(),
            KeyCode::Tab => self.open_diff(),
            KeyCode::Char('Z') => self.open_trace(),
            KeyCode::Esc | KeyCode::Char('q' | 'z' | 'x' | '?') => self.reader = None,
            KeyCode::Char('n') if diff => self.diff_jump(1),
            KeyCode::Char('l') if diff => self.diff_lazygit(),
            KeyCode::Char('e') if diff => self.diff_edit(),
            KeyCode::Char('p') if diff => self.diff_jump(-1),
            KeyCode::Char('m') if diff => {
                self.diff_mode = if self.diff_mode == Mode::Uncommitted { Mode::Branch } else { Mode::Uncommitted };
                self.open_diff();
            }
            KeyCode::Enter => {
                self.reader = None;
                self.enter();
            }
            KeyCode::Up | KeyCode::Char('k') => r.scroll = r.scroll.saturating_add(1),
            KeyCode::Down | KeyCode::Char('j') => r.scroll = r.scroll.saturating_sub(1),
            KeyCode::PageUp | KeyCode::Char('b') => r.scroll = r.scroll.saturating_add(page),
            KeyCode::PageDown | KeyCode::Char(' ') => r.scroll = r.scroll.saturating_sub(page),
            KeyCode::Char('u') => r.scroll = r.scroll.saturating_add(page / 2),
            KeyCode::Char('d') => r.scroll = r.scroll.saturating_sub(page / 2),
            KeyCode::Home | KeyCode::Char('g') => r.scroll = usize::MAX, // clamped to the top when drawn
            KeyCode::End | KeyCode::Char('G') => r.scroll = 0,
            KeyCode::Char('J') => self.reader_step(1),
            KeyCode::Char('K') => self.reader_step(-1),
            KeyCode::Char('c') => self.ask_copy(true),
            KeyCode::Char('B') => self.ask_copy(false),
            _ => {}
        }
    }

    /// J / K: switch to the next / previous session in the list and keep viewing
    fn reader_step(&mut self, d: isize) {
        let cur = self.list.selected().unwrap_or(0) as isize;
        let mut next = cur + d;
        // Fold rows aren't sessions, skip them
        while next >= 0 && next < self.view.len() as isize && self.is_fold_row(next as usize) {
            next += d;
        }
        if next < 0 || next >= self.view.len() as isize {
            return;
        }
        self.select(Some(next as usize));
        if self.reader.as_ref().is_some_and(|r| r.diff.is_some()) {
            self.open_diff();
        } else {
            self.open_reader();
        }
    }
}

/// `?`'s keys popup. On desktop centered, three columns in one screen; on a phone it rises from the bottom, one column scrolling down, a
/// few list rows showing on top
pub(super) fn draw_help(f: &mut Frame, app: &mut App) {
    let full = f.area();
    let float = floating(full);
    let w = if float { full.width.saturating_sub(8).min(116) } else { full.width };
    let (probe, _) = modal_area(full, w, full.height);
    let inner_w = probe.width.saturating_sub(if float { 6 } else { 4 }) as usize;
    let hit = app.reader.as_ref().and_then(|r| r.hit.clone());
    let (lines, lit) = crate::help::lines(inner_w, hit.as_ref());
    let total = lines.len();
    // Title, blank line, (content), blank line, a line at the bottom
    let room = full.height.saturating_sub(if float { 2 } else { 3 } + modal_chrome(full) + 4) as usize;
    let body_h = total.min(room).max(1);
    let Some(r) = app.reader.as_mut() else { return };
    r.scroll = r.scroll.min(total - body_h);
    // Just pressed a key: if the first lit row isn't on this screen, scroll to it (stop one row above it); after that, scroll freely
    if let Some(h) = r.hit.as_mut().filter(|h| h.jump) {
        h.jump = false;
        let top = total - body_h - r.scroll;
        if let Some(&y) = lit.first().filter(|&&y| y < top || y >= top + body_h) {
            r.scroll = total - body_h - y.saturating_sub(1).min(total - body_h);
        }
    }
    r.view_h = body_h;
    let end = total - r.scroll;
    let top = end - body_h;
    let pages = total.div_ceil(body_h);
    let page = if r.scroll == 0 { pages } else { top / body_h + 1 };
    let pos = if pages > 1 { vec![Span::styled(format!("{page}/{pages}"), dim())] } else { esc_hint(t!("关闭", "close")) };
    // Left of the bottom bar: say something only when the pressed key isn't a shortcut; for a shortcut, its rows light up by themselves
    let said = match &hit {
        Some(h) if lit.is_empty() => Span::styled(tr!("「{}」不是快捷键", "“{}” isn't a key", h.label), dim()),
        _ => Span::raw(""),
    };
    let right =
        if float && pages == 1 { vec![] } else { key_spans(&[("↑↓", t!("滚动", "scroll")), ("Esc", t!("关闭", "close"))], 30).0 };
    let mut body = vec![modal_title(t!("快捷键", "Keys"), theme::accent(), pos, inner_w), Line::raw("")];
    body.extend(lines[top..end].iter().cloned());
    if !(said.content.is_empty() && right.is_empty()) {
        body.push(Line::raw(""));
        body.push(spread(vec![said], right, inner_w));
    }
    let inner = modal_frame(f, w, body.len() as u16 + modal_chrome(full), theme::pal().accent);
    f.render_widget(Paragraph::new(body), inner);
}

/// Full-screen reader (`z`): an accent-colored line with the title on top (like the preview on a phone), the wrapped output, the bottom bar
pub(super) fn draw_reader(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let w = area.width as usize;
    let h = body.height as usize;
    let pos = app.list.selected().unwrap_or(0);
    let n = app.view.len();
    let Some(r) = app.reader.as_mut() else { return };
    if r.dirty || r.wrap_w != w {
        let old = r.wrapped.len();
        let old_top = r.top(h);
        let same_width = r.wrap_w == w;
        let (wrapped, starts) = ansi::wrap_rows(&r.lines, w);
        r.wrapped = wrapped;
        if let Some(d) = r.diff.as_mut() {
            d.rows = d.files.iter().filter_map(|&i| starts.get(i).copied()).collect();
            d.starts = starts;
            // Diff refreshed: keep the same line number at the top
            if same_width {
                r.scroll = r.wrapped.len().saturating_sub(old_top + h);
            }
        } else if same_width && r.scroll > 0 {
            // New content grows at the bottom while scrolled up: add the distance from the bottom so the view stays put
            // (not when a rotation changed the width)
            r.scroll = r.scroll.saturating_add(r.wrapped.len().saturating_sub(old));
        }
        r.wrap_w = w;
        r.dirty = false;
    }
    let total = r.wrapped.len();
    r.scroll = r.scroll.min(total.saturating_sub(h));
    let end = total - r.scroll;
    let top = end.saturating_sub(h);
    r.view_h = h;
    let rows: Vec<Styled> = r.wrapped[top..end].to_vec();
    let scroll = r.scroll;
    let name = r.name.clone();
    let empty = r.lines.is_empty();
    // Viewing a diff: uncommitted / with commits, which file you're on
    let diff = r.diff.as_ref().map(|d| {
        let mode = if d.mode == Mode::Uncommitted { t!("未提交", "uncommitted") } else { t!("连同提交", "with commits") };
        let cur = d.rows.iter().filter(|&&x| x <= top).count();
        let at = match (d.loading, d.rows.len()) {
            (true, _) => t!(" · 读取中…", " · loading…").to_string(),
            (_, 0) => String::new(),
            (_, all) if cur == 0 => {
                let w = noun(all as u64, "file", "files");
                tr!(" · {all} 个文件", " · {all} {w}")
            }
            (_, all) => tr!(" · 文件 {cur}/{all}", " · file {cur}/{all}"),
        };
        (d.mode, format!("{mode}{at}"))
    });

    let s = app.all.iter().find(|s| s.name == name);
    let ic = s.map_or(("·", dim()), session_icon);
    let where_ = if scroll == 0 { t!("最新", "latest").to_string() } else { tr!("↑{scroll} 行", "↑{scroll} lines") };
    let title = s.map_or(name.as_str(), |s| s.title.as_str());
    let plugin = r.plugin.is_some();
    let (title, right) = match &diff {
        Some((_, label)) => (tr!("改动 · {title}", "Diff · {title}"), label.clone()),
        // A plugin's output isn't a session: no "which session number"
        None if plugin => (title.to_string(), where_),
        None => (title.to_string(), format!("{}/{n} · {where_}", pos + 1)),
    };
    f.render_widget(Paragraph::new(title_rule(ic, &title, vec![Span::styled(right, dim())], w)), head);
    if empty {
        f.render_widget(Paragraph::new(t!(" 无内容 · 会话可能已结束", " nothing here · the session may have ended")).style(dim()), body);
    } else {
        f.render_widget(Paragraph::new(rows.iter().map(ansi::to_line).collect::<Vec<_>>()), body);
        ansi::paint_links(f.buffer_mut(), body, &rows);
    }
    let foot_line = match &app.msg {
        Some((m, t)) if t.elapsed() < app.msg_ttl => Line::styled(m.clone(), theme::warn()).centered(),
        _ => match diff {
            Some((mode, _)) => menu_bar(
                &[
                    // On a phone drop 「滚动」 ("scroll") first (everyone knows it), keep n p and m
                    vec![
                        ("↑↓", t!("滚动", "scroll"), 2),
                        ("n p", t!("切换文件", "file"), 0),
                        (
                            "m",
                            if mode == Mode::Uncommitted {
                                t!("连同提交", "with commits")
                            } else {
                                t!("仅未提交", "uncommitted only")
                            },
                            1,
                        ),
                        ("J K", t!("切换会话", "session"), 5),
                    ],
                    vec![("⏎", t!("打开", "open"), 0), ("e", t!("编辑", "edit"), 4), ("l", "lazygit", 6), ("Tab", t!("记录", "record"), 3)],
                    vec![("Esc", t!("返回", "back"), 0)],
                ],
                Some(1),
                w,
            ),
            None if plugin => menu_bar(
                &[
                    vec![("↑↓", t!("滚动", "scroll"), 0), (t!("空格 b", "Space b"), t!("翻页", "page"), 2)],
                    vec![("r", t!("重新运行", "rerun"), 0), ("c", t!("复制", "copy"), 1)],
                    vec![("Esc", t!("返回", "back"), 0)],
                ],
                Some(1),
                w,
            ),
            None => menu_bar(
                &[
                    vec![
                        ("↑↓", t!("滚动", "scroll"), 0),
                        (t!("空格 b", "Space b"), t!("翻页", "page"), 2),
                        ("J K", t!("切换会话", "session"), 1),
                    ],
                    vec![("⏎", t!("打开", "open"), 0), ("c", t!("复制", "copy"), 3), ("Tab", t!("改动", "diff"), 4)],
                    vec![("Esc", t!("返回", "back"), 0)],
                ],
                Some(1),
                w,
            ),
        },
    };
    f.render_widget(Paragraph::new(foot_line), foot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{at, fixture, press, reader_rows, select, shows};

    #[test]
    fn question_mark_shows_the_keys_from_the_top() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.reader_target().is_none()); // not a session, the run loop doesn't capture it from tmux
        // On a phone it rises from the bottom, one column scrolling down from the top; a quick reference grouped by purpose,
        // not the text of aca help keys
        shows(&reader_rows(&mut app, 60, 12), &["快捷键", "1/", "导航"]);
        // On a desktop a centered popup, three columns fitting on one screen
        let rows = reader_rows(&mut app, 150, 40);
        assert!(rows.iter().any(|r| r.contains("导航") && r.contains("新建时")), "{rows:#?}");
        // Pressing a key inside = looking it up: lights up the rows describing it, doesn't actually do it
        let lit = |app: &App| crate::help::lines(100, app.reader.as_ref().and_then(|r| r.hit.as_ref())).1.len();
        press(&mut app, KeyCode::Char('T'));
        assert!(app.split.is_none());
        assert_eq!(lit(&app), 1);
        press(&mut app, KeyCode::Char('t'));
        assert!(app.term_req.is_none());
        assert_eq!(lit(&app), 1);
        press(&mut app, KeyCode::Char('y'));
        assert!(app.external.is_none());
        assert_eq!(lit(&app), 1);
        press(&mut app, KeyCode::Char('J')); // doesn't switch sessions
        press(&mut app, KeyCode::Enter); // doesn't enter the session
        assert!(app.reader.is_some() && app.attach.is_none());
        assert_eq!(lit(&app), 1);
        // Only says so for something that isn't a key
        assert!(!reader_rows(&mut app, 150, 40).iter().any(|r| r.contains("不是快捷键")));
        press(&mut app, KeyCode::Char('U'));
        shows(&reader_rows(&mut app, 150, 40), &["「U」不是快捷键"]);
        // On a phone, one column scrolling down: if the lit row isn't on this screen, scroll to it
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(lit(&app), 2);
        shows(&reader_rows(&mut app, 46, 12), &["移到分组"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
        // The bottom bar can't fit every key: drop the least used first; ? and ⏎ always stay
        assert_eq!(reader_rows(&mut app, 46, 12)[11], "n 新建 │ ⏎ 打开 • i 下一个 │ / 搜索 • ? 快捷键");
    }

    #[test]
    fn c_copies_last_response_and_shift_b_a_reference_to_the_session() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('c'));
        match app.clip.take() {
            Some(Clip::Last { profile, id, title, .. }) => {
                assert_eq!((profile.as_str(), id.as_str(), title.as_str()), ("default", "id-跑", "跑"))
            }
            other => panic!("{other:?}"),
        }
        // B on a registered Claude: the reference another agent acts on
        press(&mut app, KeyCode::Char('B'));
        match app.clip.take() {
            Some(Clip::Ref { title, text }) => {
                assert_eq!(title, "跑");
                assert_eq!(
                    text,
                    "aca 会话 id-跑「跑」(Claude,/p/work)\n看它:aca output id-跑 --turns 5(整段:--file)　给它传话:aca msg id-跑 \"…\""
                );
            }
            other => panic!("{other:?}"),
        }
        // C's all: the lines B used to copy, ending with the config dir Claude uses (this machine's default / account): only
        // that it's there
        press(&mut app, KeyCode::Char('C'));
        press(&mut app, KeyCode::Char('a'));
        match app.clip.take() {
            Some(Clip::Text(text)) => {
                let v: Vec<&str> = text.lines().collect();
                assert_eq!((v[0], v.len()), ("Path: /p/work", 2));
                assert!(v[1].starts_with("Home: /"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // C: pick something to copy; this row has no conversation id, so s only says there is none
        press(&mut app, KeyCode::Char('C'));
        assert!(app.modal_open());
        press(&mut app, KeyCode::Char('s'));
        assert!(app.modal_open() && app.clip.is_none());
        press(&mut app, KeyCode::Char('p'));
        assert_eq!((app.modal_open(), app.clip.take()), (false, Some(Clip::Text("/p/work".into()))));
        press(&mut app, KeyCode::Char('z')); // Copying works in full-screen reading too
        press(&mut app, KeyCode::Char('c'));
        assert!(matches!(app.clip.take(), Some(Clip::Last { .. })));
    }

    #[test]
    fn picking_for_aca_ref_puts_what_b_copies_instead_of_going_in() {
        let mut app = fixture();
        app.insert_to = Some("%7".into());
        let foot = |app: &mut App| {
            let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            let buf = t.backend().buffer();
            let (mut out, mut x) = (String::new(), 0);
            while x < 100 {
                out.push_str(buf[(x, 19)].symbol());
                x += buf[(x, 19)].cell_width().max(1);
            }
            out
        };
        select(&mut app, "跑");
        assert!(foot(&mut app).contains("⏎ 插入 • Esc 取消"), "{}", foot(&mut app));
        assert!(!foot(&mut app).contains("新建"), "{}", foot(&mut app));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.put.take().map(|p| p.0).as_deref(),
            Some("aca 会话 id-跑「跑」(Claude,/p/work)\n看它:aca output id-跑 --turns 5(整段:--file)　给它传话:aca msg id-跑 \"…\"")
        );
        assert!(app.attach.is_none() && app.start.is_none() && !app.quit);
        // The b that opened it, repeating into the list as it appears, and other keys that act on sessions: nothing
        for c in ['b', 'B', 'd', 'n', 'e', 'c'] {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(!app.modal_open() && app.put.is_none() && app.clip.is_none(), "{:?}", app.clip);
        // Finding one still works: j / k, / to search
        press(&mut app, KeyCode::Char('j'));
        assert_ne!(at(&app), "跑");
        app.insert_to = None;
        // Ctrl+B (tmux's prefix key, pressed where no tmux is in between) and Ctrl+Alt+B aren't b in the list either: no trash
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        assert!(!app.modal_open());
        press(&mut app, KeyCode::Char('b'));
        assert!(app.modal_open());
    }

    #[test]
    fn z_reads_without_attaching_and_scrolls_history() {
        let mut app = fixture(); // 1 day: 问 跑 闲
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('z'));
        assert!(app.reader_load && app.attach.is_none());
        assert_eq!(app.reader_target().as_deref(), Some("=跑:")); // Fake sessions have no pane id, so capture by session name
        app.reader_load = false;
        // History captured by the run loop: 100 lines + the input box at the bottom (to be stripped)
        let bar = "─".repeat(20);
        let mut hist: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
        hist.extend([bar.clone(), "❯ ".into(), bar]);
        app.set_reader_lines(hist);
        let rows = reader_rows(&mut app, 30, 10); // Title + 8 lines + bottom bar
        assert!(rows[0].contains("跑") && rows[0].contains("2/3 · 最新"), "{rows:?}");
        assert_eq!(rows[8], "line 100"); // Input box stripped, the last line is content
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('b')); // 1 + one page (10 - 3 = 7)
        let rows = reader_rows(&mut app, 30, 10);
        assert_eq!(rows[8], "line 92");
        assert!(rows[0].contains("↑8 行"), "{rows:?}");
        // New content grows at the bottom while scrolled up: the view stays put
        let mut more: Vec<String> = (1..=105).map(|i| format!("line {i}")).collect();
        more.push(String::new());
        app.set_reader_lines(more);
        assert_eq!(reader_rows(&mut app, 30, 10)[8], "line 92");
        press(&mut app, KeyCode::Char('g')); // To the top, no overrun
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(reader_rows(&mut app, 30, 10)[1], "line 1");
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(reader_rows(&mut app, 30, 10)[8], "line 105");
    }

    #[test]
    fn reader_j_k_switch_sessions_and_enter_attaches() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.reader_target().as_deref(), Some("=跑:")); // Fake sessions have no pane id, so capture by session name
        press(&mut app, KeyCode::Char('J'));
        press(&mut app, KeyCode::Char('J')); // At the end: a message, still viewing 闲
        assert_eq!(app.reader_target().as_deref(), Some("=闲:"));
        assert!(app.msg.is_none());
        press(&mut app, KeyCode::Char('q')); // q only leaves reading, doesn't quit aca
        assert!(app.reader.is_none() && !app.quit);
        assert_eq!(at(&app), "闲"); // Back on the list, highlight on the last one viewed
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Enter);
        assert!(app.reader.is_none());
        assert_eq!(app.attach.as_deref(), Some("闲"));
    }

    #[test]
    fn x_shows_the_diff_and_n_p_m_tab_get_around() {
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('x'));
        assert!(app.reader_target().is_none()); // Doesn't capture output from tmux
        let (name, _, mode) = app.diff_request(false).unwrap();
        assert_eq!((name.as_str(), mode), ("跑", Mode::Uncommitted));
        assert!(app.diff_request(false).is_none()); // Already read
        assert!(app.diff_request(true).is_none()); // Timed refresh, nothing new from it: no reread
        app.all.iter_mut().find(|s| s.name == "跑").unwrap().active_at += 1; // Reread only once there's activity
        assert!(app.diff_request(true).is_some());
        assert!(app.diff_request(true).is_none());
        shows(&reader_rows(&mut app, 40, 12), &["改动 · 跑", "未提交 · 读取中…", "正在读取 git…"]);
        // Three files, 10 lines each
        let patch = |files: &[&str]| {
            files
                .iter()
                .map(|f| {
                    format!(
                        "diff --git a/{f} b/{f}\n--- a/{f}\n+++ b/{f}\n@@ -1 +1,10 @@\n{}",
                        (1..=10).map(|j| format!("+{f}.{j}\n")).collect::<String>()
                    )
                })
                .collect::<String>()
        };
        let head = diff::Head { dir: "~/p".into(), branch: "main".into(), ..Default::default() };
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["f1", "f2", "f3"]), &[]));
        let rows = reader_rows(&mut app, 40, 12); // Title + 10 lines + bottom bar
        shows(&rows, &["未提交 · 3 个文件", "n p 切换文件"]);
        assert_eq!(rows[1], " ~/p  main +30");
        let top = |app: &mut App| reader_rows(app, 40, 12)[1].clone();
        // n: the next file's header goes to the very top
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(top(&mut app), "▌M f1 +10");
        shows(&reader_rows(&mut app, 40, 12), &["文件 1/3"]);
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(top(&mut app), "▌M f3 +10");
        press(&mut app, KeyCode::Char('n'));
        assert!(app.msg.is_none());
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(top(&mut app), "▌M f1 +10");
        press(&mut app, KeyCode::Char('p')); // Back past the first: to the file list
        assert_eq!(top(&mut app), " ~/p  main +30");
        press(&mut app, KeyCode::Char('p'));
        assert!(app.msg.is_none());
        // The diff grew on refresh (the file list gained a line): the top line stays
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('j')); // The new result comes before a redraw; this key still counts
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["f0", "f1", "f2", "f3"]), &[]));
        // Pinned by line number (the top was line 7, f1's @@): the file list gained a line, so line 7 is now f0's header
        let rows = reader_rows(&mut app, 40, 12);
        assert_eq!(rows[1], "▌M f0 +10", "{rows:#?}");
        // m: switch to with commits, read again; an old result still on its way is dropped
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.diff_request(false).map(|r| r.2), Some(Mode::Branch));
        app.set_diff("跑", Mode::Uncommitted, diff::render(&head, &patch(&["old"]), &[]));
        app.msg = None; // Keep the bottom bar free of messages
        shows(&reader_rows(&mut app, 46, 12), &["连同提交 · 读取中…", "m 仅未提交"]);
        // Tab cycles: diff → record → output → diff, the mode carried over
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_some() && app.reader.is_none());
        press(&mut app, KeyCode::Tab);
        assert!(app.trace.is_none());
        assert_eq!(app.reader_target().as_deref(), Some("=跑:"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.diff_request(false).map(|r| r.2), Some(Mode::Branch));
        // J switches session, still on the diff
        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.diff_request(false).map(|r| r.0), Some("闲".to_string()));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.reader.is_none() && !app.quit);
        assert_eq!(at(&app), "闲");
    }

    #[test]
    fn diff_e_opens_the_editor_where_the_top_line_is_and_l_opens_lazygit() {
        let tmp = std::env::temp_dir().join(format!("aca-ui-edit-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("f1"), "x\n".repeat(20)).unwrap();
        let mut app = fixture();
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.msg.as_ref().unwrap().0, "正在读取 git,请稍候");
        let patch = "diff --git a/f1 b/f1\n--- a/f1\n+++ b/f1\n@@ -5,3 +5,4 @@\n a\n-b\n+B\n+C\n c\ndiff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let mut d = diff::render(&diff::Head { dir: "~/p".into(), ..Default::default() }, patch, &[]);
        d.top = tmp.to_string_lossy().into_owned();
        let name = app.reader.as_ref().unwrap().name.clone();
        app.set_diff(&name, Mode::Uncommitted, d);
        // Only 3 lines per screen, so it can scroll
        reader_rows(&mut app, 40, 5);
        let f1 = tmp.join("f1").to_string_lossy().into_owned();
        // File list on top: opens the first change in the first file
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Edit { path: f1.clone(), line: 6 })); // -b is on line 6
        // Scrolled so the +C line is at the very top: line 7
        press(&mut app, KeyCode::Char('n'));
        for _ in 0..5 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(reader_rows(&mut app, 40, 5)[1], "+C");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Edit { path: f1, line: 7 }));
        // A deleted file can't be opened
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('e'));
        assert!(app.external.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "该文件已被删除");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.external.take(), Some(External::Lazygit(tmp.to_string_lossy().into_owned())));
        // The diff is reread after coming back
        let _ = app.diff_request(false);
        app.external_done(None);
        assert!(app.diff_request(false).is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_change_count_is_read_for_the_highlighted_one_when_it_changes_or_ticks() {
        let mut app = fixture();
        assert_ne!(app.mode(), Preview::Off);
        let first = app.glance_request(false).map(|r| r.0);
        assert!(first.is_some());
        assert!(app.glance_request(false).is_none()); // Same one, not read again
        assert!(app.glance_request(true).is_none()); // Timed refresh, nothing new: no read (no git every 2s while on a big repo)
        // It has activity (running: pane output; idle: a hook reported something): read again
        let i = app.view[app.list.selected().unwrap()];
        app.all[i].active_at += 1;
        assert!(app.glance_request(true).is_some());
        assert!(app.glance_request(true).is_none());
        // Also read once GLANCE_EVERY has passed: files changed elsewhere show up too
        app.glance_for.as_mut().unwrap().2 = Instant::now() - GLANCE_EVERY;
        assert!(app.glance_request(true).is_some());
        select(&mut app, "跑");
        let next = app.glance_request(false).map(|r| r.0);
        assert!(next.is_some() && next != first);
        press(&mut app, KeyCode::Char('z')); // Not read in full screen
        assert!(app.glance_request(true).is_none());
    }
}
