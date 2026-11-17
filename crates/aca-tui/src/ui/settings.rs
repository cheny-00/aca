//! The settings page (ctrl-s) and the theme list: rows that write config.toml, the rows plugins add, and the pages they open.

use super::*;

/// One row of the `^S` settings page
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SetRow {
    Theme,
    Lang,
    Ime,
    Accounts,
    Tools,
    GroupDir,
    Config,
    /// Added by a feature itself (`App::with_setting`): index in `Settings::extra`
    Extra(usize),
    /// The broken ones (not listed if none)
    Problems,
}

impl SetRow {
    /// The key at the start of the row; the broken row has none, reach it with ↑↓. For rows added by a feature, ask `Settings::key`
    fn key(self) -> Option<char> {
        match self {
            SetRow::Theme => Some('t'),
            SetRow::Lang => Some('l'),
            SetRow::Ime => Some('i'),
            SetRow::Accounts => Some('a'),
            SetRow::Tools => Some('n'),
            SetRow::GroupDir => Some('g'),
            SetRow::Config => Some('e'),
            SetRow::Extra(_) | SetRow::Problems => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            SetRow::Theme => t!("主题", "theme"),
            SetRow::Lang => t!("语言", "language"),
            SetRow::Ime => t!("自动切英文", "auto English input"),
            SetRow::Accounts => t!("账号", "accounts"),
            SetRow::Tools => t!("工具菜单", "tool menu"),
            SetRow::GroupDir => t!("新分组目录", "new group directory"),
            SetRow::Config => "config.toml",
            SetRow::Extra(_) | SetRow::Problems => "",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SetPage {
    Main,
    Problems,
}

/// The `^S` settings page. Values shown are read fresh on open (and reread after a change, or when config.toml was changed), not read
/// from file every frame
#[derive(Clone, Debug)]
pub(super) struct Settings {
    page: SetPage,
    /// Highlighted row on the main page
    sel: usize,
    theme: String,
    lang: Lang,
    ime: bool,
    group_dir: String,
    bad: Vec<String>,
    accts: Vec<crate::accounts::Account>,
    /// Rows added by features, asked for on open
    extra: Vec<ExtraRow>,
}

/// What a row added by a feature (`overlay::Setting`) looks like when the settings page opens
#[derive(Clone, Debug)]
struct ExtraRow {
    key: char,
    name: &'static str,
    value: Vec<Span<'static>>,
    mark: &'static str,
}

impl Settings {
    fn rows(&self) -> Vec<SetRow> {
        let mut v = vec![SetRow::Theme, SetRow::Lang];
        // Switching input method is only possible on macOS (ime.rs): not listed elsewhere, so there's no 「开」 ("on") that does nothing
        // when pressed
        if cfg!(target_os = "macos") {
            v.push(SetRow::Ime);
        }
        v.extend([SetRow::Accounts, SetRow::Tools, SetRow::GroupDir, SetRow::Config]);
        v.extend((0..self.extra.len()).map(SetRow::Extra));
        if !self.bad.is_empty() {
            v.push(SetRow::Problems);
        }
        v
    }

    fn key(&self, r: SetRow) -> Option<char> {
        match r {
            SetRow::Extra(i) => self.extra.get(i).map(|x| x.key),
            r => r.key(),
        }
    }

    fn name(&self, r: SetRow) -> &'static str {
        match r {
            SetRow::Extra(i) => self.extra.get(i).map_or("", |x| x.name),
            r => r.name(),
        }
    }
}

/// The palette list (「主题」 ("theme") in `^S`, see `open_themes`): all themes (each one's colors prepared up front; moving onto one
/// switches the whole interface to it), which one is highlighted,
/// the one in use before opening (Esc switches back), the one saved in config (marked ●)
pub(super) struct ThemeMenu {
    list: Vec<(theme::Theme, &'static theme::Pal)>,
    sel: usize,
    was: &'static theme::Pal,
    now: String,
}

impl App {
    /// The color list (the settings page's "theme"): lists themes, the highlight on the one in use
    fn open_themes(&mut self) {
        let now = theme::current();
        let list: Vec<(theme::Theme, &'static theme::Pal)> = theme::list()
            .into_iter()
            .map(|t| {
                let p: &'static theme::Pal = Box::leak(Box::new(t.pal));
                (t, p)
            })
            .collect();
        let sel = list.iter().position(|x| x.0.name == now).unwrap_or(0);
        self.theme_menu = Some(ThemeMenu { list, sel, was: theme::pal(), now });
    }

    /// Color list: moving ↑↓ switches the whole interface to that theme, ⏎ uses it
    /// (saved to config.toml), Esc switches back to the one before opening
    pub(super) fn on_theme_key(&mut self, k: KeyEvent) {
        let Some(m) = self.theme_menu.as_mut() else { return };
        let n = m.list.len();
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => m.sel = (m.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') => m.sel = (m.sel + n - 1) % n,
            KeyCode::Enter => {
                let (t, p) = &m.list[m.sel];
                let name = t.name.clone();
                theme::set(p);
                self.theme_menu = None;
                match theme::choose(&name) {
                    Ok(()) => self.flash(&tr!("配色:{name}", "colors: {name}")),
                    Err(e) => self.flash(&e),
                }
                return self.back_to_settings();
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                theme::set(m.was);
                self.theme_menu = None;
                return self.back_to_settings();
            }
            _ => return,
        }
        theme::set(m.list[m.sel].1);
    }

    /// `^S`: the settings page. Everything rarely used lives here, taking no single key
    pub(super) fn open_settings(&mut self) {
        self.settings = Some(self.load_settings(SetPage::Main, 0));
    }

    /// Values shown by the settings page, read fresh
    fn load_settings(&self, page: SetPage, sel: usize) -> Settings {
        let mut bad = (self.config_bad)();
        bad.extend(
            self.sync_failed.iter().map(|e| {
                tr!("config.toml 有误,已忽略账号和工具设置:{e}", "config.toml has a problem; accounts and tools are ignored: {e}")
            }),
        );
        let mut s = Settings {
            page,
            sel,
            theme: theme::current(),
            lang: i18n::lang(),
            ime: (self.ime_auto)(),
            group_dir: (self.new_group_dir)(),
            bad,
            accts: (self.accounts_lister)(),
            extra: self.setting_rows.iter().map(|r| ExtraRow { key: r.key(), name: r.name(), value: r.value(), mark: r.mark() }).collect(),
        };
        s.sel = s.sel.min(s.rows().len() - 1);
        if s.page == SetPage::Problems && s.bad.is_empty() {
            s.page = SetPage::Main;
        }
        s
    }

    /// Language changed (l on the settings page, another aca changed config.toml): text written and stored in the old language is
    /// void, what needs rereading is reread at once. List and popup text is built every frame, nothing to do; this handles what was
    /// read in the background or computed on opening
    pub fn lang_changed(&mut self) {
        // Diff stats at the top of the preview (the branch's "detached", why a directory can't be read)
        self.glances.clear();
        self.glance_for = None;
        // Billing: quota descriptions are written by a subprocess in the language of the time; reread at once if open, next open if closed
        self.bill_data = None;
        self.bill_got = None;
        self.bill_want = self.bill.is_some();
        // Records: node labels are set when read, reread all of it
        if let Some(v) = self.trace.take() {
            self.trace = Some(TraceView::new(v.name));
        }
        if let Some(d) = self.reader.as_mut().and_then(|r| r.diff.as_mut()) {
            d.want = true;
        }
        for o in &mut self.overlays {
            o.reload();
        }
        self.reload_settings();
    }

    /// config.toml changed (in an editor, another aca): an open settings page rereads
    pub fn reload_settings(&mut self) {
        if let Some(s) = self.settings.take() {
            self.settings = Some(self.load_settings(s.page, s.sel));
        }
    }

    /// What was done from the settings page is finished (or cancelled): back to where it was on the settings page, values reread
    pub(super) fn back_to_settings(&mut self) {
        if let Some(s) = self.settings_back.take() {
            self.settings = Some(self.load_settings(s.page, s.sel));
        }
    }

    /// Leave the settings page to do something, back when done
    fn leave_settings(&mut self) {
        self.settings_back = self.settings.take();
    }

    pub(super) fn on_settings_key(&mut self, k: KeyEvent) {
        let Some(s) = self.settings.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('s') {
            self.settings = None;
            return;
        }
        match s.page {
            SetPage::Main => {
                let rows = s.rows();
                let n = rows.len();
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => s.sel = (s.sel + 1) % n,
                    KeyCode::Up | KeyCode::Char('k') => s.sel = (s.sel + n - 1) % n,
                    KeyCode::Enter => {
                        let r = rows[s.sel];
                        self.setting(r);
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.settings = None,
                    KeyCode::Char(c) if !ctrl => {
                        if let Some(i) = rows.iter().position(|&r| s.key(r) == Some(c)) {
                            s.sel = i;
                            self.setting(rows[i]);
                        }
                    }
                    _ => {}
                }
            }
            SetPage::Problems => match k.code {
                KeyCode::Char('e') => self.setting(SetRow::Config),
                KeyCode::Esc | KeyCode::Char('q') => s.page = SetPage::Main,
                _ => {}
            },
        }
    }

    /// A settings page row: switches flip on the spot, others go in
    fn setting(&mut self, r: SetRow) {
        let Some(s) = self.settings.as_mut() else { return };
        match r {
            SetRow::Theme => {
                self.leave_settings();
                self.open_themes();
            }
            // Switch on the spot (the whole interface is in that language next frame), also saved to config.toml for next time
            SetRow::Lang => {
                let to = if s.lang == Lang::Zh { Lang::En } else { Lang::Zh };
                match (self.config_set)("ui", "lang", toml::Value::String(to.code().into())) {
                    Ok(()) => {
                        i18n::set(to);
                        s.lang = to;
                        if i18n::forced() {
                            self.flash(t!(
                                "已切换;当前终端设置了 ACA_LANG,下次打开仍以它为准",
                                "switched; this terminal sets ACA_LANG, which wins next time"
                            ));
                        }
                    }
                    Err(e) => self.flash(&e),
                }
            }
            SetRow::Ime => {
                let on = !s.ime;
                match (self.config_set)("ime", "auto", toml::Value::Boolean(on)) {
                    Ok(()) => {
                        s.ime = on;
                        self.ime_changed = true;
                        if crate::ime::forced() {
                            self.flash(t!("当前终端设置了 ACA_IME,以它为准", "this terminal sets ACA_IME, which wins"));
                        }
                    }
                    Err(e) => self.flash(&e),
                }
            }
            // The accounts page is its own popup (acctpage.rs): closing it returns to this settings row, accounts recounted
            SetRow::Accounts => {
                if let Some(page) = self.accounts_page {
                    self.leave_settings();
                    self.open(page(false));
                }
            }
            SetRow::Tools | SetRow::Config => {
                self.leave_settings();
                let path = crate::config::path().to_string_lossy().into_owned();
                self.external = Some(External::Config { path, template: CONFIG_TEMPLATE.to_string() });
            }
            SetRow::GroupDir => {
                let text = s.group_dir.clone();
                self.leave_settings();
                self.prompt = Some(Prompt { ask: Ask::GroupDir, text, hint: String::new() });
            }
            SetRow::Extra(i) => {
                let mut fx = Fx::default();
                if let Some(row) = self.setting_rows.get(i) {
                    row.enter(&mut fx);
                }
                self.leave_settings();
                self.apply(fx);
                // Didn't go anywhere (switched on the spot): back to the settings page, values reread
                if self.overlays.is_empty() && self.external.is_none() {
                    self.back_to_settings();
                }
            }
            SetRow::Problems => s.page = SetPage::Problems,
        }
    }
}

/// Color theme list. Unlike other popups it doesn't grey out what's below: the aca below is the preview. On desktop it floats in the
/// bottom-left corner (most of the list and preview stay visible), on a phone it rises from the bottom. One theme per row: ● on the saved
/// one, name, its own accent color, state colors (red green yellow), account colors
pub(super) fn draw_themes(f: &mut Frame, m: &ThemeMenu) {
    let (outer, w) = side_width(f.area(), 50);
    let mut body = vec![modal_title(t!("配色", "Colors"), theme::accent(), esc_hint(t!("返回", "back")), w), Line::raw("")];
    let name_w = m.list.iter().map(|x| x.0.name.width()).max().unwrap_or(0).min(16);
    let mut last = None;
    for (i, (t, _)) in m.list.iter().enumerate() {
        if last != Some(t.dark) {
            if last.is_some() {
                body.push(Line::raw(""));
            }
            body.push(Line::styled(if t.dark { t!("深色", "dark") } else { t!("浅色", "light") }, dim()));
            last = Some(t.dark);
        }
        let p = &t.pal;
        let block = |c: Color, n: usize| Span::styled("█".repeat(n), Style::new().fg(c));
        let mut spans = vec![
            if t.name == m.now { Span::styled("● ", theme::accent()) } else { Span::raw("  ") },
            Span::styled(clip(&t.name, name_w), Style::new().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            block(p.accent, 2),
            Span::raw(" "),
        ];
        spans.extend([p.red, p.green, p.amber].map(|c| block(c, 1)));
        spans.push(Span::raw(" "));
        spans.extend(p.series.iter().take(w.saturating_sub(name_w + 11)).map(|&c| block(c, 1)));
        let mut line = Line::from(spans);
        if i == m.sel {
            let pad = w.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(pad)));
            line = line.patch_style(Style::new().bg(theme::pal().sel));
        }
        body.push(line);
    }
    body.push(Line::raw(""));
    body.push(keys_line(&[("↑↓", t!("预览", "preview")), ("⏎", t!("应用", "use it"))], vec![], w));
    side_panel(f, outer, body);
}

/// `^S` settings page: a centered popup (greys out what's below like Tab / S; when picking a theme, it becomes the color theme list in the
/// bottom-left that doesn't grey out). One item per row, "key  name  current value", › = go in to pick, ↗ = open in an editor; Problems get
/// their own page, accounts are a separate popup (acctpage.rs)
pub(super) fn draw_settings(f: &mut Frame, s: &Settings) {
    modal(f, 54, theme::pal().accent, |w| settings_lines(s, w));
}

fn settings_lines(s: &Settings, w: usize) -> Vec<Line<'static>> {
    let pad = |mut line: Line<'static>, on: bool| {
        if on {
            let n = w.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(n)));
            line = line.patch_style(Style::new().bg(theme::pal().sel));
        }
        line
    };
    let mut body;
    match s.page {
        SetPage::Main => {
            body = vec![modal_title(t!("设置", "Settings"), theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
            let rows = s.rows();
            let name_w = rows.iter().map(|&r| s.name(r).width()).max().unwrap_or(0);
            let claude = s.accts.iter().filter(|a| a.agent == Agent::Claude).count();
            let codex = s.accts.len() - claude;
            for (i, &r) in rows.iter().enumerate() {
                let forced = |on: bool| if on { vec![Span::styled(t!(" · 环境变量", " · env var"), dim())] } else { vec![] };
                let (val, mark): (Vec<Span<'static>>, &str) = match r {
                    SetRow::Theme => (
                        [vec![Span::styled(s.theme.clone(), Style::new().add_modifier(Modifier::BOLD))], forced(theme::forced())].concat(),
                        "›",
                    ),
                    // Show both, the one in use lit up
                    SetRow::Lang => {
                        let opt = |l: Lang| {
                            let st = if l == s.lang { theme::accent().add_modifier(Modifier::BOLD) } else { theme::faint() };
                            Span::styled(l.code(), st)
                        };
                        ([vec![opt(Lang::Zh), Span::raw("  "), opt(Lang::En)], forced(i18n::forced())].concat(), "")
                    }
                    SetRow::Ime => (
                        [
                            vec![Span::styled(
                                if s.ime { t!("开", "on") } else { t!("关", "off") },
                                Style::new().add_modifier(Modifier::BOLD),
                            )],
                            forced(crate::ime::forced()),
                        ]
                        .concat(),
                        "",
                    ),
                    SetRow::Accounts => (
                        vec![
                            Span::styled("Claude ", agent_style(Agent::Claude)),
                            Span::raw(claude.to_string()),
                            Span::styled("  Codex ", agent_style(Agent::Codex)),
                            Span::raw(codex.to_string()),
                        ],
                        "›",
                    ),
                    SetRow::GroupDir => (vec![Span::raw(s.group_dir.clone())], ""),
                    SetRow::Tools | SetRow::Config => (vec![], "↗"),
                    SetRow::Extra(x) => s.extra.get(x).map_or((vec![], ""), |x| (x.value.clone(), x.mark)),
                    SetRow::Problems => (vec![Span::styled(tr!("{} 处错误", "problems: {}", s.bad.len()), theme::warn())], "›"),
                };
                let key = s.key(r).map_or_else(|| Span::styled("⚠", theme::warn()), |c| Span::styled(c.to_string(), key_style()));
                let mut left = vec![key, Span::raw("  ")];
                if r != SetRow::Problems {
                    left.push(Span::raw(clip(s.name(r), name_w)));
                    left.push(Span::raw("  "));
                }
                left.extend(val);
                let right = if mark.is_empty() { vec![] } else { vec![Span::styled(mark, theme::faint())] };
                body.push(pad(spread(left, right, w), i == s.sel));
            }
            body.push(Line::raw(""));
            body.push(keys_line(&[("⏎", t!("修改", "change"))], vec![], w));
        }
        SetPage::Problems => {
            body = vec![modal_title(t!("错误项", "Problems"), theme::accent(), esc_hint(t!("返回", "back")), w), Line::raw("")];
            for b in &s.bad {
                body.extend(wrap_width(b, w).into_iter().map(|l| Line::styled(l, theme::warn())));
            }
            body.push(Line::raw(""));
            body.push(keys_line(&[("e", t!("编辑 config.toml", "edit config.toml"))], vec![], w));
        }
    }
    body
}

/// What gets written into config.toml first when the settings page opens it in an editor and the file doesn't exist yet
const CONFIG_TEMPLATE: Tr = Tr::new(
    "# aca 的配置:账号、工具、界面的设置。写法见 https://github.com/cheny-00/aca/blob/main/docs/configuration.zh-CN.md\n",
    "# aca config: accounts, tools, interface settings. See https://github.com/cheny-00/aca/blob/main/docs/configuration.md\n",
);

/// Which of the tool menu's two sets it shows: `n`'s (true = the special environments' are there for `^E`), or the special
/// environments' (back: switched to from `n`'s, Esc goes back; relogin: the row asked whether to log in again)
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Set {
    Here(bool),
    Special { back: bool, relogin: Option<usize> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{
        FakePage, FakeRow, SaysRow, ctrl, fixture, keys, life_and_work, press, reader_rows, sessions, shows, target, tool,
    };

    #[test]
    fn plus_in_the_tool_menu_opens_the_accounts_page_ready_to_add() {
        let mut app = fixture().with_tools(|| Ok(vec![])).with_accounts(|adding| Box::new(FakePage(if adding { 7 } else { 0 })));
        app.tools_loader = || Ok(vec![tool("work", "claude", "work", Agent::Claude)]);
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 60, 14), &["+ 添加账号"]);
        press(&mut app, KeyCode::Char('+'));
        // The menu closes, into the accounts page (opening on adding an account); closing it returns to the list, not the
        // menu, no settings page
        assert!(app.tool_menu.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 7"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && app.tool_menu.is_none() && app.settings.is_none());
        // By default (snapshots, other tests) there's no accounts page and config isn't touched: + does nothing, the menu stays
        let mut plain = fixture().with_tools(|| Ok(vec![tool("x", "x", "", Agent::Other)]));
        press(&mut plain, KeyCode::Char('n'));
        press(&mut plain, KeyCode::Char('+'));
        assert!(plain.tool_menu.is_some() && plain.overlays.is_empty());
    }

    #[test]
    fn the_theme_list_previews_each_theme_on_the_whole_screen_and_esc_puts_it_back() {
        let mut app = App::new(String::new(), life_and_work());
        let latte = theme::pal().accent;
        // Takes no single key (C is pick something to copy)
        keys(&mut app, "C");
        assert!(app.theme_menu.is_none() && app.modal_open());
        keys(&mut app, "⎋");
        app.open_themes();
        let m = app.theme_menu.as_ref().unwrap();
        assert_eq!(m.list.iter().map(|x| x.0.name.as_str()).collect::<Vec<_>>(), ["latte", "mocha"]);
        assert_eq!(m.sel, 0);
        keys(&mut app, "j");
        let mocha = theme::pal().accent;
        assert_ne!(mocha, latte);
        // The layer below isn't dimmed (the layer below is the preview), the popup sits in the bottom-left corner
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let buf = t.backend().buffer().clone();
        assert!(!buf.content().iter().any(|c| c.fg == theme::pal().fade));
        let rows = reader_rows(&mut app, 100, 30);
        let y = rows.iter().position(|r| r.contains("配色")).unwrap();
        assert!(y > 10 && rows[y].find("配色").unwrap() < 10, "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("● latte")) && rows.iter().any(|r| r.contains("深色")), "{rows:#?}");
        keys(&mut app, "⎋");
        assert!(app.theme_menu.is_none());
        assert_eq!(theme::pal().accent, latte);
        // ⏎ uses it
        app.open_themes();
        keys(&mut app, "k⏎");
        assert!(app.theme_menu.is_none());
        assert_eq!(theme::pal().accent, mocha);
        assert_eq!(app.msg.as_ref().unwrap().0, "配色:mocha");
    }

    thread_local! {
        /// What the settings page writes to config.toml (the fake config_set records it here)
        static WROTE: std::cell::RefCell<Vec<(String, String, String)>> = const { std::cell::RefCell::new(vec![]) };
    }

    fn wrote() -> Vec<(String, String, String)> {
        WROTE.with(|w| w.borrow_mut().drain(..).collect())
    }

    fn settings_app() -> App {
        fixture().with_settings(
            || true,
            Vec::new,
            |sec, key, v| {
                WROTE.with(|w| w.borrow_mut().push((sec.into(), key.into(), v.to_string())));
                Ok(())
            },
            || {
                let acct = |agent, name: &str, dir: &str| crate::accounts::Account { agent, name: name.into(), dir: dir.into() };
                vec![acct(Agent::Claude, "work", "~/.claude-work"), acct(Agent::Codex, "cx", "~/.codex-cx")]
            },
        )
    }

    #[test]
    fn ctrl_s_opens_settings_letters_act_and_plain_s_still_sends() {
        let mut app = settings_app();
        wrote();
        ctrl(&mut app, 's');
        assert!(app.settings.is_some() && app.confirm.is_none());
        press(&mut app, KeyCode::Char('i'));
        if cfg!(target_os = "macos") {
            assert_eq!(wrote(), [("ime".to_string(), "auto".to_string(), "false".to_string())]);
            assert!(!app.settings.as_ref().unwrap().ime && app.ime_changed);
            press(&mut app, KeyCode::Enter); // Highlight stays on the row just pressed: ⏎ switches back
            assert_eq!(wrote()[0].2, "true");
        } else {
            // Auto English input is macOS-only: elsewhere the row isn't there, and pressing writes nothing
            assert!(wrote().is_empty() && !app.ime_changed);
        }
        ctrl(&mut app, 's');
        assert!(app.settings.is_none());
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Esc);
        assert!(app.settings.is_none() && !app.quit);
    }

    #[test]
    fn l_in_settings_switches_the_language_right_away_and_saves_it() {
        i18n::with(Lang::Zh, || {
            let mut app = settings_app();
            wrote();
            ctrl(&mut app, 's');
            shows(&reader_rows(&mut app, 100, 30), &["l  语言", "zh  en"]);
            press(&mut app, KeyCode::Char('l'));
            assert_eq!(wrote(), [("ui".to_string(), "lang".to_string(), "\"en\"".to_string())]);
            assert_eq!(i18n::lang(), Lang::En);
            // Settings page still open, already in English
            shows(&reader_rows(&mut app, 100, 30), &["Settings", "l  language", "zh  en"]);
            press(&mut app, KeyCode::Enter);
            assert_eq!((wrote()[0].2.as_str(), i18n::lang()), ("\"zh\"", Lang::Zh));
            shows(&reader_rows(&mut app, 100, 30), &["设置", "l  语言"]);
        });
    }

    #[test]
    fn a_language_change_rereads_what_was_asked_when_settings_opened() {
        i18n::with(Lang::Zh, || {
            let mut app = settings_app().with_setting(Box::new(SaysRow));
            app.bill_got = Some(Instant::now());
            ctrl(&mut app, 's');
            shows(&reader_rows(&mut app, 100, 30), &["假功能", "关着"]);
            press(&mut app, KeyCode::Char('l'));
            app.lang_changed();
            shows(&reader_rows(&mut app, 100, 30), &["fake feature", "off", "language"]);
            assert!(app.bill_got.is_none() && app.bill_data.is_none());
        });
    }

    #[test]
    fn a_feature_adds_a_settings_row_and_its_own_popup() {
        let mut app = settings_app().with_setting(Box::new(FakeRow));
        ctrl(&mut app, 's');
        shows(&reader_rows(&mut app, 100, 30), &["d  假功能", "现在关着"]);
        // Pressing its key: the settings page closes, its popup opens, and all keys go to it (j doesn't move the highlight)
        press(&mut app, KeyCode::Char('d'));
        assert!(app.settings.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        shows(&reader_rows(&mut app, 44, 30), &["假页面 0"]);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.msg.as_ref().unwrap().0, "说一句");
        // On a phone the popup covers the bottom bar: the message goes inside the popup
        let rows = reader_rows(&mut app, 44, 30);
        let at = |want: &str| rows.iter().position(|r| r.contains(want)).unwrap_or_else(|| panic!("没有「{want}」:{rows:#?}"));
        assert!(at("说一句") > at("假页面 0") && rows[at("说一句")].starts_with('│'), "{rows:#?}");
        // Open another layer; closing it goes back to the one before
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 30), &["假页面 1"]);
        // The top layer says it's typing: several keys at once are a paste, which the run loop doesn't drop
        assert!(app.typing());
        press(&mut app, KeyCode::Esc);
        assert!(!app.typing());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // Background work goes to the run loop; another one before it's done is refused; when it's done the popup reloads
        press(&mut app, KeyCode::Char('j'));
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Stop { t: target("闲") });
        // While it's being done, the popup says 「正在…」 (e.g. "stopping …")
        shows(&reader_rows(&mut app, 44, 30), &["│ 正在停止「闲」…"]);
        press(&mut app, KeyCode::Char('j'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在停止「闲」…");
        app.job_done(&job, Outcome::Done("好了".into()), sessions());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 10"]);
        // What to copy goes to the run loop (it writes the clipboard, the bottom bar says what was copied); the popup stays open
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(app.clip.take(), Some(Clip::Text("https://a.ts.net/".into())));
        assert!(!app.overlays.is_empty());
        // Opening an editor: back in the popup afterwards, reloaded
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.external.take(), Some(External::Lazygit("/p".into())));
        app.external_done(None);
        assert!(app.settings.is_none());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 20"]);
        // Closed: back to the settings page, highlight still on its row
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        let s = app.settings.as_ref().unwrap();
        assert_eq!(s.rows()[s.sel], SetRow::Extra(0));
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && !app.quit);
    }

    #[test]
    fn shift_p_opens_the_plugins_page_from_the_list_not_from_settings() {
        // Opens straight from the list (on a group too), closing goes back to the list, never via the settings page
        let mut app = fixture().with_plugins(|| Box::new(FakePage(0)));
        press(&mut app, KeyCode::Char('P'));
        assert!(app.modal_open() && app.settings.is_none());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // Into a page from the menu (another layer), Esc back to the menu, another Esc to the list
        press(&mut app, KeyCode::Char('n'));
        shows(&reader_rows(&mut app, 100, 30), &["假页面 1"]);
        press(&mut app, KeyCode::Esc);
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.modal_open() && app.settings.is_none() && !app.quit);
        // The settings page has no row for it
        let mut app = settings_app().with_plugins(|| Box::new(FakePage(0)));
        ctrl(&mut app, 's');
        assert!(!reader_rows(&mut app, 100, 30).iter().any(|r| r.contains("tailnet")));
    }

    #[test]
    fn settings_come_back_after_the_theme_list_the_dir_prompt_and_the_editor() {
        let mut app = settings_app();
        wrote();
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('t'));
        assert!(app.theme_menu.is_some() && app.settings.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.theme_menu.is_none() && app.settings.as_ref().is_some_and(|s| s.sel == 0));

        press(&mut app, KeyCode::Char('g'));
        assert!(matches!(app.prompt.as_ref().map(|p| (&p.ask, p.text.as_str())), Some((Ask::GroupDir, "~/workspace"))));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.as_ref().is_some_and(|p| p.hint == "目录不能为空") && wrote().is_empty());
        for c in "~/code".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(wrote(), [("new_group".to_string(), "default_path".to_string(), "\"~/code\"".to_string())]);
        assert!(app.prompt.is_none() && app.settings.as_ref().is_some_and(|s| s.rows()[s.sel] == SetRow::GroupDir));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.settings.is_some() && app.msg.is_none());

        press(&mut app, KeyCode::Char('n'));
        let Some(External::Config { path, template }) = app.external.take() else { panic!("{:?}", app.external) };
        assert!(path.ends_with("config.toml") && template.starts_with('#'));
        assert!(app.settings.is_none());
        app.external_done(None);
        assert!(app.settings.as_ref().is_some_and(|s| s.rows()[s.sel] == SetRow::Tools));
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::Config { path, .. }) if path.ends_with("config.toml")));
        // An editor not opened from the settings page doesn't bring the settings page back on return
        app.external_done(None);
        app.settings = None;
        app.external_done(None);
        assert!(app.settings.is_none());
    }

    #[test]
    fn a_in_settings_opens_the_accounts_page_and_closing_it_comes_back() {
        let mut app = settings_app().with_accounts(|adding| Box::new(FakePage(if adding { 7 } else { 0 })));
        ctrl(&mut app, 's');
        // The settings row only counts how many of each; accounts are managed in their own popup (acctpage.rs)
        shows(&reader_rows(&mut app, 100, 30), &["a  账号", "Claude 1  Codex 1"]);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.settings.is_none() && app.modal_open());
        shows(&reader_rows(&mut app, 100, 30), &["假页面 0"]);
        // Closed: back to the settings page, highlight still on the accounts row
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty());
        assert_eq!(app.settings.as_ref().map(|s| (s.page, s.rows()[s.sel])), Some((SetPage::Main, SetRow::Accounts)));
        // Without an accounts page hooked up (the unit test default): pressing does nothing
        let mut plain = settings_app();
        ctrl(&mut plain, 's');
        press(&mut plain, KeyCode::Char('a'));
        assert!(plain.settings.is_some() && plain.overlays.is_empty());
    }

    #[test]
    fn problems_are_listed_only_when_there_are_some() {
        let mut app = settings_app();
        ctrl(&mut app, 's');
        assert!(!app.settings.as_ref().unwrap().rows().contains(&SetRow::Problems));
        app.settings = None;
        app.sync_failed = vec!["config.toml 有误".into()];
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Char('k')); // Up from the top = the last row
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.settings.as_ref().unwrap().page, SetPage::Problems);
        let rows = reader_rows(&mut app, 120, 36);
        assert!(rows.iter().any(|r| r.contains("config.toml 有误")), "{rows:#?}");
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.external.take(), Some(External::Config { path, .. }) if path.ends_with("config.toml")));
    }

    #[test]
    fn the_settings_popup_is_centered_on_desktop_and_fills_the_width_on_a_phone() {
        let mut app = settings_app();
        ctrl(&mut app, 's');
        let rows = reader_rows(&mut app, 120, 36);
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("{t}: {rows:#?}"));
        for t in [
            "设置",
            "t  主题         latte",
            "l  语言         zh  en",
            "Claude 1  Codex 1",
            "g  新分组目录   ~/workspace",
            "e  config.toml",
        ] {
            at(t);
        }
        // Auto English input is macOS-only
        assert_eq!(rows.iter().any(|r| r.contains("i  自动切英文   开")), cfg!(target_os = "macos"), "{rows:#?}");
        // Centered, the layer below dimmed (like Tab / S)
        let top = at("设置") - 2;
        // Counted by display width (there may be Chinese on the left)
        let col = |c: char| rows[top][..rows[top].find(c).unwrap()].width();
        let (l, r) = (col('╭'), col('╮'));
        assert!((l as i32 - (120 - 1 - r as i32)).abs() <= 1, "{l} {r}: {rows:#?}");
        let mut t = Terminal::new(TestBackend::new(120, 36)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        assert!(t.backend().buffer().content().iter().any(|c| c.fg == theme::pal().fade));
        let rows = reader_rows(&mut app, 46, 20);
        let head = rows.iter().position(|r| r.contains("设置")).unwrap();
        assert!(rows[head - 1].starts_with('╭') && rows[head - 1].ends_with('╮'), "{rows:#?}");
    }
}
