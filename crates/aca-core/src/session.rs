//! 对会话的操作:新建、启动、停、重启、分叉、改名、挪组、删、发消息。命令行和界面都调这里。
//!
//! 一个会话 = 登记表里一行 + tmux 里一个会话(会话名就是标题)。agent 在记着 `@aca_agent` 的那一格里,
//! 那个窗口开着 remain-on-exit:agent 退出后窗格连最后的画面留着,**停和重启都在原地**,
//! 你在这个 tmux 会话里另开的窗口不受影响。tmux 里整个没了(重启过电脑)也没事,登记还在,`start` 重新建、接上对话。

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::agent::{self, How, Launch};
use crate::config::Config;
use crate::event;
use crate::paths::{self, Res};
use crate::proc;
use crate::runtime::{self, Run};
use crate::status::{self, State};
use crate::store::{Session, Store};
use crate::tmux::{self, Pane};
use crate::{t, tr};

#[derive(Clone, Debug, Default)]
pub struct New {
    /// 工具名,或者「工具:账号」
    pub tool: String,
    pub dir: String,
    pub title: String,
    pub group: String,
    /// 父会话;空 = 在某个会话里建的就挂在它下面(看 `ACA_SESSION_ID`),`-` = 不挂
    pub parent: String,
    pub args: Vec<String>,
    /// 只登记,不启动
    pub hold: bool,
    /// 哪些事通知 parent(见 [`Run::notify`]);空 = 照配置的
    pub notify: String,
    /// 起在这个分支的 git worktree 里(见 [`crate::worktree`]);空 = 就在 `dir`
    pub worktree: String,
}

pub(crate) fn fresh_id(store: &Store) -> String {
    loop {
        let id = paths::short_id();
        if store.get(&id).is_err() {
            return id;
        }
    }
}

/// 分组写成这个(`-g .`):按会话的目录定组 —— 哪个组的默认目录就是它就进哪个,不然是以目录名命名的组,
/// 重名(记着别的目录)的往后编号:aca-2、aca-3…(见 `Store::dir_group`)
pub const BY_DIR: &str = ".";

pub fn create(store: &Store, cfg: &Config, n: New) -> Res<Session> {
    make(store, cfg, n, None).map(|(s, _)| s)
}

/// 建会话并说开场那句话(`aca new --say`)。第二个 = 那句话没说成的原因,会话照样建好了。
/// 能当启动参数带的那几家(Claude、Codex,见 [`agent::Agent::opening`])带进去就算说了:它自己拿着过启动时
/// 那几道关,起来了自己发,这里不等;别家等它起来再打字进去([`send_fresh`],最多等一分钟)
pub fn create_saying(store: &Store, cfg: &Config, n: New, text: &str) -> Res<(Session, Option<String>)> {
    if text.len() > agent::OPENING_MAX {
        return Err(tr!(
            "开场消息过长({} 字节,上限 {})",
            "the opening message is too long ({} bytes, at most {})",
            text.len(),
            agent::OPENING_MAX
        ));
    }
    let hold = n.hold;
    let (s, said) = make(store, cfg, n, Some(text))?;
    let unsaid = match (hold, said) {
        (true, _) => Some(t!("仅登记,未启动", "only registered, not started; not carried").to_string()),
        (false, true) => None,
        (false, false) => send_fresh(store, cfg, &s.id, text, Duration::from_secs(60)).err(),
    };
    Ok((s, unsaid))
}

/// 第二个 = 开场那句话当启动参数带上了
fn make(store: &Store, cfg: &Config, n: New, say: Option<&str>) -> Res<(Session, bool)> {
    let agent::Pick { tool, account, agent, .. } = agent::pick(cfg, &n.tool)?;
    let dir = Path::new(&paths::expand(&n.dir)).canonicalize().map_err(|e| format!("{}:{e}", n.dir))?;
    if !dir.is_dir() {
        return Err(tr!("{} 不是目录", "{} is not a directory", dir.display()));
    }
    let dir = if n.worktree.is_empty() { dir } else { crate::worktree::make(&dir, n.worktree.trim())? };
    // 在某个会话里建的:挂成它的子会话,跟它进同一个分组
    let parent = match n.parent.as_str() {
        "" => std::env::var("ACA_SESSION_ID").ok().and_then(|p| store.get(&p).ok()),
        // 界面里新建的:就算界面开在某个会话的窗格里,也不算它的子会话
        "-" => None,
        p => Some(store.get(p)?),
    };
    let group = match n.group.trim_matches('/') {
        _ if n.group.is_empty() => parent.as_ref().map(|p| p.group.clone()).unwrap_or_default(),
        BY_DIR => store.group_claim(&dir.to_string_lossy())?.0,
        g => g.to_string(),
    };
    let title = n.title.trim();
    let s = Session {
        id: fresh_id(store),
        title: if title.is_empty() {
            dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "aca".into())
        } else {
            title.to_string()
        },
        title_locked: !title.is_empty(),
        order: store.next_order(&group),
        group,
        tool,
        account,
        dir: dir.to_string_lossy().into_owned(),
        args: n.args,
        conv: agent.new_conv(),
        parent: parent.map(|p| p.id).unwrap_or_default(),
        fork_of: String::new(),
        created: paths::now(),
        archived: 0,
    };
    store.insert(&s)?;
    runtime::update(&s.id, |r| {
        r.locked = s.title_locked;
        r.notify = n.notify.clone();
    });
    event::emit(
        "session.created",
        &s.id,
        json!({ "title": s.title, "tool": s.tool, "account": s.account, "dir": s.dir, "group": s.group, "parent": s.parent }),
    );
    if n.hold {
        return Ok((s, false));
    }
    match launch_saying(cfg, &s, None, How::Fresh, say) {
        Ok(said) => Ok((s, said)),
        Err(e) => {
            // 根本没起来(程序不在 PATH 里这类):登记撤回,刚建的干净 worktree 一起收,列表里不留一条起不来的
            if tmux::find(&s.id).is_none() {
                let _ = remove(store, &s.id);
            }
            Err(e)
        }
    }
}

/// 接着原来的对话起,还是起一段新的(各家自己判断);第二个是现在这段对话的 id
fn how_to_resume(cfg: &Config, s: &Session, r: &Run) -> (How, String) {
    let conv = if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() };
    let how = agent::of(cfg, s).map_or(How::Fresh, |(agent, _)| agent.resume(&conv, r));
    (how, conv)
}

/// 在 target 那一格起 agent。账号的环境用 `respawn-pane -e` 只交给这一个进程,不进 tmux 会话的环境:
/// `new-session -e` 写的是会话的环境,之后在这个会话里开的窗口都会继承,换了账号的也只盖得掉自己写了的变量,
/// env_file 里的 key 这类照样漏过去(tmux 3.7c 实测)
fn respawn(target: &str, dir: &str, l: &Launch, argv: Vec<String>) -> Vec<String> {
    let mut a: Vec<String> = ["respawn-pane", "-k", "-t", target, "-c", dir].iter().map(|x| x.to_string()).collect();
    a.extend(l.env.iter().flat_map(|(k, v)| ["-e".to_string(), format!("{k}={v}")]));
    a.extend(argv);
    a
}

/// 把 agent 起起来:有窗格就在原地(`respawn-pane`),没有就建一个以标题命名的 tmux 会话
fn launch(cfg: &Config, s: &Session, pane: Option<&Pane>, how: How) -> Res<()> {
    launch_saying(cfg, s, pane, how, None).map(drop)
}

/// [`launch`],带着开场那句话。返回它是不是当启动参数带上了(见 [`agent::launch`])
fn launch_saying(cfg: &Config, s: &Session, pane: Option<&Pane>, how: How, say: Option<&str>) -> Res<bool> {
    let l = agent::launch(cfg, s, &how, say)?;
    let argv: Vec<String> = l.argv.iter().map(|a| tmux::arg(a)).collect();
    match pane {
        Some(p) => {
            tmux::run(&respawn(&p.pane, &s.dir, &l, argv))?;
        }
        None => {
            let r = runtime::load(&s.id);
            let mut taken: Vec<String> = tmux::panes().into_iter().map(|p| p.session).collect();
            taken.dedup();
            let name = tmux::unique_name(&tmux::safe_name(&status::title(s, &r)), &taken);
            let target = format!("={name}:");
            // 会话不带 -e 建(它的环境是干净的),先跑一个占位,同一条命令的最后再把这一格换成 agent(见 `respawn`)。
            // 占位自己会退:最后那步没跑成的话,窗格停在「没在跑」,和 agent 一起来就退一样,`start` 能在原地再起
            let mut a: Vec<String> = ["new-session", "-d", "-s", &name, "-n", &name, "-c", &s.dir, "/usr/bin/env", "sleep", "5"]
                .iter()
                .map(|x| x.to_string())
                .collect();
            // 接在同一条命令里设:agent 一起来就退(参数写错)时窗格也已经是留着的,错在哪看得到
            for opt in [
                vec!["set-option", "-t", &target, "@aca_id", &s.id],
                vec!["set-option", "-p", "-t", &target, "@aca_agent", "1"],
                vec!["set-option", "-w", "-t", &target, "remain-on-exit", "on"],
                vec!["set-option", "-w", "-t", &target, "automatic-rename", "off"],
            ] {
                a.push(";".into());
                a.extend(opt.iter().map(|x| x.to_string()));
            }
            a.push(";".into());
            a.extend(respawn(&target, &s.dir, &l, argv));
            tmux::run(&a)?;
            tmux::bind_detach();
        }
    }
    runtime::update(&s.id, |r| {
        r.clear_error();
        r.clear_ask();
        // 门铃等这次起来的 agent 自己接上(Claude 的 SessionStart)
        r.bell = false;
        r.started = paths::now();
        // 记录现在有多长:这次定的只会写在后面(见 schedule::due)
        r.started_file = r.transcript.clone();
        r.started_len = std::fs::metadata(&r.transcript).map_or(0, |m| m.len());
        r.idle_stopped = 0;
        if r.status == "ended" {
            r.status = "idle".into();
        }
    });
    crate::inbox::launched(cfg, s);
    // 头一回进这个目录它会先问「信不信任」:开关开着就替你答
    crate::trust::spawn(cfg, &s.id);
    let how = match how {
        How::Fresh => "fresh",
        How::Resume(_) => "resume",
        How::Fork(_) => "fork",
    };
    event::emit("session.started", &s.id, json!({ "how": how }));
    Ok(l.said)
}

/// 没在跑就起(接着原来的对话);返回做了什么
pub fn start(store: &Store, cfg: &Config, key: &str) -> Res<&'static str> {
    let s = store.get(key)?;
    let pane = tmux::find(&s.id);
    if pane.as_ref().is_some_and(|p| !p.dead) {
        return Ok(t!("已在运行", "already running"));
    }
    let (how, conv) = how_to_resume(cfg, &s, &runtime::load(&s.id));
    let said = match how {
        How::Resume(_) => t!("已启动,继续原对话", "started, resuming the conversation"),
        How::Fork(_) => t!(
            "已启动,从原对话重新分叉(分叉后尚未对话)",
            "started, forked again from the original conversation (the fork hadn't talked yet)"
        ),
        How::Fresh => t!("已启动,开始新对话", "started, with a new conversation"),
    };
    launch(cfg, &Session { conv, ..s }, pane.as_ref(), how)?;
    Ok(said)
}

/// 等 `gone` 成真,最多 3 秒;到时间还没成 = false
fn wait_gone(gone: impl Fn() -> bool) -> bool {
    let t = Instant::now();
    while !gone() {
        if t.elapsed() > Duration::from_secs(3) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// 让 agent 自己退出(先客气地,3 秒不走再硬的)。窗格留着
fn halt(p: &Pane) {
    if p.dead || p.pid == 0 {
        return;
    }
    let kill = |pid: u32, sig: libc::c_int| unsafe { libc::kill(pid as i32, sig) };
    let dead = || !tmux::run(&["display-message", "-p", "-t", &p.pane, "#{pane_dead}"]).is_ok_and(|o| o.trim() == "0");
    // 接管来的会话,agent 不是窗格的根进程(外面包着一层 shell,或者是在 shell 里手动起的):先让 agent 自己退干净,
    // 再收外面那层。只杀外面那层的话,窗格是空出来了,agent 可能还在收尾,紧接着在原地再起的就和它撞在同一段对话上
    let inner = proc::agent_at(&proc::fg_kids(), p.pid).map(|a| a.1).filter(|pid| *pid != p.pid);
    if let Some(pid) = inner {
        kill(pid, libc::SIGTERM);
        if !wait_gone(|| !proc::alive(pid) || dead()) {
            kill(pid, libc::SIGKILL);
        }
        if dead() {
            return;
        }
    }
    // 交互式的 shell 不理 SIGTERM,挂断(终端没了)它才走
    kill(p.pid, if inner.is_some() { libc::SIGHUP } else { libc::SIGTERM });
    if !wait_gone(dead) {
        kill(p.pid, libc::SIGKILL);
    }
}

/// 对话 id 以 hook 报的为准,记回登记表(界面、导出看的是表)
fn sync_conv(store: &Store, s: &Session) {
    let r = runtime::load(&s.id);
    if !r.conv.is_empty() && r.conv != s.conv {
        let _ = store.set_conv(&s.id, &r.conv);
    }
}

pub fn stop(store: &Store, key: &str) -> Res<()> {
    let s = store.get(key)?;
    let p = tmux::find(&s.id).ok_or(t!("未运行", "not running"))?;
    halt(&p);
    sync_conv(store, &s);
    runtime::update(&s.id, |r| {
        r.status = "ended".into();
        r.idle_stopped = 0;
    });
    event::emit("session.stopped", &s.id, json!({}));
    Ok(())
}

/// 闲着被停掉的一个
#[derive(Clone, Debug, PartialEq)]
pub struct Idled {
    pub id: String,
    pub title: String,
    /// 停之前空闲了几秒
    pub idle: u64,
}

/// 空闲了 `after` 秒以上的会话停掉(`[idle] stop_after`:界面开着时定时跑;`aca stop --idle`)。和 [`stop`] 一样 agent 退出、
/// 窗格和登记留着,另外记下是闲着被停的(列表里画 ☾,人停的画 ■),再起照常接着原来的对话。
/// 宁可不停,不能停在半路上:跑完没看的、在等你的、出错的、有人连着的、有后台任务的、派出去的会话还开着的(停了它就收不到
/// 它们的通知)、正在压缩对话的、再起接不上原来对话的(shell、接不上对话的工具、还没说过话的)都不停;停之前把它自己从头再看一遍(见 [`idle_for`])。
/// `spare`:调用的一方另外要留着的
pub fn stop_idle(store: &Store, cfg: &Config, after: u64, spare: &dyn Fn(&str) -> bool) -> Res<Vec<Idled>> {
    let rows = status::collect(store, cfg, true)?;
    let mut out = vec![];
    for row in candidates(&rows, paths::now(), after).into_iter().filter(|r| !spare(&r.id)) {
        let Ok(s) = store.get(&row.id) else { continue };
        let Some(idle) = idle_for(cfg, &s, after) else { continue };
        if how_to_resume(cfg, &s, &runtime::load(&s.id)).0 == How::Fresh {
            continue;
        }
        let Some(p) = tmux::find(&s.id).filter(|p| !p.dead) else { continue };
        halt(&p);
        sync_conv(store, &s);
        runtime::update(&s.id, |r| {
            r.status = "ended".into();
            r.idle_stopped = paths::now();
        });
        event::emit("session.stopped", &s.id, json!({ "idle": idle }));
        out.push(Idled { id: s.id.clone(), title: row.title.clone(), idle });
    }
    Ok(out)
}

/// 列表里看着能停的(还要过 [`idle_for`]):空闲、没人连着、没有后台任务、派出去的会话都没开着,
/// 有人动过 / hook 报过事是 `after` 秒以前(真正的「多久没动静」还要算上屏幕输出、对话记录,在 [`idle_for`] 里)
fn candidates(rows: &[status::Row], now: u64, after: u64) -> Vec<&status::Row> {
    let open_kids: HashSet<&str> =
        rows.iter().filter(|r| !matches!(r.state, State::Exited | State::Stopped)).map(|r| r.parent.as_str()).collect();
    rows.iter().filter(|r| quiet(r) && now.saturating_sub(r.active) >= after && !open_kids.contains(r.id.as_str())).collect()
}

/// 读了屏幕的状态是空闲(没在跑、没在问、没出错),没人连着,没有后台任务
fn quiet(r: &status::Row) -> bool {
    r.state == State::Idle && r.attached == 0 && r.bg == 0
}

/// 它空闲了几秒;不到 `after`,或者有一样说不准它真闲着,就是 None。停之前现读它自己,几样都要对:
/// - 读了屏幕的状态是空闲([`quiet`])
/// - hook 也说这一轮跑完了。只报了开头(在跑)、在等确认的都不算 —— 屏幕万一认错了,也不会停在半路上。
///   代价:按 Esc 打断的 Claude 不报事件,一直算在跑,要等它下一轮跑完才会被停
/// - 认得出输入框,里面没有你没发的字
/// - 最后一次有动静(见 [`stirred`])是 `after` 秒以前
/// - 它没给自己定着还没到点的(Claude 的 CronCreate、`/loop` 的下一次,见 [`crate::schedule`]):那些只在它进程里,停了就没了
/// - 没在压缩对话([`compacting`])。Claude 闲着快一小时会自己压一次(提示缓存过期之前),那时 hook 状态还是「跑完」
fn idle_for(cfg: &Config, s: &Session, after: u64) -> Option<u64> {
    let p = tmux::find(&s.id).filter(|p| !p.dead)?;
    let r = runtime::load(&s.id);
    if !matches!(r.status.as_str(), "" | "idle" | "done") || compacting(&r, paths::now()) || !quiet(&status::one(cfg, s, true)) {
        return None;
    }
    let (agent, _) = agent::of(cfg, s).ok()?;
    if let Some(m) = agent.prompt_mark()
        && crate::inbox::draft(m, &tmux::capture_ansi(&p.pane)) != Some(false)
    {
        return None;
    }
    let now = paths::now();
    let idle = now.saturating_sub(stirred(&p, &r, s.created, now));
    (idle >= after && !crate::schedule::pending(&r, now)).then_some(idle)
}

/// PreCompact 之后一直没等到压完的(压缩失败、进程没了),过了这么久就不再当它在压。真的压一次多是一到四分钟
const COMPACT_LONGEST: u64 = 30 * 60;

/// 在压缩对话:报了开始(PreCompact),还没报压完,开始不到 [`COMPACT_LONGEST`]
fn compacting(r: &Run, now: u64) -> bool {
    r.compacting != 0 && now.saturating_sub(r.compacting) < COMPACT_LONGEST
}

/// hook 报过事的会话,屏幕有输出、对话记录写入离现在这么久以内才算动静(见 [`stirred`])
const FRESH: u64 = 60;

/// 最后一次有动静的时刻,几样里最晚的:有人连进去 / 按过键、这次起来、登记、hook 报的这一轮开始 / 跑完([`Run::hook_at`]),
/// 子代理的记录最后写入(Claude;后台的子代理在主线程跑完之后还在写)。屏幕有输出、对话记录写入:hook 一个事都没报过的
/// (shell、不报事的工具)只能看这两样;报过事的只认 [`FRESH`] 以内的 —— 一直在刷的多半在干活(hook 没报的一轮、
/// 读屏幕没认出来的转圈),更早的那一下多半是闲着的 Claude 换了设置、窗口变了大小重画、往记录里补几行,算上它就一直停不下来。
/// 只在 hook 说这一轮跑完了时用(见 [`idle_for`])
fn stirred(p: &Pane, r: &Run, created: u64, now: u64) -> u64 {
    let (main, subs) = written(r);
    let screen = p.win_activity.max(main);
    let screen = if r.hook_at() == 0 || now.saturating_sub(screen) < FRESH { screen } else { 0 };
    [r.hook_at(), screen, p.activity, p.last_attached, r.started, created, subs].into_iter().max().unwrap_or(0)
}

/// 对话记录、它旁边 `subagents/` 里子代理的记录(Claude)各自最后写入的时刻;没有是 0
fn written(r: &Run) -> (u64, u64) {
    if r.transcript.is_empty() {
        return (0, 0);
    }
    let t = Path::new(&r.transcript);
    let at = |p: &Path| {
        p.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
    };
    let subs = std::fs::read_dir(t.with_extension("").join("subagents")).into_iter().flatten().flatten().map(|e| at(&e.path()));
    (at(t), subs.max().unwrap_or(0))
}

/// 在原地重启,接着原来的对话;`fresh` = 换一段新的
pub fn restart(store: &Store, cfg: &Config, key: &str, fresh: bool) -> Res<()> {
    let s = store.get(key)?;
    let pane = tmux::find(&s.id);
    if let Some(p) = &pane {
        halt(p);
    }
    let (how, conv) = how_to_resume(cfg, &s, &runtime::load(&s.id));
    let (how, conv) = if fresh { (How::Fresh, agent::of(cfg, &s)?.0.new_conv()) } else { (how, conv) };
    if fresh {
        store.set_conv(&s.id, &conv)?;
        runtime::update(&s.id, |r| {
            r.conv.clear();
            r.transcript.clear();
        });
    }
    launch(cfg, &Session { conv, ..s }, pane.as_ref(), how)
}

/// 换账号接着跑([`rehome`])做了什么
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rehomed {
    /// 现在的「工具:账号」
    pub to: String,
    /// 拷过去几个文件(对话记录、子代理的记录……);0 = 没有要拷的(还没说过话、两个账号用的是同一个目录)
    pub copied: usize,
    /// 参数上改了什么(Codex 带上原来的模型),给人看的;空 = 没改
    pub note: String,
}

/// `--as` 写的那个换成「工具:账号」:写了冒号的照写的;只写一个词的先当这个会话那个工具的账号(登记过的、写成目录的),
/// 不是的话当工具名(用它默认的账号)
fn rehome_spec(cfg: &Config, s: &Session, to: &str) -> String {
    let kind = agent::of(cfg, s).map(|(a, _)| a.name().to_string()).unwrap_or_default();
    match to.contains(':') || (cfg.account(&kind, to).is_none() && !agent::is_dir(to)) {
        true => to.to_string(),
        false => format!("{}:{to}", s.tool),
    }
}

/// 换账号接着跑:换成同一家的另一个账号(`to` 是「工具:账号」,或者只写账号),在原地重启、接着这段对话。
/// 用在一个账号额度用完了、活还没干完的时候:上下文原样带过去,行、tmux 会话、标题、便签都不动。
/// 对话记录从原账号的配置目录拷到新账号的同一位置(拷哪些见 [`agent::Agent::conv_files`]),原账号那份留着
/// (之前那几轮计费还算它的,见 [`Run::moves`])。只能在同一家里换:两家的对话记录不通,要换家用界面的 `H` 交接。
/// 正在跑的这一轮会被打断(和重启一样);还没说过话的会话只是换个账号重起
pub fn rehome(store: &Store, cfg: &Config, key: &str, to: &str) -> Res<Rehomed> {
    let s = store.get(key)?;
    let (agent, _) = agent::of(cfg, &s)?;
    let p = agent::pick(cfg, &rehome_spec(cfg, &s, to))?;
    if p.agent.name() != agent.name() {
        return Err(tr!(
            "{} 不属于 {}:只能在同一工具的账号之间切换(不同工具的对话记录不互通),跨工具请用界面中的 H 交接",
            "{} isn't a {} tool: accounts can only be switched within one family (their conversations don't mix); use H in the UI to hand off",
            p.tool,
            agent.name()
        ));
    }
    let label = |tool: &str, account: &str| if account.is_empty() { tool.to_string() } else { format!("{tool}:{account}") };
    let target = label(&p.tool, &p.account);
    if p.tool == s.tool && p.account == s.account {
        return Err(tr!("已是 {target}", "it's already on {target}"));
    }
    let (from, dest) = (agent::home(cfg, &s.tool, &s.account)?, agent::home(cfg, &p.tool, &p.account)?);
    let r = runtime::load(&s.id);
    let (how, conv) = how_to_resume(cfg, &s, &r);
    let carry = match &how {
        How::Resume(c) | How::Fork(c) => c.clone(),
        How::Fresh => String::new(),
    };
    // 要搬的:两个账号的配置目录不一样,这段对话也有记录
    let moving = match (&from, &dest) {
        (Some(a), Some(b)) if !carry.is_empty() => (!same_dir(a, b)).then(|| (a.clone(), b.clone())),
        _ => None,
    };
    let files = match &moving {
        Some((a, _)) => {
            let f = agent.conv_files(a, &carry);
            if f.is_empty() {
                return Err(tr!(
                    "{} 中找不到对话 {carry} 的记录,无法继续",
                    "can't find the record of conversation {carry} under {}, so it can't move",
                    a.display()
                ));
            }
            f
        }
        None => vec![],
    };
    let fixed: Vec<String> = p.def.args.iter().chain(&agent::account(cfg, &*p.agent, &p.account)?.args).cloned().collect();
    let (args, note) = match &moving {
        Some((a, b)) => agent.rehome_args(&s.args, &a.join(&files[0]), a, b, &fixed),
        None => (s.args.clone(), String::new()),
    };
    // 先停(对话记录不再变),再拷;拷不成就停在这,登记还是原来的账号,⏎ 照原样起
    let pane = tmux::find(&s.id);
    if let Some(x) = &pane {
        halt(x);
    }
    let mut copied = 0;
    if let Some((a, b)) = &moving {
        for rel in &files {
            copied += copy_over(&a.join(rel), &b.join(rel))
                .map_err(|e| tr!("对话记录复制失败({}):{e}", "couldn't copy the conversation ({}): {e}", rel.display()))?;
        }
    }
    store.set_account(&s.id, &p.tool, &p.account)?;
    if args != s.args {
        store.set_args(&s.id, &args)?;
    }
    if let Some((a, b)) = &moving {
        let move_ = runtime::Move { at: paths::now(), conv: carry.clone(), from: a.display().to_string(), to: b.display().to_string() };
        runtime::update(&s.id, |r| {
            // 记录换成新账号那份(内容一样):hook 报上来之前,界面、交接读的也是它
            if let Ok(rest) = Path::new(&r.transcript).strip_prefix(a) {
                r.transcript = b.join(rest).display().to_string();
            }
            r.moves.push(move_);
        });
    }
    event::emit("session.rehomed", &s.id, json!({ "tool": p.tool, "account": p.account, "from": label(&s.tool, &s.account) }));
    launch(cfg, &Session { tool: p.tool.clone(), account: p.account.clone(), args, conv, ..s }, pane.as_ref(), how)?;
    Ok(Rehomed { to: target, copied, note })
}

/// 两个目录是不是同一个(换了写法、经过链接的也算)
fn same_dir(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

/// 把 `from`(文件或目录)拷到 `to`,目录整个往里拷,同名的盖掉;返回拷了几个文件。
/// 对话记录那种只往后加的文件:那边已经有一份、又不是这边的开头一段(不是之前从这边拷过去的),先留一份 `.bak-<时间>` 再盖
fn copy_over(from: &Path, to: &Path) -> std::io::Result<usize> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        let mut n = 0;
        for e in std::fs::read_dir(from)?.flatten() {
            n += copy_over(&e.path(), &to.join(e.file_name()))?;
        }
        return Ok(n);
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if to.is_file() && !is_prefix(to, from)? {
        let mut bak = to.as_os_str().to_owned();
        bak.push(format!(".bak-{}", paths::now()));
        std::fs::rename(to, bak)?;
    }
    // 先拷到旁边再换进去:拷到一半不会留下半个文件
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".aca-tmp");
    std::fs::copy(from, &tmp)?;
    std::fs::rename(&tmp, to)?;
    Ok(1)
}

/// `a` 的内容是不是 `b` 的开头一段(一样也算)
fn is_prefix(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let (la, lb) = (std::fs::metadata(a)?.len(), std::fs::metadata(b)?.len());
    if la > lb {
        return Ok(false);
    }
    let (mut fa, mut fb) = (std::io::BufReader::new(std::fs::File::open(a)?), std::io::BufReader::new(std::fs::File::open(b)?).take(la));
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = fa.read(&mut x)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

/// 从这个会话现在的对话分出一个新会话。它是你自己接着聊的另一条路,不是原会话派出去的:不记 `parent`
/// (通知、代答、状态栏都按 `parent` 认 worker),分自谁记在 `fork_of`;和原会话平级,排在它紧后面
pub fn fork(store: &Store, cfg: &Config, key: &str, title: &str) -> Res<Session> {
    let from = store.get(key)?;
    // 原会话是挂在别人下面的:排到最上面那个的后面(列表里一家是挨着的,见界面的 `link_parents`)
    let mut after = from.clone();
    for _ in 0..32 {
        match store.get(&after.parent) {
            Ok(up) if up.id != after.id && up.group == after.group => after = up,
            _ => break,
        }
    }
    let r = runtime::load(&from.id);
    let (agent, _) = agent::of(cfg, &from)?;
    let conv = if r.conv.is_empty() { from.conv.clone() } else { r.conv.clone() };
    if !agent.can_fork() || conv.is_empty() {
        return Err(t!("该会话没有可分叉的对话", "this session has no conversation to fork").into());
    }
    let s = Session {
        id: fresh_id(store),
        title: if title.trim().is_empty() { tr!("{} (分叉)", "{} (fork)", status::title(&from, &r)) } else { title.trim().to_string() },
        title_locked: !title.trim().is_empty(),
        order: store.next_order(&from.group),
        conv: agent.new_conv(),
        parent: String::new(),
        fork_of: from.id.clone(),
        created: paths::now(),
        archived: 0,
        ..from
    };
    store.insert(&s)?;
    // 排不成不碍事:它留在组尾,照样起
    let _ = store.place_after(&s.id, &after.id);
    runtime::update(&s.id, |r| {
        r.locked = s.title_locked;
        r.fork_from = conv.clone();
    });
    event::emit(
        "session.created",
        &s.id,
        json!({
            "title": s.title, "tool": s.tool, "account": s.account, "dir": s.dir, "group": s.group,
            "parent": s.parent, "fork": true, "fork_of": s.fork_of
        }),
    );
    launch(cfg, &s, None, How::Fork(conv))?;
    Ok(s)
}

/// 改名:登记表、tmux 会话名、窗口名一起改;之后不再跟 agent 自己起的标题
pub fn rename(store: &Store, key: &str, title: &str) -> Res<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(t!("名称不能为空", "the name can't be empty").into());
    }
    let s = store.get(key)?;
    store.set_title(&s.id, title, true)?;
    runtime::update(&s.id, |r| r.locked = true);
    let name = tmux::sync_name(&s.id, title).unwrap_or_default();
    event::emit("session.renamed", &s.id, json!({ "title": title, "tmux": name }));
    Ok(name)
}

/// 放开改名的锁:标题重新跟着 agent 自己起的走
pub fn unlock(store: &Store, key: &str) -> Res<()> {
    let s = store.get(key)?;
    store.set_title(&s.id, &s.title, false)?;
    let (_, r) = runtime::update(&s.id, |r| r.locked = false);
    if !r.auto_title.is_empty() {
        tmux::sync_name(&s.id, &r.auto_title);
    }
    Ok(())
}

/// 挪到别的分组(没有就建);写 `.` 是按它的目录定的组(见 [`BY_DIR`])。返回挪进了哪个组
pub fn move_to(store: &Store, key: &str, group: &str) -> Res<String> {
    let s = store.get(key)?;
    let group = match group.trim_matches('/') {
        BY_DIR => store.group_claim(&s.dir)?.0,
        g => g.to_string(),
    };
    store.set_group(&s.id, &group)?;
    event::emit("session.moved", &s.id, json!({ "group": group }));
    Ok(group)
}

fn kill_tmux(s: &Session) {
    if let Some(p) = tmux::find(&s.id) {
        halt(&p);
        let _ = tmux::run(&["kill-session", "-t", &p.sid]);
    }
}

/// 归档:tmux 会话关掉,登记留着(`unarchive` + `start` 回来,对话接得上)
pub fn archive(store: &Store, key: &str, on: bool) -> Res<()> {
    let s = store.get(key)?;
    if on {
        sync_conv(store, &s);
        kill_tmux(&s);
    }
    store.set_archived(&s.id, if on { paths::now() } else { 0 })?;
    if on {
        event::emit("session.archived", &s.id, json!({}));
    } else {
        runtime::update(&s.id, |r| r.restored = paths::now());
    }
    Ok(())
}

/// 返回删掉的那个会话显示的标题
/// 删掉;返回显示的标题,和它起在 aca 开的 worktree 里时那个 worktree 怎么样了(干净的一起删掉,分支留着)
pub fn remove(store: &Store, key: &str) -> Res<(String, Option<String>)> {
    let s = store.get(key)?;
    let title = status::title(&s, &runtime::load(&s.id));
    kill_tmux(&s);
    store.remove(&s.id)?;
    runtime::remove(&s.id);
    crate::inbox::remove(&s.id);
    event::emit("session.removed", &s.id, json!({ "title": title }));
    // 归档着的也算在用:取回来还要在那里接着干
    let others: Vec<String> = store.sessions(true).unwrap_or_default().into_iter().map(|o| o.dir).collect();
    let worktree = crate::worktree::release(Path::new(&s.dir), |root| others.iter().any(|d| Path::new(d).starts_with(root)));
    Ok((title, worktree))
}

/// 放手([`crate::adopt`] 的反面):登记删掉,tmux 会话原样留着、里面的进程照跑,变回一个普通的 tmux 会话。
/// 会话名不改回去。返回 (显示的标题, tmux 会话名;没在跑是空的)
pub fn release(store: &Store, key: &str) -> Res<(String, String)> {
    let s = store.get(key)?;
    let title = status::title(&s, &runtime::load(&s.id));
    let mut name = String::new();
    if let Some(p) = tmux::find(&s.id) {
        // remain-on-exit 是 aca 开的:收回去,不然它退出后那一格会一直留着
        tmux::run(&[
            "set-option",
            "-u",
            "-t",
            &p.sid,
            "@aca_id",
            ";",
            "set-option",
            "-p",
            "-u",
            "-t",
            &p.pane,
            "@aca_agent",
            ";",
            "set-option",
            "-w",
            "-u",
            "-t",
            &p.pane,
            "remain-on-exit",
        ])?;
        name = p.session;
    }
    store.remove(&s.id)?;
    runtime::remove(&s.id);
    event::emit("session.removed", &s.id, json!({ "title": title, "released": true }));
    Ok((title, name))
}

/// 往会话的输入框里发一段话并回车。`wait`:它还在跑 / 在等你时先等它空下来,最多等这么久。
/// 不带 `wait` 碰上它在等确认:不打(回车可能直接选中默认那一项),报错;要替它回答用 `aca answer`
pub fn send(store: &Store, cfg: &Config, key: &str, text: &str, wait: Option<Duration>) -> Res<()> {
    let s = store.get(key)?;
    let t = Instant::now();
    let pane = loop {
        let row = status::one(cfg, &s, true);
        match (row.state, wait) {
            (State::Stopped | State::Exited, _) => return Err(t!("会话未运行", "the session isn't running").into()),
            (State::Running | State::Ask, Some(w)) if t.elapsed() < w => std::thread::sleep(Duration::from_millis(500)),
            (State::Running | State::Ask, Some(_)) => return Err(t!("会话一直未空闲,未发送", "it never went idle; not sent").into()),
            (State::Ask, None) => {
                return Err(tr!(
                    "会话正在等待确认,未发送(输入后回车可能会直接选中默认选项);如需代为回答:aca answer {}",
                    "it's waiting for a confirmation; not sent (Enter might pick the default option); to answer for it: aca answer {}",
                    s.id
                ));
            }
            _ => break row.pane,
        }
    };
    type_in(&pane, text)
}

/// 粘进输入框再回车
fn type_in(pane: &str, text: &str) -> Res<()> {
    tmux::paste(pane, text)?;
    // 回车要隔一会儿:紧跟在括号粘贴后面的回车会被 Claude 当成粘贴的一部分吞掉
    std::thread::sleep(Duration::from_millis(150));
    tmux::run(&["send-keys", "-t", pane, "Enter"]).map(|_| ())
}

/// 往刚起的会话里打字(交接、远程任务;`aca new --say` 只在那一家不能把话当启动参数带时才走这里):
/// 等它把界面画稳(屏幕上有字、隔半秒再看没变)**而且**空着,再粘贴、回车。一共最多等 `wait`。
/// 两样要同时成立:启动时那几道关(信任目录、审 hook)算「在等你」,有人答掉的那一下它不在问了、界面却还没起来,
/// 那时候打进去的字没人收
pub fn send_fresh(store: &Store, cfg: &Config, key: &str, text: &str, wait: Duration) -> Res<()> {
    let s = store.get(key)?;
    let t = Instant::now();
    let mut last: Vec<String> = vec![];
    let pane = loop {
        let p = tmux::find(&s.id).filter(|p| !p.dead).ok_or(t!("会话未运行", "the session isn't running"))?;
        let now = tmux::capture(&p.pane);
        let settled = now.iter().any(|l| !l.trim().is_empty()) && now == last;
        let busy = matches!(status::one(cfg, &s, true).state, State::Running | State::Ask);
        if settled && !busy {
            break p.pane;
        }
        if t.elapsed() > wait {
            return Err(if settled {
                t!("会话一直未空闲,未发送", "it never went idle; not sent").into()
            } else {
                t!("会话一直未启动完成,未发送", "it never came up; not sent").into()
            });
        }
        last = now;
        std::thread::sleep(Duration::from_millis(500));
    };
    type_in(&pane, text)
}

/// 标成看过了
pub fn seen(store: &Store, key: &str) -> Res<()> {
    let s = store.get(key)?;
    runtime::update(&s.id, |r| r.seen = paths::now());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conversation_to_resume_is_the_one_the_hook_last_reported() {
        let cfg = Config::default();
        let cx = Session { tool: "codex".into(), conv: String::new(), ..Default::default() };
        assert_eq!(how_to_resume(&cfg, &cx, &Run { conv: "th".into(), ..Default::default() }), (How::Resume("th".into()), "th".into()));
        assert_eq!(how_to_resume(&cfg, &cx, &Run::default()), (How::Fresh, String::new()));
        // 工具从配置里删了:只能新开
        let gone = Session { tool: "ccx".into(), conv: "c1".into(), ..Default::default() };
        assert_eq!(how_to_resume(&cfg, &gone, &Run::default()), (How::Fresh, "c1".into()));
    }

    #[test]
    fn only_quiet_sessions_nobody_watches_and_nothing_waits_on_are_candidates() {
        let row = |id: &str, state: State, active: u64| status::Row {
            id: id.into(),
            title: id.into(),
            state,
            tool: "claude".into(),
            account: String::new(),
            group: String::new(),
            dir: String::new(),
            parent: String::new(),
            fork_of: String::new(),
            tmux: id.into(),
            pane: String::new(),
            attached: 0,
            conv: String::new(),
            ask: String::new(),
            ran: 0,
            active,
            bg: 0,
            locked: false,
            created: 0,
            order: 0,
            tokens: Default::default(),
            idle_stopped: 0,
        };
        let now = 100_000;
        let mut rows = vec![
            row("old", State::Idle, now - 4000),
            row("new", State::Idle, now - 100),
            row("unread", State::Unread, now - 9000),
            row("asks", State::Ask, now - 9000),
            row("runs", State::Running, now - 9000),
            row("err", State::Error, now - 9000),
            row("off", State::Exited, now - 9000),
            row("watched", State::Idle, now - 9000),
            row("bg", State::Idle, now - 9000),
            // 派了一个还在跑的、一个停了的
            row("boss", State::Idle, now - 9000),
            row("worker", State::Running, now),
            row("boss2", State::Idle, now - 9000),
            row("done", State::Exited, now - 9000),
        ];
        rows[7].attached = 1;
        rows[8].bg = 2;
        rows[10].parent = "boss".into();
        rows[12].parent = "boss2".into();
        let ids = |after| candidates(&rows, now, after).iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(3600), ["old", "boss2"]);
        assert_eq!(ids(60), ["old", "new", "boss2"]);
    }

    #[test]
    fn an_agent_that_reported_counts_from_its_hooks_not_from_an_old_redraw() {
        // 700 开始、900 跑完,960 是闲了一会儿的提醒;之后没人碰过它(连进去 800、按键 850),15000 重画过一下
        let p = Pane { activity: 850, last_attached: 800, win_activity: 15_000, ..Default::default() };
        let done = Run { status: "done".into(), ts: 960, turn: 700, done: 900, started: 100, ..Default::default() };
        assert_eq!(stirred(&p, &done, 500, 20_000), 900);
        // 屏幕刚才还在刷:可能在干活(hook 没报的一轮),先算上
        assert_eq!(stirred(&p, &done, 500, 15_030), 15_000);
        // 有人连进去按过键:算
        assert_eq!(stirred(&Pane { activity: 2000, ..p.clone() }, &done, 500, 20_000), 2000);
        // hook 一个事都没报过(shell、不报事的工具):只能看屏幕,多久以前的都算
        assert_eq!(stirred(&p, &Run { started: 100, ..Default::default() }, 500, 20_000), 15_000);
    }

    #[test]
    fn a_compaction_counts_until_it_reports_done_or_half_an_hour_has_gone() {
        let at = |compacting| Run { compacting, ..Default::default() };
        assert!(!compacting(&at(0), 10_000));
        assert!(compacting(&at(10_000), 10_000));
        assert!(compacting(&at(10_000), 10_000 + 29 * 60));
        // 没等到压完的(压缩失败、进程没了):半小时之后不再算
        assert!(!compacting(&at(10_000), 10_000 + 30 * 60));
    }

    #[test]
    fn a_transcript_counts_only_fresh_once_hooks_report_and_its_subagents_always() {
        let dir = std::env::temp_dir().join(format!("aca-written-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("c1/subagents")).unwrap();
        let t = dir.join("c1.jsonl");
        std::fs::write(&t, "{}\n").unwrap();
        let now = paths::now();
        let transcript = t.to_string_lossy().into_owned();
        let hooked = Run { status: "done".into(), ts: 1000, done: 1000, transcript: transcript.clone(), ..Default::default() };
        let unhooked = Run { transcript, ..Default::default() };
        let p = Pane::default();
        // 闲着的 Claude 往记录里补了几行:报过事的过了一会儿就不算,没报过的一直算
        assert!((now..=now + 1).contains(&written(&hooked).0));
        assert!(stirred(&p, &hooked, 500, now) >= now);
        assert_eq!(stirred(&p, &hooked, 500, now + 120), 1000);
        assert!(stirred(&p, &unhooked, 500, now + 120) >= now);
        // 后台的子代理主线程跑完之后还在写:一直算
        std::fs::write(dir.join("c1/subagents/agent-a1.jsonl"), "{}\n").unwrap();
        assert!(stirred(&p, &hooked, 500, now + 120) >= now);
        assert_eq!(written(&Run::default()), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
