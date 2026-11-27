//! Pages other modules draw (plugins, MCPs, the trash, a plugin's commands): opening them, handing them keys, and what
//! their actions hand back.

use super::*;

impl App {
    /// M: this session's MCP
    pub(super) fn open_mcp(&mut self) {
        let Some(t) = self.deck_target(t!("查看 MCP", "view the MCPs of")) else { return };
        let Some(s) = self.current() else { return };
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只有 Claude / Codex 会话有 MCP", "only Claude / Codex sessions have MCPs"));
        }
        let info = (self.mcp_loader)(s);
        let v = mcp::View::new(t, s.dir.clone(), info.rows);
        self.open(Box::new(v));
    }

    /// P: the mounts / remote tasks menu (not aimed at a session, opens on a group too)
    pub(super) fn open_plugins(&mut self) {
        if let Some(page) = self.plugins_page {
            self.open(page());
        }
    }

    /// d on a group: the confirm box says subgroups and sessions inside are deleted with it, y deletes (see groups.rs)
    pub(super) fn ask_remove_group(&mut self) {
        if let Some(b) = self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)) {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        let Some(g) = self.group_row().map(String::from) else { return };
        if g == "-" {
            return self.flash(t!("这些是普通 tmux 会话,没有可删除的分组", "these are ordinary tmux sessions; no group to delete"));
        }
        // The scratch group stays (deleting a group above it would take it along)
        if self.scratch.as_deref().is_some_and(|s| groups::within(s, &g)) {
            return self.flash(t!("无法删除随手问所在的分组", "the scratch group can't be deleted"));
        }
        let rows: Vec<groups::Row> = (self.groups_loader)().into_iter().filter(|r| groups::within(&r.path, &g)).collect();
        if !rows.iter().any(|r| r.path == g) {
            return self.flash(&tr!("「{}」不是已登记的分组", "“{}” isn't a registered group", self.shown(&g)));
        }
        let archived = rows.iter().flat_map(|r| &r.archived).filter(|id| !self.trash.ids().any(|x| x == id.as_str())).count();
        let mine = |s: &&Session| s.sub.is_none() && !s.blank && !s.deck_id.is_empty() && groups::within(&s.group, &g);
        let busy = self.all.iter().filter(mine).filter(|s| s.state == State::Running).count();
        let running = self.all.iter().filter(mine).filter(|s| !s.state.gone()).map(|s| s.deck_id.clone()).collect();
        self.confirm = Some(Confirm::Group(GroupRemoval {
            profile: crate::data::REGISTERED.into(),
            group: g,
            kids: rows.len() - 1,
            sessions: rows.iter().map(|r| r.live).sum::<usize>() + archived,
            archived,
            busy,
            running,
        }));
    }

    /// b: the trash (see trash.rs)
    pub(super) fn open_trash(&mut self) {
        let page = crate::trash::Page::new(self.trash.clone(), epoch).with_notes(self.book.gone()).with_scratch(self.scratch.clone());
        self.open(Box::new(page));
    }

    /// Open a feature's own popup (see overlay.rs)
    pub(super) fn open(&mut self, o: Box<dyn Overlay>) {
        self.overlays.push(o);
    }

    /// `?` on the list: its keys by section, the plugins' commands first, aimed at the highlighted row (session / group; plain
    /// tmux sessions and extra windows count as elsewhere)
    pub(super) fn open_main_sheet(&mut self) {
        let row = self.current().filter(|_| !self.on_rail());
        let target = match (self.group_row(), row) {
            (Some(g), _) => crate::caps::Target { on: "group", session: String::new(), label: g.to_string() },
            (None, Some(s)) if s.sub.is_none() && !s.deck_id.is_empty() => {
                crate::caps::Target { on: "session", session: s.deck_id.clone(), label: s.title.clone() }
            }
            _ => crate::caps::Target { on: "global", ..Default::default() },
        };
        let asks = !self.on_group_row() && row.is_some_and(|s| s.state == State::Ask);
        let mut sheet = crate::sheet::main((self.plugin_menu)(), target, self.on_all_row());
        // The scratch group's router is the desk: called that on its row, its sessions' rows and the row for all groups
        let group = self.route_group().or(row.map(|s| s.group.as_str()));
        if self.on_all_row() || group.is_some_and(|g| self.is_scratch(g)) {
            sheet.rename("o", t!("打开 / 新建总台", "open / start the desk"));
        }
        if self.row_router().or(row).is_some_and(list::wakes_on_send) {
            sheet.rename("s", t!("发话唤醒灰色 @", "send to wake the gray @"));
            if self.on_route_row() {
                sheet.rename("o", t!("启动新一班", "start a new shift"));
            } else {
                sheet.rename("⏎", t!("启动新一班", "start a new shift"));
            }
        }
        // On a row waiting for you, s answers it
        if asks {
            sheet.rename("s", t!("回答", "answer"));
        }
        self.sheet = Some(sheet);
    }

    /// `?` on any other page: that page's keys
    pub(super) fn open_sheet(&mut self, keys: &[crate::sheet::Key]) {
        self.sheet = Some(crate::sheet::Sheet::page(keys));
    }

    /// A key while `?` is open: a row picked closes it and its key goes to the page below, as if pressed there
    pub(super) fn on_sheet_key(&mut self, k: KeyEvent) {
        let Some(s) = self.sheet.as_mut() else { return };
        let mut fx = Fx::default();
        if s.on_key(k, &mut fx) == Flow::Close {
            self.sheet = None;
        }
        self.apply(fx);
    }

    /// A plugin finished: views and long output go into the full-screen reader page; short output goes in the bottom bar
    pub fn plugin_done(&mut self, req: crate::caps::Req, ok: bool, out: String) {
        use aca_core::plugin::ItemKind;
        // List view: replace the open page with the new one; if none is open, open one (not for rereads or one already closed)
        if req.item.kind == ItemKind::View && req.item.rows {
            // The "running / rereading…" message is stale by now: the list is there on its own
            self.msg = None;
            if let Some(o) = self.overlays.iter_mut().find(|o| o.takes(&req)) {
                o.feed(ok, &out);
            } else if !req.refresh {
                self.open(Box::new(crate::caps::List::new(req, ok, &out)));
            }
            return;
        }
        // A list key finished: the result goes below that page and the list rereads; if the page is already closed, handle it as usual
        if let Some(view) = req.then.as_deref()
            && let Some(o) = self.overlays.iter_mut().find(|o| o.takes(view))
        {
            o.said(ok, &out);
            self.plugin_q.push(view.clone());
            return;
        }
        let title = req.title();
        let text = out.trim_end();
        let one_line = !text.contains('\n') && text.width() <= 100;
        let mark = if ok { "✓" } else { "✕" };
        match req.item.kind {
            ItemKind::Pane if ok => {}
            ItemKind::Command if text.is_empty() => self.flash(&if ok {
                tr!("✓ {title} 已完成", "✓ {title} done")
            } else {
                tr!("✕ {title} 出错", "✕ {title} failed")
            }),
            ItemKind::Command | ItemKind::Pane if one_line => self.flash(&format!("{mark} {title}:{text}")),
            _ => {
                let lines = ansi::parse_lines(&text.lines().map(str::to_string).collect::<Vec<_>>());
                // The rerun one is open: replace it, keeping the scroll position
                let scroll = self.reader.as_ref().filter(|r| r.plugin.as_ref() == Some(&req)).map_or(usize::MAX, |r| r.scroll);
                self.trace = None;
                self.reader = Some(Reader {
                    name: format!("{mark} {title}"),
                    lines,
                    scroll,
                    wrapped: vec![],
                    wrap_w: 0,
                    dirty: true,
                    view_h: 0,
                    diff: None,
                    plugin: Some(req),
                });
            }
        }
    }

    /// Keys go to the top popup; do what it asks, and when it closes the one below shows (none left and it came from the
    /// settings page: back to the settings page)
    pub(super) fn on_overlay_key(&mut self, k: KeyEvent) {
        let Some(top) = self.overlays.last_mut() else { return };
        // ?: this page's keys, unless it's typing or asking right now (then ? is its own key)
        if k.code == KeyCode::Char('?')
            && !k.modifiers.contains(KeyModifiers::CONTROL)
            && let Some(keys) = top.sheet()
        {
            return self.open_sheet(&keys);
        }
        let mut fx = Fx::default();
        if top.on_key(k, &mut fx) == Flow::Close {
            self.overlays.pop();
        }
        self.apply(fx);
        if self.overlays.is_empty() && self.external.is_none() {
            self.back_to_settings();
        }
    }

    /// What a popup / settings row asks the interface to do
    pub(super) fn apply(&mut self, fx: Fx) {
        if let Some(m) = fx.flash {
            self.flash(&m);
        }
        if let Some(j) = fx.job {
            match self.doing() {
                Some(b) => self.flash(&tr!("请稍候:{b}", "still busy: {b}")),
                None => self.job = Some(j),
            }
        }
        if let Some(x) = fx.external {
            self.external = Some(x);
        }
        if let Some(t) = fx.term {
            // The terminal takes the lower half of the screen: all popups are put
            // away, and closing the terminal doesn't return to the settings page
            self.overlays.clear();
            self.settings_back = None;
            self.term_req = Some(t);
        }
        if let Some(o) = fx.open {
            self.open(o);
        }
        if let Some(t) = fx.copy {
            self.clip = Some(Clip::Text(t));
        }
        if let Some(r) = fx.plugin {
            self.plugin_q.push(r);
        }
        if let Some(k) = fx.key {
            self.on_key(k);
        }
    }

    /// Background work finished, back from the editor: open popups reread
    pub(super) fn reload_overlays(&mut self) {
        for o in &mut self.overlays {
            o.reload();
        }
    }

    /// Before m moves a group: a session that follows its group would follow to the new group's recorded workspace, so record where
    /// it is now and leave it there after the move
    pub(super) fn stay_on_move(&mut self, name: &str, to: &str) {
        let mut after = self.all.clone();
        for s in after.iter_mut().filter(|s| s.family() == name) {
            s.group = to.to_string();
        }
        let sessions = self.settle(&after, &HashMap::new(), &[]);
        if !sessions.is_empty() {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&sessions, &[], &keep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{ctrl, fixture, keys, press, reader_rows, shows};

    fn plugin_req(id: &str, kind: aca_core::plugin::ItemKind) -> crate::caps::Req {
        crate::caps::Req {
            item: aca_core::plugin::MenuItem {
                plugin: "p.x".into(),
                plugin_name: "PX".into(),
                id: id.into(),
                kind,
                title: "看 CI".into(),
                ..Default::default()
            },
            session: "id-闲".into(),
            ..Default::default()
        }
    }

    #[test]
    fn question_mark_on_a_page_lists_its_keys_and_a_key_there_goes_to_that_page() {
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            app.open(Box::new(crate::ui::tests::FakePage(0)));
            press(&mut app, KeyCode::Char('?'));
            assert!(app.sheet.is_some() && app.overlays.len() == 1);
            shows(&reader_rows(&mut app, 100, 30), &["快捷键", "f  说一句", "n  再开一页"]);
            // f in the sheet: the sheet closes, the page gets f (and stays)
            press(&mut app, KeyCode::Char('f'));
            assert!(app.sheet.is_none() && app.overlays.len() == 1);
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("说一句"));
            // ⏎ on the second row: n opens another page over it
            press(&mut app, KeyCode::Char('?'));
            keys(&mut app, "j");
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.overlays.len(), 2);
            // A page that's typing takes ? as its own key
            app.overlays.clear();
            app.open(Box::new(crate::ui::tests::FakePage(1)));
            press(&mut app, KeyCode::Char('?'));
            assert!(app.sheet.is_none());
            press(&mut app, KeyCode::Esc);
            assert!(app.overlays.is_empty());
        });
    }

    #[test]
    fn every_page_ends_its_bottom_bar_with_esc_back_and_keys_and_popups_with_keys() {
        use aca_core::plugin::ItemKind;
        i18n::with(Lang::Zh, || {
            let mut app = fixture().with_tools(crate::ui::tests::rehome_tools);
            crate::ui::tests::select(&mut app, "跑");
            let last = |app: &mut App, w: u16| -> String {
                app.msg = None;
                reader_rows(app, w, 20).last().unwrap().trim_end().to_string()
            };
            // Output, diff, record, billing, a plugin's output: the page's keys on the left, Esc and ? at the right end
            for open in ["z", "x", "Z", "$", "plugin"] {
                match open {
                    "plugin" => app.plugin_done(plugin_req("ci", ItemKind::View), true, "build  ok".into()),
                    _ => keys(&mut app, open),
                }
                for w in [200, 46] {
                    let bar = last(&mut app, w);
                    assert!(bar.ends_with("Esc 返回 • ? 快捷键") && bar.width() <= w as usize, "{open} {w}: {bar}");
                    // Some of the page's own keys on the left, named as its ? names them
                    assert!(bar.split("Esc 返回").next().unwrap().trim().len() > 4, "{open} {w}: {bar}");
                }
                keys(&mut app, "⎋");
            }
            // Back on the list: its own bar, the keys as caps
            assert!(last(&mut app, 200).contains(&crate::ui::tests::caps("? 快捷键")));
            // Popups with a ?: their key line ends with it; short of room it goes first
            for open in ["F", "S", "n"] {
                keys(&mut app, open);
                let rows = reader_rows(&mut app, 120, 30);
                assert!(
                    rows.iter().any(|r| r.trim_end().trim_end_matches(['│', '░']).trim_end().ends_with("? 快捷键")),
                    "{open}: {rows:#?}"
                );
                keys(&mut app, "⎋");
            }
            keys(&mut app, "F");
            let narrow = reader_rows(&mut app, 24, 20);
            assert!(!narrow.iter().any(|r| r.contains("快捷键")) && narrow.iter().any(|r| r.contains("⏎ 筛选")), "{narrow:#?}");
        });
    }

    #[test]
    fn a_plugins_short_output_goes_to_the_bottom_bar_and_a_long_one_or_a_view_to_the_reader() {
        use aca_core::plugin::ItemKind;
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            app.plugin_done(plugin_req("ci", ItemKind::Command), true, "全绿\n".into());
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("✓ PX · 看 CI:全绿"));
            app.plugin_done(plugin_req("ci", ItemKind::Command), false, String::new());
            assert_eq!(app.msg.as_ref().map(|m| m.0.as_str()), Some("✕ PX · 看 CI 出错"));
            assert!(app.reader.is_none());
            // A view: opens the reading page even for a single line; ^R reruns, c copies all, Esc comes back
            app.plugin_done(plugin_req("ci", ItemKind::View), true, "build  ok\ntest   ok".into());
            app.msg = None;
            let rows = reader_rows(&mut app, 100, 20);
            shows(&rows, &["build  ok", "test   ok", "^R 重新运行", "c 复制"]);
            assert!(!rows[0].contains("1/") && rows[0].contains("✓ PX · 看 CI"), "{}", rows[0]);
            press(&mut app, KeyCode::Char('r'));
            assert!(app.plugin_q.is_empty());
            ctrl(&mut app, 'r');
            assert_eq!(app.plugin_q.len(), 1);
            press(&mut app, KeyCode::Char('c'));
            assert_eq!(app.clip, Some(Clip::Text("build  ok\ntest   ok".into())));
            // Nothing to go into: ⏎ just closes it
            press(&mut app, KeyCode::Enter);
            assert!(app.reader.is_none() && app.external.is_none());
        });
    }

    #[test]
    fn a_list_view_opens_as_its_own_page_and_its_keys_results_land_there_and_reload_it() {
        use aca_core::plugin::ItemKind;
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            let mut view = plugin_req("tasks", ItemKind::View);
            view.item.rows = true;
            let list = r#"{"rows":[{"id":"t1","mark":"●","text":"nas  备份","right":"在等"}]}"#;
            app.plugin_done(view.clone(), true, list.into());
            assert_eq!(app.overlays.len(), 1);
            assert!(app.reader.is_none());
            shows(&reader_rows(&mut app, 100, 30), &["● nas  备份", "在等"]);
            // One of its keys finished: the result goes at the bottom of that page, the list is queued to reload; no bottom-bar
            // message, no reading page
            let refresh = crate::caps::Req { refresh: true, ..view.clone() };
            let rm = crate::caps::Req { then: Some(Box::new(refresh.clone())), ..plugin_req("rm", ItemKind::Command) };
            app.plugin_done(rm, true, "已撤 nas 上的「备份」".into());
            assert!(app.msg.is_none() && app.reader.is_none());
            assert_eq!(app.plugin_q, vec![refresh.clone()]);
            app.plugin_done(refresh.clone(), true, r#"{"empty":"没有远端任务"}"#.into());
            assert_eq!(app.overlays.len(), 1);
            shows(&reader_rows(&mut app, 100, 30), &["没有远端任务", "已撤 nas 上的「备份」"]);
            // A reload that comes back after it closed: doesn't open another page
            app.overlays.clear();
            app.plugin_done(refresh, true, list.into());
            assert!(app.overlays.is_empty());
        });
    }
}
