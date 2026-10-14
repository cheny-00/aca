//! `aca adopt`:接管一个不是 aca 起的、正在跑的 tmux 会话 —— 不用停它。
//!
//! 接管 = 登记一行 + 在那个 tmux 会话上记 `@aca_id`(agent 那一格记 `@aca_agent`、那个窗口开 remain-on-exit)+ 会话名改成标题。
//! 里面的进程原样跑着,一个键都不往里按。之后它就是个普通的 aca 会话:改名、挪组、停、在原地重启、分叉都行。
//!
//! 认得出多少:
//! - **哪一家**:看窗格里跑的进程([`proc::agent_at`]);没认出就登记成 shell,或者 `--as` 指定
//! - **哪个账号**:进程环境里的配置目录对回 config.toml 里登记的账号([`account_of`]),没登记过的目录直接记目录
//! - **哪段对话、标题、参数**:各家自己认([`agent::Agent::live`])。Claude 都认得出;Codex 只有 `codex resume <id>` 起的认得出对话
//! - **agent-deck 起的**:标题、分组、id 照它那边登记的(便签这些按 id 记的接得上)。它的库只读不写
//!
//! 接管之后、重启之前,这个进程没带 aca 的 hook:状态只靠读屏幕(在跑 / 在等你看得出,「跑完没看」没有),
//! 标题跟它写在窗格标题上的([`status::follow_titles`])。
//! agent-deck 起的例外:它带着 agent-deck 的 hook、照常在报,列表借那边记的([`status::load`]),「跑完没看」不用等重启。
//! 重启一次(接着原来的对话)之后就和 aca 自己起的一样了。反过来是 [`crate::session::release`]:只删登记,进程照留。

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
    /// 给了就用它当标题(锁住,不再跟 agent 自己起的)
    pub title: String,
    pub group: Option<String>,
    /// `--as 工具[:账号]`:不看进程,照这个登记(没认出来、认错了时用)
    pub spec: String,
    /// `--conv`:对话 id(认不出来时自己给)
    pub conv: String,
    /// `--` 后面的:每次启动带的参数。给了就不照搬它命令行里的
    pub args: Option<Vec<String>>,
}

/// 认出来的、打算怎么登记。[`plan`] 只看不动,[`apply`] 才写
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub s: Session,
    /// 登记表里已经有这个 id(从 agent-deck 搬过来、还停着的那条):不新登记,把这个 tmux 会话接到它上面
    pub known: bool,
    /// tmux 的会话号、现在的会话名、agent 那一格
    pub sid: String,
    pub tmux: String,
    pub pane: String,
    pub transcript: String,
    /// agent 自己起的标题(没锁名字时显示它)
    pub auto_title: String,
    /// agent-deck 那边登记着它:(profile, id)
    pub deck: Option<(String, String)>,
    /// 要说给人听的:哪些没认出来、之后要留意什么
    pub notes: Vec<String>,
}

impl Plan {
    /// 接管之后显示的标题(tmux 会话名会改成它)
    pub fn title(&self) -> &str {
        if self.s.title_locked || self.auto_title.is_empty() { &self.s.title } else { &self.auto_title }
    }

    /// 「claude:sub」
    pub fn who(&self) -> String {
        match self.s.account.as_str() {
            "" => self.s.tool.clone(),
            a => format!("{}:{}", self.s.tool, agent::account_label(a)),
        }
    }
}

/// 从外面读来的:全部窗格、进程表
struct World {
    panes: Vec<Pane>,
    kids: Kids,
}

impl World {
    fn read() -> World {
        World { panes: tmux::panes(), kids: proc::fg_kids() }
    }
}

/// 登记表里有这个 id(原样对上,不是「开头几位」)
fn registered(store: &Store, id: &str) -> Option<Session> {
    store.sessions(true).ok()?.into_iter().find(|s| s.id == id)
}

/// 「接管哪个」→ (tmux 会话号, 指名的那一格)。认:`%窗格号`、`$会话号`、会话名、会话名里的一段(只对上一个时)
fn locate<'a>(panes: &'a [Pane], target: &str) -> Res<(&'a str, Option<&'a Pane>)> {
    if target.starts_with('%') {
        let p = panes.iter().find(|p| p.pane == target).ok_or_else(|| tr!("tmux 里没有 {target} 这一格", "no pane {target} in tmux"))?;
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
            "tmux 里没有 {target} 这个会话(aca adopt 不带参数列出能接管的)",
            "no tmux session {target} (aca adopt with no arguments lists what can be adopted)"
        )),
        [one] => Ok((&one.sid, None)),
        many => Err(tr!(
            "{target} 对上了 {} 个会话:{},写长一点",
            "{target} matches {} sessions: {}; type more of it",
            many.len(),
            many.iter().map(|p| p.session.as_str()).collect::<Vec<_>>().join(t!("、", ", "))
        )),
    }
}

/// 一个 tmux 会话的窗格,按窗口号、格号
fn panes_of<'a>(panes: &'a [Pane], sid: &str) -> Vec<&'a Pane> {
    let mut v: Vec<&Pane> = panes.iter().filter(|p| p.sid == sid).collect();
    v.sort_by_key(|p| (p.window, p.index));
    v
}

/// `a` 是不是连着出现在 `b` 里(空的不算)
fn within(a: &[String], b: &[String]) -> bool {
    !a.is_empty() && b.windows(a.len()).any(|w| w == a)
}

/// 这个进程用的是哪个账号。`home` 是它环境里的配置目录(没换目录 = None)。
/// - 换了目录的:登记的账号里目录一样的那个;有几个就挑它带的环境变量、参数在进程上都对得上的,带得最多的优先;
///   一个都对不上(目录登记过,但那个账号还要带别的参数,这个进程没带)或者根本没登记过:直接记目录
/// - 没换目录的:只带环境变量 / 参数的账号里对得上的那个;都对不上 = 不带账号(它用的是默认的)
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
        // 没换目录时,什么都不带的「账号」对不上任何东西;只靠 env_file 的看不出来(里面是 key,不读)
        .filter(|(_, a)| home.is_some() || weight(a) > 0)
        .max_by_key(|(name, a)| (weight(a), std::cmp::Reverse(name.as_str())))
        .map(|(name, _)| name.clone());
    best.or_else(|| home.map(String::from)).unwrap_or_default()
}

/// 照搬的参数里,工具 / 账号自己已经会带的那一段拿掉(不然重启后带两遍)
fn without_known(mut args: Vec<String>, known: &[&[String]]) -> Vec<String> {
    for k in known.iter().filter(|k| !k.is_empty()) {
        if let Some(at) = args.windows(k.len()).position(|w| w == *k) {
            args.drain(at..at + k.len());
        }
    }
    args
}

/// 不算人起的名字:tmux 自己编的号(`0`、`1` …)、agent-deck 编的(`agentdeck_xxx_3f2a`;它那边的登记已经删了才会走到这)
fn default_name(name: &str) -> bool {
    !name.is_empty() && (name.chars().all(|c| c.is_ascii_digit()) || name.starts_with("agentdeck_"))
}

/// 认出来的几样,拼成要登记的那一行。纯函数
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
        // 你自己起的 tmux 会话名留着不动(要跟 agent 自己起的标题走:aca rename <id> --auto)
        (true, None) => (pane.session.clone(), !default_name(&pane.session)),
    };
    let conv = [opts.conv.trim(), live.conv.as_str(), rec.map_or("", |r| r.conv.as_str())].into_iter().find(|c| !c.is_empty());
    if conv.is_none() && tool != "shell" {
        notes.push(
            t!(
                "没认出它现在是哪段对话:重启会是新的一段。知道 id 的话接管时加 --conv <id>",
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
                notes.push(t!("它命令行里的参数没照搬(里面有一段话,分不清哪些是参数);要每次启动都带的,接管时写在 -- 后面", "its command-line arguments weren't copied (they contain a message, so it can't tell which are arguments); for arguments on every start, put them after -- when adopting").into());
            }
            vec![]
        }
    };
    let s = Session {
        // id、顺序在外面定(要查登记表)
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
        return Err(tr!("「{}」已经是 aca 的会话了({})", "“{}” is already an aca session ({})", p.session, p.id));
    }
    // agent 在哪一格:指名的那格;没指名就是头一个认得出 agent 的,都认不出就是第一格
    let found = |p: &Pane| if p.dead { None } else { proc::agent_at(&w.kids, p.pid) };
    let pane = named
        .or_else(|| all.iter().copied().find(|p| found(p).is_some()))
        .or(all.first().copied())
        .ok_or(t!("这个会话里没有窗格", "this session has no panes"))?;
    if pane.dead {
        return Err(tr!("{} 那一格的程序已经退了,没有可以接管的", "the program in pane {} has exited; nothing to adopt", pane.pane));
    }
    let running = found(pane);
    let mut notes = vec![];
    let (tool, given) = match opts.spec.split_once(':') {
        Some((t, a)) => (t.to_string(), Some(a)),
        None if !opts.spec.is_empty() => (opts.spec.clone(), None),
        None => (running.map_or("shell", |r| r.0).to_string(), None),
    };
    if opts.spec.is_empty() && running.is_none() {
        notes.push(t!("没认出里面跑的是哪家 agent,登记成了 shell;是别的话放手(aca release)再接管,加 --as 工具[:账号]", "couldn't tell which agent runs inside, so it's registered as a shell; if it's something else, let go (aca release) and adopt again with --as TOOL[:ACCOUNT]").into());
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
        // 指名了账号:照 `aca new` 的认法(登记的名字,或者一个目录)
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
    // 对话是自己给的 / agent-deck 记的那个:它的记录得另找(Claude 没有记录接不上)
    if s.conv != live.conv {
        live.transcript = import::locate(cfg, agent.name(), &s.conv).map(|(_, t)| t.to_string_lossy().into_owned()).unwrap_or_default();
    }
    // id:agent-deck 起的沿用它那边的。登记表里已经有这条(整批搬过来、还停着):接到它上面,那一行不改
    let old = record.as_ref().and_then(|r| registered(store, &r.s.id));
    // 那条登记已经连着别的 tmux 会话(不该有:同一条被接了两次):这个另起一个 id
    let taken = old.as_ref().is_some_and(|o| w.panes.iter().any(|p| p.id == o.id));
    let known = old.is_some() && !taken;
    // 这个进程本来就是 aca 起的(放手之后又接回来):它带着的 hook 还在往原来那个 id 上报,沿用它,状态不用等重启
    let hooked = var("ACA_SESSION_ID").filter(|id| !id.is_empty() && !id.contains('/') && registered(store, id).is_none());
    let from_deck = record.as_ref().filter(|_| old.is_none()).map(|r| r.s.id.clone());
    match old {
        // 那一行是照 agent-deck 的登记搬的,不知道它带着什么参数:对话、参数以进程上现在的为准,别的不改
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
            "agent-deck 那边还登记着它,接管后在那边会显示成停了:别在那边再启动(会对同一段对话再起一个),用不着了就删掉:agent-deck -p {} remove {}", "agent-deck still has it registered and will show it as stopped: don't start it there again (that would start a second copy of the same conversation); once you don't need it, remove it: agent-deck -p {} remove {}",
            r.profile, r.s.id
        ));
    }
    if tool != "shell" && hooked.is_none() {
        notes.push(
            match import::agent_deck_hook(&s.id) {
                Some(_) => t!("重启之前「跑完没看」借 agent-deck 的 hook 记的;重启一次(aca restart,接着原来的对话)就带上 aca 自己的 hook 了", "until a restart, “finished, unread” comes from agent-deck's hooks; restart once (aca restart, the conversation continues) and it carries aca's own hooks"),
                None => t!("重启之前状态只靠读屏幕(没有「跑完没看」);重启一次(aca restart,接着原来的对话)就带上 hook 了", "until a restart, state comes only from reading the screen (no “finished, unread”); restart once (aca restart, the conversation continues) and it carries the hooks"),
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
        // 窗格标题上写着的优先:接管之后列表也是照它跟的(`status::follow_titles`)
        auto_title: agent.live_title(&runtime::Run::default(), &pane.title).unwrap_or(live.title),
        deck: record.map(|r| (r.profile, r.s.id)),
        notes,
        s,
    })
}

/// 看一个会话接管过来会是什么样。只看,不动任何东西。
/// `target`:tmux 会话名(或者名字里的一段)、`$会话号`、`%窗格号`(agent 不在第一格时指名是哪一格)
pub fn plan(store: &Store, cfg: &Config, target: &str, opts: &Opts) -> Res<Plan> {
    plan_in(store, cfg, &World::read(), target, opts)
}

/// 能直接接管的:没登记过、第一格里跑着认得出的 agent 的 tmux 会话。
/// 第二个返回值:别的没登记的 tmux 会话(没认出 agent,或者 agent 不在第一格)—— 要接管得指名
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

/// -g .:按目录定的组到真登记时才定(一次接好几个、目录名一样的,各自编号)
fn by_dir(store: &Store, s: &mut Session) -> Res<()> {
    if s.group == session::BY_DIR {
        s.group = store.group_claim(&s.dir)?.0;
        s.order = store.next_order(&s.group);
    }
    Ok(())
}

/// 照 [`plan`] 认出来的接管:在 tmux 会话上记身份、登记、会话名改成标题。返回登记好的那一行和现在的 tmux 会话名
pub fn apply(store: &Store, p: &Plan) -> Res<(Session, String)> {
    let mut s = p.s.clone();
    let target = &p.pane;
    // 先记在 tmux 上:那个会话要是刚好没了,这一步就失败,登记表不用动
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
    tmux::run(&a).map_err(|e| tr!("tmux 上记不了:{e}", "can't mark it in tmux: {e}"))?;
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
        // 以前停过的记号不留:它现在是在跑的
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
        // 名字整个对上的优先于「里面有这一段」的
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
        // 没登记过的目录:直接记目录
        assert_eq!(account_of(&cfg, "claude", Some("/h/.new"), Some(&plain)), "/h/.new");
        // 没换目录:默认的那个,不带账号
        assert_eq!(account_of(&cfg, "claude", None, Some(&plain)), "");
        // 只带环境变量的账号:进程上都对得上才算
        let br = im(&["claude"], &[("CLAUDE_CODE_USE_BEDROCK", "1"), ("AWS_PROFILE", "work")]);
        assert_eq!(account_of(&cfg, "claude", None, Some(&br)), "bedrock");
        assert_eq!(account_of(&cfg, "claude", None, Some(&im(&["claude"], &[("AWS_PROFILE", "work")]))), "");
        // 同一个目录两个账号:带着那个账号的参数的是它,没带的是只换目录的那个
        let ll = im(&["codex", "--yolo", "--profile", "labs"], &[]);
        assert_eq!(account_of(&cfg, "codex", Some("/h/.ll"), Some(&ll)), "labs");
        assert_eq!(account_of(&cfg, "codex", Some("/h/.ll"), Some(&im(&["codex", "--yolo"], &[]))), "labs2");
        // 读不到进程(不是自己的):只换目录的账号照样认得出
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
        // agent-deck 起的:标题、分组、父会话、创建时间照那边的;对话以进程上现在的为准;账号已经会带的参数不重复记
        let (s, notes) = draft(&p, "codex", "labs".into(), &live, &known, true, Some(&rec), &Opts::default());
        assert_eq!(
            (s.title.as_str(), s.title_locked, s.group.as_str(), s.parent.as_str(), s.created),
            ("那边的标题", false, "work", "ad-0", 7)
        );
        assert_eq!((s.conv.as_str(), s.dir.as_str(), s.args), ("c-live", "/p/app", words(&["--yolo", "--search"])));
        assert!(notes.is_empty(), "{notes:?}");
        // 进程上认不出对话:用那边记的
        let quiet = Live { args: Some(vec![]), ..Default::default() };
        assert_eq!(draft(&p, "codex", String::new(), &quiet, &known, false, Some(&rec), &Opts::default()).0.conv, "c-deck");
        // 自己给的都优先;给了标题就锁住
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

        // 不是 agent-deck 的:你起的 tmux 会话名就是标题,锁着;tmux 自己编的号不算
        let mine = pane("$2", "修登录", "%2", 1);
        let (s, notes) = draft(&mine, "claude", "sub".into(), &Live::default(), &known, true, None, &Opts::default());
        assert_eq!((s.title.as_str(), s.title_locked, s.group.as_str(), s.conv.as_str()), ("修登录", true, "", ""));
        // 没认出对话、参数没敢搬:都说一声
        assert!(notes.iter().any(|n| n.contains("--conv")) && notes.iter().any(|n| n.contains("没照搬")), "{notes:?}");
        for machine in ["0", "agentdeck_iron-dune_92cb"] {
            let (s, _) = draft(&pane("$3", machine, "%3", 1), "claude", String::new(), &quiet, &known, false, None, &Opts::default());
            assert_eq!((s.title.as_str(), s.title_locked), (machine, false));
        }
        // shell 没有对话,不提
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
