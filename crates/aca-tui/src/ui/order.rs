//! Arranging the list: switching groups, the only-these menu, pins, and moving rows and groups, written in
//! the background one at a time.

use super::*;

impl App {
    /// Switch to showing only this group (None = all groups). The highlight goes to the row it was on when that place was
    /// left, the first row if it never was there (or that session is gone). With no rail on screen to show where the list is
    /// now, the bottom bar says it
    pub(super) fn set_group(&mut self, to: Option<String>) {
        if to != self.group {
            // All groups is left (or come back to): where `a` would have gone back to is forgotten; `hop` sets it after this
            self.hop_back = None;
            if let Some(n) = self.selected.clone() {
                self.left_at.insert(self.group.clone(), n);
            }
            self.group = to;
            // A group with tickets still to finish opens on its tasks, the cursor on the first of them. One with none
            // opens on the page it was left on — unless that was its tasks: nothing there is waiting
            if let Some(g) = self.group.clone() {
                match self.tickets.open(&g) > 0 {
                    true => self.show_page(&g, Page::Tasks),
                    false if self.pages.get(&g) == Some(&Page::Tasks) => self.show_page(&g, Page::Sessions),
                    false => {}
                }
                self.task_at = None;
            }
            self.selected = self.left_at.get(&self.group).cloned();
            self.list.select(Some(0));
            *self.disp.offset_mut() = 0;
        }
        if !self.rail_on_screen() {
            let m = match &self.group {
                Some(g) => tr!("分组:{}", "group: {}", self.shown(g)),
                None => t!("全部分组", "all groups").into(),
            };
            self.flash(&m);
        }
        self.rebuild();
    }

    /// `F`: the small only-one-state list. The highlight is on the state picked now (on all, the top row, if none)
    pub(super) fn open_only_menu(&mut self) {
        self.digits.clear();
        self.only_menu = Some(self.status.and_then(|p| PICKS.iter().position(|&x| x == p)).unwrap_or(0));
    }

    /// In the only-state list: ↑↓ j k pick, ⏎ shows only it (on the one already picked = cancel), the key at the start of a row picks
    /// directly (the same one again = cancel); Esc / F / q close
    pub(super) fn on_only_menu_key(&mut self, k: KeyEvent) {
        let Some(sel) = self.only_menu else { return };
        let n = PICKS.len();
        let choose = |me: &mut Self, p: Pick| {
            me.only_menu = None;
            me.toggle_status(p);
        };
        match k.code {
            KeyCode::Char('?') if !k.modifiers.contains(KeyModifiers::CONTROL) => self.open_sheet(&super::screen::only_keys()),
            KeyCode::Down | KeyCode::Char('j') => self.only_menu = Some((sel + 1) % n),
            KeyCode::Up | KeyCode::Char('k') => self.only_menu = Some((sel + n - 1) % n),
            KeyCode::Enter => choose(self, PICKS[sel]),
            KeyCode::Esc | KeyCode::Char('F' | 'q') => self.only_menu = None,
            KeyCode::Char(c) => {
                if let Some(&p) = PICKS.iter().find(|p| p.label().0 == c) {
                    choose(self, p);
                }
            }
            _ => {}
        }
    }

    /// How many in this workspace are in this state (counted like the scopes' numbers in the top bar)
    pub(super) fn pick_count(&self, p: Pick) -> usize {
        self.all.iter().filter(|s| counted(s) && s.space == self.space && p.hit(s)).count()
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
            s.group_noted = self.book.top(&s.group).is_some();
            s.space = c;
        }
        // The scratch group's sessions (and those of the groups under it) come before the pinned ones, as its row does on the
        // rail; an empty one's heading sinks to the bottom with the other empty groups'
        let dir = (self.scratch_dir)();
        self.scratch = if dir.is_empty() { None } else { groups::scratch_in(&(self.group_hints)().groups, &dir) };
        // Its router is the desk; it and the groups under it are named by its mark
        for s in &mut self.all {
            s.desk = s.router && self.scratch.as_deref() == Some(s.group.as_str());
            s.scratch = scratch_len(&s.group, self.scratch.as_deref());
            // The ticket it's doing, else the one it finished last
            s.ticket = match (self.tickets.doing(&s.deck_id), self.tickets.finished(&s.deck_id)) {
                (Some(t), _) => format!("#{}", t.n),
                (None, Some(t)) => format!("✓ #{}", t.n),
                (None, None) => String::new(),
            };
        }
        let (pins, scratch) = (&self.pins, self.scratch.as_deref());
        // The desk (with its windows and what it started itself) leads the scratch group's rows: the group's other sessions
        // are drawn under it (see `tree_marks`)
        let desk = self.all.iter().find(|s| s.desk && s.sub.is_none()).map(|s| s.name.clone());
        let of_desk = |s: &Session| desk.as_deref().is_some_and(|d| s.family() == d || s.ancestors(&hangs).any(|p| p == d));
        self.all.sort_by_key(|s| match rank(pins, s) {
            Some(r) => (1, r),
            None if !s.blank && scratch.is_some_and(|g| groups::within(&s.group, g)) => (0, usize::from(!of_desk(s))),
            None => (2, 0),
        });
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
            // It leads its group's rows wherever the registry has it (see `apply_pins`)
            _ if s.desk => Some(t!("总台固定在最前", "the desk always comes first").into()),
            _ => None,
        };
        if let Some(m) = m {
            return self.flash(&m);
        }
        // Swappable: same profile, same group, same level (top level with top
        // level, child sessions under the same parent), not pinned itself. Not the desk: it stays first
        let peer = |o: &Session| {
            o.sub.is_none()
                && !o.deck_id.is_empty()
                && o.profile == s.profile
                && o.group == s.group
                && o.under() == s.under()
                && !o.desk
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
        // The groups in order: the list's sections, or from the rail (the list shows one group then) its rows
        let rail: Vec<String> = match self.on_rail() {
            true => self.rail_rows().into_iter().filter_map(|r| if let RailKind::Group(g) = r.kind { Some(g) } else { None }).collect(),
            false => vec![],
        };
        let secs: Vec<&str> = match self.on_rail() {
            true => rail.iter().map(String::as_str).collect(),
            false => {
                self.section_starts().into_iter().map(|p| &self.all[self.view[p]]).filter(|s| !s.pinned).map(|s| s.group.as_str()).collect()
            }
        };
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
        caps, fixture, keys, life_and_work, mk, only, order, pin, press, reader_rows, said, select, sessions, shift, target, titles,
        to_scope,
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
        to_scope(&mut app, Scope::Week);
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
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "错"]);
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["闲", "问", "未读", "跑", "错"]); // pinned ones above "needs you"
        assert_eq!(app.current().unwrap().title, "闲");
        app.set_sessions(sessions()); // 2-second refresh, pinned is false all over the new data
        assert_eq!(order(&app), ["闲", "问", "未读", "跑", "错"]);
    }

    #[test]
    fn later_pins_go_below_and_unpin_returns_to_place() {
        let mut app = fixture();
        pin(&mut app, "跑");
        pin(&mut app, "闲");
        assert_eq!(order(&app), ["跑", "闲", "问", "未读", "错"]);
        pin(&mut app, "跑");
        assert_eq!(order(&app), ["闲", "问", "未读", "跑", "错"]);
    }

    #[test]
    fn header_counts_follow_the_visible_list() {
        let mut app = fixture();
        let header = |app: &App| top_bar(app, 240).to_string();
        assert!(header(&app).contains("◆ 1 等待回应"));
        assert!(header(&app).contains("● 1 运行中"));
        assert!(header(&app).contains("◐ 1 未读") && header(&app).contains("✕ 1 出错"));

        app.set_group(Some("work".into()));
        assert!(!header(&app).contains('◆') && !header(&app).contains('◐'));
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        assert_eq!(said(&app), "暂无待处理会话");

        app.set_group(None);
        to_scope(&mut app, Scope::Week);
        assert!(header(&app).contains("◐ 1 未读") && header(&app).contains("✕ 1 出错"));
        keys(&mut app, "/未读⏎");
        assert!(header(&app).contains("◐ 1 未读"));
        assert!(!header(&app).contains('◆') && !header(&app).contains('●'));
        press(&mut app, KeyCode::Esc);
        only(&mut app, '1');
        assert!(header(&app).contains("● 1 运行中"));
        assert!(!header(&app).contains('◆') && !header(&app).contains('◐'));

        only(&mut app, '1');
        assert!(header(&app).contains("◆ 1 等待回应") && header(&app).contains("◐ 1 未读"));
    }

    #[test]
    fn a_rail_row_counts_what_going_there_would_list_in_the_current_scope() {
        let mut v = sessions();
        v.extend([mk("停", "work", State::Stopped, 10), mk("老停", "lab", State::Stopped, 9 * 86_400)]);
        let mut app = App::new(String::new(), v);
        assert_eq!(app.scope, Scope::Open);
        // The row of a group: the number on its header in the list; all groups: the top bar's; a group with none here has none.
        // Written as the name, the number, then how many are ◆ ● ◐ ✕
        let row = |app: &App, name: &str| -> String {
            let rows = app.rail_rows();
            let r = rows.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("{name}"));
            let marks: Vec<String> =
                ["◆", "●", "◐", "✕"].iter().zip(r.marks).filter(|m| m.1 > 0).map(|(ic, n)| format!("{ic}{n}")).collect();
            [vec![name.to_string(), r.n.to_string()], marks].concat().join(" ")
        };
        let list = reader_rows(&mut app, 80, 24);
        assert!(list.iter().any(|r| r.contains("▾ work  3")) && list[0].contains("未停止 5"), "{list:#?}");
        assert_eq!(row(&app, "work"), "work 3 ●1 ✕1");
        assert_eq!(row(&app, "全部"), "全部 5 ◆1 ●1 ◐1 ✕1");
        assert_eq!(row(&app, "lab"), "lab 0");
        // Going there lists that many
        app.set_group(Some("work".into()));
        assert_eq!(titles(&app).len(), 3);
        app.set_group(None);
        // 7 days: 「停」 too, not 「老停」 from nine days ago
        to_scope(&mut app, Scope::Week);
        assert_eq!(
            (row(&app, "work"), row(&app, "全部"), row(&app, "lab")),
            ("work 4 ●1 ✕1".into(), "全部 6 ◆1 ●1 ◐1 ✕1".into(), "lab 0".into())
        );
        // A state picked: only that state
        only(&mut app, '1');
        assert_eq!((row(&app, "work"), row(&app, "全部"), row(&app, "fun")), ("work 1 ●1".into(), "全部 1 ●1".into(), "fun 0".into()));
    }

    #[test]
    fn on_a_narrow_screen_h_lays_the_rail_over_the_left_and_tab_opens_nothing() {
        let mut app = App::new(String::new(), life_and_work());
        let shown = |app: &mut App| reader_rows(app, 80, 24);
        // No rail of its own at this width
        assert!(!shown(&mut app).iter().any(|r| r.contains("回收站")));
        // h at the list's outermost level: the rail, over the left, the cursor on the row of where the list is
        keys(&mut app, "h");
        let rows = shown(&mut app);
        assert!(app.on_rail() && rows[0].starts_with("▌   全部") && rows.iter().any(|r| r.contains("回收站")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains(&caps("⏎ 进入"))), "{rows:#?}");
        // j: the next place, the list behind follows at once; ⏎ goes in, and the rail is put away
        keys(&mut app, "j");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), true));
        keys(&mut app, "j⏎");
        assert_eq!((app.group.as_deref(), app.on_rail(), titles(&app)), (Some("work"), false, vec!["工作".to_string()]));
        assert!(!shown(&mut app).iter().any(|r| r.contains("回收站")));
        // Esc on the rail: put away, and the list back where it was when the rail was called out; h does the same
        keys(&mut app, "hk");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), true));
        keys(&mut app, "⎋");
        assert_eq!((app.group.as_deref(), app.on_rail(), app.quit), (Some("work"), false, false));
        keys(&mut app, "hkkh");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("work"), false));
        // l and → go in as ⏎ does
        keys(&mut app, "hkk");
        press(&mut app, KeyCode::Right);
        assert_eq!((app.group.as_deref(), app.on_rail()), (None, false));
        // The rail takes the whole width where a list wouldn't fit beside it (a phone)
        keys(&mut app, "h");
        let phone = reader_rows(&mut app, 46, 24);
        assert!(phone[0].starts_with("▌   全部") && phone[0].ends_with('│') && phone[0].width() == 46, "{phone:#?}");
        keys(&mut app, "⎋");
        // Tab opens nothing, here or in a group; { } ⇧Tab do nothing either
        keys(&mut app, "⇥");
        assert!(!app.on_rail() && !app.modal_open() && app.group.is_none());
        keys(&mut app, "W");
        let own = app.group.clone();
        app.msg = None;
        keys(&mut app, "⇥}{⇤");
        assert!(own.is_some() && app.group == own && !app.on_rail() && !app.modal_open() && app.msg.is_none());
    }

    #[test]
    fn a_rail_laid_over_the_list_shows_the_groups_letters_alone_keeps_the_wheel_and_goes_back_only_to_a_group_still_there() {
        let mut app = App::new(String::new(), life_and_work());
        reader_rows(&mut app, 80, 24);
        app.set_group(Some("work".into()));
        keys(&mut app, "h");
        reader_rows(&mut app, 80, 24);
        // Space: the sessions' letters would be written under the rail, so only the groups' own show, on it; a lowercase
        // letter has nothing to open there and only puts them away
        keys(&mut app, " ");
        assert!(app.jump.is_some() && app.on_rail());
        let rows = reader_rows(&mut app, 80, 24);
        assert!(rows.iter().any(|r| r.contains("F fun")) && rows.iter().any(|r| r.contains("W work")), "{rows:#?}");
        crate::ui::tests::shows(&rows, &[&caps("⇧字母 进分组"), &caps("空格 取消")]);
        assert!(!rows.iter().any(|r| r.contains(&caps("字母 打开"))), "{rows:#?}");
        keys(&mut app, "s");
        assert!(app.jump.is_none() && app.on_rail() && app.attach.is_none());
        // The wheel over the rail moves along it, and doesn't put it away
        let wheel = |app: &mut App, kind: MouseEventKind| {
            let (column, row) = (app.rail_area.x + 4, app.rail_area.y + 1);
            app.on_mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
        };
        wheel(&mut app, MouseEventKind::ScrollUp);
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), true));
        wheel(&mut app, MouseEventKind::ScrollDown);
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("work"), true));
        // The group it was called out from was renamed meanwhile: Esc goes back to it under its new name
        keys(&mut app, "k");
        app.group_renamed("work", "job");
        let mut v = life_and_work();
        v.iter_mut().filter(|s| s.group == "work").for_each(|s| s.group = "job".into());
        app.set_sessions(v);
        keys(&mut app, "⎋");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("job"), false));
        // Gone altogether: Esc puts the rail away and the list stays on what it shows
        keys(&mut app, "hk");
        let mut v = life_and_work();
        v.retain(|s| s.group != "work");
        app.set_sessions(v);
        keys(&mut app, "⎋");
        assert_eq!((app.group.as_deref(), app.on_rail()), (Some("fun"), false));
        // Another workspace: nothing of this one to go back to
        keys(&mut app, "hjSh");
        keys(&mut app, "⎋");
        assert!(!app.on_rail() && app.group.is_none());
    }
}
