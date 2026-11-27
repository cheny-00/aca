//! Questions before acting: the confirmations, the one-line prompts (renaming, a group's name and path, notes) and what
//! they show about what is about to happen.

use super::*;

/// Things waiting for y to confirm
pub(super) enum Confirm {
    Remove(Removal),
    /// d on a group: remove the group, with its subgroups and the sessions in it
    Group(GroupRemoval),
    /// why: why ask (running / waiting for your choice); force: not used now
    Restart {
        t: Target,
        why: String,
        force: bool,
    },
    /// D: attached = connected from elsewhere, will be disconnected
    Stop {
        t: Target,
        attached: bool,
    },
    /// D / R when a session has more than one pane: only stop (move to the end of the queue) / restart this pane in place, see park.rs.
    /// restart: it's R; why: why ask (running…); next: when the head pane is stopped, the pane that becomes the head
    Pane {
        t: Target,
        p: crate::park::Pane,
        row: String,
        restart: bool,
        why: String,
        next: Option<String>,
    },
    /// A: adopt this plain tmux session. plan: what was recognized and how it will be registered (that's what's shown, and what y
    /// carries out)
    Adopt {
        t: Target,
        plan: Box<aca_core::adopt::Plan>,
    },
    /// s on a row waiting for you (◆): answer it from the list. question: what the box says it asks; shown: that as answering reads
    /// it (None for an unregistered one: only its own screen answers it), checked against every refresh and once more before
    /// pressing; offer: the keys it can be answered with here. Unlike the others, only the keys written in it do anything, and ⏎
    /// goes in
    Answer {
        t: Target,
        question: String,
        shown: Option<answer::Asking>,
        offer: answer::Offer,
    },
    /// d / D / R after v multi-select: on these sessions (in list order). notes: what to watch out for, written in the box (how many
    /// running ones will be interrupted…)
    Many {
        act: Batch,
        ts: Vec<Target>,
        notes: Vec<String>,
    },
    /// d on a group's notes page: delete this note (its id, and its name for the box)
    Note {
        id: String,
        name: String,
    },
    /// d on a group's tasks page: delete this ticket (its number, and its title for the box)
    Task {
        n: i64,
        title: String,
    },
}

/// The kinds after v multi-select that ask first
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Batch {
    Remove,
    Stop,
    Restart,
}

impl Batch {
    pub(super) fn job(self, t: Target) -> Job {
        match self {
            Batch::Remove => Job::Remove { t, pane: None },
            Batch::Stop => Job::Stop { t },
            Batch::Restart => Job::Restart { t, force: false },
        }
    }
}

/// What the confirm box for d on a group says: what gets removed along with it
pub(super) struct GroupRemoval {
    pub(super) profile: String,
    pub(super) group: String,
    /// Number of subgroups
    pub(super) kids: usize,
    /// Number of sessions (including those in subgroups and archived ones; ones in the Trash don't count)
    pub(super) sessions: usize,
    /// How many of them are archived
    pub(super) archived: usize,
    /// How many of them are still running this turn (removing interrupts them)
    pub(super) busy: usize,
    /// The open ones among them (registered ids; restarted after ^Z undoes it)
    pub(super) running: Vec<String>,
}

/// Single-line input in the bottom bar
pub(super) struct Prompt {
    pub(super) ask: Ask,
    pub(super) text: String,
    /// Hint on the line below the input box (on error); empty = show the description
    pub(super) hint: String,
}

pub(super) enum Ask {
    /// `r`: new title
    Rename(Target),
    /// `G`: new group name; after Enter, go on to fill in the path
    NewGroup,
    /// `e`: the note (key in `notes::key`); orig: the note when opened, if it changed when done, record an undo step
    Note { key: String, title: String, orig: String },
    /// `s`: a reply to send it. Written to its note as typed (key, title, orig as for `Note`); t / pane: whom ⏎ sends it to
    Reply { key: String, title: String, orig: String, t: Target, pane: String },
    /// `f`: what to call the forked session (what follows is what a Claude session must check before forking, see `Job::Fork`)
    Fork(Target),
    /// ^R in S: the workspace's new name
    Space(char),
    /// Settings page: the default directory of `^N`
    GroupDir,
    /// Settings page: the scratch directory (empty = none). `desk`: asked for by `o` on the rail's row for all groups, which
    /// goes on to the desk's new-session box once the directory has its group
    ScratchDir { desk: bool },
    /// `r` on a group: the new group name (may include parents); after Enter, go on to change the default directory
    GroupName(String),
    /// Then fill in its default directory: group is renamed to to, was is the old default directory (empty = none recorded)
    GroupHome { group: String, to: String, was: String },
    /// Tab on a new name in `m`'s list: the directory the new group `to` works in, before it's created and `pick` moved into it
    MoveHome { pick: Box<GroupPick>, to: String },
    /// `n` / `e` on a group's tasks page: a ticket's title — a new one in `group`, or ticket `n`'s rewritten; after Enter, go
    /// on to what counts as done
    TaskTitle { n: Option<i64>, group: String },
    /// Then what counts as done (may be left empty)
    TaskCriteria { n: Option<i64>, group: String, title: String },
}

/// q / Q pressed, waiting for the next key
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pending {
    /// q: q again quits, any other key cancels
    Quit,
    /// Q: which workspace to send the highlighted session to
    Send,
    /// c: what to copy from the highlighted session (c the last reply, b a reference, i s p a see copies.rs)
    Copy,
}

/// A note handed to the editor with ^G
pub struct NoteEdit {
    key: String,
    title: String,
    /// The note before editing started (see `Ask::Note`)
    orig: String,
    pub text: String,
    /// Pressed from the note editing in the bottom bar: go back there to keep writing when done
    from_prompt: bool,
    /// Pressed from the reply box: back into it (whom it goes to, which pane)
    to: Option<(Target, String)>,
}

/// What `m` needs to pick a group
pub struct GroupPick {
    pub t: Target,
    /// Its current group
    from: String,
    /// Its directory (unchanged by moving, shown at the top of fzf)
    pub(super) here: String,
    /// fzf candidates: groups in the same directory, then the list's section order, then groups not in the list; its current one excluded
    pub rows: Vec<ops::GroupRow>,
    /// All groups this profile already has (moving into a missing one creates it first)
    groups: Vec<String>,
    /// The others moved along after v multi-select, with their current groups; empty = only move t
    more: Vec<(Target, String)>,
    /// The group a bare new name is created under: the one they're all moved out of; empty when they aren't in one group
    pub(super) under: String,
    /// Every group with where it works (as in `rows`): a new group works in a directory under its parent's
    pub(super) dirs: Vec<(String, String)>,
}

impl GroupPick {
    /// The top of fzf says what's being moved
    pub fn what(&self) -> String {
        ops::named(&self.t.title, 1 + self.more.len())
    }
}

impl App {
    pub(super) fn ask_remove(&mut self) {
        // Other deletes still running don't matter, delete on; other work (restart, rename…) must finish first
        if let Some(b) = self.busy.clone().or_else(|| self.job.as_ref().map(Job::doing)) {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        let Some(s) = self.current() else { return };
        // A plain tmux session row with other windows / panes inside: close only this cell, the lowest-numbered one becomes the head
        // (tmux's renumber-windows turns #2 into #1). A registered session deletes the whole session: extra windows inside close with
        // it (the confirm box says how many)
        let others = self.all.iter().filter(|x| x.sub.is_some() && x.subagent.is_none() && x.family() == s.name).count();
        let next = match &s.sub {
            None if !s.pane.is_empty() && s.deck_id.is_empty() => self
                .all
                .iter()
                .filter(|x| x.family() == s.name)
                .filter_map(|x| Some((x.sub.as_ref()?, x)))
                .min_by_key(|(y, _)| (y.win, y.pane)),
            _ => None,
        };
        self.confirm = Some(Confirm::Remove(Removal {
            name: s.name.clone(),
            title: s.title.clone(),
            profile: s.profile.clone(),
            id: s.deck_id.clone(),
            worktree: s.worktree,
            attached: s.attached,
            pane: match (&s.sub, next) {
                (Some(x), _) => Some((s.pane.clone(), x.split)),
                (None, Some(_)) => Some((s.pane.clone(), false)),
                (None, None) => None,
            },
            next: next.map(|(y, x)| tr!("{}「{}」", "{} “{}”", y.loc(), clip(&x.title, 16).trim_end())),
            others: if s.sub.is_none() && !s.deck_id.is_empty() { others } else { 0 },
            kids: if s.sub.is_some() || next.is_some() {
                0
            } else {
                self.all.iter().filter(|x| x.sub.is_none() && x.up.parent() == Some(&s.name)).count()
            },
        }));
    }

    /// What R / r / m need: the highlighted registered session. Background
    /// busy, on a group, or a plain tmux session: say so, return None
    pub(super) fn deck_target(&mut self, what: &str) -> Option<Target> {
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            self.flash(&m);
            return None;
        }
        if self.on_group(what) {
            return None;
        }
        let s = self.current()?;
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能按 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            self.flash(&m);
            return None;
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        if t.id.is_empty() {
            let m = tr!("普通 tmux 会话 · A 接管后才能{what}", "can't {what} an ordinary tmux session · A adopts it");
            self.flash(&m);
            return None;
        }
        Some(t)
    }

    /// s's cell, handed to park.rs
    pub(super) fn pane_of(&self, s: &Session) -> crate::park::Pane {
        let title = clip(&s.title, 16).trim_end().to_string();
        crate::park::Pane {
            id: s.pane.clone(),
            label: s.sub.as_ref().map_or_else(|| title.clone(), |x| tr!("{}「{title}」", "{} “{title}”", x.loc())),
        }
    }

    /// D on a cell that isn't a registered agent (extra windows / panes in a session, a plain tmux session): stops only this cell,
    /// the pane is kept with its last screen (see park.rs). Asks first. Returns false = not handled here (a registered session's
    /// row: it's the agent that stops, via `session::stop`)
    fn ask_pane(&mut self) -> bool {
        if self.on_group_row() {
            return false;
        }
        let Some(s) = self.current() else { return false };
        if s.pane.is_empty() || (s.sub.is_none() && !s.deck_id.is_empty()) {
            return false;
        }
        if let Some(b) = self.doing() {
            self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
            return true;
        }
        if s.state == State::Dead {
            self.flash(t!("已停止 · ⏎ 启动", "already stopped · ⏎ starts it"));
            return true;
        }
        let p = self.pane_of(s);
        let why = match s.state {
            State::Running => t!("正在运行,将被中断", "it's running; this interrupts it"),
            State::Ask => t!("正在等待回应,将被中断", "it's waiting for your choice; this interrupts it"),
            _ => "",
        };
        let t = Target { name: s.family().to_string(), title: p.label.clone(), profile: s.profile.clone(), id: String::new() };
        let row = s.name.clone();
        self.confirm = Some(Confirm::Pane { t, p, row, restart: false, why: why.into(), next: None });
        true
    }

    /// R: running / waiting for your choice asks first (a restart interrupts it), others restart directly
    pub(super) fn ask_restart(&mut self) {
        let why = match self.current().map(|s| s.state) {
            Some(State::Running) => t!("正在运行,将被中断", "it's running; this interrupts it"),
            Some(State::Ask) => t!("正在等待回应,将被中断", "it's waiting for your choice; this interrupts it"),
            _ => "",
        };
        let Some(t) = self.deck_target(t!("重启", "restart")) else { return };
        if why.is_empty() {
            self.job = Some(Job::Restart { t, force: false });
        } else {
            self.confirm = Some(Confirm::Restart { t, why: why.into(), force: false });
        }
    }

    /// D: stop — the agent exits, the pane is kept with its last screen, the registration too; later ⏎ restarts it in place and
    /// the conversation continues. Asks first
    pub(super) fn ask_stop(&mut self) {
        if self.ask_pane() {
            return;
        }
        let Some(t) = self.deck_target(t!("停止", "stop")) else { return };
        let (state, attached) = self.current().map_or((State::Idle, false), |s| (s.state, s.attached));
        if state.gone() || state == State::Dead {
            self.flash(t!("已停止 · ⏎ 启动", "already stopped · ⏎ starts it"));
        } else {
            self.confirm = Some(Confirm::Stop { t, attached });
        }
    }

    /// A: adopt the highlighted plain tmux session (not started by aca) — register it with aca, processes untouched. Identify it
    /// first and show what was found in the confirm box. On an extra window in a plain session: adopt the session it's in, the
    /// agent counted as this cell's
    pub(super) fn ask_adopt(&mut self) {
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("接管", "adopt")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let registered = |name: &str| self.all.iter().any(|x| x.sub.is_none() && x.name == name && !x.deck_id.is_empty());
        if !s.deck_id.is_empty() || registered(s.family()) {
            return self.flash(t!("它已是 aca 会话", "it's already an aca session"));
        }
        if s.state == State::Dead {
            return self.flash(t!("该窗格的程序已退出,无法接管", "the program in that pane has exited; nothing to adopt"));
        }
        let target = if s.sub.is_some() { s.pane.clone() } else { s.tmux.clone() };
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: String::new() };
        match (self.adopt_planner)(&target) {
            Ok(plan) => self.confirm = Some(Confirm::Adopt { t, plan: Box::new(plan) }),
            Err(e) => self.flash(&tr!("无法接管:{e}", "can't adopt: {e}")),
        }
    }

    /// r: change the title in a popup, the old title pre-filled. On a group: edit the group (name, default directory)
    pub(super) fn ask_rename(&mut self) {
        if let Some(g) = self.group_row().map(String::from) {
            return self.ask_group_edit(g);
        }
        let Some(t) = self.deck_target(t!("重命名", "rename")) else { return };
        self.prompt = Some(Prompt { text: t.title.clone(), ask: Ask::Rename(t), hint: String::new() });
    }

    /// r on a group: rename first (old name pre-filled, may include parents), then after Enter the default directory. Only groups
    /// in the registry ("No group" and the plain tmux sessions section aren't)
    fn ask_group_edit(&mut self, g: String) {
        if let Some(b) = self.doing() {
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        if g == "-" {
            return self.flash(t!("这些是普通 tmux 会话,没有可修改的分组", "these are ordinary tmux sessions; no group to edit"));
        }
        if !(self.group_hints)().groups.iter().any(|(x, _)| *x == g) {
            return self.flash(&tr!("「{g}」不是已登记的分组:用 m 移到分组", "“{g}” isn't a registered group: use m to put it in a group"));
        }
        self.prompt = Some(Prompt { text: g.clone(), ask: Ask::GroupName(g), hint: String::new() });
    }

    /// The group's new name is in: if it doesn't clash with another group, go on to the default directory (old one pre-filled)
    fn group_name_given(&mut self, group: String, text: String) {
        let hints = (self.group_hints)().groups;
        let to = match ops::clean_group(&text) {
            None => Err(t!("分组名不能为空", "the group name is empty").to_string()),
            Some(to) if to.strip_prefix(&group).is_some_and(|r| r.starts_with('/')) => {
                Err(tr!("{group} 不能移到自身下面", "{group} can't move under itself"))
            }
            Some(to) => match hints.iter().find(|(x, _)| x.eq_ignore_ascii_case(&to) && *x != group) {
                Some((x, _)) => Err(tr!("分组 {x} 已存在", "group {x} already exists")),
                None => Ok(to),
            },
        };
        match to {
            Err(hint) => self.prompt = Some(Prompt { ask: Ask::GroupName(group), text, hint }),
            Ok(to) => {
                let was = hints.iter().find(|(x, _)| *x == group).map(|(_, d)| d.clone()).unwrap_or_default();
                let text = words::tilde(&was);
                self.prompt = Some(Prompt { ask: Ask::GroupHome { group, to, was }, text, hint: String::new() });
            }
        }
    }

    /// The default directory is in too: empty = not recorded; otherwise it must be an existing directory and not another group's
    /// default. Neither name nor directory changed = nothing done
    fn group_home_given(&mut self, group: String, to: String, was: String, text: String) {
        let real = aca_core::paths::real;
        let dir = match text.trim() {
            "" => Ok(String::new()),
            // Unchanged: no more checks (lets you just rename even if the old directory is gone)
            t if !was.is_empty() && real(&create::expand(t)) == real(&was) => Ok(was.clone()),
            t => {
                let abs = create::expand(t);
                let taken = (self.group_hints)().groups.into_iter().find(|(x, d)| *x != group && !d.is_empty() && real(d) == real(&abs));
                match taken {
                    _ if !std::path::Path::new(&abs).is_dir() => Err(tr!("{} 不是目录", "{} is not a directory", words::tilde(&abs))),
                    Some((x, _)) => Err(tr!("该目录已是分组 {x} 的默认目录", "this directory is already the default of group {x}")),
                    None => Ok(real(&abs)),
                }
            }
        };
        let dir = match dir {
            Ok(d) => d,
            Err(hint) => return self.prompt = Some(Prompt { ask: Ask::GroupHome { group, to, was }, text, hint }),
        };
        let changed = dir != was;
        if to == group && !changed {
            return;
        }
        let dir = changed.then_some((was, dir));
        self.job = Some(Job::Group(groups::Act::Edit { profile: crate::data::REGISTERED.into(), group, to, dir }));
    }

    /// A group was renamed (an undo renaming it back counts): whatever the interface remembers of it — the group shown, the group
    /// heading the highlight is on, workspace records — follows
    pub(super) fn group_renamed(&mut self, from: &str, to: &str) {
        let r = |g: &str| groups::renamed(g, from, to);
        if let Some(g) = self.group.as_deref().and_then(r) {
            self.group = Some(g);
        }
        if let Some(g) = self.head.as_deref().and_then(r) {
            self.head = Some(g);
        }
        if let Some((Some(g), _)) = self.hop_back.as_mut()
            && let Some(to) = r(g)
        {
            *g = to;
        }
        if let Some(Some(g)) = self.rail_from.as_mut()
            && let Some(to) = r(g)
        {
            *g = to;
        }
        self.spot = None;
        // The workspace file's records were already changed in the background (`groups::rename_all`)
        self.spaces.reload();
    }

    /// m: pick a group in fzf to move into. Each row shows that group's
    /// directory, groups with the same directory first (m ⏎ puts it back home)
    pub(super) fn ask_move(&mut self) {
        let Some(t) = self.deck_target(t!("移到分组", "move")) else { return };
        let (from, here) = self.current().map(|s| (s.group.clone(), s.dir.clone())).unwrap_or_default();
        self.pick_group = Some(self.group_pick(t, from, here, vec![]));
    }

    /// Groups m's fzf lists: if all moved ones are in one group, not that one; `here` is their shared directory (different = empty),
    /// groups with that directory first
    pub(super) fn group_pick(&self, t: Target, from: String, here: String, more: Vec<(Target, String)>) -> GroupPick {
        let hints = (self.group_hints)().groups;
        let mut groups: Vec<String> = hints.iter().map(|(g, _)| g.clone()).collect();
        groups.sort();
        groups.dedup();
        // First in the list's section order, then those not in the list (empty)
        let mut cands: Vec<String> = self.groups().into_iter().filter(|g| groups.contains(g)).collect();
        let rest: Vec<String> = groups.iter().filter(|g| !cands.contains(g)).cloned().collect();
        cands.extend(rest);
        let one = more.iter().all(|(_, g)| *g == from);
        if one {
            cands.retain(|g| *g != from);
        }
        let real = aca_core::paths::real;
        let mine = real(&here);
        let works_in = |group: &str| {
            let home = hints.iter().find(|(g, d)| g == group && !d.is_empty()).map(|(_, d)| d.clone());
            home.unwrap_or_else(|| self.usual_dir(group))
        };
        let mut rows: Vec<ops::GroupRow> = cands
            .into_iter()
            .map(|group| {
                let dir = works_in(&group);
                let same = !dir.is_empty() && !here.is_empty() && real(&dir) == mine;
                ops::GroupRow { group, dir, same }
            })
            .collect();
        // Stable sort: the rest keep their order
        rows.sort_by_key(|r| !r.same);
        let dirs = groups.iter().map(|g| (g.clone(), works_in(g))).collect();
        let under = if one && groups.contains(&from) { from.clone() } else { String::new() };
        GroupPick { t, from, here, rows, groups, more, under, dirs }
    }

    /// The directory with the most sessions in the group (on a tie, the first in the list); no sessions in the group = empty.
    /// `m` shows this when a group has no default directory recorded
    fn usual_dir(&self, group: &str) -> String {
        let mut seen: Vec<(&str, usize)> = vec![];
        for s in self.all.iter().filter(|s| s.group == group && !s.dir.is_empty()) {
            match seen.iter_mut().find(|(d, _)| *d == s.dir) {
                Some((_, n)) => *n += 1,
                None => seen.push((&s.dir, 1)),
            }
        }
        // max_by_key takes the later one on a tie, so search reversed
        seen.iter().rev().max_by_key(|(_, n)| *n).map(|(d, _)| d.to_string()).unwrap_or_default()
    }

    /// f: fork a new session carrying the conversation, naming it first (the fork is identical to the original, without a name
    /// they can't be told apart): pre-filled with the default "<title> (fork)", ⏎ just uses it
    pub(super) fn ask_fork(&mut self) {
        let Some(t) = self.deck_target(t!("分叉", "fork")) else { return };
        if !self.current().is_some_and(list::can_fork) {
            return self.flash(t!("该工具不支持分叉", "this agent can't fork"));
        }
        self.prompt = Some(Prompt { text: fork_title(&t.title), ask: Ask::Fork(t), hint: String::new() });
    }

    /// X: switch account and carry on — the box shows its current account and quota and the accounts it can switch to
    /// (logged in or not, quota); ⏎ switches directly (see `rehome`)
    pub(super) fn ask_rehome(&mut self) {
        self.digits.clear();
        let Some(t) = self.deck_target(t!("换账号", "switch accounts of")) else { return };
        let Some(s) = self.current().cloned() else { return };
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只有 Claude / Codex 会话可以切换账号", "only Claude / Codex sessions can switch accounts"));
        }
        let tools = match (self.tools_loader)() {
            Ok(v) => v,
            Err(e) => return self.flash(&e),
        };
        let env = crate::rehome::Env { ask: self.rehome_ask, background: self.rehome_background, accounts: self.accounts_page };
        let now = epoch() as i64;
        let menu = crate::rehome::Menu::new(t, &s, tools, &(self.rehome_quotas)(s.agent), (now, aca_records::usage::local_offset()), env);
        self.open(Box::new(menu));
    }

    /// H: hand off to a new session — first pick the new session's tool / account (`n`'s menu, without shell), create it in the
    /// same directory and group, send the handoff note, and have the new session read its conversation and carry on
    /// (see `handoff`). Its note goes along too
    pub(super) fn ask_handoff(&mut self) {
        self.digits.clear();
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("交接", "hand off")) {
            return;
        }
        let Some(s) = self.current().cloned() else { return };
        if let Some(x) = &s.sub {
            let m = tr!("{} 是窗口,只能按 ⏎ / d / D", "{} is a window: only ⏎ / d / D", x.loc());
            return self.flash(&m);
        }
        if !matches!(s.agent, Agent::Claude | Agent::Codex) {
            return self.flash(t!("只能交接 Claude / Codex 会话", "only Claude / Codex sessions can be handed off"));
        }
        let mut from = match (self.source_loader)(&s) {
            Ok(f) => f,
            Err(e) => return self.flash(&e),
        };
        let key = notes::key(&s);
        from.note = self.notes.get(&key).map(String::from);
        let draft = Draft::from(Some(&s), self.group_hints);
        let menu: Vec<Tool> = match (self.tools_loader)() {
            Ok(v) => v.into_iter().filter(|t| t.agent != Agent::None).collect(),
            Err(e) => return self.flash(&e),
        };
        if menu.is_empty() {
            return self.flash(t!("工具菜单中没有可接手的工具(只有 shell)", "nothing in the tool menu can take over (only shell)"));
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        let note = from.note.is_some().then_some(key);
        let busy = matches!(s.state, State::Running | State::Ask);
        self.handoff = Some(Pass { t, from: Box::new(from), draft, note, busy });
        self.load_tool_quotas();
        self.show_tools(menu);
    }

    /// Back from picking a group in fzf: the selected one or a newly typed name. A new group gets the directory under its
    /// parent's (see `ops::typed_group`)
    pub fn group_picked(&mut self, p: GroupPick, got: Result<Option<String>, String>) {
        match got {
            Ok(Some(g)) => match ops::typed_group(&g, &p.under, &p.dirs) {
                Some(ops::Typed::Existing(to)) => self.move_picked(p, to, String::new()),
                Some(ops::Typed::New { group, dir }) => self.move_picked(p, group, dir),
                None => {}
            },
            Ok(None) => {}
            Err(e) => self.flash(&e),
        }
    }

    /// Tab on a new name in `m`'s list: ask the directory the new group works in first (the usual one filled in); ⏎ there
    /// creates it and moves
    pub fn ask_move_home(&mut self, p: GroupPick, name: &str) {
        match ops::typed_group(name, &p.under, &p.dirs) {
            Some(ops::Typed::New { group, dir }) => {
                let text = words::tilde(&dir);
                self.prompt = Some(Prompt { ask: Ask::MoveHome { pick: Box::new(p), to: group }, text, hint: String::new() });
            }
            // fzf only ends this way on a new name
            _ => self.group_picked(p, Ok(Some(name.to_string()))),
        }
    }

    /// The new group's directory is in: empty = none recorded; otherwise it mustn't be a file or another group's default (one
    /// that isn't there yet is made when the group is)
    fn move_home_given(&mut self, pick: Box<GroupPick>, to: String, text: String) {
        let real = aca_core::paths::real;
        let dir = match text.trim() {
            "" => Ok(String::new()),
            t => {
                let abs = create::expand(t);
                let path = std::path::Path::new(&abs);
                let taken = (self.group_hints)().groups.into_iter().find(|(_, d)| !d.is_empty() && real(d) == real(&abs));
                match taken {
                    _ if path.exists() && !path.is_dir() => Err(tr!("{} 不是目录", "{} is not a directory", words::tilde(&abs))),
                    Some((x, _)) => Err(tr!("该目录已是分组 {x} 的默认目录", "this directory is already the default of group {x}")),
                    None => Ok(abs),
                }
            }
        };
        match dir {
            Ok(dir) => self.move_picked(*pick, to, dir),
            Err(hint) => self.prompt = Some(Prompt { ask: Ask::MoveHome { pick, to }, text, hint }),
        }
    }

    /// Move what `m` picked into group `to`; `home`: the directory it works in if it gets created here (empty = none)
    fn move_picked(&mut self, p: GroupPick, to: String, home: String) {
        if p.more.is_empty() {
            if to.eq_ignore_ascii_case(&p.from) {
                return self.flash(&tr!("已在 {to} 中", "already in {to}"));
            }
            self.stay_on_move(&p.t.name, &to);
            self.job = Some(Job::Move { t: p.t, from: p.from, to, groups: p.groups, home });
            return;
        }
        // Multi-selected ones move together: ones already in that group stay
        let moves: Vec<(Target, String)> =
            std::iter::once((p.t, p.from)).chain(p.more).filter(|(_, from)| !to.eq_ignore_ascii_case(from)).collect();
        if moves.is_empty() {
            return self.flash(&tr!("均已在 {to} 中", "all already in {to}"));
        }
        for (t, _) in &moves {
            self.stay_on_move(&t.name, &to);
        }
        let jobs = moves
            .into_iter()
            .map(|(t, from)| Job::Move { t, from, to: to.clone(), groups: p.groups.clone(), home: home.clone() })
            .collect();
        self.drop_picks();
        self.job = Some(Job::Many(jobs));
    }

    /// e: write a note, the existing one pre-filled
    pub(super) fn ask_note(&mut self) {
        let Some((key, title)) = self.note_target() else { return };
        let text = self.note_text(&key).unwrap_or_default().to_string();
        // A note started on the row of the group being viewed: the middle column turns to its notes page, where the new one
        // shows as it's written
        if let Some(g) = notes::group_of(&key).filter(|g| self.group.as_deref() == Some(*g)) {
            self.show_page(g, Page::Notes);
        }
        self.prompt = Some(Prompt { ask: Ask::Note { key, title, orig: text.clone() }, text, hint: String::new() });
    }

    /// Whose note e / ^G writes: (key, what it's called). On a group's row a new one in the group's notebook; on its notes page
    /// the one under the cursor; otherwise the highlighted session's
    fn note_target(&self) -> Option<(String, String)> {
        if self.in_notes() {
            return self.note_cur().map(|n| (notes::note_key(&n.id), n.name().to_string()));
        }
        match self.group_row() {
            Some(g) => Some((group_key(g), self.shown(g).into_owned())),
            None => self.current().map(|s| (notes::key(s), s.title.clone())),
        }
    }

    /// The note under this key: a session's, or one in a group's notebook
    pub(super) fn note_text(&self, key: &str) -> Option<&str> {
        if notes::in_book(key) { self.book.text(key) } else { self.notes.get(key) }
    }

    /// Write the note under this key (blank = delete it), wherever it's kept. A notebook's is settled at once: this isn't typing
    pub(super) fn put_note(&mut self, key: &str, text: &str) -> Result<(), String> {
        match notes::in_book(key) {
            true => self.book.save(key, text).map(|()| drop(self.book.done(key))),
            false => self.notes.set(key, text).map_err(|e| e.to_string()),
        }
    }

    /// E / Shift+Enter: go in with the note — pasted into its input box before going in (not sent), deleted once filled. No note = Enter.
    /// Only fills Claude / Codex (other programs, like vim, would take the pasted text as commands); not while waiting for your choice (!)
    /// — the text would be taken as an option. If it can't fill, say why and don't go in (so you don't think it filled)
    pub(super) fn enter_with_note(&mut self) {
        if self.on_group_row() {
            return self.enter();
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.enter() };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("未附带便签:{w}", "didn't go in with the draft: {w}"));
        }
        self.fill = Some((key, text));
        self.enter();
    }

    /// s: reply to it without going in. A box to write in, where `e` writes (its note, if any, already in it: the note is the next
    /// thing to say to it); ⏎ sends it (pasted into its input box, then Enter), you stay in the list. Where nothing could be sent
    /// the box doesn't open: say why, and what to press instead. A window / split pane opened inside a session (an agent team's
    /// teammate…) goes like an unregistered session: its own row (`name` is the session's key plus its pane id), its own note,
    /// and what's written goes into that pane, never the session's own (`deck_id` is empty, see `Job::Send`).
    /// On a group's row the one replied to is the group's router, by the same rules; on the rail's row for all groups, the desk
    pub(super) fn ask_reply(&mut self) {
        self.digits.clear();
        let no_desk = t!("还没有总台 · o 新建", "no desk yet · o starts one");
        if let Some(g) = self.route_group().map(String::from) {
            return match self.router_at(&g) {
                Some(i) => self.reply_to(i),
                None if self.is_scratch(&g) => self.flash(no_desk),
                None if router::can_route(&g) => self.flash(t!("还没有 router · o 新建", "no router yet · o starts one")),
                None => drop(self.on_group(t!("回话", "reply"))),
            };
        }
        if self.on_all_row() {
            return self.flash(no_desk);
        }
        if let Some(i) = self.list.selected().and_then(|p| self.view.get(p).copied()) {
            self.reply_to(i);
        }
    }

    /// The key that starts the session `s` would reach, where the cursor is: on a group's row (its router) ⏎ goes into the
    /// group, and `o` is what opens the router; so it is on the rail's row for all groups (the desk)
    fn start_key(&self) -> &'static str {
        if self.on_route_row() { "o" } else { "⏎" }
    }

    /// `s` for the session at `all[i]`: the highlighted one, the router of the group the cursor is on, or the one doing the
    /// ticket under the cursor
    pub(super) fn reply_to(&mut self, i: usize) {
        let s = &self.all[i];
        let why = match () {
            _ if list::wakes_on_send(s) => None,
            _ if starts(s) => Some(not_running(s, self.start_key())),
            _ if s.state == State::Ask => return self.ask_answer(i),
            _ => cant_fill(s).map(|w| tr!("无法回话:{w}", "can't reply: {w}")),
        };
        if let Some(m) = why {
            return self.flash(&m);
        }
        let key = notes::key(s);
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        let pane = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        let ask = Ask::Reply { key, title: s.title.clone(), orig: text.clone(), t: target_of(s), pane };
        self.prompt = Some(Prompt { ask, text, hint: String::new() });
        // The bottom bar now belongs to the box: only what's said while it's open shows there
        self.msg = None;
    }

    /// s on a row waiting for you: a box with its question and the keys that answer it here (see `answer::offer`); only ⏎ to go
    /// in when it can't be answered from the list
    fn ask_answer(&mut self, i: usize) {
        let s = &self.all[i];
        let (shown, offer) = match &s.asked {
            Some(a) => (Some(a.what.clone()), a.offer.clone()),
            None => (None, answer::Offer::Nothing),
        };
        let question = shown.as_ref().map(|a| a.ask.clone()).filter(|q| !q.is_empty()).unwrap_or_else(|| s.question.clone());
        self.confirm = Some(Confirm::Answer { t: target_of(s), question, shown, offer });
    }

    /// Whether the answer box still shows what its row asks, by the latest list: still waiting, the same question. Found by `name`,
    /// which is that row's alone (a window's carries its pane id)
    fn still_asks(&self, t: &Target, question: &str, shown: Option<&answer::Asking>) -> bool {
        self.all.iter().find(|s| s.name == t.name).is_some_and(|s| {
            s.state == State::Ask
                && match shown {
                    Some(a) => s.asked.as_ref().is_some_and(|x| x.what == *a),
                    None => s.asked.is_none() && s.question == question,
                }
        })
    }

    /// After each refresh: an answer box whose session no longer asks what it shows closes
    pub(super) fn recheck_answer(&mut self) {
        if let Some(Confirm::Answer { t, question, shown, .. }) = &self.confirm
            && !self.still_asks(t, question, shown.as_ref())
        {
            self.confirm = None;
            self.flash(&ops::changed_msg());
        }
    }

    /// A key in the answer box: the numbers / y / n it lists answer (in the background, see `Job::Answer`); ⏎ goes in, as on the
    /// list; Esc / q / s close it; any other key closes it too, answering nothing. No highlighted row: ⏎ never picks for you
    pub(super) fn on_answer_key(&mut self, k: KeyEvent) {
        let Some(Confirm::Answer { t, question, shown, offer }) = self.confirm.take() else { return };
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let reply = match (k.code, &offer) {
            (KeyCode::Char(c @ '1'..='9'), answer::Offer::Number(n)) if plain && (c as usize - '0' as usize) <= *n => c.to_string(),
            (KeyCode::Char('y'), answer::Offer::YesNo) if plain => "yes".into(),
            (KeyCode::Char('n'), answer::Offer::YesNo) if plain => "no".into(),
            // On a group's row the box is its router's: ⏎ there would go into the group. The same on the rail's row for all
            // groups, where it is the desk's
            (KeyCode::Enter, _) if self.on_route_row() => return self.open_router(),
            (KeyCode::Enter, _) => return self.enter(),
            (KeyCode::Esc | KeyCode::Char('q' | 's'), _) => return,
            _ => return self.flash(t!("未回答", "not answered")),
        };
        if let Some(b) = self.doing() {
            self.confirm = Some(Confirm::Answer { t, question, shown, offer });
            return self.flash(&tr!("请稍候:{b}", "still busy: {b}"));
        }
        // Once more by the latest list, right before: a refresh may have come in since the box was drawn
        match shown {
            Some(shown) if self.still_asks(&t, &question, Some(&shown)) => self.job = Some(Job::Answer { t, reply, shown }),
            _ => self.flash(&ops::changed_msg()),
        }
    }

    /// ⏎ in the reply box: send what's written. Empty sends nothing and the box stays; so it does while other work runs. Its
    /// state is looked at again first (the list refreshes while you type: it may have started asking); not sendable now = the
    /// text stays as its note, the bottom bar says why
    fn send_reply(&mut self) {
        let Some(text) = self.prompt.as_ref().map(|p| p.text.trim_end().to_string()) else { return };
        if text.trim().is_empty() {
            return;
        }
        if let Some(b) = self.doing()
            && let Some(p) = self.prompt.as_mut()
        {
            p.hint = tr!("请稍候:{b}", "still busy: {b}");
            return;
        }
        let Some(Prompt { ask: Ask::Reply { key, title, orig, t, pane }, .. }) = self.prompt.take() else { return };
        let why = match self.all.iter().find(|s| s.name == t.name) {
            None => Some(t!("未发送:会话可能刚结束", "not sent: the session may have just ended").to_string()),
            Some(s) => unsendable(s, self.start_key()),
        };
        if let Some(m) = why {
            self.note_done(key, title, orig);
            return self.flash(&m);
        }
        self.job = Some(Job::Send { t, pane, text, key });
    }

    /// ^G (in the list): write the highlighted session's / group's note in the editor
    pub(super) fn note_in_editor(&mut self) {
        let Some((key, title)) = self.note_target() else { return };
        let text = self.note_text(&key).unwrap_or_default().to_string();
        self.edit_note = Some(NoteEdit { key, title, orig: text.clone(), text, from_prompt: false, to: None });
    }

    /// Back from the editor: save it; if it came from bottom bar editing, go back there to keep writing
    pub fn note_editor_done(&mut self, ed: NoteEdit, got: Result<String, String>) {
        let (text, err) = match got {
            Ok(t) => (t, None),
            Err(e) => (ed.text.clone(), Some(e)),
        };
        if err.is_none() {
            self.save_note(&ed.key, &text);
        }
        if ed.from_prompt {
            let hint = err.unwrap_or_default();
            let ask = match ed.to {
                Some((t, pane)) => Ask::Reply { key: ed.key, title: ed.title, orig: ed.orig, t, pane },
                None => Ask::Note { key: ed.key, title: ed.title, orig: ed.orig },
            };
            self.prompt = Some(Prompt { ask, text, hint });
        } else if let Some(e) = err {
            self.flash(&e);
        } else {
            let (book, had) = (notes::in_book(&ed.key), !ed.orig.is_empty());
            self.note_done(ed.key, ed.title, ed.orig);
            self.note_saved_flash(book, &text, had);
        }
    }

    /// Save the note whenever it changes (every keystroke: when a phone disconnects aca is killed outright, it never gets to exit)
    pub(super) fn save_note(&mut self, key: &str, text: &str) {
        let saved = match notes::in_book(key) {
            true => self.book.save(key, text),
            false => self.notes.set(key, text).map_err(|e| e.to_string()),
        };
        if let Err(e) = saved {
            self.flash(&tr!("保存失败:{e}", "not saved: {e}"));
        }
        // One in a notebook: the notes page's cursor is on what's being written
        if let Some(id) = self.book.id_of(key).filter(|_| notes::in_book(key)) {
            self.note_at = Some(id.to_string());
        }
        self.apply_pins();
    }

    /// The note is done: if it differs from before writing started, record an undo step
    fn note_done(&mut self, key: String, title: String, orig: String) {
        // In a notebook the writing ends here: one left blank goes, one started just now goes by its own key from here on
        let key = match notes::in_book(&key) {
            true => self.book.done(&key).unwrap_or(key),
            false => key,
        };
        if self.note_text(&key).unwrap_or_default() != orig {
            self.remember(Undo::Note { key, title, text: orig });
        }
        self.apply_pins();
    }

    /// What the bottom bar says once a note is done. `book`: it's one in a group's notebook; left empty, that one is deleted if
    /// it was there before (`had`), and was never written if it wasn't: nothing to say
    fn note_saved_flash(&mut self, book: bool, text: &str, had: bool) {
        self.flash(match (book, text.trim().is_empty()) {
            (true, true) if !had => return,
            (true, true) => t!("笔记已删除 · ^Z 撤销", "note deleted · ^Z undoes"),
            (true, false) => t!("笔记已保存", "note saved"),
            (false, true) => t!("便签已清空", "draft cleared"),
            (false, false) => t!("便签已保存", "draft saved"),
        });
    }

    /// The note was filled in: delete it
    pub fn note_filled(&mut self, key: &str) {
        let _ = self.notes.take(key);
        self.apply_pins();
    }

    /// ^N: name the new group first, after Enter go straight into fzf to pick a directory (first row is the default directory);
    /// the new session opens in this group
    pub(super) fn ask_new_group(&mut self) {
        self.prompt = Some(Prompt { ask: Ask::NewGroup, text: String::new(), hint: String::new() });
    }

    /// The new group named with ^N: created in the background (when done the highlight goes to its heading, see `job_done`).
    /// If not created in 1, the group is recorded in this workspace (it shows here in the list, and new ones in it later land here)
    fn add_group(&mut self, g: &str) {
        if self.space != spaces::HOME {
            let keep = self.keep_marks();
            let _ = self.spaces.assign(&[], &[((crate::data::REGISTERED.into(), g.to_string()), Some(self.space))], &keep);
        }
        self.job = Some(Job::Group(groups::Act::Create { group: g.to_string() }));
    }

    pub(super) fn on_prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = self.prompt.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let to = match &p.ask {
            Ask::Reply { t, pane, .. } => Some((t.clone(), pane.clone())),
            _ => None,
        };
        if let Ask::Note { key, title, orig } | Ask::Reply { key, title, orig, .. } = &p.ask {
            // Note: Enter is a newline, Esc leaves (already saved long ago), ^G hands it to the editor.
            // Reply: the same, except Enter sends (as in the box it goes into: Shift+Enter / ^J is the newline); keys arriving
            // together in a burst are a paste, where an Enter is a line break, not a send
            let reply = to.is_some();
            match k.code {
                KeyCode::Esc => {
                    let (key, title, orig) = (key.clone(), title.clone(), orig.clone());
                    let text = p.text.trim_end().to_string();
                    self.prompt = None;
                    self.save_note(&key, &text);
                    let book = notes::in_book(&key);
                    self.note_done(key, title, orig.clone());
                    // Nothing written, nothing there before: nothing to say
                    if reply && text.trim().is_empty() && orig.is_empty() {
                        return;
                    }
                    return self.note_saved_flash(book, &text, !orig.is_empty());
                }
                KeyCode::Char('g') if ctrl => {
                    let ed = NoteEdit {
                        key: key.clone(),
                        title: title.clone(),
                        orig: orig.clone(),
                        text: p.text.clone(),
                        from_prompt: true,
                        to,
                    };
                    self.prompt = None;
                    self.edit_note = Some(ed);
                    return;
                }
                KeyCode::Enter if reply && !self.burst && !k.modifiers.contains(KeyModifiers::SHIFT) => return self.send_reply(),
                KeyCode::Enter => p.text.push('\n'),
                KeyCode::Char('j') if ctrl => p.text.push('\n'), // a pasted newline
                KeyCode::Backspace => {
                    p.text.pop();
                }
                KeyCode::Char('u') if ctrl => p.text.clear(),
                KeyCode::Char('w') if ctrl => {
                    let t = p.text.trim_end_matches([' ', '\n']);
                    let cut = t.rfind([' ', '\n']).map_or(0, |i| i + 1);
                    p.text.truncate(cut);
                }
                KeyCode::Char(c) if !ctrl => p.text.push(c),
                _ => return,
            }
            p.hint.clear();
            let (key, text) = (key.clone(), p.text.clone());
            return self.save_note(&key, &text);
        }
        match k.code {
            KeyCode::Enter => return self.finish_prompt(),
            KeyCode::Tab if matches!(p.ask, Ask::GroupDir | Ask::ScratchDir { .. } | Ask::GroupHome { .. } | Ask::MoveHome { .. }) => {
                match create::complete_path(&p.text) {
                    Some(t) => p.text = t,
                    None => return,
                }
            }
            KeyCode::Esc => {
                self.prompt = None;
                if self.settings_back.is_some() {
                    return self.back_to_settings();
                }
                return;
            }
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Char('u') if ctrl => p.text.clear(),
            // Delete the last word (along with the space / the group's `/` next to it)
            KeyCode::Char('w') if ctrl => {
                let t = p.text.trim_end_matches([' ', '/']);
                let cut = t.rfind([' ', '/']).map_or(0, |i| i + 1);
                p.text.truncate(cut);
            }
            KeyCode::Char(c) if !ctrl => p.text.push(c),
            _ => return,
        }
        p.hint.clear();
    }

    fn finish_prompt(&mut self) {
        let Some(Prompt { ask, text, .. }) = self.prompt.take() else { return };
        match ask {
            Ask::Rename(t) => {
                let to = text.trim().to_string();
                if to.is_empty() {
                    self.prompt = Some(Prompt { ask: Ask::Rename(t), text, hint: t!("标题不能为空", "the title is empty").into() });
                } else if to != t.title {
                    self.job = Some(Job::Rename { t, to, unlock: false });
                }
            }
            Ask::Fork(t) => {
                let to = text.trim().to_string();
                if to.is_empty() {
                    self.prompt = Some(Prompt { ask: Ask::Fork(t), text, hint: t!("标题不能为空", "the title is empty").into() });
                } else {
                    // Unchanged: use the default (not locked, Claude can still change it later); changed: lock it
                    let title = (to != fork_title(&t.title)).then_some(to);
                    self.job = Some(Job::Fork { t, title });
                }
            }
            // Enter in a note is a newline, in a reply it sends: neither gets here
            Ask::Note { .. } | Ask::Reply { .. } => {}
            Ask::GroupDir => {
                let dir = text.trim().to_string();
                let r = if dir.is_empty() {
                    Err(t!("目录不能为空", "the directory is empty").to_string())
                } else {
                    (self.config_set)("new_group", "default_path", toml::Value::String(dir))
                };
                match r {
                    Ok(()) => self.back_to_settings(),
                    Err(hint) => self.prompt = Some(Prompt { ask: Ask::GroupDir, text, hint }),
                }
            }
            // Empty = no scratch group any more (the group that was it stays, as an ordinary one). Otherwise the group that
            // works there is made sure of first: a directory that can't be made isn't written down
            Ask::ScratchDir { desk } => {
                let dir = text.trim().to_string();
                let at = create::expand(&dir);
                let r = match dir.as_str() {
                    "" => Ok(None),
                    _ if std::path::Path::new(&at).is_file() => Err(tr!("{} 不是目录", "{} is not a directory", words::tilde(&at))),
                    _ => (self.scratch_ensure)(&at).map(Some),
                };
                match r.and_then(|g| (self.config_set)("scratch", "dir", toml::Value::String(dir)).map(|()| g)) {
                    Ok(group) => {
                        self.refresh = true;
                        self.back_to_settings();
                        // Asked for on the way to a desk: on to its new-session box, unless the group that works there
                        // has a router already — then that one is the desk
                        if let Some(g) = group.filter(|g| desk && self.router_at(g).is_none()) {
                            self.new_draft = Some(self.desk_draft(&g, &at));
                            self.scratch = Some(g);
                        }
                    }
                    Err(hint) => self.prompt = Some(Prompt { ask: Ask::ScratchDir { desk }, text, hint }),
                }
            }
            Ask::GroupName(group) => self.group_name_given(group, text),
            Ask::GroupHome { group, to, was } => self.group_home_given(group, to, was, text),
            Ask::MoveHome { pick, to } => self.move_home_given(pick, to, text),
            Ask::TaskTitle { n, group } => {
                let title = text.trim().to_string();
                if title.is_empty() {
                    let hint = t!("名字不能为空", "the title is empty").into();
                    self.prompt = Some(Prompt { ask: Ask::TaskTitle { n, group }, text, hint });
                } else {
                    // What counts as done, the ticket's own already in the box when it's being rewritten
                    let text = n.and_then(|n| self.tickets.get(n)).map(|t| t.criteria.clone()).unwrap_or_default();
                    self.prompt = Some(Prompt { ask: Ask::TaskCriteria { n, group, title }, text, hint: String::new() });
                }
            }
            Ask::TaskCriteria { n, group, title } => {
                let criteria = text.trim().to_string();
                self.job = Some(Job::Task(match n {
                    Some(n) => TaskAct::Set { n, title, criteria },
                    None => TaskAct::Add { group, title, criteria },
                }));
            }
            Ask::Space(c) => {
                let m = match self.spaces.rename(c, &text) {
                    Ok(()) => tr!("工作区 {c}:{}", "workspace {c}: {}", self.spaces.name(c)),
                    Err(e) => tr!("保存失败:{e}", "not saved: {e}"),
                };
                self.flash(&m);
            }
            Ask::NewGroup => {
                let Some(mut name) = ops::clean_group(&text) else {
                    self.prompt =
                        Some(Prompt { ask: Ask::NewGroup, text, hint: t!("分组名不能为空", "the group name is empty").into() });
                    return;
                };
                match self.draft() {
                    Ok(mut d) => {
                        match d.hints.groups.iter().find(|(g, _)| g.eq_ignore_ascii_case(&name)) {
                            // Actually exists already (different case): use the existing one
                            Some((g, _)) => name = g.clone(),
                            // New: create it now, the group stays even if Esc is pressed midway
                            None => self.add_group(&name),
                        }
                        d.start_group(name, &(self.new_group_dir)());
                        self.new_draft = Some(d);
                        self.pick = true; // Esc out of it lands in the input box with the default directory filled
                    }
                    Err(e) => self.flash(&e),
                }
            }
        }
    }
}

/// Confirm for delete / restart: delete uses a red box (the state red). First line asks what, then which session, what to watch out for
/// (attached elsewhere, worktree directory), last the y button; any other key cancels
pub(super) fn draw_confirm(f: &mut Frame, c: &Confirm, all: &[Session]) {
    let danger = theme::pal().red;
    let red = Style::new().fg(danger);
    let warn = theme::warn();
    // A group is named as the list names it (its rows, an empty group's placeholder included, know how)
    let group = match c {
        Confirm::Group(g) => group_shown(&g.group, all.iter().find(|s| s.group == g.group).map_or(0, |s| s.scratch)).into_owned(),
        _ => String::new(),
    };
    let (name, title, border, head, what, notes, act): (&str, &str, Color, &str, String, Vec<String>, &str) = match c {
        Confirm::Remove(r) if r.pane.is_some() => {
            let split = r.pane.as_ref().is_some_and(|p| p.1);
            let head = if r.next.is_some() {
                t!("关闭此窗格？", "Close this pane?")
            } else if split {
                t!("关闭窗格？", "Close the pane?")
            } else {
                t!("关闭窗口？", "Close the window?")
            };
            let notes = r.next.iter().map(|n| tr!("其余依次前移，{n}成为首个", "the rest move up, {n} becomes first")).collect();
            (r.name.as_str(), r.title.as_str(), danger, head, String::new(), notes, t!("关闭", "close"))
        }
        Confirm::Remove(r) => {
            // Say the result first: registered ones go to the Trash, recoverable; plain tmux sessions are really closed
            let mut notes = vec![if r.id.is_empty() {
                t!("普通 tmux 会话:将直接关闭,无法撤销", "a plain tmux session: closed for good, no undo").to_string()
            } else {
                t!("移入回收站，保留 1 天 · b 恢复", "goes to the trash; b brings it back within a day").to_string()
            }];
            if r.worktree {
                notes.push(t!("同时删除 worktree 目录", "the worktree directory is deleted too").to_string());
            }
            if r.others > 0 {
                notes.push(tr!("同时关闭其中的 {} 个窗口 / 窗格", "{} windows / panes opened inside close too", r.others));
            }
            if r.attached {
                notes.push(t!("⇄ 在别处打开，将被断开", "⇄ open elsewhere; it will be disconnected").to_string());
            }
            if r.kids > 0 {
                notes.push(tr!("{} 个子会话不会删除", "{} child sessions won't be deleted with it", r.kids));
            }
            (r.name.as_str(), r.title.as_str(), danger, t!("删除会话？", "Delete the session?"), String::new(), notes, t!("删除", "delete"))
        }
        Confirm::Group(g) => {
            let mut notes = vec![];
            if g.sessions > 0 {
                let mut also = vec![];
                if g.busy > 0 {
                    also.push(tr!("{} 个运行中", "{} running", g.busy));
                }
                if g.archived > 0 {
                    also.push(tr!("{} 个已归档", "{} archived", g.archived));
                }
                let also = if also.is_empty() { String::new() } else { tr!("（{}）", " ({})", also.join(t!("，", ", "))) };
                notes.push(tr!("同时删除其中 {} 个会话{also}", "its sessions are deleted too: {}{also}", g.sessions));
            }
            if g.kids > 0 {
                notes.push(tr!("同时删除 {} 个子分组", "subgroups deleted with it: {}", g.kids));
            }
            notes.push(
                t!("移入回收站，保留 1 天 · b 恢复整组", "goes to the trash; b brings the whole group back within a day").to_string(),
            );
            (
                g.group.as_str(),
                group.as_str(),
                danger,
                t!("删除分组？", "Delete the group?"),
                String::new(),
                notes,
                t!("删除", "delete"),
            )
        }
        Confirm::Restart { t, why, force } => {
            let border = if *force { danger } else { theme::pal().accent };
            let head = if *force { t!("强制重启？", "Force a restart?") } else { t!("重启会话？", "Restart the session?") };
            let notes = if why.is_empty() { vec![] } else { vec![why.clone()] };
            (
                t.name.as_str(),
                t.title.as_str(),
                border,
                head,
                String::new(),
                notes,
                if *force { t!("强制重启", "force restart") } else { t!("重启", "restart") },
            )
        }
        Confirm::Stop { t, attached } => {
            let notes = if *attached {
                vec![t!("⇄ 在别处打开，将被断开", "⇄ open elsewhere; it will be disconnected").to_string()]
            } else {
                vec![]
            };
            (
                t.name.as_str(),
                t.title.as_str(),
                theme::pal().accent,
                t!("停止会话？", "Stop the session?"),
                String::new(),
                notes,
                t!("停止", "stop"),
            )
        }
        Confirm::Pane { t, row, restart, why, next, .. } => {
            let mut notes: Vec<String> = [why.clone()].into_iter().filter(|w| !w.is_empty()).collect();
            if *restart {
                notes.push(t!("只重启此窗格，其他窗口不受影响", "restarts only this pane; other windows are untouched").into());
            } else {
                notes.push(t!("停止程序，保留窗格 · ⏎ 启动", "the program stops, the pane stays, ⏎ starts it again").into());
                notes.extend(next.iter().map(|n| tr!("{n}将成为首个", "{n} moves up to be first")));
            }
            let (head, act) = if *restart {
                (t!("重启此窗格？", "Restart this pane?"), t!("重启", "restart"))
            } else {
                (t!("停止此窗格？", "Stop this pane?"), t!("停止", "stop"))
            };
            (row.as_str(), t.title.as_str(), theme::pal().accent, head, String::new(), notes, act)
        }
        Confirm::Answer { t, question, shown, offer } => return draw_answer(f, None, false, t, question, shown.as_ref(), offer),
        Confirm::Adopt { t, plan } => (
            t.name.as_str(),
            t.title.as_str(),
            theme::pal().accent,
            t!("接管这个会话？", "Adopt this session?"),
            adopt_summary(plan),
            vec![],
            t!("接管", "adopt"),
        ),
        Confirm::Many { act, ts, notes } => return draw_many(f, *act, ts, notes, all),
        Confirm::Note { name, .. } => (
            "",
            name.as_str(),
            danger,
            t!("删除笔记？", "Delete the note?"),
            String::new(),
            vec![t!("移入回收站，保留 1 天 · b 恢复", "goes to the trash; b brings it back within a day").to_string()],
            t!("删除", "delete"),
        ),
        Confirm::Task { title, .. } => (
            "",
            title.as_str(),
            danger,
            t!("删除任务？", "Delete the task?"),
            String::new(),
            vec![t!("无法恢复", "it can't be brought back").to_string()],
            t!("删除", "delete"),
        ),
    };
    let s = all.iter().find(|s| !name.is_empty() && s.name == name);
    let head_style = if border == danger { red } else { theme::accent() };
    modal(f, 60, border, |w| {
        let (ic, sty) = match c {
            Confirm::Group(_) => ("▾", theme::faint()),
            Confirm::Note { .. } => ("✎", theme::faint()),
            Confirm::Task { .. } => ("·", dim()),
            _ => s.map_or(("·", dim()), session_icon),
        };
        let label = s.map(|s| s.agent_label()).unwrap_or_default();
        let who = s.map_or(Style::new(), |s| agent_style(s.agent));
        let mut out = vec![
            modal_title(head, head_style, esc_hint(t!("取消", "cancel")), w),
            Line::raw(""),
            spread(
                vec![
                    Span::styled(ic, sty),
                    Span::raw(" "),
                    Span::styled(
                        clip(title, w.saturating_sub(4 + label.width())).trim_end().to_string(),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                ],
                vec![Span::styled(label, who)],
                w,
            ),
        ];
        out.extend(what.lines().flat_map(|l| wrap_width(l, w)).map(|l| Line::styled(l, dim())));
        out.extend(notes.iter().map(|n| Line::styled(clip(n, w).trim_end().to_string(), warn)));
        out.push(Line::raw(""));
        out.push(Line::from(vec![button(&format!("y {act}"), border)]));
        out
    });
}

/// The answer box shows at most this many lines of the question, then …
const ASK_LINES: usize = 6;

/// The answer box: 「回答 · <title>」 ("Answer · <title>"), Esc to close on the right; the question, the options one per line with
/// their numbers, then the keys that answer it here (and ⏎ to go in). Small, over the list (`list`, see `modal_beside`): the
/// preview stays in view
pub(super) fn draw_answer(
    f: &mut Frame,
    list: Option<Rect>,
    preview: bool,
    t: &Target,
    question: &str,
    shown: Option<&answer::Asking>,
    offer: &answer::Offer,
) {
    let head = tr!("回答 · {}", "Answer · {}", t.title);
    let opts: &[String] = match (offer, shown) {
        (answer::Offer::Number(n), Some(a)) => &a.opts[..(*n).min(a.opts.len())],
        _ => &[],
    };
    let n = opts.len();
    let pick = match n {
        1 => "1".to_string(),
        n => format!("1–{n}"),
    };
    let keys: Vec<(&str, &str)> = match offer {
        answer::Offer::Number(_) => vec![(pick.as_str(), t!("选", "pick")), ("⏎", t!("进去回答", "go in to answer"))],
        answer::Offer::YesNo => vec![("y", t!("允许", "allow")), ("n", t!("拒绝", "deny")), ("⏎", t!("进去回答", "go in to answer"))],
        answer::Offer::Nothing => vec![("⏎", t!("进去回答", "go in to answer"))],
    };
    modal_beside(f, list, preview, 60, theme::pal().accent, |w| {
        let mut out = vec![modal_title(&head, theme::accent(), esc_hint(t!("关闭", "close")), w), Line::raw("")];
        let mut body: Vec<String> = question.lines().flat_map(|l| wrap_width(l, w)).collect();
        if body.len() > ASK_LINES {
            body.truncate(ASK_LINES);
            body.push("…".into());
        }
        out.extend(body.into_iter().map(Line::raw));
        if !opts.is_empty() {
            out.push(Line::raw(""));
        }
        for (i, o) in opts.iter().enumerate() {
            let num = format!("{} ", i + 1);
            for (j, l) in wrap_width(o, w.saturating_sub(num.width())).into_iter().enumerate() {
                let lead = if j == 0 { Span::styled(num.clone(), key_style()) } else { Span::raw(" ".repeat(num.width())) };
                out.push(Line::from(vec![lead, Span::raw(l)]));
            }
        }
        out.push(Line::raw(""));
        out.push(keys_line(&keys, vec![], w));
        out
    });
}

/// The multi-select confirm lists at most this many sessions; beyond that it says how many more
const MANY_SHOWN: usize = 6;

/// Confirm for d / D / R after v multi-select: what it asks, one session per row (icon, title, account, as for a single one), what to watch
/// out for, the y button
fn draw_many(f: &mut Frame, act: Batch, ts: &[Target], notes: &[String], all: &[Session]) {
    let danger = theme::pal().red;
    let n = ts.len();
    let (head, label, border) = match act {
        Batch::Remove => (tr!("删除 {n} 个会话？", "Delete {n} sessions?"), t!("删除", "delete"), danger),
        Batch::Stop => (tr!("停止 {n} 个会话？", "Stop {n} sessions?"), t!("停止", "stop"), theme::pal().accent),
        Batch::Restart => (tr!("重启 {n} 个会话？", "Restart {n} sessions?"), t!("重启", "restart"), theme::pal().accent),
    };
    let head_style = if border == danger { Style::new().fg(danger) } else { theme::accent() };
    // Too many: list only the first few, the last line says how many more
    let shown = if n > MANY_SHOWN { MANY_SHOWN - 1 } else { n };
    modal(f, 60, border, |w| {
        let mut out = vec![modal_title(&head, head_style, vec![], w), Line::raw("")];
        for t in &ts[..shown] {
            let s = all.iter().find(|s| s.sub.is_none() && s.deck_id == t.id && s.profile == t.profile);
            let (ic, sty) = s.map_or(("·", dim()), session_icon);
            let who = s.map(|s| s.agent_label()).unwrap_or_default();
            let who_st = s.map_or(Style::new(), |s| agent_style(s.agent));
            let title = clip(&t.title, w.saturating_sub(4 + who.width())).trim_end().to_string();
            out.push(spread(
                vec![Span::styled(ic, sty), Span::raw(" "), Span::styled(title, Style::new().add_modifier(Modifier::BOLD))],
                vec![Span::styled(who, who_st)],
                w,
            ));
        }
        if shown < n {
            out.push(Line::styled(tr!("还有 {} 个", "{} more", n - shown), dim()));
        }
        out.extend(notes.iter().map(|m| Line::styled(clip(m, w).trim_end().to_string(), theme::warn())));
        out.push(Line::raw(""));
        out.push(Line::from(vec![button(&format!("y {label}"), border)]));
        out
    });
}

/// `A`: work out what adopting this tmux session would look like (look only, touch nothing)
pub(super) fn adopt_plan(target: &str) -> Result<aca_core::adopt::Plan, String> {
    let store = aca_core::store::Store::open()?;
    aca_core::adopt::plan(&store, &aca_core::config::load(), target, &aca_core::adopt::Opts::default())
}

/// In the adopt confirm: what it was recognized as, and what happens after adopting. One thing per line
fn adopt_summary(p: &aca_core::adopt::Plan) -> String {
    let mut v = vec![tr!(
        "登记为 {}{}",
        "register as {}{}",
        p.who(),
        if p.s.group.is_empty() { String::new() } else { tr!(" · 分组 {}", " · group {}", p.s.group) }
    )];
    v.push(match p.s.conv.as_str() {
        "" if p.s.tool == "shell" => t!("其中的程序不受影响", "the program inside is left alone").to_string(),
        "" => t!("无法识别对话：重启后将开始新对话", "couldn't tell which conversation: a restart starts a new one").to_string(),
        c => tr!(
            "对话 {}，进程不受影响，重启后继续",
            "conversation {}; the process is left alone and it continues after a restart",
            c.chars().take(8).collect::<String>()
        ),
    });
    if p.title() != p.tmux {
        v.push(tr!("tmux 会话名改为「{}」", "tmux session renamed to “{}”", p.title()));
    }
    if !p.s.args.is_empty() {
        v.push(tr!("每次启动时附带 {}", "every start adds {}", p.s.args.join(" ")));
    }
    if p.deck.is_some() {
        v.push(
            t!(
                "agent-deck 中会显示为已停止，请勿在 agent-deck 中再次启动",
                "agent-deck will show it as stopped; don't start it there again"
            )
            .to_string(),
        );
    }
    v.join("\n")
}

/// Why `s` can't reach it now and what to press instead: its agent isn't running. `key` (⏎, or `o` for a group's router)
/// starts only a stopped one
fn not_running(s: &Session, key: &str) -> String {
    if starts(s) { tr!("未运行 · {key} 启动", "not running · {key} starts it") } else { t!("未运行", "not running").into() }
}

/// What the reply box's ⏎ says instead of sending, by the row's latest state: waiting for you (answer it with s), stopped, not a
/// program that takes typing. None = send it. `key`: see `not_running`
fn unsendable(s: &Session, key: &str) -> Option<String> {
    match () {
        _ if s.state == State::Ask => Some(ops::asking_msg()),
        _ if list::wakes_on_send(s) => None,
        _ if s.state.gone() || s.state == State::Dead => Some(tr!("未发送:{}", "not sent: {}", not_running(s, key))),
        _ => cant_fill(s).map(|w| tr!("未发送:{w}", "not sent: {w}")),
    }
}

/// Default title when forking (same as what `session::fork` gives when no title is passed)
fn fork_title(title: &str) -> String {
    tr!("{title} (分叉)", "{title} (fork)")
}

/// The reply box's keys (its line in the popup, the bottom bar when it's in the preview): what doesn't fit goes from the right
pub(super) fn reply_keys() -> [(&'static str, &'static str); 4] {
    [("⏎", t!("发送", "send")), ("^J", t!("换行", "newline")), ("Esc", t!("存成便签", "keep as draft")), ("^G", t!("编辑器", "editor"))]
}

/// Input popup for rename / new group / note / reply (when the preview is off)
pub(super) fn draw_prompt(f: &mut Frame, p: &Prompt) {
    let reply = reply_keys();
    let (title, keys): (String, &[(&str, &str)]) = match &p.ask {
        Ask::Rename(_) => (t!("重命名", "Rename").into(), &[("⏎", t!("确定", "ok"))]),
        Ask::NewGroup => (t!("新建分组", "New group").into(), &[("⏎", t!("下一步", "next"))]),
        // A note's Esc means done (not cancel): like the bottom bar when writing a note in the preview, first in the key line
        Ask::Note { title, key, .. } => (
            // One in a group's notebook: called by the group (a new one) or by its own name
            if notes::in_book(key) { tr!("笔记 · {title}", "Note · {title}") } else { tr!("便签 · {title}", "Draft · {title}") },
            &[("Esc", t!("保存", "done")), ("⏎", t!("换行", "newline")), ("^G", t!("编辑器", "editor"))],
        ),
        Ask::Reply { title, .. } => (tr!("↩ 回话 · {title}", "↩ Reply · {title}"), &reply),
        Ask::Fork(t) => (tr!("分叉 · {}", "Fork · {}", t.title), &[("⏎", t!("分叉", "fork")), ("^U", t!("清空重写", "clear and rename"))]),
        Ask::Space(c) => (tr!("重命名工作区 {c}", "Rename workspace {c}"), &[("⏎", t!("确定", "ok"))]),
        Ask::GroupDir => (t!("新分组目录", "new group directory").into(), &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete"))]),
        Ask::ScratchDir { .. } => (
            t!("随手问目录", "scratch directory").into(),
            &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete")), ("^U", t!("清空 = 不设置", "empty = none"))],
        ),
        Ask::GroupName(g) => (tr!("改分组 · {g}", "Edit group · {g}"), &[("⏎", t!("下一步", "next"))]),
        Ask::GroupHome { to, .. } => (
            tr!("改分组 · {to} · 默认目录", "Edit group · {to} · default directory"),
            &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete")), ("^U", t!("清空 = 不设置", "empty = forget"))],
        ),
        Ask::MoveHome { to, .. } => (
            tr!("新分组 · {to} · 默认目录", "New group · {to} · default directory"),
            &[("⏎", t!("移过去", "move")), ("Tab", t!("补全", "complete")), ("^U", t!("清空 = 不设置", "empty = none"))],
        ),
        Ask::TaskTitle { n: None, .. } => (t!("新任务", "New task").into(), &[("⏎", t!("下一步", "next"))]),
        Ask::TaskTitle { n: Some(n), .. } => (tr!("改任务 · #{n}", "Edit task · #{n}"), &[("⏎", t!("下一步", "next"))]),
        Ask::TaskCriteria { n, title, .. } => (
            match n {
                Some(n) => tr!("改任务 · #{n} · 完成标准", "Edit task · #{n} · done when"),
                None => tr!("新任务 · {} · 完成标准", "New task · {} · done when", clip(title, 24).trim_end()),
            },
            &[("⏎", t!("确定", "ok")), ("^U", t!("清空 = 不写", "empty = none"))],
        ),
    };
    // Cancel's Esc goes to the right of the title, like other popups
    let esc = if matches!(p.ask, Ask::Note { .. } | Ask::Reply { .. }) { vec![] } else { esc_hint(t!("取消", "cancel")) };
    modal(f, 64, theme::pal().accent, |w| {
        let mut out = vec![modal_title(&title, theme::accent(), esc.clone(), w), Line::raw("")];
        if let Ask::Note { .. } | Ask::Reply { .. } = p.ask {
            // Multi-line: at most 8 lines, keeping the last few (where the cursor is)
            let mut body: Vec<String> = p.text.split('\n').flat_map(|l| wrap_width(l, w.saturating_sub(1))).collect();
            match body.last_mut() {
                Some(l) if l.width() < w => l.push('▏'),
                _ => body.push("▏".into()),
            }
            let skip = body.len().saturating_sub(8);
            out.extend(body.into_iter().skip(skip).map(Line::raw));
        } else {
            out.push(input_row(&p.text, vec![], w));
        }
        out.push(input_rule(w));
        if !p.hint.is_empty() {
            out.push(Line::styled(clip(&p.hint, w).trim_end().to_string(), theme::warn()));
        }
        out.push(Line::raw(""));
        out.push(keys_line(keys, vec![], w));
        out
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::screen::top_bar;
    use crate::ui::tests::{
        at, caps, ctrl, fixture, fun_in_h, has_head, hints, jump_to_group, keys, life_and_work, mk, on_ding, order, press, reader_rows,
        rehome_tools, row_of, said, select, sessions, shows, space, target, three, titles, to_scope, tool, typ, with_children,
        with_grandchildren, with_windows,
    };

    #[test]
    fn shift_r_restarts_idle_now_and_asks_when_running() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('R'));
        assert_eq!(app.job, Some(Job::Restart { t: target("闲"), force: false }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在重启「闲」…"));
        // No new job until the background one is done
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "请稍候:正在重启「闲」…");
        app.job_done(&job, Outcome::Done("已重启「闲」".into()), sessions());
        assert!(app.busy.is_none() && app.confirm.is_none());
        // Running ones ask first; any other key cancels
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('R'));
        assert!(app.job.is_none());
        shows(&reader_rows(&mut app, 46, 12), &["重启会话？", "跑", "正在运行,将被中断", "y 重启"]);
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none() && app.attach.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "未重启");
        press(&mut app, KeyCode::Char('R'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.job, Some(Job::Restart { t: target("跑"), force: false }));
    }

    #[test]
    fn fork_and_mcp_are_only_for_registered_claude_or_codex_sessions() {
        let mut v = sessions();
        v[3].agent = Agent::None; // 「闲」: a shell session
        v[2].deck_id.clear(); // 「跑」: a plain tmux session
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "该工具不支持分叉");
        press(&mut app, KeyCode::Char('M'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "只有 Claude / Codex 会话有 MCP");
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能分叉");
        press(&mut app, KeyCode::Char('M'));
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能查看 MCP");
    }

    #[test]
    fn antigravity_offers_reply_but_no_fork_in_the_list_or_on_keypress() {
        let mut v = sessions();
        let idle = v.iter_mut().find(|s| s.title == "闲").unwrap();
        idle.agent = Agent::Agy;
        let keys = list::row_keys(idle, false);
        assert!(keys.iter().any(|(k, _)| *k == "s"));
        assert!(!keys.iter().any(|(k, _)| *k == "f"));
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "该工具不支持分叉");
    }

    fn rehome_quotas(_: Agent) -> Vec<aca_records::usage::Quota> {
        let q = |who: &str, five: f64, week: f64| aca_records::usage::Quota {
            agent: Agent::Claude,
            who: who.into(),
            plan: String::new(),
            wins: vec![
                aca_records::usage::Win { label: "5h".into(), used: five, resets: None },
                aca_records::usage::Win { label: "7d".into(), used: week, resets: None },
            ],
            updated: 0,
            note: String::new(),
        };
        vec![q("ding", 98.0, 61.0), q("cw", 12.0, 40.0)]
    }

    /// cw2 isn't logged in, all the others are
    fn rehome_ask(_: Agent, name: &str) -> crate::accounts::Login {
        if name == "cw2" { crate::accounts::Login::Out } else { crate::accounts::Login::In("max".into()) }
    }

    /// ka has 5h, 7d and Fable; kb only 7d and Fable; kc only 7d
    fn column_quotas(_: Agent) -> Vec<aca_records::usage::Quota> {
        let win = |label: &str, used: f64| aca_records::usage::Win { label: label.into(), used, resets: None };
        let q = |who: &str, wins| aca_records::usage::Quota {
            agent: Agent::Claude,
            who: who.into(),
            plan: String::new(),
            wins,
            updated: 0,
            note: String::new(),
        };
        vec![
            q("ka", vec![win("5h", 5.0), win("7d", 57.0), win("Fable", 31.0)]),
            q("kb", vec![win("Fable", 100.0), win("7d", 8.0)]),
            q("kc", vec![win("7d", 64.0)]),
        ]
    }

    fn column_tools() -> Result<Vec<crate::tools::Tool>, String> {
        let t = |n: &str| crate::tools::Tool { note: String::new(), ..crate::ui::tests::tool(n, "claude", n, Agent::Claude) };
        Ok(vec![t("ding"), t("ka"), t("kb"), t("kc")])
    }

    #[test]
    fn quota_windows_sit_in_columns_in_x_and_n_and_a_missing_one_is_left_blank() {
        let mut app = fixture().with_tools(column_tools).with_rehome(column_quotas, rehome_ask, false);
        app.set_sessions(on_ding());
        select(&mut app, "闲");
        let col = |rows: &[String], who: &str, t: &str| {
            let r = rows.iter().find(|r| r.contains(&format!(" {who} "))).unwrap_or_else(|| panic!("{who}: {rows:#?}"));
            r.find(t).map(|i| r[..i].width())
        };
        for open in ["X", "n"] {
            keys(&mut app, open);
            let rows = reader_rows(&mut app, 100, 24);
            keys(&mut app, "⎋");
            // 7d in one column on all three rows, Fable in one column on the two that have it; the numbers right-aligned
            let sevens: Vec<_> = ["ka", "kb", "kc"].iter().map(|w| col(&rows, w, "7d")).collect();
            assert!(sevens[0].is_some() && sevens.iter().all(|c| *c == sevens[0]), "{open}: {sevens:?} {rows:#?}");
            assert_eq!(col(&rows, "ka", "Fable"), col(&rows, "kb", "Fable"), "{open}: {rows:#?}");
            assert_eq!(col(&rows, "kc", "Fable"), None);
            assert_eq!(col(&rows, "ka", "57%").map(|c| c + 1), col(&rows, "kb", "8%"), "{open}: {rows:#?}");
            assert!(rows.iter().any(|r| r.contains("5h 5% · 7d 57% · Fable  31%")), "{open}: {rows:#?}");
        }
    }

    #[test]
    fn x_lists_the_other_accounts_with_their_quota_and_login_and_enter_switches() {
        let mut app = fixture().with_tools(rehome_tools).with_rehome(rehome_quotas, rehome_ask, false).with_accounts(|o| {
            Box::new(crate::ui::tests::FakePage(if o == crate::acctpage::Opening::At(Agent::Claude, "cw".into()) { 3 } else { 0 }))
        });
        app.set_sessions(on_ding());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        // At the top, the current account and quota; below, only the other accounts of the same tool (not itself, no Codex, no
        // shell), whether logged in, quota
        let rows = reader_rows(&mut app, 80, 24);
        shows(
            &rows,
            &[
                "「闲」切换账号继续",
                "当前  Claude·ding   5h 98% · 7d 61%",
                "  1 cw    Claude  ✓         5h 12% · 7d 40%",
                "  2 cw2   Claude  未登录",
                "⏎ 切换 • i 登录 • a 账号页 • + 添加账号",
                "? 快捷键",
            ],
        );
        // The highlighted one is on the selected background, with no mark before it
        let lit = crate::ui::tests::lit_rows(&mut app, 80, 24);
        assert!(lit.iter().any(|l| l.starts_with("1 cw")) && !lit.iter().any(|l| l.contains("cw2")), "{lit:#?}");
        assert!(!rows.iter().any(|r| r.contains(" cx ") || r.contains("将中断")), "{rows:#?}");
        // Can't switch to one that isn't logged in; i logs it in right here, the box closing
        press(&mut app, KeyCode::Char('2'));
        assert!(app.job.is_none() && !app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "cw2 未登录 · i 登录");
        press(&mut app, KeyCode::Char('i'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.term_req.take().map(|r| r.title), Some("登录 Claude Code · cw2".to_string()));
        press(&mut app, KeyCode::Char('X'));
        // On a logged-in one i asks first (the login drops the current one as it starts); only y goes ahead
        press(&mut app, KeyCode::Char('i'));
        shows(&reader_rows(&mut app, 80, 24), &["重新登录 Claude Code 账号 cw?", "y 重新登录 • 其他键 取消"]);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.term_req.is_none() && !app.overlays.is_empty());
        // a: the accounts page, on the highlighted account
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.overlays.len(), 1);
        shows(&reader_rows(&mut app, 80, 24), &["假页面 3"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('X'));
        // ⏎ switches to the highlighted one, the box closes
        press(&mut app, KeyCode::Enter);
        assert!(app.overlays.is_empty());
        assert_eq!(app.job, Some(Job::Rehome { t: target("闲"), to: "claude:cw".into(), label: "cw".into() }));
        let job = app.take_job().unwrap();
        assert_eq!(app.busy.as_deref(), Some("正在将「闲」切换到 cw…"));
        app.job_done(&job, Outcome::Done("已切换到 cw · 发送「继续」即可接着做".into()), on_ding());
        assert_eq!(
            (app.current().unwrap().title.as_str(), app.msg.as_ref().unwrap().0.as_str()),
            ("闲", "已切换到 cw · 发送「继续」即可接着做")
        );

        // Running: the box says it will be interrupted
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('X'));
        shows(&reader_rows(&mut app, 80, 24), &["⚠ 切换将中断当前任务"]);
        press(&mut app, KeyCode::Esc);
        assert!(app.overlays.is_empty() && app.job.is_none());
    }

    #[test]
    fn x_says_why_it_cannot_switch() {
        // No other accounts: say how to add one
        let mut app =
            fixture().with_tools(|| Ok(vec![tool("ding", "claude", "ding", Agent::Claude)])).with_rehome(rehome_quotas, rehome_ask, false);
        app.set_sessions(on_ding());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        shows(&reader_rows(&mut app, 80, 20), &["没有其他 Claude 账号 · + 添加"]);
        press(&mut app, KeyCode::Enter);
        assert!(app.job.is_none());
        press(&mut app, KeyCode::Esc);
        // Shell sessions, plain tmux sessions: doesn't open
        let mut v = on_ding();
        v[3].agent = Agent::None; // 「闲」
        v[0].deck_id.clear(); // 「问」
        let mut app = App::new(String::new(), v).with_tools(rehome_tools);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('X'));
        assert!(app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "只有 Claude / Codex 会话可以切换账号");
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('X'));
        assert_eq!(app.msg.as_ref().unwrap().0, "普通 tmux 会话 · A 接管后才能换账号");
    }

    #[test]
    fn windows_and_split_panes_hang_under_their_session() {
        let mut app = with_windows();
        // Right below its own session, ordered by window number, pane number
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "htop", "zsh", "错", "构建"]);
        let rows = reader_rows(&mut app, 120, 30);
        // A session's windows are joined with ├, the last one with └
        shows(&rows, &["├ ○ #1.1 htop", "└ ○ #2 zsh", "└ ● #3 构建"]);
        // Window numbers of different lengths, the time column still lines up
        let col = |t: &str| {
            let r = row_of(&rows, t);
            r[..r.find(t).unwrap()].width()
        };
        assert_eq!((col(" 10秒"), col(" 30秒")), (col(" 20秒"), col(" 20秒")), "{rows:#?}");
        // The account follows the title, a ▪ in the tool's color before it
        let t = rows.iter().find(|r| r.contains("#3 构建")).unwrap();
        assert!(t.contains("▪cx "), "{t}");
        // The count in the section header counts sessions only
        assert!(has_head(&rows, "work", 3), "{rows:#?}");
        // A session whose own program has exited but with a window running: still among the open ones, the whole family with it
        let mut v = app.all.clone();
        v.iter_mut().find(|s| s.title == "错").unwrap().state = State::Dead;
        app.set_sessions(v);
        assert!(order(&app).contains(&"构建".to_string()) && order(&app).contains(&"错".to_string()));
        // Enter: go into its session, switching to that pane first
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
        // Operations on registered sessions don't apply to windows (D stops only this pane, see below)
        for k in ['R', 'r', 'm'] {
            press(&mut app, KeyCode::Char(k));
            assert!(app.confirm.is_none() && app.job.is_none() && app.prompt.is_none(), "{k}");
            assert!(app.msg.as_ref().unwrap().0.contains("#2 是窗口"), "{k}: {:?}", app.msg);
        }
        // d: closes only this pane
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭窗口？", "y 关闭"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job();
        assert!(matches!(&job, Some(Job::Remove { t, pane: Some(p) }) if t.title == "zsh" && p == "%2"), "{job:?}");
        assert_eq!(app.doing().as_deref(), Some("正在关闭「zsh」…"));
        // Only this pane is hidden; the session and the other windows stay
        let at = |app: &App, t: &str| app.all.iter().any(|s| s.title == t);
        assert!(!at(&app, "zsh") && at(&app, "闲") && at(&app, "htop"));
        let fresh = with_windows().all; // read before closing: still has it
        app.job_done(&job.unwrap(), Outcome::Done("已关闭「zsh」".into()), fresh);
        assert!(app.doing().is_none() && !at(&app, "zsh"));
        select(&mut app, "htop");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭窗格？"]);
        press(&mut app, KeyCode::Esc);
        // p: pins the session it's in, the windows go up with it
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(order(&app)[..2], ["闲", "htop"]);
    }

    #[test]
    fn big_d_on_a_window_stops_just_that_pane_and_enter_brings_it_back_in_place() {
        let mut app = with_windows();
        reader_rows(&mut app, 120, 30);
        // Window row: stops only it, stays in place
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('D'));
        shows(&reader_rows(&mut app, 100, 30), &["停止此窗格？", "停止程序，保留窗格 · ⏎ 启动", "y 停止"]);
        press(&mut app, KeyCode::Char('y'));
        let pane = |id: &str, label: &str| crate::park::Pane { id: id.into(), label: label.into() };
        let t = |label: &str| Target { name: "闲".into(), title: label.into(), profile: "default".into(), id: String::new() };
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Park { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") });
        // Stopped: record a step (^Z starts it again in place, see below)
        let mut fresh = with_windows().all;
        fresh.iter_mut().find(|s| s.title == "zsh").unwrap().state = State::Dead;
        app.job_done(&job, Outcome::Done("已停「#2「zsh」」 · ⏎ 再启动".into()), fresh.clone());
        // Stopped: D says so, ⏎ starts it again in place (attaching once the run loop is up)
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('D'));
        assert_eq!(said(&app), "已停止 · ⏎ 启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.respawn.take(), app.attach.take()), (Some(("闲".to_string(), pane("%2", "#2「zsh」"))), None));
        ctrl(&mut app, 'z');
        assert_eq!(app.take_job(), Some(Job::Respawn { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") }));
        app.job_done(&Job::Respawn { t: t("#2「zsh」"), p: pane("%2", "#2「zsh」") }, Outcome::Done("已重启".into()), fresh);
        assert_eq!(said(&app), "已撤销停止:「#2「zsh」」已在原位置重新启动");
        // Registered session row: stops the agent (its pane and the other windows stay), not "this pane"
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('D'));
        assert!(matches!(app.confirm, Some(Confirm::Stop { .. })));
        press(&mut app, KeyCode::Esc);
        // R: restarts the agent in place, other windows untouched
        press(&mut app, KeyCode::Char('R'));
        assert_eq!(app.take_job(), Some(Job::Restart { t: target("闲"), force: false }));
    }

    #[test]
    fn an_agent_that_quit_but_left_its_pane_starts_again_on_enter() {
        // Agent exited (stopped by D or on its own), pane kept with its screen: ■. ⏎ starts again continuing the original
        // conversation; not going straight in, nor rerunning the pane's original command
        let mut v = with_windows().all;
        let h = v.iter_mut().find(|s| s.title == "闲").unwrap();
        (h.state, h.pane) = (State::Dead, "%0".into());
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take(), app.respawn.take()), (Some(target("闲")), None, None));
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "已停止 · ⏎ 启动");
        // Another window inside it is entered as usual
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
        // Plain tmux session: D stops its pane, ⏎ starts it again in place with the original command
        let mut s = mk("外", "-", State::Idle, 5);
        (s.deck_id, s.profile, s.pane) = (String::new(), String::new(), "%7".into());
        let mut app = App::new(String::new(), vec![s.clone()]);
        press(&mut app, KeyCode::Char('D'));
        press(&mut app, KeyCode::Char('y'));
        let p = crate::park::Pane { id: "%7".into(), label: "外".into() };
        assert_eq!(
            app.take_job(),
            Some(Job::Park {
                t: Target { name: "外".into(), title: "外".into(), profile: String::new(), id: String::new() },
                p: p.clone()
            })
        );
        s.state = State::Dead;
        let mut app = App::new(String::new(), vec![s]);
        to_scope(&mut app, Scope::Week); // stopped: no longer among the open ones
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.respawn.take(), app.start.take()), (Some(("外".to_string(), p)), None));
    }

    #[test]
    fn d_on_a_registered_session_takes_its_windows_along_and_a_plain_one_only_loses_its_head() {
        let at = |app: &App, t: &str| app.all.iter().any(|s| s.title == t);
        // Registered session: removes the whole session, other windows in it close too; the confirmation says so
        let mut app = with_windows();
        reader_rows(&mut app, 120, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["删除会话？", "同时关闭其中的 2 个窗口 / 窗格", "y 删除"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Remove { t, pane: None } if t.title == "闲"), "{job:?}");
        assert!(!at(&app, "闲") && !at(&app, "htop") && !at(&app, "zsh"));

        // Plain tmux session row with other windows inside: closes only the first pane, the next one moves up
        let plain = |v: &mut Vec<Session>, pane: &str| {
            v.iter_mut().filter(|s| s.family() == "闲").for_each(|s| s.profile.clear());
            let h = v.iter_mut().find(|s| s.title == "闲").unwrap();
            (h.deck_id, h.pane) = (String::new(), pane.into());
        };
        let mut v = with_windows().all;
        plain(&mut v, "%0");
        let mut app = App::new(String::new(), v);
        reader_rows(&mut app, 120, 30);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 100, 30), &["关闭此窗格？", "其余依次前移，#1.1「htop」成为首个", "y 关闭"]);
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Remove { t, pane: Some(p) } if t.title == "闲" && p == "%0"), "{job:?}");
        // Nothing is hidden while closing: the session row stays until htop moves up, windows aren't hidden along with the
        // session (they'd be lost), and the family never loses its head
        assert!(at(&app, "闲") && at(&app, "htop") && at(&app, "zsh"));
        reader_rows(&mut app, 100, 30);
        // Closed: htop moves up and becomes the session row (session name unchanged)
        let mut fresh: Vec<Session> = with_windows().all.into_iter().filter(|s| s.title != "htop").collect();
        plain(&mut fresh, "%1");
        app.job_done(&job, Outcome::Done("已关闭「闲」".into()), fresh);
        assert!(at(&app, "闲") && at(&app, "zsh"));
        // Only one pane left: d closes the whole session
        let mut v = sessions();
        plain(&mut v, "%1");
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        assert!(matches!(&app.confirm, Some(Confirm::Remove(r)) if r.pane.is_none() && r.next.is_none() && r.others == 0));
    }

    #[test]
    fn children_hang_under_the_session_that_sent_them() {
        let mut app = with_children(10);
        // Follows its parent session (and its windows); a child stopped three days ago isn't among the open ones
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "错"]);
        for (w, two) in [(46, false), (120, true)] {
            let rows = reader_rows(&mut app, w, 30);
            assert!(row_of(&rows, "子跑").contains("└ ● 子跑"), "{w}: {rows:#?}");
            assert!(row_of(&rows, "zsh").contains("  └ ○ #2 zsh"), "{w}: {rows:#?}");
            // Count in the section header: a child session is a session too
            assert!(has_head(&rows, "work", 4), "{two}: {rows:#?}");
        }
        to_scope(&mut app, Scope::Week);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "子旧", "错"]);
        // Removing the parent: the children aren't removed with it, say so (hidden ones count too)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('d'));
        shows(&reader_rows(&mut app, 46, 16), &["删除会话？", "2 个子会话不会删除"]);
        press(&mut app, KeyCode::Esc);
        // A child itself, a window: nothing said
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.confirm.as_ref().map(|c| matches!(c, Confirm::Remove(r) if r.kids == 0)), Some(true));
    }

    #[test]
    fn sub_sessions_of_sub_sessions_hang_one_level_further_in() {
        let mut app = with_grandchildren();
        // A session is followed by its whole family: windows, children (with their windows and what they spawned), and only
        // then the next one
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙", "错"]);
        for w in [46, 120] {
            let rows = reader_rows(&mut app, w, 30);
            for want in ["└ ● 子跑", "  ├ ○ #2 zsh", "  └ ● 孙", "    ├ ○ #3 top", "    └ ● 曾孙"] {
                assert!(row_of(&rows, want).contains(want), "{w}: {rows:#?}");
            }
        }
        // 子跑 has a sibling after it (子旧): its level's vertical line runs down to the last row of its family
        to_scope(&mut app, Scope::Week);
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙", "子旧", "错"]);
        let rows = reader_rows(&mut app, 46, 30);
        let wants = ["├ ● 子跑", "│ ├ ○ #2 zsh", "│ └ ● 孙", "│   ├ ○ #3 top", "│   └ ● 曾孙", "└ ■ 子旧"];
        for want in wants {
            assert!(row_of(&rows, want).contains(want), "{rows:#?}");
        }
        // h goes out one level at a time
        select(&mut app, "曾孙");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "孙");
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.current().unwrap().title, "子跑");
        // Removing the middle one: the confirmation counts the ones it spawned (once it's gone they're top level; deeper ones
        // stay below them)
        select(&mut app, "子跑");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.confirm.as_ref().map(|c| matches!(c, Confirm::Remove(r) if r.kids == 1)), Some(true));
        press(&mut app, KeyCode::Esc);
        // Searching the innermost: every level above comes along (with their windows); searching the middle one: the ones
        // below don't
        let v: Vec<Session> = app.all.clone();
        assert_eq!(order(&App::new("曾孙".into(), v.clone())), ["闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        assert_eq!(order(&App::new("子跑".into(), v)), ["闲", "子跑", "zsh"]);
    }

    #[test]
    fn big_d_stops_after_y_and_a_stopped_one_starts_on_enter() {
        // Real activity times: after stopping, idle time keeps counting against the current time
        let real = |v: Vec<Session>| -> Vec<Session> {
            v.into_iter()
                .map(|mut s| {
                    s.active_at = epoch() - s.idle_for;
                    s
                })
                .collect()
        };
        let mut app = App::new(String::new(), real(sessions()));
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('D'));
        shows(&reader_rows(&mut app, 46, 16), &["停止会话？", "y 停止"]);
        press(&mut app, KeyCode::Enter); // only y counts
        assert!(app.job.is_none() && app.attach.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "未停止");
        press(&mut app, KeyCode::Char('D'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.job, Some(Job::Stop { t: target("闲") }));
        let job = app.take_job().unwrap();
        let fresh: Vec<Session> = real(sessions())
            .into_iter()
            .map(|mut s| {
                if s.title == "闲" {
                    (s.state, s.idle_for, s.active_at) = (State::Stopped, 9_000_000, 0);
                }
                s
            })
            .collect();
        app.job_done(&job, Outcome::Done("已停止「闲」".into()), fresh);
        // After stopping it leaves the open ones; the highlight goes on to the row after it
        assert!(!titles(&app).contains(&"闲".to_string()));
        assert_eq!(app.current().unwrap().title, "错");
        // Listed from the last 7 days on, marked ■: the tmux activity time is gone, keep using what was seen before stopping, so
        // it isn't taken for one stopped long ago
        to_scope(&mut app, Scope::Week);
        select(&mut app, "闲");
        let s = app.current().unwrap();
        assert_eq!((s.title.as_str(), s.state), ("闲", State::Stopped));
        assert!(s.idle_for < 60, "{}", s.idle_for);
        app.msg = None; // the message saying it stopped covers the bottom bar for a moment
        shows(&reader_rows(&mut app, 120, 16), &["已停止", caps("⏎ 启动 • Z 记录 • d 删除").as_str()]);
        // D again: already stopped; ⏎ starts it first, then goes in, no direct attach
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已停止 · ⏎ 启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
    }

    #[test]
    fn a_crashed_one_is_a_cross_but_starts_like_a_stopped_one() {
        let mut v = sessions();
        v[3].state = State::Crashed; // 「闲」: the last turn ended in an error, gone from tmux too
        v[4].state = State::Idle; // 「错」: leave just one with an error, to see whether the top bar counts it
        let mut app = App::new(String::new(), v);
        assert!(!titles(&app).contains(&"闲".to_string())); // not among the open ones
        to_scope(&mut app, Scope::Week);
        select(&mut app, "闲");
        assert!(row_of(&reader_rows(&mut app, 46, 16), "闲").contains("✕ 闲"));
        shows(&reader_rows(&mut app, 120, 16), &["出错 · 已停止", caps("⏎ 启动 • Z 记录 • d 删除").as_str()]);
        assert!(!top_bar(&app, 80).to_string().contains("出错")); // one that isn't running doesn't count as an error
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
    }

    #[test]
    fn r_renames_in_the_footer_with_the_old_title_filled_in() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('r'));
        shows(&reader_rows(&mut app, 46, 12), &["重命名", "Esc 取消", "> 闲▏", "⏎ 确定"]);
        press(&mut app, KeyCode::Enter); // unchanged
        assert!(app.job.is_none() && app.prompt.is_none());
        assert!(app.msg.is_none());
        press(&mut app, KeyCode::Char('r'));
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter); // empty isn't accepted
        shows(&reader_rows(&mut app, 46, 12), &["标题不能为空"]);
        typ(&mut app, "闲着 jk"); // while typing, j k are letters, not up/down
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(Job::Rename { t: target("闲"), to: "闲着 jk".into(), unlock: false }));
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.job.is_none());
        assert!(app.msg.is_none());
        // Refresh moved to ^R
        ctrl(&mut app, 'r');
        assert!(app.refresh && app.prompt.is_none());
    }

    #[test]
    fn m_picks_a_group_and_a_new_name_becomes_a_new_group() {
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑"); // in work
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        // The list's section order first, without its current work, no duplicates; groups with no default directory show
        // their sessions' directory
        let rows: Vec<(&str, &str, bool)> = p.rows.iter().map(|r| (r.group.as_str(), r.dir.as_str(), r.same)).collect();
        assert_eq!(rows, [("fun", "/p/fun", false), ("Zeta", "", false)]);
        assert_eq!(p.here, "/p/work");
        assert_eq!(p.under, "work");
        let all = ["Zeta", "fun", "work"].map(String::from).to_vec();
        let mv = |to: &str, home: &str| {
            Some(Job::Move { t: target("跑"), from: "work".into(), to: to.into(), groups: all.clone(), home: home.into() })
        };
        // What m's list came back with → the job
        let mut picked = |got: &str| {
            press(&mut app, KeyCode::Char('m'));
            let p = app.pick_group.take().unwrap();
            app.group_picked(p, Ok(Some(got.into())));
            (app.job.take(), app.msg.take().map(|m| m.0))
        };
        // A bare new name: a group under the one it's in, working in a directory of that name under that one's
        assert_eq!(picked(" new thing "), (mv("work/new-thing", "/p/work/new-thing"), None));
        // With a / in it: the whole path from the top. Under a group that's there: a directory under that group's
        assert_eq!(picked("fun/part"), (mv("fun/part", "/p/fun/part"), None));
        // No such parent, a group with no directory above it, or the top level: no directory, as it always was
        assert_eq!(picked(" side proj/part "), (mv("side-proj/part", ""), None));
        assert_eq!(picked("Zeta/x"), (mv("Zeta/x", ""), None));
        assert_eq!(picked("/top"), (mv("top", ""), None));
        // The name of a group that's there, in any case, is that group (nothing is made under this one)
        assert_eq!(picked("zeta"), (mv("Zeta", ""), None));
        assert_eq!(picked("Work"), (None, Some("已在 work 中".into())));
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(None)); // Esc pressed in fzf
        assert!(app.job.is_none());
    }

    #[test]
    fn tab_on_a_new_name_in_ms_list_asks_its_directory_before_anything_is_made() {
        let hints = || {
            let g = |n: &str, d: &str| (n.to_string(), d.to_string());
            crate::data::GroupHints { groups: vec![g("work", ""), g("fun", "/w/fun-home"), g("Zeta", "")] }
        };
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑"); // in work, directory /p/work
        let all = ["Zeta", "fun", "work"].map(String::from).to_vec();
        let mv =
            |home: &str| Job::Move { t: target("跑"), from: "work".into(), to: "work/rss".into(), groups: all.clone(), home: home.into() };
        let ask = |app: &mut App| {
            press(app, KeyCode::Char('m'));
            let p = app.pick_group.take().unwrap();
            app.ask_move_home(p, "rss");
        };
        // The usual directory is filled in; ⏎ takes it as it is
        ask(&mut app);
        shows(&reader_rows(&mut app, 80, 24), &["新分组 · work/rss · 默认目录", "/p/work/rss", "⏎ 移过去", "Tab 补全", "Esc 取消"]);
        assert!(app.job.is_none() && app.typing());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(mv("/p/work/rss")));
        // Another one typed: it needn't be there yet
        ask(&mut app);
        ctrl(&mut app, 'u');
        typ(&mut app, "/p/elsewhere/rss/");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(mv("/p/elsewhere/rss")));
        // Another group's default, or a file: said under the box, nothing made
        ask(&mut app);
        ctrl(&mut app, 'u');
        typ(&mut app, "/w/fun-home");
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 80, 24), &["该目录已是分组 fun 的默认目录"]);
        ctrl(&mut app, 'u');
        typ(&mut app, "/etc/hosts");
        press(&mut app, KeyCode::Enter);
        shows(&reader_rows(&mut app, 80, 24), &["/etc/hosts 不是目录"]);
        assert!(app.job.is_none() && app.typing());
        // Emptied: a group with no directory. Esc: nothing at all
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.job.take(), Some(mv("")));
        ask(&mut app);
        press(&mut app, KeyCode::Esc);
        assert!(app.job.is_none() && app.prompt.is_none() && app.pick_group.is_none());
    }

    #[test]
    fn m_lists_the_group_in_its_own_directory_first_and_prefers_the_default_directory() {
        let hints = || {
            let g = |n: &str, d: &str| (n.to_string(), d.to_string());
            crate::data::GroupHints { groups: vec![g("work", ""), g("fun", "/w/fun-home"), g("Zeta", "/p/work")] }
        };
        let mut app = fixture().with_group_hints(hints);
        select(&mut app, "跑"); // in work, directory /p/work
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        let rows: Vec<(&str, &str, bool)> = p.rows.iter().map(|r| (r.group.as_str(), r.dir.as_str(), r.same)).collect();
        // Zeta's default directory is its directory: moved to the front; fun has a default directory, so its sessions' /p/fun
        // isn't looked at
        assert_eq!(rows, [("Zeta", "/p/work", true), ("fun", "/w/fun-home", false)]);
    }

    #[test]
    fn keys_for_registered_sessions_refuse_plain_tmux_sessions() {
        let mut s = mk("外", "-", State::Idle, 5);
        (s.deck_id, s.profile) = (String::new(), String::new());
        let mut app = App::new(String::new(), vec![s]);
        for (key, what) in [('R', "重启"), ('r', "重命名"), ('m', "移到分组")] {
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.msg.as_ref().unwrap().0, format!("普通 tmux 会话 · A 接管后才能{what}"));
        }
        assert!(app.job.is_none() && app.prompt.is_none() && app.pick_group.is_none() && app.confirm.is_none());
    }

    /// What's recognized before adopting (fake): only recognizes the tmux session 「外」
    fn fake_plan(target: &str) -> Result<aca_core::adopt::Plan, String> {
        if target != "$外" {
            return Err(format!("tmux 里没有 {target} 这个会话"));
        }
        Ok(aca_core::adopt::Plan {
            s: aca_core::store::Session {
                id: "id-接过来".into(),
                title: "agentdeck_x".into(),
                tool: "claude".into(),
                account: "sub".into(),
                group: "work".into(),
                conv: "c-1234567890".into(),
                args: vec!["--yolo".into()],
                ..Default::default()
            },
            known: false,
            sid: "$外".into(),
            tmux: "agentdeck_x".into(),
            pane: "%9".into(),
            transcript: String::new(),
            auto_title: "修登录".into(),
            deck: Some(("default".into(), "ad-1".into())),
            notes: vec![],
        })
    }

    #[test]
    fn big_a_adopts_a_plain_tmux_session_after_showing_what_it_recognised() {
        let plain = |title: &str| {
            let mut s = mk(title, "-", State::Idle, 5);
            (s.deck_id, s.profile, s.tool) = (String::new(), String::new(), String::new());
            s
        };
        let mut all = sessions();
        all.extend([plain("外"), plain("没了的")]);
        let mut app = App::new(String::new(), all).with_adopt(fake_plan);
        // Registered session: already is one
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.msg.as_ref().unwrap().0, "它已是 aca 会话");
        assert!(app.confirm.is_none());
        // Can't be recognized (that session just went away): say why, no popup
        select(&mut app, "没了的");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(app.msg.as_ref().unwrap().0, "无法接管:tmux 里没有 $没了的 这个会话");
        assert!(app.confirm.is_none());

        // Plain session: lay out what was recognized first
        select(&mut app, "外");
        press(&mut app, KeyCode::Char('A'));
        assert!(matches!(app.confirm, Some(Confirm::Adopt { .. })));
        let rows = reader_rows(&mut app, 100, 30).join("\n");
        for want in [
            "接管这个会话？",
            "登记为 claude:sub · 分组 work",
            "对话 c-123456，进程不受影响，重启后继续",
            "tmux 会话名改为「修登录」",
            "每次启动时附带 --yolo",
            "agent-deck 中会显示为已停止",
            "y 接管",
        ] {
            assert!(rows.contains(want), "确认框里没有「{want}」:\n{rows}");
        }
        // Only y counts; any other key means don't adopt
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.msg.as_ref().unwrap().0, "未接管");
        assert!(app.job.is_none() && app.confirm.is_none());
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        let t = Target { name: "外".into(), title: "外".into(), profile: String::new(), id: String::new() };
        assert_eq!(job, Job::Adopt { t, plan: Box::new(fake_plan("$外").unwrap()) });
        assert_eq!(job.doing(), "正在接管「外」…");

        // After adopting, that row becomes the registered one (keyed by id, in its group): the highlight follows
        let mut fresh = sessions();
        let mut got = mk("修登录", "work", State::Idle, 5);
        (got.name, got.deck_id) = ("id-接过来".into(), "id-接过来".into());
        fresh.push(got);
        app.job_done(&job, Outcome::Made { msg: "已接管「修登录」".into(), id: "id-接过来".into() }, fresh);
        assert_eq!(app.current().map(|s| (s.name.as_str(), s.title.as_str())), Some(("id-接过来", "修登录")));
        assert_eq!(app.msg.as_ref().unwrap().0, "已接管「修登录」");
    }

    #[test]
    fn the_adopt_dialog_says_when_nothing_will_be_renamed_or_resumed() {
        let mut p = fake_plan("$外").unwrap();
        // A session name you gave yourself (locked), no conversation recognized, no args, not agent-deck's
        (p.s.title, p.s.title_locked, p.tmux) = ("我的会话".into(), true, "我的会话".into());
        (p.s.conv, p.s.args, p.s.group, p.deck) = (String::new(), vec![], String::new(), None);
        assert_eq!(adopt_summary(&p), "登记为 claude:sub\n无法识别对话：重启后将开始新对话");
        (p.s.tool, p.s.account) = ("shell".into(), String::new());
        assert_eq!(adopt_summary(&p), "登记为 shell\n其中的程序不受影响");
    }

    #[test]
    fn capital_f_lists_the_states_to_look_at_and_the_row_keys_pick_one() {
        let mut app = fixture();
        assert_eq!(titles(&app).len(), 5);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(0));
        // One row per state: at the start the key that picks it directly, icon, name, count; row 0 is all
        let rows = reader_rows(&mut app, 60, 16);
        shows(
            &rows,
            &["按状态筛选", "Esc 关闭", "0", "所有", "1  ●", "运行中", "2  ◆◐", "等待回应 · 未读", "6    ☾", "自动停止", "⏎ 筛选 • 0 所有"],
        );
        // ☾ is a part of ■: its icon and name sit two cells further in, the key column stays
        let at = |row: &str, t: &str| {
            let r = rows.iter().find(|r| r.contains(row)).unwrap();
            r[..r.find(row).unwrap() + row.find(t).unwrap()].width()
        };
        assert_eq!([at("5  ■", "5"), at("6    ☾", "6")], [at("1  ●   运行中", "1"); 2]);
        assert_eq!(at("6    ☾", "☾"), at("5  ■", "■") + 2);
        assert_eq!(at("6    ☾   自动停止", "自动停止"), at("5  ■   已停止", "已停止") + 2);
        // The old symbols do nothing in the small list
        press(&mut app, KeyCode::Char('!'));
        assert_eq!((app.only_menu, app.status), (Some(0), None));
        // The row's key picks directly and closes the small list; reopened, the highlight is on the picked one
        press(&mut app, KeyCode::Char('1'));
        assert_eq!((app.only_menu, app.status), (None, Some(Pick::Run)));
        assert_eq!(titles(&app), ["跑"]);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(1));
        // ⏎ on the picked one = cancel
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.only_menu, app.status), (None, None));
        // ↓ to "needs you · unread" ⏎; 0 = every session (Space does nothing); Esc only closes the small list
        keys(&mut app, "Fjj⏎");
        assert_eq!((app.status, titles(&app)), (Some(Pick::Wait), vec!["未读".to_string(), "问".to_string()]));
        keys(&mut app, "F ");
        assert_eq!((app.status, app.only_menu.is_some()), (Some(Pick::Wait), true));
        keys(&mut app, "0");
        assert_eq!(app.status, Some(Pick::Every));
        keys(&mut app, "F⎋");
        assert!(app.only_menu.is_none() && !app.quit);
        // f only forks, no longer doubles as F
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt.is_some() && app.only_menu.is_none());
    }

    #[test]
    fn capital_q_sends_s_switches_and_h_stays_hidden() {
        let v = life_and_work();
        let mut app = App::new(String::new(), v.clone());
        select(&mut app, "zsh"); // Pressed on a window = sends the session it belongs to
        keys(&mut app, "Ql");
        assert_eq!(titles(&app), ["工作", "生活二", "私"]); // Windows and child sessions went along
        select(&mut app, "私");
        keys(&mut app, "Qh");
        assert_eq!(titles(&app), ["工作", "生活二"]);
        assert!(!top_bar(&app, 80).to_string().contains("未读")); // Other workspaces aren't counted
        press(&mut app, KeyCode::Char('i')); // Nor jumped into
        assert!(app.attach.is_none());
        keys(&mut app, "/生活");
        assert_eq!(titles(&app), ["生活二"]); // Nor found by search
        keys(&mut app, "⎋");
        app.set_sessions(v.clone()); // Same after a refresh
        assert_eq!(titles(&app), ["工作", "生活二"]);
        keys(&mut app, "Sl");
        assert_eq!((app.space, titles(&app)), ('l', vec!["zsh".to_string(), "子".into(), "生活".into()]));
        assert!(top_bar(&app, 80).to_string().contains(" l ") && top_bar(&app, 80).to_string().contains("1 未读"));
        keys(&mut app, "Sh");
        assert_eq!(titles(&app), ["私"]);
        keys(&mut app, "S1");
        assert_eq!(at(&app), "生活二"); // Back in 1: the highlight is on the one it was on when we left
        keys(&mut app, "q⎋"); // Cancel, don't quit
        assert!(!app.quit && app.pending.is_none());
        keys(&mut app, "qq");
        assert!(app.quit);
    }

    #[test]
    fn moving_into_a_group_kept_in_h_does_not_hide_the_session() {
        let mut app = fun_in_h().with_group_hints(hints);
        select(&mut app, "工作");
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(Some("fun".into())));
        assert!(matches!(app.job.take(), Some(Job::Move { .. })));
        let mut v = life_and_work();
        v[0].group = "fun".into(); // Moved
        app.set_sessions(v);
        assert_eq!(space(&app, "工作"), '1');
    }

    #[test]
    fn e_writes_a_note_saving_every_key_and_esc_just_leaves() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        assert!(app.typing());
        typ(&mut app, "先跑测试");
        press(&mut app, KeyCode::Enter); // ⏎ is a newline
        typ(&mut app, "再提交");
        assert_eq!(app.notes.get("id-闲"), Some("先跑测试\n再提交")); // Saved as you type, no waiting for Esc
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("✎ 便签")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("再提交▏")), "光标在预览里:{rows:#?}");
        // Written in the preview, so the bottom bar only has the keys, without repeating the text
        assert!(rows.last().unwrap().trim_start().starts_with(&caps("Esc 保存")), "{rows:#?}");
        assert!(!rows.last().unwrap().contains("先跑测试"), "{rows:#?}");
        // With no preview (off), written in a popup
        app.preview = Preview::Off;
        shows(&reader_rows(&mut app, 100, 24), &["便签 · 闲", "先跑测试", "再提交▏", "Esc 保存"]);
        app.preview = Preview::Auto;
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "便签已保存");
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("○ 闲") && r.contains('✎')), "{rows:#?}");
        for want in ["空闲 · 10 秒前", "/p/work", "✎ 先跑测试", "再提交"] {
            assert!(rows.iter().any(|r| r.contains(want)), "缺「{want}」:{rows:#?}");
        }
        // e again: the old note prefilled; ^U clears it and deletes it on the spot
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "先跑测试\n再提交");
        ctrl(&mut app, 'u');
        assert_eq!(app.notes.get("id-闲"), None);
        press(&mut app, KeyCode::Esc);
        assert!(!reader_rows(&mut app, 100, 24).iter().any(|r| r.contains('✎')));
    }

    #[test]
    fn ctrl_g_hands_the_note_to_the_editor() {
        let mut app = fixture();
        select(&mut app, "闲");
        // ^G halfway through: takes what's written to the editor, then back to writing in the bottom bar
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "草稿");
        ctrl(&mut app, 'g');
        assert!(app.prompt.is_none());
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "草稿");
        app.note_editor_done(ed, Ok("草稿\n编辑器里补的".into()));
        assert_eq!(app.notes.get("id-闲"), Some("草稿\n编辑器里补的"));
        assert_eq!(app.prompt.as_ref().unwrap().text, "草稿\n编辑器里补的");
        press(&mut app, KeyCode::Esc);
        // ^G straight from the list; the editor not exiting cleanly (:cq) = no change
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "草稿\n编辑器里补的");
        app.note_editor_done(ed, Err("nvim 未正常退出,便签未修改".into()));
        assert_eq!(app.notes.get("id-闲"), Some("草稿\n编辑器里补的"));
        assert!(app.prompt.is_none());
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        app.note_editor_done(ed, Ok(String::new())); // Emptied in the editor = note deleted
        assert_eq!(app.notes.get("id-闲"), None);
    }

    #[test]
    fn e_on_a_group_starts_a_new_note_in_its_notebook() {
        let mut app = fixture();
        let notes = |app: &App| app.book.of("work").map(|n| n.text.clone()).collect::<Vec<_>>();
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "这一组:先把测试补齐");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "再看性能");
        // Saved as it's typed: one note in the group's notebook, nothing in the sessions' notes file
        assert_eq!(notes(&app), ["这一组:先把测试补齐\n再看性能"]);
        assert_eq!(app.notes.get("group:work"), None);
        // Written where the preview is, under 「新笔记」 (new note), cursor at the end; in a popup when the preview is off
        shows(&reader_rows(&mut app, 100, 24), &["✎ 新笔记", "再看性能▏"]);
        app.preview = Preview::Off;
        shows(&reader_rows(&mut app, 100, 24), &["笔记 · work", "再看性能▏"]);
        app.preview = Preview::Auto;
        press(&mut app, KeyCode::Esc);
        assert_eq!(said(&app), "笔记已保存");
        // ✎ after the group name; the group's sessions have no note of their own. Beside the group's row only pinned notes
        // are named
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("▾ work") && r.contains('✎')), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("这一组:先把测试补齐")), "{rows:#?}");
        assert!(app.all.iter().filter(|s| s.group == "work").all(|s| s.group_noted && !s.noted));
        let id = app.book.top("work").unwrap().id.clone();
        app.book.pin(&id, true).unwrap();
        shows(&reader_rows(&mut app, 100, 24), &["★ 这一组:先把测试补齐"]);
        // e on a session still writes the session's own
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "");
        press(&mut app, KeyCode::Esc);
        // Back on the group: e starts another one (the first is rewritten on the group's notes page); ^Z takes it back out
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "");
        typ(&mut app, "第二条");
        press(&mut app, KeyCode::Esc);
        assert_eq!(notes(&app).len(), 2);
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "已撤销 · 笔记「work」");
        assert_eq!(notes(&app), ["这一组:先把测试补齐\n再看性能"]);
        // ^G: a new one written in the editor; nothing written there = no note
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "");
        app.note_editor_done(ed, Ok(String::new()));
        assert_eq!(notes(&app).len(), 1);
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        app.note_editor_done(ed, Ok("编辑器里写的".into()));
        assert_eq!((notes(&app).len(), said(&app)), (2, "笔记已保存"));
        // Opened and left blank: no note is made
        jump_to_group(&mut app, "fun");
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "写");
        assert_eq!(app.book.of("fun").count(), 1);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.book.of("fun").count(), 0);
    }

    #[test]
    fn only_shift_e_takes_the_note_in_and_only_when_it_is_safe() {
        let mut app = fixture();
        for t in ["闲", "跑", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的")).unwrap();
        }
        app.set_sessions(sessions());
        to_scope(&mut app, Scope::Week); // 7 days (「未读」 has been quiet for five days)
        select(&mut app, "闲");
        press(&mut app, KeyCode::Enter); // ⏎: the note stays a note
        assert_eq!((app.attach.take().as_deref(), app.fill.take()), (Some("闲"), None));
        press(&mut app, KeyCode::Char('E'));
        assert_eq!(app.attach.take().as_deref(), Some("闲"));
        assert_eq!(app.fill.take(), Some(("id-闲".into(), "给闲的".into())));
        select(&mut app, "跑"); // Fills while running too (Claude takes typing as it runs, ⏎ queues it); Shift+Enter in kitty too
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.fill.take().map(|f| f.0), Some("id-跑".into()));
        app.attach = None;
        select(&mut app, "问"); // Waiting for your choice: text would be taken as an option → doesn't go in, says why
        press(&mut app, KeyCode::Char('E'));
        assert_eq!((app.attach.take(), app.fill.take()), (None, None));
        assert!(app.msg.as_ref().unwrap().0.contains("等待回应"));
        select(&mut app, "未读"); // No note: same as ⏎
        press(&mut app, KeyCode::Char('E'));
        assert_eq!((app.attach.take().as_deref(), app.fill.take()), (Some("未读"), None));
        // Deleted once filled, and the ✎ goes with it
        app.note_filled("id-闲");
        assert_eq!(app.notes.get("id-闲"), None);
        assert!(!app.all.iter().find(|s| s.title == "闲").unwrap().noted);
        // Not a built-in agent: no telling what program is in front, so no fill
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "跑").unwrap().agent = Agent::None;
        app.set_sessions(v);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('E'));
        assert_eq!(app.attach.take(), None);
        assert!(app.msg.as_ref().unwrap().0.contains("不是内置 agent"));
    }

    #[test]
    fn s_opens_a_reply_box_and_enter_sends_what_is_written_without_asking() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.typing() && app.confirm.is_none());
        typ(&mut app, "继续");
        assert_eq!(app.notes.get("id-闲"), Some("继续")); // Kept as its note while written
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_none() && app.confirm.is_none());
        let job = app.take_job().unwrap();
        let Job::Send { t, pane, text, key } = &job else { panic!("{job:?}") };
        assert_eq!((t, pane.as_str(), text.as_str(), key.as_str()), (&target("闲"), "=闲:", "继续", "id-闲"));
        app.job_done(&job, Outcome::Done("已发给「闲」".into()), sessions());
        assert_eq!(said(&app), "已发给「闲」");
        assert_eq!(app.notes.get("id-闲"), None);
        assert!(!reader_rows(&mut app, 100, 24).iter().any(|r| r.contains('✎')));
    }

    #[test]
    fn a_note_is_already_in_the_reply_box_and_is_gone_once_sent() {
        let mut app = fixture();
        app.notes.set("id-闲", "给闲的\n第二行").unwrap();
        app.set_sessions(sessions());
        select(&mut app, "闲");
        assert!(reader_rows(&mut app, 100, 24).iter().any(|r| r.contains("○ 闲") && r.contains('✎')));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "给闲的\n第二行");
        // The cursor at its end, in the preview's top half, under the reply's own head
        let rows = reader_rows(&mut app, 100, 24);
        shows(&rows, &["↩ 回话", "给闲的", "第二行▏"]);
        assert!(!rows.iter().any(|r| r.contains("发送便签")), "{rows:#?}");
        typ(&mut app, ",先跑测试");
        press(&mut app, KeyCode::Enter);
        assert!(app.confirm.is_none());
        let job = app.take_job().unwrap();
        let Job::Send { text, .. } = &job else { panic!("{job:?}") };
        assert_eq!(text, "给闲的\n第二行,先跑测试");
        app.job_done(&job, Outcome::Done("已发给「闲」".into()), sessions());
        assert!(!reader_rows(&mut app, 100, 24).iter().any(|r| r.contains('✎')));
    }

    #[test]
    fn esc_keeps_the_reply_as_its_note_and_an_empty_enter_sends_nothing() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter); // Empty: nothing sent, the box stays
        typ(&mut app, "  ");
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_some() && app.take_job().is_none());
        ctrl(&mut app, 'u');
        typ(&mut app, "先别动 store");
        press(&mut app, KeyCode::Esc);
        assert!(app.prompt.is_none() && app.take_job().is_none());
        assert_eq!(said(&app), "便签已保存");
        assert!(reader_rows(&mut app, 100, 24).iter().any(|r| r.contains("○ 闲") && r.contains('✎')));
        // s again: still there
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "先别动 store");
        // Esc with nothing written and nothing there before says nothing
        ctrl(&mut app, 'u');
        press(&mut app, KeyCode::Esc);
        app.msg = None;
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(said(&app), "");
    }

    #[test]
    fn ctrl_j_and_shift_enter_break_the_line_and_a_paste_does_not_send_halfway() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "一");
        ctrl(&mut app, 'j');
        typ(&mut app, "二");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        typ(&mut app, "三");
        // Keys that came together (a paste): an Enter in it is a line break
        app.burst = true;
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "四");
        app.burst = false;
        assert!(app.take_job().is_none());
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        let Job::Send { text, .. } = &job else { panic!("{job:?}") };
        assert_eq!(text, "一\n二\n三\n四");
        assert!(app.prompt.is_none() && app.take_job().is_none()); // Sent once
    }

    #[test]
    fn a_session_that_started_asking_while_you_typed_is_not_sent_to() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "继续");
        // The list refreshed meanwhile: it's asking now
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "闲").unwrap().state = State::Ask;
        app.set_sessions(v);
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_none() && app.take_job().is_none());
        assert_eq!(app.notes.get("id-闲"), Some("继续"));
        assert!(said(&app).contains("正在等待回应") && said(&app).contains("s 回答"), "{}", said(&app));
        // Background work still running: the box stays, the bottom bar says to wait
        let mut app = fixture();
        app.busy = Some("正在重启「跑」…".into());
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "继续");
        press(&mut app, KeyCode::Enter);
        assert!(app.prompt.is_some() && app.take_job().is_none());
        shows(&reader_rows(&mut app, 100, 24), &["请稍候:正在重启「跑」…"]);
        app.busy = None;
        press(&mut app, KeyCode::Enter);
        assert!(app.take_job().is_some());
    }

    #[test]
    fn the_reply_box_and_the_note_box_are_told_apart_by_their_head_and_keys() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "继续");
        let rows = reader_rows(&mut app, 100, 24);
        shows(&rows, &["↩ 回话", "继续▏"]);
        assert!(!rows.iter().any(|r| r.contains("✎ 便签")), "{rows:#?}");
        assert!(rows.last().unwrap().trim_start().starts_with(&caps("⏎ 发送 • ^J 换行 • Esc 存成便签 • ^G 编辑器")), "{rows:#?}");
        // ^G: into the editor and back into the reply box
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        app.note_editor_done(ed, Ok("继续\n编辑器里补的".into()));
        assert!(matches!(app.prompt.as_ref().unwrap().ask, Ask::Reply { .. }));
        assert_eq!(app.prompt.as_ref().unwrap().text, "继续\n编辑器里补的");
        press(&mut app, KeyCode::Esc);
        // e on it: the note box, as it was
        press(&mut app, KeyCode::Char('e'));
        let rows = reader_rows(&mut app, 100, 24);
        shows(&rows, &["✎ 便签", "编辑器里补的▏"]);
        assert!(!rows.iter().any(|r| r.contains("↩ 回话")), "{rows:#?}");
        assert!(rows.last().unwrap().trim_start().starts_with(&caps("Esc 保存")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        // Narrow: the keys go from the right
        press(&mut app, KeyCode::Char('s'));
        let rows = reader_rows(&mut app, 64, 20);
        let bar = rows.last().unwrap().trim();
        assert!(bar.starts_with(&caps("⏎ 发送")) && rows.iter().any(|r| r.contains("↩ 回话")), "{rows:#?}");
    }

    /// The note box (`e`), frame for frame: in the preview (under the rule of the column's head), and as a popup (whose list bar
    /// behind it is left out: it names the row's keys, `s 回话` (s reply) among them now)
    #[test]
    fn the_note_box_is_drawn_exactly_as_before() {
        let mut app = fixture();
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "先跑测试");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "再提交");
        let before = [
            " 全部分组   未停止 5   近 7 天 5                  ◆ 1 等待回应    ● 1 运行中    ◐ 1 未读    ✕ 1 出错",
            " ▾ fun  2                                        ○ 闲                                       Claude",
            "  ◆ 问                                 11天      空闲 · 10 秒前   /p/work",
            "  ◐ 未读                                5天      ─────────────────────────────────────────────────",
            " ▾ work  3                                       ✎ 便签",
            "  ● 跑                                  5秒        先跑测试",
            "▌ ○ 闲                              ✎  10秒        再提交▏",
            "  ✕ 错                                  1天",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "                                  Esc  保存    ⏎  换行    ^G  编辑器",
        ];
        assert_eq!(reader_rows(&mut app, 100, 24), before);
        app.preview = Preview::Off;
        let before = [
            " 全部分组   未停止 5   近 7 天 5                  ◆ 1 等待回应    ● 1 运行中    ◐ 1 未读    ✕ 1 出错",
            " ▾ fun  2",
            "  1 ◆ 问                                                                                       11天",
            "  2 ◐ 未读                                                                                      5天",
            " ▾ work  3",
            "  3 ● 跑                                                                                        5秒",
            "▌ 4 ○ 闲          ╭──────────────────────────────────────────────────────────────╮          ✎  10秒",
            "  5 ✕ 错          │                                                              │              1天",
            "                  │  便签 · 闲                                                   │",
            "                  │                                                              │",
            "                  │  先跑测试                                                    │",
            "                  │  再提交▏                                                     │",
            "                  │  ──────────────────────────────────────────────────────────  │",
            "                  │                                                              │",
            "                  │  Esc 保存 • ⏎ 换行 • ^G 编辑器                               │",
            "                  │                                                              │",
            "                  ╰──────────────────────────────────────────────────────────────╯",
            "",
            "",
            "",
            "",
            "",
            "─ ○ 空闲 · 10 秒前 · /p/work ───────────────────────────────────────────────────────────────── 4/5 ─",
        ];
        assert_eq!(reader_rows(&mut app, 100, 24)[..23], before);
    }

    #[test]
    fn with_the_preview_off_the_reply_box_is_a_popup_with_the_same_keys() {
        let mut app = fixture();
        (app.preview, app.preview_narrow) = (Preview::Off, Preview::Off);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "先别动 store,把 runtime 那层拆出来");
        let rows = reader_rows(&mut app, 46, 16);
        shows(&rows, &["↩ 回话 · 闲", "▏", "⏎ 发送 • ^J 换行"]);
        assert!(rows.iter().all(|r| r.width() <= 46));
        // The text wraps inside the box, nothing cut
        let body: String = rows.iter().map(|r| r.trim_matches(|c: char| c == '│' || c == ' ')).collect();
        assert!(
            body.contains("先别动store,把runtime那层拆出来") || body.replace(' ', "").contains("先别动store,把runtime那层拆出来"),
            "{rows:#?}"
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.take_job().is_some());
    }

    #[test]
    fn s_where_nothing_could_be_sent_opens_nothing_and_says_why() {
        let mut app = three();
        to_scope(&mut app, Scope::Week);
        select(&mut app, "错"); // Stopped
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_none());
        assert_eq!(said(&app), "未运行 · ⏎ 启动");
        // Not a built-in agent: no telling what program is in front
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "跑").unwrap().agent = Agent::None;
        app.set_sessions(v);
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_none());
        assert_eq!(said(&app), "无法回话:不是内置 agent");
        // A group: it's its router that is replied to, and this one has none yet
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_none());
        assert_eq!(said(&app), "还没有 router · o 新建");
        // Running: it takes one (it queues it)
        let mut app = fixture();
        select(&mut app, "跑");
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(app.prompt.as_ref().unwrap().ask, Ask::Reply { .. }));
    }

    #[test]
    fn on_a_phone_an_idle_rows_bottom_bar_keeps_open_and_reply() {
        let mut app = fixture();
        select(&mut app, "闲");
        let bar = reader_rows(&mut app, 46, 16).last().unwrap().clone();
        assert!(bar.contains(&caps("⏎ 打开 • s 回话")) && !bar.contains(&caps("e 便签")), "{bar}");
    }

    /// 「问」 ("asking") waiting on a question: what it asks as answering reads it, and the keys its agent offers for it
    fn asking(kind: &str, ask: &str, opts: &[&str], agent: Agent) -> Vec<Session> {
        let what =
            answer::Asking { ask: ask.into(), kind: kind.into(), opts: opts.iter().map(|o| o.to_string()).collect(), ..Default::default() };
        let offer = answer::offer(aca_records::agent::builtin(agent), &what);
        let mut v = sessions();
        let s = v.iter_mut().find(|s| s.title == "问").unwrap();
        (s.question, s.agent, s.asked) = (ask.into(), agent, Some(crate::data::Asked { what, offer }));
        v
    }

    fn shown_of(app: &App) -> answer::Asking {
        app.all.iter().find(|s| s.title == "问").unwrap().asked.clone().unwrap().what
    }

    #[test]
    fn s_on_a_single_choice_lists_its_options_and_a_number_answers_it() {
        let mut app = fixture();
        app.set_sessions(asking("choice", "用哪种方式存?", &["SQLite", "JSON 文件", "先不存"], Agent::Claude));
        select(&mut app, "问");
        shows(&reader_rows(&mut app, 100, 24), &[caps("⏎ 打开 • s 回答 • z 输出").as_str()]);
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(app.confirm, Some(Confirm::Answer { .. })) && app.prompt.is_none());
        let rows = reader_rows(&mut app, 100, 24);
        shows(&rows, &["回答 · 问", "Esc 关闭", "用哪种方式存?", "1 SQLite", "2 JSON 文件", "3 先不存", "1–3 选 • ⏎ 进去回答"]);
        // The preview beside it isn't covered
        assert!(crate::ui::tests::beside(&mut app, 100, 24)[0].starts_with("◆ 问"), "{rows:#?}");
        press(&mut app, KeyCode::Char('2'));
        assert!(app.confirm.is_none());
        let job = app.take_job().unwrap();
        assert_eq!(job, Job::Answer { t: target("问"), reply: "2".into(), shown: shown_of(&app) });
        let Job::Answer { t, reply, .. } = &job else { unreachable!() };
        app.job_done(&job, Outcome::Done(ops::answered_msg(&t.title, reply, "2 JSON 文件")), sessions());
        assert_eq!(said(&app), "已回答「问」:2 JSON 文件");
        // A number past the options only closes it
        app.set_sessions(asking("choice", "用哪种方式存?", &["SQLite", "JSON 文件", "先不存"], Agent::Claude));
        keys(&mut app, "s4");
        assert!(app.confirm.is_none() && app.take_job().is_none());
    }

    #[test]
    fn a_permission_prompt_is_allowed_with_y_and_denied_with_n_where_the_tool_has_the_keys() {
        for agent in [Agent::Claude, Agent::Codex, Agent::Opencode] {
            let mut app = fixture();
            app.set_sessions(asking("permission", "Bash: cargo test -p aca-tui", &[], agent));
            select(&mut app, "问");
            press(&mut app, KeyCode::Char('s'));
            shows(&reader_rows(&mut app, 100, 24), &["Bash: cargo test -p aca-tui", "y 允许 • n 拒绝 • ⏎ 进去回答"]);
            press(&mut app, KeyCode::Char('y'));
            assert_eq!(
                app.take_job().map(|j| if let Job::Answer { reply, .. } = j { reply } else { String::new() }).as_deref(),
                Some("yes")
            );
            app.busy = None;
            keys(&mut app, "sn");
            assert_eq!(
                app.take_job().map(|j| if let Job::Answer { reply, .. } = j { reply } else { String::new() }).as_deref(),
                Some("no")
            );
        }
        assert_eq!(ops::answered_msg("问", "yes", "yes"), "已允许「问」");
        assert_eq!(ops::answered_msg("问", "no", "no"), "已拒绝「问」");
        // A tool whose prompt keys aren't known: only going in
        let mut app = fixture();
        app.set_sessions(asking("permission", "Bash: cargo test", &[], Agent::Pi));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        let rows = reader_rows(&mut app, 100, 24);
        shows(&rows, &["Bash: cargo test", "⏎ 进去回答"]);
        assert!(!rows.iter().any(|r| r.contains("y 允许")), "{rows:#?}");
    }

    #[test]
    fn what_can_only_be_answered_in_its_own_screen_offers_just_going_in() {
        let several = {
            let mut v = asking("choice", "两个问题", &["A", "B"], Agent::Claude);
            let a = v.iter_mut().find(|s| s.title == "问").unwrap().asked.as_mut().unwrap();
            a.what.multi = true;
            a.offer = answer::offer(aca_records::agent::builtin(Agent::Claude), &a.what);
            v
        };
        for v in [several, asking("", "它在问什么", &[], Agent::Claude)] {
            let mut app = fixture();
            app.set_sessions(v);
            select(&mut app, "问");
            for k in ['1', 'y', 'n'] {
                press(&mut app, KeyCode::Char('s'));
                let rows = reader_rows(&mut app, 100, 24);
                shows(&rows, &["回答 · 问", "⏎ 进去回答"]);
                assert!(!rows.iter().any(|r| r.contains("选 •") || r.contains("y 允许")), "{rows:#?}");
                press(&mut app, KeyCode::Char(k));
                assert!(app.confirm.is_none() && app.take_job().is_none(), "{k}");
            }
        }
        // Not registered (a claude started by hand in a plain tmux session): its question, and going in
        let mut v = sessions();
        let s = v.iter_mut().find(|s| s.title == "问").unwrap();
        (s.deck_id, s.profile, s.question) = (String::new(), String::new(), "Do you want to proceed?".into());
        let mut app = App::new(String::new(), v);
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        shows(&reader_rows(&mut app, 100, 24), &["Do you want to proceed?", "⏎ 进去回答"]);
    }

    #[test]
    fn enter_in_the_answer_box_goes_in_and_esc_q_s_or_another_key_only_close_it() {
        let mut app = fixture();
        app.set_sessions(asking("choice", "选哪个?", &["A", "B"], Agent::Claude));
        select(&mut app, "问");
        keys(&mut app, "s⏎");
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert_eq!(app.attach.take().as_deref(), Some("问"));
        for k in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('s'), KeyCode::Char('j')] {
            press(&mut app, KeyCode::Char('s'));
            assert!(app.confirm.is_some());
            app.msg = None;
            press(&mut app, k);
            assert!(app.confirm.is_none() && app.take_job().is_none() && app.attach.is_none() && !app.quit, "{k:?}");
            // Closing on purpose says nothing; another key says nothing was answered
            assert_eq!(said(&app), if k == KeyCode::Char('j') { "未回答" } else { "" }, "{k:?}");
        }
        assert_eq!(app.current().unwrap().title, "问"); // j closed the box, it didn't move
    }

    #[test]
    fn a_question_that_changed_or_went_away_is_not_answered() {
        let opts = ["A", "B"];
        let mut app = fixture();
        app.set_sessions(asking("choice", "问题 A", &opts, Agent::Claude));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        // A refresh: it asks B now → the box closes by itself
        app.set_sessions(asking("choice", "问题 B", &opts, Agent::Claude));
        assert!(app.confirm.is_none());
        assert_eq!(said(&app), "问题变了,没回答");
        // Shown A; the list learns of B only as the key is pressed: nothing pressed
        app.set_sessions(asking("choice", "问题 A", &opts, Agent::Claude));
        press(&mut app, KeyCode::Char('s'));
        let fresh = asking("choice", "问题 B", &opts, Agent::Claude);
        app.all.iter_mut().find(|s| s.title == "问").unwrap().asked = fresh.into_iter().find(|s| s.title == "问").unwrap().asked;
        press(&mut app, KeyCode::Char('1'));
        assert!(app.take_job().is_none());
        assert_eq!(said(&app), "问题变了,没回答");
        // aca-core found it changed by the time it pressed: the same words
        app.set_sessions(asking("choice", "问题 A", &opts, Agent::Claude));
        keys(&mut app, "s1");
        let job = app.take_job().unwrap();
        app.job_done(&job, Outcome::Failed(ops::changed_msg()), sessions());
        assert_eq!(said(&app), "问题变了,没回答");
        // It's running again (answered elsewhere): the box goes at the next refresh
        app.set_sessions(asking("choice", "问题 A", &opts, Agent::Claude));
        press(&mut app, KeyCode::Char('s'));
        let mut v = sessions();
        v.iter_mut().find(|s| s.title == "问").unwrap().state = State::Running;
        app.set_sessions(v);
        assert!(app.confirm.is_none());
    }

    #[test]
    fn the_answer_box_leaves_the_preview_in_view() {
        let mut v = asking("permission", "Bash: cargo test -p aca-tui", &[], Agent::Claude);
        v.iter_mut().find(|s| s.title == "问").unwrap().screen =
            vec!["⏺ Bash(cargo test -p aca-tui)".into(), String::new(), " Do you want to proceed?".into(), " ❯ 1. Yes".into()];
        let mut app = App::new(String::new(), v);
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        // Beside (desktop), below (a phone): the box goes over the list, the command it wants to run stays readable
        for (w, h) in [(120, 24), (46, 20)] {
            let rows = reader_rows(&mut app, w, h);
            shows(&rows, &["回答 · 问", "y 允许", "⏺ Bash(cargo test -p aca-tui)", "Do you want to proceed?", "❯ 1. Yes"]);
            let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap();
            if w == 46 {
                assert!(at("回答 · 问") < at("⏺ Bash"), "{rows:#?}");
            }
        }
    }

    #[test]
    fn the_answer_box_fits_a_phone_and_wraps_long_options() {
        let long = "把 runtime 那层单独拆成一个 crate,store 先不动,等边界清楚了再说";
        let mut app = fixture();
        app.set_sessions(asking("choice", "接下来怎么拆?这一步要不要连 store 一起动?", &[long, "先不拆"], Agent::Claude));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        let rows = reader_rows(&mut app, 46, 20);
        assert!(rows.iter().all(|r| r.width() <= 46), "{rows:#?}");
        shows(&rows, &["回答 · 问", "1 把 runtime", "2 先不拆", "1–2 选"]);
        // The option goes on under its own text, nothing cut off
        let body: String = rows.iter().map(|r| r.replace(['│', ' '], "")).collect();
        assert!(body.contains(&long.replace(' ', "")), "{rows:#?}");
    }

    #[test]
    fn the_keys_sheet_calls_s_answer_on_a_row_waiting_for_you() {
        let mut app = fixture();
        app.set_sessions(asking("choice", "选哪个?", &["A", "B"], Agent::Claude));
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('?'));
        let rows = reader_rows(&mut app, 120, 60);
        assert!(rows.iter().any(|r| r.contains("s ") && r.contains("回答")) && !rows.iter().any(|r| r.contains("回话")), "{rows:#?}");
        press(&mut app, KeyCode::Esc);
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('?'));
        let rows = reader_rows(&mut app, 120, 60);
        assert!(rows.iter().any(|r| r.contains("回话")) && !rows.iter().any(|r| r.contains("s 回答")), "{rows:#?}");
    }

    /// 「闲」 with a second window running a built-in agent (an agent team's teammate) and a third running a shell; both idle
    fn with_a_teammate() -> Vec<Session> {
        let mut v = sessions();
        let idle = v.iter().find(|s| s.title == "闲").unwrap().clone();
        let window = |win: u32, title: &str, agent: Agent| Session {
            name: format!("{}\u{1}%{win}", idle.name),
            title: title.into(),
            deck_id: String::new(),
            pane: format!("%{win}"),
            agent,
            sub: Some(crate::data::Sub { parent: idle.name.clone(), parent_title: idle.title.clone(), win, pane: 0, split: false }),
            ..idle.clone()
        };
        v.push(window(2, "队友", Agent::Claude));
        v.push(window(3, "zsh", Agent::None));
        v
    }

    #[test]
    fn a_window_with_a_built_in_agent_is_replied_to_in_its_own_pane() {
        let mut app = App::new(String::new(), with_a_teammate());
        select(&mut app, "队友");
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(app.prompt.as_ref().unwrap().ask, Ask::Reply { .. }), "{:?}", app.msg);
        typ(&mut app, "你那边跑完了吗");
        // Its own note, not the session's
        assert_eq!(app.notes.get("tmux:闲\u{1}%2"), Some("你那边跑完了吗"));
        assert_eq!(app.notes.get("id-闲"), None);
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        let Job::Send { t, pane, text, key } = &job else { panic!("{job:?}") };
        // Into that pane: no session id, so not through session::say (which would type into the session's own pane)
        assert_eq!((t.id.as_str(), pane.as_str(), text.as_str(), key.as_str()), ("", "%2", "你那边跑完了吗", "tmux:闲\u{1}%2"));
        app.job_done(&job, Outcome::Done("已发给「队友」".into()), with_a_teammate());
        assert_eq!(app.notes.get("tmux:闲\u{1}%2"), None);
        // ⏎ goes by this window's own latest state, not its session's: the window asking, the session idle
        press(&mut app, KeyCode::Char('s'));
        typ(&mut app, "继续");
        let mut v = with_a_teammate();
        v.iter_mut().find(|s| s.title == "队友").unwrap().state = State::Ask;
        app.set_sessions(v);
        press(&mut app, KeyCode::Enter);
        assert!(app.take_job().is_none());
        assert!(said(&app).contains("正在等待回应"), "{}", said(&app));
        // The session asking, the window idle: it's sent
        let mut v = with_a_teammate();
        v.iter_mut().find(|s| s.title == "闲").unwrap().state = State::Ask;
        app.set_sessions(v);
        select(&mut app, "队友");
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Enter);
        let job = app.take_job().unwrap();
        assert!(matches!(&job, Job::Send { pane, .. } if pane == "%2"), "{job:?}");
        // A shell in the window: no telling what's in front, nothing opens
        select(&mut app, "zsh");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.prompt.is_none());
        assert_eq!(said(&app), "无法回话:不是内置 agent");
    }

    #[test]
    fn a_window_waiting_for_you_only_offers_going_in() {
        let mut v = with_a_teammate();
        let w = v.iter_mut().find(|s| s.title == "队友").unwrap();
        (w.state, w.question) = (State::Ask, "Do you want to proceed?".into());
        let mut app = App::new(String::new(), v);
        select(&mut app, "队友");
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(app.confirm, Some(Confirm::Answer { .. })));
        let rows = reader_rows(&mut app, 100, 24);
        shows(&rows, &["回答 · 队友", "Do you want to proceed?", "⏎ 进去回答"]);
        assert!(!rows.iter().any(|r| r.contains("y 允许") || r.contains("选 •")), "{rows:#?}");
        // A refresh with the same question keeps it open (it's found by its own row); 1 / y only close it
        let again = app.all.clone();
        app.set_sessions(again);
        assert!(app.confirm.is_some());
        press(&mut app, KeyCode::Char('y'));
        assert!(app.confirm.is_none() && app.take_job().is_none());
        // ⏎ goes in, to that window
        keys(&mut app, "s⏎");
        assert_eq!((app.attach.take().as_deref(), app.focus.take().as_deref()), (Some("闲"), Some("%2")));
    }
}
