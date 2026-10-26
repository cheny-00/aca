//! 建分组:`^N` 起名回车就建(`Act::Create`),列表里马上有它的标题,接着挑目录开会话;中途 Esc 组也留着。
//! 删分组:列表里停在分组上(标题 / 折叠行 / 空组)按 `d`,确认框说一起删掉什么,`y` 删(`Act::Delete`)。
//! 改分组:停在分组上按 `r`,先改名(可以带上级:`fun/aca`),再改默认目录(`Act::Edit`,见 `rename_all`)。
//!
//! 删分组 = 子分组和里面的会话(连子分组里的、归档着的)一起进回收站(见 trash.rs):
//! - 会话归档(在跑的停掉,一起等它们退干净),分组的登记删掉、照原样(先后、默认目录)记进回收站的那一条
//! - 1 天内 `^Z` / `b` 那一页 `⏎` 整条拿回来;过了 1 天会话才真删
//! - 删之前就归档着的(连 `d` 删了还在回收站里的)记成归档着的,拿回来还归档着
//!
//! 组在工作区里的记录、折叠是界面那边记进回收站 / 清掉的(`ui::job_done`)。

use std::collections::HashMap;

use aca_core::paths::Res;
use aca_core::store::Store;
use aca_core::{t, tr};

use crate::ops::Outcome;
use crate::trash::{Gone, GroupRec, Item};

/// 登记表里的一个分组
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub profile: String,
    /// 分组路径(`上级/名字`)
    pub path: String,
    /// 直接在这个组里的会话:没归档的
    pub live: usize,
    /// 归档着的(会话 id:回收站里的界面不算)
    pub archived: Vec<String>,
}

/// g 是 top 自己或者它下面的
pub fn within(g: &str, top: &str) -> bool {
    g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'))
}

/// 分组 g 改名(from → to)以后叫什么:是 from 自己或者它下面的才改,别的 None
pub fn renamed(g: &str, from: &str, to: &str) -> Option<String> {
    within(g, from).then(|| format!("{to}{}", &g[from.len()..]))
}

/// 改分组的路径(`aca group rename`、分组上 `r`):登记表里的组和会话,连界面记着组名的
/// (工作区里记的组、视图里在看的组和折起来的、便签)一起改。返回挪了几个会话
pub fn rename_all(store: &Store, from: &str, to: &str) -> Res<usize> {
    let n = store.group_rename(from, to)?;
    // 界面记的没改上不算失败:最多是这个组回到默认的工作区、折着的展开、便签留在原来的名字下
    let _ = crate::spaces::Spaces::open(crate::spaces::Spaces::default_path()).rename_group(crate::data::REGISTERED, from, to);
    let _ = crate::notes::Notes::open(crate::notes::Notes::default_path()).rename_group(from, to);
    if let Some(p) = crate::state::state_file("view") {
        let mut v = crate::state::View::load(&p);
        if v.rename_group(from, to) {
            let _ = v.save(&p);
        }
    }
    Ok(n)
}

/// 要做的
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// `^N` 起的名字:建这个分组(已经有了不动)
    Create { group: String },
    /// 删掉这个分组:子分组、里面的会话一起进回收站
    Delete { profile: String, group: String },
    /// 分组上 `r`:group 改成 to(一样 = 不改名);dir:默认目录 (原来的, 改成的),空 = 不记;None = 不改
    Edit { profile: String, group: String, to: String, dir: Option<(String, String)> },
}

impl Act {
    /// 跑的时候底栏显示的
    pub fn doing(&self) -> String {
        match self {
            Act::Create { group } => tr!("正在创建分组 {group}…", "creating group {group}…"),
            Act::Delete { group, .. } => tr!("正在删除分组 {group}…", "deleting group {group}…"),
            Act::Edit { group, .. } => tr!("正在修改分组 {group}…", "changing group {group}…"),
        }
    }

    /// 这个分组会不会被删掉(它自己和子分组)
    pub fn covers(&self, profile: &str, group: &str) -> bool {
        match self {
            Act::Delete { profile: p, group: top, .. } => p == profile && within(group, top),
            Act::Create { .. } | Act::Edit { .. } => false,
        }
    }

    /// 改分组的反过来做(^Z)
    pub fn undo(&self) -> Option<Act> {
        match self {
            Act::Create { .. } | Act::Delete { .. } => None,
            Act::Edit { profile, group, to, dir } => Some(Act::Edit {
                profile: profile.clone(),
                group: to.clone(),
                to: group.clone(),
                dir: dir.as_ref().map(|(was, now)| (now.clone(), was.clone())),
            }),
        }
    }
}

/// run 循环放进后台跑的(`Job::Group`)
pub fn job(a: &Act) -> Outcome {
    match a {
        Act::Delete { profile, group } => delete(profile, group),
        Act::Edit { group, to, dir, .. } => edit(group, to, dir.as_ref().map(|d| d.1.as_str())),
        // 建好了不另说(新建会话的框正开着);没建成照实说
        Act::Create { group } => match Store::open().and_then(|s| s.group_create(group, "")) {
            Ok(()) => Outcome::Done(String::new()),
            Err(e) => Outcome::Failed(tr!("创建分组失败:{e}", "creating the group failed: {e}")),
        },
    }
}

/// 删分组:先把要删的照原样记成回收站的一条,再归档会话(各等各的进程退干净,一起等),都停下了才删分组的登记。
/// 有没停下的:分组留着,那几个不算进这一条(它们还在组里列着),照实说
fn delete(profile: &str, group: &str) -> Outcome {
    let failed = |e: String| Outcome::Failed(tr!("删除分组失败:{e}", "deleting the group failed: {e}"));
    let store = match Store::open() {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let groups: Vec<GroupRec> = store
        .groups()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| within(&g.path, group))
        .map(|g| GroupRec { path: g.path, order: g.order, dir: g.default_path, space: None, note: String::new() })
        .collect();
    if groups.is_empty() {
        return Outcome::Done(tr!("分组 {group} 已不存在", "group {group} was already gone"));
    }
    let sessions = match store.sessions(true) {
        Ok(v) => v.into_iter().filter(|s| within(&s.group, group)).collect::<Vec<_>>(),
        Err(e) => return failed(e),
    };
    let stuck: Vec<(String, String)> = std::thread::scope(|sc| {
        let hs: Vec<_> = sessions
            .iter()
            .filter(|s| s.archived == 0)
            .map(|s| {
                sc.spawn(move || Store::open().and_then(|st| aca_core::session::archive(&st, &s.id, true)).err().map(|e| (s.id.clone(), e)))
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().unwrap_or_else(|_| Some((String::new(), "panic".into())))).collect()
    });
    let mut item = Item {
        id: crate::trash::new_id(),
        at: aca_core::paths::now(),
        profile: profile.into(),
        group: group.into(),
        groups,
        sessions: sessions
            .iter()
            .filter(|s| !stuck.iter().any(|(id, _)| *id == s.id))
            .map(|s| Gone {
                id: s.id.clone(),
                title: aca_core::status::title(s, &aca_core::runtime::load(&s.id)),
                group: s.group.clone(),
                archived: s.archived > 0,
            })
            .collect(),
    };
    if let Some((_, e)) = stuck.first() {
        item.groups.clear();
        let msg = tr!("{} 个会话未能删除,分组已保留:{e}", "{} sessions weren't deleted, the group stays: {e}", stuck.len());
        return if item.sessions.is_empty() { Outcome::Failed(msg) } else { Outcome::Binned { msg, item: Box::new(item) } };
    }
    if let Err(e) = store.group_drop(group) {
        return failed(e);
    }
    let n = item.taken();
    let msg = if n > 0 {
        tr!("已删除分组 {group} 和里面 {n} 个会话 · ^Z 撤销", "deleted group {group} and its {n} sessions · ^Z undoes")
    } else {
        tr!("已删除分组 {group} · ^Z 撤销", "deleted group {group} · ^Z undoes")
    };
    Outcome::Binned { msg, item: Box::new(item) }
}

/// 改名、改默认目录。目录先查(撞了别的组的就都不改),再改名,再记目录
fn edit(group: &str, to: &str, dir: Option<&str>) -> Outcome {
    let store = match Store::open() {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(tr!("修改分组失败:{e}", "changing the group failed: {e}")),
    };
    if let Some(d) = dir.filter(|d| !d.is_empty())
        && let Some(g) =
            store.groups().unwrap_or_default().into_iter().find(|g| {
                g.path != group && !g.default_path.is_empty() && aca_core::paths::real(&g.default_path) == aca_core::paths::real(d)
            })
    {
        return Outcome::Failed(tr!(
            "未修改:{} 已是分组 {} 的默认目录",
            "unchanged: {} is already the default directory of group {}",
            crate::create::tilde(d),
            g.path
        ));
    }
    let mut said = vec![];
    if to != group {
        if let Err(e) = rename_all(&store, group, to) {
            return Outcome::Failed(tr!("重命名分组失败:{e}", "renaming the group failed: {e}"));
        }
        said.push(tr!("分组 {group} 已重命名为 {to}", "group {group} renamed to {to}"));
    }
    if let Some(d) = dir {
        if let Err(e) = store.group_set_dir(to, d) {
            return Outcome::Failed(tr!(
                "{}默认目录修改失败:{e}",
                "{}default directory not changed: {e}",
                if said.is_empty() { "" } else { t!("已重命名,", "renamed, ") }
            ));
        }
        let d = if d.is_empty() { t!("已清除", "forgotten").to_string() } else { crate::create::tilde(d) };
        said.push(if said.is_empty() {
            tr!("分组 {to} 的默认目录:{d}", "default directory of group {to}: {d}")
        } else {
            tr!("默认目录:{d}", "default directory: {d}")
        });
    }
    Outcome::Done(said.join(" · "))
}

/// 登记表里的分组,按它的树排(父组紧跟着子组,同级按 sort_order 再按名字),带上组里有几个会话
pub fn load() -> Vec<Row> {
    let Ok(store) = Store::open() else { return vec![] };
    rows_of(&store)
}

fn rows_of(store: &Store) -> Vec<Row> {
    let order: HashMap<String, i64> = store.groups().unwrap_or_default().into_iter().map(|g| (g.path, g.order)).collect();
    let mut counts: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for s in store.sessions(true).unwrap_or_default() {
        let c = counts.entry(s.group).or_default();
        if s.archived > 0 { c.1.push(s.id) } else { c.0 += 1 }
    }
    let ranks = crate::data::group_ranks(order.keys().map(String::as_str), &order);
    let mut paths: Vec<&String> = order.keys().collect();
    paths.sort_by_key(|p| ranks[p.as_str()]);
    paths
        .into_iter()
        .map(|p| {
            let (live, archived) = counts.remove(p.as_str()).unwrap_or_default();
            Row { profile: crate::data::REGISTERED.into(), path: p.clone(), live, archived }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_act_knows_what_goes() {
        let a = Act::Delete { profile: "default".into(), group: "fun/app".into() };
        assert!(a.covers("default", "fun/app") && a.covers("default", "fun/app/old"));
        assert!(!a.covers("default", "fun") && !a.covers("default", "fun/apple") && !a.covers("firm", "fun/app"));
        assert_eq!(a.doing(), "正在删除分组 fun/app…");
        // 撤销走回收站(见 trash.rs),不是反过来做一遍
        assert_eq!(a.undo(), None);
    }

    #[test]
    fn a_rename_reaches_subgroups_only() {
        assert_eq!(renamed("fun", "fun", "play").as_deref(), Some("play"));
        assert_eq!(renamed("fun/app/old", "fun/app", "deck").as_deref(), Some("deck/old"));
        assert_eq!(renamed("fun/apple", "fun/app", "deck"), None);
        assert_eq!(renamed("fun", "fun/app", "deck"), None);
        // 撤销:名字、目录都反过来
        let a = Act::Edit { profile: "default".into(), group: "a".into(), to: "b/a".into(), dir: Some(("/x".into(), String::new())) };
        let back = Act::Edit { profile: "default".into(), group: "b/a".into(), to: "a".into(), dir: Some((String::new(), "/x".into())) };
        assert_eq!(a.undo(), Some(back));
        assert_eq!(a.doing(), "正在修改分组 a…");
        assert!(!a.covers("default", "a"));
    }

    #[test]
    fn spaces_and_the_saved_view_follow_a_renamed_group() {
        let dir = std::env::temp_dir().join(format!("aca-groups-rename-{}", std::process::id()));
        let path = dir.join("spaces");
        crate::state::write_atomic(&path, "group\tdefault\tfun/app\th\ngroup\tdefault\tfun/apple\t2\ngroup\t\t-\t3\n").unwrap();
        let mut sp = crate::spaces::Spaces::open(Some(path.clone()));
        sp.rename_group("default", "fun/app", "deck").unwrap();
        let again = crate::spaces::Spaces::open(Some(path));
        assert_eq!((again.group_of("default", "deck"), again.group_of("default", "fun/app")), (Some('h'), None));
        assert_eq!((again.group_of("default", "fun/apple"), again.group_of("", "-")), (Some('2'), Some('3')));
        let _ = std::fs::remove_dir_all(dir);
        let mut v =
            crate::state::View { group: Some("fun/app/old".into()), folded: vec!["fun".into(), "fun/app".into()], ..Default::default() };
        assert!(v.rename_group("fun/app", "deck"));
        assert_eq!((v.group.as_deref(), v.folded.as_slice()), (Some("deck/old"), &["fun".to_string(), "deck".to_string()][..]));
        assert!(!v.rename_group("nope", "x"));
    }

    #[test]
    fn rows_read_groups_and_counts_from_the_registry() {
        let store = Store::memory();
        let mk = |id: &str, group: &str, archived: u64| aca_core::store::Session {
            id: id.into(),
            title: id.into(),
            tool: "shell".into(),
            group: group.into(),
            dir: "/".into(),
            created: 1,
            archived,
            ..Default::default()
        };
        store.group_create("fun/app", "").unwrap();
        store.group_create("empty", "").unwrap();
        for s in [mk("a", "fun", 0), mk("b", "fun/app", 0), mk("c", "fun/app", 9)] {
            store.insert(&s).unwrap();
        }
        let got: Vec<(String, usize, Vec<String>)> = rows_of(&store).into_iter().map(|r| (r.path, r.live, r.archived)).collect();
        assert_eq!(got, [("fun".into(), 1, vec![]), ("fun/app".into(), 1, vec!["c".to_string()]), ("empty".into(), 0, vec![])]);
    }
}
