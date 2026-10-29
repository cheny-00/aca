//! The registry: which sessions there are, which group each is in, which tool and account it uses, which conversation it resumes.
//! One SQLite file.
//!
//! Only **what people and the CLI change** goes here (create, rename, move groups, order, archive). State the agent changes every
//! turn (running / needs you / finished, the current conversation id, the auto title) stays out of the database; hooks write it
//! to each session's own file (runtime.rs): hooks run on every event, and a dozen sessions writing one database would wait on
//! each other's locks.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::paths::{self, Res};
use crate::{t, tr};

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    /// Renamed by a person: no longer follows the title the agent sets
    pub title_locked: bool,
    pub group: String,
    pub order: i64,
    /// claude / codex / shell, or one defined by `[tools.<name>]` in config.toml
    pub tool: String,
    pub account: String,
    pub dir: String,
    /// Extra arguments added at every start
    pub args: Vec<String>,
    /// The conversation id at registration; if it changes later (/clear, compaction), what the hook reports in runtime wins
    pub conv: String,
    /// Who delegated it (`aca new` run inside some session, or named with `--parent`): notices, answering for it, and what it hangs
    /// under in the list all go by this
    pub parent: String,
    /// Forked: which session it was forked from. Not `parent` — a fork is another path you continue yourself, a peer of the original
    /// session
    pub fork_of: String,
    pub created: u64,
    pub archived: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Group {
    pub path: String,
    pub order: i64,
    pub default_path: String,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
create table if not exists sessions(
  id text primary key,
  title text not null,
  title_locked integer not null default 0,
  group_path text not null default '',
  sort_order integer not null default 0,
  tool text not null,
  account text not null default '',
  dir text not null,
  args text not null default '[]',
  conv text not null default '',
  parent text not null default '',
  created_at integer not null,
  archived_at integer not null default 0,
  fork_of text not null default ''
);
create table if not exists groups(
  path text primary key,
  sort_order integer not null default 0,
  default_path text not null default ''
);
";

const COLS: &str =
    "id, title, title_locked, group_path, sort_order, tool, account, dir, args, conv, parent, created_at, archived_at, fork_of";

/// Registry version (`pragma user_version`). 2: adds `fork_of`
const VERSION: i64 = 2;

fn e(err: rusqlite::Error) -> String {
    tr!("登记表:{err}", "registry: {err}")
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        title: r.get(1)?,
        title_locked: r.get::<_, i64>(2)? != 0,
        group: r.get(3)?,
        order: r.get(4)?,
        tool: r.get(5)?,
        account: r.get(6)?,
        dir: r.get(7)?,
        args: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        conv: r.get(9)?,
        parent: r.get(10)?,
        created: r.get::<_, i64>(11)? as u64,
        archived: r.get::<_, i64>(12)? as u64,
        fork_of: r.get(13)?,
    })
}

impl Store {
    pub fn open() -> Res<Store> {
        let path = paths::db();
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| tr!("无法创建 {}:{e}", "can't create {}: {e}", path.display()))?;
        Store::at(Connection::open(&path).map_err(e)?)
    }

    pub fn memory() -> Store {
        Store::at(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn at(conn: Connection) -> Res<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(3)).map_err(e)?;
        // A database already at this version doesn't run the table creation again: an ordinary open is just one pragma
        let v: i64 = conn.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
        if v < 1 {
            conn.execute_batch(&format!("pragma journal_mode = wal; {SCHEMA} pragma user_version = {VERSION};")).map_err(e)?;
            return Ok(Store { conn });
        }
        let store = Store { conn };
        if v < VERSION {
            // Forks used to be recorded in parent, indistinguishable from delegated workers; the recognizable ones are those whose runtime
            // records which conversation they forked from (`Run::fork_from`, written at fork time and kept)
            let moved = add_fork_of(&store.conn, |id| !crate::runtime::load(id).fork_from.is_empty())?;
            for (id, from) in moved {
                let _ = store.place_after(&id, &from);
            }
        }
        Ok(store)
    }

    /// Not archived, in their order within the group
    pub fn sessions(&self, archived: bool) -> Res<Vec<Session>> {
        let sql =
            format!("select {COLS} from sessions {} order by sort_order, created_at", if archived { "" } else { "where archived_at = 0" });
        let mut st = self.conn.prepare_cached(&sql).map_err(e)?;
        let v = st.query_map([], row).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
        Ok(v)
    }

    /// Find by id: the full id, its first few characters (matching only one), or an identical title
    pub fn get(&self, key: &str) -> Res<Session> {
        let one = |sql: &str, k: &str| -> Res<Vec<Session>> {
            let mut st = self.conn.prepare_cached(sql).map_err(e)?;
            let v = st.query_map([k], row).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
            Ok(v)
        };
        if key.is_empty() {
            return Err(t!("需要会话 id", "missing a session id").into());
        }
        let mut hit = one(&format!("select {COLS} from sessions where id = ?1"), key)?;
        if hit.is_empty() {
            hit = one(&format!("select {COLS} from sessions where substr(id, 1, length(?1)) = ?1"), key)?;
        }
        if hit.is_empty() {
            hit = one(&format!("select {COLS} from sessions where title = ?1 and archived_at = 0"), key)?;
        }
        match hit.len() {
            0 => Err(tr!("没有会话 {key}", "no session {key}")),
            1 => Ok(hit.remove(0)),
            n => Err(tr!("{key} 匹配到 {n} 个会话,请写得更完整", "{key} matches {n} sessions; type more of it")),
        }
    }

    pub fn insert(&self, s: &Session) -> Res<()> {
        self.conn
            .execute(
                &format!("insert into sessions({COLS}) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"),
                params![
                    s.id,
                    s.title,
                    s.title_locked as i64,
                    s.group,
                    s.order,
                    s.tool,
                    s.account,
                    s.dir,
                    serde_json::to_string(&s.args).unwrap(),
                    s.conv,
                    s.parent,
                    s.created as i64,
                    s.archived as i64,
                    s.fork_of
                ],
            )
            .map_err(e)?;
        if !s.group.is_empty() {
            self.group_create(&s.group, "")?;
        }
        Ok(())
    }

    fn set(&self, id: &str, col: &str, v: &dyn rusqlite::ToSql) -> Res<()> {
        let n = self.conn.execute(&format!("update sessions set {col} = ?2 where id = ?1"), params![id, v]).map_err(e)?;
        if n == 1 { Ok(()) } else { Err(tr!("没有会话 {id}", "no session {id}")) }
    }

    pub fn set_title(&self, id: &str, title: &str, locked: bool) -> Res<()> {
        self.set(id, "title", &title)?;
        self.set(id, "title_locked", &(locked as i64))
    }

    pub fn set_conv(&self, id: &str, conv: &str) -> Res<()> {
        self.set(id, "conv", &conv)
    }

    /// Change the account (with the tool: another tool of the same kind works too)
    pub fn set_account(&self, id: &str, tool: &str, account: &str) -> Res<()> {
        self.set(id, "tool", &tool)?;
        self.set(id, "account", &account)
    }

    pub fn set_args(&self, id: &str, args: &[String]) -> Res<()> {
        self.set(id, "args", &serde_json::to_string(args).unwrap())
    }

    pub fn set_order(&self, id: &str, order: i64) -> Res<()> {
        self.set(id, "sort_order", &order)
    }

    pub fn set_archived(&self, id: &str, at: u64) -> Res<()> {
        self.set(id, "archived_at", &(at as i64))
    }

    /// Move to another group (created if missing), placed last in that group
    pub fn set_group(&self, id: &str, group: &str) -> Res<()> {
        if !group.is_empty() {
            self.group_create(group, "")?;
        }
        let last: i64 = self
            .conn
            .query_row("select coalesce(max(sort_order), 0) + 1 from sessions where group_path = ?1", [group], |r| r.get(0))
            .map_err(e)?;
        self.set(id, "group_path", &group)?;
        self.set(id, "sort_order", &last)
    }

    /// Place right after `by`: the whole group is renumbered (archived ones included, keeping relative positions). Nothing moves if
    /// they aren't in the same group
    pub fn place_after(&self, id: &str, by: &str) -> Res<()> {
        let (me, it) = (self.get(id)?, self.get(by)?);
        if me.group != it.group || me.id == it.id {
            return Ok(());
        }
        let mut ids: Vec<String> =
            self.sessions(true)?.into_iter().filter(|s| s.group == me.group && s.id != me.id).map(|s| s.id).collect();
        let at = ids.iter().position(|x| *x == it.id).map_or(ids.len(), |p| p + 1);
        ids.insert(at, me.id);
        let tx = self.conn.unchecked_transaction().map_err(e)?;
        for (n, id) in ids.iter().enumerate() {
            tx.execute("update sessions set sort_order = ?2 where id = ?1", params![id, n as i64 + 1]).map_err(e)?;
        }
        tx.commit().map_err(e)
    }

    pub fn remove(&self, id: &str) -> Res<()> {
        self.conn.execute("delete from sessions where id = ?1", [id]).map(|_| ()).map_err(e)
    }

    /// A new session goes last in its group
    pub fn next_order(&self, group: &str) -> i64 {
        self.conn
            .query_row("select coalesce(max(sort_order), 0) + 1 from sessions where group_path = ?1", [group], |r| r.get(0))
            .unwrap_or(1)
    }

    pub fn groups(&self) -> Res<Vec<Group>> {
        let mut st = self.conn.prepare_cached("select path, sort_order, default_path from groups order by sort_order, path").map_err(e)?;
        let v = st
            .query_map([], |r| Ok(Group { path: r.get(0)?, order: r.get(1)?, default_path: r.get(2)? }))
            .map_err(e)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(e)?;
        Ok(v)
    }

    /// Left alone if it exists (a missing default directory gets filled in); `a/b` creates the parent `a` too
    pub fn group_create(&self, path: &str, default_path: &str) -> Res<()> {
        let mut upto = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !upto.is_empty() {
                upto.push('/');
            }
            upto.push_str(part);
            self.conn
                .execute(
                    "insert into groups(path, sort_order) values(?1, (select coalesce(max(sort_order), 0) + 1 from groups)) on conflict do nothing",
                    [&upto],
                )
                .map_err(e)?;
        }
        if !default_path.is_empty() {
            self.conn.execute("update groups set default_path = ?2 where path = ?1", [path, default_path]).map_err(e)?;
        }
        Ok(())
    }

    pub fn group_order(&self, path: &str, order: i64) -> Res<()> {
        let n = self.conn.execute("update groups set sort_order = ?2 where path = ?1", params![path, order]).map_err(e)?;
        if n == 1 { Ok(()) } else { Err(tr!("没有分组 {path}", "no group {path}")) }
    }

    /// Remove a group: its sessions (including those in subgroups) move up one level
    pub fn group_remove(&self, path: &str) -> Res<usize> {
        let exists: Option<i64> =
            self.conn.query_row("select 1 from groups where path = ?1", [path], |r| r.get(0)).optional().map_err(e)?;
        if exists.is_none() {
            return Err(tr!("没有分组 {path}", "no group {path}"));
        }
        let up = path.rsplit_once('/').map_or("", |x| x.0);
        let sub = format!("{path}/%");
        let moved = self
            .conn
            .execute("update sessions set group_path = ?3 where group_path = ?1 or group_path like ?2", params![path, sub, up])
            .map_err(e)?;
        self.conn.execute("delete from groups where path = ?1 or path like ?2", params![path, sub]).map_err(e)?;
        Ok(moved)
    }

    /// Only drop the group's registration (with subgroups); its sessions are untouched (when the interface removes a group, its sessions
    /// are already archived along with it, see aca-tui's trash.rs). Returns how many groups were dropped
    pub fn group_drop(&self, path: &str) -> Res<usize> {
        // No like: _ and % in a group name would be taken as wildcards
        self.conn
            .execute("delete from groups where path = ?1 or substr(path, 1, length(?2)) = ?2", params![path, format!("{path}/")])
            .map_err(e)
    }

    /// Decide the group by directory (`-g .`), only looking, not creating. Rules in [`group_by_dir`]
    pub fn dir_group(&self, dir: &str) -> Res<(String, bool)> {
        let groups: Vec<(String, String)> = self.groups()?.into_iter().map(|g| (g.path, paths::real(&g.default_path))).collect();
        group_by_dir(&groups, &paths::real(dir))
    }

    /// Decide the group by directory (see [`Store::dir_group`]), creating it if missing; a group with no default directory yet gets
    /// this directory
    pub fn group_claim(&self, dir: &str) -> Res<(String, bool)> {
        let (g, new) = self.dir_group(dir)?;
        self.group_create(&g, "")?;
        self.conn
            .execute("update groups set default_path = ?2 where path = ?1 and default_path = ''", [&g, &paths::real(dir)])
            .map_err(e)?;
        Ok((g, new))
    }

    /// Change a group's path (rename it, or move it under another parent): subgroups and its sessions (archived ones too) follow,
    /// default directory and order unchanged; a missing parent of the new path is created. Returns how many sessions moved
    pub fn group_rename(&self, from: &str, to: &str) -> Res<usize> {
        let (from, to) = (from.trim_matches('/'), to.trim_matches('/'));
        let groups = self.groups()?;
        if !groups.iter().any(|g| g.path == from) {
            return Err(tr!("没有分组 {from}", "no group {from}"));
        }
        if to.is_empty() {
            return Err(t!("新分组名为空", "the new group name is empty").into());
        }
        if to == from {
            return Ok(0);
        }
        if to.strip_prefix(from).is_some_and(|r| r.starts_with('/')) {
            return Err(tr!("{from} 不能移到它自己下面", "{from} can't move under itself"));
        }
        // A case difference still counts as existing; changing only the case (Work → work) isn't a clash
        if let Some(g) = groups.iter().find(|g| g.path.eq_ignore_ascii_case(to) && g.path != from) {
            return Err(tr!("分组 {} 已存在", "group {} already exists", g.path));
        }
        // Itself and "it/…"; no like: _ and % in a group name would be taken as wildcards
        let tx = self.conn.unchecked_transaction().map_err(e)?;
        tx.execute(
            "update groups set path = ?2 || substr(path, length(?1) + 1)
             where path = ?1 or substr(path, 1, length(?1) + 1) = ?1 || '/'",
            [from, to],
        )
        .map_err(e)?;
        let moved = tx
            .execute(
                "update sessions set group_path = ?2 || substr(group_path, length(?1) + 1)
                 where group_path = ?1 or substr(group_path, 1, length(?1) + 1) = ?1 || '/'",
                [from, to],
            )
            .map_err(e)?;
        tx.commit().map_err(e)?;
        // Moved under a parent that doesn't exist yet: create the parent
        self.group_create(to, "")?;
        Ok(moved)
    }

    /// Change a group's default directory (empty = no longer recorded). A directory is the default of only one group: `-g .` finds
    /// the group by it
    pub fn group_set_dir(&self, path: &str, dir: &str) -> Res<()> {
        if !dir.is_empty() {
            let real = paths::real(dir);
            if let Some(g) =
                self.groups()?.iter().find(|g| g.path != path && !g.default_path.is_empty() && paths::real(&g.default_path) == real)
            {
                return Err(tr!("{dir} 已是分组 {} 的默认目录", "{dir} is already the default directory of group {}", g.path));
            }
        }
        let n = self.conn.execute("update groups set default_path = ?2 where path = ?1", [path, dir]).map_err(e)?;
        if n == 1 { Ok(()) } else { Err(tr!("没有分组 {path}", "no group {path}")) }
    }
}

/// Upgrade a version 1 database to version 2: add `fork_of`, and move `parent` there for the sessions `is_fork` recognizes (nobody
/// delegated them). Returns the ones moved: (session, forked from). When two processes upgrade at once, the later waits for the
/// first to finish writing, sees it's already upgraded and does nothing
fn add_fork_of(conn: &Connection, is_fork: impl Fn(&str) -> bool) -> Res<Vec<(String, String)>> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let v: i64 = tx.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
    if v >= VERSION {
        return Ok(vec![]);
    }
    tx.execute_batch("alter table sessions add column fork_of text not null default '';").map_err(e)?;
    let kids: Vec<(String, String)> = {
        let mut st = tx.prepare("select id, parent from sessions where parent != ''").map_err(e)?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.collect::<rusqlite::Result<_>>().map_err(e)?
    };
    let moved: Vec<(String, String)> = kids.into_iter().filter(|(id, _)| is_fork(id)).collect();
    for (id, _) in &moved {
        tx.execute("update sessions set fork_of = parent, parent = '' where id = ?1", [id]).map_err(e)?;
    }
    tx.execute_batch(&format!("pragma user_version = {VERSION};")).map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(moved)
}

/// Decide the group by directory: `aca new -g .`, `aca move <id> .`, `aca group add`, adopting and the interface's `N` all use this
/// one, only looking, not creating. `groups` is (group path, default directory); `dir` and the default directories both go
/// through [`paths::real`] first. Returns (group, the group doesn't exist yet):
/// 1. A group whose default directory is this one → that group
/// 2. The directory name is already a group: a top-level one with that name, or the only nested group ending in it (…/dirname)
///    → that group
/// 3. The parent directory's name is a group (top-level with that name, or the only nested group ending in it) → nested as
///    "that group/dirname"
/// 4. None of these → a top-level "dirname"
/// 5. The group decided by 2–4 records another directory: number onward, "…-2", "…-3" (under the same parent); create it if
///    missing, and use one that exists without a directory
///
/// A case difference still counts as the same group (as when moving groups); whitespace in the directory name becomes -
pub fn group_by_dir(groups: &[(String, String)], dir: &str) -> Res<(String, bool)> {
    if let Some((g, _)) = groups.iter().find(|(_, d)| !d.is_empty() && d == dir) {
        return Ok((g.clone(), false));
    }
    let name =
        |p: Option<&std::ffi::OsStr>| p.map(|f| f.to_string_lossy().split_whitespace().collect::<Vec<_>>().join("-")).unwrap_or_default();
    let path = std::path::Path::new(dir);
    let leaf = name(path.file_name());
    if leaf.is_empty() {
        return Err(tr!("{dir} 没有目录名,无法确定分组名", "{dir} has no directory name to name a group after"));
    }
    let parent = name(path.parent().and_then(|p| p.file_name()));
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    // A top-level one with that name, or the only nested group "…/name"
    let named = |n: &str| -> Option<String> {
        if n.is_empty() {
            return None;
        }
        if let Some((g, _)) = groups.iter().find(|(g, _)| same(g, n)) {
            return Some(g.clone());
        }
        let tail = format!("/{}", n.to_ascii_lowercase());
        let mut m: Vec<&String> = groups.iter().map(|(g, _)| g).filter(|g| g.to_ascii_lowercase().ends_with(&tail)).collect();
        m.dedup();
        (m.len() == 1).then(|| m[0].clone())
    };
    let base = named(&leaf).or_else(|| named(&parent).map(|p| format!("{p}/{leaf}"))).unwrap_or(leaf);
    for n in 1.. {
        let want = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        match groups.iter().find(|(g, _)| same(g, &want)) {
            None => return Ok((want, true)),
            Some((g, d)) if d.is_empty() => return Ok((g.clone(), false)),
            Some(_) => {}
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, title: &str, group: &str) -> Session {
        Session {
            id: id.into(),
            title: title.into(),
            group: group.into(),
            tool: "claude".into(),
            dir: "/p".into(),
            created: 1,
            ..Default::default()
        }
    }

    #[test]
    fn a_session_is_found_by_id_prefix_or_title() {
        let st = Store::memory();
        st.insert(&Session { args: vec!["--model".into(), "haiku".into()], ..s("ab12cd34", "修登录", "work/app") }).unwrap();
        st.insert(&s("ab99ffff", "别的", "")).unwrap();
        assert_eq!(st.get("ab12cd34").unwrap().args, ["--model", "haiku"]);
        assert_eq!(st.get("ab12").unwrap().title, "修登录");
        assert_eq!(st.get("修登录").unwrap().id, "ab12cd34");
        assert!(st.get("ab").unwrap_err().contains("2 个"));
        assert!(st.get("zz").unwrap_err().contains("没有"));
        // Creating a session also created the group along with its parents
        let g: Vec<String> = st.groups().unwrap().into_iter().map(|g| g.path).collect();
        assert_eq!(g, ["work", "work/app"]);
    }

    #[test]
    fn moving_puts_a_session_last_and_archived_ones_are_hidden() {
        let st = Store::memory();
        for (i, id) in ["a1", "b2", "c3"].iter().enumerate() {
            st.insert(&Session { order: i as i64 + 1, ..s(id, id, "g") }).unwrap();
        }
        st.set_group("a1", "h").unwrap();
        st.set_group("b2", "h").unwrap();
        let order: Vec<(String, i64)> =
            st.sessions(false).unwrap().into_iter().filter(|x| x.group == "h").map(|x| (x.id, x.order)).collect();
        assert_eq!(order, [("a1".to_string(), 1), ("b2".to_string(), 2)]);
        st.set_archived("c3", 9).unwrap();
        assert_eq!(st.sessions(false).unwrap().len(), 2);
        assert_eq!(st.sessions(true).unwrap().len(), 3);
        st.set_title("a1", "新名字", true).unwrap();
        assert!(st.get("a1").unwrap().title_locked);
        st.set_args("a1", &["--model".to_string(), "opus".to_string()]).unwrap();
        assert_eq!(st.get("a1").unwrap().args, ["--model", "opus"]);
        assert!(st.set_conv("nope", "x").is_err());
    }

    #[test]
    fn placing_after_another_renumbers_the_group_and_keeps_the_rest_in_order() {
        let st = Store::memory();
        // a and b have the same sort_order (adopted ones can): ordered by creation time
        for (id, order, created) in [("a1", 1, 1), ("b2", 1, 2), ("c3", 2, 3), ("d4", 3, 4)] {
            st.insert(&Session { order, created, ..s(id, id, "g") }).unwrap();
        }
        st.insert(&Session { order: 1, ..s("x9", "x", "h") }).unwrap();
        st.set_archived("c3", 9).unwrap();
        let order =
            |st: &Store| -> Vec<String> { st.sessions(true).unwrap().into_iter().filter(|x| x.group == "g").map(|x| x.id).collect() };
        st.place_after("d4", "a1").unwrap();
        assert_eq!(order(&st), ["a1", "d4", "b2", "c3"]);
        st.place_after("a1", "c3").unwrap();
        assert_eq!(order(&st), ["d4", "b2", "c3", "a1"]);
        // Not in the same group, or after itself: nothing moves
        st.place_after("d4", "x9").unwrap();
        st.place_after("d4", "d4").unwrap();
        assert_eq!(order(&st), ["d4", "b2", "c3", "a1"]);
        assert_eq!(st.get("x9").unwrap().order, 1);
        assert!(st.place_after("d4", "nope").is_err());
    }

    /// A version 1 database (no fork_of): forks were recorded in parent. Upgrading: the ones recognized as forks move to fork_of,
    /// delegated workers stay
    #[test]
    fn an_older_registry_gains_fork_of_and_forks_stop_being_children() {
        let v1 = SCHEMA.replace(",\n  fork_of text not null default ''", "");
        assert_ne!(v1, SCHEMA);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{v1} pragma user_version = 1;")).unwrap();
        let add = |id: &str, parent: &str, order: i64| {
            conn.execute(
                "insert into sessions(id, title, group_path, sort_order, tool, dir, parent, created_at) values(?1, ?1, 'g', ?2, 'claude', '/p', ?3, ?2)",
                params![id, order, parent],
            )
            .unwrap();
        };
        add("lead", "", 1);
        add("other", "", 2);
        add("worker", "lead", 3);
        add("fork", "lead", 4);
        assert_eq!(add_fork_of(&conn, |id| id == "fork").unwrap(), [("fork".to_string(), "lead".to_string())]);
        // Already upgraded: running it again does nothing
        assert!(add_fork_of(&conn, |_| true).unwrap().is_empty());
        let st = Store::at(conn).unwrap();
        let of = |id: &str| st.get(id).map(|x| (x.parent, x.fork_of)).unwrap();
        assert_eq!(of("fork"), (String::new(), "lead".into()));
        assert_eq!(of("worker"), ("lead".into(), String::new()));
        assert_eq!(of("lead"), (String::new(), String::new()));
        // Newly written rows carry this column
        st.insert(&Session { fork_of: "other".into(), ..s("f2", "f2", "g") }).unwrap();
        assert_eq!(st.get("f2").unwrap().fork_of, "other");
    }

    /// An old database with no forks and no children: upgraded on open (nobody's runtime needs reading)
    #[test]
    fn opening_an_older_registry_upgrades_it_in_place() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{} pragma user_version = 1;", SCHEMA.replace(",\n  fork_of text not null default ''", ""))).unwrap();
        conn.execute("insert into sessions(id, title, tool, dir, created_at) values('a1', 'a', 'claude', '/p', 1)", []).unwrap();
        let st = Store::at(conn).unwrap();
        assert_eq!(st.get("a1").unwrap().fork_of, "");
        assert_eq!(st.conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap(), VERSION);
    }

    #[test]
    fn removing_a_group_moves_its_sessions_up() {
        let st = Store::memory();
        st.insert(&s("a1", "a", "work/app")).unwrap();
        st.insert(&s("b2", "b", "work/app/deep")).unwrap();
        st.insert(&s("c3", "c", "work")).unwrap();
        assert_eq!(st.group_remove("work/app").unwrap(), 2);
        assert!(st.sessions(false).unwrap().iter().all(|x| x.group == "work"));
        assert_eq!(st.groups().unwrap().len(), 1);
        assert!(st.group_remove("work/app").is_err());
    }

    #[test]
    fn dropping_a_group_leaves_its_sessions_where_they_were() {
        let st = Store::memory();
        for g in ["my_app/deep", "myXapp", "my_apple"] {
            st.group_create(g, "").unwrap();
        }
        st.insert(&s("a1", "a", "my_app/deep")).unwrap();
        // Subgroups are dropped too; _ in a name isn't a wildcard
        assert_eq!(st.group_drop("my_app").unwrap(), 2);
        assert_eq!(st.groups().unwrap().into_iter().map(|g| g.path).collect::<Vec<_>>(), ["myXapp", "my_apple"]);
        assert_eq!(st.get("a1").unwrap().group, "my_app/deep");
        assert_eq!(st.group_drop("my_app").unwrap(), 0);
    }

    /// Directories for the test: really created (grouping by directory resolves to real paths)
    fn dirs(names: &[&str]) -> (std::path::PathBuf, Vec<String>) {
        let root = std::env::temp_dir().join(format!("aca-store-dirs-{}-{}", std::process::id(), paths::short_id()));
        let v = names
            .iter()
            .map(|n| {
                let d = root.join(n);
                std::fs::create_dir_all(&d).unwrap();
                paths::real(&d.to_string_lossy())
            })
            .collect();
        (root, v)
    }

    fn default_dir(st: &Store, g: &str) -> String {
        st.groups().unwrap().into_iter().find(|x| x.path == g).map(|x| x.default_path).unwrap()
    }

    #[test]
    fn a_dir_gets_the_group_named_after_it_numbered_when_taken() {
        let st = Store::memory();
        let (root, d) = dirs(&["a/aca", "b/aca", "c/aca", "My Proj"]);
        // Missing → created, with the directory as its default; again gives the same one
        assert_eq!(st.group_claim(&d[0]).unwrap(), ("aca".into(), true));
        assert_eq!(default_dir(&st, "aca"), d[0]);
        assert_eq!(st.group_claim(&format!("{}/", d[0])).unwrap(), ("aca".into(), false));
        // One with the same name records another directory → numbered onward
        assert_eq!(st.dir_group(&d[1]).unwrap(), ("aca-2".into(), true));
        assert_eq!(st.group_claim(&d[1]).unwrap(), ("aca-2".into(), true));
        assert_eq!(st.group_claim(&d[2]).unwrap(), ("aca-3".into(), true));
        assert_eq!(st.group_claim(&d[1]).unwrap(), ("aca-2".into(), false));
        // One with the same name and no directory (a case difference counts too): use it and record it
        st.group_create("My-proj", "").unwrap();
        assert_eq!(st.group_claim(&d[3]).unwrap(), ("My-proj".into(), false));
        assert_eq!(default_dir(&st, "My-proj"), d[3]);
        // Only looks, doesn't create
        let n = st.groups().unwrap().len();
        let (_, more) = dirs(&["z/fresh"]);
        assert_eq!(st.dir_group(&more[0]).unwrap(), ("fresh".into(), true));
        assert_eq!(st.groups().unwrap().len(), n);
        assert!(st.dir_group("/").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// The interface's N and -g . are the same logic: if the directory name or the parent directory's name is an existing group (top
    /// level, or the only nested one), join it / nest under it; if the group decided on records another directory, number onward
    #[test]
    fn a_dir_joins_or_nests_under_an_existing_group_and_is_numbered_when_taken() {
        let g = |v: &[(&str, &str)], dir: &str| {
            let groups: Vec<(String, String)> = v.iter().map(|(g, d)| (g.to_string(), d.to_string())).collect();
            group_by_dir(&groups, dir).unwrap()
        };
        let h = [
            ("fun", ""),
            ("work", ""),
            ("work/api", ""),
            ("a/dup", ""),
            ("b/dup", ""),
            ("notes", "/w/my-notes"),
            ("lab/x", ""),
            ("aca", "/w/a/aca"),
            ("aca-2", "/w/b/aca"),
            ("Tools", "/w/t/tools"),
        ];
        // 1 default directory: wins over a group with the same name
        assert_eq!(g(&h, "/w/my-notes"), ("notes".into(), false));
        assert_eq!(g(&h, "/w/b/aca"), ("aca-2".into(), false));
        // 2 the directory name is already a group: top level with that name / the only "…/name"
        assert_eq!(g(&h, "/w/anywhere/fun"), ("fun".into(), false));
        assert_eq!(g(&h, "/w/code/api"), ("work/api".into(), false));
        assert_eq!(g(&h, "/w/code/FUN"), ("fun".into(), false));
        // 3 the parent directory's name is a group → nested
        assert_eq!(g(&h, "/w/work/newthing"), ("work/newthing".into(), true));
        assert_eq!(g(&h, "/w/x/deeper"), ("lab/x/deeper".into(), true));
        // More than one nested group with that name: doesn't count, move on
        assert_eq!(g(&h, "/w/code/dup"), ("dup".into(), true));
        // 4 none of these: the top-level directory name (whitespace becomes -)
        assert_eq!(g(&h, "/w/code/my brand"), ("my-brand".into(), true));
        // 5 a group with that name records another directory: numbered onward, nested ones too
        assert_eq!(g(&h, "/w/c/aca"), ("aca-3".into(), true));
        assert_eq!(g(&h, "/w/u/tools"), ("Tools-2".into(), true));
        assert_eq!(g(&[("work", ""), ("work/api", "/w/work/api")], "/w/code/api"), ("work/api-2".into(), true));
    }

    #[test]
    fn renaming_a_group_takes_subgroups_and_sessions_along() {
        let st = Store::memory();
        st.insert(&s("a1", "a", "fun/app")).unwrap();
        st.insert(&s("b2", "b", "fun/app/old")).unwrap();
        st.insert(&s("c3", "c", "fun/apple")).unwrap();
        st.insert(&s("d4", "d", "f_n")).unwrap();
        st.group_create("fun/app", "/w/app").unwrap();
        let order = |g: &str| st.groups().unwrap().into_iter().find(|x| x.path == g).map(|x| x.order);
        let was = order("fun/app");
        assert_eq!(st.group_rename("fun/app", "tools/deck").unwrap(), 2);
        let groups: Vec<String> = st.groups().unwrap().into_iter().map(|g| g.path).collect();
        for g in ["fun", "fun/apple", "tools", "tools/deck", "tools/deck/old"] {
            assert!(groups.contains(&g.to_string()), "缺 {g}:{groups:?}");
        }
        assert!(!groups.iter().any(|g| g.starts_with("fun/app/") || g == "fun/app"), "{groups:?}");
        assert_eq!(order("tools/deck"), was);
        assert_eq!(default_dir(&st, "tools/deck"), "/w/app");
        let group = |id: &str| st.get(id).unwrap().group;
        assert_eq!((group("a1"), group("b2"), group("c3")), ("tools/deck".into(), "tools/deck/old".into(), "fun/apple".into()));
        // _ isn't a wildcard
        st.insert(&s("e5", "e", "fan")).unwrap();
        assert_eq!(st.group_rename("f_n", "fx").unwrap(), 1);
        assert_eq!(group("e5"), "fan");
        // Changing only the case is fine; a clash with an existing one, moving under itself, a missing group: not changed
        assert_eq!(st.group_rename("fx", "FX").unwrap(), 1);
        assert!(st.group_rename("fun", "Tools").unwrap_err().contains("分组 tools 已存在"));
        assert!(st.group_rename("tools", "tools/x").unwrap_err().contains("自己下面"));
        assert!(st.group_rename("nope", "x").is_err());
        assert!(st.group_rename("fun", "").is_err());
    }

    #[test]
    fn a_dir_is_the_default_of_one_group_only() {
        let st = Store::memory();
        st.group_create("a", "/w/x").unwrap();
        st.group_create("b", "").unwrap();
        assert!(st.group_set_dir("b", "/w/x/").unwrap_err().contains("分组 a"));
        st.group_set_dir("b", "/w/y").unwrap();
        st.group_set_dir("a", "/w/x").unwrap();
        st.group_set_dir("a", "").unwrap();
        st.group_set_dir("b", "/w/x").unwrap();
        assert_eq!((default_dir(&st, "a"), default_dir(&st, "b")), (String::new(), "/w/x".into()));
        assert!(st.group_set_dir("nope", "/w").is_err());
    }
}
