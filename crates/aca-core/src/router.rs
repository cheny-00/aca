//! A group's router: the session that hands the group's work to the right sessions for you. Which session it is, the
//! registry records ([`crate::store::Store::routers`]); here is what a new one is called and what it is told when it starts.
//!
//! Claude receives a registry handoff on startup. Codex still receives its dispatch instructions in its opening brief.
//! The desk is the scratch group's router and dispatches across projects.

use crate::paths::Res;
use crate::store::{RouterRole, Session, Store, Task, TaskState};
use crate::{t, tr};

/// SSH cages have no host-side plugin files or MCP server. Local sandboxes keep their normal account environment.
pub fn check_account(cfg: &crate::config::Config, kind: &str, account: &str) -> Res<()> {
    if kind == "claude" && crate::sandbox::cage_of(cfg, kind, account).is_some() {
        return Err(t!("SSH 沙箱无法使用 router 工具", "Router tools are unavailable in SSH sandboxes").into());
    }
    Ok(())
}

/// Serialize a Claude router's pane transitions across CLI, timer and UI processes. Posting mail uses a separate lock.
pub(crate) fn lock_shift(id: &str) -> Res<std::fs::File> {
    let dir = crate::paths::inbox_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = std::fs::File::options().create(true).append(true).open(dir.join(format!("{id}.shift"))).map_err(|e| e.to_string())?;
    file.lock().map_err(|e| e.to_string())?;
    Ok(file)
}

pub(crate) fn shift_lock(store: &Store, cfg: &crate::config::Config, s: &Session) -> Res<Option<std::fs::File>> {
    if crate::agent::tool(cfg, &s.tool).is_some_and(|t| t.kind == "claude") && store.router_role(&s.id)?.is_some() {
        return lock_shift(&s.id).map(Some);
    }
    Ok(None)
}

/// What a group's new router is called: the last level of the group's path, then "router"
pub fn title(group: &str) -> String {
    format!("{} router", group.rsplit('/').next().unwrap_or(group))
}

/// A new router's opening message. `scratch`: the group sessions that belong to no project start in (empty = there's none),
/// named as the usual place it pulls sessions over from
pub fn brief(group: &str, scratch: &str) -> String {
    let from = match scratch {
        "" => String::new(),
        s if s == group => String::new(),
        s => tr!("(比如「{s}」里的)", " (the ones in “{s}”, say)"),
    };
    tr!(
        "你是项目「{group}」的 router:用户把这个项目的事交给你,你把它分给合适的会话。你只分发,不自己动手做,也不验收。

- 这个项目的会话:aca ls --json 里 group 是 {group} 的;某一个在做什么:aca show <id>、aca output <id>
- 每件要做的事先开一张任务单:aca task add -t <名字> --criteria \"做到什么算完成\",它打印单号(#9);已有的单:aca task ls -g {group}。收到「#9 新任务」的是别人已经开好的单,不用再开
- 有合适的会话就把单交给它:aca task give <单号> <id> \"…\"(它会收到这张单和你说的话);没有就新建一个来做:aca new <工具> <目录> --parent - -g {group} -t <标题> --task <单号> --say \"…\"。--parent - 和 -g 都要写:新会话的结果直接回到用户那里,不经过你
- 一张单要等另一张:先不交,写清它在等什么,aca task set <单号> --note \"等 #9:原因\"。收到「#9 完成」时,看哪些还没交的单可以交出去了。单做完由做它的会话自己标,你不标
- 不值得开单的小事(问一句、补一句):aca msg <id> \"…\"
- 用户提到别的分组里和这个项目有关的会话{from}:用 aca move --tree <id>… {group} 拉过来,不用先问
- 要长期记住的事写进项目的笔记:aca note add \"…\";已有的:aca note ls
- 每次只回用户一两句:开了哪张单、交给了谁、拉了哪些。定不了的,用选择题问用户",
        "You are the router of project “{group}”: the user hands you this project's work and you pass it to the right session. You only dispatch: you don't do the work yourself, and you don't check it.

- This project's sessions: the ones whose group is {group} in aca ls --json; what one of them is doing: aca show <id>, aca output <id>
- Each thing to do gets a task ticket first: aca task add -t <TITLE> --criteria \"what counts as done\", which prints its number (#9); the ones there are: aca task ls -g {group}. A “#9 new task” you're told of is a ticket someone already made: don't make it again
- Hand the ticket to a session that fits: aca task give <N> <id> \"…\" (it's sent the ticket and what you said); when none does, start one on it: aca new <TOOL> <DIR> --parent - -g {group} -t <TITLE> --task <N> --say \"…\". Always write both --parent - and -g: what the new session does goes straight back to the user, not through you
- A ticket that has to wait for another: don't hand it over yet, and write what it waits for, aca task set <N> --note \"waits for #9: why\". When you're told “#9 done”, see which tickets not handed over can go out now. The session doing a ticket marks it done itself; you don't
- Small things not worth a ticket (a question, a word more): aca msg <id> \"…\"
- Sessions in other groups that the user says belong to this project{from}: pull them over with aca move --tree <id>… {group}, without asking first
- What should be remembered goes into the project's notes: aca note add \"…\"; what's there: aca note ls
- Answer the user in a sentence or two: which ticket you made, who got it, which sessions you pulled over. What you can't decide, ask the user as a multiple-choice question"
    )
}

/// The directory and tool that starting a session on this group uses.
pub fn defaults(group: &crate::store::Group, rows: &[crate::status::Row], fallback_dir: &str) -> (String, String) {
    let latest = rows.iter().filter(|r| r.group == group.path).min_by_key(|r| (std::cmp::Reverse(r.active), r.order));
    let dir = if !group.default_path.is_empty() {
        group.default_path.clone()
    } else {
        latest.map(|r| r.dir.clone()).unwrap_or_else(|| crate::paths::expand(fallback_dir))
    };
    let tool = latest.map_or_else(
        || "claude".into(),
        |r| {
            if r.account.is_empty() { r.tool.clone() } else { format!("{}:{}", r.tool, r.account) }
        },
    );
    (dir, tool)
}

pub fn session_line(row: &crate::status::Row, project_dir: &str, tasks: &[crate::store::Task], now: u64) -> String {
    let mut parts = vec![
        row.id.clone(),
        format!("{} {} {}", row.glyph(), crate::status::state_name(row.state), age(now.saturating_sub(row.active))),
        if row.account.is_empty() { row.tool.clone() } else { format!("{}:{}", row.tool, row.account) },
        row.title.clone(),
    ];
    if crate::paths::real(&row.dir) != crate::paths::real(project_dir) {
        parts.push(row.dir.clone());
    }
    parts.extend(tasks.iter().filter(|t| t.session == row.id && t.done == 0).map(|t| format!("#{}", t.n)));
    parts.join("  ")
}

fn ticket_line(t: &Task) -> String {
    let state = match t.state() {
        TaskState::Todo => t!("待交", "unassigned"),
        TaskState::Doing => t!("在做", "assigned"),
        TaskState::Done => t!("完成", "done"),
    };
    let title = t.title.lines().next().unwrap_or("");
    let mut fields = vec![format!("#{} {title}", t.n), state.to_owned()];
    if !t.session.is_empty() {
        fields.push(t.session.clone());
    }
    if let Some(line) = (if t.done == 0 { &t.note } else { &t.result }).lines().next().filter(|s| !s.is_empty()) {
        fields.push(line.to_owned());
    }
    fields.join("  ") + t.watch_label()
}

fn section(out: &mut Vec<String>, name: &str, lines: Vec<String>) {
    out.push(if lines.is_empty() { format!("{name}  {}", t!("无", "none")) } else { name.to_owned() });
    out.extend(lines);
}

/// Registry snapshot at the beginning of a Claude router conversation, and the directory the snapshot describes.
pub(crate) fn handoff(
    store: &Store,
    role: RouterRole,
    session: &Session,
    now: u64,
    say: Option<&str>,
    mail: &[crate::inbox::Msg],
) -> Res<(String, String)> {
    let rows = crate::status::peek(store)?;
    let groups = store.groups()?;
    let group =
        groups.iter().find(|g| g.path == session.group).ok_or_else(|| tr!("找不到项目 {}", "Project not found: {}", session.group))?;
    let (dir, _) = defaults(group, &rows, &session.dir);
    let mut out = vec![];
    if role == RouterRole::Desk {
        out.push(t!("总台交接 · 项目", "Desk handoff · Projects").into());
        let routers = store.routers()?;
        let fallback = crate::config::new_group_dir();
        for g in groups.iter().filter(|g| g.path != group.path) {
            let (path, _) = defaults(g, &rows, &fallback);
            let router =
                routers.iter().find(|(name, _)| *name == g.path).map_or(t!("无 router", "no router").into(), |(_, id)| format!("@{id}"));
            out.push(format!("{}  {path}  {router}", g.path));
        }
    }
    out.push(tr!("项目交接 · {}", "Project handoff · {}", group.path));
    out.push(tr!("项目目录  {dir}", "Project directory  {dir}"));
    let mut dirs: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.') && e.path().is_dir()).then_some(name)
        })
        .collect();
    dirs.sort();
    dirs.truncate(30);
    if !dirs.is_empty() {
        out.push(tr!("子目录  {}", "Directories  {}", dirs.join(", ")));
    }
    let tasks: Vec<_> = store.tasks()?.into_iter().filter(|t| t.group == group.path).collect();
    section(&mut out, t!("没完成的单", "Open tickets"), tasks.iter().filter(|t| t.done == 0).map(ticket_line).collect());
    let mut done: Vec<_> = tasks.iter().filter(|t| t.done > 0 && t.done >= now.saturating_sub(86400)).collect();
    done.sort_by_key(|t| std::cmp::Reverse((t.done, t.n)));
    section(&mut out, t!("刚完成", "Recently completed"), done.into_iter().take(5).map(ticket_line).collect());
    let notes = store.notes_in(&group.path)?.into_iter().take(10).map(|n| n.name().to_owned()).collect();
    section(&mut out, t!("项目笔记", "Project notes"), notes);
    let mut sessions: Vec<_> = rows.iter().filter(|r| r.group == group.path && r.id != session.id).collect();
    sessions.sort_by_key(|r| (std::cmp::Reverse(r.active), r.order));
    let mut lines = vec![];
    // Budget roughly 2k tokens, keeping tickets and recent sessions first.
    let cost = |s: &str| s.bytes().filter(u8::is_ascii).count().div_ceil(3) + s.chars().filter(|c| !c.is_ascii()).count() + 1;
    let mut used: usize = out.iter().map(|s| cost(s)).sum();
    for (i, row) in sessions.iter().enumerate() {
        let line = session_line(row, &dir, &tasks, now);
        used += cost(&line);
        if used > 1950 {
            lines.push(tr!("另有 {} 个会话", "{} more sessions", sessions.len() - i));
            break;
        }
        lines.push(line);
    }
    section(&mut out, t!("本组会话", "Sessions"), lines);
    let mut threads = crate::thread::Threads::new();
    threads.refresh(std::slice::from_ref(&session.id));
    let mut recent = Vec::new();
    for e in threads.of(&session.id).iter().rev().filter(|e| !mail.iter().any(|m| m.id == e.id)).take(15) {
        use crate::thread::Kind::*;
        let action = match e.kind {
            You => t!("用户", "user"),
            From => t!("来话", "from"),
            To => t!("发给", "to"),
            Notice => t!("通知", "notice"),
            WorkerReply | Reply => t!("回复", "reply"),
            TaskDone => t!("单已完成", "ticket done"),
            WorkerAsks | Asks => t!("提问", "question"),
            WorkerFailed | Failed => t!("出错", "failed"),
            Pulled => t!("移动", "moved"),
            Started => t!("新建", "started"),
        };
        let mut fields = vec![crate::schedule::clock(e.ts, now), action.to_owned()];
        fields.extend([&e.who, &e.via, &e.text, &e.aux].into_iter().filter(|s| !s.is_empty()).cloned());
        let line = fields.join("  ");
        used += cost(&line);
        if used > 1950 {
            break;
        }
        recent.push(line);
    }
    recent.reverse();
    section(&mut out, t!("最近往来", "Recent exchanges"), recent);
    if !mail.is_empty() || say.is_some() {
        out.push(t!("叫醒你的", "What woke you").into());
        for m in mail {
            let via = if m.via.is_empty() && m.from == "you" { t!("界面", "interface") } else { &m.via };
            let who = if m.from == "you" { t!("用户", "user") } else { &m.who };
            let kind = if m.at > 0 { t!("定时", "scheduled") } else { &m.what };
            let source = [who, via, kind].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
            out.push(format!("{}  {}\n{}", crate::schedule::clock(m.ts, now), source, m.text));
        }
    }
    if say.is_some() || mail.is_empty() {
        out.push(String::new());
        out.push(
            say.unwrap_or_else(|| t!("看完等用户说话,只回一个字。", "Wait for the user to speak. Reply with just one word.")).to_owned(),
        );
    }
    Ok((dir, format!("[aca] {}", out.join("\n"))))
}

pub fn age(sec: u64) -> String {
    match sec {
        0..60 => tr!("{sec}秒", "{sec}s"),
        60..3600 => tr!("{}分", "{}m", sec / 60),
        3600..86400 => tr!("{}时", "{}h", sec / 3600),
        _ => tr!("{}天", "{}d", sec / 86400),
    }
}

/// What the desk is called. The desk is the router of the group sessions that belong to no project start in: it takes
/// what the user doesn't know whom to hand to, whichever group that turns out to be for
pub fn desk_title() -> String {
    tr!("总台", "desk")
}

/// The desk's opening message. `scratch`: its own group, where what belongs to no project is started; `dir`: the
/// directory that group works in
pub fn desk_brief(scratch: &str, dir: &str) -> String {
    tr!(
        "你是 aca 的总台:用户把不知道该交给谁的事交给你(在界面里说的,或者从别的渠道发来的),你把它交给合适的会话。你只分发,不自己动手做,不验收,也不建项目。

- 有哪些项目:aca group ls --json(path 是项目名,default_path 是它的目录,router 是它的 router 的 id,空的就是没有);某个项目的会话:aca ls --json 里 group 是它的;某一个在做什么:aca show <id>、aca output <id>
- 已经有会话在做这件事:交给它,aca msg <id> \"…\"
- 属于某个项目:在那个项目里开一张任务单,aca task add -g <项目> -t <名字> --criteria \"做到什么算完成\",它打印单号(#9)。项目有 router 时,router 会收到这张单、由它交出去,你不用再发
- 那个项目没有 router:单由你交。有合适的会话:aca task give <单号> <id> \"…\";没有就新建一个来做:aca new <工具> <项目的目录> --parent - -g <项目> -t <标题> --task <单号> --say \"…\"。--parent - 和 -g 都要写:新会话的结果直接回到用户那里,不经过你;<工具> 照那个项目里别的会话的 tool 和 account 写成 工具:账号
- 不属于哪个项目:aca new <工具> {dir} --parent - -g {scratch} -t <标题> --say \"…\"
- 看不出属于哪:用选择题问用户,列出最可能的两三个项目和「{scratch}」
- 要长期记住的事:aca note add \"…\";已有的:aca note ls
- 每次只回用户一两句:开了哪张单、交给了谁",
        "You are aca's desk: the user hands you what they don't know whom to give to (said in the interface, or sent in from another channel) and you pass it to the right session. You only dispatch: you don't do the work yourself, you don't check it, and you don't create projects.

- The projects: aca group ls --json (path is a project's name, default_path its directory, router the id of its router, empty when it has none); a project's sessions: the ones whose group is it in aca ls --json; what one of them is doing: aca show <id>, aca output <id>
- A session is already on it: hand it over, aca msg <id> \"…\"
- It belongs to a project: make a task ticket there, aca task add -g <PROJECT> -t <TITLE> --criteria \"what counts as done\", which prints its number (#9). A project with a router: the router is told of the ticket and hands it out, you send nothing more
- That project has no router: the ticket is yours to hand over. To a session that fits: aca task give <N> <id> \"…\"; when none does, start one on it: aca new <TOOL> <PROJECT-DIR> --parent - -g <PROJECT> -t <TITLE> --task <N> --say \"…\". Always write both --parent - and -g: what the new session does goes straight back to the user, not through you; for <TOOL> write TOOL:ACCOUNT from the tool and account of that project's other sessions
- It belongs to no project: aca new <TOOL> {dir} --parent - -g {scratch} -t <TITLE> --say \"…\"
- You can't tell where it belongs: ask the user as a multiple-choice question, listing the two or three likeliest projects and “{scratch}”
- What should be remembered: aca note add \"…\"; what's there: aca note ls
- Answer the user in a sentence or two: which ticket you made, who got it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{self, Lang};

    #[test]
    fn the_handoff_reads_the_registry_and_only_the_first_directory_level() {
        let dir = std::env::temp_dir().join(format!("aca-handoff-{}", crate::paths::uuid4()));
        for name in ["src/nested", "data", ".cache"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("README"), "x").unwrap();
        let path = dir.to_str().unwrap();
        let store = Store::memory();
        let s = Session {
            id: "worker-r2".into(),
            title: "Build".into(),
            group: "app".into(),
            tool: "claude".into(),
            account: "team".into(),
            dir: dir.join("src").display().to_string(),
            created: 99700,
            ..Default::default()
        };
        store.group_create("app", path).unwrap();
        store.insert(&s).unwrap();
        let router = Session { id: "router-r2".into(), ..s.clone() };
        store.insert(&router).unwrap();
        store.group_set_router("app", &router.id).unwrap();
        store.group_create("scratch", "/asks").unwrap();
        store.note_add("app", "Build rules\nprivate body", "").unwrap();
        let task =
            Task { group: "app".into(), title: "Fix".into(), session: s.id.clone(), note: "waiting for CI".into(), ..Default::default() };
        for (n, done) in [(1, 0), (2, 13600), (3, 13599)] {
            store.task_put(&Task { n, done, result: "green\nextra".into(), ..task.clone() }).unwrap();
        }
        let scratch = Session { group: "scratch".into(), dir: "/asks".into(), ..Default::default() };
        i18n::with(Lang::En, || {
            assert_eq!(
                handoff(&store, RouterRole::Project, &router, 100000, None, &[]).unwrap(),
                (
                    path.to_string(),
                    format!(
                        "[aca] Project handoff · app\nProject directory  {path}\nDirectories  data, src\nOpen tickets\n#1 Fix  assigned  worker-r2  waiting for CI\nRecently completed\n#2 Fix  done  worker-r2  green\nProject notes\nBuild rules\nSessions\nworker-r2  ■ stopped 5m  claude:team  Build  {path}/src  #1\nRecent exchanges  none\n\nWait for the user to speak. Reply with just one word."
                    )
                )
            );
            assert_eq!(
                handoff(&store, RouterRole::Desk, &scratch, 100000, Some("Where are we?\nPlease check."), &[]).unwrap().1,
                format!(
                    "[aca] Desk handoff · Projects\napp  {path}  @router-r2\nProject handoff · scratch\nProject directory  /asks\nOpen tickets  none\nRecently completed  none\nProject notes  none\nSessions  none\nRecent exchanges  none\nWhat woke you\n\nWhere are we?\nPlease check."
                )
            );
            for n in 4..10 {
                store.task_put(&Task { n, done: 90000 + n as u64, ..task.clone() }).unwrap();
            }
            for i in 0..31 {
                std::fs::create_dir(dir.join(format!("d{i:02}"))).unwrap();
            }
            for i in 0..11 {
                store.note_add("app", &format!("note-{i}"), "").unwrap();
            }
            let got = handoff(&store, RouterRole::Project, &router, 100000, None, &[]).unwrap().1;
            assert!(got.contains("d29") && !got.contains("d30"));
            assert_eq!(got.matches("  done  ").count(), 5);
            assert!(got.contains("#9 Fix") && !got.contains("#4 Fix"));
            assert_eq!(got.split("Project notes\n").nth(1).unwrap().split("\nSessions").next().unwrap().lines().count(), 10);
        });
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_desk_is_told_it_dispatches_for_every_project_and_where_what_belongs_to_none_starts() {
        for (lang, name, place) in [(Lang::Zh, "总台", "你是 aca 的总台"), (Lang::En, "desk", "You are aca's desk")] {
            i18n::with(lang, || {
                assert_eq!(desk_title(), name);
                let b = desk_brief("随手问", "/h/asks");
                assert!(b.starts_with(place), "{b}");
                // Every command it is to work with is there as it's typed
                let cmds = [
                    "aca group ls --json",
                    "aca ls --json",
                    "aca show <id>",
                    "aca output <id>",
                    "aca msg <id> \"…\"",
                    "aca task add -g <",
                    "aca task give <",
                    " --task <",
                    "aca new <",
                    " /h/asks --parent - -g 随手问 -t <",
                    "aca note add \"…\"",
                    "aca note ls",
                ];
                for cmd in cmds {
                    assert!(b.contains(cmd), "{cmd}: {b}");
                }
                // Both ways of starting a session make it nobody's child (the desk hands work on and doesn't look after it)
                // and name its group: with no parent it wouldn't land in any otherwise
                let starts: Vec<&str> = b.lines().filter(|l| l.contains("aca new <")).collect();
                assert_eq!(starts.len(), 2, "{b}");
                assert!(starts.iter().all(|l| l.contains(" --parent - -g ") && l.contains(" -t <") && l.contains(" --say \"…\"")), "{b}");
            });
        }
    }

    #[test]
    fn a_router_is_named_after_its_group_and_told_its_place_and_its_commands() {
        assert_eq!(title("fun/probe"), "probe router");
        assert_eq!(title("nuc"), "nuc router");
        for (lang, place, pull) in [(Lang::Zh, "项目「fun/probe」的 router", "「随手问」"), (Lang::En, "router of project “fun/probe”", "“随手问”")] {
            i18n::with(lang, || {
                let b = brief("fun/probe", "随手问");
                assert!(b.contains(place) && b.contains(pull), "{b}");
                // Every command it is to work with is there as it's typed
                let cmds = [
                    "aca ls --json",
                    "aca show <id>",
                    "aca output <id>",
                    "aca msg <id>",
                    "aca task add -t <",
                    "aca task ls -g fun/probe",
                    "aca task give <",
                    "aca task set <",
                    "aca new <",
                    " --task <",
                    "aca move --tree <id>… fun/probe",
                    "aca note add",
                    "aca note ls",
                ];
                for cmd in cmds {
                    assert!(b.contains(cmd), "{cmd}: {b}");
                }
                // What it starts is nobody's child (it hands work on and doesn't look after it), so the group is named:
                // without a parent the new session wouldn't follow the router into its group
                let start = b.lines().find(|l| l.contains("aca new <")).unwrap();
                assert!(start.contains("> --parent - -g fun/probe -t <") && start.contains(" --say \"…\""), "{start}");
                // No scratch group, or the router is the scratch group's own: no place is named to pull from
                assert!(!brief("fun/probe", "").contains(pull) && !brief("随手问", "随手问").contains("比如"));
            });
        }
    }
}
