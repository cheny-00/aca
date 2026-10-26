//! 对选中会话的操作:`R` 重启、`X` 换账号接着跑、`r` 改名、`m` 挪分组、`⇧↑` `⇧↓` 组内挪位置(分组上是挪分组的先后)、`f` / `F` 分叉、`H` 交接、`M` 挂 MCP、`d` 删除(分组上是删分组)、`A` 接管,
//! 还有 `^Z` 撤回删掉 / 停掉的、回收站里拿回来 / 真删的,都是 aca-core 里的函数(`session::*`、登记表);`s` 发便签走 tmux。
//! 停、删要等进程退干净(最长几秒),由 run 循环放进后台线程跑,界面不卡。

use aca_core::session;
use aca_core::store::Store;
use aca_core::{t, tr};

use crate::create::{NewReq, Removed, fzf};
use crate::data::Agent;
use crate::handoff::Source;

/// 要操作的会话
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// 会话的键(登记的是 id,普通 tmux 会话是会话名)
    pub name: String,
    pub title: String,
    pub profile: String,
    /// 登记的会话 id,空 = 普通 tmux 会话
    pub id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// force:现在不用(留着确认框那条路)
    Restart { t: Target, force: bool },
    /// unlock:改完解开标题锁(`^Z` 改回一个原来没锁的标题;改名总是锁上)
    Rename { t: Target, to: String, unlock: bool },
    /// from:它现在的分组(`^Z` 挪回去用);groups:已有的分组(大小写不同也算同一个),要挪进的分组没有就建
    Move { t: Target, from: String, to: String, groups: Vec<String> },
    /// D:停掉 agent(窗格连最后的画面留着),登记留着
    Stop { t: Target },
    /// 会话里另开的窗口 / 窗格上的 D:只停这一格(见 park.rs)
    Park { t: Target, p: crate::park::Pane },
    /// ⏎ 停着的那格、^Z 撤回上面那个:在原地再启动这一格
    Respawn { t: Target, p: crate::park::Pane },
    /// s:把便签发给它(粘进 pane 的输入框再回车);key 是便签的键,发出去就删掉
    Send { t: Target, pane: String, text: String, key: String },
    /// f / F:带着对话分叉出一个新会话。title 空 = 默认的「标题 (分叉)」,不锁标题(Claude 之后还能自己起);给了就锁
    Fork { t: Target, title: Option<String> },
    /// M:挂上 / 卸下项目一层的 MCP(写 dir 下的 .mcp.json),dir 是登记目录,做完收拾它
    Mcp { t: Target, dir: String, on: Vec<String>, off: Vec<String> },
    /// d:删会话(见 `create::remove`);pane 有 = 只关会话里另开的窗口 / 分出来的窗格
    Remove { t: Target, pane: Option<String> },
    /// ⇧↑ ⇧↓:组内挪到另一个会话(id 是 by)紧前面 / 紧后面(after)
    Order { t: Target, by: String, after: bool },
    /// 分组上 ⇧↑ ⇧↓:分组(连着子分组)挪到同级的 by 紧前面 / 紧后面。不对着哪个会话
    GroupOrder { group: String, by: String, after: bool },
    /// H:照 req 新建一个会话(to 是它哪家),把 from 的交接说明发过去(见 `handoff`)。
    /// note:from 的便签带过去了,新会话建成就删(它的键)
    Handoff { t: Target, from: Box<Source>, req: NewReq, to: Agent, note: Option<String> },
    /// X 的框里挑了一个账号:换成它,在原地重启、接着这段对话(见 `session::rehome`)。
    /// to 是「工具:账号」,label 是菜单里那个名字
    Rehome { t: Target, to: String, label: String },
    /// A:接管一个普通 tmux 会话(不是 aca 起的),照确认框里给人看过的那份登记(见 `aca_core::adopt`)
    Adopt { t: Target, plan: Box<aca_core::adopt::Plan> },
    /// `^Z`:撤销停掉的(再启动)
    Revive { t: Target },
    /// 回收站里的一条拿回来(`b` 那一页 ⏎、`^Z` 撤删除,见 trash.rs);start:这些会话再启动(^Z 撤的,删之前在跑)。不对着哪个会话
    Restore { item: Box<crate::trash::Item>, start: Vec<String> },
    /// 回收站里的一条现在就真删(`b` 那一页 d y)。不对着哪个会话
    Purge { item: Box<crate::trash::Item> },
    /// 建分组、删分组、改分组(见 groups.rs)。不对着哪个会话
    Group(crate::groups::Act),
    /// 远端任务:测一下 server(见 hooks.rs)。不对着哪个会话
    /// L 那一页:装 / 更新 / 删 / 开停一个插件(见 plugins.rs)。不对着哪个会话
    Plugins(crate::plugins::Act),
    /// v 多选后一起做的:同一种活对着几个会话(删、停、重启、挪分组,^Z 撤回它们的再启动、挪回去),
    /// 或者回收站里的几条(恢复、真删),见 `many`。不对着哪个会话
    Many(Vec<Job>),
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done(String),
    Failed(String),
    /// 新建出来一个会话(分叉、交接):高亮跟过去。id 是它的会话 id
    Made {
        msg: String,
        id: String,
    },
    /// 交接:新会话建好了,说明没发出去,留在它的便签里(note)
    Handed {
        msg: String,
        id: String,
        note: String,
    },
    /// d 删的会话 / 分组进了回收站(见 trash.rs):界面记下这一条,^Z 能撤回
    Binned {
        msg: String,
        item: Box<crate::trash::Item>,
    },
    /// `Job::Many`:各个的结果,和它的活一一对应(删除的不走这里,并成一条 `Binned`)
    Many(Vec<Outcome>),
}

impl Job {
    /// 对着的会话;挪分组的先后没有
    pub fn target(&self) -> Option<&Target> {
        Some(match self {
            Job::Restart { t, .. }
            | Job::Rename { t, .. }
            | Job::Move { t, .. }
            | Job::Stop { t }
            | Job::Park { t, .. }
            | Job::Respawn { t, .. }
            | Job::Send { t, .. }
            | Job::Fork { t, .. }
            | Job::Mcp { t, .. }
            | Job::Remove { t, .. }
            | Job::Order { t, .. }
            | Job::Handoff { t, .. }
            | Job::Rehome { t, .. }
            | Job::Adopt { t, .. }
            | Job::Revive { t, .. } => t,
            Job::GroupOrder { .. } | Job::Restore { .. } | Job::Purge { .. } | Job::Group(_) | Job::Plugins(_) | Job::Many(_) => {
                return None;
            }
        })
    }

    /// 跑的时候底栏显示的
    pub fn doing(&self) -> String {
        match self {
            Job::Restart { t, .. } => tr!("正在重启「{}」…", "restarting “{}”…", short(&t.title)),
            Job::Rename { to, .. } => tr!("正在重命名为「{}」…", "renaming to “{}”…", short(to)),
            Job::Move { t, to, .. } => tr!("正在将「{}」移到 {to}…", "moving “{}” to {to}…", short(&t.title)),
            Job::Stop { t } => tr!("正在停止「{}」…", "stopping “{}”…", short(&t.title)),
            Job::Park { p, .. } => tr!("正在停止「{}」…", "stopping “{}”…", short(&p.label)),
            Job::Respawn { p, .. } => tr!("正在重启「{}」…", "restarting “{}”…", short(&p.label)),
            Job::Send { t, .. } => tr!("正在发送给「{}」…", "sending to “{}”…", short(&t.title)),
            Job::Fork { t, .. } => tr!("正在分叉「{}」…", "forking “{}”…", short(&t.title)),
            Job::Mcp { t, .. } => tr!("正在修改「{}」的 MCP…", "changing the MCPs of “{}”…", short(&t.title)),
            Job::Remove { t, pane: Some(_) } => tr!("正在关闭「{}」…", "closing “{}”…", short(&t.title)),
            Job::Remove { t, pane: None } => tr!("正在删除「{}」…", "deleting “{}”…", short(&t.title)),
            Job::Order { t, .. } => tr!("正在移动「{}」…", "moving “{}”…", short(&t.title)),
            Job::GroupOrder { group, .. } => tr!("正在移动分组 {}…", "moving group {}…", short(group)),
            Job::Handoff { t, .. } => tr!("正在交接「{}」…", "handing off “{}”…", short(&t.title)),
            Job::Rehome { t, label, .. } => tr!("正在将「{}」切换到 {label}…", "moving “{}” to {label}…", short(&t.title)),
            Job::Adopt { t, .. } => tr!("正在接管「{}」…", "adopting “{}”…", short(&t.title)),
            Job::Revive { t, .. } => tr!("正在撤销「{}」…", "undoing “{}”…", short(&t.title)),
            Job::Restore { item, .. } => tr!("正在恢复{}…", "restoring {}…", item.name()),
            Job::Purge { item } => tr!("正在永久删除{}…", "deleting {} for good…", item.name()),
            Job::Group(a) => a.doing(),
            Job::Plugins(a) => a.doing(),
            Job::Many(jobs) => {
                let n = jobs.len();
                match jobs.first() {
                    Some(Job::Remove { .. }) => tr!("正在删除 {n} 个…", "deleting {n}…"),
                    Some(Job::Stop { .. }) => tr!("正在停止 {n} 个…", "stopping {n}…"),
                    Some(Job::Restart { .. }) => tr!("正在重启 {n} 个…", "restarting {n}…"),
                    Some(Job::Revive { .. }) => tr!("正在撤销停止 {n} 个…", "undoing the stop of {n}…"),
                    Some(Job::Restore { .. }) => tr!("正在恢复 {n} 条…", "restoring {n}…"),
                    Some(Job::Purge { .. }) => tr!("正在永久删除 {n} 条…", "deleting {n} for good…"),
                    Some(Job::Move { to, .. }) if jobs.iter().all(|j| matches!(j, Job::Move { to: x, .. } if x == to)) => {
                        tr!("正在将 {n} 个移到 {to}…", "moving {n} to {to}…")
                    }
                    _ => tr!("正在移动 {n} 个…", "moving {n}…"),
                }
            }
        }
    }
}

/// 几个会话合起来怎么称呼:一个是「标题」,几个是「第一个」等 n 个。前面不用空一格(「已删除{}」)
pub fn named(first: &str, n: usize) -> String {
    let more = n.saturating_sub(1);
    match n {
        0 | 1 => tr!("「{}」", "“{}”", short(first)),
        _ => tr!("「{}」等 {n} 个", "“{}” and {more} more", short(first)),
    }
}

/// `Job::Many` 做完底栏说的:做成了几个,没成的说第一个为什么
pub fn many_msg(jobs: &[Job], outs: &[Outcome]) -> String {
    let ok = outs.iter().filter(|o| matches!(o, Outcome::Done(_))).count();
    let bad: Vec<&str> = outs.iter().filter_map(|o| if let Outcome::Failed(e) = o { Some(e.as_str()) } else { None }).collect();
    let n = bad.len();
    if ok == 0 {
        return match bad.first() {
            Some(e) if n > 1 => tr!("{n} 个均失败:{e}", "none of the {n} worked: {e}"),
            Some(e) => e.to_string(),
            None => String::new(),
        };
    }
    let mut msg = match jobs.first() {
        Some(Job::Stop { .. }) => tr!("已停止 {ok} 个", "stopped {ok}"),
        Some(Job::Restart { .. }) => tr!("已重启 {ok} 个", "restarted {ok}"),
        Some(Job::Revive { .. }) => tr!("已撤销停止:{ok} 个已重新启动", "undid the stop: {ok} started again"),
        Some(Job::Restore { .. }) => tr!("已恢复 {ok} 条", "restored {ok}"),
        Some(Job::Purge { .. }) => tr!("已永久删除 {ok} 条", "deleted {ok} for good"),
        Some(Job::Move { to, .. }) if jobs.iter().all(|j| matches!(j, Job::Move { to: x, .. } if x == to)) => {
            tr!("已将 {ok} 个移到 {to}", "moved {ok} to {to}")
        }
        _ => tr!("已移动 {ok} 个", "moved {ok}"),
    };
    if let Some(e) = bad.first() {
        msg += &tr!(" · {n} 个失败:{e}", " · {n} failed: {e}");
    }
    msg
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(16).collect();
    if s.chars().count() > 16 {
        out.push('…');
    }
    out
}

pub fn run(job: &Job) -> Outcome {
    let store = match Store::open() {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(e),
    };
    let cfg = aca_core::config::load();
    match job {
        Job::Restart { t, .. } => match session::restart(&store, &cfg, &t.id, false) {
            Ok(()) => Outcome::Done(tr!("已重启「{}」", "restarted “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("重启失败:{e}", "restart failed: {e}")),
        },
        Job::Rename { t, to, unlock } => match session::rename(&store, &t.id, to) {
            Ok(_) if *unlock => match session::unlock(&store, &t.id) {
                Ok(()) => Outcome::Done(tr!("已改名为「{}」", "renamed to “{}”", short(to))),
                Err(e) => Outcome::Failed(tr!("已重命名,但解锁失败:{e}", "renamed, but unlocking failed: {e}")),
            },
            Ok(_) => Outcome::Done(tr!("已改名为「{}」", "renamed to “{}”", short(to))),
            Err(e) => Outcome::Failed(tr!("改名失败:{e}", "rename failed: {e}")),
        },
        Job::Move { t, to, groups, .. } => {
            // 大小写不同也算同一个:挪进已有的那个
            let (path, made) = match groups.iter().find(|g| g.eq_ignore_ascii_case(to)) {
                Some(g) => (g.clone(), false),
                None => (to.clone(), true),
            };
            match session::move_to(&store, &t.id, &path) {
                Ok(_) => Outcome::Done(tr!(
                    "已将「{}」移到{} {path}",
                    "moved “{}” to{} {path}",
                    short(&t.title),
                    if made { t!("新分组", " new group") } else { "" }
                )),
                Err(e) => Outcome::Failed(tr!("移动分组失败:{e}", "moving group failed: {e}")),
            }
        }
        Job::Stop { t } => match session::stop(&store, &t.id) {
            Ok(()) => Outcome::Done(tr!("已停止「{}」", "stopped “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("停止失败:{e}", "stop failed: {e}")),
        },
        Job::Park { p, .. } => match crate::park::park(p) {
            Ok(()) => Outcome::Done(tr!("已停止「{}」", "stopped “{}”", short(&p.label))),
            Err(e) => Outcome::Failed(tr!("停止失败:{e}", "stop failed: {e}")),
        },
        Job::Respawn { p, .. } => match crate::park::respawn(p) {
            Ok(()) => Outcome::Done(tr!("已重启「{}」", "restarted “{}”", short(&p.label))),
            Err(e) => Outcome::Failed(tr!("重启失败:{e}", "restart failed: {e}")),
        },
        Job::Send { t, pane, text, .. } => match crate::data::send_into(pane, text) {
            Ok(()) => Outcome::Done(tr!("已发给「{}」", "sent to “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("发送失败:{e}", "not sent: {e}")),
        },
        Job::Fork { t, title } => match session::fork(&store, &cfg, &t.id, title.as_deref().unwrap_or("")) {
            Ok(s) => Outcome::Made { msg: tr!("已分叉「{}」", "forked “{}”", short(&s.title)), id: s.id },
            Err(e) => Outcome::Failed(tr!("分叉失败:{e}", "fork failed: {e}")),
        },
        Job::Mcp { dir, on, off, .. } => crate::mcp::apply(dir, on, off),
        Job::Remove { t, pane } => {
            // 关的是窗格(会话还在)还是删会话:英文的动词变形不一样,整句分开写
            let close = pane.is_some();
            match crate::create::remove(t, pane.as_deref()) {
                Ok(Removed::Archived { was, group }) => {
                    let s = crate::trash::Gone { id: t.id.clone(), title: t.title.clone(), group, archived: was };
                    let item = Box::new(crate::trash::Item::session(&t.profile, s));
                    Outcome::Binned { msg: tr!("已删除「{}」 · ^Z 撤销", "deleted “{}” · ^Z undoes", short(&t.title)), item }
                }
                Ok(Removed::Gone) if close => Outcome::Done(tr!("已关闭「{}」", "closed “{}”", short(&t.title))),
                Ok(Removed::Gone) => Outcome::Done(tr!("已删除「{}」", "deleted “{}”", short(&t.title))),
                Err(e) if close => Outcome::Failed(tr!("关闭失败:{e}", "close failed: {e}")),
                Err(e) => Outcome::Failed(tr!("删除失败:{e}", "delete failed: {e}")),
            }
        }
        Job::Order { t, by, after } => order(&store, t, by, *after),
        Job::GroupOrder { group, by, after } => group_order(&store, group, by, *after),
        Job::Handoff { from, req, to, .. } => crate::handoff::run(from, req, *to),
        Job::Rehome { t, to, label } => match session::rehome(&store, &cfg, &t.id, to) {
            Ok(r) => {
                let mut msg = match r.copied {
                    0 => tr!("已切换到 {label}", "moved to {label}"),
                    _ => tr!("已切换到 {label} · 发送「继续」即可接着做", "moved to {label} · say “continue” to carry on"),
                };
                if !r.note.is_empty() {
                    msg = format!("{msg} · {}", r.note);
                }
                Outcome::Done(msg)
            }
            Err(e) => Outcome::Failed(tr!("换账号失败:{e}", "switching accounts failed: {e}")),
        },
        Job::Adopt { plan, .. } => match aca_core::adopt::apply(&store, plan) {
            // 进程没动过,还没带 aca 的 hook:重启一次才有「跑完没看」
            Ok((s, _)) => Outcome::Made {
                msg: tr!(
                    "已接管「{}」 · R 重启后才能识别任务完成",
                    "adopted “{}” · restart (R) so aca can tell when it finishes",
                    short(plan.title())
                ),
                id: s.id,
            },
            Err(e) => Outcome::Failed(tr!("接管失败:{e}", "adopt failed: {e}")),
        },
        Job::Revive { t } => match crate::create::start(&t.id) {
            Ok(()) => Outcome::Done(tr!("已撤销停止:「{}」已重新启动", "undid the stop: “{}” started again", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("撤销失败:{e}", "undo failed: {e}")),
        },
        Job::Restore { item, start } => restore(&store, item, start),
        Job::Purge { item } => {
            let n = crate::trash::drop_now(&store, item);
            match (item.group.is_empty(), n) {
                (true, _) | (false, 0) => Outcome::Done(tr!("已永久删除{}", "deleted {} for good", item.name())),
                _ => Outcome::Done(tr!("已永久删除{}及其中 {n} 个会话", "deleted {} and its {n} sessions for good", item.name())),
            }
        }
        Job::Group(a) => crate::groups::job(a),
        Job::Plugins(a) => crate::plugins::job(a),
        Job::Many(jobs) => many(jobs),
    }
}

/// v 多选的一批:各做各的、一起等(停、删要等进程退干净,一个个来要等好几轮);挪分组一个接一个(要新建的分组只建一次),
/// 回收站里的几条也一个接一个(恢复要建回分组、排先后,一起写会写乱)。删除的并成回收站里的一条,^Z 一起回来
fn many(jobs: &[Job]) -> Outcome {
    if !jobs.is_empty() && jobs.iter().all(|j| matches!(j, Job::Remove { pane: None, .. })) {
        return remove_many(jobs);
    }
    if jobs.iter().any(|j| matches!(j, Job::Move { .. } | Job::Restore { .. } | Job::Purge { .. })) {
        return Outcome::Many(jobs.iter().map(run).collect());
    }
    Outcome::Many(std::thread::scope(|sc| {
        let hs: Vec<_> = jobs.iter().map(|j| sc.spawn(move || run(j))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Outcome::Failed("panic".into()))).collect()
    }))
}

/// 一起删的几个会话:各自归档(一起等进程退干净),删掉了的记成回收站里的一条;没删掉的照实说,还在列表里
fn remove_many(jobs: &[Job]) -> Outcome {
    let ts: Vec<&Target> = jobs.iter().filter_map(Job::target).collect();
    let got: Vec<Result<Removed, String>> = std::thread::scope(|sc| {
        let hs: Vec<_> = ts.iter().map(|&t| sc.spawn(move || crate::create::remove(t, None))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("panic".into()))).collect()
    });
    let mut gone = vec![];
    let mut bad = vec![];
    for (t, r) in ts.iter().zip(got) {
        match r {
            Ok(Removed::Archived { was, group }) => {
                gone.push(crate::trash::Gone { id: t.id.clone(), title: t.title.clone(), group, archived: was })
            }
            Ok(Removed::Gone) => {}
            Err(e) => bad.push(e),
        }
    }
    let n = bad.len();
    if gone.is_empty() {
        return Outcome::Failed(match bad.first() {
            Some(e) if n > 1 => tr!("{n} 个均未删除:{e}", "none of the {n} were deleted: {e}"),
            Some(e) => tr!("删除失败:{e}", "delete failed: {e}"),
            None => t!("删除失败", "delete failed").to_string(),
        });
    }
    let item = crate::trash::Item::sessions(&ts[0].profile, gone);
    let mut msg = tr!("已删除{} · ^Z 撤销", "deleted {} · ^Z undoes", item.name());
    if let Some(e) = bad.first() {
        msg += &tr!(" · {n} 个未删除:{e}", " · {n} weren't deleted: {e}");
    }
    Outcome::Binned { msg, item: Box::new(item) }
}

/// 回收站里的一条拿回来:照记的建回分组(同名的期间又建了就进它,不改它的设置),取消归档(删之前就归档着的不动),
/// start 里的再启动。期间在别处真删掉的跳过,照实说
fn restore(store: &Store, item: &crate::trash::Item, start: &[String]) -> Outcome {
    let had: Vec<String> = store.groups().unwrap_or_default().into_iter().map(|g| g.path).collect();
    for g in &item.groups {
        if had.contains(&g.path) {
            continue;
        }
        if let Err(e) = store.group_create(&g.path, "") {
            return Outcome::Failed(tr!("恢复失败:{e}", "restore failed: {e}"));
        }
        let _ = store.group_order(&g.path, g.order);
        // 目录期间成了别的组的默认目录:不抢
        if !g.dir.is_empty() {
            let _ = store.group_set_dir(&g.path, &g.dir);
        }
    }
    let (mut back, mut lost, mut cold) = (0, 0, vec![]);
    for s in item.sessions.iter().filter(|s| !s.archived) {
        if !store.get(&s.id).is_ok_and(|x| x.id == s.id) {
            lost += 1;
            continue;
        }
        // 单删的会话:它的组期间删掉了,建回来(不然排不上位置)
        if !s.group.is_empty() && !had.contains(&s.group) && !item.groups.iter().any(|g| g.path == s.group) {
            let _ = store.group_create(&s.group, "");
        }
        if let Err(e) = session::archive(store, &s.id, false) {
            return Outcome::Failed(tr!("恢复失败:{e}", "restore failed: {e}"));
        }
        back += 1;
        if start.contains(&s.id)
            && let Err(e) = crate::create::start(&s.id)
        {
            cold.push(e);
        }
    }
    let what = item.name();
    let mut msg = match (item.group.is_empty(), back) {
        (true, 0) => return Outcome::Done(tr!("{what}已被永久删除,无法恢复", "{what} is gone for good; can't restore")),
        (true, _) | (false, 0) => tr!("已恢复{what}", "restored {what}"),
        (false, n) => tr!("已恢复{what} · {n} 个会话", "restored {what} · {n} sessions"),
    };
    // 分组、一起删的几个:有的期间真删了,照实说
    if lost > 0 && (!item.group.is_empty() || item.sessions.len() > 1) {
        msg += &tr!(" · {lost} 个已被永久删除", " · {lost} already gone for good");
    }
    // 登记已经回来了,只是没起来:算恢复了(再撤一次也还是起不来),照实说
    if let Some(e) = cold.first() {
        msg += &tr!(" · 启动失败:{e}", " · didn't start: {e}");
    }
    Outcome::Done(msg)
}

/// 把 ids 按给的先后重新编号(1、2、3…):组内顺序、分组的先后都是这么记的
fn renumber(ids: &[String], set: impl Fn(&str, i64) -> Result<(), String>) -> Result<(), String> {
    ids.iter().enumerate().try_for_each(|(i, id)| set(id, i as i64 + 1))
}

/// `me` 挪到 `by` 紧前面 / 紧后面之后的先后;两个里有一个不在里面 = None
fn moved(mut v: Vec<String>, me: &str, by: &str, after: bool) -> Option<Vec<String>> {
    let at = v.iter().position(|x| x == me)?;
    let x = v.remove(at);
    let to = v.iter().position(|x| x == by)? + usize::from(after);
    v.insert(to, x);
    Some(v)
}

/// ⇧↑ ⇧↓:挪到 by 紧前面 / 紧后面,这一组整个重新编号(归档的也算在里面,保持相对位置)。做成了不说话,列表已经挪好了
fn order(store: &Store, t: &Target, by: &str, after: bool) -> Outcome {
    let run = || -> Result<(), String> {
        let (me, it) = (store.get(&t.id)?, store.get(by)?);
        if me.group != it.group {
            return Err(t!("已不在同一分组", "they're no longer in the same group").into());
        }
        let mut all: Vec<_> = store.sessions(true)?.into_iter().filter(|s| s.group == me.group).collect();
        all.sort_by_key(|s| (s.order, s.created));
        let ids = moved(all.into_iter().map(|s| s.id).collect(), &me.id, &it.id, after)
            .ok_or(t!("找不到目标位置", "can't find where to move it"))?;
        renumber(&ids, |id, n| store.set_order(id, n))
    };
    match run() {
        Ok(()) => Outcome::Done(String::new()),
        Err(e) => Outcome::Failed(tr!("移动失败:{e}", "moving failed: {e}")),
    }
}

/// 分组上 ⇧↑ ⇧↓:挪到同级的 by 紧前面 / 紧后面,子分组跟着走(子分组排在哪只看上级)。
/// 同级的整个重新编号;别的层级的编号不动(它们只和自己的同级比)。做成了不说话
fn group_order(store: &Store, group: &str, by: &str, after: bool) -> Outcome {
    let Some(sibs) = crate::data::group_siblings(group) else {
        return Outcome::Failed(tr!("移动分组失败:分组 {group} 不存在", "moving group failed: no group {group}"));
    };
    let Some(ids) = moved(sibs, group, by, after) else {
        return Outcome::Failed(tr!(
            "移动分组失败:{by} 和 {group} 不在同一级",
            "moving group failed: {by} and {group} aren't on the same level"
        ));
    };
    match renumber(&ids, |g, n| store.group_order(g, n)) {
        Ok(()) => Outcome::Done(String::new()),
        Err(e) => Outcome::Failed(tr!("移动分组失败:{e}", "moving group failed: {e}")),
    }
}

/// 手打的分组名规整一下:两头的 / 和空白去掉,空白换成 -(分组名要能在命令行里直接打),空的一级去掉。什么都不剩 = None
pub fn clean_group(name: &str) -> Option<String> {
    let parts: Vec<String> =
        name.split('/').map(|p| p.split_whitespace().collect::<Vec<_>>().join("-")).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `m` 列表里的一个分组
#[derive(Clone, Debug, PartialEq)]
pub struct GroupRow {
    pub group: String,
    /// 这个组在哪干活:记着的默认目录,没记就是组里会话最多的那个目录;都没有 = 空
    pub dir: String,
    /// 和要挪的会话是同一个目录
    pub same: bool,
}

/// `m`:用 fzf 挑要挪进的分组。挪只换分组,会话还在原来的目录跑,所以每行带上那个组的目录,对不上的一眼看得出。
/// 只按分组名搜(打个新名字不会被别的组的目录匹配走),输入的名字一个都匹配不上时回车 = 用这个名字新建。
/// what:挪的是谁(见 `named`);here:它的目录(多选挪的几个不在同一个目录 = 空)
pub fn pick_group(rows: &[GroupRow], what: &str, here: &str) -> Result<Option<String>, String> {
    let header = if here.is_empty() {
        tr!("--header=移动{what}", "--header=move {what}")
    } else {
        tr!(
            "--header=移动{what} · 只换分组,目录不变:{}",
            "--header=move {what} · only the group changes, it stays in {}",
            crate::create::tilde(here)
        )
    };
    let args = [
        "--ansi",
        "--delimiter=\t",
        "--with-nth=2..",
        "--nth=1",
        "--accept-nth=1",
        t!("--prompt=移到 ", "--prompt=move to "),
        &header,
        "--bind=enter:accept-or-print-query",
    ];
    fzf(&group_lines(rows), &args)
}

/// 每行三个字段,用 Tab 分开:分组名(选中后输出)、显示的名字(补到一样宽,后面的 Tab 才对得齐)、目录
fn group_lines(rows: &[GroupRow]) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let w = rows.iter().map(|r| r.group.width()).max().unwrap_or(0);
    let (b, d, green, off) = ("\x1b[1m", "\x1b[2m", "\x1b[32m", "\x1b[0m");
    rows.iter()
        .map(|r| {
            let pad = " ".repeat(w - r.group.width());
            let dir = crate::create::tilde(&r.dir);
            let dir = match (r.dir.is_empty(), r.same) {
                (true, _) => String::new(),
                (false, true) => format!("{green}{dir}  {}{off}", t!("同目录", "same directory")),
                (false, false) => format!("{d}{dir}{off}"),
            };
            format!("{}\t{b}{}{off}{pad}  \t{dir}", r.group, r.group)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_says_how_many_worked_and_why_the_first_failed_one_did_not() {
        let t = |s: &str| Target { name: s.into(), title: s.into(), profile: "default".into(), id: format!("id-{s}") };
        let stops = [Job::Stop { t: t("a") }, Job::Stop { t: t("b") }, Job::Stop { t: t("c") }];
        let ok = || Outcome::Done(String::new());
        let bad = |e: &str| Outcome::Failed(e.into());
        assert_eq!(many_msg(&stops, &[ok(), ok(), ok()]), "已停止 3 个");
        assert_eq!(many_msg(&stops, &[ok(), bad("停止失败:x"), bad("停止失败:y")]), "已停止 1 个 · 2 个失败:停止失败:x");
        assert_eq!(many_msg(&stops, &[bad("停止失败:x"), bad("停止失败:y"), bad("z")]), "3 个均失败:停止失败:x");
        assert_eq!(many_msg(&stops[..1], &[bad("停止失败:x")]), "停止失败:x");
        let mv = |s: &str, to: &str| Job::Move { t: t(s), from: "f".into(), to: to.into(), groups: vec![] };
        assert_eq!(many_msg(&[mv("a", "g"), mv("b", "g")], &[ok(), ok()]), "已将 2 个移到 g");
        assert_eq!(many_msg(&[mv("a", "g"), mv("b", "h")], &[ok(), ok()]), "已移动 2 个");
        assert_eq!(Job::Many(stops.to_vec()).doing(), "正在停止 3 个…");
        assert_eq!(named("一个很长很长很长很长很长很长的标题", 3), "「一个很长很长很长很长很长很长的标…」等 3 个");
        assert_eq!(named("a", 1), "「a」");
    }

    #[test]
    fn typed_group_names_are_tidied() {
        assert_eq!(clean_group(" fun/new thing "), Some("fun/new-thing".into()));
        assert_eq!(clean_group("/a//b/"), Some("a/b".into()));
        assert_eq!(clean_group("工作  台"), Some("工作-台".into()));
        assert_eq!(clean_group(" / "), None);
    }

    #[test]
    fn group_rows_show_the_name_padded_then_the_directory() {
        let row = |group: &str, dir: &str, same| GroupRow { group: group.into(), dir: dir.into(), same };
        let lines = group_lines(&[row("fun/aca", "/x/aca", true), row("w", "/x/w", false), row("空", "", false)]);
        assert_eq!(
            lines,
            [
                "fun/aca\t\x1b[1mfun/aca\x1b[0m  \t\x1b[32m/x/aca  同目录\x1b[0m",
                "w\t\x1b[1mw\x1b[0m        \t\x1b[2m/x/w\x1b[0m",
                // 中文占两格,补的空格少一个
                "空\t\x1b[1m空\x1b[0m       \t",
            ]
        );
    }

    #[test]
    fn moving_takes_itself_out_first_then_lands_next_to_the_other() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        // a b S c d
        assert_eq!(moved(v("a b S c d"), "S", "b", false).unwrap(), v("a S b c d"));
        assert_eq!(moved(v("a b S c d"), "S", "a", false).unwrap(), v("S a b c d"));
        assert_eq!(moved(v("a b S c d"), "S", "c", true).unwrap(), v("a b c S d"));
        assert_eq!(moved(v("a b S c d"), "S", "d", true).unwrap(), v("a b c d S"));
        // 已经在那了:不变
        assert_eq!(moved(v("a b S c d"), "S", "b", true).unwrap(), v("a b S c d"));
        assert_eq!(moved(v("a b S c d"), "S", "c", false).unwrap(), v("a b S c d"));
        assert!(moved(v("a b"), "x", "a", false).is_none());
        assert!(moved(v("a b"), "a", "x", false).is_none());
    }

    #[test]
    fn reordering_renumbers_a_group_and_leaves_other_groups_alone() {
        let store = Store::memory();
        let mk = |id: &str, group: &str, order: i64| aca_core::store::Session {
            id: id.into(),
            title: id.into(),
            tool: "shell".into(),
            group: group.into(),
            order,
            dir: "/".into(),
            created: 1,
            ..Default::default()
        };
        for s in [mk("a", "g", 5), mk("b", "g", 9), mk("c", "g", 12), mk("x", "other", 1)] {
            store.insert(&s).unwrap();
        }
        let t = Target { name: "c".into(), title: "c".into(), profile: "default".into(), id: "c".into() };
        assert_eq!(order(&store, &t, "a", false), Outcome::Done(String::new()));
        let got: Vec<(String, i64)> = ["c", "a", "b", "x"].iter().map(|id| (id.to_string(), store.get(id).unwrap().order)).collect();
        assert_eq!(got, [("c".to_string(), 1), ("a".to_string(), 2), ("b".to_string(), 3), ("x".to_string(), 1)]);
        // 不在同一组:不挪
        assert_eq!(order(&store, &t, "x", false), Outcome::Failed("移动失败:已不在同一分组".into()));
    }
}
