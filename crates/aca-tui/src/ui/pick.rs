//! Acting on several sessions at once: `v` picks a run of rows and the picked rows act together; `i` jumps through the
//! waiting ones.

use super::*;

impl App {
    /// Go into the next one in the visible list that needs you, in the order `aca next` goes (see `bar::waiting`): ones wanting
    /// your answer (`◆`) before finished-and-unread ones (`◐`), within a kind the most recently active first. Ones looked at with
    /// no new output since are skipped — come back and press `i` again for the next one, going through them one by one
    pub(super) fn jump_waiting(&mut self) {
        let pick = (0..self.view.len())
            .filter(|&p| {
                let s = &self.all[self.view[p]];
                waiting(s) && self.reads.read_at(&notes::key(s)).is_none_or(|t| s.active_at > t + SEEN_SLACK)
            })
            .min_by_key(|&p| {
                let s = &self.all[self.view[p]];
                (s.state != State::Ask, s.idle_for)
            });
        match pick {
            Some(p) => {
                self.rail = None;
                self.select(Some(p));
                self.enter();
            }
            None => self.flash(t!("暂无待处理会话", "nothing needs you")),
        }
    }

    /// Keys in jump mode, like agent-deck: lowercase letters spell a hint, a match goes into that session. A capital letter is
    /// a group's own (see `group_keys`): into that group; one no group has only puts the letters away. Space and Esc put the
    /// letters away; so does a (no hint has it), which then goes back to where it was pressed from, if it was. Other keys
    /// leave jump mode and are handled as usual (returns false)
    pub(super) fn on_jump_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc | KeyCode::Char(' ') => {
                self.jump = None;
                true
            }
            KeyCode::Char('a') if !ctrl => {
                self.jump = None;
                if let Some(back) = self.hop_back.take() {
                    self.hop_to(back);
                }
                true
            }
            KeyCode::Char(c) if c.is_ascii_uppercase() && !ctrl => {
                self.jump = None;
                if let Some((_, g)) = self.group_keys().into_iter().find(|(key, _)| *key == c) {
                    self.jump_into(g);
                }
                true
            }
            // A rail laid over the list covers the sessions' letters: only the groups' are there to press
            KeyCode::Char(c) if c.is_ascii_lowercase() && !ctrl && self.on_rail() && !rail_shown(self.screen) => {
                self.jump = None;
                true
            }
            KeyCode::Char(c) if c.is_ascii_lowercase() && !ctrl => {
                let targets = self.jump_stops();
                let hints = jump_hints(targets.len());
                let typed = self.jump.get_or_insert_default();
                typed.push(c);
                match match_hint(&hints, typed) {
                    HintMatch::Hit(i) => {
                        self.jump = None;
                        self.rail = None;
                        if let Jump::Row(p) = targets[i] {
                            self.select(Some(p));
                            self.enter();
                        }
                    }
                    HintMatch::Prefix => {}
                    HintMatch::Miss => self.jump = None,
                }
                true
            }
            _ => {
                self.jump = None;
                false
            }
        }
    }

    /// A key while the groups' letters show for `` ` `` (see `Letters`): a group's letter, either case (no session has a letter
    /// to share them with), puts the cursor on that group's row. False = not taken here: the letters go, and the key is
    /// pressed as usual on the row the cursor is on (j k move along the rail, ⏎ goes in, o into its router)
    pub(super) fn on_letters_key(&mut self, k: KeyEvent) -> bool {
        let Some(back) = self.letters.take() else { return false };
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match k.code {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                let c = c.to_ascii_uppercase();
                let Some((_, g)) = self.group_keys().into_iter().find(|(key, _)| *key == c) else { return false };
                self.rail_go(&RailKind::Group(g));
            }
            // ` again or Esc: back where it was
            KeyCode::Char('`') | KeyCode::Esc => {
                self.rail = back.rail;
                self.rail_from = back.from;
                self.hop_back = back.hop;
            }
            _ => return false,
        }
        true
    }

    /// Multi-selecting: some are ticked, or a range is in progress
    pub(super) fn picking(&self) -> bool {
        !self.picked.is_empty() || self.anchor.is_some()
    }

    /// Whether row p of the visible list can be multi-selected: only registered sessions (not windows, subagents, plain tmux
    /// sessions or empty groups)
    pub(super) fn pickable(&self, p: usize) -> bool {
        let s = &self.all[self.view[p]];
        s.sub.is_none() && !s.blank && !s.deck_id.is_empty()
    }

    /// The range being selected (positions in the visible list): from the start row to the highlighted row. Start not listed = None.
    /// With the highlight on a group heading `list` selects the session below it: the range stops at the heading, not including it
    fn range(&self) -> Option<std::ops::RangeInclusive<usize>> {
        let a = self.anchor.as_deref()?;
        let at = self.view.iter().position(|&i| self.all[i].sub.is_none() && self.all[i].deck_id == a)?;
        let cur = self.list.selected()?;
        Some(if self.head.is_some() && at < cur { at..=cur - 1 } else { at.min(cur)..=at.max(cur) })
    }

    /// Selected sessions (registered ids): ticked plus the range in progress (an unticking range: minus), in list order;
    /// ticked ones not listed now (filtered out) come last
    pub(super) fn chosen(&self) -> Vec<String> {
        let id = |p: usize| &self.all[self.view[p]].deck_id;
        let ranged: Vec<&String> = self.range().map_or(vec![], |r| r.filter(|&p| self.pickable(p)).map(id).collect());
        let on = |id: &String| match self.unpick {
            true => self.picked.contains(id) && !ranged.contains(&id),
            false => self.picked.contains(id) || ranged.contains(&id),
        };
        let mut v: Vec<String> = (0..self.view.len()).filter(|&p| self.pickable(p)).map(id).filter(|id| on(id)).cloned().collect();
        for id in &self.picked {
            if on(id) && !v.contains(id) {
                v.push(id.clone());
            }
        }
        v
    }

    /// The selected sessions themselves, in `chosen` order
    pub(super) fn chosen_rows(&self) -> Vec<&Session> {
        self.chosen().iter().filter_map(|id| self.all.iter().find(|s| s.sub.is_none() && s.deck_id == *id)).collect()
    }

    pub(super) fn drop_picks(&mut self) {
        self.picked.clear();
        self.anchor = None;
        self.unpick = false;
    }

    /// Only one selected and it's listed: the highlight moves to it, multi-select ends, and the key acts as if pressed on it
    /// (confirm boxes and messages as usual)
    fn only_one(&mut self) -> bool {
        let ids = self.chosen();
        let [id] = &ids[..] else { return false };
        let Some(p) = self.view.iter().position(|&i| self.all[i].sub.is_none() && self.all[i].deck_id == *id) else { return false };
        self.select(Some(p));
        self.drop_picks();
        true
    }

    /// v: range select from the highlighted row (↑↓ extends it); V (unpick) unticks a range. v again while ranging: the unfixed
    /// range is dropped and starts again from this row (like yazi). On a group heading it starts from the group's first session
    /// Why the highlighted row can't be selected (None = it can)
    fn unpickable(&self) -> Option<&'static str> {
        let p = self.list.selected()?;
        let s = &self.all[self.view[p]];
        match () {
            _ if self.pickable(p) => None,
            _ if s.blank => Some(t!("空分组里没有会话", "the group has no sessions")),
            _ if s.sub.is_some() => Some(t!("只能多选会话", "only sessions can be selected")),
            _ => Some(t!("普通 tmux 会话 · A 接管后才能多选", "can't select an ordinary tmux session · A adopts it")),
        }
    }

    pub(super) fn start_pick(&mut self, unpick: bool) {
        if let Some(m) = self.unpickable() {
            return self.flash(m);
        }
        let Some(p) = self.list.selected() else { return };
        self.unpick = unpick;
        self.anchor = Some(self.all[self.view[p]].deck_id.clone());
    }

    /// Keys while multi-selecting (shown in the bottom bar), like yazi: v / V start a new range from this row; Esc while ranging
    /// fixes the range, otherwise unselects all; Space ticks / unticks one and moves down (also while ranging, extending the range).
    /// Returns false = not handled here, as usual (move highlight, view, search, ⏎…)
    pub(super) fn on_pick_key(&mut self, k: KeyEvent) -> bool {
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        let ranging = self.anchor.is_some();
        match k.code {
            KeyCode::Esc if ranging => {
                self.picked = self.chosen();
                self.anchor = None;
            }
            KeyCode::Esc => self.picked.clear(),
            KeyCode::Char(c @ ('v' | 'V')) => self.start_pick(c == 'V'),
            KeyCode::Char(' ') => self.tick(),
            KeyCode::Char('d') => self.ask_many(Batch::Remove),
            KeyCode::Char('D') => self.ask_many(Batch::Stop),
            KeyCode::Char('R') => self.ask_many(Batch::Restart),
            KeyCode::Char('m') => self.ask_move_many(),
            KeyCode::Char('p') => self.pin_many(),
            KeyCode::Char('u') => self.read_many(),
            KeyCode::Char('Q') => self.pending = Some(Pending::Send),
            _ => return false,
        }
        true
    }

    /// Space: tick / untick the highlighted session and move down a row. On a group heading: tick the group's listed sessions
    /// together (all ticked = untick all)
    fn tick(&mut self) {
        let Some(p) = self.list.selected() else { return };
        let ids: Vec<String> = match &self.head {
            Some(g) => (0..self.view.len())
                .filter(|&q| self.pickable(q) && !self.all[self.view[q]].pinned && self.all[self.view[q]].group == *g)
                .map(|q| self.all[self.view[q]].deck_id.clone())
                .collect(),
            None if self.pickable(p) => vec![self.all[self.view[p]].deck_id.clone()],
            None => vec![],
        };
        if ids.iter().all(|id| self.picked.contains(id)) {
            self.picked.retain(|id| !ids.contains(id));
        } else {
            for id in ids {
                if !self.picked.contains(&id) {
                    self.picked.push(id);
                }
            }
        }
        self.move_by(1);
    }

    /// d / D / R after multi-select: ask first, then do them together (restart doesn't ask when none is running, same as R).
    /// With only one selected it's like pressing on it
    fn ask_many(&mut self, act: Batch) {
        // Other deletes still running don't matter, delete on (same as d); other work must wait
        let busy = match act {
            Batch::Remove => self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)),
            _ => self.doing(),
        };
        if let Some(b) = busy {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        if self.only_one() {
            return match act {
                Batch::Remove => self.ask_remove(),
                Batch::Stop => self.ask_stop(),
                Batch::Restart => self.ask_restart(),
            };
        }
        let ids = self.chosen();
        let rows = self.chosen_rows();
        let count = |f: &dyn Fn(&Session) -> bool| rows.iter().filter(|s| f(s)).count();
        let busy = count(&|s| matches!(s.state, State::Running | State::Ask));
        let attached = count(&|s| s.attached);
        let mut notes = vec![];
        let mut add = |n: usize, m: String| {
            if n > 0 {
                notes.push(m);
            }
        };
        let (ts, ask): (Vec<Target>, bool) = match act {
            Batch::Remove => {
                let names: Vec<&str> = rows.iter().map(|s| s.name.as_str()).collect();
                let windows = self.all.iter().filter(|x| x.sub.is_some() && x.subagent.is_none() && names.contains(&x.family())).count();
                let kids = self
                    .all
                    .iter()
                    .filter(|x| x.sub.is_none() && !ids.contains(&x.deck_id) && x.up.parent().is_some_and(|p| names.contains(&p)))
                    .count();
                let worktrees = count(&|s| s.worktree);
                add(busy, tr!("{busy} 个正在运行或等待回应，将被中断", "{busy} running or waiting for you; this interrupts them"));
                add(attached, tr!("⇄ {attached} 个在别处打开，将被断开", "⇄ {attached} open elsewhere; they will be disconnected"));
                add(worktrees, tr!("同时删除 {worktrees} 个 worktree 目录", "{worktrees} worktree directories are deleted too"));
                add(windows, tr!("同时关闭其中的 {windows} 个窗口 / 窗格", "{windows} windows / panes opened inside close too"));
                add(kids, tr!("{kids} 个子会话未选中，不会删除", "{kids} child sessions aren't selected and won't be deleted"));
                (rows.iter().map(|s| target_of(s)).collect(), true)
            }
            Batch::Stop => {
                let live: Vec<&&Session> = rows.iter().filter(|s| !s.state.gone() && s.state != State::Dead).collect();
                if live.is_empty() {
                    return self.flash(t!("均已停止", "they're all stopped already"));
                }
                let attached = live.iter().filter(|s| s.attached).count();
                let off = rows.len() - live.len();
                add(attached, tr!("⇄ {attached} 个在别处打开，将被断开", "⇄ {attached} open elsewhere; they will be disconnected"));
                add(off, tr!("另有 {off} 个已停止", "{off} more are stopped already"));
                (live.iter().map(|s| target_of(s)).collect(), true)
            }
            Batch::Restart => {
                add(busy, tr!("{busy} 个正在运行或等待回应，将被中断", "{busy} running or waiting for you; this interrupts them"));
                (rows.iter().map(|s| target_of(s)).collect(), busy > 0)
            }
        };
        if ts.is_empty() {
            return self.flash(t!("尚未选中会话", "nothing selected"));
        }
        if ask {
            self.confirm = Some(Confirm::Many { act, ts, notes });
        } else {
            self.run_many(act, ts);
        }
    }

    /// Confirmed d / D / R: run together in the background, multi-select cleared. Deleted ones are hidden from the list first
    /// (same as d); ones running before deletion are noted, to start again after ^Z undo
    pub(super) fn run_many(&mut self, act: Batch, ts: Vec<Target>) {
        self.drop_picks();
        let live = |t: &Target| self.all.iter().any(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile && !s.state.gone());
        let running: Vec<String> = ts.iter().filter(|t| live(t)).map(|t| t.id.clone()).collect();
        let job = Job::Many(ts.into_iter().map(|t| act.job(t)).collect());
        if act == Batch::Remove {
            self.vanish.push(Vanish { job: job.clone(), until: None, running });
            self.job = Some(job);
            self.drop_vanished();
            self.apply_pins();
            self.rebuild();
        } else {
            self.job = Some(job);
        }
    }

    /// `[idle] remove_after`: ones stopped too long (see `stale`) go to the trash like a v multi-select delete, as one entry,
    /// ^Z brings them back, the bottom bar says it was automatic. `archived` is archived sessions (not listed) and their last
    /// activity time. Skips this round while busy or multi-selecting
    pub(super) fn sweep(&mut self, after: u64, now: u64, archived: Vec<(Target, u64)>) {
        if self.job.is_some() || self.doing().is_some() || !self.picked.is_empty() || self.anchor.is_some() {
            return;
        }
        let ts = self.stale(after, now, archived);
        if ts.is_empty() {
            return;
        }
        let job = Job::Many(ts.into_iter().map(|t| Job::Remove { t, pane: None }).collect());
        self.swept = Some((job.clone(), after));
        self.vanish.push(Vanish { job: job.clone(), until: None, running: vec![] });
        self.job = Some(job);
        self.drop_vanished();
        self.apply_pins();
        self.rebuild();
    }

    /// Auto-deletable: registered sessions stopped (■ ☾, closed ✕) with no activity for over `after` seconds, not pinned, no note,
    /// nobody attached, with no child session hanging under them and no extra window inside still running; plus archived ones
    /// equally long idle, with no note, not pinned (archiving doesn't clear pins; restored they're back in place). Not ones in the trash
    pub(super) fn stale(&self, after: u64, now: u64, archived: Vec<(Target, u64)>) -> Vec<Target> {
        let hangs = hangs(&self.all);
        let needed: HashSet<&str> =
            self.all.iter().filter(|s| open(s)).flat_map(|s| s.ancestors(&hangs).chain(s.sub.as_ref().map(|x| x.parent.as_str()))).collect();
        let old = |at: u64| now.saturating_sub(at) >= after;
        let listed = self.all.iter().filter(|s| {
            s.sub.is_none()
                && s.subagent.is_none()
                && !s.blank
                && !s.deck_id.is_empty()
                && s.profile == crate::data::REGISTERED
                && !open(s)
                && !s.pinned
                && !s.noted
                && !s.attached
                && old(s.active_at)
                && !needed.contains(s.name.as_str())
        });
        let hidden = archived.into_iter().filter(|(t, at)| {
            old(*at) && self.notes.get(&t.id).is_none() && self.pins.rank(&t.name).is_none() && !self.trash.has(&t.profile, &t.id)
        });
        listed.map(target_of).chain(hidden.map(|(t, _)| t)).collect()
    }

    /// m after multi-select: pick a group in fzf, the selected ones move into it together (ones already in it stay)
    fn ask_move_many(&mut self) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        if self.only_one() {
            return self.ask_move();
        }
        let rows = self.chosen_rows();
        let dirs: HashSet<&str> = rows.iter().map(|s| s.dir.as_str()).collect();
        let here = if dirs.len() == 1 { rows[0].dir.clone() } else { String::new() };
        let mut all: Vec<(Target, String)> = rows.iter().map(|s| (target_of(s), s.group.clone())).collect();
        if all.is_empty() {
            return self.flash(t!("尚未选中会话", "nothing selected"));
        }
        let (t, from) = all.remove(0);
        self.pick_group = Some(self.group_pick(t, from, here, all));
    }

    /// p after multi-select: pin all if any is unpinned, unpin all if all are pinned
    fn pin_many(&mut self) {
        if self.only_one() {
            return self.toggle_pin();
        }
        let rows: Vec<(String, String, String)> =
            self.chosen_rows().iter().map(|s| (s.name.clone(), s.title.clone(), notes::key(s))).collect();
        let on = rows.iter().any(|(name, ..)| self.pins.rank(name).is_none());
        let (mut us, mut err) = (vec![], None);
        for (name, title, key) in rows {
            let rank = self.pins.rank(&name);
            if on && rank.is_some() {
                continue;
            }
            match self.pins.toggle(&name) {
                Ok(_) => us.push(Undo::Pin { key, name, title, rank }),
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let n = us.len();
        let m = match err {
            Some(e) => tr!("置顶保存失败:{e}", "pin not saved: {e}"),
            None if on => tr!("已置顶 {n} 个", "pinned {n}"),
            None => tr!("已取消置顶 {n} 个", "unpinned {n}"),
        };
        if n > 0 {
            self.remember(Undo::Many(us));
            self.drop_picks();
        }
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// u after multi-select: if any is unread (◐) mark all read, otherwise mark the idle ones unread; running ones and ones
    /// needing your answer are left alone
    fn read_many(&mut self) {
        if self.only_one() {
            return self.toggle_read();
        }
        let idx: Vec<usize> =
            self.chosen().iter().filter_map(|id| self.all.iter().position(|s| s.sub.is_none() && s.deck_id == *id)).collect();
        let read = idx.iter().any(|&i| self.all[i].state == State::Unread);
        let want = if read { State::Unread } else { State::Idle };
        let idx: Vec<usize> = idx.into_iter().filter(|&i| self.all[i].state == want).collect();
        let (mut us, mut err) = (vec![], None);
        for i in idx {
            match self.mark_one(i) {
                Some(Ok(u)) => us.push(u),
                Some(Err(e)) => {
                    err = Some(e);
                    break;
                }
                None => {}
            }
        }
        let n = us.len();
        let what = if read { t!("已读", "read") } else { t!("未读", "unread") };
        let m = match err {
            Some(e) => tr!("保存失败:{e}", "not saved: {e}"),
            None if n == 0 => t!("只能标记空闲或未读的会话", "only idle / unread ones can be marked").to_string(),
            None => tr!("已将 {n} 个标为{what}", "marked {n} {what}"),
        };
        if n > 0 {
            self.remember(Undo::Many(us));
            self.drop_picks();
        }
        self.flash(&m);
        self.rebuild(); // with @ picked, ones marked read drop off the list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{
        at, caps, ctrl, fixture, keys, many, mk, only, order, pin, press, reader_rows, said, select, sessions, shows, space, ticked,
        to_scope,
    };

    #[test]
    fn i_goes_through_waiting_sessions_the_asking_ones_first_and_newest_first_within_a_kind() {
        let v = vec![
            mk("旧问", "fun", State::Ask, 50),
            mk("新读", "fun", State::Unread, 10),
            mk("跑", "work", State::Running, 1),
            mk("错", "work", State::Error, 5),
            mk("中读", "work", State::Unread, 30),
            mk("新问", "work", State::Ask, 40),
        ];
        let mut app = App::new(String::new(), v.clone());
        let next = |app: &mut App| {
            press(app, KeyCode::Char('i'));
            let name = app.attach.take();
            if let Some(n) = &name {
                app.mark_seen(n); // back after looking
            }
            name
        };
        // The asking ones first, though both unread ones were active more recently (the order `aca next` goes in)
        assert_eq!(next(&mut app).as_deref(), Some("新问"));
        assert_eq!(app.current().unwrap().title, "新问"); // back, the highlight is on the one just looked at
        assert_eq!(next(&mut app).as_deref(), Some("旧问"));
        assert_eq!(next(&mut app).as_deref(), Some("新读"));
        assert_eq!(next(&mut app).as_deref(), Some("中读"));
        assert_eq!(next(&mut app), None); // running and error ones don't count; all seen, so it doesn't enter
        let mut v = v;
        v[1].active_at = epoch() + 10; // 新读 has new output again
        app.set_sessions(v);
        assert_eq!(next(&mut app).as_deref(), Some("新读"));
    }

    #[test]
    fn i_only_looks_at_what_is_shown() {
        let mut app = fixture();
        app.set_group(Some("work".into())); // nothing in the work group needs you (问 and 未读 are in fun)
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        app.set_group(None);
        only(&mut app, '1'); // only running ones listed
        press(&mut app, KeyCode::Char('i'));
        assert!(app.attach.is_none());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.attach.as_deref(), Some("问")); // the one asking, before the unread one with later activity
    }

    #[test]
    fn v_stretches_a_range_and_d_deletes_them_together_as_one_trash_entry() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('v'));
        keys(&mut app, "jj");
        assert_eq!(ticked(&mut app), ["跑", "闲", "错"]);
        let foot = reader_rows(&mut app, 80, 20).pop().unwrap();
        for want in ["连选".to_string(), "已选 3".into(), caps("d 删除"), caps("m 移到分组"), caps("Esc 结束")] {
            assert!(foot.contains(&want), "缺「{want}」:{foot}");
        }
        // Pulling back, the range shrinks with it
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(ticked(&mut app), ["跑", "闲"]);
        press(&mut app, KeyCode::Char('j'));
        // d: asks first, listing which ones and how many running ones get interrupted; any other key doesn't remove, the
        // selection stays
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 80, 24), &["删除 3 个会话？", "跑", "闲", "错", "1 个正在运行或等待回应，将被中断", "y 删除"]);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!((said(&app), app.chosen().len()), ("未删除", 3));
        keys(&mut app, "dy");
        let job = app.take_job().unwrap();
        assert_eq!(job, many(|t| Job::Remove { t, pane: None }, &["跑", "闲", "错"]));
        // Selection ends; hidden from the list first, removed together in the background
        assert!(!app.picking());
        assert_eq!(order(&app), ["问", "未读"]);
        assert_eq!(app.doing().as_deref(), Some("正在删除 3 个…"));
        let gone = |t: &str| crate::trash::Gone { id: format!("id-{t}"), title: t.into(), group: "work".into(), archived: false };
        let item = crate::trash::Item::sessions("default", vec![gone("跑"), gone("闲"), gone("错")]);
        let out = Outcome::Binned { msg: format!("已删除{} · ^Z 撤销", item.name()), item: Box::new(item) };
        app.job_done(&job, out, sessions().into_iter().filter(|s| s.group != "work").collect());
        assert_eq!(said(&app), "已删除「跑」等 3 个 · ^Z 撤销");
        // One entry in the Trash; ^Z restores them together, ones open before the removal start again (ones stopped on an
        // error don't count)
        assert_eq!(app.trash.items(epoch()).len(), 1);
        ctrl(&mut app, 'z');
        let Some(Job::Restore { item, start }) = app.take_job() else { panic!("{:?}", app.job) };
        assert_eq!((item.sessions.len(), start), (3, vec!["id-跑".to_string(), "id-闲".into(), "id-错".into()]));
    }

    #[test]
    fn space_jumps_until_something_is_selected_and_then_ticks() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
        select(&mut app, "跑");
        // Nothing selected: Space is the letter jump, nothing gets ticked
        press(&mut app, KeyCode::Char(' '));
        assert!(app.jump.is_some() && !app.picking());
        press(&mut app, KeyCode::Esc);
        // Once a range is kept, Space ticks / unticks the highlighted row and moves down
        keys(&mut app, "vj");
        press(&mut app, KeyCode::Esc);
        assert_eq!(ticked(&mut app), ["跑", "闲"]);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!((ticked(&mut app), at(&app)), (vec!["跑".to_string()], "错".to_string()));
        assert!(app.jump.is_none());
        // Unticking the last one ends multi-select, and Space jumps again
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.picking());
        press(&mut app, KeyCode::Char(' '));
        assert!(app.jump.is_some());
        press(&mut app, KeyCode::Esc);
        // Picking a reference (aca ui --ref): Space jumps there too, and ⇧ with a group's letter goes into that group, still
        // picking
        app.insert_to = Some("%7".into());
        press(&mut app, KeyCode::Char(' '));
        assert!(app.jump.is_some() && !app.picking());
        press(&mut app, KeyCode::Char('F'));
        assert!(app.jump.is_none() && app.group.as_deref() == Some("fun") && app.insert_to.is_some() && app.put.is_none());
    }

    #[test]
    fn v_and_shift_v_stretch_runs_that_select_and_unselect_as_in_yazi() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
        // (wide enough for all four keys after the first act: each cap is two cells wider than the words were)
        let foot = |app: &mut App| reader_rows(app, 90, 20).pop().unwrap();
        // What the green block says, then keys (as caps)
        let has = |foot: String, said: &[&str], keys: &[&str]| {
            let keys = keys.iter().map(|k| caps(k));
            said.iter().map(|s| s.to_string()).chain(keys).for_each(|w| assert!(foot.contains(&w), "缺「{w}」:{foot}"))
        };
        // v again while range-selecting: the unsettled range is dropped, starting over from the highlighted row (this row
        // included)
        select(&mut app, "问");
        keys(&mut app, "vjv");
        assert!(app.anchor.is_some());
        assert_eq!(ticked(&mut app), ["未读"]);
        has(foot(&mut app), &["连选", "已选 1"], &["Esc 结束"]);
        // Esc: this range is settled (ticked), range select ends
        keys(&mut app, "k");
        press(&mut app, KeyCode::Esc);
        assert!(app.anchor.is_none());
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        // Highlight on a ticked row: Space unticks it; v range-selects, V range-unticks, whether or not this row is ticked
        has(foot(&mut app), &["已选 2"], &["空格 取消选中", "v 连选", "V 连续取消", "Esc 清空选择"]);
        // Space ticks one and moves down a row; back on it, pressing again unticks it
        select(&mut app, "闲");
        has(foot(&mut app), &[], &["空格 选中", "v 连选"]);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.current().unwrap().title, "错");
        assert_eq!(ticked(&mut app), ["问", "未读", "闲"]);
        keys(&mut app, "k ");
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        // v from a ticked row still range-selects; ticked ones stay ticked
        select(&mut app, "问");
        keys(&mut app, "vj");
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        press(&mut app, KeyCode::Esc);
        // V range-unticks; V or v again while ranging: the unsettled range is dropped, starting over from this row with
        // that kind (V / v)
        select(&mut app, "问");
        keys(&mut app, "Vj");
        assert!(ticked(&mut app).is_empty());
        has(foot(&mut app), &["取消选中", "已选 0"], &["Esc 结束"]);
        press(&mut app, KeyCode::Char('V'));
        assert_eq!(ticked(&mut app), ["问"]);
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(ticked(&mut app), ["问", "未读"]);
        has(foot(&mut app), &["连选", "已选 2"], &[]);
        press(&mut app, KeyCode::Esc);
        // Space while ranging: ticks / unticks this one and moves down, still ranging, the range grows with it
        select(&mut app, "跑");
        keys(&mut app, "v ");
        assert!(app.anchor.is_some());
        assert_eq!(app.current().unwrap().title, "闲");
        assert_eq!(ticked(&mut app), ["问", "未读", "跑", "闲"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(ticked(&mut app), ["问", "未读", "跑", "闲"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking());
        // Not ranging: Esc unselects all, pressing it again exits
        select(&mut app, "问");
        keys(&mut app, "vj");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking() && !app.quit && ticked(&mut app).is_empty());
        // V works with nothing ticked (as in yazi): an untick range, none selected; Esc leaves
        press(&mut app, KeyCode::Char('V'));
        assert!(app.picking());
        has(foot(&mut app), &["取消选中", "已选 0"], &["Esc 结束"]);
        assert!(!foot(&mut app).contains(&caps("d 删除")));
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking() && !app.quit);
        // Space on a group header while multi-selecting: ticks all listed in the group, again unticks them all (outside
        // multi-select Space still jumps)
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Esc);
        let p = app.head_pos("work").unwrap();
        app.select_head(p);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(ticked(&mut app), ["问", "跑", "闲", "错"]);
        app.select_head(p);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(ticked(&mut app), ["问"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking());
        // Ranging up from the group's first one to its header: the one below the header isn't included
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        assert!(app.head.is_some());
        assert_eq!(ticked(&mut app), ["未读"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(ticked(&mut app), ["未读"]);
        press(&mut app, KeyCode::Esc);
        assert!(!app.picking());
        press(&mut app, KeyCode::Esc);
        assert!(!app.quit); // at the bottom, Esc doesn't exit
        keys(&mut app, "qq");
        assert!(app.quit);
    }

    #[test]
    fn p_u_and_q_on_the_selection_are_one_step_each() {
        let mut app = fixture();
        to_scope(&mut app, Scope::Week);
        let pinned = |app: &App| app.all.iter().filter(|s| s.pinned).map(|s| s.title.clone()).collect::<Vec<_>>();
        let st = |app: &App, t: &str| app.all.iter().find(|s| s.title == t).unwrap().state;
        // p: if any aren't pinned, pin them all
        pin(&mut app, "跑");
        select(&mut app, "闲");
        keys(&mut app, "vj");
        app.msg = None;
        let foot = reader_rows(&mut app, 120, 20).pop().unwrap();
        assert!(foot.contains(&caps("p 置顶")) && foot.contains(&caps("Q 移动到工作区")), "{foot}");
        // When narrow, the later ones are dropped first
        assert!(!reader_rows(&mut app, 80, 20).pop().unwrap().contains(&caps("p 置顶")));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!((pinned(&app), said(&app)), (vec!["跑".to_string(), "闲".into(), "错".into()], "已置顶 2 个"));
        assert!(!app.picking());
        // u: if any are unread, mark them read; idle ones untouched
        select(&mut app, "未读");
        press(&mut app, KeyCode::Char('v'));
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!((st(&app, "未读"), st(&app, "闲"), said(&app)), (State::Idle, State::Idle, "已将 1 个标为已读"));
        // Q+key: sends the selected ones there together
        select(&mut app, "问");
        keys(&mut app, "vjQw");
        assert_eq!((space(&app, "问"), space(&app, "未读")), ('w', 'w'));
        assert!(said(&app).starts_with("「问」等 2 个 → "), "{}", said(&app));
        // ^Z goes back one step at a time: sending to a workspace, marking read and pinning are one step each
        ctrl(&mut app, 'z');
        assert_eq!((space(&app, "问"), space(&app, "未读")), ('1', '1'));
        ctrl(&mut app, 'z');
        assert_eq!((st(&app, "未读"), said(&app)), (State::Unread, "已撤销标为已读 1 个"));
        ctrl(&mut app, 'z');
        assert_eq!((pinned(&app), said(&app)), (vec!["跑".to_string()], "已撤销置顶 2 个"));
    }
}
