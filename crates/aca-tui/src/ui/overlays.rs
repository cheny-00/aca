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
        let rows: Vec<groups::Row> = (self.groups_loader)().into_iter().filter(|r| groups::within(&r.path, &g)).collect();
        if !rows.iter().any(|r| r.path == g) {
            return self.flash(&tr!("「{}」不是已登记的分组", "“{}” isn't a registered group", group_label(&g)));
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
        self.open(Box::new(crate::trash::Page::new(self.trash.clone(), epoch)));
    }

    /// Open a feature's own popup (see overlay.rs)
    pub(super) fn open(&mut self, o: Box<dyn Overlay>) {
        self.overlays.push(o);
    }

    /// `:`: the command palette (aca's actions + plugins'), aimed at the highlighted row (session / group; plain tmux sessions and
    /// extra windows count as elsewhere)
    pub(super) fn open_caps(&mut self) {
        let target = match (&self.head, self.current()) {
            (Some(g), _) => crate::caps::Target { on: "group", session: String::new(), label: g.clone() },
            (None, Some(s)) if !self.on_fold_row() && s.sub.is_none() && !s.deck_id.is_empty() => {
                crate::caps::Target { on: "session", session: s.deck_id.clone(), label: s.title.clone() }
            }
            _ => crate::caps::Target { on: "global", ..Default::default() },
        };
        self.open(Box::new(crate::caps::Menu::new(aca_core::plugin::menu(), target)));
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
                    help: false,
                    view_h: 0,
                    diff: None,
                    hit: None,
                    plugin: Some(req),
                });
            }
        }
    }

    /// Keys go to the top popup; do what it asks, and when it closes the one below shows (none left and it came from the
    /// settings page: back to the settings page)
    pub(super) fn on_overlay_key(&mut self, k: KeyEvent) {
        let Some(top) = self.overlays.last_mut() else { return };
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
    use crate::ui::tests::{fixture, press, reader_rows, shows};

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
    fn colon_opens_the_plugin_menu_on_the_highlighted_session() {
        i18n::with(Lang::Zh, || {
            let mut app = fixture();
            app.selected = Some("闲".into());
            app.rebuild();
            press(&mut app, KeyCode::Char(':'));
            shows(&reader_rows(&mut app, 100, 30), &["命令 · 对「闲」", "n  新建", "aca"]);
            press(&mut app, KeyCode::Esc);
            assert!(app.overlays.is_empty());
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
            // A view: opens the reading page even for a single line; r reruns, c copies all, Esc comes back
            app.plugin_done(plugin_req("ci", ItemKind::View), true, "build  ok\ntest   ok".into());
            app.msg = None;
            let rows = reader_rows(&mut app, 100, 20);
            shows(&rows, &["build  ok", "test   ok", "r 重新运行", "c 复制"]);
            assert!(!rows[0].contains("1/") && rows[0].contains("✓ PX · 看 CI"), "{}", rows[0]);
            press(&mut app, KeyCode::Char('r'));
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
