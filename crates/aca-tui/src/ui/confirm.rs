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
    /// s: send the note to it. pane: which pane to send to; key: the note's key
    Send {
        t: Target,
        pane: String,
        key: String,
        text: String,
    },
    /// A: adopt this plain tmux session. plan: what was recognized and how it will be registered (that's what's shown, and what y
    /// carries out)
    Adopt {
        t: Target,
        plan: Box<aca_core::adopt::Plan>,
    },
    /// d / D / R after v multi-select: on these sessions (in list order). notes: what to watch out for, written in the box (how many
    /// running ones will be interrupted…)
    Many {
        act: Batch,
        ts: Vec<Target>,
        notes: Vec<String>,
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
    /// `f`: what to call the forked session (what follows is what a Claude session must check before forking, see `Job::Fork`)
    Fork(Target),
    /// ^R in S: the workspace's new name
    Space(char),
    /// Settings page: the default directory of `^N`
    GroupDir,
    /// `r` on a group: the new group name (may include parents); after Enter, go on to change the default directory
    GroupName(String),
    /// Then fill in its default directory: group is renamed to to, was is the old default directory (empty = none recorded)
    GroupHome { group: String, to: String, was: String },
}

/// q / Q pressed, waiting for the next key
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pending {
    /// q: which workspace to switch to
    Switch,
    /// Q: which workspace to send the highlighted session to
    Send,
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
    /// busy, on a fold row, or a plain tmux session: say so, return None
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
            self.flash(t!("已停止 · ⏎ 重新启动", "already stopped · ⏎ starts it"));
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
            self.flash(t!("已停止 · ⏎ 重新启动", "already stopped · ⏎ starts it"));
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

    /// A group was renamed (an undo renaming it back counts): whatever the interface remembers of it — the group shown, folded ones,
    /// the group heading the highlight is on, workspace records — follows
    pub(super) fn group_renamed(&mut self, from: &str, to: &str) {
        let r = |g: &str| groups::renamed(g, from, to);
        if let Some(g) = self.group.as_deref().and_then(r) {
            self.group = Some(g);
        }
        if let Some(g) = self.head.as_deref().and_then(r) {
            self.head = Some(g);
        }
        self.folded = std::mem::take(&mut self.folded).into_iter().map(|g| r(&g).unwrap_or(g)).collect();
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
        if more.iter().all(|(_, g)| *g == from) {
            cands.retain(|g| *g != from);
        }
        let real = aca_core::paths::real;
        let mine = real(&here);
        let mut rows: Vec<ops::GroupRow> = cands
            .into_iter()
            .map(|group| {
                let home = hints.iter().find(|(g, d)| *g == group && !d.is_empty()).map(|(_, d)| d.clone());
                let dir = home.unwrap_or_else(|| self.usual_dir(&group));
                let same = !dir.is_empty() && !here.is_empty() && real(&dir) == mine;
                ops::GroupRow { group, dir, same }
            })
            .collect();
        // Stable sort: the rest keep their order
        rows.sort_by_key(|r| !r.same);
        GroupPick { t, from, here, rows, groups, more }
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
        if self.current().is_none_or(|s| !matches!(s.agent, Agent::Claude | Agent::Codex | Agent::Pi | Agent::Opencode)) {
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

    /// Back from picking a group in fzf: the selected one or a newly typed name
    pub fn group_picked(&mut self, p: GroupPick, got: Result<Option<String>, String>) {
        match got {
            Ok(Some(g)) => match ops::clean_group(&g) {
                Some(to) if p.more.is_empty() && to.eq_ignore_ascii_case(&p.from) => self.flash(&tr!("已在 {to} 中", "already in {to}")),
                Some(to) if p.more.is_empty() => {
                    self.stay_on_move(&p.t.name, &to);
                    self.job = Some(Job::Move { t: p.t, from: p.from, to, groups: p.groups });
                }
                // Multi-selected ones move together: ones already in that group stay
                Some(to) => {
                    let moves: Vec<(Target, String)> =
                        std::iter::once((p.t, p.from)).chain(p.more).filter(|(_, from)| !to.eq_ignore_ascii_case(from)).collect();
                    if moves.is_empty() {
                        return self.flash(&tr!("均已在 {to} 中", "all already in {to}"));
                    }
                    for (t, _) in &moves {
                        self.stay_on_move(&t.name, &to);
                    }
                    let jobs = moves.into_iter().map(|(t, from)| Job::Move { t, from, to: to.clone(), groups: p.groups.clone() }).collect();
                    self.drop_picks();
                    self.job = Some(Job::Many(jobs));
                }
                None => {}
            },
            Ok(None) => {}
            Err(e) => self.flash(&e),
        }
    }

    /// e: write a note, the existing one pre-filled
    pub(super) fn ask_note(&mut self) {
        let Some((key, title)) = self.note_target() else { return };
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        self.prompt = Some(Prompt { ask: Ask::Note { key, title, orig: text.clone() }, text, hint: String::new() });
    }

    /// Whose note e / ^G writes: (key, what it's called). On a group (heading /
    /// fold row) it's the group's, otherwise the highlighted session's
    fn note_target(&self) -> Option<(String, String)> {
        match self.group_row() {
            Some(g) => Some((group_key(g), group_label(g).to_string())),
            None => self.current().map(|s| (notes::key(s), s.title.clone())),
        }
    }

    /// E / Shift+Enter: go in with the note — pasted into its input box before going in (not sent), deleted once filled. No note = Enter.
    /// Only fills Claude / Codex (other programs, like vim, would take the pasted text as commands); not while waiting for your choice (!)
    /// — the text would be taken as an option. If it can't fill, say why and don't go in (so you don't think it filled)
    pub(super) fn enter_with_note(&mut self) {
        if self.on_group_row() {
            return self.toggle_fold();
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.enter() };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("未附带便签:{w}", "didn't go in with the note: {w}"));
        }
        self.fill = Some((key, text));
        self.enter();
    }

    /// s: send the note to it directly (pasted into the input box, then Enter), you stay in the list. No note sends nothing;
    /// with one it still asks first, against slips
    pub(super) fn ask_send(&mut self) {
        self.digits.clear();
        if let Some(b) = self.doing() {
            let m = tr!("请稍候:{b}", "still busy: {b}");
            return self.flash(&m);
        }
        if self.on_group(t!("发便签", "send the note")) {
            return;
        }
        let Some(s) = self.current() else { return };
        let key = notes::key(s);
        let Some(text) = self.notes.get(&key).map(String::from) else { return self.flash(t!("没有便签", "no note")) };
        if let Some(w) = cant_fill(s) {
            return self.flash(&tr!("未发送:{w}", "not sent: {w}"));
        }
        let t = Target { name: s.name.clone(), title: s.title.clone(), profile: s.profile.clone(), id: s.deck_id.clone() };
        let pane = if s.pane.is_empty() { format!("={}:", s.family()) } else { s.pane.clone() };
        self.confirm = Some(Confirm::Send { t, pane, key, text });
    }

    /// ^G (in the list): write the highlighted session's / group's note in the editor
    pub(super) fn note_in_editor(&mut self) {
        let Some((key, title)) = self.note_target() else { return };
        let text = self.notes.get(&key).unwrap_or_default().to_string();
        self.edit_note = Some(NoteEdit { key, title, orig: text.clone(), text, from_prompt: false });
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
            self.prompt = Some(Prompt { ask: Ask::Note { key: ed.key, title: ed.title, orig: ed.orig }, text, hint });
        } else if let Some(e) = err {
            self.flash(&e);
        } else {
            self.note_done(ed.key, ed.title, ed.orig);
            self.note_saved_flash(&text);
        }
    }

    /// Save the note whenever it changes (every keystroke: when a phone disconnects aca is killed outright, it never gets to exit)
    pub(super) fn save_note(&mut self, key: &str, text: &str) {
        if let Err(e) = self.notes.set(key, text) {
            self.flash(&tr!("便签保存失败:{e}", "note not saved: {e}"));
        }
        self.apply_pins();
    }

    /// The note is done: if it differs from before writing started, record an undo step
    fn note_done(&mut self, key: String, title: String, orig: String) {
        if self.notes.get(&key).unwrap_or_default() != orig {
            self.remember(Undo::Note { key, title, text: orig });
        }
    }

    fn note_saved_flash(&mut self, text: &str) {
        self.flash(if text.trim().is_empty() { t!("便签已清空", "note cleared") } else { t!("便签已保存", "note saved") });
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
        if let Ask::Note { key, title, orig } = &p.ask {
            // Note: Enter is a newline, Esc leaves (already saved long ago), ^G hands it to the editor
            match k.code {
                KeyCode::Esc => {
                    let (key, title, orig) = (key.clone(), title.clone(), orig.clone());
                    let text = p.text.trim_end().to_string();
                    self.prompt = None;
                    self.save_note(&key, &text);
                    self.note_done(key, title, orig);
                    return self.note_saved_flash(&text);
                }
                KeyCode::Char('g') if ctrl => {
                    let ed =
                        NoteEdit { key: key.clone(), title: title.clone(), orig: orig.clone(), text: p.text.clone(), from_prompt: true };
                    self.prompt = None;
                    self.edit_note = Some(ed);
                    return;
                }
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
            KeyCode::Tab if matches!(p.ask, Ask::GroupDir | Ask::GroupHome { .. }) => match create::complete_path(&p.text) {
                Some(t) => p.text = t,
                None => return,
            },
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
            // Enter in a note is a newline, never gets here
            Ask::Note { .. } => {}
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
            Ask::GroupName(group) => self.group_name_given(group, text),
            Ask::GroupHome { group, to, was } => self.group_home_given(group, to, was, text),
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
                group_label(&g.group),
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
                notes.push(t!("停止程序，保留窗格 · ⏎ 重新启动", "the program stops, the pane stays, ⏎ starts it again").into());
                notes.extend(next.iter().map(|n| tr!("{n}将成为首个", "{n} moves up to be first")));
            }
            let (head, act) = if *restart {
                (t!("重启此窗格？", "Restart this pane?"), t!("重启", "restart"))
            } else {
                (t!("停止此窗格？", "Stop this pane?"), t!("停止", "stop"))
            };
            (row.as_str(), t.title.as_str(), theme::pal().accent, head, String::new(), notes, act)
        }
        Confirm::Send { t, text, .. } => {
            // What gets sent: the note's text, only the first few lines when too long
            let mut lines: Vec<&str> = text.lines().collect();
            let more = lines.len() > 4;
            lines.truncate(4);
            let mut what = lines.join("\n");
            if more {
                what.push_str("\n…");
            }
            (t.name.as_str(), t.title.as_str(), theme::pal().accent, t!("发送便签？", "Send the note?"), what, vec![], t!("发送", "send"))
        }
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
    };
    let s = all.iter().find(|s| s.name == name);
    let head_style = if border == danger { red } else { theme::accent() };
    modal(f, 60, border, |w| {
        let (ic, sty) = match c {
            Confirm::Group(_) => ("▾", theme::faint()),
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

/// Default title when forking (same as what `session::fork` gives when no title is passed)
fn fork_title(title: &str) -> String {
    tr!("{title} (分叉)", "{title} (fork)")
}

/// Input popup for rename / new group / note (when the preview is off)
pub(super) fn draw_prompt(f: &mut Frame, p: &Prompt) {
    let (title, keys): (String, &[(&str, &str)]) = match &p.ask {
        Ask::Rename(_) => (t!("重命名", "Rename").into(), &[("⏎", t!("确定", "ok"))]),
        Ask::NewGroup => (t!("新建分组", "New group").into(), &[("⏎", t!("下一步", "next"))]),
        // A note's Esc means done (not cancel): like the bottom bar when writing a note in the preview, first in the key line
        Ask::Note { title, .. } => (
            tr!("便签 · {title}", "Note · {title}"),
            &[("Esc", t!("保存", "done")), ("⏎", t!("换行", "newline")), ("^G", t!("编辑器", "editor"))],
        ),
        Ask::Fork(t) => (tr!("分叉 · {}", "Fork · {}", t.title), &[("⏎", t!("分叉", "fork")), ("^U", t!("清空重写", "clear and rename"))]),
        Ask::Space(c) => (tr!("重命名工作区 {c}", "Rename workspace {c}"), &[("⏎", t!("确定", "ok"))]),
        Ask::GroupDir => (t!("新分组目录", "new group directory").into(), &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete"))]),
        Ask::GroupName(g) => (tr!("改分组 · {g}", "Edit group · {g}"), &[("⏎", t!("下一步", "next"))]),
        Ask::GroupHome { to, .. } => (
            tr!("改分组 · {to} · 默认目录", "Edit group · {to} · default directory"),
            &[("⏎", t!("确定", "ok")), ("Tab", t!("补全", "complete")), ("^U", t!("清空 = 不设置", "empty = forget"))],
        ),
    };
    // Cancel's Esc goes to the right of the title, like other popups
    let esc = if matches!(p.ask, Ask::Note { .. }) { vec![] } else { esc_hint(t!("取消", "cancel")) };
    modal(f, 64, theme::pal().accent, |w| {
        let mut out = vec![modal_title(&title, theme::accent(), esc.clone(), w), Line::raw("")];
        if let Ask::Note { .. } = p.ask {
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
        at, ctrl, fixture, fun_in_h, hints, jump_to_group, keys, life_and_work, mk, on_ding, order, press, reader_rows, rehome_tools,
        row_of, said, select, sessions, shows, space, target, titles, tool, typ, with_children, with_grandchildren, with_windows,
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

    #[test]
    fn x_lists_the_other_accounts_with_their_quota_and_login_and_enter_switches() {
        let mut app = fixture().with_tools(rehome_tools).with_rehome(rehome_quotas, rehome_ask, false);
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
                "› 1 cw    ● 已登录      5h 12% · 7d 40%",
                "  2 cw2   ○ 未登录",
                "⏎ 切换",
            ],
        );
        assert!(!rows.iter().any(|r| r.contains(" cx ") || r.contains("将中断")), "{rows:#?}");
        // Can't switch to one that isn't logged in
        press(&mut app, KeyCode::Char('2'));
        assert!(app.job.is_none() && !app.overlays.is_empty());
        assert_eq!(app.msg.as_ref().unwrap().0, "cw2 未登录 · a 登录");
        // ⏎ switches to the highlighted one, the box closes
        press(&mut app, KeyCode::Up);
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
        // Right below its own session, ordered by window number, pane number; the session idle for a day but a window running:
        // the whole family stays
        assert_eq!(order(&app), ["问", "跑", "闲", "htop", "zsh", "错", "构建"]);
        let rows = reader_rows(&mut app, 120, 30);
        // A session's windows are joined with ├, the last one with └
        shows(&rows, &["├ ○ #1.1 htop", "└ ○ #2 zsh", "└ ● #3 构建"]);
        // Window numbers of different lengths, the time column still lines up
        let col = |t: &str| {
            let r = row_of(&rows, t);
            r[..r.find(t).unwrap()].width()
        };
        assert_eq!((col(" 10秒"), col(" 30秒")), (col(" 20秒"), col(" 20秒")), "{rows:#?}");
        // The account follows the title, colored by tool
        let t = rows.iter().find(|r| r.contains("#3 构建")).unwrap();
        assert!(t.contains(" cx "), "{t}");
        // The count in the section header counts sessions only
        assert!(rows.iter().any(|r| r.contains("work  3")), "{rows:#?}");
        // w: an idle session with a running window counts too
        press(&mut app, KeyCode::Char('w'));
        assert!(order(&app).contains(&"构建".to_string()) && order(&app).contains(&"错".to_string()));
        press(&mut app, KeyCode::Char('w'));
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
        shows(&reader_rows(&mut app, 100, 30), &["停止此窗格？", "停止程序，保留窗格 · ⏎ 重新启动", "y 停止"]);
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
        assert_eq!(said(&app), "已停止 · ⏎ 重新启动");
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
        assert_eq!(said(&app), "已停止 · ⏎ 重新启动");
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
        // Last day: follows its parent session (and its windows); a child idle for three days is still hidden
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh"]);
        for (w, two) in [(46, false), (120, true)] {
            let rows = reader_rows(&mut app, w, 30);
            assert!(row_of(&rows, "子跑").contains("└ ● 子跑"), "{w}: {rows:#?}");
            assert!(row_of(&rows, "zsh").contains("  └ ○ #2 zsh"), "{w}: {rows:#?}");
            // Count in the section header: a child session is a session too
            assert!(rows.iter().any(|r| r.contains("work  3")), "{two}: {rows:#?}");
        }
        press(&mut app, KeyCode::Char('a'));
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
        assert_eq!(order(&app), ["问", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙"]);
        for w in [46, 120] {
            let rows = reader_rows(&mut app, w, 30);
            for want in ["└ ● 子跑", "  ├ ○ #2 zsh", "  └ ● 孙", "    ├ ○ #3 top", "    └ ● 曾孙"] {
                assert!(row_of(&rows, want).contains(want), "{w}: {rows:#?}");
            }
        }
        // 子跑 has a sibling after it (子旧): its level's vertical line runs down to the last row of its family
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(order(&app), ["问", "未读", "跑", "闲", "子跑", "zsh", "孙", "top", "曾孙", "子旧", "错"]);
        let rows = reader_rows(&mut app, 46, 30);
        let wants = ["├ ● 子跑", "│ ├ ○ #2 zsh", "│ └ ● 孙", "│   ├ ○ #3 top", "│   └ ● 曾孙", "└ ○ 子旧"];
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
        // After stopping: still in the list, marked ■; the tmux activity time is gone, keep using what was seen before stopping,
        // so it isn't hidden by "over 1 day"
        let fresh: Vec<Session> = real(sessions())
            .into_iter()
            .map(|mut s| {
                if s.title == "闲" {
                    (s.state, s.idle_for, s.active_at) = (State::Stopped, 9_000_000, 0);
                }
                s
            })
            .collect();
        app.job_done(&job, Outcome::Done("已停止「闲」· ⏎ 重新启动".into()), fresh);
        let s = app.current().unwrap();
        assert_eq!((s.title.as_str(), s.state), ("闲", State::Stopped));
        assert!(s.idle_for < 60, "{}", s.idle_for);
        assert!(titles(&app).contains(&"闲".to_string()));
        shows(&reader_rows(&mut app, 120, 16), &["已停止", "⏎ 启动 · d 删除"]);
        // D again: already stopped; ⏎ starts it first, then goes in, no direct attach
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已停止 · ⏎ 重新启动");
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
        // Not in w (open ones)
        press(&mut app, KeyCode::Char('w'));
        assert!(!titles(&app).contains(&"闲".to_string()));
    }

    #[test]
    fn a_crashed_one_is_a_cross_but_starts_like_a_stopped_one() {
        let mut v = sessions();
        v[3].state = State::Crashed; // 「闲」: the last turn ended in an error, gone from tmux too
        v[4].state = State::Idle; // 「错」: leave just one with an error, to see whether the top bar counts it
        let mut app = App::new(String::new(), v);
        select(&mut app, "闲");
        assert!(row_of(&reader_rows(&mut app, 46, 16), "闲").contains("✕ 闲"));
        shows(&reader_rows(&mut app, 120, 16), &["出错 · 已停止", "⏎ 启动 · d 删除"]);
        assert!(!top_bar(&app, 80).to_string().contains("出错")); // one that isn't running doesn't count as an error
        press(&mut app, KeyCode::Char('D'));
        assert!(app.confirm.is_none());
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.start.take(), app.attach.take()), (Some(target("闲")), None));
        press(&mut app, KeyCode::Char('w')); // nor as open
        assert!(!titles(&app).contains(&"闲".to_string()));
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
        app.group_picked(p, Ok(Some(" side proj/part ".into())));
        let all = ["Zeta", "fun", "work"].map(String::from).to_vec();
        assert_eq!(app.job.take(), Some(Job::Move { t: target("跑"), from: "work".into(), to: "side-proj/part".into(), groups: all }));
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(Some("Work".into())));
        assert!(app.job.is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "已在 Work 中");
        press(&mut app, KeyCode::Char('m'));
        let p = app.pick_group.take().unwrap();
        app.group_picked(p, Ok(None)); // Esc pressed in fzf
        assert!(app.job.is_none());
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
        assert_eq!(titles(&app), ["跑", "问", "闲"]);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(0));
        // One row per state: at the start the key that picks it directly, icon, name, count; row 0 is all
        let rows = reader_rows(&mut app, 60, 16);
        shows(
            &rows,
            &["按状态筛选", "Esc 关闭", "␣", "全部", "!  ●", "运行中", "@  ◆◐", "等待回应 · 未读", "%  ☾", "自动停止", "⏎ 筛选 • ␣ 全部"],
        );
        // The row's key picks directly and closes the small list; reopened, the highlight is on the picked one
        press(&mut app, KeyCode::Char('!'));
        assert_eq!((app.only_menu, app.status), (None, Some(Pick::Run)));
        assert_eq!(titles(&app), ["跑"]);
        press(&mut app, KeyCode::Char('F'));
        assert_eq!(app.only_menu, Some(1));
        // ⏎ on the picked one = cancel
        press(&mut app, KeyCode::Enter);
        assert_eq!((app.only_menu, app.status), (None, None));
        // ↓ to "needs you · unread" ⏎; Space = all; Esc only closes the small list
        keys(&mut app, "Fjj⏎");
        assert_eq!((app.status, titles(&app)), (Some(Pick::Wait), vec!["问".to_string()]));
        keys(&mut app, "F ");
        assert_eq!(app.status, None);
        keys(&mut app, "F⎋");
        assert!(app.only_menu.is_none() && !app.quit);
        // f only forks, no longer doubles as F
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('f'));
        assert!(app.prompt.is_some() && app.only_menu.is_none());
    }

    #[test]
    fn q_switches_capital_q_sends_brackets_cycle_and_h_stays_hidden() {
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
        keys(&mut app, "]"); // Skips h: only 1 and l
        assert_eq!((app.space, titles(&app)), ('l', vec!["zsh".to_string(), "子".into(), "生活".into()]));
        assert!(top_bar(&app, 80).to_string().contains(" l ") && top_bar(&app, 80).to_string().contains("1 未读"));
        keys(&mut app, "]");
        assert_eq!(app.space, '1');
        keys(&mut app, "[");
        assert_eq!(app.space, 'l');
        keys(&mut app, "qh");
        assert_eq!(titles(&app), ["私"]);
        keys(&mut app, "]"); // Leaving h carries on forward
        assert_eq!(app.space, 'l');
        keys(&mut app, "q1");
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
        assert!(rows.last().unwrap().trim_start().starts_with("Esc 保存"), "{rows:#?}");
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
    fn e_on_a_group_writes_the_groups_own_note() {
        let mut app = fixture();
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('e'));
        typ(&mut app, "这一组:先把测试补齐");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "再看性能");
        assert_eq!(app.notes.get("group:work"), Some("这一组:先把测试补齐\n再看性能"));
        // Written in the group's preview, cursor at the end; in a popup when the preview is off
        shows(&reader_rows(&mut app, 100, 24), &["✎ 便签", "再看性能▏"]);
        app.preview = Preview::Off;
        shows(&reader_rows(&mut app, 100, 24), &["便签 · work", "再看性能▏"]);
        app.preview = Preview::Auto;
        press(&mut app, KeyCode::Esc);
        assert_eq!(said(&app), "便签已保存");
        // ✎ after the group name, shown in the preview; the group's sessions have no note of their own
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("▾ work") && r.contains('✎')), "{rows:#?}");
        shows(&rows, &["✎ 这一组:先把测试补齐", "再看性能"]);
        assert!(app.all.iter().filter(|s| s.group == "work").all(|s| s.group_noted && !s.noted));
        // e on a session still writes the session's own
        select(&mut app, "闲");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "");
        press(&mut app, KeyCode::Esc);
        // Same on the fold row: marked ✎, the old note prefilled; ^Z undoes the edit
        jump_to_group(&mut app, "work");
        press(&mut app, KeyCode::Char('o'));
        assert!(app.on_fold_row());
        let rows = reader_rows(&mut app, 100, 24);
        assert!(rows.iter().any(|r| r.contains("▸ work") && r.contains('✎')), "{rows:#?}");
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.prompt.as_ref().unwrap().text, "这一组:先把测试补齐\n再看性能");
        typ(&mut app, "!");
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'z');
        assert_eq!(said(&app), "已撤销修改便签「work」");
        assert_eq!(app.notes.get("group:work"), Some("这一组:先把测试补齐\n再看性能"));
        // ^G: what's written in the editor is the group's too; emptied = note deleted
        ctrl(&mut app, 'g');
        let ed = app.edit_note.take().unwrap();
        assert_eq!(ed.text, "这一组:先把测试补齐\n再看性能");
        app.note_editor_done(ed, Ok(String::new()));
        assert_eq!(app.notes.get("group:work"), None);
        assert!(!reader_rows(&mut app, 100, 24).iter().any(|r| r.contains('✎')));
    }

    #[test]
    fn only_shift_e_takes_the_note_in_and_only_when_it_is_safe() {
        let mut app = fixture();
        for t in ["闲", "跑", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的")).unwrap();
        }
        app.set_sessions(sessions());
        press(&mut app, KeyCode::Char('a')); // All (「未读」 has been quiet for five days)
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
    fn s_sends_the_note_only_after_y_and_nothing_without_one() {
        let mut app = fixture();
        for t in ["闲", "问"] {
            app.notes.set(&format!("id-{t}"), &format!("给{t}的\n第二行")).unwrap();
        }
        app.set_sessions(sessions());
        select(&mut app, "跑"); // No note: sends nothing, just says so
        press(&mut app, KeyCode::Char('s'));
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert!(app.msg.as_ref().unwrap().0.contains("没有便签"), "{:?}", app.msg);
        app.msg = None; // The message holds the bottom bar
        assert!(!reader_rows(&mut app, 120, 20).iter().any(|r| r.contains("发便签")));
        // With a note it still asks first: to whom and what (multiple lines kept as lines); s shows in the bottom bar only with a note
        select(&mut app, "闲");
        let rows = reader_rows(&mut app, 120, 20);
        assert!(rows.iter().any(|r| r.contains("s 发送")), "{rows:#?}");
        press(&mut app, KeyCode::Char('s'));
        let rows = reader_rows(&mut app, 46, 16);
        let at = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap_or_else(|| panic!("缺「{t}」:{rows:#?}"));
        assert!(at("发送便签？") < at("给闲的") && at("给闲的") < at("第二行") && at("第二行") < at("y 发送"), "{rows:#?}");
        press(&mut app, KeyCode::Char('s')); // Pressed twice by mistake: the second one just cancels
        assert!(app.confirm.is_none() && app.take_job().is_none());
        assert_eq!(app.msg.as_ref().unwrap().0, "未发送");
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('y'));
        let job = app.take_job().unwrap();
        let Job::Send { t, pane, text, key } = &job else { panic!("{job:?}") };
        assert_eq!((t.name.as_str(), pane.as_str(), text.as_str(), key.as_str()), ("闲", "=闲:", "给闲的\n第二行", "id-闲"));
        // Once sent, the note is deleted
        app.job_done(&job, Outcome::Done("已发给「闲」".into()), sessions());
        assert_eq!(app.notes.get("id-闲"), None);
        // Waiting for your choice: text would be taken as an option, so not sent
        select(&mut app, "问");
        press(&mut app, KeyCode::Char('s'));
        assert!(app.confirm.is_none());
        assert!(app.msg.as_ref().unwrap().0.contains("等待回应"));
    }
}
