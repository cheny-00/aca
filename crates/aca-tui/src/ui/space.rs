//! Workspaces: switching between them, sending sessions and groups to one, and the menu that lists them.

use super::*;

impl App {
    /// Workspaces that can be listed (ones with sessions, plus the current one), not h; sorted by key (digits before letters)
    pub(super) fn space_list(&self) -> Vec<char> {
        let mut v: Vec<char> = self.all.iter().map(|s| s.space).chain([self.space]).filter(|&c| c != spaces::HIDDEN).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Only the first workspace is in use and it was never renamed: nothing on screen needs to name it
    pub(super) fn lone_space(&self) -> bool {
        self.space == spaces::HOME && !self.spaces.renamed(spaces::HOME) && self.space_list().len() <= 1
    }

    /// What to call the current workspace in the top bar / list: with only 1 in use and never renamed it's "all groups", as before
    pub(super) fn space_label(&self) -> String {
        if self.lone_space() { t!("全部分组", "All groups").into() } else { self.spaces.name(self.space) }
    }

    /// Picked in S: switch to this workspace, highlight the session last highlighted there
    fn switch_space(&mut self, c: char) {
        self.space_menu = None;
        if c != self.space {
            if let Some(n) = self.selected.take() {
                self.space_sel.insert(self.space, n);
            }
            self.space = c;
            self.group = None;
            self.left_at.clear();
            self.hop_back = None;
            self.rail_from = None;
            self.selected = self.space_sel.get(&c).cloned();
            self.head = None;
            self.list.select(Some(0));
            self.rebuild();
        }
        let m = tr!("工作区:{}", "workspace: {}", self.spaces.name(c));
        self.flash(&m);
    }

    /// ^` (in the popup): this client switches back to the previous session, landing in the cell it left from; switching away quits aca
    pub(super) fn back_session(&mut self) {
        match crate::data::last_session() {
            Some(name) => {
                self.focus = None;
                self.attach = Some(name);
            }
            None => self.flash(t!("没有上一个会话", "no previous session")),
        }
    }

    /// Q+key: send the highlighted session to that workspace (child row = the session it's in); child sessions never placed on their
    /// own go along (and theirs too). On a group = this workspace's whole group, recording the group too, so new ones opened in it
    /// later (including `aca new` in a terminal) land there directly. You stay here
    fn send_to_space(&mut self, c: char) {
        if c == self.space {
            return self.flash(t!("已在该工作区", "already in this workspace"));
        }
        // v multi-selecting: send the selected ones
        let many = self.picking().then(|| self.chosen());
        let fold = many.is_none() && self.on_group_row();
        let cur = self.current().filter(|_| many.is_none());
        let (fam, group) = cur.map(|s| (s.family().to_string(), s.group.clone())).unwrap_or_default();
        // Empty group: no sessions to send, just record the group
        let blank = cur.filter(|s| s.blank);
        let heads = || self.all.iter().filter(|x| x.sub.is_none() && !x.blank);
        let go: Vec<&Session> = match &many {
            Some(ids) => heads().filter(|x| !x.deck_id.is_empty() && ids.contains(&x.deck_id)).collect(),
            None if fold => heads().filter(|x| x.space == self.space && x.group == group).collect(),
            None => heads().filter(|x| x.name == fam).collect(),
        };
        let Some(first) = go.first().or(blank.as_ref()) else { return };
        let what = match () {
            _ if many.is_some() => ops::named(&first.title, go.len()),
            _ if fold => tr!("「{}」整组", "group “{}”", self.shown(&group)),
            _ => tr!("「{}」", "“{}”", first.title),
        };
        let mut names: HashSet<&str> = go.iter().map(|x| x.name.as_str()).collect();
        // Add child sessions never placed on their own, level by level, until no more are added
        loop {
            let more: Vec<&str> = heads()
                .filter(|x| !names.contains(x.name.as_str()) && self.spaces.of(&notes::key(x)).is_none())
                .filter(|x| x.up.parent().is_some_and(|p| names.contains(p)))
                .map(|x| x.name.as_str())
                .collect();
            if more.is_empty() {
                break;
            }
            names.extend(more);
        }
        let want: HashMap<String, char> = heads().filter(|x| names.contains(x.name.as_str())).map(|x| (notes::key(x), c)).collect();
        // A group of the same name may be in several profiles, one record each; sending back to 1 = group not recorded
        let mut groups: Vec<(spaces::Group, Option<char>)> = vec![];
        if fold {
            for x in go.iter().chain(blank.as_ref()) {
                let g = (x.profile.clone(), x.group.clone());
                if !groups.iter().any(|(y, _)| *y == g) {
                    groups.push((g, (c != spaces::HOME).then_some(c)));
                }
            }
        }
        let sessions = self.settle(&self.all, &want, &groups);
        let keep = self.keep_marks();
        // Put back on undo: what these sessions and groups had recorded before
        let before = (
            sessions.iter().map(|(k, _)| (k.clone(), self.spaces.of(k))).collect(),
            groups.iter().map(|(g, _)| (g.clone(), self.spaces.group_of(&g.0, &g.1))).collect(),
        );
        let m = match self.spaces.assign(&sessions, &groups, &keep) {
            Ok(()) => {
                let what = format!("{what} → {}", self.spaces.name(c));
                self.remember(Undo::Space { what: what.clone(), sessions: before.0, groups: before.1 });
                if many.is_some() {
                    self.drop_picks();
                }
                what
            }
            Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// Which sessions' records must change so each session lands in the workspace `want` says (ones not given stay where they are);
    /// groups are group records to change at the same time. after = the sessions after the change (when moving groups, the group has
    /// already changed). Records kept minimal: not recorded if it would be there anyway without one
    pub(super) fn settle(
        &self,
        after: &[Session],
        want: &HashMap<String, char>,
        groups: &[(spaces::Group, Option<char>)],
    ) -> Vec<(String, Option<char>)> {
        let mut next = self.spaces.clone().detached();
        let boxed = Boxed::load();
        for (g, c) in groups {
            next.set_group(g, *c);
        }
        let heads: Vec<&Session> = after.iter().filter(|s| s.sub.is_none() && !s.blank).collect();
        let goal = |s: &Session| want.get(&notes::key(s)).copied().unwrap_or(s.space);
        // Record it if it's not where it should be. Child sessions follow their parent, and a parent's record change changes them, so
        // repeat until nothing changes (any depth takes two passes: once a record above changes, the ones below that moved with it
        // record themselves in the second pass, affecting no one else)
        for _ in 0..3 {
            let mut changed = false;
            for s in &heads {
                if space_of(after, &next, &boxed, s, true) != goal(s) {
                    next.set(&notes::key(s), Some(goal(s)));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for s in &heads {
            let k = notes::key(s);
            if next.of(&k).is_some_and(|c| c == space_of(after, &next, &boxed, s, false)) {
                next.set(&k, None);
            }
        }
        heads.iter().map(|s| notes::key(s)).filter(|k| next.of(k) != self.spaces.of(k)).map(|k| (k.clone(), next.of(&k))).collect()
    }

    /// A newly created / forked session lands in the current workspace (even if its group is recorded elsewhere)
    pub fn adopt(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        // One of a sandbox with its own workspace goes there instead (see `Boxed`)
        if let Ok(s) = aca_core::store::Store::open().and_then(|st| st.get(id))
            && aca_core::sandbox::space(&aca_core::config::load(), &s.tool, &s.account).is_some_and(spaces::is_key)
        {
            return;
        }
        let keep = self.keep_marks();
        let sessions = if self.all.iter().any(|s| s.sub.is_none() && notes::key(s) == id) {
            self.settle(&self.all, &HashMap::from([(id.to_string(), self.space)]), &[])
        } else {
            // Not in the list yet: record it first; the extra record is cleared next time another session is sent
            vec![(id.to_string(), Some(self.space))]
        };
        let _ = self.spaces.assign(&sessions, &[], &keep);
    }

    /// The key after q / Q: q after q quits; a workspace key after Q sends there; other keys (Esc etc.) cancel
    pub(super) fn on_pending_key(&mut self, p: Pending, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match (p, k.code) {
            (Pending::Quit, KeyCode::Char('q')) if !ctrl => self.quit = true,
            (Pending::Send, KeyCode::Char(c)) if !ctrl && spaces::is_key(c) => self.send_to_space(c),
            (Pending::Copy, KeyCode::Char(c)) if !ctrl => self.copy_by(c),
            _ => {}
        }
    }

    /// S's workspace list: ↑↓ pick, ⏎ switches, typing a key switches too (even unlisted ones like h); ^R renames
    pub(super) fn on_space_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.space_menu else { return };
        let v = self.space_list();
        let n = v.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('?') if !ctrl => self.open_sheet(&super::screen::space_keys()),
            KeyCode::Down => self.space_menu = Some((sel + 1) % n),
            KeyCode::Up => self.space_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => self.switch_space(v[sel.min(n - 1)]),
            KeyCode::Char('r') if ctrl => {
                let c = v[sel.min(n - 1)];
                self.space_menu = None;
                self.prompt = Some(Prompt { text: self.spaces.name(c), ask: Ask::Space(c), hint: String::new() });
            }
            KeyCode::Char(c) if !ctrl && spaces::is_key(c) => self.switch_space(c),
            KeyCode::Esc | KeyCode::Char('S' | 'q') => self.space_menu = None,
            _ => {}
        }
    }

    pub(super) fn open_space_menu(&mut self) {
        let v = self.space_list();
        self.space_menu = Some(v.iter().position(|&c| c == self.space).unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::screen::top_bar;
    use crate::ui::tests::{at, caps, fun_in_h, jump_to_group, keys, life_and_work, mk, reader_rows, select, shows, space, titles};

    #[test]
    fn s_and_a_key_is_the_one_way_to_switch_and_q_waits_only_for_a_second_q() {
        let mut app = App::new(String::new(), life_and_work());
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        select(&mut app, "私");
        keys(&mut app, "Qh");
        select(&mut app, "生活二");
        // [ ] ` and q followed by a workspace's key: nothing happens
        app.msg = None;
        keys(&mut app, "[]`ql");
        assert_eq!((app.space, app.pending, app.msg.as_ref(), at(&app)), ('1', None, None, "生活二".into()));
        // After q the bottom bar says what the next key does; anything but q cancels
        keys(&mut app, "q");
        shows(&reader_rows(&mut app, 60, 20), &[&caps("q 退出 • Esc 取消")]);
        keys(&mut app, "⎋");
        assert!(app.pending.is_none() && !app.quit);
        // S and a key: a listed one, the hidden h, one never used yet (made on the spot); the highlight comes back too
        keys(&mut app, "Sl");
        assert_eq!(app.space, 'l');
        keys(&mut app, "Sh");
        assert_eq!((app.space, titles(&app)), ('h', vec!["私".to_string()]));
        keys(&mut app, "Sx");
        assert_eq!((app.space, titles(&app).len()), ('x', 0));
        keys(&mut app, "S1");
        assert_eq!((app.space, at(&app)), ('1', "生活二".into()));
    }

    #[test]
    fn capital_q_on_a_group_header_sends_the_whole_group_and_new_ones_land_here() {
        let mut app = App::new(String::new(), life_and_work());
        jump_to_group(&mut app, "fun");
        keys(&mut app, "Q2");
        assert_eq!(titles(&app), ["工作"]);
        keys(&mut app, "S2");
        assert_eq!(titles(&app), ["zsh", "子", "生活", "生活二", "私"]);
        app.adopt("id-新"); // Made in 2
        let mut v = life_and_work();
        v.push(mk("新", "work", State::Idle, 1));
        app.set_sessions(v);
        assert!(titles(&app).contains(&"新".to_string()));
        keys(&mut app, "S1");
        assert_eq!(titles(&app), ["工作"]);
    }

    #[test]
    fn capital_s_lists_workspaces_and_ctrl_r_renames() {
        let mut app = App::new(String::new(), life_and_work());
        assert!(top_bar(&app, 80).to_string().contains(" 全部分组 ")); // Only 1 in use: the label reads all groups
        select(&mut app, "生活");
        keys(&mut app, "Ql");
        select(&mut app, "私");
        keys(&mut app, "Qh");
        keys(&mut app, "S");
        assert_eq!(app.space_list(), ['1', 'l']); // h isn't listed
        assert_eq!(app.space_menu, Some(0));
        keys(&mut app, "↓");
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        keys(&mut app, "生活⏎");
        assert_eq!(app.spaces.name('l'), "生活");
        keys(&mut app, "Sl"); // Switch straight there by key
        assert_eq!(app.space, 'l');
        assert!(top_bar(&app, 80).to_string().contains(" 生活 "));
        keys(&mut app, "Sh"); // Unlisted ones can be pressed directly too
        assert_eq!(titles(&app), ["私"]);
    }

    #[test]
    fn a_group_sent_to_h_takes_sessions_opened_later_along() {
        let mut app = fun_in_h();
        assert_eq!(titles(&app), ["工作"]);
        assert_eq!(app.spaces.group_of("default", "fun"), Some('h'));
        assert_eq!(app.spaces.of("id-生活"), None); // Following the group is enough, no need to record each one
        // Opened in fun without the interface (aca new in a terminal, a plugin): follows the group into h, doesn't show up in 1
        let mut v = life_and_work();
        v.push(mk("外面开的", "fun", State::Unread, 1));
        app.set_sessions(v);
        assert_eq!(titles(&app), ["工作"]);
        assert!(!top_bar(&app, 80).to_string().contains("未读"));
        assert_eq!(space(&app, "外面开的"), 'h');
    }
}
