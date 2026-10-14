//! 登记表:有哪些会话、各在哪个分组、用什么工具和账号、接着哪段对话。一个 SQLite 文件。
//!
//! 这里只放**人和命令行改的东西**(建、改名、挪组、排序、归档)。agent 每一轮都在变的状态
//! (在跑 / 等你 / 跑完、当前对话 id、自动起的标题)不进库,由 hook 写各会话自己的文件(runtime.rs):
//! hook 每个事件都要跑,十几个会话同时写一个库会互相等锁。

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::paths::{self, Res};
use crate::{t, tr};

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    /// 人改过名:不再跟着 agent 自己起的标题变
    pub title_locked: bool,
    pub group: String,
    pub order: i64,
    /// claude / codex / shell,或 config.toml 里 `[tools.<名>]` 定义的
    pub tool: String,
    pub account: String,
    pub dir: String,
    /// 每次启动都带上的额外参数
    pub args: Vec<String>,
    /// 登记时的对话 id;之后变了(/clear、压缩)以 runtime 里 hook 报的为准
    pub conv: String,
    /// 谁派的它(在某个会话里 `aca new` 的、`--parent` 指定的):通知、代答、列表里挂在谁下面都按这个
    pub parent: String,
    /// 分叉出来的:分自哪个会话。不是 `parent` —— 分叉是你自己接着聊的另一条路,和原会话平级
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

/// 登记表的版本(`pragma user_version`)。2:多了 `fork_of`
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
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| tr!("建不了 {}:{e}", "can't create {}: {e}", path.display()))?;
        Store::at(Connection::open(&path).map_err(e)?)
    }

    pub fn memory() -> Store {
        Store::at(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn at(conn: Connection) -> Res<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(3)).map_err(e)?;
        // 已经是这一版的库就不再跑建表:平常的一次打开只剩一条 pragma
        let v: i64 = conn.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
        if v < 1 {
            conn.execute_batch(&format!("pragma journal_mode = wal; {SCHEMA} pragma user_version = {VERSION};")).map_err(e)?;
            return Ok(Store { conn });
        }
        let store = Store { conn };
        if v < VERSION {
            // 以前分叉出来的记在 parent 上,和派出去的 worker 分不开;认得出来的是 runtime 里记着从哪段对话分的那些
            // (`Run::fork_from`,分叉时写的,一直留着)
            let moved = add_fork_of(&store.conn, |id| !crate::runtime::load(id).fork_from.is_empty())?;
            for (id, from) in moved {
                let _ = store.place_after(&id, &from);
            }
        }
        Ok(store)
    }

    /// 没归档的,按分组里的先后
    pub fn sessions(&self, archived: bool) -> Res<Vec<Session>> {
        let sql =
            format!("select {COLS} from sessions {} order by sort_order, created_at", if archived { "" } else { "where archived_at = 0" });
        let mut st = self.conn.prepare_cached(&sql).map_err(e)?;
        let v = st.query_map([], row).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
        Ok(v)
    }

    /// 按 id 找:完整的、开头几位(只对上一个)、或者标题一模一样的
    pub fn get(&self, key: &str) -> Res<Session> {
        let one = |sql: &str, k: &str| -> Res<Vec<Session>> {
            let mut st = self.conn.prepare_cached(sql).map_err(e)?;
            let v = st.query_map([k], row).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
            Ok(v)
        };
        if key.is_empty() {
            return Err(t!("要给会话 id", "missing a session id").into());
        }
        let mut hit = one(&format!("select {COLS} from sessions where id = ?1"), key)?;
        if hit.is_empty() {
            hit = one(&format!("select {COLS} from sessions where substr(id, 1, length(?1)) = ?1"), key)?;
        }
        if hit.is_empty() {
            hit = one(&format!("select {COLS} from sessions where title = ?1 and archived_at = 0"), key)?;
        }
        match hit.len() {
            0 => Err(tr!("没有 {key} 这个会话", "no session {key}")),
            1 => Ok(hit.remove(0)),
            n => Err(tr!("{key} 对上了 {n} 个会话,写长一点", "{key} matches {n} sessions; type more of it")),
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
        if n == 1 { Ok(()) } else { Err(tr!("没有 {id} 这个会话", "no session {id}")) }
    }

    pub fn set_title(&self, id: &str, title: &str, locked: bool) -> Res<()> {
        self.set(id, "title", &title)?;
        self.set(id, "title_locked", &(locked as i64))
    }

    pub fn set_conv(&self, id: &str, conv: &str) -> Res<()> {
        self.set(id, "conv", &conv)
    }

    /// 换账号(连工具一起:同一家的另一个工具也行)
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

    /// 挪到别的分组(没有就建),排到那一组的最后
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

    /// 排到 `by` 紧后面:这一组整个重新编号(归档的也算在里面,保持相对位置)。不在同一组的不动
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

    /// 新会话在它那一组里排最后
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

    /// 已经有就不动(给了默认目录的补上);`a/b` 连上级 `a` 一起建
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
        if n == 1 { Ok(()) } else { Err(tr!("没有 {path} 这个分组", "no group {path}")) }
    }

    /// 删分组:里面的会话(连下级分组的)挪到上一级
    pub fn group_remove(&self, path: &str) -> Res<usize> {
        let exists: Option<i64> =
            self.conn.query_row("select 1 from groups where path = ?1", [path], |r| r.get(0)).optional().map_err(e)?;
        if exists.is_none() {
            return Err(tr!("没有 {path} 这个分组", "no group {path}"));
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

    /// 按目录定分组(`-g .`),只看不建。规则见 [`group_by_dir`]
    pub fn dir_group(&self, dir: &str) -> Res<(String, bool)> {
        let groups: Vec<(String, String)> = self.groups()?.into_iter().map(|g| (g.path, paths::real(&g.default_path))).collect();
        group_by_dir(&groups, &paths::real(dir))
    }

    /// 按目录定分组(见 [`Store::dir_group`]),没有就建;组还没记默认目录的,记成这个目录
    pub fn group_claim(&self, dir: &str) -> Res<(String, bool)> {
        let (g, new) = self.dir_group(dir)?;
        self.group_create(&g, "")?;
        self.conn
            .execute("update groups set default_path = ?2 where path = ?1 and default_path = ''", [&g, &paths::real(dir)])
            .map_err(e)?;
        Ok((g, new))
    }

    /// 改分组的路径(改名,或者挪到别的上级下面):子分组、里面的会话(连归档的)一起跟过去,
    /// 默认目录、先后不变;新路径的上级没有就建。返回挪了几个会话
    pub fn group_rename(&self, from: &str, to: &str) -> Res<usize> {
        let (from, to) = (from.trim_matches('/'), to.trim_matches('/'));
        let groups = self.groups()?;
        if !groups.iter().any(|g| g.path == from) {
            return Err(tr!("没有 {from} 这个分组", "no group {from}"));
        }
        if to.is_empty() {
            return Err(t!("新的分组名是空的", "the new group name is empty").into());
        }
        if to == from {
            return Ok(0);
        }
        if to.strip_prefix(from).is_some_and(|r| r.starts_with('/')) {
            return Err(tr!("{from} 不能挪到它自己下面", "{from} can't move under itself"));
        }
        // 大小写不同也算有了;只改大小写的(Work → work)不算撞
        if let Some(g) = groups.iter().find(|g| g.path.eq_ignore_ascii_case(to) && g.path != from) {
            return Err(tr!("已经有 {} 这个分组了", "group {} already exists", g.path));
        }
        // 它自己和「它/…」;不用 like:分组名里的 _ % 会被当成通配符
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
        // 挪到了还没有的上级下面:上级补上
        self.group_create(to, "")?;
        Ok(moved)
    }

    /// 改分组的默认目录(空 = 不记了)。一个目录只当一个组的默认目录:`-g .` 按它找组
    pub fn group_set_dir(&self, path: &str, dir: &str) -> Res<()> {
        if !dir.is_empty() {
            let real = paths::real(dir);
            if let Some(g) =
                self.groups()?.iter().find(|g| g.path != path && !g.default_path.is_empty() && paths::real(&g.default_path) == real)
            {
                return Err(tr!("{dir} 已经是分组 {} 的默认目录了", "{dir} is already the default directory of group {}", g.path));
            }
        }
        let n = self.conn.execute("update groups set default_path = ?2 where path = ?1", [path, dir]).map_err(e)?;
        if n == 1 { Ok(()) } else { Err(tr!("没有 {path} 这个分组", "no group {path}")) }
    }
}

/// 第 1 版的库升到第 2 版:加上 `fork_of`,`is_fork` 认出来的那些会话把 `parent` 挪过去(它们不是谁派的)。
/// 返回挪了的:(会话, 分自谁)。两个进程一起升的,后到的等前一个写完,看到已经升过就什么都不做
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

/// 按目录定分组:`aca new -g .`、`aca move <id> .`、`aca group add`、接管、界面的 `N` 都用这一份,只看不建。
/// `groups` 是 (分组路径, 默认目录),`dir` 和默认目录都先过 [`paths::real`]。返回 (分组, 现在还没有这个组):
/// 1. 哪个组的默认目录就是它 → 那个组
/// 2. 目录名已经是分组:顶层同名的,或者唯一一个以它结尾的嵌套组(…/目录名)→ 那个组
/// 3. 上一级目录名是分组(顶层同名,或者唯一一个以它结尾的嵌套组)→ 嵌套成「那个组/目录名」
/// 4. 都不是 → 顶层的「目录名」
/// 5. 2–4 定下的组记着别的目录:往后编号,「…-2」「…-3」(同一个上级下面);没有就新建,有、没记目录就用它
///
/// 大小写不同也算同一个组(和挪分组一样);目录名里的空白换成 -
pub fn group_by_dir(groups: &[(String, String)], dir: &str) -> Res<(String, bool)> {
    if let Some((g, _)) = groups.iter().find(|(_, d)| !d.is_empty() && d == dir) {
        return Ok((g.clone(), false));
    }
    let name =
        |p: Option<&std::ffi::OsStr>| p.map(|f| f.to_string_lossy().split_whitespace().collect::<Vec<_>>().join("-")).unwrap_or_default();
    let path = std::path::Path::new(dir);
    let leaf = name(path.file_name());
    if leaf.is_empty() {
        return Err(tr!("{dir} 没有目录名,定不了分组名", "{dir} has no directory name to name a group after"));
    }
    let parent = name(path.parent().and_then(|p| p.file_name()));
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    // 顶层同名的,或者唯一一个「…/名字」的嵌套组
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
        // 建会话时分组连上级一起有了
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
        // a、b 的 sort_order 一样(接管来的会这样):先后看创建时间
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
        // 不在同一组、挪到自己后面:不动
        st.place_after("d4", "x9").unwrap();
        st.place_after("d4", "d4").unwrap();
        assert_eq!(order(&st), ["d4", "b2", "c3", "a1"]);
        assert_eq!(st.get("x9").unwrap().order, 1);
        assert!(st.place_after("d4", "nope").is_err());
    }

    /// 第 1 版的库(没有 fork_of):分叉出来的记在 parent 上。升上来:认得出是分叉的挪到 fork_of,派出去的 worker 不动
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
        // 升过了:再来一次什么都不做
        assert!(add_fork_of(&conn, |_| true).unwrap().is_empty());
        let st = Store::at(conn).unwrap();
        let of = |id: &str| st.get(id).map(|x| (x.parent, x.fork_of)).unwrap();
        assert_eq!(of("fork"), (String::new(), "lead".into()));
        assert_eq!(of("worker"), ("lead".into(), String::new()));
        assert_eq!(of("lead"), (String::new(), String::new()));
        // 新写进去的带着这一列
        st.insert(&Session { fork_of: "other".into(), ..s("f2", "f2", "g") }).unwrap();
        assert_eq!(st.get("f2").unwrap().fork_of, "other");
    }

    /// 没有分叉、没有子会话的旧库:打开就升好了(不用去读谁的 runtime)
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

    /// 测试用的目录:真建出来(按目录定组会解析成真实路径)
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
        // 没有 → 新建,目录记成默认目录;再来一次还是它
        assert_eq!(st.group_claim(&d[0]).unwrap(), ("aca".into(), true));
        assert_eq!(default_dir(&st, "aca"), d[0]);
        assert_eq!(st.group_claim(&format!("{}/", d[0])).unwrap(), ("aca".into(), false));
        // 同名的记着别的目录 → 往后编号
        assert_eq!(st.dir_group(&d[1]).unwrap(), ("aca-2".into(), true));
        assert_eq!(st.group_claim(&d[1]).unwrap(), ("aca-2".into(), true));
        assert_eq!(st.group_claim(&d[2]).unwrap(), ("aca-3".into(), true));
        assert_eq!(st.group_claim(&d[1]).unwrap(), ("aca-2".into(), false));
        // 同名的没记目录(大小写不同也算):用它,记上
        st.group_create("My-proj", "").unwrap();
        assert_eq!(st.group_claim(&d[3]).unwrap(), ("My-proj".into(), false));
        assert_eq!(default_dir(&st, "My-proj"), d[3]);
        // 只看不建
        let n = st.groups().unwrap().len();
        let (_, more) = dirs(&["z/fresh"]);
        assert_eq!(st.dir_group(&more[0]).unwrap(), ("fresh".into(), true));
        assert_eq!(st.groups().unwrap().len(), n);
        assert!(st.dir_group("/").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// 界面的 N 和 -g . 是同一套:目录名、上一级目录名是已有的分组(顶层的或唯一的嵌套组)就进它 / 嵌在它下面,
    /// 定下的组记着别的目录就往后编号
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
        // 1 默认目录:比同名的组优先
        assert_eq!(g(&h, "/w/my-notes"), ("notes".into(), false));
        assert_eq!(g(&h, "/w/b/aca"), ("aca-2".into(), false));
        // 2 目录名已是分组:顶层同名 / 唯一的「…/名字」
        assert_eq!(g(&h, "/w/anywhere/fun"), ("fun".into(), false));
        assert_eq!(g(&h, "/w/code/api"), ("work/api".into(), false));
        assert_eq!(g(&h, "/w/code/FUN"), ("fun".into(), false));
        // 3 上一级目录名是分组 → 嵌套
        assert_eq!(g(&h, "/w/work/newthing"), ("work/newthing".into(), true));
        assert_eq!(g(&h, "/w/x/deeper"), ("lab/x/deeper".into(), true));
        // 同名的嵌套组不止一个:不算,往下走
        assert_eq!(g(&h, "/w/code/dup"), ("dup".into(), true));
        // 4 都不是:顶层目录名(空白换成 -)
        assert_eq!(g(&h, "/w/code/my brand"), ("my-brand".into(), true));
        // 5 同名的组记着别的目录:往后编号,嵌套的也一样
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
        // _ 不是通配符
        st.insert(&s("e5", "e", "fan")).unwrap();
        assert_eq!(st.group_rename("f_n", "fx").unwrap(), 1);
        assert_eq!(group("e5"), "fan");
        // 只改大小写可以;撞上已有的、挪到自己下面、没有这个组:不改
        assert_eq!(st.group_rename("fx", "FX").unwrap(), 1);
        assert!(st.group_rename("fun", "Tools").unwrap_err().contains("已经有 tools"));
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
