//! Creating a group: `^N`, type a name, ⏎ creates it (`Act::Create`); its title is in the list right away, then you pick a
//! directory to open a session; Esc midway still keeps the group.
//! Deleting a group: on a group in the list (title / fold row / empty group) press `d`; the confirm box says what goes with it;
//! `y` deletes (`Act::Delete`).
//! Editing a group: on a group press `r`: rename first (a parent may be included: `fun/aca`), then change the default directory
//! (`Act::Edit`, see `rename_all`).
//!
//! Deleting a group = its subgroups and the sessions in them (including those in subgroups, and archived ones) go to the trash
//! together (see trash.rs):
//! - sessions are archived (running ones stopped, all waited on together to exit cleanly); the group's registration is deleted
//!   and recorded as it was (order, default directory) in that trash entry
//! - within 1 day, `^Z` / `⏎` on the `b` page brings the whole entry back; only after 1 day are the sessions really deleted
//! - sessions already archived before the delete (including ones `d` deleted that are still in the trash) are recorded as
//!   archived and come back archived
//!
//! The group's workspace record and fold state are put in the trash / cleared by the interface (`ui::job_done`).

use std::collections::HashMap;

use aca_core::paths::Res;
use aca_core::store::Store;
use aca_core::{t, tr};

use crate::ops::Outcome;
use crate::trash::{Gone, GroupRec, Item};

/// A group in the registry
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub profile: String,
    /// The group path (`parent/name`)
    pub path: String,
    /// Sessions directly in this group: not archived
    pub live: usize,
    /// Archived ones (session ids; the interface doesn't count the ones in the trash)
    pub archived: Vec<String>,
}

/// g is top itself or under it
pub fn within(g: &str, top: &str) -> bool {
    g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'))
}

/// What group g is called after a rename (from → to): changed only if it's from itself or under it; otherwise None
pub fn renamed(g: &str, from: &str, to: &str) -> Option<String> {
    within(g, from).then(|| format!("{to}{}", &g[from.len()..]))
}

/// Change a group's path (`aca group rename`, `r` on a group): the groups and sessions in the registry, along with the places
/// the interface remembers group names (groups recorded in workspaces, the group being viewed and the folded ones in the view,
/// notes). Returns how many sessions moved
pub fn rename_all(store: &Store, from: &str, to: &str) -> Res<usize> {
    let n = store.group_rename(from, to)?;
    // The interface's records not updating isn't a failure: at worst this group goes back to the default workspace, folded ones
    // unfold, notes stay under the old name
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

/// What to do
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// The name typed at `^N`: create this group (left alone if it exists)
    Create { group: String },
    /// Delete this group: its subgroups and the sessions in them go to the trash together
    Delete { profile: String, group: String },
    /// `r` on a group: group becomes to (the same = no rename); dir: the default directory (old, new), empty = none; None = unchanged
    Edit { profile: String, group: String, to: String, dir: Option<(String, String)> },
}

impl Act {
    /// Shown in the bottom bar while running
    pub fn doing(&self) -> String {
        match self {
            Act::Create { group } => tr!("正在创建分组 {group}…", "creating group {group}…"),
            Act::Delete { group, .. } => tr!("正在删除分组 {group}…", "deleting group {group}…"),
            Act::Edit { group, .. } => tr!("正在修改分组 {group}…", "changing group {group}…"),
        }
    }

    /// Whether this group gets deleted (itself and its subgroups)
    pub fn covers(&self, profile: &str, group: &str) -> bool {
        match self {
            Act::Delete { profile: p, group: top, .. } => p == profile && within(group, top),
            Act::Create { .. } | Act::Edit { .. } => false,
        }
    }

    /// An edit to a group, done in reverse (^Z)
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

/// What the run loop runs in the background (`Job::Group`)
pub fn job(a: &Act) -> Outcome {
    match a {
        Act::Delete { profile, group } => delete(profile, group),
        Act::Edit { group, to, dir, .. } => edit(group, to, dir.as_ref().map(|d| d.1.as_str())),
        // Created: nothing more to say (the new-session box is open); not created: say so plainly
        Act::Create { group } => match Store::open().and_then(|s| s.group_create(group, "")) {
            Ok(()) => Outcome::Done(String::new()),
            Err(e) => Outcome::Failed(tr!("创建分组失败:{e}", "creating the group failed: {e}")),
        },
    }
}

/// Delete a group: first record what goes, as it is, as one trash entry; then archive the sessions (each waits for its own
/// process to exit cleanly, all waited on together); only when all have stopped is the group's registration deleted.
/// Any that didn't stop: the group stays, those aren't in this entry (they're still listed in the group); say so plainly
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

/// Rename, change the default directory. The directory is checked first (if it clashes with another group's, nothing changes),
/// then the rename, then the directory is recorded
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

/// The registry's groups in tree order (each parent followed by its subgroups, siblings by sort_order then name), with how many
/// sessions each has
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
        // Undo goes through the trash (see trash.rs), not by doing it in reverse
        assert_eq!(a.undo(), None);
    }

    #[test]
    fn a_rename_reaches_subgroups_only() {
        assert_eq!(renamed("fun", "fun", "play").as_deref(), Some("play"));
        assert_eq!(renamed("fun/app/old", "fun/app", "deck").as_deref(), Some("deck/old"));
        assert_eq!(renamed("fun/apple", "fun/app", "deck"), None);
        assert_eq!(renamed("fun", "fun/app", "deck"), None);
        // Undo: both name and directory reversed
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
