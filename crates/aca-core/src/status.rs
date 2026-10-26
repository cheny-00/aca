//! 列表:把登记表、tmux 里实际在的窗格、hook 写的状态、屏幕上看到的,合成一行一个会话。
//!
//! 一次刷新 = 一条 `tmux list-panes -a` + 读一次库 + 每个会话一个小文件 + (要读屏幕时)一条 tmux 命令把在跑的会话的屏幕
//! 一起抓回来(`tmux::capture_many`)。不读屏幕(`--no-screen`)就只剩前三样,几毫秒。

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use serde::Serialize;

use crate::agent::{self, Agent, Handle};
use crate::config::Config;
use crate::import;
use crate::paths::{self, Res};
use crate::runtime::{self, Run};
use crate::screen::{self, Seen};
use crate::store::{Session, Store};
use crate::tmux::{self, Pane};
use crate::{event, hook};

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// 在跑
    Running,
    /// 在等你(选择框 / 确认框)
    Ask,
    /// 跑完了还没看
    Unread,
    Idle,
    Error,
    /// agent 退出了,窗格还留着(`aca start` 在原地接着对话再起)
    Exited,
    /// 登记着,tmux 里没有(停掉的,或者重启过电脑):`aca start` 重新建
    Stopped,
}

impl State {
    pub fn glyph(self) -> &'static str {
        match self {
            State::Running => "●",
            State::Ask => "◆",
            State::Unread => "◐",
            State::Idle => "○",
            State::Error => "✕",
            State::Exited | State::Stopped => "■",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub state: State,
    pub tool: String,
    pub account: String,
    pub group: String,
    pub dir: String,
    pub parent: String,
    /// 分叉出来的:分自哪个会话(不是 `parent`:没人派它)
    pub fork_of: String,
    /// tmux 会话名(= 标题)和 agent 的窗格;没在跑是空的
    pub tmux: String,
    pub pane: String,
    pub attached: u32,
    pub conv: String,
    /// 在等你时它问的那句
    pub ask: String,
    /// 在跑时已经跑了几秒
    pub ran: u64,
    /// 最近一次有动静的时刻:登记的 agent 不在跑时是 hook 报的(见 [`Run::hook_at`]),别的是窗口最后有输出的时刻
    pub active: u64,
    pub bg: u32,
    pub locked: bool,
    pub created: u64,
    pub order: i64,
    /// 插件挂上来的小段字
    pub tokens: BTreeMap<String, String>,
    /// 闲着被 aca 停掉的时刻(见 [`crate::session::stop_idle`]);别的(在跑、人停的)是 0
    pub idle_stopped: u64,
}

impl Row {
    /// 状态图标:闲着被 aca 停掉的画 ☾,别的照状态
    pub fn glyph(&self) -> &'static str {
        if self.idle_stopped > 0 { IDLE_STOPPED } else { self.state.glyph() }
    }
}

/// 闲着被 aca 停掉的会话的图标(人停的、agent 自己退的是 ■)
pub const IDLE_STOPPED: &str = "☾";

/// 显示的标题:人改过名听人的,否则 agent 自己起的,还没有就用登记时的
pub fn title(s: &Session, r: &Run) -> String {
    if s.title_locked || r.auto_title.is_empty() { s.title.clone() } else { r.auto_title.clone() }
}

/// 窗格标题上写着的标题和记着的不一样的:记下来,tmux 会话名跟过去(和 hook 里做的是同一件事)。有改名的返回 true。
/// 列表每次刷新对一遍,不读文件、不多问 tmux:没带 hook 的会话(接管了还没重启)标题只有这一条路跟,
/// 带了 hook 的也不用等到这一轮跑完
pub fn follow_titles(cfg: &Config, sessions: &[Session], panes: &[Pane]) -> bool {
    let mut renamed = false;
    for s in sessions {
        let Some(p) = tmux::agent_pane(panes, &s.id).filter(|p| !p.dead) else { continue };
        let Some(agent) = agent_of(cfg, s) else { continue };
        let r = runtime::load(&s.id);
        if agent.live_title(&r, &p.title).is_none_or(|t| t == r.auto_title) {
            continue;
        }
        // 到这才拿锁再问一次:hook 可能刚好也在改
        for (name, data) in hook::retitle(&s.id, |r| agent.live_title(r, &p.title)) {
            event::emit(name, &s.id, data);
            renamed = true;
        }
    }
    renamed
}

/// 列表看的运行时状态:aca 的 hook 写的那份([`runtime::load`])。接管来还没重启的进程没带 aca 的 hook,那份是空的;
/// 其中 agent-deck 起的还带着它的 hook、照常在报:借它记的最后一件事,「在跑」「跑完没看」不用等重启
pub fn load(id: &str) -> Run {
    let mut r = runtime::load(id);
    if r.status.is_empty()
        && let Some((event, ts)) = import::agent_deck_hook(id)
    {
        borrow(&mut r, &event, ts);
    }
    r
}

/// agent-deck 的 hook 记的事件 → aca 的状态。它那边跑完、刚起来、等你批准都记成 waiting,分不出来,所以看事件名
fn borrow(r: &mut Run, event: &str, ts: u64) {
    let status = match event {
        "UserPromptSubmit" => {
            r.turn = ts;
            "running"
        }
        "Stop" => {
            r.done = ts;
            "done"
        }
        "PermissionRequest" | "Notification" => "waiting",
        "SessionStart" => "idle",
        _ => return,
    };
    (r.status, r.event, r.ts) = (status.into(), event.into(), ts);
}

/// 状态以屏幕为准(读了的话),hook 补屏幕看不出来的
pub fn merge(pane: Option<&Pane>, r: &Run, seen: Option<&Seen>, now: u64) -> (State, String, u64) {
    let Some(p) = pane else { return (State::Stopped, String::new(), 0) };
    if p.dead {
        return (State::Exited, String::new(), 0);
    }
    let since_turn = if r.status == "running" && r.turn > 0 { now.saturating_sub(r.turn) } else { 0 };
    // 屏幕上看不出来的那几家,当成没读屏幕:hook 说什么是什么
    let seen = seen.filter(|s| **s != Seen::Unsure);
    match seen {
        // 问的那句:hook 报的说得出是哪个工具、哪条命令,比屏幕上那句「Do you want to proceed?」有用
        Some(Seen::Ask(q)) => {
            return (State::Ask, if q.is_empty() || (r.status == "waiting" && !r.ask.is_empty()) { r.ask.clone() } else { q.clone() }, 0);
        }
        // 一轮刚开始那几秒屏幕上还没有计时:用 hook 记的这一轮开始的时刻
        Some(Seen::Running(n)) => return (State::Running, String::new(), if *n > 0 { *n } else { since_turn }),
        _ => {}
    }
    if screen::spinning(&p.title) {
        return (State::Running, String::new(), since_turn);
    }
    let state = match r.status.as_str() {
        // 读了屏幕却没看到框:要么框还没画出来(刚报的),要么已经按 Esc 打断了(打断不发事件)
        "waiting" if seen.is_none() || now.saturating_sub(r.ts) < 5 => State::Ask,
        // 一轮刚报开始、屏幕还没转起来:事件触发的那次重读常常赶在屏幕前面,不信 hook 就要等下一轮 2 秒的重读。
        // 过了这几秒屏幕上还安静,当是按 Esc 打断了。正文还在往外吐的(屏幕上只有那几行在变)不算安静
        "running" if seen.is_none() || now.saturating_sub(r.turn) < 3 || seen == Some(&Seen::Moving) => State::Running,
        "error" => State::Error,
        // 看过:手动标的 / 跑完那一刻正连着(hook 记的)、跑完之后连进去过、跑完之后在里面按过键(一直连着看它跑完再切走的)。
        // 按键和跑完在同一秒的不算:分不出先后,会话刚建时 tmux 也记一次「有人动过」
        "done" if p.attached == 0 && r.done > r.seen.max(p.last_attached) && r.done >= p.activity => State::Unread,
        _ => State::Idle,
    };
    match state {
        State::Ask => (state, r.ask.clone(), 0),
        State::Running => (state, String::new(), since_turn),
        _ => (state, String::new(), 0),
    }
}

fn row(s: &Session, pane: Option<&Pane>, r: &Run, seen: Option<&Seen>, now: u64) -> Row {
    let (state, ask, ran) = merge(pane, r, seen, now);
    Row {
        id: s.id.clone(),
        title: title(s, r),
        state,
        tool: s.tool.clone(),
        account: s.account.clone(),
        group: s.group.clone(),
        dir: s.dir.clone(),
        parent: s.parent.clone(),
        fork_of: s.fork_of.clone(),
        tmux: pane.map(|p| p.session.clone()).unwrap_or_default(),
        pane: pane.map(|p| p.pane.clone()).unwrap_or_default(),
        attached: pane.map_or(0, |p| p.attached),
        conv: if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() },
        ask,
        ran,
        active: match r.hook_at() {
            // 不在跑、还开着的 agent 照 hook 报的(见 [`Run::hook_at`]):闲着的一重画,窗口的活动时间就成了「刚刚」
            at if at > 0 && matches!(state, State::Idle | State::Unread | State::Ask | State::Error) => at,
            _ => pane.map_or(0, |p| p.activity).max(r.ts).max(r.restored).max(s.created),
        },
        bg: r.bg,
        locked: s.title_locked,
        created: s.created,
        order: s.order,
        tokens: r.live_tokens(now),
        idle_stopped: if matches!(state, State::Exited | State::Stopped) { r.idle_stopped } else { 0 },
    }
}

/// 正文的尾巴(见 [`Agent::body`])上一次看是什么样、最后一次变是什么时候(毫秒):会话 id → (指纹, 时刻)。
/// 只记在这个进程里:界面、`aca wait` 这种一直在看的记得住;`aca ls` 只看一次,没得比,当它没在动
static BODIES: LazyLock<Mutex<HashMap<String, (u64, u64)>>> = LazyLock::new(Default::default);
/// 正文最后一次变之后这么久(毫秒)还算在动。按 Esc 打断之后 ● 多留这么久
const MOVING_MS: u64 = 3000;

/// 读屏幕:这一家的读法;读出来安静的,再看正文还在不在动([`Seen::Moving`])。每次看都记下正文的样子
pub fn look(id: &str, agent: &dyn Agent, lines: &[String]) -> Seen {
    let seen = agent.screen(lines);
    let Some(fp) = agent.body(lines) else { return seen };
    let mut bodies = BODIES.lock().unwrap_or_else(|e| e.into_inner());
    let (next, moving) = moved(bodies.get(id).copied(), fp, paths::now_ms());
    bodies.insert(id.to_string(), next);
    if seen == Seen::Quiet && moving { Seen::Moving } else { seen }
}

/// 这次看到的指纹 `fp` 和上一次记下的(指纹, 最后一次变的时刻)比:返回要记下的、这会儿算不算在动。
/// 头一回看(没有上一次)不算在动
fn moved(last: Option<(u64, u64)>, fp: u64, now: u64) -> ((u64, u64), bool) {
    let at = match last {
        Some((was, at)) if was == fp => at,
        Some(_) => now,
        None => 0,
    };
    ((fp, at), at > 0 && now.saturating_sub(at) < MOVING_MS)
}

/// 这个会话是哪一家(屏幕怎么读看它);工具从配置里删了的认不出来,就不读屏幕
fn agent_of(cfg: &Config, s: &Session) -> Option<Handle> {
    agent::of(cfg, s).ok().map(|x| x.0)
}

/// 全部登记着的会话,按分组的先后、组里的先后
pub fn collect(store: &Store, cfg: &Config, read_screen: bool) -> Res<Vec<Row>> {
    let sessions = store.sessions(false)?;
    let mut panes = tmux::panes();
    // 会话名改了的话重新列一遍(行里带着会话名)
    if follow_titles(cfg, &sessions, &panes) {
        panes = tmux::panes();
    }
    let now = paths::now();
    let picked: Vec<Option<&Pane>> = sessions.iter().map(|s| tmux::agent_pane(&panes, &s.id)).collect();
    let agents: Vec<Option<Handle>> = sessions.iter().map(|s| if read_screen { agent_of(cfg, s) } else { None }).collect();
    // 要读屏幕的那些格一条 tmux 命令抓回来:一格起一个 tmux 的话,几十个会话每刷新一次就是几十个进程
    let want: Vec<Option<&Pane>> = agents.iter().zip(&picked).map(|(a, p)| a.as_ref().and_then(|_| p.filter(|p| !p.dead))).collect();
    let ids: Vec<&str> = want.iter().flatten().map(|p| p.pane.as_str()).collect();
    let mut screens = tmux::capture_many(&ids, false).into_iter();
    let seen: Vec<Option<Seen>> = sessions
        .iter()
        .zip(&agents)
        .zip(&want)
        .map(|((s, a), w)| {
            let agent: &dyn Agent = a.as_deref()?;
            w.and_then(|_| screens.next()).map(|out| look(&s.id, agent, &tmux::plain_lines(&out)))
        })
        .collect();
    let rank: HashMap<String, i64> = store.groups()?.into_iter().map(|g| (g.path, g.order)).collect();
    let mut rows: Vec<Row> = sessions
        .iter()
        .zip(&picked)
        .zip(&seen)
        .zip(&agents)
        .map(|(((s, p), sn), a)| row(s, *p, &settled(cfg, s, a.as_deref(), sn.as_ref()), sn.as_ref(), now))
        .collect();
    rows.sort_by(|a, b| {
        let key = |r: &Row| (rank.get(&r.group).copied().unwrap_or(i64::MAX), r.group.clone(), r.order, r.created);
        key(a).cmp(&key(b))
    });
    Ok(rows)
}

/// 只读地看一眼全部会话:不读屏幕、不替没报的那一轮补事件、不跟改名。tmux 状态栏这种一直在问的用(`aca tmux-status`),
/// 不该每问一次就动一下别的
pub fn peek(store: &Store) -> Res<Vec<Row>> {
    let panes = tmux::panes();
    let now = paths::now();
    Ok(store.sessions(false)?.iter().map(|s| row(s, tmux::agent_pane(&panes, &s.id), &load(&s.id), None, now)).collect())
}

/// 一个会话现在的样子(`show`、发消息前等它空下来)
pub fn one(cfg: &Config, s: &Session, read_screen: bool) -> Row {
    let pane = tmux::find(&s.id);
    let agent = agent_of(cfg, s);
    let seen = pane.as_ref().filter(|p| read_screen && !p.dead).and_then(|p| Some(look(&s.id, agent.as_deref()?, &tmux::capture(&p.pane))));
    row(s, pane.as_ref(), &settled(cfg, s, agent.as_deref(), seen.as_ref()), seen.as_ref(), paths::now())
}

/// 运行时状态,先补上它没报的那一轮结束:hook 说在跑、屏幕上却停了,问这一家是不是这一轮已经结束、
/// 又没报 hook(Codex 出错的、Stop hook 没被信任的,见 [`Agent::unreported_end`])。是的话替它补那个事件,
/// 和真的 hook 一样记状态、发 `turn.done` / `turn.failed`、通知派它的那个。
/// 没有常驻进程:有人看(`aca ls`、界面刷新、`aca wait`)才发现
fn settled(cfg: &Config, s: &Session, agent: Option<&dyn Agent>, seen: Option<&Seen>) -> Run {
    let r = load(&s.id);
    if r.status != "running" || seen != Some(&Seen::Quiet) {
        return r;
    }
    match agent.and_then(|a| Some((a, a.unreported_end(&r)?))) {
        Some((a, v)) => {
            hook::feed(cfg, &s.id, a, &v);
            load(&s.id)
        }
        None => r,
    }
}

/// 自己读屏幕、自己拼列表的那一头(界面)用的 [`settled`]:hook 说在跑的会话逐个问一遍,补上了的接下来读状态文件就是新的。
/// `screen` = 这个窗格现在的屏幕(不带颜色的字)
pub fn settle(cfg: &Config, sessions: &[Session], panes: &[Pane], screen: &dyn Fn(&Pane) -> Vec<String>) {
    for s in sessions {
        if runtime::load(&s.id).status != "running" {
            continue;
        }
        let Some(p) = tmux::agent_pane(panes, &s.id).filter(|p| !p.dead) else { continue };
        let Some(agent) = agent_of(cfg, s) else { continue };
        settled(cfg, s, Some(&*agent), Some(&look(&s.id, &*agent, &screen(p))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `aca ls --json` / `aca show --json` 外面那一层是插件接口:每个字段 docs/plugins.md(英文)和 plugins.zh-CN.md「接口约定」里都写了。
    /// Row 加了字段这里编不过 —— 补上,再去文档里写一句
    #[test]
    fn every_field_of_a_listed_session_is_in_the_plugin_contract() {
        let r = Row {
            id: String::new(),
            title: String::new(),
            state: State::Idle,
            tool: String::new(),
            account: String::new(),
            group: String::new(),
            dir: String::new(),
            parent: String::new(),
            fork_of: String::new(),
            tmux: String::new(),
            pane: String::new(),
            attached: 0,
            conv: String::new(),
            ask: String::new(),
            ran: 0,
            active: 0,
            bg: 0,
            locked: false,
            created: 0,
            order: 0,
            tokens: BTreeMap::new(),
            idle_stopped: 0,
        };
        for (file, docs, from, to) in [
            ("plugins.md", include_str!("../../../docs/plugins.md"), "**JSON that aca prints**", "**Names**"),
            ("plugins.zh-CN.md", include_str!("../../../docs/plugins.zh-CN.md"), "**aca 输出的 JSON**", "**命名**"),
        ] {
            let at = docs.find(from).unwrap_or_else(|| panic!("{file} 里没有 {from}"));
            let part = &docs[at..at + docs[at..].find(to).unwrap()];
            for k in serde_json::to_value(&r).unwrap().as_object().unwrap().keys() {
                assert!(part.contains(&format!("`{k}`")), "{file} 没写 aca ls --json 的 {k}");
            }
        }
    }

    fn pane() -> Pane {
        Pane { id: "ab12".into(), agent: true, pane: "%1".into(), ..Default::default() }
    }

    fn run(status: &str) -> Run {
        Run { status: status.into(), ..Default::default() }
    }

    #[test]
    fn the_screen_decides_and_hooks_fill_in_what_it_cannot_show() {
        let p = pane();
        let st = |r: &Run, seen: Option<Seen>| merge(Some(&p), r, seen.as_ref(), 100).0;
        // 屏幕上有框 / 在转:听屏幕的,不管 hook 说什么
        assert_eq!(merge(Some(&p), &run("done"), Some(&Seen::Ask("要删吗?".into())), 100), (State::Ask, "要删吗?".into(), 0));
        let asked = Run { ask: "Bash:rm -rf build".into(), ..run("waiting") };
        assert_eq!(merge(Some(&p), &asked, Some(&Seen::Ask("Do you want to proceed?".into())), 100).1, "Bash:rm -rf build");
        assert_eq!(merge(Some(&p), &run("idle"), Some(&Seen::Running(72)), 100), (State::Running, String::new(), 72));
        // 屏幕上在跑但还没有计时(一轮刚开始):跑了多久从 hook 记的这一轮开始算
        assert_eq!(merge(Some(&p), &Run { turn: 91, ..run("running") }, Some(&Seen::Running(0)), 100).2, 9);
        // 按 Esc 打断不发事件:hook 还说在跑,屏幕上已经安静了
        assert_eq!(st(&run("running"), Some(Seen::Quiet)), State::Idle);
        // 一轮刚报开始,屏幕还没转起来:先信 hook;几秒了还安静的才当是打断了
        assert_eq!(merge(Some(&p), &Run { turn: 99, ..run("running") }, Some(&Seen::Quiet), 100), (State::Running, String::new(), 1));
        assert_eq!(st(&Run { turn: 97, ..run("running") }, Some(Seen::Quiet)), State::Idle);
        // 没读屏幕:信 hook
        assert_eq!(merge(Some(&p), &Run { turn: 40, ..run("running") }, None, 100), (State::Running, String::new(), 60));
        assert_eq!(merge(Some(&p), &Run { ask: "Bash:ls".into(), ..run("waiting") }, None, 100), (State::Ask, "Bash:ls".into(), 0));
        // 刚报的在等你,框还没画出来
        assert_eq!(st(&Run { ts: 98, ..run("waiting") }, Some(Seen::Quiet)), State::Ask);
        assert_eq!(st(&Run { ts: 10, ..run("waiting") }, Some(Seen::Quiet)), State::Idle);
        assert_eq!(st(&run("error"), Some(Seen::Quiet)), State::Error);
        // 屏幕上看不出来的那几家(声明式工具没写 busy):hook 说在跑就是在跑、说在等就是在等
        assert_eq!(st(&run("running"), Some(Seen::Unsure)), State::Running);
        assert_eq!(st(&Run { ts: 10, ..run("waiting") }, Some(Seen::Unsure)), State::Ask);
        assert_eq!(st(&run("idle"), Some(Seen::Unsure)), State::Idle);
        // 窗格标题在转圈也算在跑
        assert_eq!(merge(Some(&Pane { title: "⠋ 修登录".into(), ..pane() }), &run("idle"), Some(&Seen::Quiet), 100).0, State::Running);
        // 正文还在往外吐(屏幕上只有那几行在变):hook 说在跑就是在跑。跑完那一下最后几行也在重画,hook 说跑完了就不算
        assert_eq!(merge(Some(&p), &Run { turn: 40, ..run("running") }, Some(&Seen::Moving), 100), (State::Running, String::new(), 60));
        assert_eq!(st(&Run { done: 99, ..run("done") }, Some(Seen::Moving)), State::Unread);
        assert_eq!(st(&run("idle"), Some(Seen::Moving)), State::Idle);
    }

    #[test]
    fn the_body_counts_as_moving_for_a_few_seconds_after_it_last_changed() {
        // 头一回看:没得比
        assert_eq!(moved(None, 7, 10_000), ((7, 0), false));
        assert_eq!(moved(Some((7, 0)), 7, 12_000), ((7, 0), false));
        // 变了:从这一刻算
        assert_eq!(moved(Some((7, 0)), 8, 14_000), ((8, 14_000), true));
        // 之后没再变(吐完了、按 Esc 打断了):3 秒内还算,过了就不算
        assert_eq!(moved(Some((8, 14_000)), 8, 16_999), ((8, 14_000), true));
        assert_eq!(moved(Some((8, 14_000)), 8, 17_000), ((8, 14_000), false));
    }

    #[test]
    fn looking_again_tells_text_still_coming_out_from_a_quiet_screen() {
        let claude = agent::builtin("claude").unwrap();
        let bar = "─".repeat(40);
        let frame = |text: &str| -> Vec<String> {
            format!("⏺ 两条路的差别在于:\n{text}\n{bar}\n❯ \n{bar}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)")
                .lines()
                .map(String::from)
                .collect()
        };
        let (a, b) = (frame("  - 第一条"), frame("  - 第一条\n  - 第二条"));
        // 头一回看是安静的;再看正文长了,算在动,紧接着再看一眼没变也还算
        assert_eq!(look("look-1", claude, &a), Seen::Quiet);
        assert_eq!(look("look-1", claude, &b), Seen::Moving);
        assert_eq!(look("look-1", claude, &b), Seen::Moving);
        // 各会话分开记
        assert_eq!(look("look-2", claude, &b), Seen::Quiet);
        // 屏幕上自己认得出的照旧
        assert_eq!(look("look-1", claude, &frame("✻ Pondering… (12s)")), Seen::Running(12));
    }

    #[test]
    fn done_is_unread_until_someone_has_looked() {
        let done = Run { done: 50, ..run("done") };
        let st = |p: &Pane, r: &Run| merge(Some(p), r, None, 100).0;
        assert_eq!(st(&pane(), &done), State::Unread);
        // 跑完之后连进去过
        assert_eq!(st(&Pane { last_attached: 60, ..pane() }, &done), State::Idle);
        // 现在正连着
        assert_eq!(st(&Pane { attached: 1, ..pane() }, &done), State::Idle);
        // 跑完那一刻有人连着(hook 记的),或者手动标了已读
        assert_eq!(st(&pane(), &Run { seen: 50, ..done.clone() }), State::Idle);
        // 一直连着看它跑完(连进去在跑完之前),之后在里面按过键才切走
        assert_eq!(st(&Pane { last_attached: 40, activity: 55, ..pane() }, &done), State::Idle);
        // 按键在跑完之前(发了话就切走了)、和跑完同一秒:没看
        assert_eq!(st(&Pane { last_attached: 40, activity: 45, ..pane() }, &done), State::Unread);
        assert_eq!(st(&Pane { activity: 50, ..pane() }, &done), State::Unread);
        // 看过之后又跑完一轮
        assert_eq!(st(&Pane { last_attached: 60, ..pane() }, &Run { done: 70, ..done }), State::Unread);
    }

    #[test]
    fn a_session_without_acas_hook_borrows_what_agent_decks_hook_last_said() {
        let at = |event: &str| {
            let mut r = Run { conv: "c-1".into(), seen: 7, ..Default::default() };
            borrow(&mut r, event, 50);
            r
        };
        let st = |r: &Run| merge(Some(&pane()), r, None, 100).0;
        assert_eq!((at("Stop").status.as_str(), at("Stop").done, st(&at("Stop"))), ("done", 50, State::Unread));
        // 手动标的「看过」(`aca seen`、界面里的 u)记在 aca 自己那份上,照样算
        assert_eq!(st(&Run { seen: 60, ..at("Stop") }), State::Idle);
        assert_eq!(merge(Some(&pane()), &at("UserPromptSubmit"), None, 100), (State::Running, String::new(), 50));
        assert_eq!((st(&at("PermissionRequest")), st(&at("SessionStart"))), (State::Ask, State::Idle));
        // 认不得的事件(SessionEnd、以后新加的)不动:还是只看屏幕;原来记着的对话 id 这些不丢
        assert_eq!((at("SessionEnd").status.as_str(), at("Stop").conv.as_str(), at("Stop").seen), ("", "c-1", 7));
    }

    #[test]
    fn no_pane_is_stopped_and_a_dead_pane_is_exited() {
        assert_eq!(merge(None, &run("running"), None, 1).0, State::Stopped);
        assert_eq!(merge(Some(&Pane { dead: true, ..pane() }), &run("running"), None, 1).0, State::Exited);
    }

    #[test]
    fn a_renamed_session_keeps_the_name_a_person_gave_it() {
        let s = Session { title: "aca".into(), ..Default::default() };
        let r = Run { auto_title: "修登录 bug".into(), ..Default::default() };
        assert_eq!(title(&s, &r), "修登录 bug");
        assert_eq!(title(&s, &Run::default()), "aca");
        assert_eq!(title(&Session { title_locked: true, ..s }, &r), "aca");
    }
}
