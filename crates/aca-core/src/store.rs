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
    /// The session named to route this group's work, as recorded (empty = nobody). Whether it still counts is
    /// [`Store::routers`]'s to say
    #[serde(skip_serializing_if = "String::is_empty")]
    pub router: String,
}

/// One note in a group's notebook. A group has any number of them; they follow the group when it's renamed
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Note {
    pub id: String,
    pub group: String,
    /// The first line is its name
    pub text: String,
    /// Who wrote it: a session's id; empty = you
    pub by: String,
    pub created: u64,
    pub updated: u64,
    pub pinned: bool,
    /// Deleted at (kept a while so it can be brought back); 0 = not deleted
    #[serde(skip_serializing_if = "is_zero")]
    pub gone: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Note {
    /// What it's called: its first line with anything on it
    pub fn name(&self) -> &str {
        self.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
    }
}

/// One task ticket: a thing to do in a group, handed to one session at a time. Its state isn't recorded, it follows from
/// the rest (see [`Task::state`]). Tickets follow their group when it's renamed
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Task {
    /// Its number, written #n: one sequence for the whole registry, never used twice
    pub n: i64,
    pub group: String,
    pub title: String,
    /// What counts as done
    pub criteria: String,
    /// One line for whoever handles it to write: what it waits for, what matters most now. Only kept and shown
    pub note: String,
    /// The session doing it; empty = nobody
    pub session: String,
    /// Who made it: a session's id; empty = you
    pub by: String,
    /// Who handed it to `session` (a session's id; empty = you), when, and what was said handing it over
    pub given_by: String,
    pub given: u64,
    pub say: String,
    /// What was said marking it done
    pub result: String,
    pub created: u64,
    /// When it was marked done; 0 = it isn't
    pub done: u64,
    pub watch: bool,
}

/// Where a ticket stands
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TaskState {
    /// Nobody has it
    Todo,
    /// A session has it
    Doing,
    Done,
}

impl Task {
    pub fn watch_label(&self) -> &'static str {
        if self.watch && self.done == 0 { crate::t!("  盯着", "  watching") } else { "" }
    }

    /// Done once marked so; otherwise being done when a session has it, waiting when none does
    pub fn state(&self) -> TaskState {
        match (self.done > 0, self.session.is_empty()) {
            (true, _) => TaskState::Done,
            (false, false) => TaskState::Doing,
            (false, true) => TaskState::Todo,
        }
    }
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
  default_path text not null default '',
  router text not null default ''
);
";

/// The notes table (version 3). `gone_with`: the key of the trash entry it was deleted along with (a deleted group's); empty
/// = deleted on its own
const NOTES: &str = "
create table if not exists notes(
  id text primary key,
  group_path text not null,
  body text not null,
  author text not null default '',
  created_at integer not null,
  updated_at integer not null,
  pinned integer not null default 0,
  gone_at integer not null default 0,
  gone_with text not null default ''
);
";

/// The tasks table (version 5). `autoincrement`: a deleted ticket's number isn't given to another
const TASKS: &str = "
create table if not exists tasks(
  n integer primary key autoincrement,
  group_path text not null,
  title text not null,
  criteria text not null default '',
  note text not null default '',
  session text not null default '',
  author text not null default '',
  given_by text not null default '',
  given_at integer not null default 0,
  say text not null default '',
  result text not null default '',
  created_at integer not null,
  done_at integer not null default 0
);
";

const COLS: &str =
    "id, title, title_locked, group_path, sort_order, tool, account, dir, args, conv, parent, created_at, archived_at, fork_of";

const NOTE_COLS: &str = "id, group_path, body, author, created_at, updated_at, pinned, gone_at";

const TASK_COLS: &str =
    "n, group_path, title, criteria, note, session, author, given_by, given_at, say, result, created_at, done_at, watch";

/// Registry version (`pragma user_version`). 2: adds `fork_of`. 3: adds the notes table. 4: adds a group's `router`.
/// 5: adds the tasks table. 6: opt-in notifications for individual tickets.
const VERSION: i64 = 6;
const ADD_WATCH: &str = "alter table tasks add column watch integer not null default 0";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouterRole {
    Project,
    Desk,
}

impl Store {
    pub fn claim_router(&self, s: &Session) -> Res<()> {
        let n = self
            .conn
            .execute(
                "update groups set router = ?2 where path = ?1 and not exists
            (select 1 from sessions where id = groups.router and group_path = groups.path and archived_at = 0)",
                [&s.group, &s.id],
            )
            .map_err(e)?;
        if n == 0 {
            return Err(tr!("项目 {} 已有 router", "project {} already has a router", s.group));
        }
        Ok(())
    }

    pub fn router_role(&self, id: &str) -> Res<Option<RouterRole>> {
        let Some((group, _)) = self.routers()?.into_iter().find(|(_, router)| router == id) else { return Ok(None) };
        let scratch = crate::config::scratch_dir();
        let desk = !scratch.is_empty()
            && self.groups()?.iter().any(|g| {
                g.path == group && !g.default_path.is_empty() && crate::paths::real(&g.default_path) == crate::paths::real(&scratch)
            });
        Ok(Some(if desk { RouterRole::Desk } else { RouterRole::Project }))
    }

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
        let version = |conn: &Connection| conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0));
        let mut v = version(&conn).map_err(e)?;
        // A new file. Commands started together the very first time all find it new, and turning on WAL takes the file for
        // itself without waiting for it: the ones that can't have it look again in a moment, when the first has made the tables
        let mut tries = 0;
        while v < 1 {
            let init = || -> rusqlite::Result<()> {
                conn.execute_batch("pragma journal_mode = wal")?;
                let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
                if version(&tx)? == 0 {
                    tx.execute_batch(&format!("{SCHEMA} {NOTES} {TASKS} {ADD_WATCH}; pragma user_version = {VERSION}"))?;
                }
                tx.commit()
            };
            match init() {
                Ok(()) => v = version(&conn).map_err(e)?,
                Err(rusqlite::Error::SqliteFailure(f, _)) if tries < 60 && f.code == rusqlite::ErrorCode::DatabaseBusy => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    v = version(&conn).map_err(e)?;
                }
                Err(err) => return Err(e(err)),
            }
        }
        let store = Store { conn };
        if v < 2 {
            // Forks used to be recorded in parent, indistinguishable from delegated workers; the recognizable ones are those whose runtime
            // records which conversation they forked from (`Run::fork_from`, written at fork time and kept)
            let moved = add_fork_of(&store.conn, |id| !crate::runtime::load(id).fork_from.is_empty())?;
            for (id, from) in moved {
                let _ = store.place_after(&id, &from);
            }
        }
        if v < 3 {
            add_notes(&store.conn)?;
        }
        if v < 4 {
            add_router(&store.conn)?;
        }
        if v < 5 {
            add_tasks(&store.conn)?;
        }
        if v < 6 {
            let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
            if version(&tx).map_err(e)? < 6 {
                tx.execute_batch(&format!("{ADD_WATCH}; pragma user_version = 6")).map_err(e)?;
            }
            tx.commit().map_err(e)?;
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
        let mut st =
            self.conn.prepare_cached("select path, sort_order, default_path, router from groups order by sort_order, path").map_err(e)?;
        let v = st
            .query_map([], |r| Ok(Group { path: r.get(0)?, order: r.get(1)?, default_path: r.get(2)?, router: r.get(3)? }))
            .map_err(e)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(e)?;
        Ok(v)
    }

    /// Name the session that routes this group's work; an empty `id` = nobody any more. It has to be one of the group's own
    /// sessions (a group not registered yet gets registered). Sessions in no group have no router
    pub fn group_set_router(&self, path: &str, id: &str) -> Res<()> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(t!("需要一个分组", "needs a group").into());
        }
        if id.is_empty() {
            let n = self.conn.execute("update groups set router = '' where path = ?1", [path]).map_err(e)?;
            return if n == 1 { Ok(()) } else { Err(tr!("没有分组 {path}", "no group {path}")) };
        }
        let s = self.get(id)?;
        if s.group != path || s.archived > 0 {
            return Err(tr!("{} 不在分组 {path} 中", "{} isn't in group {path}", s.id));
        }
        self.group_create(path, "")?;
        self.conn.execute("update groups set router = ?2 where path = ?1", [path, s.id.as_str()]).map(|_| ()).map_err(e)
    }

    /// The routers that count, as (group, session): the session a group names is still registered, not deleted, and still in
    /// that group. One deleted or moved away leaves its group without a router, and is it again once put back
    pub fn routers(&self) -> Res<Vec<(String, String)>> {
        let mut st = self
            .conn
            .prepare_cached(
                "select g.path, s.id from groups g join sessions s on s.id = g.router
                 where g.router != '' and s.group_path = g.path and s.archived_at = 0 order by g.sort_order, g.path",
            )
            .map_err(e)?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
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
        // Its notes don't move up with the sessions (they were about this group): deleted, each on its own
        self.notes_bin_group(path, "")?;
        Ok(moved)
    }

    /// Only drop the group's registration (with subgroups); its sessions and notes are untouched (when the interface removes a
    /// group, its sessions are already archived along with it and its notes deleted with [`Store::notes_bin_group`], see aca-tui's
    /// trash.rs). Returns how many groups were dropped
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
        // Deleted ones too: brought back, they belong to the group under its new name
        tx.execute(
            "update notes set group_path = ?2 || substr(group_path, length(?1) + 1)
             where group_path = ?1 or substr(group_path, 1, length(?1) + 1) = ?1 || '/'",
            [from, to],
        )
        .map_err(e)?;
        tx.execute(
            "update tasks set group_path = ?2 || substr(group_path, length(?1) + 1)
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
        self.dir_free(path, dir)?;
        let n = self.conn.execute("update groups set default_path = ?2 where path = ?1", [path, dir]).map_err(e)?;
        if n == 1 { Ok(()) } else { Err(tr!("没有分组 {path}", "no group {path}")) }
    }

    /// Create a group that works in `dir`. Nothing is created when that directory is already another group's default; a group
    /// that's already there keeps the default directory it has
    pub fn group_create_in(&self, path: &str, dir: &str) -> Res<()> {
        self.dir_free(path, dir)?;
        self.group_create(path, "")?;
        self.conn.execute("update groups set default_path = ?2 where path = ?1 and default_path = ''", [path, dir]).map_err(e)?;
        Ok(())
    }

    /// `dir` may become the default directory of group `path`: no other group has it (empty = none, always free)
    fn dir_free(&self, path: &str, dir: &str) -> Res<()> {
        if dir.is_empty() {
            return Ok(());
        }
        let real = paths::real(dir);
        match self.groups()?.iter().find(|g| g.path != path && !g.default_path.is_empty() && paths::real(&g.default_path) == real) {
            Some(g) => Err(tr!("{dir} 已是分组 {} 的默认目录", "{dir} is already the default directory of group {}", g.path)),
            None => Ok(()),
        }
    }

    fn note_rows(&self, rest: &str, args: impl rusqlite::Params) -> Res<Vec<Note>> {
        let mut st = self.conn.prepare_cached(&format!("select {NOTE_COLS} from notes {rest}")).map_err(e)?;
        let v = st.query_map(args, note_row).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
        Ok(v)
    }

    /// Every group's notes, as a notebook lists them: pinned first, then the most recently changed (changed within the same
    /// second: the one written later). Deleted ones aren't in it
    pub fn notes(&self) -> Res<Vec<Note>> {
        self.note_rows("where gone_at = 0 order by pinned desc, updated_at desc, rowid desc", [])
    }

    /// One group's notes (not its subgroups'), in the same order
    pub fn notes_in(&self, group: &str) -> Res<Vec<Note>> {
        self.note_rows("where gone_at = 0 and group_path = ?1 order by pinned desc, updated_at desc, rowid desc", [group])
    }

    /// Notes deleted on their own (not along with their group), most recently deleted first
    pub fn notes_gone(&self) -> Res<Vec<Note>> {
        self.note_rows("where gone_at > 0 and gone_with = '' order by gone_at desc, rowid desc", [])
    }

    /// Find a note by its id or the first few characters of it (matching only one). Deleted ones aren't found
    pub fn note(&self, key: &str) -> Res<Note> {
        if key.is_empty() {
            return Err(t!("需要笔记 id", "missing a note id").into());
        }
        let mut hit = self.note_rows("where gone_at = 0 and id = ?1", [key])?;
        if hit.is_empty() {
            hit = self.note_rows("where gone_at = 0 and substr(id, 1, length(?1)) = ?1", [key])?;
        }
        match hit.len() {
            0 => Err(tr!("没有笔记 {key}", "no note {key}")),
            1 => Ok(hit.remove(0)),
            n => Err(tr!("{key} 匹配到 {n} 条笔记,请写得更完整", "{key} matches {n} notes; type more of it")),
        }
    }

    /// Write a new note into a group's notebook. `by`: the session that wrote it, empty = you
    pub fn note_add(&self, group: &str, text: &str, by: &str) -> Res<Note> {
        let now = paths::now();
        let mut n = Note {
            id: String::new(),
            group: group.into(),
            text: text.into(),
            by: by.into(),
            created: now,
            updated: now,
            ..Note::default()
        };
        // An id that's taken (8 hex digits): draw again
        for _ in 0..8 {
            n.id = paths::short_id();
            if self.note_put(&n)? {
                return Ok(n);
            }
        }
        Err(t!("登记表:笔记 id 冲突", "registry: note ids keep clashing").into())
    }

    /// Record a note exactly as given (its id, times and all). False = a note with this id is already there, nothing changed
    pub fn note_put(&self, n: &Note) -> Res<bool> {
        let done = self
            .conn
            .execute(
                &format!("insert into notes({NOTE_COLS}) values(?1,?2,?3,?4,?5,?6,?7,?8) on conflict do nothing"),
                params![n.id, n.group, n.text, n.by, n.created as i64, n.updated as i64, n.pinned as i64, n.gone as i64],
            )
            .map_err(e)?;
        Ok(done == 1)
    }

    fn note_change(&self, id: &str, sql: &str, args: impl rusqlite::Params) -> Res<()> {
        let n = self.conn.execute(sql, args).map_err(e)?;
        if n == 1 { Ok(()) } else { Err(tr!("没有笔记 {id}", "no note {id}")) }
    }

    /// Rewrite a note. A deleted one is brought back by it
    pub fn note_set(&self, id: &str, text: &str) -> Res<()> {
        let sql = "update notes set body = ?2, updated_at = ?3, gone_at = 0, gone_with = '' where id = ?1";
        self.note_change(id, sql, params![id, text, paths::now() as i64])
    }

    pub fn note_pin(&self, id: &str, on: bool) -> Res<()> {
        self.note_change(id, "update notes set pinned = ?2 where id = ?1", params![id, on as i64])
    }

    /// Delete a note: it's kept, out of every list, until [`Store::notes_purge`] (or [`Store::note_unbin`] brings it back)
    pub fn note_bin(&self, id: &str) -> Res<()> {
        self.note_change(id, "update notes set gone_at = ?2, gone_with = '' where id = ?1", params![id, paths::now() as i64])
    }

    pub fn note_unbin(&self, id: &str) -> Res<()> {
        self.note_change(id, "update notes set gone_at = 0, gone_with = '' where id = ?1", params![id])
    }

    /// Delete a note for good, right now
    pub fn note_remove(&self, id: &str) -> Res<()> {
        self.conn.execute("delete from notes where id = ?1", [id]).map(|_| ()).map_err(e)
    }

    /// A group is being deleted: its notes and its subgroups' are deleted with it. `with`: the key of the trash entry they go
    /// along with (they come back with it, see [`Store::notes_unbin_with`]); empty = each deleted on its own. Returns how many
    pub fn notes_bin_group(&self, group: &str, with: &str) -> Res<usize> {
        // No like: _ and % in a group name would be taken as wildcards
        self.conn
            .execute(
                "update notes set gone_at = ?3, gone_with = ?4
                 where gone_at = 0 and (group_path = ?1 or substr(group_path, 1, length(?2)) = ?2)",
                params![group, format!("{group}/"), paths::now() as i64, with],
            )
            .map_err(e)
    }

    /// A trash entry is brought back: the notes deleted along with it are back too
    pub fn notes_unbin_with(&self, with: &str) -> Res<usize> {
        if with.is_empty() {
            return Ok(0);
        }
        self.conn.execute("update notes set gone_at = 0, gone_with = '' where gone_at > 0 and gone_with = ?1", [with]).map_err(e)
    }

    /// A trash entry is deleted for good: so are the notes deleted along with it
    pub fn notes_drop_with(&self, with: &str) -> Res<usize> {
        if with.is_empty() {
            return Ok(0);
        }
        self.conn.execute("delete from notes where gone_at > 0 and gone_with = ?1", [with]).map_err(e)
    }

    /// Delete for good the notes deleted at `before` or earlier
    pub fn notes_purge(&self, before: u64) -> Res<usize> {
        self.conn.execute("delete from notes where gone_at > 0 and gone_at <= ?1", [before as i64]).map_err(e)
    }

    /// Every ticket, by number
    pub fn tasks(&self) -> Res<Vec<Task>> {
        let mut st = self.conn.prepare_cached(&format!("select {TASK_COLS} from tasks order by n")).map_err(e)?;
        let v = st.query_map([], task_row).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
        Ok(v)
    }

    pub(crate) fn watched_by_worker(&self, worker: &str) -> Res<Vec<Task>> {
        let sql = format!("select {TASK_COLS} from tasks where session = ?1 and done_at = 0 and watch != 0 order by n");
        let mut st = self.conn.prepare_cached(&sql).map_err(e)?;
        st.query_map([worker], task_row).map_err(e)?.collect::<rusqlite::Result<_>>().map_err(e)
    }

    /// Threads that follow this worker's open tickets, or one specific ticket at completion.
    pub fn task_routers(&self, worker: &str, ticket: Option<i64>) -> Res<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "select distinct g.router from tasks t join groups g on g.path = t.group_path or g.router = t.given_by
             join sessions s on s.id = g.router and s.group_path = g.path and s.archived_at = 0
             where t.n = ?2 or (?2 is null and t.session = ?1 and t.done_at = 0) order by g.router"
        ).map_err(e)?;
        st.query_map(params![worker, ticket], |r| r.get(0)).map_err(e)?.collect::<rusqlite::Result<_>>().map_err(e)
    }

    pub fn task(&self, n: i64) -> Res<Task> {
        let sql = format!("select {TASK_COLS} from tasks where n = ?1");
        self.conn.query_row(&sql, [n], task_row).optional().map_err(e)?.ok_or_else(|| tr!("没有任务 #{n}", "no task #{n}"))
    }

    /// Record a new ticket as given, but for its number (the next one) and when it was made (now). Returns it as recorded
    pub fn task_add(&self, t: &Task) -> Res<Task> {
        let now = paths::now();
        self.conn
            .execute(
                "insert into tasks(group_path, title, criteria, note, session, author, given_by, given_at, say, created_at, watch)
                 values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    t.group,
                    t.title,
                    t.criteria,
                    t.note,
                    t.session,
                    t.by,
                    t.given_by,
                    if t.session.is_empty() { 0 } else { now as i64 },
                    t.say,
                    now as i64,
                    t.watch
                ],
            )
            .map_err(e)?;
        self.task(self.conn.last_insert_rowid())
    }

    /// Record a ticket exactly as given, its number and times too (a copy of a registry is made this way). False = its
    /// number is taken, nothing changed
    pub fn task_put(&self, t: &Task) -> Res<bool> {
        let done = self
            .conn
            .execute(
                &format!("insert into tasks({TASK_COLS}) values(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) on conflict do nothing"),
                params![
                    t.n,
                    t.group,
                    t.title,
                    t.criteria,
                    t.note,
                    t.session,
                    t.by,
                    t.given_by,
                    t.given as i64,
                    t.say,
                    t.result,
                    t.created as i64,
                    t.done as i64,
                    t.watch
                ],
            )
            .map_err(e)?;
        Ok(done == 1)
    }

    fn task_change(&self, n: i64, sql: &str, args: impl rusqlite::Params) -> Res<()> {
        let done = self.conn.execute(sql, args).map_err(e)?;
        if done == 1 { Ok(()) } else { Err(tr!("没有任务 #{n}", "no task #{n}")) }
    }

    /// A watch change belongs to the assignment or edit that requested it. A rejected edit rolls both back.
    pub(crate) fn with_watch<T>(&self, n: i64, watch: Option<bool>, edit: impl FnOnce() -> Res<T>) -> Res<T> {
        self.edit_tasks(|| {
            self.set_watch(n, watch)?;
            edit()
        })
    }

    fn set_watch(&self, n: i64, watch: Option<bool>) -> Res<()> {
        if let Some(watch) = watch {
            self.task_change(n, "update tasks set watch = ?2 where n = ?1", params![n, watch])?;
        }
        Ok(())
    }

    pub(crate) fn edit_tasks<T>(&self, edit: impl FnOnce() -> Res<T>) -> Res<T> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
        let result = edit()?;
        tx.commit().map_err(e)?;
        Ok(result)
    }

    /// Hand a ticket to a session (empty = to nobody: it waits again). `by`: who hands it over (a session's id, empty =
    /// you), `say`: what they said doing so. One that was done is open again
    pub fn task_give(&self, n: i64, session: &str, by: &str, say: &str) -> Res<()> {
        let sql = "update tasks set session = ?2, given_by = ?3, given_at = ?4, say = ?5, done_at = 0, result = '' where n = ?1";
        self.task_change(n, sql, params![n, session, by, paths::now() as i64, say])
    }

    pub(crate) fn assign_start(&self, n: i64, id: &str, by: &str, say: &str, watch: Option<bool>) -> Res<(Task, Task)> {
        self.edit_tasks(|| {
            let old = self.task(n)?;
            self.task_give(n, id, by, say)?;
            self.set_watch(n, watch)?;
            Ok((old, self.task(n)?))
        })
    }

    /// A worker that never launched releases the ticket unless it has since changed hands or finished.
    pub(crate) fn undo_task_start(&self, old: &Task, worker: &str, watch: Option<bool>) -> Res<bool> {
        self.conn
            .execute(
                "update tasks set session=?3, given_by=?4, given_at=?5, say=?6, result=?7, done_at=?8,
             watch=case when watch=?9 then ?10 else watch end where n=?1 and session=?2 and done_at=0",
                params![old.n, worker, old.session, old.given_by, old.given as i64, old.say, old.result, old.done as i64, watch, old.watch],
            )
            .map(|changed| changed == 1)
            .map_err(e)
    }

    pub fn task_done(&self, n: i64, result: &str) -> Res<()> {
        let sql = "update tasks set done_at = ?2, result = ?3 where n = ?1 and done_at = 0";
        if self.conn.execute(sql, params![n, paths::now() as i64, result]).map_err(e)? == 1 {
            return Ok(());
        }
        self.task(n)?;
        Err(tr!("任务 #{n} 已经完成", "task #{n} is done already"))
    }

    /// Rewrite what's given of a ticket's title, what counts as done, and its line
    pub fn task_set(&self, n: i64, title: Option<&str>, criteria: Option<&str>, note: Option<&str>) -> Res<()> {
        let sql = "update tasks set title = coalesce(?2, title), criteria = coalesce(?3, criteria), note = coalesce(?4, note) where n = ?1";
        self.task_change(n, sql, params![n, title, criteria, note])
    }

    /// Delete a ticket for good
    pub fn task_remove(&self, n: i64) -> Res<()> {
        self.task_change(n, "delete from tasks where n = ?1", [n])
    }
}

fn task_row(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        n: r.get(0)?,
        group: r.get(1)?,
        title: r.get(2)?,
        criteria: r.get(3)?,
        note: r.get(4)?,
        session: r.get(5)?,
        by: r.get(6)?,
        given_by: r.get(7)?,
        given: r.get::<_, i64>(8)? as u64,
        say: r.get(9)?,
        result: r.get(10)?,
        created: r.get::<_, i64>(11)? as u64,
        done: r.get::<_, i64>(12)? as u64,
        watch: r.get(13)?,
    })
}

fn note_row(r: &rusqlite::Row) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        group: r.get(1)?,
        text: r.get(2)?,
        by: r.get(3)?,
        created: r.get::<_, i64>(4)? as u64,
        updated: r.get::<_, i64>(5)? as u64,
        pinned: r.get::<_, i64>(6)? != 0,
        gone: r.get::<_, i64>(7)? as u64,
    })
}

/// Upgrade a version 1 database to version 2: add `fork_of`, and move `parent` there for the sessions `is_fork` recognizes (nobody
/// delegated them). Returns the ones moved: (session, forked from). When two processes upgrade at once, the later waits for the
/// first to finish writing, sees it's already upgraded and does nothing
fn add_fork_of(conn: &Connection, is_fork: impl Fn(&str) -> bool) -> Res<Vec<(String, String)>> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let v: i64 = tx.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
    if v >= 2 {
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
    tx.execute_batch("pragma user_version = 2;").map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(moved)
}

/// Upgrade a version 2 database to version 3: add the notes table. Two processes upgrading at once: as in [`add_fork_of`]
fn add_notes(conn: &Connection) -> Res<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let v: i64 = tx.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
    if v >= 3 {
        return Ok(());
    }
    tx.execute_batch(&format!("{NOTES} pragma user_version = 3;")).map_err(e)?;
    tx.commit().map_err(e)
}

/// Upgrade a version 3 database to version 4: a group can name its router. Two processes upgrading at once: as in
/// [`add_fork_of`]
fn add_router(conn: &Connection) -> Res<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let v: i64 = tx.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
    if v >= 4 {
        return Ok(());
    }
    tx.execute_batch("alter table groups add column router text not null default ''; pragma user_version = 4;").map_err(e)?;
    tx.commit().map_err(e)
}

/// Upgrade a version 4 database to version 5: add the tasks table. Two processes upgrading at once: as in [`add_fork_of`]
fn add_tasks(conn: &Connection) -> Res<()> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let v: i64 = tx.query_row("pragma user_version", [], |r| r.get(0)).map_err(e)?;
    if v >= 5 {
        return Ok(());
    }
    tx.execute_batch(&format!("{TASKS} pragma user_version = 5;")).map_err(e)?;
    tx.commit().map_err(e)
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
        let v1 = v3_schema().replace(",\n  fork_of text not null default ''", "");
        assert_ne!(v1, v3_schema());
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
        conn.execute_batch(&format!("{} pragma user_version = 1;", v3_schema().replace(",\n  fork_of text not null default ''", "")))
            .unwrap();
        conn.execute("insert into sessions(id, title, tool, dir, created_at) values('a1', 'a', 'claude', '/p', 1)", []).unwrap();
        let st = Store::at(conn).unwrap();
        assert_eq!(st.get("a1").unwrap().fork_of, "");
        assert_eq!(st.conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        // Every step ran: it has the notes table too, and its groups can name a router
        assert!(st.notes().unwrap().is_empty());
        assert!(st.routers().unwrap().is_empty());
    }

    /// The tables as they were through version 3: a group had no router
    fn v3_schema() -> String {
        let old = SCHEMA.replace(",\n  router text not null default ''", "");
        assert_ne!(old, SCHEMA);
        old
    }

    /// A version 3 database: opening it lets its groups name a router, and what it held is as it was
    #[test]
    fn a_registry_whose_groups_had_no_router_gains_the_column_and_keeps_its_groups() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{} {NOTES} pragma user_version = 3;", v3_schema())).unwrap();
        conn.execute("insert into groups(path, sort_order, default_path) values('work', 2, '/p')", []).unwrap();
        conn.execute("insert into sessions(id, title, group_path, tool, dir, created_at) values('a1', 'a', 'work', 'claude', '/p', 1)", [])
            .unwrap();
        let st = Store::at(conn).unwrap();
        assert_eq!(st.conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        assert_eq!(st.groups().unwrap(), [Group { path: "work".into(), order: 2, default_path: "/p".into(), router: String::new() }]);
        st.group_set_router("work", "a1").unwrap();
        // Already upgraded: running the step again does nothing, the router is still named
        add_router(&st.conn).unwrap();
        assert_eq!(st.routers().unwrap(), [("work".to_string(), "a1".to_string())]);
    }

    #[test]
    fn v5_tickets_gain_watch_once_with_false_as_the_default() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "{SCHEMA} {NOTES} {TASKS} pragma user_version=5;
            insert into tasks(group_path,title,created_at) values('p','keep me',1)"
        ))
        .unwrap();
        let store = Store::at(conn).unwrap();
        assert_eq!(store.conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 6);
        let t = store.task(1).unwrap();
        assert_eq!(t.title, "keep me");
        assert!(!t.watch);
        store.with_watch(1, Some(true), || Ok(())).unwrap();
        let reopened = Store::at(store.conn).unwrap();
        assert!(reopened.task(1).unwrap().watch);
    }

    /// A version 4 database: opening it adds the tasks table, and what it held is as it was
    #[test]
    fn a_registry_from_before_tickets_gains_their_table_and_keeps_what_it_held() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{SCHEMA} {NOTES} pragma user_version = 4;")).unwrap();
        conn.execute("insert into groups(path, sort_order, router) values('work', 2, 'a1')", []).unwrap();
        conn.execute("insert into sessions(id, title, group_path, tool, dir, created_at) values('a1', 'a', 'work', 'claude', '/p', 1)", [])
            .unwrap();
        let st = Store::at(conn).unwrap();
        assert_eq!(st.conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        assert_eq!(st.routers().unwrap(), [("work".to_string(), "a1".to_string())]);
        assert!(st.tasks().unwrap().is_empty());
        let t = st.task_add(&Task { group: "work".into(), title: "修告警".into(), ..Task::default() }).unwrap();
        // Already upgraded: running the step again does nothing, the ticket is still there
        add_tasks(&st.conn).unwrap();
        assert_eq!(st.tasks().unwrap(), [t]);
    }

    /// A ticket waits until a session has it, is being done while one does, and is done once marked so; handing a done one
    /// over opens it again. Its number is the next of one sequence, and a deleted ticket's is not used again
    #[test]
    fn a_ticket_is_numbered_once_and_its_state_follows_from_who_has_it_and_whether_it_is_done() {
        let st = Store::memory();
        let new = |group: &str, title: &str| Task { group: group.into(), title: title.into(), ..Task::default() };
        let a = st.task_add(&Task { criteria: "告警响".into(), note: "等 #2".into(), by: "r1".into(), ..new("work", "修告警") }).unwrap();
        let b = st.task_add(&Task { session: "w1".into(), given_by: "r1".into(), say: "你昨天改过".into(), ..new("work/sub", "加面板") }).unwrap();
        assert_eq!((a.n, b.n), (1, 2));
        assert_eq!((a.state(), a.given, a.created > 0), (TaskState::Todo, 0, true));
        assert_eq!((b.state(), b.given > 0, b.say.as_str()), (TaskState::Doing, true, "你昨天改过"));
        assert_eq!((a.criteria.as_str(), a.note.as_str(), a.by.as_str()), ("告警响", "等 #2", "r1"));
        // Handed over, marked done, handed over again
        st.task_give(1, "w2", "", "接着做").unwrap();
        let t = st.task(1).unwrap();
        assert_eq!((t.state(), t.session.as_str(), t.given_by.as_str(), t.say.as_str()), (TaskState::Doing, "w2", "", "接着做"));
        st.task_done(1, "响了").unwrap();
        let t = st.task(1).unwrap();
        assert_eq!((t.state(), t.result.as_str(), t.session.as_str(), t.done > 0), (TaskState::Done, "响了", "w2", true));
        st.task_give(1, "", "r1", "").unwrap();
        let t = st.task(1).unwrap();
        assert_eq!((t.state(), t.result.as_str(), t.done), (TaskState::Todo, "", 0));
        // Only what's given is rewritten
        st.task_set(1, Some("修告警规则"), None, Some("")).unwrap();
        let t = st.task(1).unwrap();
        assert_eq!((t.title.as_str(), t.criteria.as_str(), t.note.as_str()), ("修告警规则", "告警响", ""));
        // They follow their group's new name, the ones of its subgroups too
        st.group_create("work/sub", "").unwrap();
        st.group_rename("work", "probe").unwrap();
        let groups: Vec<String> = st.tasks().unwrap().into_iter().map(|t| t.group).collect();
        assert_eq!(groups, ["probe", "probe/sub"]);
        // Deleted for good: its number stays taken
        st.task_remove(2).unwrap();
        assert!(st.task(2).unwrap_err().contains("#2") && st.task_remove(2).is_err() && st.task_done(9, "").is_err());
        assert_eq!(st.task_add(&new("probe", "下一张")).unwrap().n, 3);
    }

    /// A group's router is one of its own sessions, and only for as long as it is there: deleted or moved away it no longer
    /// counts, put back it does again; the group's new name keeps it, and naming another replaces it
    #[test]
    fn a_groups_router_is_a_session_of_its_own_for_as_long_as_it_is_there() {
        let st = Store::memory();
        for (id, g) in [("r1", "work"), ("w1", "work"), ("o1", "other")] {
            st.insert(&s(id, id, g)).unwrap();
        }
        let routers = || st.routers().unwrap();
        assert!(routers().is_empty());
        // Not one of the group's own, no such session, no group to name it for
        assert!(st.group_set_router("work", "o1").unwrap_err().contains("work"));
        assert!(st.group_set_router("work", "zz").is_err());
        assert!(st.group_set_router("", "r1").is_err());
        st.group_set_router("/work/", "r1").unwrap();
        assert_eq!(routers(), [("work".to_string(), "r1".to_string())]);
        assert_eq!(st.groups().unwrap().iter().find(|g| g.path == "work").unwrap().router, "r1");
        // Deleted (in the trash): the group has none; brought back, it is the router again
        st.set_archived("r1", 5).unwrap();
        assert!(routers().is_empty());
        st.set_archived("r1", 0).unwrap();
        assert_eq!(routers().len(), 1);
        // Moved to another group: neither group's router; moved back, this group's again
        st.set_group("r1", "other").unwrap();
        assert!(routers().is_empty());
        st.set_group("r1", "work").unwrap();
        assert_eq!(routers().len(), 1);
        // The group renamed: still its router
        st.group_rename("work", "jobs/work").unwrap();
        assert_eq!(routers(), [("jobs/work".to_string(), "r1".to_string())]);
        // Another one named: it replaces the first; nobody named: none
        st.group_set_router("jobs/work", "w1").unwrap();
        assert_eq!(routers(), [("jobs/work".to_string(), "w1".to_string())]);
        st.group_set_router("jobs/work", "").unwrap();
        assert!(routers().is_empty());
        assert!(st.group_set_router("nowhere", "").is_err());
    }

    /// A version 2 database (sessions with fork_of, no notes table): opening it adds the table and leaves the sessions as they are
    #[test]
    fn a_registry_without_notes_gains_the_table_and_keeps_its_sessions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("{} pragma user_version = 2;", v3_schema())).unwrap();
        conn.execute(
            "insert into sessions(id, title, tool, dir, parent, created_at, fork_of) values('a1', 'a', 'claude', '/p', 'p0', 1, 'b2')",
            [],
        )
        .unwrap();
        let st = Store::at(conn).unwrap();
        let a = st.get("a1").unwrap();
        assert_eq!((a.parent.as_str(), a.fork_of.as_str()), ("p0", "b2"));
        assert_eq!(st.conn.query_row("pragma user_version", [], |r| r.get::<_, i64>(0)).unwrap(), VERSION);
        let n = st.note_add("g", "写下来", "").unwrap();
        // Already upgraded: running the step again does nothing, the note is still there
        add_notes(&st.conn).unwrap();
        assert_eq!(st.notes().unwrap(), [n]);
    }

    fn note(id: &str, group: &str, text: &str, at: u64) -> Note {
        Note { id: id.into(), group: group.into(), text: text.into(), created: at, updated: at, ..Note::default() }
    }

    #[test]
    fn a_groups_notes_list_pinned_first_then_the_latest_change() {
        let st = Store::memory();
        assert!(st.note_put(&note("aa11", "work", "\n发布前 \n先跑测试", 10)).unwrap());
        assert!(st.note_put(&note("ab22", "work", "口径", 20)).unwrap());
        assert!(st.note_put(&note("cc33", "work/app", "子组的", 30)).unwrap());
        // An id that's taken: nothing changes
        assert!(!st.note_put(&note("aa11", "work", "同一个 id", 40)).unwrap());
        let ids = |g: &str| st.notes_in(g).unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>();
        assert_eq!(ids("work"), ["ab22", "aa11"]);
        st.note_pin("aa11", true).unwrap();
        assert_eq!(ids("work"), ["aa11", "ab22"]);
        assert_eq!(st.notes().unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>(), ["aa11", "cc33", "ab22"]);
        // Found by its id or the start of it; its name is the first line with anything on it
        assert_eq!(st.note("aa").unwrap().name(), "发布前");
        assert!(st.note("a").unwrap_err().contains("2 条"));
        assert!(st.note("zz").unwrap_err().contains("没有"));
        // A new one gets an id of its own, and says who wrote it
        let n = st.note_add("work", "新的", "s1").unwrap();
        assert_eq!((n.by.as_str(), n.id.len()), ("s1", 8));
        st.note_set(&n.id, "改过").unwrap();
        assert_eq!(st.note(&n.id).unwrap().text, "改过");
        assert!(st.note_set("nope", "x").is_err() && st.note_pin("nope", true).is_err());
    }

    #[test]
    fn a_deleted_note_is_out_of_every_list_until_brought_back_or_purged() {
        let st = Store::memory();
        let (a, b) = (st.note_add("work", "a", "").unwrap(), st.note_add("work", "b", "").unwrap());
        let listed = |st: &Store| st.notes().unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>();
        st.note_bin(&a.id).unwrap();
        assert_eq!(listed(&st), [b.id.as_str()]);
        assert!(st.note(&a.id).is_err());
        assert_eq!(st.notes_gone().unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>(), [a.id.as_str()]);
        st.note_unbin(&a.id).unwrap();
        assert_eq!(listed(&st).len(), 2);
        // Purged: only the ones deleted that long ago
        st.note_bin(&a.id).unwrap();
        assert_eq!(st.notes_purge(paths::now() - 10).unwrap(), 0);
        assert_eq!(st.notes_purge(paths::now()).unwrap(), 1);
        assert!(st.notes_gone().unwrap().is_empty() && st.note_unbin(&a.id).is_err());
        // Rewriting a deleted one brings it back
        st.note_bin(&b.id).unwrap();
        st.note_set(&b.id, "b2").unwrap();
        assert_eq!(st.note(&b.id).unwrap().text, "b2");
        st.note_remove(&b.id).unwrap();
        assert!(listed(&st).is_empty());
    }

    #[test]
    fn notes_follow_their_group_when_renamed_and_go_with_it_when_deleted() {
        let st = Store::memory();
        for g in ["my_app/deep", "myXapp"] {
            st.group_create(g, "").unwrap();
        }
        let (top, deep) = (st.note_add("my_app", "顶层", "").unwrap(), st.note_add("my_app/deep", "子组", "").unwrap());
        let x = st.note_add("myXapp", "别的组", "").unwrap();
        st.group_rename("my_app", "play/app").unwrap();
        let at = |id: &str| st.note(id).unwrap().group;
        assert_eq!((at(&top.id), at(&deep.id), at(&x.id)), ("play/app".into(), "play/app/deep".into(), "myXapp".into()));
        // The interface deletes a group: its notes and its subgroups' go with that trash entry (_ in a name isn't a wildcard)
        st.group_rename("play/app", "my_app").unwrap();
        assert_eq!(st.notes_bin_group("my_app", "entry-1").unwrap(), 2);
        st.group_drop("my_app").unwrap();
        assert_eq!(st.notes().unwrap(), std::slice::from_ref(&x));
        // ... not as notes deleted on their own, and they come back with the entry
        assert!(st.notes_gone().unwrap().is_empty());
        assert_eq!(st.notes_unbin_with("entry-1").unwrap(), 2);
        assert_eq!(st.notes().unwrap().len(), 3);
        // The entry deleted for good takes them along
        st.notes_bin_group("my_app", "entry-2").unwrap();
        assert_eq!(st.notes_drop_with("entry-2").unwrap(), 2);
        assert_eq!(st.notes_unbin_with("entry-2").unwrap(), 0);
        // `aca group rm`: the sessions move up a level, the notes are deleted each on its own
        st.group_remove("myXapp").unwrap();
        assert_eq!(st.notes_gone().unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>(), [x.id]);
        assert!(st.notes().unwrap().is_empty());
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

    #[test]
    fn a_group_created_in_a_directory_another_group_has_is_not_created_at_all() {
        let st = Store::memory();
        let (root, d) = dirs(&["home", "home/rss"]);
        st.group_create_in("home", &d[0]).unwrap();
        // Its parent is made along with it; only the group itself gets the directory
        st.group_create_in("home/rss", &d[1]).unwrap();
        assert_eq!((default_dir(&st, "home"), default_dir(&st, "home/rss")), (d[0].clone(), d[1].clone()));
        // Taken (written another way, too): refused, and no group is left behind
        let err = st.group_create_in("feeds", &format!("{}/", d[1])).unwrap_err();
        assert!(err.contains("home/rss"), "{err}");
        assert!(!st.groups().unwrap().iter().any(|g| g.path == "feeds"));
        // One that's already there keeps the directory it has; one with none gets it
        let (root2, more) = dirs(&["other"]);
        st.group_create_in("home", &more[0]).unwrap();
        assert_eq!(default_dir(&st, "home"), d[0]);
        st.group_create("bare", "").unwrap();
        st.group_create_in("bare", &more[0]).unwrap();
        assert_eq!(default_dir(&st, "bare"), more[0]);
        // No directory: just the group
        st.group_create_in("plain", "").unwrap();
        assert_eq!(default_dir(&st, "plain"), "");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(root2);
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
