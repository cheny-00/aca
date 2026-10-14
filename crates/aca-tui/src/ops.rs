//! 对选中会话的操作:`R` 重启、`H` 里挑同一家的别的账号 = 换账号接着跑、`r` 改名、`m` 挪分组、`⇧↑` `⇧↓` 组内挪位置(分组上是挪分组的先后)、`f` / `F` 分叉、`H` 交接、`M` 挂 MCP、`d` 删除(分组上是删分组)、`A` 接管,
//! 还有 `^Z` 撤回删掉 / 停掉的,都是 aca-core 里的函数(`session::*`、登记表);`s` 发便签走 tmux。
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
    /// H 里挑了同一家的别的账号、确认框里按 y:换成那个账号,在原地重启、接着这段对话(见 `session::rehome`)。
    /// to 是「工具:账号」,label 是菜单里那个名字
    Rehome { t: Target, to: String, label: String },
    /// A:接管一个普通 tmux 会话(不是 aca 起的),照确认框里给人看过的那份登记(见 `aca_core::adopt`)
    Adopt { t: Target, plan: Box<aca_core::adopt::Plan> },
    /// `^Z`:撤回删掉的(unarchive = 取消归档,见 trash.rs)/ 停掉的;start = 再启动(原来在跑)
    Revive { t: Target, unarchive: bool, start: bool },
    /// 分组上 d 的那一页:删分组(见 groups.rs)。不对着哪个会话
    Group(crate::groups::Act),
    /// 远端任务:测一下 server(见 hooks.rs)。不对着哪个会话
    /// L 那一页:装 / 更新 / 删 / 开停一个插件(见 plugins.rs)。不对着哪个会话
    Plugins(crate::plugins::Act),
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
    /// d 删的会话归档了(was = 本来就归档着),等界面退出再真删,之前能 ^Z 撤回(见 trash.rs)
    Trashed {
        msg: String,
        was: bool,
    },
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
            Job::GroupOrder { .. } | Job::Group(_) | Job::Plugins(_) => return None,
        })
    }

    /// 跑的时候底栏显示的
    pub fn doing(&self) -> String {
        match self {
            Job::Restart { t, .. } => tr!("正在重启「{}」…", "restarting “{}”…", short(&t.title)),
            Job::Rename { to, .. } => tr!("正在改名为「{}」…", "renaming to “{}”…", short(to)),
            Job::Move { t, to, .. } => tr!("正在把「{}」挪到 {to}…", "moving “{}” to {to}…", short(&t.title)),
            Job::Stop { t } => tr!("正在停止「{}」…", "stopping “{}”…", short(&t.title)),
            Job::Park { p, .. } => tr!("正在停止「{}」…", "stopping “{}”…", short(&p.label)),
            Job::Respawn { p, .. } => tr!("正在重启「{}」…", "restarting “{}”…", short(&p.label)),
            Job::Send { t, .. } => tr!("正在发给「{}」…", "sending to “{}”…", short(&t.title)),
            Job::Fork { t, .. } => tr!("正在分叉「{}」…", "forking “{}”…", short(&t.title)),
            Job::Mcp { t, .. } => tr!("正在改「{}」的 MCP…", "changing the MCPs of “{}”…", short(&t.title)),
            Job::Remove { t, pane: Some(_) } => tr!("正在关闭「{}」…", "closing “{}”…", short(&t.title)),
            Job::Remove { t, pane: None } => tr!("正在删除「{}」…", "deleting “{}”…", short(&t.title)),
            Job::Order { t, .. } => tr!("正在挪「{}」…", "moving “{}”…", short(&t.title)),
            Job::GroupOrder { group, .. } => tr!("正在挪分组 {}…", "moving group {}…", short(group)),
            Job::Handoff { t, .. } => tr!("正在交接「{}」…", "handing off “{}”…", short(&t.title)),
            Job::Rehome { t, label, .. } => tr!("正在把「{}」换到 {label}…", "moving “{}” to {label}…", short(&t.title)),
            Job::Adopt { t, .. } => tr!("正在接管「{}」…", "adopting “{}”…", short(&t.title)),
            Job::Revive { t, .. } => tr!("正在撤回「{}」…", "undoing “{}”…", short(&t.title)),
            Job::Group(a) => a.doing(),
            Job::Plugins(a) => a.doing(),
        }
    }
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
                Err(e) => Outcome::Failed(tr!("改了名,解锁失败:{e}", "renamed, but unlocking failed: {e}")),
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
                    "已把「{}」挪到{} {path}",
                    "moved “{}” to{} {path}",
                    short(&t.title),
                    if made { t!("新分组", " new group") } else { "" }
                )),
                Err(e) => Outcome::Failed(tr!("挪分组失败:{e}", "moving group failed: {e}")),
            }
        }
        Job::Stop { t } => match session::stop(&store, &t.id) {
            Ok(()) => Outcome::Done(tr!("已停止「{}」", "stopped “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("停止失败:{e}", "stop failed: {e}")),
        },
        Job::Park { p, .. } => match crate::park::park(p) {
            Ok(()) => Outcome::Done(tr!("已停「{}」 · ⏎ 再启动", "stopped “{}” · ⏎ starts it again", short(&p.label))),
            Err(e) => Outcome::Failed(tr!("停止失败:{e}", "stop failed: {e}")),
        },
        Job::Respawn { p, .. } => match crate::park::respawn(p) {
            Ok(()) => Outcome::Done(tr!("已重启「{}」", "restarted “{}”", short(&p.label))),
            Err(e) => Outcome::Failed(tr!("重启失败:{e}", "restart failed: {e}")),
        },
        Job::Send { t, pane, text, .. } => match crate::data::send_into(pane, text) {
            Ok(()) => Outcome::Done(tr!("已发给「{}」", "sent to “{}”", short(&t.title))),
            Err(e) => Outcome::Failed(tr!("没发出去:{e}", "not sent: {e}")),
        },
        Job::Fork { t, title } => match session::fork(&store, &cfg, &t.id, title.as_deref().unwrap_or("")) {
            Ok(s) => Outcome::Made { msg: tr!("已分出「{}」", "forked “{}”", short(&s.title)), id: s.id },
            Err(e) => Outcome::Failed(tr!("分叉失败:{e}", "fork failed: {e}")),
        },
        Job::Mcp { dir, on, off, .. } => crate::mcp::apply(dir, on, off),
        Job::Remove { t, pane } => {
            // 关的是窗格(会话还在)还是删会话:英文的动词变形不一样,整句分开写
            let close = pane.is_some();
            match crate::create::remove(t, pane.as_deref()) {
                Ok(Removed::Archived { was }) => {
                    Outcome::Trashed { msg: tr!("已删除「{}」 · ^Z 撤回", "deleted “{}” · ^Z undoes", short(&t.title)), was }
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
                    0 => tr!("已换到 {label},重启了", "moved to {label} and restarted"),
                    _ => tr!(
                        "已换到 {label},对话接着来 · 说一句「继续」它就接着干",
                        "moved to {label}, conversation resumed · say “continue” and it carries on"
                    ),
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
                    "已接管「{}」 · R 重启一次才带上 hook",
                    "adopted “{}” · restart once (R) to bring in the hooks",
                    short(plan.title())
                ),
                id: s.id,
            },
            Err(e) => Outcome::Failed(tr!("接管失败:{e}", "adopt failed: {e}")),
        },
        Job::Revive { t, unarchive, start } => revive(&store, t, *unarchive, *start),
        Job::Group(a) => crate::groups::job(a),
        Job::Plugins(a) => crate::plugins::job(a),
    }
}

fn revive(store: &Store, t: &Target, unarchive: bool, start: bool) -> Outcome {
    if unarchive && let Err(e) = session::archive(store, &t.id, false) {
        // 期间在别处真删掉了:这步没得撤了,别放回去挡着更早的
        if store.get(&t.id).is_err() {
            return Outcome::Done(tr!("「{}」已经真删了,撤不回", "“{}” is gone for good; can't undo", short(&t.title)));
        }
        return Outcome::Failed(tr!("撤回失败:{e}", "undo failed: {e}"));
    }
    let what = if unarchive { t!("删除", "delete") } else { t!("停止", "stop") };
    // 登记已经回来了,只是没起来:算撤成了(再撤一次也还是起不来),照实说
    if start && let Err(e) = crate::create::start(&t.id) {
        return Outcome::Done(tr!(
            "已撤销{what}「{}」,没启动起来:{e}",
            "undid the {what} of “{}”, but it didn't start: {e}",
            short(&t.title)
        ));
    }
    Outcome::Done(tr!("已撤销{what}「{}」", "undid the {what} of “{}”", short(&t.title)))
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
            return Err(t!("已经不在同一组了", "they're no longer in the same group").into());
        }
        let mut all: Vec<_> = store.sessions(true)?.into_iter().filter(|s| s.group == me.group).collect();
        all.sort_by_key(|s| (s.order, s.created));
        let ids = moved(all.into_iter().map(|s| s.id).collect(), &me.id, &it.id, after)
            .ok_or(t!("找不到要挪到的位置", "can't find where to move it"))?;
        renumber(&ids, |id, n| store.set_order(id, n))
    };
    match run() {
        Ok(()) => Outcome::Done(String::new()),
        Err(e) => Outcome::Failed(tr!("挪位置失败:{e}", "moving failed: {e}")),
    }
}

/// 分组上 ⇧↑ ⇧↓:挪到同级的 by 紧前面 / 紧后面,子分组跟着走(子分组排在哪只看上级)。
/// 同级的整个重新编号;别的层级的编号不动(它们只和自己的同级比)。做成了不说话
fn group_order(store: &Store, group: &str, by: &str, after: bool) -> Outcome {
    let Some(sibs) = crate::data::group_siblings(group) else {
        return Outcome::Failed(tr!("挪分组失败:没有 {group} 这个分组", "moving group failed: no group {group}"));
    };
    let Some(ids) = moved(sibs, group, by, after) else {
        return Outcome::Failed(tr!(
            "挪分组失败:{by} 和 {group} 不是同一级的",
            "moving group failed: {by} and {group} aren't on the same level"
        ));
    };
    match renumber(&ids, |g, n| store.group_order(g, n)) {
        Ok(()) => Outcome::Done(String::new()),
        Err(e) => Outcome::Failed(tr!("挪分组失败:{e}", "moving group failed: {e}")),
    }
}

/// 手打的分组名规整一下:两头的 / 和空白去掉,空白换成 -(分组名要能在命令行里直接打),空的一级去掉。什么都不剩 = None
pub fn clean_group(name: &str) -> Option<String> {
    let parts: Vec<String> =
        name.split('/').map(|p| p.split_whitespace().collect::<Vec<_>>().join("-")).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `m`:用 fzf 挑要挪进的分组。输入的名字一个都匹配不上时回车 = 用这个名字新建
pub fn pick_group(cands: &[String], title: &str) -> Result<Option<String>, String> {
    let header = tr!("--header=挪「{}」", "--header=move “{}”", short(title));
    fzf(cands, &[t!("--prompt=挪到 ", "--prompt=move to "), &header, "--bind=enter:accept-or-print-query"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_group_names_are_tidied() {
        assert_eq!(clean_group(" fun/new thing "), Some("fun/new-thing".into()));
        assert_eq!(clean_group("/a//b/"), Some("a/b".into()));
        assert_eq!(clean_group("工作  台"), Some("工作-台".into()));
        assert_eq!(clean_group(" / "), None);
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
        assert_eq!(order(&store, &t, "x", false), Outcome::Failed("挪位置失败:已经不在同一组了".into()));
    }
}
