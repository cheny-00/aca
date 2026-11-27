//! Running what a key asked for in the background and taking it back: jobs and their results, rows that vanish until the job
//! lands, and ctrl-z.

use super::*;

/// A row confirmed with d and hidden ahead of time (removing a group: everything in it is hidden)
#[derive(Debug)]
pub(super) struct Vanish {
    pub(super) job: Job,
    /// Removal finished, hidden until this time; None = still removing
    pub(super) until: Option<Instant>,
    /// Sessions running before the removal (registered ids; restarted after ^Z undoes it, since once hidden they can't be found in the
    /// list)
    pub(super) running: Vec<String>,
}

impl Vanish {
    /// A session is hidden along with its windows; closing a single pane hides just that pane (by pane id: the session name is the whole
    /// session's). Not hidden when it's the session's first pane being closed: windows can't lose their head; after a refresh, the pane
    /// that becomes the head moves up onto the session's row
    fn hides(&self, s: &Session) -> bool {
        hides(&self.job, s)
    }

    /// This one was undone (the item in the Trash): no need to keep hiding it
    fn undone_by(&self, item: &crate::trash::Item) -> bool {
        undone_by(&self.job, item)
    }
}

fn hides(job: &Job, s: &Session) -> bool {
    match job {
        Job::Remove { pane: Some(p), .. } => s.sub.is_some() && s.pane == *p,
        Job::Remove { t, pane: None } => s.family() == t.name,
        Job::Group(a) => a.covers(&s.profile, &s.group),
        Job::Many(jobs) => jobs.iter().any(|j| hides(j, s)),
        _ => false,
    }
}

fn undone_by(job: &Job, item: &crate::trash::Item) -> bool {
    match job {
        Job::Remove { t, pane: None } => item.group.is_empty() && item.sessions.iter().any(|g| g.id == t.id),
        Job::Group(groups::Act::Delete { group, .. }) => *group == item.group,
        Job::Many(jobs) => jobs.iter().any(|j| undone_by(j, item)),
        _ => false,
    }
}

/// What a job on this session needs (a registered session's key is its id)
pub(super) fn target_of(s: &Session) -> Target {
    Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() }
}

/// The step of a done job that can be undone (removing sessions into the Trash is recorded separately, see `job_done`). locked: whether
/// the title lock was on before the rename
fn undo_of(job: &Job, locked: bool) -> Option<Undo> {
    match job {
        // A closed pane / plain tmux session: can't be undone, but still recorded as a step (see undo.rs)
        Job::Remove { t, .. } => Some(Undo::Gone { title: t.title.clone() }),
        Job::Stop { t } => Some(Undo::Stop { t: t.clone() }),
        Job::Park { t, p } => Some(Undo::Park { t: t.clone(), p: p.clone() }),
        Job::Move { t, from, groups, .. } => Some(Undo::Move { t: t.clone(), from: from.clone(), groups: groups.clone() }),
        Job::Rename { t, .. } => Some(Undo::Rename { t: t.clone(), from: t.title.clone(), unlock: !locked }),
        Job::Mcp { t, dir, on, off } => Some(Undo::Mcp { t: t.clone(), dir: dir.clone(), on: off.clone(), off: on.clone() }),
        Job::Group(a) => a.undo().map(Undo::Group),
        _ => None,
    }
}

/// Jobs that remove things: hide from the list first, without waiting for the background removal; several can be removed together (see
/// `Vanish`)
fn deleting(job: &Job) -> bool {
    match job {
        Job::Remove { .. } | Job::Group(groups::Act::Delete { .. }) => true,
        Job::Many(jobs) => !jobs.is_empty() && jobs.iter().all(deleting),
        _ => false,
    }
}

impl App {
    /// Work the run loop should put in the background; also record "busy" and whether the highlight should follow the restart.
    /// Deletes don't record "busy" (`vanish` tracks them)
    pub fn take_job(&mut self) -> Option<Job> {
        if self.job.is_none()
            && self.doing().is_none()
            && !self.reorder_writing
            && let Some(job) = self.reorders.front().cloned()
        {
            self.reorder_writing = true;
            self.job = Some(job);
        }
        let job = self.job.take()?;
        // Earlier messages give way to the "…ing…" status; ⇧↑ ⇧↓ writes don't count
        // (each write while pressing repeatedly would wipe other messages)
        if !matches!(job, Job::Order { .. } | Job::GroupOrder { .. }) {
            self.msg = None;
        }
        if !deleting(&job) {
            self.busy = Some(job.doing());
            self.follow = job.target().is_some_and(|t| self.selected.as_deref() == Some(t.name.as_str()));
        }
        Some(job)
    }

    /// The "…ing…" status in the bottom bar, also what other operations wait for: other work, or deletes still running (can be several)
    pub(super) fn doing(&self) -> Option<String> {
        if let Some(b) = &self.busy {
            return Some(b.clone());
        }
        let live: Vec<&Job> = self.vanish.iter().filter(|v| v.until.is_none()).map(|v| &v.job).collect();
        // A batch deleted together counts by sessions
        let n: usize = live.iter().map(|j| if let Job::Many(v) = j { v.len() } else { 1 }).sum();
        match live[..] {
            [] => None,
            [j] => Some(j.doing()),
            _ if live.iter().all(|j| matches!(j, Job::Remove { pane: Some(_), .. })) => Some(tr!("正在关闭 {n} 个…", "closing {n}…")),
            _ => Some(tr!("正在删除 {n} 个…", "deleting {n}…")),
        }
    }

    /// Remove from all what d deleted (still deleting, just deleted); child sessions hanging under it are counted as after the
    /// delete (deleting a parent doesn't take the child sessions with it)
    pub(super) fn drop_vanished(&mut self) {
        let now = Instant::now();
        self.vanish.retain(|v| v.until.is_none_or(|u| now < u));
        if self.vanish.is_empty() {
            return;
        }
        let vanish = &self.vanish;
        self.all.retain(|s| !vanish.iter().any(|v| v.hides(s)));
        let heads: Vec<&str> = vanish
            .iter()
            .flat_map(|v| match &v.job {
                Job::Many(jobs) => jobs.iter().collect(),
                j => vec![j],
            })
            .filter_map(|j| match j {
                Job::Remove { t, pane: None } => Some(t.name.as_str()),
                _ => None,
            })
            .collect();
        for s in &mut self.all {
            if s.up.parent().is_some_and(|p| heads.contains(&p)) {
                s.up = Up::Gone;
            }
        }
    }

    /// Background work done. fresh is the sessions reread afterwards: a restart
    /// changes the tmux session name, so pins, highlight and seen records follow
    pub fn job_done(&mut self, job: &Job, out: Outcome, fresh: Vec<Session>) {
        let mut was_running = None;
        if deleting(job) {
            // Deleted (into the trash): hide it a while longer; not deleted: put it back
            let ok = matches!(out, Outcome::Done(_) | Outcome::Binned { .. });
            let at = self.vanish.iter().position(|v| v.until.is_none() && v.job == *job);
            was_running = at.map(|i| self.vanish[i].running.clone());
            match at {
                Some(i) if ok => self.vanish[i].until = Some(Instant::now() + GONE_LINGER),
                Some(i) => drop(self.vanish.remove(i)),
                None => {}
            }
        } else {
            self.busy = None;
        }
        if let Job::Order { .. } | Job::GroupOrder { .. } = job {
            self.reorder_writing = false;
            self.reorders.pop_front();
            // Write failed, the queued ones after it don't count either, the list goes back to the registry's order
            if matches!(out, Outcome::Failed(_)) {
                self.reorders.clear();
            }
        }
        let follow = std::mem::take(&mut self.follow);
        if let (Job::Group(groups::Act::Edit { group, to, .. }), Outcome::Done(_)) = (job, &out)
            && group != to
        {
            self.group_renamed(group, to);
        }
        if let Some((_, u, msg)) = self.reverting.take_if(|(j, ..)| j == job) {
            return self.reverted(job, u, msg, out, fresh);
        }
        // Done and undoable: record a step
        // What it was like before (running or not, title locked or not)
        let (running, locked) = self
            .all
            .iter()
            .find(|s| s.sub.is_none() && job.target().is_some_and(|t| s.deck_id == t.id && s.profile == t.profile))
            .map_or((false, true), |s| (!s.state.gone(), s.locked));
        let running = match (was_running, job.target()) {
            (Some(v), _) => v,
            (None, Some(t)) if running => vec![t.id.clone()],
            _ => vec![],
        };
        let mut out = out;
        // Deleted by auto-cleanup: say it was automatic and stopped for how long
        if let Some((_, after)) = self.swept.take_if(|(j, _)| j == job)
            && let Outcome::Binned { msg, item } = &mut out
        {
            let (age, who) = (crate::print::age(after), item.name());
            *msg = tr!("已自动删除停止超过 {age} 的{who} · ^Z 撤销", "auto-deleted {who}, stopped over {age} · ^Z undoes");
        }
        // A session / group deleted with d went to the trash: record the entry (no longer listed), ^Z brings it back
        if let Outcome::Binned { item, .. } = &mut out {
            let mut item = *std::mem::take(item);
            // The group's recorded workspace: restored as before when brought back (its notes went with this entry in the
            // registry, see `groups::job`)
            for g in &mut item.groups {
                g.space = self.spaces.group_of(&item.profile, &g.path);
            }
            let u = Undo::Bin { id: item.id.clone(), label: item.name(), start: running };
            let gone = item.groups.clone();
            match self.trash.add(item) {
                Ok(()) => {
                    self.remember(u);
                    // The group's records are cleared: a later group of the same name shouldn't land in the old place
                    if !gone.is_empty() {
                        let profile = crate::data::REGISTERED;
                        let _ = self.spaces.forget_groups(&|p, g| p == profile && gone.iter().any(|x| x.path == g));
                    }
                }
                Err(e) => out = Outcome::Failed(tr!("已删除,但未能记入回收站:{e}", "deleted, but the trash didn't record it: {e}")),
            }
        }
        // An entry in the trash was brought back / deleted for real
        match (job, &out) {
            (Job::Restore { item, .. }, Outcome::Done(m)) => return self.restored(item, fresh, m),
            (Job::Purge { item }, Outcome::Done(_)) => drop(self.trash.forget(&item.id)),
            _ => {}
        }
        if let Outcome::Done(_) = &out
            && let Some(u) = undo_of(job, locked)
        {
            self.remember(u);
        }
        // Done together: the ones that succeeded become one step, undone together by ^Z. Brought back from the trash together:
        // the highlight goes to the first one
        let mut landed = None;
        if let (Job::Many(jobs), Outcome::Many(outs)) = (job, &out) {
            let done = || jobs.iter().zip(outs).filter(|(_, o)| matches!(o, Outcome::Done(_))).map(|(j, _)| j);
            let us: Vec<Undo> = done().filter_map(|j| undo_of(j, true)).collect();
            if !us.is_empty() {
                self.remember(Undo::Many(us));
            }
            for j in done() {
                match j {
                    Job::Restart { t, .. } => {
                        let follow = self.selected.as_deref() == Some(t.name.as_str());
                        self.restarted(t, &fresh, follow);
                    }
                    Job::Restore { item, .. } if item.note.is_some() => item.note.iter().for_each(|n| self.note_back(n)),
                    Job::Restore { item, .. } => {
                        self.unbin(item);
                        if landed.is_none() {
                            self.land(item, &fresh);
                            landed = Some(item);
                        }
                    }
                    Job::Purge { item } => drop(self.trash.forget(&item.id)),
                    _ => {}
                }
            }
        }
        if let (Job::Restart { t, .. } | Job::Rehome { t, .. }, Outcome::Done(_)) = (job, &out) {
            self.restarted(t, &fresh, follow);
        }
        // Adopted: the row's key changed from the tmux session name to the registered id, pins follow
        if let (Job::Adopt { t, .. }, Outcome::Made { id, .. }) = (job, &out) {
            let _ = self.pins.rename(&t.name, id);
        }
        if let (Job::Send { key, .. }, Outcome::Done(_)) = (job, &out) {
            self.note_filled(key);
        }
        // Hand off: the note went with the handoff note to the new session
        if let (Job::Handoff { note: Some(key), .. }, Outcome::Made { .. } | Outcome::Handed { .. }) = (job, &out) {
            self.note_filled(key);
        }
        self.set_sessions(fresh);
        if let Some(item) = landed {
            self.land_on_group(item);
        }
        // A group made with ^N is still empty: the highlight goes to its heading
        // (even while the new session box is open, so Esc lands here)
        if let (Job::Group(groups::Act::Create { group }), Outcome::Done(_)) = (job, &out)
            && let Some(p) = self.head_pos(group).filter(|&p| self.all[self.view[p]].blank)
        {
            self.select_head(p);
        }
        self.reload_overlays();
        self.reload_settings();
        match out {
            Outcome::Done(m) | Outcome::Failed(m) | Outcome::Binned { msg: m, .. } if !m.is_empty() => self.flash(&m),
            Outcome::Done(_) | Outcome::Failed(_) | Outcome::Binned { .. } => {}
            Outcome::Many(outs) => {
                if let Job::Many(jobs) = job {
                    self.flash(&ops::many_msg(jobs, &outs));
                }
            }
            Outcome::Made { msg, id } => self.made(&id, &msg),
            Outcome::Handed { msg, id, note } => {
                self.save_note(&id, &note);
                self.made(&id, &msg);
            }
        }
    }

    /// After a restart / account switch the tmux session name changed: pins follow, and the highlight too with follow
    fn restarted(&mut self, t: &Target, fresh: &[Session], follow: bool) {
        let new = fresh.iter().find(|s| s.deck_id == t.id && s.profile == t.profile).map(|s| s.name.clone());
        if let Some(new) = new.filter(|n| *n != t.name) {
            let _ = self.pins.rename(&t.name, &new);
            if follow {
                self.selected = Some(new);
            }
        }
    }

    /// A new session forked / handed off: lands in the current workspace and gets the highlight (if filtered out by the current
    /// scope / filter, the highlight stays put)
    fn made(&mut self, id: &str, msg: &str) {
        self.adopt(id);
        self.apply_pins();
        self.rebuild();
        let name = self.all.iter().find(|s| !id.is_empty() && s.deck_id == id && s.sub.is_none()).map(|s| s.name.clone());
        if let Some(p) = name.and_then(|n| self.view.iter().position(|&i| self.all[i].name == n)) {
            self.select(Some(p));
            // The highlight is on a session: a group showing its notes page turns to its sessions
            self.show_sessions();
        }
        self.flash(msg);
    }

    /// Record a step ^Z can undo
    pub(super) fn remember(&mut self, u: Undo) {
        undo::push(&mut self.undo, u);
    }

    /// An entry in the trash was brought back (⏎ on b's page, ^Z): the group's recorded workspace as before, no longer hidden,
    /// the highlight on it (a group: on its heading)
    fn restored(&mut self, item: &crate::trash::Item, fresh: Vec<Session>, msg: &str) {
        // A note: back in its notebook
        if let Some(n) = &item.note {
            self.note_back(n);
            self.set_sessions(fresh);
            self.reload_overlays();
            return self.flash(msg);
        }
        self.unbin(item);
        self.land(item, &fresh);
        let want = self.selected.clone();
        self.set_sessions(fresh);
        self.land_on_group(item);
        self.reload_overlays();
        // Brought back stopped: listing only open ones it isn't there, so say which key shows it (bringing it back counts as
        // activity, so the last 7 days have it). Not when it's in another workspace or group, where w wouldn't show it either
        let ours = |s: &Session| match item.group.as_str() {
            "" => want.as_ref() == Some(&s.name),
            g => s.group == g,
        };
        let shown = |s: &Session| self.view.iter().any(|&i| self.all[i].name == s.name);
        let waiting = self.all.iter().any(|s| {
            ours(s) && s.space == self.space && self.group.as_ref().is_none_or(|g| &s.group == g) && in_scope(Scope::Week, s) && !shown(s)
        });
        let m = if self.scope == Scope::Open && self.status.is_none() && !self.all.iter().any(|s| ours(s) && shown(s)) && waiting {
            tr!("{msg} · w 近 7 天", "{msg} · w 7 days")
        } else {
            msg.to_string()
        };
        self.flash(&m);
    }

    /// A note deleted on its own was brought back: in the group being looked at, the notes page's cursor goes to it
    fn note_back(&mut self, n: &crate::trash::GoneNote) {
        if self.group.as_deref() == Some(n.group.as_str()) {
            self.note_at = Some(n.id.clone());
        }
    }

    /// An entry brought back: removed from the trash, the group's recorded workspace as before, no longer hidden (its notes came
    /// back in the registry, see `ops::restore`)
    fn unbin(&mut self, item: &crate::trash::Item) {
        let _ = self.trash.forget(&item.id);
        let spaces: Vec<(spaces::Group, Option<char>)> =
            item.groups.iter().filter(|g| g.space.is_some()).map(|g| ((item.profile.clone(), g.path.clone()), g.space)).collect();
        if !spaces.is_empty() {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&[], &spaces, &keep);
        }
        self.vanish.retain(|v| !v.undone_by(item));
    }

    /// A session was brought back (several deleted together: the first): after the list is replaced, the highlight goes to it
    fn land(&mut self, item: &crate::trash::Item, fresh: &[Session]) {
        let one = item.sessions.first().filter(|_| item.group.is_empty());
        if let Some(s) = one.and_then(|g| fresh.iter().find(|s| s.sub.is_none() && s.deck_id == g.id && s.profile == item.profile)) {
            self.selected = Some(s.name.clone());
        }
    }

    /// A group was brought back: the highlight goes to its heading (after the list is replaced)
    fn land_on_group(&mut self, item: &crate::trash::Item) {
        if !item.group.is_empty()
            && let Some(p) = self.head_pos(&item.group)
        {
            self.select_head(p);
        }
    }

    /// The session undo looks for: the one still listed (its tmux name may have changed with a restart). If gone, say so, return None
    fn still_here(&mut self, t: &Target) -> Option<(Target, &Session)> {
        if self.here(t).is_none() {
            self.flash(&tr!("「{}」已不存在", "“{}” is gone", clip(&t.title, 16).trim_end()));
            return None;
        }
        self.here(t)
    }

    /// Like `still_here`, but says nothing if gone (one missing among several undone together is no big deal)
    fn here(&self, t: &Target) -> Option<(Target, &Session)> {
        let s = self.all.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile)?;
        Some((Target { name: s.name.clone(), title: s.title.clone(), ..t.clone() }, s))
    }

    /// Find a session's current tmux name by key (`notes::key`) (a restart changes it); if not found, use the recorded one
    fn name_now(&self, key: &str, name: String) -> String {
        self.all.iter().find(|s| s.sub.is_none() && notes::key(s) == key).map_or(name, |s| s.name.clone())
    }

    /// ^Z: undo the last step (see undo.rs)
    pub(super) fn undo_last(&mut self) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        let Some(u) = self.undo.pop() else { return self.flash(t!("没有可撤销的操作", "nothing to undo")) };
        let keep = self.keep_marks();
        let short = |t: &str| clip(t, 16).trim_end().to_string();
        match u.clone() {
            Undo::Gone { title } => self.flash(&tr!("「{}」无法撤销", "“{}” can't be undone", short(&title))),
            Undo::Pin { key, name, title, rank } => {
                let name = self.name_now(&key, name);
                let m = match self.pins.put(&name, rank) {
                    Ok(()) => tr!(
                        "已撤销{}「{}」",
                        "undid {} “{}”",
                        if rank.is_some() { t!("取消置顶", "unpin") } else { t!("置顶", "pin") },
                        short(&title)
                    ),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.selected = Some(name);
                self.flash(&m);
                self.apply_pins();
                self.rebuild();
            }
            Undo::Read { key, name, title, what, mark, state, seen } => {
                let name = self.name_now(&key, name);
                if let Some(t) = seen {
                    aca_core::runtime::update(&name, |r| r.seen = t);
                }
                let m = match self.reads.put(&key, mark, &keep) {
                    Ok(()) => tr!("已撤销标为{what}「{}」", "undid marking “{}” {what}", short(&title)),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                // The list's state has the read records laid over it, recomputing would lose the original: put it back directly,
                // the next refresh computes from the new records
                if let Some(s) = self.all.iter_mut().find(|s| s.name == name) {
                    s.state = state;
                }
                self.selected = Some(name);
                self.flash(&m);
                self.rebuild();
            }
            Undo::Space { what, sessions, groups } => {
                let m = match self.spaces.assign(&sessions, &groups, &keep) {
                    Ok(()) => tr!("已撤销 {what}", "undid {what}"),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.flash(&m);
                self.apply_pins();
                self.rebuild();
            }
            Undo::Note { key, title, text } => {
                let m = match (self.put_note(&key, &text), notes::note_of(&key)) {
                    // One in a notebook: rewritten, deleted or just written; the notes page's cursor goes to it
                    (Ok(()), Some(id)) => {
                        self.note_at = Some(id.to_string());
                        tr!("已撤销 · 笔记「{}」", "undid · note “{}”", short(&title))
                    }
                    (Ok(()), None) => tr!("已撤销修改便签「{}」", "undid the draft change on “{}”", short(&title)),
                    (Err(e), _) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.flash(&m);
                self.apply_pins();
            }
            Undo::Bin { id, label, start } => {
                self.trash.reload();
                let Some(item) = self.trash.get(&id).cloned() else {
                    return self.flash(&tr!("回收站中已没有{label}", "{label} is no longer in the trash"));
                };
                self.revert(Job::Restore { item: Box::new(item), start }, u, None);
            }
            Undo::Park { t, p } => {
                if !self.all.iter().any(|s| s.pane == p.id && s.state == State::Dead) {
                    return self.flash(&tr!("「{}」已不是停止状态", "“{}” isn't stopped any more", short(&p.label)));
                }
                let m = tr!("已撤销停止:「{}」已在原位置重新启动", "undid the stop: “{}” started again in place", short(&p.label));
                self.revert(Job::Respawn { t, p }, u, Some(m));
            }
            Undo::Stop { t } => {
                let Some((t, s)) = self.still_here(&t) else { return };
                if !s.state.gone() && s.state != State::Dead {
                    return self.flash(&tr!("「{}」已在运行", "“{}” is already running", short(&t.title)));
                }
                self.revert(Job::Revive { t }, u, None);
            }
            Undo::Move { t, from, groups } => {
                let Some((t, s)) = self.still_here(&t) else { return };
                let now = s.group.clone();
                if now.eq_ignore_ascii_case(&from) {
                    return self.flash(&tr!("已在 {from} 中", "already in {from}"));
                }
                let m = tr!("已撤销移动分组:「{}」回到 {from}", "undid the move: “{}” is back in {from}", short(&t.title));
                self.stay_on_move(&t.name, &from);
                self.revert(Job::Move { t, from: now, to: from, groups, home: String::new() }, u, Some(m));
            }
            Undo::Rename { t, from, unlock } => {
                let Some((t, _)) = self.still_here(&t) else { return };
                let m = tr!("已撤销重命名:恢复为「{}」", "undid the rename: back to “{}”", short(&from));
                self.revert(Job::Rename { t, to: from, unlock }, u, Some(m));
            }
            Undo::Mcp { t, dir, on, off } => {
                let Some((t, _)) = self.still_here(&t) else { return };
                let m = tr!(
                    "已撤销修改 MCP「{}」 · 重启后生效",
                    "undid the MCP change on “{}” · takes effect after a restart",
                    short(&t.title)
                );
                self.revert(Job::Mcp { t, dir, on, off }, u, Some(m));
            }
            Undo::Group(a) => {
                let m = match &a {
                    groups::Act::Edit { group, to, .. } if group != to => {
                        tr!("已撤销修改分组:{group} 改回 {to}", "undid the group change: {group} back to {to}")
                    }
                    _ => t!("已撤销修改分组默认目录", "undid the change to the group's default directory").to_string(),
                };
                self.revert(Job::Group(a), u, Some(m));
            }
            Undo::Many(us) => self.undo_many(&us, u),
        }
    }

    /// ^Z undoing a step done together via v multi-select: pins and read marks restored on the spot; stops and group moves undone
    /// together in the background (ones gone or already back are skipped)
    fn undo_many(&mut self, us: &[Undo], u: Undo) {
        let keep = self.keep_marks();
        let (mut put, mut jobs, mut err) = (0, vec![], None);
        // Put back in reverse: pin order as before
        for x in us.iter().rev().cloned() {
            match x {
                Undo::Pin { key, name, rank, .. } => {
                    let name = self.name_now(&key, name);
                    match self.pins.put(&name, rank) {
                        Ok(()) => put += 1,
                        Err(e) => err = Some(e.to_string()),
                    }
                }
                Undo::Read { key, name, mark, state, seen, .. } => {
                    let name = self.name_now(&key, name);
                    if let Some(t) = seen {
                        aca_core::runtime::update(&name, |r| r.seen = t);
                    }
                    match self.reads.put(&key, mark, &keep) {
                        Ok(()) => put += 1,
                        Err(e) => err = Some(e.to_string()),
                    }
                    if let Some(s) = self.all.iter_mut().find(|s| s.name == name) {
                        s.state = state;
                    }
                }
                Undo::Stop { t } => {
                    if let Some((t, s)) = self.here(&t)
                        && (s.state.gone() || s.state == State::Dead)
                    {
                        jobs.push(Job::Revive { t });
                    }
                }
                Undo::Move { t, from, groups } => {
                    if let Some((t, s)) = self.here(&t)
                        && !s.group.eq_ignore_ascii_case(&from)
                    {
                        let now = s.group.clone();
                        self.stay_on_move(&t.name, &from);
                        jobs.push(Job::Move { t, from: now, to: from, groups, home: String::new() });
                    }
                }
                _ => {}
            }
        }
        jobs.reverse();
        let n = jobs.len();
        let m = match us.first() {
            Some(Undo::Pin { rank: Some(_), .. }) => tr!("已撤销取消置顶 {put} 个", "undid unpinning {put}"),
            Some(Undo::Pin { .. }) => tr!("已撤销置顶 {put} 个", "undid pinning {put}"),
            Some(Undo::Read { what, .. }) => tr!("已撤销标为{what} {put} 个", "undid marking {put} {what}"),
            Some(Undo::Stop { .. }) => tr!("已撤销停止:{n} 个已重新启动", "undid the stop: {n} started again"),
            Some(Undo::Move { .. }) => tr!("已撤销移动分组:{n} 个回到原分组", "undid the move: {n} back in their groups"),
            _ => String::new(),
        };
        if !jobs.is_empty() {
            return self.revert(Job::Many(jobs), u, Some(m));
        }
        let m = match err {
            Some(e) => tr!("保存失败:{e}", "not saved: {e}"),
            None if put == 0 => t!("没有可撤销的操作", "nothing left to undo").to_string(),
            None => m,
        };
        self.flash(&m);
        self.apply_pins();
        self.rebuild();
    }

    /// Undo work done in the background: queue it, `reverted` finishes up when done
    fn revert(&mut self, job: Job, u: Undo, msg: Option<String>) {
        self.reverting = Some((job.clone(), u, msg));
        self.job = Some(job);
    }

    fn reverted(&mut self, job: &Job, u: Undo, msg: Option<String>, out: Outcome, fresh: Vec<Session>) {
        let m = match out {
            Outcome::Done(m) => {
                if let Job::Restore { item, .. } = job {
                    return self.restored(item, fresh, &msg.unwrap_or(m));
                }
                // The highlight goes to the one brought back
                if let Some(t) = u.target()
                    && let Some(s) = fresh.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile)
                {
                    self.selected = Some(s.name.clone());
                }
                msg.unwrap_or(m)
            }
            // Undo failed: put this step back, ^Z can be pressed again
            Outcome::Failed(m) => {
                self.undo.push(u);
                m
            }
            // Undone together: put back only if none succeeded; a partial undo is reported as it is
            Outcome::Many(outs) => {
                let jobs = if let Job::Many(v) = job { v.as_slice() } else { &[] };
                let ok = outs.iter().filter(|o| matches!(o, Outcome::Done(_))).count();
                if ok == 0 {
                    self.undo.push(u);
                }
                match msg {
                    Some(m) if ok == outs.len() => m,
                    _ => ops::many_msg(jobs, &outs),
                }
            }
            Outcome::Made { msg, .. } | Outcome::Handed { msg, .. } | Outcome::Binned { msg, .. } => msg,
        };
        self.set_sessions(fresh);
        self.flash(&m);
    }
}
