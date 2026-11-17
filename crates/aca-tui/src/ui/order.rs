//! Arranging the list: switching groups, the group menu and the only-these menu, pins, and moving rows and groups, written in
//! the background one at a time.

use super::*;

impl App {
    /// Cycle: all groups → each group (in section order) → all groups
    pub(super) fn cycle_group(&mut self, d: isize) {
        let gs = self.groups();
        let cur = self.group.as_ref().and_then(|g| gs.iter().position(|n| n == g)).map_or(0, |p| p as isize + 1);
        let next = (cur + d).rem_euclid(gs.len() as isize + 1);
        self.set_group(if next == 0 { None } else { Some(gs[next as usize - 1].clone()) });
    }

    /// ⇧Tab: switch back to the group just looked at (same as Tab ⏎); again switches back
    pub(super) fn back_group(&mut self) {
        match self.prev_group.clone() {
            Some(Some(g)) if !self.groups().contains(&g) => {
                self.flash(&tr!("「{}」中已没有会话", "no sessions left in {}", group_label(&g)))
            }
            Some(g) => self.set_group(g),
            None => self.flash(t!("还没有切换过分组", "no group switched yet")),
        }
    }

    /// Switch to showing only this group (None = all groups), remembering the one shown before
    pub(super) fn set_group(&mut self, to: Option<String>) {
        if to != self.group {
            self.prev_group = Some(std::mem::replace(&mut self.group, to));
        }
        let m = match &self.group {
            Some(g) => tr!("分组:{}", "group: {}", group_label(g)),
            None => t!("全部分组", "all groups").into(),
        };
        self.flash(&m);
        self.rebuild();
    }

    /// Rows of the group list: (letter, group); row 0 is all groups (Space)
    pub(super) fn group_rows(&self) -> Vec<(char, Option<String>)> {
        let gs = self.groups();
        let keys = group_keys(&gs);
        std::iter::once((' ', None)).chain(keys.into_iter().zip(gs.into_iter().map(Some))).collect()
    }

    /// Tab: this workspace's group list, the highlight on the group last looked at (on the current one if never switched)
    /// `F`: the small only-one-state list. The highlight is on the state picked now ("All" if none)
    pub(super) fn open_only_menu(&mut self) {
        self.digits.clear();
        self.only_menu = Some(self.status.and_then(|p| PICKS.iter().position(|&x| x == p)).map_or(0, |i| i + 1));
    }

    /// In the only-state list: ↑↓ j k pick, ⏎ shows only it (on the one already picked = cancel), the key at the start of a row picks
    /// directly, Space = all; Esc / F / q close
    pub(super) fn on_only_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.only_menu else { return };
        let n = PICKS.len() + 1;
        let choose = |me: &mut Self, p: Option<Pick>| {
            me.only_menu = None;
            match (p, me.status) {
                (Some(p), _) => me.toggle_status(p),
                (None, Some(cur)) => me.toggle_status(cur),
                (None, None) => {}
            }
        };
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.only_menu = Some((sel + 1) % n),
            KeyCode::Up | KeyCode::Char('k') => self.only_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => choose(self, if sel == 0 { None } else { Some(PICKS[sel - 1]) }),
            KeyCode::Char(' ') => choose(self, None),
            KeyCode::Esc | KeyCode::Char('F' | 'q') => self.only_menu = None,
            KeyCode::Char(c) => {
                if let Some(&p) = PICKS.iter().find(|p| p.label().0 == c) {
                    choose(self, Some(p));
                }
            }
            _ => {}
        }
    }

    /// How many in this workspace are in this state (not counting subagents or
    /// placeholder empty rows; same as the count at the right of the top bar)
    pub(super) fn pick_count(&self, p: Option<Pick>) -> usize {
        self.all.iter().filter(|s| s.subagent.is_none() && !s.blank && s.space == self.space && p.is_none_or(|p| p.hit(s))).count()
    }

    pub(super) fn open_group_menu(&mut self) {
        let rows = self.group_rows();
        let at = |g: &Option<String>| rows.iter().position(|(_, x)| x == g);
        self.group_menu = Some(self.prev_group.as_ref().and_then(at).or_else(|| at(&self.group)).unwrap_or(0));
    }

    /// In the group list: ↑↓ pick, ⏎ switches there, typing a letter switches too, Space = all groups; Esc / Tab / q close
    pub(super) fn on_group_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.group_menu else { return };
        let rows = self.group_rows();
        let n = rows.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let pick = |me: &mut Self, g: Option<String>| {
            me.group_menu = None;
            me.set_group(g);
        };
        match k.code {
            KeyCode::Down => self.group_menu = Some((sel + 1) % n),
            KeyCode::Up => self.group_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => pick(self, rows[sel.min(n - 1)].1.clone()),
            KeyCode::Char(' ') => pick(self, None),
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') => self.group_menu = None,
            KeyCode::Char(c) if !ctrl => {
                if let Some((_, g)) = rows.iter().skip(1).find(|(x, _)| *x == c) {
                    pick(self, g.clone());
                }
            }
            _ => {}
        }
    }

    /// Pinned ones go on top in pin order, the rest sort as usual. Everything is
    /// re-sorted first, so a just-unpinned one goes back to its place
    pub(super) fn apply_pins(&mut self) {
        self.apply_group_reorders();
        sort_sessions(&mut self.all);
        self.apply_reorders();
        // Child panes go with their own session (pinned together); hanging child sessions that aren't pinned themselves go with the
        // nearest pinned one above (the parent, the one above that…)
        let hangs = hangs(&self.all);
        let rank = |pins: &Pins, s: &Session| pins.rank(s.family()).or_else(|| s.ancestors(&hangs).find_map(|p| pins.rank(p)));
        let boxed = Boxed::load();
        let space: Vec<char> = self.all.iter().map(|s| space_of(&self.all, &self.spaces, &boxed, s, true)).collect();
        for (s, c) in self.all.iter_mut().zip(space) {
            s.pinned = rank(&self.pins, s).is_some();
            s.noted = self.notes.get(&notes::key(s)).is_some();
            s.group_noted = self.notes.get(&group_key(&s.group)).is_some();
            s.space = c;
        }
        let pins = &self.pins;
        self.all.sort_by_key(|s| rank(pins, s).unwrap_or(usize::MAX));
    }

    /// ⇧↑ ⇧↓ moves not yet written to the registry are applied to the list first: the whole family (windows, all levels of child
    /// sessions hanging below) moves to right before / right after that session's family.
    /// Until written, every refresh reads the old order, so move it again; moving one already written again changes nothing
    fn apply_reorders(&mut self) {
        let hangs = hangs(&self.all);
        let row = |all: &[Session], t: &Target| -> Option<String> {
            all.iter().find(|s| s.sub.is_none() && s.profile == t.profile && s.deck_id == t.id).map(|s| s.name.clone())
        };
        for job in &self.reorders {
            let Job::Order { t, by, after } = job else { continue };
            let anchor = Target { id: by.clone(), ..t.clone() };
            let (Some(me), Some(it)) = (row(&self.all, t), row(&self.all, &anchor)) else { continue };
            let home = |s: &Session, n: &str| s.family() == n || s.ancestors(&hangs).any(|p| p == n);
            let (mine, mut rest): (Vec<Session>, Vec<Session>) = std::mem::take(&mut self.all).into_iter().partition(|s| home(s, &me));
            let at = if *after { rest.iter().rposition(|s| home(s, &it)).map(|p| p + 1) } else { rest.iter().position(|s| home(s, &it)) };
            let at = at.unwrap_or(rest.len());
            rest.splice(at..at, mine);
            self.all = rest;
        }
    }

    /// ⇧↑ ⇧↓ on a group not yet written to the registry: changes group order (the first field of slot), followed when sorting.
    /// A group moves with its subgroups to right before / right after the `by` family; "Other tmux sessions" always sinks to the bottom.
    /// Moving again changes nothing
    fn apply_group_reorders(&mut self) {
        let within = |g: &str, top: &str| g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'));
        for job in &self.reorders {
            let Job::GroupOrder { group, by, after } = job else { continue };
            let mut gs: Vec<(usize, &str)> =
                self.all.iter().filter(|s| s.slot.0 != usize::MAX).map(|s| (s.slot.0, s.group.as_str())).collect();
            gs.sort();
            gs.dedup();
            let (mine, mut rest): (Vec<&str>, Vec<&str>) = gs.into_iter().map(|g| g.1).partition(|g| within(g, group));
            let at = if *after { rest.iter().rposition(|g| within(g, by)).map(|p| p + 1) } else { rest.iter().position(|g| within(g, by)) };
            let Some(at) = at.filter(|_| !mine.is_empty()) else { continue };
            rest.splice(at..at, mine);
            let rank: HashMap<String, usize> = rest.into_iter().enumerate().map(|(i, g)| (g.to_string(), i)).collect();
            for s in &mut self.all {
                if let Some(&r) = rank.get(&s.group) {
                    s.slot.0 = r;
                }
            }
        }
    }

    /// ⇧↑ ⇧↓: swap with the previous / next session in the same group (child sessions swap under the same parent), written to the
    /// registry's in-group order. Only what's visible in the list counts: ones in between (archived, other workspaces', filtered
    /// out) stay on the other side
    pub(super) fn reorder(&mut self, d: isize) {
        if let Some(g) = self.group_row().map(String::from) {
            return self.reorder_group(&g, d);
        }
        let Some(p) = self.list.selected().filter(|&p| p < self.view.len()) else { return };
        let s = &self.all[self.view[p]];
        let m = match () {
            _ if s.sub.is_some() => Some(tr!(
                "{} 是窗口,只能按 ⏎ / d / D",
                "{} is a window: only ⏎ / d / D",
                s.sub.as_ref().map(|x| x.loc()).unwrap_or_default()
            )),
            _ if s.deck_id.is_empty() => {
                Some(t!("普通 tmux 会话 · A 接管后才能移动", "an ordinary tmux session · A adopts it, then it can move").into())
            }
            _ if self.pins.rank(&s.name).is_some() => Some(t!("置顶会话不按分组排序", "pinned ones aren't ordered by group").into()),
            _ => None,
        };
        if let Some(m) = m {
            return self.flash(&m);
        }
        // Swappable: same profile, same group, same level (top level with top
        // level, child sessions under the same parent), not pinned itself
        let peer = |o: &Session| {
            o.sub.is_none()
                && !o.deck_id.is_empty()
                && o.profile == s.profile
                && o.group == s.group
                && o.under() == s.under()
                && self.pins.rank(&o.name).is_none()
        };
        // Stop searching once out of this group (child sessions: out of the parent's family); child sessions at any level hanging
        // under a sibling are still in that family, skip over them and keep looking
        let hangs = hangs(&self.all);
        let inside = |o: &Session| match s.under() {
            Some(up) => o.family() == up || o.ancestors(&hangs).any(|p| p == up),
            None => o.group == s.group,
        };
        let mut q = p as isize + d;
        let mut hit = None;
        while let Some(&i) = usize::try_from(q).ok().and_then(|q| self.view.get(q)) {
            let o = &self.all[i];
            if !inside(o) {
                break;
            }
            if peer(o) {
                hit = Some(o.deck_id.clone());
                break;
            }
            q += d;
        }
        let Some(by) = hit else { return };
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        self.queue_reorder(Job::Order { t, by, after: d > 0 });
    }

    /// ⇧↑ ⇧↓ on a group: swap with the previous / next visible sibling group (each with its subgroups), written to the registry's
    /// group order. A subgroup can't leave its parent group; invisible ones in between (other workspaces', filtered out) stay put
    fn reorder_group(&mut self, g: &str, d: isize) {
        if g == "-" {
            return self.flash(t!("普通 tmux 会话区域不能移动", "the ordinary tmux sessions section can't be moved"));
        }
        let secs: Vec<&str> =
            self.section_starts().into_iter().map(|p| &self.all[self.view[p]]).filter(|s| !s.pinned).map(|s| s.group.as_str()).collect();
        let Some(cur) = secs.iter().position(|h| *h == g) else { return };
        let depth = g.split('/').count();
        let parent = |p: &str| p.rsplit_once('/').map(|x| x.0.to_string());
        // Which group section h is at g's level (g itself = h is its subgroup); outside g's parent group = None
        let peer = |h: &str| -> Option<String> {
            let parts: Vec<&str> = h.split('/').collect();
            let at = parts.get(..depth)?.join("/");
            (h != "-" && parent(&at) == parent(g)).then_some(at)
        };
        let mut q = cur as isize + d;
        let mut hit = None;
        while let Some(h) = usize::try_from(q).ok().and_then(|q| secs.get(q)) {
            match peer(h) {
                None => break,
                Some(at) if at == g => {}
                Some(at) => {
                    hit = Some(at);
                    break;
                }
            }
            q += d;
        }
        let Some(by) = hit else { return };
        self.queue_reorder(Job::GroupOrder { group: g.to_string(), by, after: d > 0 });
    }

    /// Record a ⇧↑ ⇧↓; the list moves at once. Consecutive moves of the same one (session / group) keep only the last (only it
    /// moves; the order among the others stays); the one being written can't be changed
    fn queue_reorder(&mut self, job: Job) {
        let same = |a: &Job, b: &Job| match (a, b) {
            (Job::Order { t: x, .. }, Job::Order { t: y, .. }) => x.profile == y.profile && x.id == y.id,
            (Job::GroupOrder { group: x, .. }, Job::GroupOrder { group: y, .. }) => x == y,
            _ => false,
        };
        let busy_front = self.reorder_writing && self.reorders.len() == 1;
        match self.reorders.back_mut() {
            Some(last) if same(last, &job) && !busy_front => *last = job,
            _ => self.reorders.push_back(job),
        }
        self.apply_pins();
        self.rebuild();
    }

    /// p: on a child pane = pin the session it's in; on a child session pinned along with a session above = unpin that session
    /// (the parent, or the pinned level further up)
    pub(super) fn toggle_pin(&mut self) {
        let Some(s) = self.current() else { return };
        let hangs = hangs(&self.all);
        let lead = s.ancestors(&hangs).find(|p| self.pins.rank(p).is_some()).filter(|_| self.pins.rank(s.family()).is_none());
        let (name, title) = match (&s.sub, lead.and_then(|p| self.all.iter().find(|x| x.sub.is_none() && x.name == p))) {
            (_, Some(p)) => (p.name.clone(), p.title.clone()),
            (Some(x), _) => (x.parent.clone(), x.parent_title.clone()),
            (None, _) => (s.name.clone(), s.title.clone()),
        };
        let key = self.all.iter().find(|x| x.sub.is_none() && x.name == name).map_or_else(|| format!("tmux:{name}"), notes::key);
        let rank = self.pins.rank(&name);
        let m = match self.pins.toggle(&name) {
            Ok(on) => {
                self.remember(Undo::Pin { key, name, title: title.clone(), rank });
                if on {
                    tr!("已置顶「{title}」", "pinned “{title}”")
                } else {
                    tr!("已取消置顶「{title}」", "unpinned “{title}”")
                }
            }
            Err(e) => tr!("置顶保存失败:{e}", "pin not saved: {e}"),
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild(); // the highlight follows the session up
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::screen::top_bar;
    use crate::ui::tests::{
        fixture, keys, life_and_work, order, pin, press, reader_rows, said, select, sessions, shift, shows, target, titles,
    };

    /// What's read back after the write: in-group order as given
    fn reordered(names: &[&str]) -> Vec<Session> {
        sessions()
            .into_iter()
            .map(|mut s| {
                if let Some(p) = names.iter().position(|n| *n == s.title) {
                    s.slot.1 = p as i64;
                }
                s
            })
            .collect()
    }

    #[test]
    fn shift_arrows_swap_within_the_group_right_away_and_write_one_at_a_time() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        select(&mut app, "闲");
        shift(&mut app, KeyCode::Up);
        assert_eq!(order(&app), ["问", "未读", "闲", "跑", "错"]);
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Order { t: target("闲"), by: "id-跑".into(), after: false });
        // Already first in the group: doesn't cross into the group above
        shift(&mut app, KeyCode::Up);
        assert!(app.msg.is_none());
        // Moving again while a write is in progress: the list still moves; of two moves in a row only the last is kept
        shift(&mut app, KeyCode::Down);
        shift(&mut app, KeyCode::Down);
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.take_job().is_none());
        shift(&mut app, KeyCode::Down);
        assert!(app.msg.is_none());
        // First write done, the read-back has only the first move: the later move stays applied and is written next
        app.msg = None;
        app.job_done(&job, Outcome::Done(String::new()), reordered(&["闲", "跑", "错"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.msg.is_none());
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Order { t: target("闲"), by: "id-错".into(), after: true });
        // A periodic refresh from before the write finished still reads the old order: doesn't jump back
        app.set_sessions(reordered(&["闲", "跑", "错"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        app.job_done(&job, Outcome::Done(String::new()), reordered(&["跑", "错", "闲"]));
        assert_eq!(order(&app), ["问", "未读", "跑", "错", "闲"]);
        assert!(app.take_job().is_none());
    }

    #[test]
    fn pin_moves_to_top_keeps_highlight_and_survives_refresh() {
        let mut app = fixture();
        assert_eq!(order(&app), ["问", "跑", "闲"]);
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["闲", "问", "跑"]); // pinned ones above "needs you"
        assert_eq!(app.current().unwrap().title, "闲");
        app.set_sessions(sessions()); // 2-second refresh, pinned is false all over the new data
        assert_eq!(order(&app), ["闲", "问", "跑"]);
    }

    #[test]
    fn later_pins_go_below_and_unpin_returns_to_place() {
        let mut app = fixture();
        pin(&mut app, "跑");
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["跑", "闲", "问"]);
        pin(&mut app, "跑");
        assert_eq!(order(&app), ["闲", "问", "跑"]);
    }

    #[test]
    fn header_counts_follow_the_visible_list() {
        let mut app = fixture();
        let header = |app: &App| top_bar(app, 240).to_string();
        assert!(header(&app).contains("◆ 1 等待回应"));
        assert!(header(&app).contains("● 1 运行中"));
        assert!(!header(&app).contains('◐') && !header(&app).contains('✕'));

        app.set_group(Some("work".into()));
        assert!(!header(&app).contains('◆'));
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        assert_eq!(said(&app), "暂无待处理会话");

        app.set_group(None);
        press(&mut app, KeyCode::Char('a'));
        assert!(header(&app).contains("◐ 1 未读") && header(&app).contains("✕ 1 出错"));
        keys(&mut app, "/未读⏎");
        assert!(header(&app).contains("◐ 1 未读"));
        assert!(!header(&app).contains('◆') && !header(&app).contains('●'));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('!'));
        assert!(header(&app).contains("● 1 运行中"));
        assert!(!header(&app).contains('◆') && !header(&app).contains('◐'));

        press(&mut app, KeyCode::Char('!'));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('o'));
        assert!(!header(&app).contains('◆') && !header(&app).contains('◐'));
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Char('o'));
        assert!(header(&app).contains("◆ 1 等待回应") && header(&app).contains("◐ 1 未读"));
    }

    #[test]
    fn braces_cycle_groups_in_section_order_and_back() {
        let mut app = fixture();
        press(&mut app, KeyCode::Char('}')); // fun first like the sections, though work has more sessions
        assert_eq!(titles(&app), ["问"]);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["跑", "闲"]);
        press(&mut app, KeyCode::Char('}'));
        assert_eq!(titles(&app), ["跑", "问", "闲"]); // back to all groups
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(titles(&app), ["跑", "闲"]); // wraps back to the last group, work
    }

    #[test]
    fn tab_lists_the_groups_and_a_letter_or_enter_switches() {
        let mut app = App::new(String::new(), life_and_work());
        keys(&mut app, "⇥");
        assert_eq!(app.group_rows(), [(' ', None), ('f', Some("fun".into())), ('w', Some("work".into()))]);
        assert_eq!(app.group_menu, Some(0)); // never switched: on the current one (all)
        let rows = reader_rows(&mut app, 80, 24);
        shows(&rows, &["分组", "␣  全部", "f  fun", "w  work", "⏎ 切换"]);
        keys(&mut app, "w");
        assert_eq!((app.group.as_deref(), app.group_menu, titles(&app)), (Some("work"), None, vec!["工作".to_string()]));
        keys(&mut app, "⇥⏎"); // on the last one looked at (all): Tab ⏎ = switch back
        assert_eq!(app.group, None);
        keys(&mut app, "⇥⏎");
        assert_eq!(app.group.as_deref(), Some("work"));
        keys(&mut app, "⇥ "); // Space = all
        assert_eq!(app.group, None);
        keys(&mut app, "⇥↑⏎"); // on work, one up is fun
        assert_eq!(app.group.as_deref(), Some("fun"));
        keys(&mut app, "⇥z"); // a letter with no group: no change (highlight on the last one looked at: all)
        assert_eq!((app.group_menu, app.group.as_deref()), (Some(0), Some("fun")));
        keys(&mut app, "⇥"); // Tab again closes
        keys(&mut app, "⇥q⇥⎋"); // q / Esc close too, don't quit
        assert!(app.group_menu.is_none() && !app.quit && app.group.as_deref() == Some("fun"));
        keys(&mut app, "}"); // { } still switch one step at a time
        assert_eq!(app.group.as_deref(), Some("work"));
    }

    #[test]
    fn shift_tab_goes_back_to_the_group_seen_before() {
        let mut app = fixture();
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.msg.as_ref().unwrap().0, "还没有切换过分组");
        press(&mut app, KeyCode::Char('{'));
        assert_eq!(app.group.as_deref(), Some("work"));
        press(&mut app, KeyCode::BackTab); // Back to the previous one (all), press again to come back
        assert_eq!(app.group, None);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.group.as_deref(), Some("work"));
    }
}
