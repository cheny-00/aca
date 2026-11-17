//! Leaving the list for a session's surroundings: a terminal or yazi in its directory, a window in its session, a clicked link,
//! and another client following the highlight.

use super::*;

impl App {
    /// Clicked a desktop notification (see clicked.rs): go into that session, move the highlight onto it (if it's in the list)
    pub fn open_clicked(&mut self, id: &str) {
        let Some(i) = self.all.iter().position(|s| s.deck_id == id && s.sub.is_none() && s.subagent.is_none() && !s.blank) else {
            return self.flash(t!("通知中的会话已不存在", "the notification's session is gone"));
        };
        if let Some(p) = self.view.iter().position(|&v| v == i) {
            self.select(Some(p));
        }
        self.go(i);
    }

    /// t: open a temporary terminal in the directory it's really working in, drawn below the list; if this row already has one, back to it.
    /// Works for stopped sessions too. On a group: opens at the group's path (see `group_dir`), one per group
    pub(super) fn open_term(&mut self) {
        self.digits.clear();
        if let Some(g) = self.group_row().map(String::from) {
            let key = group_key(&g);
            if self.terms.contains_key(&key) {
                self.term_on = Some(key);
                return;
            }
            let Some((_, dir, _)) = self.group_dir(&g) else {
                return self.flash(t!("无法确定该分组的目录", "no idea which directory this group is in"));
            };
            let w = Where { dir, ..Where::default() };
            self.term_req =
                Some(TermReq { key, family: String::new(), title: group_label(&g).to_string(), w, env: vec![], run: None, link: None });
            return;
        }
        let Some(s) = self.current() else { return };
        if self.terms.contains_key(&s.name) {
            self.term_on = Some(s.name.clone());
            return;
        }
        self.term_req = Some(TermReq {
            key: s.name.clone(),
            family: s.family().to_string(),
            title: s.title.clone(),
            w: crate::data::where_of(s),
            env: vec![],
            run: None,
            link: None,
        });
    }

    /// A login link showed up in a login terminal (a hidden one counts too): pop up a choice of browser to open it with
    pub(super) fn catch_links(&mut self) {
        let mut got: Vec<(String, crate::term::Link)> =
            self.terms.values_mut().filter_map(|t| t.new_link().map(|l| (t.title.clone(), l))).collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        for (title, link) in got {
            self.open(Box::new(crate::browser::Pick::new(&title, link)));
        }
    }

    /// Terminal open: every key goes to it (^C included), only Esc hides it — the shell keeps running, t again comes back
    pub(super) fn on_term_key(&mut self, key: &str, k: KeyEvent) {
        if k.code == KeyCode::Esc && k.modifiers.is_empty() {
            self.term_on = None;
        } else if let Some(t) = self.terms.get_mut(key) {
            t.key(k);
        }
    }

    /// T: open a new window running a shell in its session (same as prefix c), in the directory it's really working in
    pub(super) fn window_here(&mut self) {
        self.digits.clear();
        if self.on_group(t!("打开窗口", "open a window")) {
            return;
        }
        let Some(s) = self.current() else { return };
        if s.state.gone() {
            return self.flash(t!("未运行,先按回车启动", "it isn't running; press Enter to start it first"));
        }
        self.split = Some(self.split_of(s, false));
    }

    /// Split next to s's pane: a window row splits that pane; a session row with no known pane id splits its current pane
    fn split_of(&self, s: &Session, yazi: bool) -> Split {
        let target = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        // A row for an extra window in a session: account comes from the session it's in
        let head = if s.sub.is_none() { Some(s) } else { self.all.iter().find(|x| x.sub.is_none() && x.name == s.family()) };
        let env = head.map(crate::data::shell_env).unwrap_or_default();
        Split { yazi, name: s.family().to_string(), target, w: crate::data::where_of(s), side: self.screen.width >= 100, env }
    }

    /// y: open yazi in the directory it's really working in, q quits back to aca.
    /// Works for stopped sessions too (uses the directory it saved)
    pub(super) fn open_yazi(&mut self) {
        self.digits.clear();
        if self.on_group(t!("打开 yazi", "open yazi")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let mut sp = self.split_of(s, true);
        if s.state.gone() {
            sp.target.clear();
        }
        self.external = Some(External::Yazi(sp));
    }

    /// Interface in a popup: where this client is now (tmux session, pane) → highlight it
    /// (that pane's own row if it has one, else the session's row)
    pub fn highlight_here(&mut self, session: &str, pane: &str) {
        let row = self.all.iter().find(|s| !pane.is_empty() && s.pane == pane && s.tmux == session);
        let Some(name) = row.or_else(|| self.all.iter().find(|s| s.sub.is_none() && s.tmux == session)).map(|s| s.name.clone()) else {
            return;
        };
        self.selected = Some(name);
        let all = std::mem::take(&mut self.all);
        self.set_sessions(all);
    }

    /// Start following (see follow.rs): session / pane is where this client is now. `highlight_here` first; whichever row the highlight
    /// lands on is taken as "the original one"
    pub fn follow_from(&mut self, session: &str, pane: &str) {
        let row = self.current().filter(|_| !self.on_group_row()).map(|s| s.name.clone()).unwrap_or_default();
        self.following = Some(Follow::new(Place { row, session: session.into(), pane: pane.into() }));
    }

    /// Where the highlighted row should take the client: group rows and sessions not running = nowhere
    fn follow_want(&self) -> Option<Place> {
        let s = self.current().filter(|_| !self.on_group_row())?;
        // A subagent row has no pane: take it to its session's pane
        let pane = match &s.subagent {
            Some(_) => self.all.iter().find(|x| x.sub.is_none() && x.name == s.family()).map_or_else(String::new, |x| x.pane.clone()),
            None => s.pane.clone(),
        };
        (!s.tmux.is_empty() && !s.state.gone()).then(|| Place { row: s.name.clone(), session: s.tmux.clone(), pane })
    }

    /// One step of following (the run loop asks each round): once the highlight settles and it's time to move, say where to switch to. Not
    /// while sliding or typing a filter
    pub fn follow_step(&mut self) -> Option<Place> {
        let want = if self.sliding() || self.filtering { None } else { self.follow_want() };
        let f = self.following.as_mut()?;
        let to = f.step(want)?;
        // Passing by doesn't count as seen: switching there makes tmux record it as "attached", and core's "finished, not looked at" goes
        // away with it → mark it unread; only ⏎ stay or closing on it clears that
        let passing = !f.is_home(&to.session);
        if let Some(key) =
            self.all.iter().find(|s| passing && s.sub.is_none() && s.tmux == to.session && s.state == State::Unread).map(notes::key)
        {
            let _ = self.reads.mark_unread(&key, &self.keep_marks());
        }
        Some(to)
    }

    /// Closed while following (^\ / qq; ⏎ and Esc each go elsewhere): you stay on the session showing beside; it counts as seen
    pub fn follow_done(&mut self) {
        if let Some(session) = self.following.as_ref().and_then(|f| f.stayed()).map(String::from) {
            self.mark_seen(&session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{ctrl, press, reader_rows, said, select, sessions, target, with_windows};

    #[test]
    fn shift_t_opens_a_new_window_in_its_session_in_its_real_work_dir() {
        let mut app = with_windows();
        // On the window row: opens in the session it belongs to, a new window running a shell (not a split)
        reader_rows(&mut app, 120, 30);
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('T'));
        let sp = app.split.take().unwrap();
        assert_eq!((sp.name.as_str(), sp.yazi), ("闲", false));
        assert_eq!(sp.w.dir, "/p/work"); // the search for the directory it's really working in starts here
        // Same on a phone
        reader_rows(&mut app, 46, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('T'));
        let sp = app.split.take().unwrap();
        assert_eq!((sp.name.as_str(), sp.yazi), ("闲", false));
        // A stopped one can't be split off
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('T'));
        assert!(app.split.is_none() && app.msg.as_ref().unwrap().0.contains("未运行"), "{:?}", app.msg);
    }

    #[test]
    fn a_login_link_pops_up_over_the_terminal_and_takes_the_keys_until_it_closes() {
        let mut app = with_windows();
        let file = std::env::temp_dir().join(format!("aca-ui-link-{}.url", std::process::id()));
        std::fs::write(&file, "https://claude.com/cai/oauth/authorize?x=1").unwrap();
        let mut t = crate::term::Term::spawn("/", &[], 5, 20).unwrap();
        t.title = "登录 Claude Code · ding".into();
        t.link = Some(crate::term::Catch { file: file.clone(), screen: false });
        app.terms.insert("account:ding".into(), t);
        app.term_on = Some("account:ding".into());
        app.catch_links();
        assert_eq!(app.overlays.len(), 1);
        // The same link doesn't pop up again
        app.catch_links();
        assert_eq!(app.overlays.len(), 1);
        // Keys go to the popup first: Esc closes it, the terminal stays open
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.term_on.as_deref() == Some("account:ding"));
        assert!(app.msg.as_ref().is_some_and(|(m, _)| m.contains("链接仍在登录终端中")), "{:?}", app.msg);
        // Esc again: hides the terminal
        press(&mut app, KeyCode::Esc);
        assert!(app.term_on.is_none());
        // Terminal closed: the link file is deleted with it
        app.terms.clear();
        assert!(!file.exists());
    }

    #[test]
    fn y_opens_yazi_where_it_really_works() {
        let mut app = with_windows();
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('y'));
        let Some(External::Yazi(sp)) = app.external.take() else { panic!("{:?}", app.msg) };
        assert_eq!(sp.w.dir, "/p/work"); // the directory it's really working in is looked up from here when handing over the terminal
        // Switched in the popup to run it in a split: it's that pane that gets split
        assert_eq!((sp.name.as_str(), sp.target.as_str(), sp.yazi), ("闲", "%2", true));
        // A stopped one can open too (directory from what it recorded), just with no pane to split
        let mut v = sessions();
        v[3].state = State::Stopped; // 「闲」
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('y'));
        let Some(External::Yazi(sp)) = app.external.take() else { panic!("{:?}", app.msg) };
        assert_eq!(sp.target, "");
        // A fold row doesn't know which one to open
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        press(&mut app, KeyCode::Char('y'));
        assert!(app.external.is_none() && app.msg.as_ref().unwrap().0.contains("先展开"), "{:?}", app.msg);
    }

    #[test]
    fn the_popup_opens_on_where_that_client_is() {
        let mut app = with_windows();
        app.highlight_here("$闲", "%2"); // on 「闲」's #2 → highlight that row
        assert_eq!(app.current().unwrap().title, "zsh");
        app.highlight_here("$跑", "%9"); // that pane has no row of its own → the session's row
        assert_eq!(app.current().unwrap().title, "跑");
        app.highlight_here("$别处", ""); // not in the list → no change
        assert_eq!(app.current().unwrap().title, "跑");
    }

    fn place(row: &str, session: &str, pane: &str) -> crate::follow::Place {
        crate::follow::Place { row: row.into(), session: session.into(), pane: pane.into() }
    }

    /// The popup opens on a pane of 「闲」 that has no row of its own (%9), following
    fn following() -> App {
        let mut app = with_windows();
        reader_rows(&mut app, 56, 30);
        app.highlight_here("$闲", "%9");
        app.follow_from("$闲", "%9");
        app
    }

    #[test]
    fn following_takes_the_client_to_the_row_the_highlight_rests_on() {
        let mut app = following();
        assert_eq!((app.follow_step(), app.current().unwrap().title.as_str()), (None, "闲"), "刚打开:不动");
        // No switch while scrolling, switch once settled; once switched, no switching again
        select(&mut app, "跑");
        app.moved = Some((Instant::now(), Duration::ZERO));
        assert_eq!(app.follow_step(), None);
        app.moved = None;
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        assert_eq!(app.follow_step(), None);
        // The row of another window in the session: lands on that pane
        select(&mut app, "zsh");
        assert_eq!(app.follow_step(), Some(place("闲\u{1}%2", "$闲", "%2")));
        // Back to the row it opened on: lands back on the original pane
        select(&mut app, "闲");
        assert_eq!(app.follow_step(), Some(place("闲", "$闲", "%9")));
        // Typing a filter: no switch; switch only once Enter settles it
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('跑'));
        assert_eq!((app.follow_step(), app.current().unwrap().title.as_str()), (None, "跑"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        press(&mut app, KeyCode::Esc); // clears the filter, doesn't exit
        assert!(!app.quit);
        // On a group: the side stays as it is
        press(&mut app, KeyCode::Char('g'));
        assert!(app.on_group_row());
        assert_eq!(app.follow_step(), None);
        // A session that isn't running: can't switch to it
        let mut v = sessions();
        v[2].state = State::Stopped; // 「跑」
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        app.follow_from("$闲", "");
        select(&mut app, "跑");
        assert_eq!(app.follow_step(), None);
    }

    #[test]
    fn esc_while_following_goes_back_and_the_popup_key_stays() {
        // Esc: go back to where it was when opened, then exit; the one left behind doesn't count as seen
        let mut app = following();
        select(&mut app, "跑");
        app.follow_step();
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.quit, app.follow_step()), (true, Some(place("闲", "$闲", "%9"))));
        app.follow_done();
        assert_eq!(app.reads.read_at("id-跑"), None);
        // ^\ (arrives as ^4 in the terminal): close, staying on the one shown beside; it counts as seen
        for key in ['4', '\\'] {
            let mut app = following();
            select(&mut app, "跑");
            app.follow_step();
            ctrl(&mut app, key);
            assert_eq!((app.quit, app.follow_step()), (true, None));
            app.follow_done();
            assert!(app.reads.read_at("id-跑").is_some());
        }
        // Pressed in the keys popup it's only looked up
        let mut app = following();
        press(&mut app, KeyCode::Char('?'));
        ctrl(&mut app, '4');
        assert!(!app.quit);
        // Closed without moving: no need to record the original one
        let mut app = following();
        ctrl(&mut app, '4');
        app.follow_done();
        assert_eq!((app.quit, app.reads.read_at("id-闲")), (true, None));
        // Not following, not in the popup: ^\ doesn't exit
        let mut app = with_windows();
        ctrl(&mut app, '4');
        assert_eq!(app.quit, crate::data::in_popup());
    }

    #[test]
    fn passing_by_an_unread_session_while_following_leaves_it_unread() {
        let mut v = sessions();
        v[2].state = State::Unread; // 「跑」: finished, not looked at yet
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        app.follow_from("$闲", "");
        select(&mut app, "跑");
        assert_eq!(app.follow_step(), Some(place("跑", "$跑", "")));
        assert!(app.reads.unread("id-跑"));
        // Passing over others: ones that aren't "finished, not looked at yet" aren't recorded
        select(&mut app, "问");
        app.follow_step();
        assert_eq!(app.reads.mark("id-问"), None);
        // Back to the unread one, closing on it: it's seen
        select(&mut app, "跑");
        app.follow_step();
        ctrl(&mut app, '4');
        app.follow_done();
        assert!(app.reads.read_at("id-跑").is_some());
    }

    #[test]
    fn following_draws_no_preview_and_shift_l_leaves_the_saved_layouts_alone() {
        let mut app = with_windows();
        // Not following: on a narrow screen it splits top/bottom, the preview takes the bottom half. Following: only the bottom
        // row is left, saying what it's doing
        let strip = |rows: &[String]| rows.iter().position(|r| r.starts_with('─')).unwrap();
        assert!(strip(&reader_rows(&mut app, 56, 30)) < 20);
        app.highlight_here("$闲", "%9");
        app.follow_from("$闲", "%9");
        let rows = reader_rows(&mut app, 56, 30);
        assert_eq!((app.mode(), strip(&rows)), (Preview::Off, 28), "{rows:#?}");
        // The bottom bar says what these two keys do while following
        assert!(rows[29].contains("Esc 返回") && rows[29].contains("⏎ 留下"), "{}", rows[29]);
        let saved = app.view_state();
        press(&mut app, KeyCode::Char('L'));
        assert!(app.mode() != Preview::Off);
        assert!(app.view_state() == saved, "L 换的只管这一次");
    }

    #[test]
    fn a_clicked_notification_goes_into_its_session_wherever_the_highlight_is() {
        let mut app = with_windows();
        select(&mut app, "htop");
        // kitty's reply arrives in the input as Alt+] … Alt+\: pick it out, don't treat it as keys
        let k = |c: char, m: KeyModifiers| Event::Key(KeyEvent::new(KeyCode::Char(c), m));
        let mut batch = vec![k(']', KeyModifiers::ALT)];
        batch.extend("99;i=aca-id-闲;".chars().map(|c| k(c, KeyModifiers::NONE)));
        batch.push(k('\\', KeyModifiers::ALT));
        for osc in crate::clicked::take(&mut batch) {
            app.open_clicked(crate::clicked::session(&osc).unwrap());
        }
        assert!(batch.is_empty());
        assert_eq!(app.current().map(|s| s.title.as_str()), Some("闲"));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        // A stopped session: start again continuing the original conversation, same as Enter
        app.all.iter_mut().find(|s| s.title == "错" && s.sub.is_none()).unwrap().state = State::Stopped;
        app.open_clicked("id-错");
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("错")), None));
        // Session removed: say so, no change
        app.open_clicked("id-没了");
        assert_eq!((app.attach.take(), app.start.take()), (None, None));
        assert_eq!(said(&app), "通知中的会话已不存在");
    }
}
