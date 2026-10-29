//! `aca adopt`: adopt a running tmux session that aca didn't start — without stopping it.
//!
//! Adopting = registering one row + marking `@aca_id` on that tmux session (`@aca_agent` on the agent's pane, remain-on-exit on
//! its window) + renaming the session to the title. The processes inside keep running as they are; not a single key is pressed
//! into them. From then on it's an ordinary aca session: rename, move groups, stop, restart in place, fork.
//!
//! How much it can recognize:
//! - **Which tool**: from the process running in the pane ([`proc::agent_at`]); if unrecognized, it's registered as shell, or
//!   named with `--as`
//! - **Which account**: the configuration directory in the process environment, matched to an account registered in config.toml
//!   ([`account_of`]); an unregistered directory is recorded as the directory itself
//! - **Which conversation, title, arguments**: each tool recognizes its own ([`agent::Agent::live`]). Claude's are all
//!   recognized; for Codex, only one started with `codex resume <id>` has a recognizable conversation
//! - **Started by agent-deck**: title, group and id as registered there (notes and other things kept by id carry over). Its
//!   database is only read, never written
//!
//! After adopting and before a restart, the process doesn't carry aca's hooks: state comes only from reading the screen
//! (running / needs you show, unread doesn't), and the title follows what it writes into the pane title
//! ([`status::follow_titles`]).
//! The exception is one started by agent-deck: it carries agent-deck's hooks and keeps reporting, the list borrows what that
//! side records ([`status::load`]), and unread doesn't wait for a restart.
//! After one restart (resuming the conversation) it's the same as one aca started itself. The reverse is
//! [`crate::session::release`]: only the registration is removed, the process stays.

use serde_json::json;

use crate::agent::{self, Live};
use crate::config::{self, Account, Config};
use crate::import::{self, Record};
use crate::paths::{self, Res};
use crate::proc::{self, Image, Kids};
use crate::store::{Session, Store};
use crate::tmux::{self, Pane};
use crate::{event, runtime, session, status};
use crate::{t, tr};

#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// When given, used as the title (locked; no longer follows the one the agent sets)
    pub title: String,
    pub group: Option<String>,
    /// `--as tool[:account]`: register as this without looking at the process (for when it's unrecognized or wrongly recognized)
    pub spec: String,
    /// `--conv`: conversation id (give it yourself when it can't be recognized)
    pub conv: String,
    /// What follows `--`: arguments added at every start. When given, the ones on its command line aren't copied
    pub args: Option<Vec<String>>,
}

/// What was recognized and how it would be registered. [`plan`] only looks; [`apply`] writes
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub s: Session,
    /// The registry already has this id (the row moved over from agent-deck that's still stopped): don't register anew, attach this
    /// tmux session to it
    pub known: bool,
    /// tmux's session id, the current session name, the agent's pane
    pub sid: String,
    pub tmux: String,
    pub pane: String,
    pub transcript: String,
    /// The title the agent set itself (shown when the name isn't locked)
    pub auto_title: String,
    /// agent-deck has it registered: (profile, id)
    pub deck: Option<(String, String)>,
    /// For telling people: what wasn't recognized, what to watch out for afterwards
    pub notes: Vec<String>,
}

impl Plan {
    /// The title shown after adopting (the tmux session is renamed to it)
    pub fn title(&self) -> &str {
        if self.s.title_locked || self.auto_title.is_empty() { &self.s.title } else { &self.auto_title }
    }

    /// "claude:sub"
    pub fn who(&self) -> String {
        match self.s.account.as_str() {
            "" => self.s.tool.clone(),
            a => format!("{}:{}", self.s.tool, agent::account_label(a)),
        }
    }
}

/// Read from outside: all panes, the process table
struct World {
    panes: Vec<Pane>,
    kids: Kids,
}

impl World {
    fn read() -> World {
        World { panes: tmux::panes(), kids: proc::fg_kids() }
    }
}

/// The registry has this id (an exact match, not "the first few characters")
fn registered(store: &Store, id: &str) -> Option<Session> {
    store.sessions(true).ok()?.into_iter().find(|s| s.id == id)
}

/// "Which one to adopt" → (tmux session id, the named pane). Accepts: `%pane-id`, `$session-id`, a session name, or part of a
/// session name (when it matches only one)
fn locate<'a>(panes: &'a [Pane], target: &str) -> Res<(&'a str, Option<&'a Pane>)> {
    if target.starts_with('%') {
        let p = panes.iter().find(|p| p.pane == target).ok_or_else(|| tr!("tmux 中没有窗格 {target}", "no pane {target} in tmux"))?;
        return Ok((&p.sid, Some(p)));
    }
    let exact = panes.iter().find(|p| p.sid == target || p.session == target);
    if let Some(p) = exact {
        return Ok((&p.sid, None));
    }
    let mut hits: Vec<&Pane> = panes.iter().filter(|p| p.session.contains(target)).collect();
    hits.dedup_by_key(|p| p.sid.as_str());
    match hits.as_slice() {
        [] => Err(tr!(
            "tmux 中没有会话 {target}(不带参数运行 aca adopt 可列出能接管的会话)",
            "no tmux session {target} (aca adopt with no arguments lists what can be adopted)"
        )),
        [one] => Ok((&one.sid, None)),
        many => Err(tr!(
            "{target} 匹配到 {} 个会话:{},请写得更完整",
            "{target} matches {} sessions: {}; type more of it",
            many.len(),
            many.iter().map(|p| p.session.as_str()).collect::<Vec<_>>().join(t!("、", ", "))
        )),
    }
}

/// A tmux session's panes, by window number and pane number
fn panes_of<'a>(panes: &'a [Pane], sid: &str) -> Vec<&'a Pane> {
    let mut v: Vec<&Pane> = panes.iter().filter(|p| p.sid == sid).collect();
    v.sort_by_key(|p| (p.window, p.index));
    v
}

/// Whether `a` appears contiguously in `b` (empty doesn't count)
fn within(a: &[String], b: &[String]) -> bool {
    !a.is_empty() && b.windows(a.len()).any(|w| w == a)
}

/// Which account this process uses. `home` is the configuration directory in its environment (same directory = None).
/// - Changed directory: the registered account with the same directory; if there are several, pick the ones whose environment
///   variables and arguments all match the process, the one carrying the most first; if none match (the directory is
///   registered, but that account carries more arguments that this process doesn't have) or it was never registered: record the
///   directory itself
/// - Same directory: the matching one among accounts that only carry environment variables / arguments; none match = no
///   account (it uses the default)
fn account_of(cfg: &Config, kind: &str, home: Option<&str>, im: Option<&Image>) -> String {
    let fits = |a: &Account| {
        let env_ok = a.env.iter().all(|(k, v)| im.and_then(|im| im.var(k)) == Some(paths::expand(v).as_str()));
        let args_ok = a.args.is_empty() || im.is_some_and(|im| within(&a.args, &im.argv));
        env_ok && args_ok
    };
    let weight = |a: &Account| a.env.len() + a.args.len();
    let best = cfg
        .accounts
        .get(kind)
        .into_iter()
        .flatten()
        .filter(|(_, a)| a.home == home.unwrap_or("") && fits(a))
        // With the directory unchanged, an "account" that carries nothing matches nothing; one relying only on env_file can't be told
        // (it holds a key, which isn't read)
        .filter(|(_, a)| home.is_some() || weight(a) > 0)
        .max_by_key(|(name, a)| (weight(a), std::cmp::Reverse(name.as_str())))
        .map(|(name, _)| name.clone());
    best.or_else(|| home.map(String::from)).unwrap_or_default()
}

/// From the copied arguments, drop the run the tool / account already adds itself (or after a restart they'd be passed twice)
fn without_known(mut args: Vec<String>, known: &[&[String]]) -> Vec<String> {
    for k in known.iter().filter(|k| !k.is_empty()) {
        if let Some(at) = args.windows(k.len()).position(|w| w == *k) {
            args.drain(at..at + k.len());
        }
    }
    args
}

/// Not a name a person gave: a number tmux made up (`0`, `1` …), or one agent-deck made up (`agentdeck_xxx_3f2a`; only reached
/// when its registration on that side is already gone)
fn default_name(name: &str) -> bool {
    !name.is_empty() && (name.chars().all(|c| c.is_ascii_digit()) || name.starts_with("agentdeck_"))
}

/// The recognized pieces, put together into the row to register. A pure function
fn draft(
    pane: &Pane,
    tool: &str,
    account: String,
    live: &Live,
    known_args: &[&[String]],
    extra_argv: bool,
    record: Option<&Record>,
    opts: &Opts,
) -> (Session, Vec<String>) {
    let mut notes = vec![];
    let rec = record.map(|r| &r.s);
    let title = opts.title.trim();
    let (title, locked) = match (title.is_empty(), rec) {
        (false, _) => (title.to_string(), true),
        (true, Some(r)) => (r.title.clone(), r.title_locked),
        // A tmux session name you gave yourself stays as it is (to follow the title the agent sets: aca rename <id> --auto)
        (true, None) => (pane.session.clone(), !default_name(&pane.session)),
    };
    let conv = [opts.conv.trim(), live.conv.as_str(), rec.map_or("", |r| r.conv.as_str())].into_iter().find(|c| !c.is_empty());
    if conv.is_none() && tool != "shell" {
        notes.push(
            t!(
                "无法识别当前对话,重启后将开始新对话。如果知道对话 id,接管时加 --conv <id>",
                "couldn't tell which conversation it's in: a restart starts a new one. If you know the id, adopt with --conv <id>"
            )
            .into(),
        );
    }
    let args = match (&opts.args, &live.args) {
        (Some(given), _) => given.clone(),
        (None, Some(found)) => without_known(found.clone(), known_args),
        (None, None) => {
            if extra_argv {
                notes.push(t!("未沿用它的命令行参数(其中有一段文字,无法区分哪些是参数);需要每次启动都带的参数,接管时写在 -- 后面", "its command-line arguments weren't copied (they contain a message, so it can't tell which are arguments); for arguments on every start, put them after -- when adopting").into());
            }
            vec![]
        }
    };
    let s = Session {
        // id and order are decided outside (the registry has to be looked up)
        id: String::new(),
        title,
        title_locked: locked,
        group: opts.group.as_deref().map(|g| g.trim_matches('/').to_string()).or(rec.map(|r| r.group.clone())).unwrap_or_default(),
        order: 0,
        tool: tool.to_string(),
        account,
        dir: if pane.path.is_empty() { rec.map(|r| r.dir.clone()).unwrap_or_default() } else { pane.path.clone() },
        args,
        conv: conv.unwrap_or_default().to_string(),
        parent: rec.map(|r| r.parent.clone()).unwrap_or_default(),
        fork_of: String::new(),
        created: rec.map_or_else(paths::now, |r| r.created),
        archived: 0,
    };
    (s, notes)
}

fn plan_in(store: &Store, cfg: &Config, w: &World, target: &str, opts: &Opts) -> Res<Plan> {
    let (sid, named) = locate(&w.panes, target)?;
    let all = panes_of(&w.panes, sid);
    if let Some(p) = all.iter().find(|p| !p.id.is_empty() && registered(store, &p.id).is_some()) {
        return Err(tr!("「{}」已是 aca 会话({})", "“{}” is already an aca session ({})", p.session, p.id));
    }
    // Which pane the agent is in: the named one; if none is named, the first one with a recognizable agent; if none, the first pane
    let found = |p: &Pane| if p.dead { None } else { proc::agent_at(&w.kids, p.pid) };
    let pane = named
        .or_else(|| all.iter().copied().find(|p| found(p).is_some()))
        .or(all.first().copied())
        .ok_or(t!("该会话中没有窗格", "this session has no panes"))?;
    if pane.dead {
        return Err(tr!("窗格 {} 中的程序已退出,无法接管", "the program in pane {} has exited; nothing to adopt", pane.pane));
    }
    let running = found(pane);
    let mut notes = vec![];
    let (tool, given) = match opts.spec.split_once(':') {
        Some((t, a)) => (t.to_string(), Some(a)),
        None if !opts.spec.is_empty() => (opts.spec.clone(), None),
        None => (running.map_or("shell", |r| r.0).to_string(), None),
    };
    if opts.spec.is_empty() && running.is_none() {
        notes.push(t!("无法识别其中运行的 agent,已登记为 shell;如果不对,先移出 aca(aca release)再接管,并加上 --as 工具[:账号]", "couldn't tell which agent runs inside, so it's registered as a shell; if it's something else, let go (aca release) and adopt again with --as TOOL[:ACCOUNT]").into());
    }
    let (agent, def) = agent::resolve(cfg, &tool)?;
    let pid = running.map_or(pane.pid, |r| r.1);
    let image = proc::image(pid);
    let var = |name: &str| match &image {
        Some(im) => im.var(name).map(String::from),
        None => proc::env_var(pid, name),
    };
    let home = agent.home_var().and_then(var).filter(|d| !d.is_empty()).map(|d| config::dir(&d));
    let account = match given {
        // An account was named: recognized the way `aca new` does (a registered name, or a directory)
        Some(_) => agent::pick(cfg, &opts.spec)?.account,
        None => account_of(cfg, agent.name(), home.as_deref(), image.as_ref()),
    };
    let layer = agent::account(cfg, &*agent, &account)?;
    let argv: &[String] = image.as_ref().map_or(&[], |im| &im.argv);
    let mut live = agent.live(pid, home.as_deref(), argv);
    let record = import::agent_deck_record(var("AGENTDECK_INSTANCE_ID").as_deref(), &pane.session);
    let known_args = [def.args.as_slice(), layer.args.as_slice()];
    let (mut s, more) = draft(pane, &tool, account, &live, &known_args, argv.len() > 1, record.as_ref(), opts);
    notes.extend(more);
    // The conversation was given by hand / is the one agent-deck recorded: its record has to be found separately (Claude can't
    // resume without the record)
    if s.conv != live.conv {
        live.transcript =
            import::locate(cfg, agent.name(), &s.conv, &s.account).map(|(_, t)| t.to_string_lossy().into_owned()).unwrap_or_default();
    }
    // id: one started by agent-deck keeps the id from that side. If the registry already has this row (moved over in bulk, still
    // stopped): attach to it and leave that row unchanged
    let old = record.as_ref().and_then(|r| registered(store, &r.s.id));
    // That registration is already attached to another tmux session (shouldn't happen: the same row adopted twice): give this one a
    // new id
    let taken = old.as_ref().is_some_and(|o| w.panes.iter().any(|p| p.id == o.id));
    let known = old.is_some() && !taken;
    // This process was started by aca in the first place (released, then adopted again): the hooks it carries still report to the
    // old id, so keep it, and state doesn't wait for a restart
    let hooked = var("ACA_SESSION_ID").filter(|id| !id.is_empty() && !id.contains('/') && registered(store, id).is_none());
    let from_deck = record.as_ref().filter(|_| old.is_none()).map(|r| r.s.id.clone());
    match old {
        // That row was moved over from agent-deck's registration, with no idea which arguments it carries: conversation and arguments go
        // by what's on the process now, the rest is unchanged
        Some(o) if !taken => s = Session { conv: if s.conv.is_empty() { o.conv.clone() } else { s.conv }, args: s.args, archived: 0, ..o },
        _ => s.id = hooked.clone().or(from_deck).unwrap_or_else(|| session::fresh_id(store)),
    }
    if !known {
        s.order = store.next_order(&s.group);
        if !s.parent.is_empty() && registered(store, &s.parent).is_none() {
            s.parent.clear();
        }
    }
    if let Some(r) = &record {
        notes.push(tr!(
            "agent-deck 中仍登记着它,接管后在 agent-deck 中会显示为已停止:不要在 agent-deck 中再次启动(会为同一对话再启动一个),不再需要时可删除:agent-deck -p {} remove {}", "agent-deck still has it registered and will show it as stopped: don't start it there again (that would start a second copy of the same conversation); once you don't need it, remove it: agent-deck -p {} remove {}",
            r.profile, r.s.id
        ));
    }
    if tool != "shell" && hooked.is_none() {
        notes.push(
            match import::agent_deck_hook(&s.id) {
                Some(_) => t!("重启前「未读」由 agent-deck 的 hook 记录;重启一次(aca restart,继续原对话)后改用 aca 自己的 hook", "until a restart, “finished, unread” comes from agent-deck's hooks; restart once (aca restart, the conversation continues) and it carries aca's own hooks"),
                None => t!("重启前状态只能通过读取屏幕判断(没有「未读」);重启一次(aca restart,继续原对话)后即可使用 hook", "until a restart, state comes only from reading the screen (no “finished, unread”); restart once (aca restart, the conversation continues) and it carries the hooks"),
            }
            .into(),
        );
    }
    Ok(Plan {
        known,
        sid: sid.to_string(),
        tmux: pane.session.clone(),
        pane: pane.pane.clone(),
        transcript: live.transcript,
        // What's written in the pane title wins: after adopting, the list follows it too (`status::follow_titles`)
        auto_title: agent.live_title(&runtime::Run::default(), &pane.title).unwrap_or(live.title),
        deck: record.map(|r| (r.profile, r.s.id)),
        notes,
        s,
    })
}

/// Show what a session would look like once adopted. Only looks, changes nothing.
/// `target`: a tmux session name (or part of the name), `$session-id`, `%pane-id` (to name the pane when the agent isn't in the
/// first one)
pub fn plan(store: &Store, cfg: &Config, target: &str, opts: &Opts) -> Res<Plan> {
    plan_in(store, cfg, &World::read(), target, opts)
}

/// The ones that can be adopted directly: unregistered tmux sessions with a recognizable agent running in the first pane.
/// Second return value: the other unregistered tmux sessions (no agent recognized, or the agent isn't in the first pane) — to
/// adopt those, name them
pub fn candidates(store: &Store, cfg: &Config) -> Res<(Vec<Plan>, Vec<String>)> {
    let w = World::read();
    let mine: Vec<String> = store.sessions(true)?.into_iter().map(|s| s.id).collect();
    let mut sids: Vec<&str> = w.panes.iter().map(|p| p.sid.as_str()).collect();
    sids.dedup();
    let (mut plans, mut others) = (vec![], vec![]);
    for sid in sids {
        let all = panes_of(&w.panes, sid);
        if all.iter().any(|p| !p.id.is_empty() && mine.contains(&p.id)) {
            continue;
        }
        let Some(first) = all.first() else { continue };
        match (!first.dead && proc::agent_at(&w.kids, first.pid).is_some()).then(|| plan_in(store, cfg, &w, sid, &Opts::default())) {
            Some(Ok(p)) => plans.push(p),
            _ => others.push(first.session.clone()),
        }
    }
    Ok((plans, others))
}

/// -g .: the group by directory is decided only when actually registering (several adopted at once with the same directory name
/// each get their own number)
fn by_dir(store: &Store, s: &mut Session) -> Res<()> {
    if s.group == session::BY_DIR {
        s.group = store.group_claim(&s.dir)?.0;
        s.order = store.next_order(&s.group);
    }
    Ok(())
}

/// Adopt as recognized by [`plan`]: mark the identity on the tmux session, register, rename the session to the title. Returns the
/// registered row and the current tmux session name
pub fn apply(store: &Store, p: &Plan) -> Res<(Session, String)> {
    let mut s = p.s.clone();
    let target = &p.pane;
    // Mark it on tmux first: if that session has just gone, this step fails and the registry is left alone
    let mut a: Vec<String> = vec![];
    for opt in [
        vec!["set-option", "-t", &p.sid, "@aca_id", &s.id],
        vec!["set-option", "-p", "-t", target, "@aca_agent", "1"],
        vec!["set-option", "-w", "-t", target, "remain-on-exit", "on"],
        vec!["set-option", "-w", "-t", target, "automatic-rename", "off"],
    ] {
        if !a.is_empty() {
            a.push(";".into());
        }
        a.extend(opt.iter().map(|x| x.to_string()));
    }
    tmux::run(&a).map_err(|e| tr!("无法在 tmux 中记录:{e}", "can't mark it in tmux: {e}"))?;
    tmux::bind_detach();
    if !p.known
        && let Err(e) = by_dir(store, &mut s).and_then(|()| store.insert(&s))
    {
        let _ = tmux::run(&["set-option", "-u", "-t", &p.sid, "@aca_id"]);
        let _ = tmux::run(&["set-option", "-p", "-u", "-t", target, "@aca_agent"]);
        return Err(e);
    }
    if p.known {
        store.set_conv(&s.id, &s.conv)?;
        store.set_args(&s.id, &s.args)?;
        store.set_archived(&s.id, 0)?;
    }
    let (_, r) = runtime::update(&s.id, |r| {
        r.locked = s.title_locked;
        r.note_conv(&s.conv);
        r.transcript = p.transcript.clone();
        if !p.auto_title.is_empty() {
            r.auto_title = p.auto_title.clone();
        }
        // Don't keep the marks from when it was stopped: it's running now
        r.clear_error();
        if r.status == "ended" {
            r.status = "idle".into();
        }
    });
    let title = status::title(&s, &r);
    let name = tmux::sync_name(&s.id, &title).unwrap_or_else(|| p.tmux.clone());
    event::emit(
        "session.created",
        &s.id,
        json!({ "title": title, "tool": s.tool, "account": s.account, "dir": s.dir, "group": s.group, "parent": s.parent, "adopted": true, "tmux": name }),
    );
    Ok((s, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(sid: &str, session: &str, pane: &str, window: u32) -> Pane {
        Pane { sid: sid.into(), session: session.into(), pane: pane.into(), window, pid: 1, path: "/p/app".into(), ..Default::default() }
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_session_is_named_by_pane_by_number_by_name_or_by_part_of_its_name() {
        let panes = vec![
            pane("$1", "agentdeck_iron-dune_92cb", "%1", 1),
            pane("$1", "agentdeck_iron-dune_92cb", "%2", 2),
            pane("$2", "agentdeck_iron-moon_573c", "%3", 1),
            pane("$3", "iron", "%4", 1),
        ];
        let at = |t: &str| locate(&panes, t).map(|(sid, p)| (sid.to_string(), p.map(|p| p.pane.clone())));
        assert_eq!(at("%2"), Ok(("$1".into(), Some("%2".into()))));
        assert_eq!(at("$2"), Ok(("$2".into(), None)));
        // A full name match wins over "contains this part"
        assert_eq!(at("iron"), Ok(("$3".into(), None)));
        assert_eq!(at("dune"), Ok(("$1".into(), None)));
        assert!(at("agentdeck_iron").unwrap_err().contains("2 个"));
        assert!(at("nope").unwrap_err().contains("没有"));
        assert!(at("%9").unwrap_err().contains("%9"));
    }

    #[test]
    fn the_account_is_the_registered_one_the_process_environment_points_at() {
        let cfg = config::parse(
            "[accounts.claude]\nsub = \"/h/.cs\"\nbedrock = { env = { CLAUDE_CODE_USE_BEDROCK = \"1\", AWS_PROFILE = \"work\" } }\nkeyed = { env_file = \"/h/k.env\" }\n\
             [accounts.codex]\ncorp = \"/h/.dd\"\nlabs = { home = \"/h/.ll\", args = [\"--profile\", \"labs\"] }\nlabs2 = \"/h/.ll\"\n",
        );
        let im = |argv: &[&str], env: &[(&str, &str)]| Image {
            argv: words(argv),
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        let plain = im(&["claude"], &[]);
        assert_eq!(account_of(&cfg, "claude", Some("/h/.cs"), Some(&plain)), "sub");
        // An unregistered directory: record the directory itself
        assert_eq!(account_of(&cfg, "claude", Some("/h/.new"), Some(&plain)), "/h/.new");
        // Same directory: the default one, no account
        assert_eq!(account_of(&cfg, "claude", None, Some(&plain)), "");
        // An account with only environment variables: counts only when they all match the process
        let br = im(&["claude"], &[("CLAUDE_CODE_USE_BEDROCK", "1"), ("AWS_PROFILE", "work")]);
        assert_eq!(account_of(&cfg, "claude", None, Some(&br)), "bedrock");
        assert_eq!(account_of(&cfg, "claude", None, Some(&im(&["claude"], &[("AWS_PROFILE", "work")]))), "");
        // Two accounts on one directory: the one whose arguments the process carries is it; without them, it's the one that only changes
        // the directory
        let ll = im(&["codex", "--yolo", "--profile", "labs"], &[]);
        assert_eq!(account_of(&cfg, "codex", Some("/h/.ll"), Some(&ll)), "labs");
        assert_eq!(account_of(&cfg, "codex", Some("/h/.ll"), Some(&im(&["codex", "--yolo"], &[]))), "labs2");
        // The process can't be read (not ours): an account that only changes the directory is still recognized
        assert_eq!(account_of(&cfg, "codex", Some("/h/.dd"), None), "corp");
        assert_eq!(account_of(&cfg, "codex", Some("/h/.ll"), None), "labs2");
        assert_eq!(account_of(&cfg, "gemini", Some("/h/.g"), None), "/h/.g");
    }

    #[test]
    fn the_row_takes_what_was_given_then_what_agent_deck_recorded_then_what_is_on_the_process() {
        let p = pane("$1", "agentdeck_iron-dune_92cb", "%1", 1);
        let live = Live {
            conv: "c-live".into(),
            title: "它自己起的".into(),
            args: Some(words(&["--yolo", "--profile", "labs", "--search"])),
            ..Default::default()
        };
        let rec = Record {
            profile: "default".into(),
            s: Session {
                id: "ad-1".into(),
                title: "那边的标题".into(),
                group: "work".into(),
                conv: "c-deck".into(),
                created: 7,
                parent: "ad-0".into(),
                dir: "/p/reg".into(),
                ..Default::default()
            },
        };
        let account_args = words(&["--profile", "labs"]);
        let known: [&[String]; 2] = [&[], &account_args];
        // Started by agent-deck: title, group, parent session and creation time as on that side; the conversation goes by what's on the
        // process now; arguments the account already adds aren't recorded again
        let (s, notes) = draft(&p, "codex", "labs".into(), &live, &known, true, Some(&rec), &Opts::default());
        assert_eq!(
            (s.title.as_str(), s.title_locked, s.group.as_str(), s.parent.as_str(), s.created),
            ("那边的标题", false, "work", "ad-0", 7)
        );
        assert_eq!((s.conv.as_str(), s.dir.as_str(), s.args), ("c-live", "/p/app", words(&["--yolo", "--search"])));
        assert!(notes.is_empty(), "{notes:?}");
        // No conversation recognized on the process: use the one recorded on that side
        let quiet = Live { args: Some(vec![]), ..Default::default() };
        assert_eq!(draft(&p, "codex", String::new(), &quiet, &known, false, Some(&rec), &Opts::default()).0.conv, "c-deck");
        // Whatever's given by hand wins; a given title gets locked
        let opts = Opts {
            title: " 我起的 ".into(),
            group: Some("/play/".into()),
            conv: "c-mine".into(),
            args: Some(words(&["--x"])),
            ..Default::default()
        };
        let (s, _) = draft(&p, "codex", String::new(), &live, &known, true, Some(&rec), &opts);
        assert_eq!(
            (s.title.as_str(), s.title_locked, s.group.as_str(), s.conv.as_str(), s.args),
            ("我起的", true, "play", "c-mine", words(&["--x"]))
        );

        // Not from agent-deck: the tmux session name you gave is the title, locked; numbers tmux made up don't count
        let mine = pane("$2", "修登录", "%2", 1);
        let (s, notes) = draft(&mine, "claude", "sub".into(), &Live::default(), &known, true, None, &Opts::default());
        assert_eq!((s.title.as_str(), s.title_locked, s.group.as_str(), s.conv.as_str()), ("修登录", true, "", ""));
        // No conversation recognized, arguments not copied for safety: both get mentioned
        assert!(notes.iter().any(|n| n.contains("--conv")) && notes.iter().any(|n| n.contains("未沿用")), "{notes:?}");
        for machine in ["0", "agentdeck_iron-dune_92cb"] {
            let (s, _) = draft(&pane("$3", machine, "%3", 1), "claude", String::new(), &quiet, &known, false, None, &Opts::default());
            assert_eq!((s.title.as_str(), s.title_locked), (machine, false));
        }
        // A shell has no conversation, so nothing to mention
        assert!(draft(&mine, "shell", String::new(), &Live::default(), &known, false, None, &Opts::default()).1.is_empty());
    }

    #[test]
    fn a_plan_shows_the_agents_own_title_unless_the_name_is_locked() {
        let p = |locked: bool, auto: &str| Plan {
            s: Session {
                title: "登记的".into(),
                title_locked: locked,
                tool: "claude".into(),
                account: "/h/.claude-x".into(),
                ..Default::default()
            },
            known: false,
            sid: "$1".into(),
            tmux: "t".into(),
            pane: "%1".into(),
            transcript: String::new(),
            auto_title: auto.into(),
            deck: None,
            notes: vec![],
        };
        assert_eq!(p(false, "它起的").title(), "它起的");
        assert_eq!(p(true, "它起的").title(), "登记的");
        assert_eq!(p(false, "").title(), "登记的");
        assert_eq!(p(false, "").who(), "claude:.claude-x");
    }
}
