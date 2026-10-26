//! `aca import agent-deck`:把 agent-deck 登记的分组和会话搬进 aca 的登记表。只读它的库,不动它任何东西。
//!
//! 搬过来的是**登记**(标题、分组、顺序、工具、账号、目录、对话 id),不是在跑的进程:
//! 在 aca 里 `start` 会用同一个对话 id 另起一个 agent。所以默认只搬 agent-deck 那边已经没在跑的;
//! 还在跑的先在那边停掉再搬(`--all` 连在跑的也登记进来,但别两边同时开同一段对话)。
//!
//! 会话 id 沿用 agent-deck 的:便签、置顶这些按 id 记的东西接得上。
//! 账号按对话记录实际在哪个配置目录认,不靠 agent-deck 的账号槽(它那边空着的槽背后是分组级 / 全局的默认)。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::config::Config;
use crate::paths::{self, Res};
use crate::runtime;
use crate::store::{Session, Store};
use crate::{t, tr};

struct Found {
    s: Session,
    alive: bool,
    transcript: Option<PathBuf>,
    /// 搬不了的原因
    skip: Option<String>,
}

fn open_ro(db: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(format!("file:{}?mode=ro", db.display()), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
}

/// 这段对话的记录在哪个账号的配置目录里:(账号名, 记录)。怎么找是各家自己的([`crate::agent::Agent::conv_files`])。
/// 几个账号都找得到(Codex 的几个账号常把 `sessions` 链到同一个目录)时,`prefer` 那个账号也有就是它
pub(crate) fn locate(cfg: &Config, kind: &str, conv: &str, prefer: &str) -> Option<(String, PathBuf)> {
    let agent = crate::agent::builtin(kind)?;
    let accounts = cfg.accounts.get(kind)?;
    // 不换目录的账号(只带 env / 参数)对话记在默认目录里,认不出是谁的
    let homes = accounts.iter().filter(|(_, a)| !a.home.is_empty());
    let first = homes.clone().filter(|(name, _)| name.as_str() == prefer);
    first.chain(homes).find_map(|(name, a)| {
        let home = PathBuf::from(&a.home);
        let rel = agent.conv_files(&home, conv).into_iter().next()?;
        Some((name.clone(), home.join(rel)))
    })
}

/// 分组:(路径, 先后, 默认目录)
type Groups = Vec<(String, i64, String)>;

const INSTANCE: &str = "id, title, title_locked, group_path, sort_order, tool, coalesce(account, ''), project_path, tool_data, \
                        tmux_session, coalesce(archived_at, 0), coalesce(parent_session_id, ''), created_at";

/// agent-deck 的一条 → (照 aca 的样子摆好的一行, 它的 tmux 会话名)
fn instance(r: &rusqlite::Row) -> rusqlite::Result<(Session, String)> {
    let tool_data: serde_json::Value = serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default();
    Ok((
        Session {
            id: r.get(0)?,
            title: r.get(1)?,
            title_locked: r.get::<_, i64>(2)? != 0,
            group: r.get(3)?,
            order: r.get(4)?,
            tool: r.get(5)?,
            account: r.get(6)?,
            dir: r.get(7)?,
            args: vec![],
            conv: tool_data["claude_session_id"].as_str().or(tool_data["codex_session_id"].as_str()).unwrap_or("").to_string(),
            parent: r.get(11)?,
            fork_of: String::new(),
            // agent-deck 记的是纳秒或秒,统一成秒
            created: match r.get::<_, i64>(12)? as u64 {
                t if t > 100_000_000_000 => t / 1_000_000_000,
                t => t,
            },
            archived: r.get::<_, i64>(10)? as u64,
        },
        r.get::<_, String>(9)?,
    ))
}

/// 各个 profile 的库
fn dbs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut dbs: Vec<PathBuf> = std::fs::read_dir(root)?.flatten().map(|d| d.path().join("state.db")).filter(|p| p.is_file()).collect();
    dbs.sort();
    Ok(dbs)
}

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/agent-deck/profiles")
}

/// agent-deck 那边登记的一条:接管([`crate::adopt`])它起的、正在跑的会话时,标题、分组、id 照那边的
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub profile: String,
    /// 照 aca 的样子摆好的一行;工具和账号是 agent-deck 的叫法,接管时不用(看进程认)
    pub s: Session,
}

fn record_in(root: &Path, instance_id: Option<&str>, tmux: &str) -> Option<Record> {
    dbs(root).ok()?.into_iter().find_map(|db| {
        let conn = open_ro(&db).ok()?;
        // 先按它自己的 id 认(进程环境里带着),再按 tmux 会话名
        let sql = format!("select {INSTANCE} from instances where id = ?1 or (tmux_session = ?2 and ?2 != '') order by id != ?1 limit 1");
        let (s, _) = conn.query_row(&sql, [instance_id.unwrap_or(""), tmux], instance).ok()?;
        let profile = db.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Some(Record { profile, s })
    })
}

/// 这个 tmux 会话是不是 agent-deck 起的:按它的会话 id(进程环境里的 `AGENTDECK_INSTANCE_ID`)找,没有再按 tmux 会话名。
/// 没装 agent-deck、不是它的 = None。只读
pub fn agent_deck_record(instance_id: Option<&str>, tmux: &str) -> Option<Record> {
    record_in(&root(), instance_id, tmux)
}

fn hook_in(deck: &Path, id: &str) -> Option<(String, u64)> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(deck.join("hooks").join(format!("{id}.json"))).ok()?).ok()?;
    Some((v["event"].as_str()?.to_string(), v["ts"].as_u64()?))
}

/// agent-deck 的 hook 给这个会话记的最后一件事:(事件名, 时刻)。它起的进程带着它的 hook(装在各账号的 settings.json 里),
/// 接管过来、还没重启时照常往 `hooks/<id>.json` 写,id 就是接管时沿用的那个。没有这个文件 = None。只读
pub fn agent_deck_hook(id: &str) -> Option<(String, u64)> {
    hook_in(root().parent()?, id)
}

fn read(db: &Path, cfg: &Config, live: &[String]) -> rusqlite::Result<(Groups, Vec<Found>)> {
    let conn = open_ro(db)?;
    let groups = conn
        .prepare("select path, sort_order, coalesce(default_path, '') from groups")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows =
        conn.prepare(&format!("select {INSTANCE} from instances"))?.query_map([], instance)?.collect::<rusqlite::Result<Vec<_>>>()?;
    let found = rows
        .into_iter()
        .map(|(mut s, tmux)| {
            let alive = !tmux.is_empty() && live.contains(&tmux);
            let kind = cfg.tool(&s.tool).map(|t| t.kind);
            let located = kind.as_deref().and_then(|k| locate(cfg, k, &s.conv, &s.account));
            let skip = match &kind {
                None => Some(tr!("config.toml 中没有工具 {}", "no tool {} in config.toml", s.tool)),
                Some(k) if !s.account.is_empty() && located.is_none() && cfg.account(k, &s.account).is_none() => {
                    Some(tr!("[accounts.{k}] 中没有 {}", "no {} under [accounts.{k}]", s.account))
                }
                _ => None,
            };
            let transcript = located.map(|(account, t)| {
                s.account = account;
                t
            });
            Found { s, alive, transcript, skip }
        })
        .collect();
    Ok((groups, found))
}

pub fn agent_deck(store: &Store, cfg: &Config, apply: bool, all: bool) -> Res<()> {
    let root = root();
    let dbs = dbs(&root).map_err(|e| format!("{}:{e}", root.display()))?;
    // agent-deck 的会话在真的那个 tmux 服务上,不看 ACA_TMUX_SOCKET
    let live: Vec<String> = std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default();
    let (mut took, mut left) = (0, 0);
    for db in dbs {
        let profile = db.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (groups, found) = read(&db, cfg, &live).map_err(|e| format!("{}:{e}", db.display()))?;
        println!(
            "{}",
            tr!("profile {profile}:{} 个分组,{} 个会话", "profile {profile}: {} groups, {} sessions", groups.len(), found.len())
        );
        if apply {
            for (path, order, default_path) in &groups {
                store.group_create(path, default_path)?;
                store.group_order(path, *order)?;
            }
        }
        for f in found {
            let why = match (&f.skip, store.get(&f.s.id).is_ok(), f.alive && !all) {
                (Some(w), _, _) => Some(w.clone()),
                (_, true, _) => Some(t!("已导入", "already imported").into()),
                (_, _, true) => Some(
                    t!(
                        "在 agent-deck 中仍在运行:先停止,或直接接管(aca adopt)",
                        "still running there: stop it first, or take it over without stopping (aca adopt)"
                    )
                    .into(),
                ),
                _ => None,
            };
            let who = if f.s.account.is_empty() { f.s.tool.clone() } else { format!("{}:{}", f.s.tool, f.s.account) };
            let mark = match (&why, f.s.archived > 0) {
                (Some(_), _) => t!("跳过", "skip"),
                (None, true) => t!("导入(已归档)", "import (archived)"),
                (None, false) => t!("导入", "import"),
            };
            println!(
                "  {mark}  {}  {who}  {}{}",
                f.s.title,
                if f.s.group.is_empty() { t!("(未分组)", "(no group)") } else { &f.s.group },
                why.as_ref().map(|w| format!("  — {w}")).unwrap_or_default()
            );
            if why.is_some() {
                left += 1;
                continue;
            }
            took += 1;
            if apply {
                store.insert(&f.s)?;
                runtime::update(&f.s.id, |r| {
                    r.locked = f.s.title_locked;
                    r.note_conv(&f.s.conv);
                    r.transcript = f.transcript.as_ref().map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
                });
            }
        }
    }
    match apply {
        true => println!(
            "{}",
            tr!(
                "已导入 {took} 个,跳过 {left} 个。它们目前已停止:aca start <id> 可继续对话",
                "imported {took}, skipped {left}. They're stopped now: aca start <id> resumes the conversation"
            )
        ),
        false => println!(
            "{}",
            tr!(
                "将导入 {took} 个,跳过 {left} 个。以上仅为预览;加 --apply 才会写入 {}",
                "would import {took}, skip {left}. This only lists them; add --apply to write into {}",
                paths::db().display()
            )
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_agent_deck_session_is_found_by_its_id_or_its_tmux_name() {
        let root = std::env::temp_dir().join(format!("aca-deck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work")).unwrap();
        // 没有库的 profile 目录跳过
        std::fs::create_dir_all(root.join("empty")).unwrap();
        let conn = Connection::open(root.join("work/state.db")).unwrap();
        conn.execute_batch(
            "create table instances(id text, title text, title_locked integer, group_path text, sort_order integer, tool text, account text, \
             project_path text, tool_data text, tmux_session text, archived_at integer, parent_session_id text, created_at integer);
             insert into instances values('ad-1', '修登录', 1, 'work/app', 3, 'claude', 'sub', '/p/app', \
               '{\"claude_session_id\":\"c-1\"}', 'agentdeck_a_1', 0, 'ad-0', 1790000000000000000);
             insert into instances values('ad-2', 'codex 的', 0, 'my-sessions', 1, 'codex', NULL, '/p/b', '{}', 'agentdeck_b_2', 0, '', 1790000001);",
        )
        .unwrap();
        drop(conn);
        let r = record_in(&root, None, "agentdeck_a_1").unwrap();
        assert_eq!((r.profile.as_str(), r.s.id.as_str(), r.s.title.as_str(), r.s.title_locked), ("work", "ad-1", "修登录", true));
        // 纳秒记的创建时间统一成秒;对话 id 从它的 tool_data 里取
        assert_eq!((r.s.group.as_str(), r.s.conv.as_str(), r.s.parent.as_str(), r.s.created), ("work/app", "c-1", "ad-0", 1_790_000_000));
        // 进程环境里带着的 id 比 tmux 会话名可靠:两个都给时听 id 的
        assert_eq!(record_in(&root, Some("ad-2"), "agentdeck_a_1").unwrap().s.id, "ad-2");
        assert_eq!(record_in(&root, Some("没有的"), "agentdeck_b_2").unwrap().s.id, "ad-2");
        assert_eq!(record_in(&root, None, "别的会话"), None);
        assert_eq!(record_in(&root, None, ""), None);
        assert_eq!(record_in(&root.join("nowhere"), None, "agentdeck_a_1"), None);
        // 它的 hook 记的最后一件事,按会话 id 一个文件
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("hooks/ad-1.json"),
            r#"{"status":"waiting","session_id":"c-1","event":"Stop","ts":1790000500,"cwd":"/p/app"}"#,
        )
        .unwrap();
        std::fs::write(root.join("hooks/ad-2.json"), "写到一半").unwrap();
        assert_eq!(hook_in(&root, "ad-1"), Some(("Stop".to_string(), 1_790_000_500)));
        assert_eq!(
            (hook_in(&root, "ad-2"), hook_in(&root, "没有的"), hook_in(&root, ""), hook_in(&root, "../hooks/ad-1")),
            (None, None, None, None)
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
