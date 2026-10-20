//! 采集:aca 登记的会话(登记表 + hook 写的状态)+ tmux 里实际在的窗格 + 每格的屏幕内容,合成列表的一行行。
//!
//! 登记过的会话,状态由 `aca_core::status::merge` 定(屏幕为准,hook 补屏幕看不出的:跑完没看、出错);
//! 普通 tmux 会话、会话里另开的窗口只看屏幕:
//!   底部出现选择框 / 确认框的提示 → Ask;输入框上方有 "· 动词… (9m 1s · …)" → Running。
//!
//! 认会话不靠 tmux 会话名(它就是标题,会变):`name` 是不变的键(登记的 id / 普通会话的名字),
//! 发 tmux 命令用 `tmux`(会话号 `$3`)和 `pane`(窗格号 `%12`)。

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, OnceLock};
use std::thread;
use std::time::Duration;

use aca_core::config::Config;
use aca_core::proc::{self, Kids};
use aca_core::runtime::Run;
use aca_core::store::{self, Store};
use aca_core::tmux::Pane;
use aca_core::{paths, status};
use aca_core::{t, tr};
use regex::Regex;

use crate::subagent::Subagent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ask,
    Running,
    Unread,
    Idle,
    Error,
    /// 不再产生(以前:里面的 Claude 退了只剩 shell);留着是界面里还按它画
    Exited,
    /// 登记着、tmux 里没有的(重启过电脑、tmux 服务没了、归档回来的)。⏎ 重新启动接着聊
    Stopped,
    /// 程序停了,窗格连最后的画面留着:`D` 停掉的、agent 自己退出的。⏎ 在原地再启动(登记的会话接着原来的对话)
    Dead,
    /// 同 Stopped,但最后一轮是出错收场的:画 ✕
    Crashed,
}

impl State {
    /// tmux 里没有、登记还在:⏎ = `session::start`(重新建 tmux 会话)
    pub fn gone(self) -> bool {
        matches!(self, State::Stopped | State::Crashed)
    }
}

/// 会话里跑的是哪家的 agent。列表里的账号名按它上色
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Agent {
    /// 普通 tmux 会话,或者本来就开成 shell 的
    #[default]
    None,
    Claude,
    Codex,
    Pi,
    Opencode,
    /// 声明式的工具(config.toml 里没写 kind 的 `[tools.<名>]`)
    Other,
}

#[derive(Clone, Debug)]
pub struct Session {
    /// 不变的键:登记的会话是它的 id,普通 tmux 会话是会话名,子窗格是「会话的键 \u{1} 窗格号」。置顶、便签、已读都按它记
    pub name: String,
    /// tmux 的会话号(`$3`),attach、分屏用;登记着、tmux 里没有的是空的
    pub tmux: String,
    pub title: String,
    /// 标题锁着(人手动起的,Claude 改不了;`r` 改名会锁上)
    pub locked: bool,
    pub group: String,
    pub state: State,
    /// Ask 状态下它在问什么(找不到就空)
    pub question: String,
    pub idle_for: u64,
    /// 最后有动静的时刻(epoch 秒):登记的 agent 不在跑时是 hook 报的(见 `Run::hook_at`),别的是窗口最后有输出的时刻
    pub active_at: u64,
    /// Running 状态下已经跑了多少秒(读屏幕得来,0 = 不知道)
    pub ran: u64,
    /// 别处有终端连着(比如桌面);控制模式的客户端不算
    pub attached: bool,
    /// 屏幕内容(已去掉尾部空行,带颜色 / 链接的转义序列),给预览用
    pub screen: Vec<String>,
    /// 置顶了。采集时恒为 false,由界面按置顶文件标上
    pub pinned: bool,
    /// 有便签(`e` 写的,回车进去时填进输入框)。采集时恒为 false,由界面按便签文件标上
    pub noted: bool,
    /// 在哪个工作区(见 spaces.rs)。采集时恒为 1,由界面按工作区文件标上
    pub space: char,
    /// 项目目录(登记的,没有就用窗格当前目录),新建会话开在这里
    pub dir: String,
    /// 登记过的会话是 `REGISTERED`,空 = 普通 tmux 会话
    pub profile: String,
    /// 登记的工具名(claude / codex / 自己起名的)和账号,新建时沿用。
    /// 空 = 用默认的;shell 会话的工具也记成空(从它新建时起默认工具,不是再开个 shell)
    pub tool: String,
    pub account: String,
    /// 实际跑的是哪家、用的哪个账号(见 `identify`),显示用;空 = 不知道
    pub agent: Agent,
    pub who: String,
    /// 跑着的 claude / codex 进程,0 = 没在跑 / 不知道(找它真正干活的目录用,见 `workdir`)
    pub pid: u32,
    /// 登记的会话 id(对它的操作都按这个找),空 = 普通 tmux 会话、会话里另开的窗口
    pub deck_id: String,
    /// `B` 复制的内容:Path / Session
    pub info: String,
    /// 会话的目录是专门给它建的 git worktree:`d` 删除时会连目录一起删,确认时要说(现在不建 worktree,恒为 false)
    pub worktree: bool,
    /// 列表里的固定位置:(分组排第几, 组内 sort_order, 创建时间),全取自登记表。
    /// 普通 tmux 会话分组排第 usize::MAX,沉在最后
    pub slot: (usize, i64, i64),
    /// tmux 窗格 id(%12):预览、全屏阅读、粘便签、进去时落在哪一格。停掉的会话没有
    pub pane: String,
    /// 会话里另开的窗口 / 分出来的窗格(列在会话下面);None = 会话本身
    pub sub: Option<Sub>,
    /// 是不是别的会话派出来的子会话(窗口跟着自己的会话)
    pub up: Up,
    /// 是不是从别的会话分叉出来的(只标在会话那一行上)
    pub fork: Fork,
    /// 它自己派出去的子代理(Claude 的 Agent 工具、Codex 的 spawn_agent):在跑的,加上最后一轮里起的(见 subagent.rs)。
    /// 采集时填在会话那一行上;界面里 `l` 展开才各占一行
    pub subagents: Vec<Subagent>,
    /// 这一行是展开出来的子代理(界面按上面那个拼的;`sub` 记着它挂的会话,pane 是它排第几):没有窗格,⏎ 看它的记录
    pub subagent: Option<Subagent>,
    /// 插件挂在行上的字(`aca meta set`,没过期的):名字 → 值
    pub tokens: Vec<(String, String)>,
    /// 出错停下的(登记的会话):hook 报的原话和类别(transient / account / fatal,见 `lifecycle::error_kind`);没出错是空的
    pub error: String,
    pub error_kind: String,
    /// 闲着被 aca 停掉的时刻(`[idle] stop_after`,见 `session::stop_idle`):画 ☾、写「闲置自动停」,和 D 停的 ■ 分开。别的是 0
    pub idle_stopped: u64,
    /// 它给自己定了、还没到点的(Claude 的 CronCreate、`/loop` 的下一次,见 `aca_core::schedule`):标题后面标 ◷ 和下次几点。
    /// 只在它进程里,所以闲着也不会被自动停。登记的、agent 还开着的会话才有,开界面的第一屏不读
    pub due: Vec<aca_core::schedule::Due>,
    /// 这一项不是会话,是登记表里的空分组(连子分组都没有会话,见 `blank_row`):列表里只画它的标题,高亮停在标题上
    pub blank: bool,
}

/// 父子会话(在某个会话里 `aca new` 的、`--parent` 指定的)。
/// 几层都挂(子会话再派出去的挂在子会话下面):父会话列着、又在同一个分组才挂在它下面,别的都顶格
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Up {
    /// 不是子会话
    #[default]
    Top,
    /// 挂在父会话下面:父会话的键、标题
    Under(String, String),
    /// 父会话还在,但在别的分组 / 没列出来:顶格
    Apart(String, String),
    /// 父会话删了(删父会话不管子会话)
    Gone,
}

impl Up {
    /// 父会话的键(挂着的、在别处的都算)
    pub fn parent(&self) -> Option<&str> {
        match self {
            Up::Under(p, _) | Up::Apart(p, _) => Some(p),
            Up::Top | Up::Gone => None,
        }
    }
}

/// 分叉出来的会话(`f` / `aca fork`,登记表里的 `fork_of`)。它不是原会话派的:和原会话平级,不挂在它下面,
/// 只在行上、预览里说一句分自谁
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Fork {
    /// 不是分叉出来的
    #[default]
    No,
    /// 分自这个会话:它的标题
    Of(String),
    /// 原会话删了
    Gone,
}

/// 会话里另开的窗口(prefix c)/ 分出来的窗格(prefix % / ")
#[derive(Clone, Debug, PartialEq)]
pub struct Sub {
    /// 所在会话的键、标题
    pub parent: String,
    pub parent_title: String,
    /// tmux 的窗口号、窗口里的第几格;列表里写成 #2 / #2.1(分了屏才带 .N)
    pub win: u32,
    pub pane: u32,
    pub split: bool,
}

impl Sub {
    pub fn loc(&self) -> String {
        if self.split { format!("#{}.{}", self.win, self.pane) } else { format!("#{}", self.win) }
    }
}

impl Session {
    /// 所在的会话的键(子窗格是它挂的那个会话,会话本身就是自己):attach、置顶、排序按它
    pub fn family(&self) -> &str {
        self.sub.as_ref().map_or(&self.name, |s| &s.parent)
    }

    /// 挂在哪个会话下面(父会话的键):子会话和它的窗口都算
    pub fn under(&self) -> Option<&str> {
        match &self.up {
            Up::Under(p, _) => Some(p),
            _ => None,
        }
    }

    /// 挂着的各层父会话,由近到远(父、祖父……)。`hangs` 是 [`hangs`] 给的那张表
    pub fn ancestors<'a>(&'a self, hangs: &'a HashMap<String, String>) -> impl Iterator<Item = &'a str> {
        std::iter::successors(self.under(), move |p| hangs.get(*p).map(String::as_str)).take(DEEP)
    }

    /// "Claude·team" / "Codex·alt";账号就是工具名(退回显示的)时只写哪家
    pub fn agent_label(&self) -> String {
        let name = match self.agent {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Pi => "Pi",
            Agent::Opencode => "OpenCode",
            Agent::None | Agent::Other => "",
        };
        match (name, self.who.as_str()) {
            (n, "") => n.to_string(),
            ("", w) => w.to_string(),
            (n, w) if n.eq_ignore_ascii_case(w) => n.to_string(),
            (n, w) => format!("{n}·{w}"),
        }
    }

    /// 列表最后一列:在跑的显示已跑多久(最近活动恒为「刚刚」没信息量),其余显示多久没动
    pub fn when(&self) -> String {
        if self.state == State::Running && self.ran > 0 { age(self.ran) } else { age(self.idle_for) }
    }
}

static ASK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Enter to select|esc to cancel|Do you want to|Would you like to").unwrap());
static RUNNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\S\s+\S+…\s+\((?:(\d+)h\s*)?(?:(\d+)m\s*)?(?:(\d+)s)?").unwrap());
const SPINNER: &str = "⠁⠂⠄⡀⢀⠠⠐⠈⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

static HOST: LazyLock<String> = LazyLock::new(|| sh(&["hostname", "-s"]).trim().to_string());

/// 所有 tmux 调用都从这里出。设了 ACA_TMUX_SOCKET 就改连 `tmux -L <它>` —— 测试时指到只有假会话的
/// 独立服务,就不可能读到或进入真实会话。
pub fn tmux_cmd() -> Command {
    let mut c = Command::new("tmux");
    if let Some(sock) = std::env::var_os("ACA_TMUX_SOCKET").filter(|s| !s.is_empty()) {
        c.arg("-L").arg(sock);
    }
    c
}

fn tmux(args: &[&str]) -> String {
    tmux_cmd().args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn sh(args: &[&str]) -> String {
    Command::new(args[0]).args(&args[1..]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// attach 进会话,detach 回来时返回(成功 = tmux 正常退出)。
/// 每次 attach 都是一台新的 tmux 客户端,tmux 不知道这个终端刚才在哪,^` / prefix L 回不去:
/// 所以先连上刚离开的那个、马上切到要进的(两个命令跑完才画第一帧,看不到那一个),tmux 就记下了「上一个会话」。
/// stdout 不往终端上打:detach 时 tmux 客户端往 stdout 打一行「[detached (from session …)]」,落在终端主屏上,
/// 进出几次、界面退出后就露出一串;这里读它,知道最后停在哪个会话(里面可能切过)。
/// 画面是 tmux 服务端往 stdin 那个终端 fd 写的,不受影响;出错信息走 stderr,照留。
/// 界面里分三步走(见 [`prepare`]、[`go_in`]、[`remember_left`]),中间让出、接回终端
pub fn attach(name: &str) -> bool {
    let e = prepare(name);
    let Some(left) = go_in(&e) else { return false };
    remember_left(&e, &left);
    true
}

/// 进一个会话之前备好的:要进的、刚离开的那个(还在的话)、这个终端待过的
pub struct Entry {
    name: String,
    back: Option<String>,
    been: Vec<String>,
}

/// 进会话前要做的都不用终端:补 Ctrl+Q、热键的绑定,找出刚离开的那个。界面在让出终端之前调它 ——
/// 这时列表还在屏上,做完 tmux 马上接着画,中间不露出底下的 shell。三件事各是 tmux 命令,同时做
pub fn prepare(name: &str) -> Entry {
    let (been, live) = thread::scope(|s| {
        s.spawn(aca_core::tmux::bind_detach);
        let live = s.spawn(live_sessions);
        (been(), live.join().unwrap_or_default())
    });
    let back = been.iter().find(|s| *s != name && live.contains(*s)).filter(|_| live.contains(name)).cloned();
    Entry { name: name.to_string(), back, been }
}

/// attach 进去,detach 回来时返回 tmux 说最后停在哪个会话(会话名,里面可能切过;没说是空的)。None = 没进去。
/// 有刚离开的那个就先路过它(进去之后 ^` 回得去),路过不算看过(见 `tmux::attach_via`)
pub fn go_in(e: &Entry) -> Option<String> {
    let run = |back: Option<&String>| {
        let mut c = tmux_cmd();
        match back {
            Some(b) => c.args(aca_core::tmux::attach_via(b, &e.name)),
            None => c.args(["attach-session", "-t", &e.name]),
        };
        c.stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::inherit()).output().ok()
    };
    let mut out = run(e.back.as_ref());
    // 刚离开的那个恰好这一下没了:没连上(什么都没打),不带它再来一次
    if e.back.is_some() && out.as_ref().is_none_or(|o| !o.status.success() && o.stdout.is_empty()) {
        out = run(None);
    }
    let o = out.filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&o.stdout);
    Some(text.lines().find_map(|l| l.strip_prefix("[detached (from session ")?.strip_suffix(")]")).unwrap_or_default().to_string())
}

/// 回来之后记下这个终端先后待过的会话(两条 tmux 命令):界面画完回来的第一帧再调
pub fn remember_left(e: &Entry, left: &str) {
    // tmux 说的是会话名(= 标题,会变);记的是会话号
    let left = (!left.is_empty()).then(|| session_id(left)).flatten();
    remember(left.as_deref().unwrap_or(&e.name), &e.name, &e.been);
}

/// 会话名 → 会话号(`$3`)
fn session_id(name: &str) -> Option<String> {
    tmux(&["list-sessions", "-F", "#{session_id}\t#{session_name}"])
        .lines()
        .find_map(|l| l.split_once('\t').filter(|x| x.1 == name).map(|x| x.0.to_string()))
}

/// 这个终端先后待过的会话(会话号)记在 tmux 服务端的哪个选项上(@aca_been_ttys000):
/// ^L 重启、退出再开的界面都接得上;手机、桌面是不同的终端,各记各的。拿不到 tty 就不记
fn been_opt() -> Option<&'static str> {
    static OPT: OnceLock<Option<String>> = OnceLock::new();
    OPT.get_or_init(|| {
        let o = Command::new("tty").stdin(Stdio::inherit()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
        let tty = String::from_utf8_lossy(&o.stdout).trim().trim_start_matches("/dev/").replace(|c: char| !c.is_ascii_alphanumeric(), "_");
        (!tty.is_empty()).then(|| format!("@aca_been_{tty}"))
    })
    .as_deref()
}

/// 这个终端待过的会话,新的在前
fn been() -> Vec<String> {
    let Some(opt) = been_opt() else { return vec![] };
    tmux(&["show-options", "-gqv", opt]).trim_end_matches('\n').split('\t').filter(|s| !s.is_empty()).map(String::from).collect()
}

/// 进了 name、最后从 left 出来:两个排到最前(left 在前),记最近 8 个
fn remember(left: &str, name: &str, been: &[String]) {
    let Some(opt) = been_opt() else { return };
    let v = been_after(left, name, been);
    let _ = tmux_cmd().args(["set-option", "-gq", opt, &v.join("\t")]).status();
}

fn been_after<'a>(left: &'a str, name: &'a str, been: &'a [String]) -> Vec<&'a str> {
    let mut v: Vec<&str> = vec![];
    for s in [left, name].into_iter().chain(been.iter().map(String::as_str)) {
        if !v.contains(&s) {
            v.push(s);
        }
    }
    v.truncate(8);
    v
}

/// tmux 里现在有的会话:会话号和会话名都在里面(一条命令,不用一个个 has-session)
fn live_sessions() -> HashSet<String> {
    tmux(&["list-sessions", "-F", "#{session_id}\t#{session_name}"]).lines().flat_map(|l| l.split('\t')).map(String::from).collect()
}

/// 把文字粘进会话的输入框:括号粘贴(程序开了这个模式的话,Claude / Codex 都开),不按回车,多行也不会被当成发送
/// target:窗格 id(%12),或 `会话号:`(它当前的那格)
pub fn paste_into(target: &str, text: &str) -> Result<(), &'static str> {
    let ok = |c: &mut Command| c.status().is_ok_and(|s| s.success());
    leave_mode(target)?;
    let pasted = ok(tmux_cmd().args(["set-buffer", "-b", "aca-note", "--", text]))
        && ok(tmux_cmd().args(["paste-buffer", "-p", "-d", "-b", "aca-note", "-t", target]));
    if pasted { Ok(()) } else { Err(t!("会话可能刚结束", "the session may have just ended")) }
}

/// 窗格停在 copy-mode 这类模式里(滚轮往上翻过没退出)时,粘贴不带括号、一个个字直接敲给程序(换行成了回车),
/// 之后的回车被 copy-mode 吃掉 —— 所以先退出来(`copy-mode -q` 什么模式都退)。还退不出来就不粘
fn leave_mode(target: &str) -> Result<(), &'static str> {
    let in_mode = || tmux(&["display-message", "-p", "-t", target, "#{pane_in_mode}"]).trim() == "1";
    if in_mode() {
        let _ = tmux_cmd().args(["copy-mode", "-q", "-t", target]).status();
        if in_mode() {
            return Err(t!("窗格停在 tmux 模式里", "the pane is in a tmux mode"));
        }
    }
    Ok(())
}

/// s:粘进输入框再回车,发出去。先认准是哪一格(`会话号:` 换成窗格 id,中途切了窗格也不会发错);
/// 回车要隔一会儿再按:紧跟在括号粘贴后面的回车会被 Claude(Ink)当成粘贴的一部分吞掉
pub fn send_into(target: &str, text: &str) -> Result<(), &'static str> {
    let pane = tmux(&["display-message", "-p", "-t", target, "#{pane_id}"]).trim().to_string();
    if !pane.starts_with('%') {
        return Err(t!("会话可能刚结束", "the session may have just ended"));
    }
    paste_into(&pane, text)?;
    thread::sleep(Duration::from_millis(150));
    let sent = tmux_cmd().args(["send-keys", "-t", &pane, "Enter"]).status().is_ok_and(|s| s.success());
    if sent { Ok(()) } else { Err(t!("会话可能刚结束", "the session may have just ended")) }
}

/// 进会话前先切到那一格(那个窗口、那个窗格),进去就落在它上面
pub fn focus_pane(pane: &str) {
    let _ = tmux_cmd().args(["select-window", "-t", pane, ";", "select-pane", "-t", pane]).stderr(Stdio::null()).status();
}

/// `--client`:界面开在 tmux 弹窗里时,是哪台客户端按的键。弹窗里裸 switch-client 切的是最近有按键的那台,
/// 手机和桌面连着同一个会话时会把另一台切走
static CLIENT: OnceLock<String> = OnceLock::new();

/// 界面开在 tmux 弹窗里:有 TMUX 没有 TMUX_PANE
pub fn in_popup() -> bool {
    std::env::var_os("TMUX").is_some() && std::env::var_os("TMUX_PANE").is_none()
}

pub fn set_client(c: String) {
    let _ = CLIENT.set(c);
}

/// 启动时给的 --client(^L 重启时原样带上)
pub fn client() -> Option<&'static str> {
    CLIENT.get().map(String::as_str)
}

/// 这台客户端的终端类型(kitty 是 xterm-kitty):弹窗里是 --client 那台,窗格里是最近用过这个会话的那台
pub fn client_term() -> String {
    let mut args = vec!["display-message", "-p"];
    if let Some(cl) = CLIENT.get() {
        args.extend(["-c", cl.as_str()]);
    }
    args.push("#{client_termname}");
    tmux(&args).trim().to_string()
}

/// 已经在 tmux 里:把这台客户端切到那个会话(不嵌套 attach)。Ctrl+Q 的绑定调用的人先补(`tmux::bind_detach`)
pub fn switch_to(name: &str) -> std::io::Result<std::process::ExitStatus> {
    let mut c = tmux_cmd();
    c.arg("switch-client");
    if let Some(cl) = CLIENT.get() {
        c.args(["-c", cl]);
    }
    c.args(["-t", name]).status()
}

/// 跟随(见 follow.rs):把这台客户端带到那个会话的那一格,界面不退 —— 弹窗挂在客户端上,切了会话它还在。
/// 一条 tmux 命令做完(先切到那一格,再切会话);没有 `--client` 就不动,裸 switch-client 切的可能是别的终端
pub fn show(session: &str, pane: &str) {
    let Some(cl) = CLIENT.get() else { return };
    let mut c = tmux_cmd();
    if !pane.is_empty() {
        c.args(["select-window", "-t", pane, ";", "select-pane", "-t", pane, ";"]);
    }
    let _ = c.args(["switch-client", "-c", cl, "-t", session]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// 这台客户端现在在哪个会话、哪一格(弹窗一打开就高亮它)
pub fn client_at() -> Option<(String, String)> {
    let cl = CLIENT.get()?;
    tmux(&["list-clients", "-F", "#{client_name}\t#{session_id}\t#{pane_id}"]).lines().find_map(|l| {
        let mut f = l.split('\t');
        (f.next() == Some(cl.as_str())).then(|| (f.next().unwrap_or("").to_string(), f.next().unwrap_or("").to_string()))
    })
}

/// 这台客户端上一个会话(tmux 的 `switch-client -l` 回的那个);刚连上、还没切过就没有
pub fn last_session() -> Option<String> {
    let mut args = vec!["display-message", "-p"];
    if let Some(cl) = CLIENT.get() {
        args.extend(["-c", cl.as_str()]);
    }
    args.push("#{client_last_session}");
    Some(tmux(&args).trim().to_string()).filter(|s| !s.is_empty()).and_then(|n| session_id(&n))
}

/// 在这一格旁边分屏开个 shell(yazi = 放大占满跑 yazi),目录是 dir(side = 左右分,否则上下),返回新那格的 id。
/// 环境同 `open_in`
pub fn split(session: &str, target: &str, dir: &str, side: bool, yazi: bool, env: &[(String, String)]) -> Option<String> {
    let mut c = tmux_cmd();
    c.args(["split-window", if side { "-h" } else { "-v" }, "-t", target, "-P", "-F", "#{pane_id}"]);
    if yazi {
        c.arg("-Z"); // 放大占满;yazi 退出这格就关,放大跟着没了
    }
    open_in(c, session, dir, yazi, env)
}

/// 在这个会话里新开一个窗口跑 shell(同 prefix c,排在最后),目录是 dir,返回新那格的 id。环境同 `open_in`
pub fn new_window(session: &str, dir: &str, env: &[(String, String)]) -> Option<String> {
    let mut c = tmux_cmd();
    c.args(["new-window", "-t", &format!("{session}:"), "-P", "-F", "#{pane_id}"]);
    open_in(c, session, dir, false, env)
}

/// 会话旁边另开的 shell 带的账号:登记的「工具:账号」那一层的环境(和 `aca run` 带的一样),不带会话的身份。
/// aca 起的会话 tmux 环境是空的(账号只带在 agent 那一格上),所以这里照配置现算;不是登记的会话、shell 会话不带
pub fn shell_env(s: &Session) -> Vec<(String, String)> {
    if s.deck_id.is_empty() || s.tool.is_empty() {
        return vec![];
    }
    aca_core::agent::account_env(&aca_core::config::load(), &crate::tools::spec(&s.tool, &s.account)).unwrap_or_default()
}

/// 分屏 / 新窗口的后半:开在 dir,跑 shell(或 yazi),拿新那格的 id。session 是 tmux 的会话号。
/// 账号:env(见 `shell_env`)用 `-e` 只带给新的这一格,不写进会话的环境。
/// 以前建的会话 tmux 环境里还有它的会话 id(ACA_SESSION_ID、CLAUDE_SESSION_ID…),新的一格会继承;
/// 新的一格是你自己的 shell,不算这个会话的 agent(在里面 `aca new` 不挂成它的子会话、远端任务的结果不发给它)→ 这些去掉
fn open_in(mut c: Command, session: &str, dir: &str, yazi: bool, env: &[(String, String)]) -> Option<String> {
    for (k, v) in env {
        c.args(["-e", &format!("{k}={v}")]);
    }
    let env = tmux(&["show-environment", "-t", session]);
    if std::path::Path::new(dir).is_dir() {
        c.args(["-c", dir]);
    }
    let strip = strip_env(&env);
    if yazi {
        c.arg(format!("exec env{strip} yazi"));
    } else if !strip.is_empty() {
        c.arg(format!("exec env{strip} \"${{SHELL:-/bin/sh}}\" -l"));
    }
    let out = c.output().ok().filter(|o| o.status.success())?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    id.starts_with('%').then_some(id)
}

/// 各家的会话 id(ACA_SESSION_ID、CLAUDE_SESSION_ID、老的 AGENTDECK_*):新开的 shell 里不带
pub fn session_id_var(k: &str) -> bool {
    k.starts_with("AGENTDECK_") || k.ends_with("_SESSION_ID")
}

/// 临时终端要带上的账号:登记的账号那一层(`shell_env`)里各家的配置目录(CLAUDE_CONFIG_DIR / CODEX_HOME / PI_CODING_AGENT_DIR);
/// agent 在跑就以它进程里的为准。s 是登记的那个会话(会话里另开的窗口用它所在的会话),pid / agent 是这一行自己的
pub fn account_env(s: Option<&Session>, pid: u32, agent: Agent) -> Vec<(String, String)> {
    let keep: Vec<&str> = aca_core::agent::kinds().filter_map(|k| aca_core::agent::builtin(k)?.home_var()).collect();
    let mut out: Vec<(String, String)> =
        s.map(shell_env).unwrap_or_default().into_iter().filter(|(k, _)| keep.contains(&k.as_str())).collect();
    if pid > 0
        && let Some(k) = home_var(agent)
        && let Some(d) = config_home(pid, agent)
    {
        out.retain(|(x, _)| x != k);
        out.push((k.into(), d));
    }
    out
}

/// 这一家的配置目录交给哪个环境变量;没有这一说(shell、声明式的、opencode)= None
fn home_var(agent: Agent) -> Option<&'static str> {
    let kind = match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Pi => "pi",
        Agent::Opencode => "opencode",
        Agent::None | Agent::Other => return None,
    };
    aca_core::agent::builtin(kind)?.home_var()
}

/// show-environment 里要去掉的:各家的会话 id。拼成 ` -u A -u B`
fn strip_env(env: &str) -> String {
    env.lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .filter(|k| session_id_var(k))
        .filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(|k| format!(" -u {k}"))
        .collect()
}

/// 多久(长写法,预览和提示里用):刚刚 / 40 秒 / 12 分钟 / 2 小时 / 3 天
pub fn dur(sec: u64) -> String {
    match sec {
        0 => t!("刚刚", "just now").into(),
        1..=59 => tr!("{sec} 秒", "{sec}s"),
        60..=3599 => tr!("{} 分钟", "{}m", sec / 60),
        3600..=86399 => tr!("{} 小时", "{}h", sec / 3600),
        _ => tr!("{} 天", "{}d", sec / 86400),
    }
}

/// 多久(短写法,列表最后一列,最宽 3 格):now / 40s / 12m / 2h / 3d
pub fn age(sec: u64) -> String {
    match sec {
        0 => "now".into(),
        1..=59 => format!("{sec}s"),
        60..=3599 => format!("{}m", sec / 60),
        3600..=86399 => format!("{}h", sec / 3600),
        _ => format!("{}d", sec / 86400),
    }
}

/// agent 进程用的配置目录(它环境里的 CLAUDE_CONFIG_DIR / CODEX_HOME / PI_CODING_AGENT_DIR);没设、没有这一说 = None(用默认的)
pub(crate) fn config_home(pid: u32, agent: Agent) -> Option<String> {
    let d = aca_core::proc::env_var(pid, home_var(agent)?)?;
    Some(expand(&d).trim_end_matches('/').to_string())
}

/// 往上数父会话最多数这么多层(同 aca-core 里 `aca answer` 数 parent 的):登记表里 parent 写成了环也数得完
pub const DEEP: usize = 32;

/// 每个会话挂在谁下面:会话的键 → 父会话的键(只有挂着的,见 [`Up::Under`])。一层层往上数用(见 [`Session::ancestors`])
pub fn hangs(v: &[Session]) -> HashMap<String, String> {
    v.iter().filter(|s| s.sub.is_none()).filter_map(|s| Some((s.name.clone(), s.under()?.to_string()))).collect()
}

/// 位置只看分组和组内顺序,状态、活动时间、标题(Claude 会改)都不影响 —— 每 2 秒刷新时行不会跳。
/// 要处理的靠图标、顶栏计数、`i` 和 `@` 找
/// 展开的子代理紧跟在自己的会话下面(由近到远:同一个进程里的排最前),照起的先后;
/// 子窗格在它们后面,按窗口号、格号排;挂着的子会话(连同它的窗口、它自己再派出去的)排在父会话的窗口后面,按自己的组内顺序。
/// 几层都一样:一个会话后面先是它整家,再轮到它的下一个兄弟
pub fn sort_sessions(v: &mut [Session]) {
    let slots: HashMap<String, (usize, i64, i64)> = v.iter().filter(|s| s.sub.is_none()).map(|s| (s.name.clone(), s.slot)).collect();
    let hangs = hangs(v);
    v.sort_by_cached_key(|s| {
        // 从顶格的那个一层层走到自己这一家,每一步是(组内位置, 键);父会话不在列表里的,从它那里起算顶格
        let mut path: Vec<_> = s.ancestors(&hangs).map_while(|p| Some((1, *slots.get(p)?, p.to_string(), (0, 0, 0)))).collect();
        path.reverse();
        path.push((1, s.slot, s.family().to_string(), (0, 0, 0)));
        // 窗口、子代理排在这一家的子会话前面(第一格比会话那一步的小)
        if let Some(x) = &s.sub {
            path.push((0, (0, 0, 0), String::new(), (if s.subagent.is_some() { 1 } else { 2 }, x.win, x.pane)));
        }
        path
    });
}

/// 分组的先后,按树排:父组紧跟着自己的子组,同级按登记表里的 sort_order 再按名字
pub(crate) fn group_ranks<'a>(groups: impl Iterator<Item = &'a str>, order: &HashMap<String, i64>) -> HashMap<String, usize> {
    let key = |g: &str| -> Vec<(i64, String)> {
        let parts: Vec<&str> = g.split('/').collect();
        (1..=parts.len()).map(|i| (order.get(&parts[..i].join("/")).copied().unwrap_or(0), parts[i - 1].to_string())).collect()
    };
    let mut gs: Vec<&str> = groups.collect();
    gs.sort_by_cached_key(|g| key(g));
    gs.dedup();
    gs.into_iter().enumerate().map(|(i, g)| (g.to_string(), i)).collect()
}

/// 读屏幕底部:是否在等你操作(附问题)、是否在跑(附已跑秒数)。
pub fn screen_state(lines: &[String]) -> (Option<State>, String, u64) {
    let nonempty: Vec<&str> = lines.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let tail = &nonempty[nonempty.len().saturating_sub(25)..];
    if tail[tail.len().saturating_sub(12)..].iter().any(|l| ASK.is_match(l)) {
        let q = tail.iter().map(|l| l.trim()).find(|l| l.ends_with('?') || l.ends_with('？')).unwrap_or("").to_string();
        return (Some(State::Ask), q, 0);
    }
    for l in tail.iter().rev() {
        if let Some(c) = RUNNING.captures(l) {
            let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
            return (Some(State::Running), String::new(), n(1) * 3600 + n(2) * 60 + n(3));
        }
    }
    (None, String::new(), 0)
}

/// 去掉末尾的空行(带着转义序列的「空」行也算)
fn trim_tail(out: String) -> Vec<String> {
    let mut lines: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| crate::ansi::strip(l).trim().is_empty()) {
        lines.pop();
    }
    lines
}

/// 全屏阅读用:会话当前窗口的屏幕 + 往上 HISTORY 行历史。-J 把 tmux 自己折的行接回去,由界面按宽度重折
/// target 同 paste_into
pub fn capture_history(target: &str) -> Vec<String> {
    const HISTORY: &str = "-3000";
    trim_tail(tmux(&["capture-pane", "-p", "-e", "-J", "-S", HISTORY, "-t", target]))
}

/// 窗格自己写的标题:Claude 的「✳ 项目标题」去掉状态符;没写过的(tmux 默认给主机名 "myhost.local")算空
fn own_title(title: &str) -> String {
    let mut cs = title.chars();
    let t: String = match (cs.next(), cs.next()) {
        (Some(c0), Some(' ')) if !c0.is_alphanumeric() => cs.collect(),
        _ => title.to_string(),
    };
    if t.split('.').next() == Some(HOST.as_str()) { String::new() } else { t }
}

/// 登记过的会话都算在这一个「profile」下面(aca 没有 profile,这个字段只用来区分「登记过」和「普通 tmux 会话」)
pub const REGISTERED: &str = "default";

/// 登记时没给分组的会话列在这个名字下面
/// 没分组的会话在界面里归到这一区。是内部的名字(界面状态按它记),不随语言变;显示时 ui.rs 的 group_label 翻
pub const UNGROUPED: &str = "未分组";

/// 「B 复制」的内容:目录,再加上 Claude / Codex 自己的对话 id
fn copy_info(path: &str, conv: &str) -> String {
    let mut v = vec![];
    if !path.is_empty() {
        v.push(format!("Path: {path}"));
    }
    if !conv.is_empty() {
        v.push(format!("Session: {conv}"));
    }
    v.join("\n")
}

fn kind_of(name: &str) -> Agent {
    match name {
        "" | "shell" => Agent::None,
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        "pi" => Agent::Pi,
        "opencode" => Agent::Opencode,
        _ => Agent::Other,
    }
}

/// 工具名是哪一家:claude / codex 本身,自己起名的工具看它的 `kind`,声明式的算别家
pub fn agent_of_tool(cfg: &Config, tool: &str) -> Agent {
    match cfg.tool(tool) {
        Some(t) if !t.kind.is_empty() => kind_of(&t.kind),
        Some(_) => Agent::Other,
        None => kind_of(tool),
    }
}

/// 账号在列表里怎么写:登记的名字;直接写目录的是目录名(去掉开头的点)
fn account_name(account: &str) -> String {
    aca_core::agent::account_label(account).trim_start_matches('.').to_string()
}

/// 登记的会话:哪家、哪个账号(没写账号就写工具名)
fn registered(cfg: &Config, s: &store::Session) -> (Agent, String) {
    let agent = agent_of_tool(cfg, &s.tool);
    if agent == Agent::None {
        return (Agent::None, String::new());
    }
    let who = if s.account.is_empty() { s.tool.clone() } else { account_name(&s.account) };
    (agent, who)
}

/// 登记了配置目录的账号:(哪家, 名字, 目录)。先 Claude 再 Codex,各自按名字
pub fn accounts(cfg: &Config) -> Vec<(Agent, String, String)> {
    let mut v: Vec<(Agent, String, String)> = cfg
        .accounts
        .iter()
        .flat_map(|(kind, m)| m.iter().filter(|(_, a)| !a.home.is_empty()).map(move |(n, a)| (kind_of(kind), n.clone(), a.home.clone())))
        .filter(|(a, _, _)| matches!(a, Agent::Claude | Agent::Codex))
        .collect();
    v.sort_by_key(|s| s.0 != Agent::Claude);
    v
}

/// 手动起的 agent(shell 会话里、普通 tmux 会话里):`home` 是它进程环境里的配置目录,对回登记的账号名,对不上就写目录名
fn identify(tool: &str, home: Option<String>, cfg: &Config) -> (Agent, String) {
    let agent = kind_of(tool);
    let who = match home {
        Some(dir) => match cfg.accounts.get(tool).and_then(|m| m.iter().find(|(_, a)| a.home == dir)) {
            Some((name, _)) => name.clone(),
            None => dir.rsplit('/').next().unwrap_or(&dir).trim_start_matches('.').to_string(),
        },
        None => tool.to_string(),
    };
    (agent, who)
}

fn state_of(st: status::State) -> State {
    match st {
        status::State::Running => State::Running,
        status::State::Ask => State::Ask,
        status::State::Unread => State::Unread,
        status::State::Idle => State::Idle,
        status::State::Error => State::Error,
        status::State::Exited => State::Dead,
        status::State::Stopped => State::Stopped,
    }
}

/// 子会话对上父会话(登记表里的 `parent`)。几层都挂:父会话列着、在同一个分组就挂在它下面(它自己是不是子会话都一样),不然顶格。
/// 往上数 [`DEEP`] 层还没到头的 —— parent 写成了环(a 的父是 b、b 的父又是 a)、父会话在环上、深得离谱 —— 当成不是子会话:
/// 挂着的这些就连不成环,往上数得完
fn link_parents(v: &mut [Session], sessions: &[store::Session]) {
    let by_id: HashMap<&str, &store::Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let listed: HashMap<String, (String, String)> =
        v.iter().filter(|s| s.sub.is_none()).map(|s| (s.name.clone(), (s.group.clone(), s.title.clone()))).collect();
    // 父会话是自己的,和没有父会话一样(下面也是这么算的)
    let parent_of = |m: &store::Session| by_id.get(m.parent.as_str()).filter(|_| m.parent != m.id).copied();
    let endless = |m: &store::Session| std::iter::successors(Some(m), |m| parent_of(m)).nth(DEEP).is_some();
    for s in v.iter_mut() {
        let Some(m) = by_id.get(s.family()).filter(|m| !m.parent.is_empty() && m.parent != m.id && !endless(m)) else { continue };
        s.up = match by_id.get(m.parent.as_str()) {
            None => Up::Gone,
            Some(pm) => match listed.get(&pm.id) {
                Some((g, t)) if *g == s.group => Up::Under(pm.id.clone(), t.clone()),
                Some((_, t)) => Up::Apart(pm.id.clone(), t.clone()),
                None => Up::Apart(pm.id.clone(), pm.title.clone()),
            },
        };
    }
}

/// 分叉出来的对上原会话(登记表里的 `fork_of`):标题用列表里显示的;原会话没列着(删了)也记一笔
fn link_forks(v: &mut [Session], sessions: &[store::Session]) {
    let from: HashMap<&str, &str> =
        sessions.iter().filter(|s| !s.fork_of.is_empty()).map(|s| (s.id.as_str(), s.fork_of.as_str())).collect();
    if from.is_empty() {
        return;
    }
    let titles: HashMap<String, String> = v.iter().filter(|s| s.sub.is_none()).map(|s| (s.name.clone(), s.title.clone())).collect();
    for s in v.iter_mut().filter(|s| s.sub.is_none()) {
        if let Some(of) = from.get(s.name.as_str()) {
            s.fork = titles.get(*of).map_or(Fork::Gone, |t| Fork::Of(t.clone()));
        }
    }
}

/// 分组的线索:`N` / `n` 按路径定分组用(见 create.rs 的 group_for)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupHints {
    /// (分组路径, 默认路径)
    pub groups: Vec<(String, String)>,
}

pub fn group_hints() -> GroupHints {
    let groups = Store::open().and_then(|s| s.groups()).unwrap_or_default();
    // 默认目录和 `aca new -g .` 一样先规整(store::group_by_dir 比的是这个)
    GroupHints { groups: groups.into_iter().map(|g| (g.path, paths::real(&g.default_path))).collect() }
}

/// group 的同级分组(同一个上级下面的,连它自己),按列表里的先后;没有这个分组 = None。⇧↑ ⇧↓ 挪分组按这个数位置
pub fn group_siblings(group: &str) -> Option<Vec<String>> {
    let groups = Store::open().and_then(|s| s.groups()).ok()?;
    let rows: Vec<(String, i64)> = groups.into_iter().map(|g| (g.path, g.order)).collect();
    siblings(&rows, group)
}

/// rows 里 group 的同级(同一个父分组下面的,连它自己),同级之间和列表里一样按 sort_order 再按名字;没有 group = None
fn siblings(rows: &[(String, i64)], group: &str) -> Option<Vec<String>> {
    let parent = |p: &str| p.rsplit_once('/').map(|x| x.0.to_string());
    rows.iter().any(|r| r.0 == group).then_some(())?;
    let mut v: Vec<(i64, &str, &str)> =
        rows.iter().filter(|r| parent(&r.0) == parent(group)).map(|(p, o)| (*o, p.rsplit('/').next().unwrap_or(p), p.as_str())).collect();
    v.sort();
    Some(v.into_iter().map(|x| x.2.to_string()).collect())
}

/// 配置 / 库里记的路径可能带 ~
fn expand(d: &str) -> String {
    paths::expand(d)
}

/// 登记过的项目目录(归档的也算),新登记的在前(`N` 里找目录的候选)
pub fn project_dirs() -> Vec<String> {
    let Ok(store) = Store::open() else { return vec![] };
    let mut rows: Vec<(u64, String)> =
        store.sessions(true).unwrap_or_default().into_iter().filter(|s| !s.dir.is_empty()).map(|s| (s.created, s.dir)).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    rows.into_iter().map(|r| r.1).collect()
}

/// 有人在终端里连着的会话(按 tmux 的会话号)。不用 `#{session_attached}`:它把控制模式的客户端也算进去
fn attached_sessions() -> HashSet<String> {
    humans(&tmux(&["list-clients", "-F", "#{client_control_mode}\t#{session_id}"]))
}

fn humans(out: &str) -> HashSet<String> {
    out.lines().filter_map(|l| l.split_once('\t')).filter(|(ctl, _)| *ctl == "0").map(|(_, name)| name.to_string()).collect()
}

/// 最后有输出的时刻(比对已读、算空闲多久都用它):窗口的活动时间;会话的那个是最后有人动的时刻,不跟输出走
fn output_at(p: &Pane, now: u64) -> u64 {
    match (p.win_activity, p.activity) {
        (0, 0) => now,
        (0, a) => a,
        (w, _) => w,
    }
}

/// 每个 tmux 会话的主窗格(按会话号):登记过的是 agent 那一格(`@aca_agent`),别的是编号最小的那格。
/// 其余的窗口 / 窗格挂在它下面
fn main_panes<'a>(panes: &'a [Pane], known: &dyn Fn(&str) -> bool) -> HashMap<&'a str, &'a Pane> {
    let mut m: HashMap<&str, &Pane> = HashMap::new();
    for p in panes {
        let e = m.entry(p.sid.as_str()).or_insert(p);
        let rank = |x: &Pane| (!(known(&x.id) && x.agent), x.window, x.index);
        if rank(p) < rank(e) {
            *e = p;
        }
    }
    m
}

/// 窗格里在跑的 agent(在 shell 里手动起的,或者窗格直接开的就是它):(claude / codex, pid)
fn agent_in(kids: &Kids, p: &Pane) -> Option<(&'static str, u32)> {
    if p.pid == 0 {
        return None;
    }
    proc::agent_under(kids, p.pid).or_else(|| aca_core::agent::by_comm(&p.cmd).map(|a| (a, p.pid)))
}

/// 采集要用到的、从外面读来的东西
pub struct Inputs {
    pub now: u64,
    pub cfg: Config,
    pub sessions: Vec<store::Session>,
    pub groups: Vec<store::Group>,
    pub panes: Vec<Pane>,
    /// 和 panes 一一对应:每格的屏幕(带颜色)
    pub screens: Vec<Vec<String>>,
    /// 有人连着的 tmux 会话号
    pub attached: HashSet<String>,
    pub kids: Kids,
}

pub fn collect() -> Vec<Session> {
    gather(true)
}

/// 开界面时的第一屏:不读定时(第一次要把各会话的记录读一遍,几百 MB 要零点几秒),两秒后的那次刷新补上
pub fn collect_first() -> Vec<Session> {
    gather(false)
}

fn gather(schedules: bool) -> Vec<Session> {
    let now = paths::now();
    let cfg = aca_core::config::load();
    let (sessions, groups) = match Store::open() {
        Ok(st) => (st.sessions(false).unwrap_or_default(), st.groups().unwrap_or_default()),
        Err(_) => (vec![], vec![]),
    };
    let panes = aca_core::tmux::panes();
    // 窗格标题上的标题变了的先记下(下面读状态文件时就是新的);会话名跟着改了也不用重新列,登记过的不按会话名认
    status::follow_titles(&cfg, &sessions, &panes);
    // 全部窗格的屏幕一条 tmux 命令抓回来(一格起一个的话每刷新一次就是几十个进程);查进程表、连着的客户端同时做。
    // 带颜色 / 链接的转义序列(-e):预览照原样上色,判断状态时再去掉
    let (screens, (attached, kids)) = thread::scope(|s| {
        let other = s.spawn(|| (attached_sessions(), proc::fg_kids()));
        let ids: Vec<&str> = panes.iter().map(|p| p.pane.as_str()).collect();
        let screens: Vec<Vec<String>> = aca_core::tmux::capture_many(&ids, true).into_iter().map(trim_tail).collect();
        (screens, other.join().unwrap_or_default())
    });
    // 它没报的那一轮结束(Codex 出错的、Stop hook 没被信任的)先补上:下面读状态文件时就是新的
    status::settle(&cfg, &sessions, &panes, &|p| {
        let at = panes.iter().position(|q| q.pane == p.pane);
        at.map(|i| screens[i].iter().map(|l| crate::ansi::strip(l)).collect()).unwrap_or_default()
    });
    let homes = homes_of(&panes, &kids);
    let home_of = |pid: u32, a: Agent| homes.get(&(pid, a)).cloned().flatten();
    let mut out = build(Inputs { now, cfg, sessions, groups, panes, screens, attached, kids }, &status::load, &home_of);
    // 各家自己派出去的子代理:读它们的记录(见 subagent.rs),后台的还在跑的会话算在跑
    crate::subagent::attach(&mut out, &status::load);
    // 给自己定了还没到点的:agent 还开着才有(进程一退就没了)。读过的记录只读后面新写的
    if schedules {
        let open = |s: &Session| !matches!(s.state, State::Dead | State::Stopped | State::Crashed | State::Exited);
        for s in out.iter_mut().filter(|s| s.sub.is_none() && !s.deck_id.is_empty() && open(s)) {
            s.due = aca_core::schedule::due(&status::load(&s.deck_id), now);
        }
    }
    out
}

type Homes = HashMap<(u32, Agent), Option<String>>;

/// 窗格里在跑的 agent 各自用的配置目录:(pid, 哪一家) → 目录。
/// 要看进程的环境(一个进程起一次 `ps`,2–3 毫秒),进程的环境不会变,所以问过的记着;没问过的并行问
fn homes_of(panes: &[Pane], kids: &Kids) -> Homes {
    static HOMES: LazyLock<std::sync::Mutex<Homes>> = LazyLock::new(Default::default);
    let want: HashSet<(u32, Agent)> =
        panes.iter().filter(|p| !p.dead).filter_map(|p| agent_in(kids, p)).map(|(tool, pid)| (pid, kind_of(tool))).collect();
    let mut known = HOMES.lock().unwrap_or_else(|e| e.into_inner());
    // 进程没了的不留(pid 以后会被别的进程用)
    known.retain(|k, _| want.contains(k));
    let missing: Vec<(u32, Agent)> = want.into_iter().filter(|k| !known.contains_key(k)).collect();
    let got: Vec<Option<String>> = thread::scope(|s| {
        let hs: Vec<_> = missing.iter().map(|&(pid, agent)| s.spawn(move || config_home(pid, agent))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or(None)).collect()
    });
    known.extend(missing.into_iter().zip(got));
    known.clone()
}

/// 屏幕上看到的:认得出里面跑的是哪家,就用那家自己的读法(aca-core 里的,和登记的会话同一份);认不出的照 Claude 的样子读
fn seen_state(tool: Option<&str>, plain: &[String]) -> (Option<State>, String, u64) {
    use aca_core::screen::Seen;
    match tool.and_then(aca_core::agent::builtin) {
        Some(agent) => match agent.screen(plain) {
            Seen::Ask(q) => (Some(State::Ask), q, 0),
            Seen::Running(n) => (Some(State::Running), String::new(), n),
            Seen::Quiet | Seen::Moving | Seen::Unsure => (None, String::new(), 0),
        },
        None => screen_state(plain),
    }
}

/// 把登记表、tmux 里实际在的窗格、hook 写的状态、屏幕上看到的,合成列表。
/// `run`:会话的运行时状态(hook 写的);`home_of`:进程环境里的配置目录(手动起的 agent 认账号用)
pub fn build(inp: Inputs, run: &dyn Fn(&str) -> Run, home_of: &dyn Fn(u32, Agent) -> Option<String>) -> Vec<Session> {
    let Inputs { now, cfg, sessions, groups, panes, screens, attached, kids } = inp;
    let by_id: HashMap<&str, &store::Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let group_name = |g: &str| if g.is_empty() { UNGROUPED.to_string() } else { g.to_string() };
    let mut order: HashMap<String, i64> = groups.iter().map(|g| (g.path.clone(), g.order)).collect();
    // 没分组的排在所有分组后面
    order.insert(UNGROUPED.into(), i64::MAX);
    let names: Vec<String> = sessions.iter().map(|s| group_name(&s.group)).collect();
    // 空分组也排上位置(列表里照样有它的标题)
    let ranks = group_ranks(names.iter().map(String::as_str).chain(groups.iter().map(|g| g.path.as_str())), &order);
    let mains = main_panes(&panes, &|id| by_id.contains_key(id));
    // 同一个登记 id 记在了两个 tmux 会话上(不该有):只认先遇到的那个,别的当普通会话
    let mut claimed: HashSet<&str> = HashSet::new();
    let mut out: Vec<Session> = vec![];
    let mut subs: Vec<(&Pane, Vec<String>)> = vec![];
    for (p, screen) in panes.iter().zip(screens) {
        if mains.get(p.sid.as_str()).is_none_or(|m| m.pane != p.pane) {
            subs.push((p, screen));
            continue;
        }
        let plain: Vec<String> = screen.iter().map(|l| crate::ansi::strip(l)).collect();
        let running = agent_in(&kids, p).filter(|_| !p.dead);
        let (scr, scr_q, scr_ran) = seen_state(running.map(|r| r.0), &plain);
        let spinning = p.title.chars().next().is_some_and(|c| SPINNER.contains(c));
        let act = output_at(p, now);
        let reg = by_id.get(p.id.as_str()).copied().filter(|s| claimed.insert(s.id.as_str()));
        let Some(s) = reg else {
            // 普通 tmux 会话:屏幕说什么是什么;在里面手动起了 claude / codex 的,按那个进程认是哪家、哪个账号
            let state = match scr {
                _ if p.dead => State::Dead,
                Some(st) => st,
                None if spinning => State::Running,
                None => State::Idle,
            };
            let (agent, who) = match running {
                Some((tool, pid)) => identify(tool, home_of(pid, kind_of(tool)), &cfg),
                None => (Agent::None, String::new()),
            };
            let own = own_title(&p.title);
            out.push(Session {
                name: p.session.clone(),
                tmux: p.sid.clone(),
                title: if own.is_empty() { p.session.clone() } else { own },
                locked: false,
                group: "-".into(),
                state,
                question: scr_q,
                idle_for: now.saturating_sub(act),
                active_at: act,
                ran: scr_ran,
                attached: attached.contains(&p.sid),
                screen,
                pinned: false,
                noted: false,
                space: crate::spaces::HOME,
                dir: p.path.clone(),
                profile: String::new(),
                tool: String::new(),
                account: String::new(),
                agent,
                who,
                pid: running.map_or(0, |r| r.1),
                deck_id: String::new(),
                info: format!("Path: {}", p.path),
                worktree: false,
                slot: (usize::MAX, 0, 0),
                pane: p.pane.clone(),
                sub: None,
                up: Up::Top,
                fork: Fork::No,
                subagents: vec![],
                subagent: None,
                tokens: vec![],
                error: String::new(),
                error_kind: String::new(),
                idle_stopped: 0,
                due: vec![],
                blank: false,
            });
            continue;
        };
        let r = run(&s.id);
        let handle = aca_core::agent::of(&cfg, s).ok().map(|x| x.0);
        let seen = handle.as_ref().filter(|_| !p.dead).map(|h| status::look(&s.id, &**h, &plain));
        let (st, question, ran) = status::merge(Some(p), &r, seen.as_ref(), now);
        let (mut state, mut question, mut ran) = (state_of(st), question, ran);
        let shell = agent_of_tool(&cfg, &s.tool) == Agent::None;
        // 开成 shell 的会话没有 hook、也没有它那家的读法:在里面手动起的 agent 在问 / 在跑,照屏幕上的算
        if shell && state == State::Idle {
            match scr {
                Some(x) => (state, question, ran) = (x, scr_q, scr_ran),
                None if spinning => state = State::Running,
                None => {}
            }
        }
        // 不在跑、还开着的 agent 照 hook 报的算多久没动(见 `Run::hook_at`):不然几天没用的一重画就成了几分钟前
        let act = match r.hook_at() {
            at if at > 0 && !shell && matches!(state, State::Idle | State::Unread | State::Ask | State::Error) => at,
            _ => act,
        };
        let (agent, who) = match running.filter(|_| shell) {
            Some((tool, pid)) => identify(tool, home_of(pid, kind_of(tool)), &cfg),
            None => registered(&cfg, s),
        };
        let conv = if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() };
        let group = group_name(&s.group);
        // shell 会话里 cd 到哪就算哪(登记的只是开的时候的目录)
        let dir = if shell && !p.path.is_empty() { p.path.clone() } else { s.dir.clone() };
        out.push(Session {
            name: s.id.clone(),
            tmux: p.sid.clone(),
            title: status::title(s, &r),
            locked: s.title_locked,
            slot: (ranks[&group], s.order, s.created as i64),
            group,
            state,
            question,
            idle_for: now.saturating_sub(act),
            active_at: act,
            ran,
            attached: attached.contains(&p.sid),
            screen,
            pinned: false,
            noted: false,
            space: crate::spaces::HOME,
            info: copy_info(&dir, &conv),
            dir,
            profile: REGISTERED.into(),
            tool: if shell { String::new() } else { s.tool.clone() },
            account: s.account.clone(),
            agent,
            who,
            pid: running.map_or(0, |r| r.1),
            deck_id: s.id.clone(),
            worktree: false,
            pane: p.pane.clone(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: r.live_tokens(now).into_iter().collect(),
            error: if state == State::Error { r.error.clone() } else { String::new() },
            error_kind: if state == State::Error { r.error_kind.clone() } else { String::new() },
            idle_stopped: if p.dead { r.idle_stopped } else { 0 },
            due: vec![],
            blank: false,
        });
    }
    // 会话里另开的窗口(prefix c)、分出来的窗格(prefix % / "):列在会话下面,自己的状态、自己认 agent。
    // 不能重启 / 改名;d 关掉、D 停下的是这一格
    let by_sid: HashMap<String, Session> = out.iter().map(|s| (s.tmux.clone(), s.clone())).collect();
    for (p, screen) in subs {
        let Some(parent) = by_sid.get(&p.sid) else { continue };
        let plain: Vec<String> = screen.iter().map(|l| crate::ansi::strip(l)).collect();
        let running = agent_in(&kids, p).filter(|_| !p.dead);
        let (scr, question, ran) = seen_state(running.map(|r| r.0), &plain);
        let spinning = p.title.chars().next().is_some_and(|c| SPINNER.contains(c));
        let state = match scr {
            _ if p.dead => State::Dead,
            Some(s @ (State::Ask | State::Running)) => s,
            _ if spinning => State::Running,
            _ => State::Idle,
        };
        let (agent, who) = match running {
            Some((tool, pid)) => identify(tool, home_of(pid, kind_of(tool)), &cfg),
            None => (Agent::None, String::new()),
        };
        let own = own_title(&p.title);
        // Claude 的进程名是版本号(tmux 自动改的窗口名也跟着是),写成 claude
        let pretty = |n: &str| aca_core::agent::by_comm(n).map_or_else(|| n.to_string(), String::from);
        let split = p.win_panes > 1;
        let title = match () {
            _ if !own.is_empty() => own,
            // 没分屏的窗口:窗口名(改过名的就是改的名字,没改过 tmux 自动写前台命令);没起名的新窗口叫 tmux,不算
            _ if !split && !p.win_name.is_empty() && p.win_name != "tmux" => pretty(&p.win_name),
            _ => pretty(&p.cmd),
        };
        let act = output_at(p, now);
        out.push(Session {
            name: format!("{}\u{1}{}", parent.name, p.pane),
            title,
            state,
            question,
            idle_for: now.saturating_sub(act),
            active_at: act,
            ran,
            attached: false,
            screen,
            pinned: false,
            noted: false,
            space: crate::spaces::HOME,
            dir: p.path.clone(),
            deck_id: String::new(),
            info: format!("Path: {}", p.path),
            worktree: false,
            agent,
            who,
            pid: running.map_or(0, |r| r.1),
            pane: p.pane.clone(),
            sub: Some(Sub { parent: parent.name.clone(), parent_title: parent.title.clone(), win: p.window, pane: p.index, split }),
            idle_stopped: 0,
            due: vec![],
            ..parent.clone()
        });
    }
    // 登记着、tmux 里没有的(停掉之后重启过电脑、tmux 服务没了的)照样列出来,⏎ 重新启动接着聊
    let live: HashSet<String> = out.iter().map(|s| s.name.clone()).collect();
    for s in sessions.iter().filter(|s| !live.contains(&s.id)) {
        let r = run(&s.id);
        let (agent, who) = registered(&cfg, s);
        // 拿回来(取消归档)也算动过:不然从回收站拿回来的,下一轮又被自动删(`[idle] remove_after`)
        let at = r.ts.max(r.restored).max(s.created);
        let conv = if r.conv.is_empty() { s.conv.clone() } else { r.conv.clone() };
        let group = group_name(&s.group);
        out.push(Session {
            name: s.id.clone(),
            tmux: String::new(),
            title: status::title(s, &r),
            locked: s.title_locked,
            slot: (ranks[&group], s.order, s.created as i64),
            group,
            state: if r.status == "error" { State::Crashed } else { State::Stopped },
            question: String::new(),
            idle_for: now.saturating_sub(at),
            active_at: at,
            ran: 0,
            attached: false,
            screen: vec![],
            pinned: false,
            noted: false,
            space: crate::spaces::HOME,
            dir: s.dir.clone(),
            profile: REGISTERED.into(),
            tool: if agent == Agent::None { String::new() } else { s.tool.clone() },
            account: s.account.clone(),
            agent,
            who,
            pid: 0,
            deck_id: s.id.clone(),
            info: copy_info(&s.dir, &conv),
            worktree: false,
            pane: String::new(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: vec![],
            error: String::new(),
            error_kind: String::new(),
            idle_stopped: r.idle_stopped,
            due: vec![],
            blank: false,
        });
    }
    // 组里(连子分组)一个会话都没有的:各占一项,列表里照样有它的标题,停在上面 N 就开在这个组里。都沉在列表最底下
    let within = |g: &str, top: &str| g == top || g.strip_prefix(top).is_some_and(|r| r.starts_with('/'));
    for g in groups.iter().filter(|g| !sessions.iter().any(|s| within(&s.group, &g.path))) {
        out.push(blank_row(&g.path, ranks[&g.path]));
    }
    link_parents(&mut out, &sessions);
    link_forks(&mut out, &sessions);
    sort_sessions(&mut out);
    out
}

/// 空分组那一项的键:这个字开头,后面是分组(会话的键里不会有)
const BLANK: char = '\u{3}';

/// 空分组 group 在列表里占的那一项(`Session::blank`):不是会话,没有窗格、目录、登记 id。
/// 排在所有会话后面(连普通 tmux 会话),空组之间照分组的先后(rank)
pub fn blank_row(group: &str, rank: usize) -> Session {
    Session {
        name: format!("{BLANK}{group}"),
        tmux: String::new(),
        title: String::new(),
        locked: false,
        group: group.to_string(),
        state: State::Idle,
        question: String::new(),
        idle_for: 0,
        active_at: 0,
        ran: 0,
        attached: false,
        screen: vec![],
        pinned: false,
        noted: false,
        space: crate::spaces::HOME,
        dir: String::new(),
        profile: REGISTERED.into(),
        tool: String::new(),
        account: String::new(),
        agent: Agent::None,
        who: String::new(),
        pid: 0,
        deck_id: String::new(),
        info: String::new(),
        worktree: false,
        slot: (usize::MAX, 1, rank as i64),
        pane: String::new(),
        sub: None,
        up: Up::Top,
        fork: Fork::No,
        subagents: vec![],
        subagent: None,
        tokens: vec![],
        error: String::new(),
        error_kind: String::new(),
        idle_stopped: 0,
        due: vec![],
        blank: true,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_session_left_last_comes_first_then_the_one_entered() {
        let been: Vec<String> = ["乙", "甲", "丙"].map(String::from).into();
        // 进了甲、在里面切到丁、从丁出来:丁最近,甲其次,老的往后挪,不重复
        assert_eq!(been_after("丁", "甲", &been), ["丁", "甲", "乙", "丙"]);
        // 进了哪个就从哪个出来
        assert_eq!(been_after("丙", "丙", &been), ["丙", "乙", "甲"]);
        let many: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        assert_eq!(been_after("x", "y", &many).len(), 8);
    }

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_terminal_clients_count_as_open_elsewhere() {
        // $1 有人连着(另外还挂着一个控制模式的);$2 只有控制模式的客户端,人早走了
        let got = humans("0\t$1\n1\t$1\n1\t$2\n0\t$3\n");
        let mut got: Vec<_> = got.into_iter().collect();
        got.sort();
        assert_eq!(got, ["$1", "$3"]);
        assert!(humans("").is_empty());
    }

    #[test]
    fn detects_ask_with_question() {
        let s =
            v(&["● 我需要你拍板", "", "要用哪个方案?", "", "❯ 1. B", "  2. A", "", "Enter to select · ↑/↓ to navigate · Esc to cancel"]);
        assert_eq!(screen_state(&s), (Some(State::Ask), "要用哪个方案?".into(), 0));
    }

    #[test]
    fn detects_running_duration() {
        let bar = "─".repeat(40);
        let s = v(&[
            "⏺ Bash(ls)",
            "· Smooshing… (1h 9m 1s · ↓ 9.1k tokens · esc to interrupt)",
            &bar,
            "❯ ",
            &bar,
            "  ⏵⏵ bypass permissions on",
        ]);
        assert_eq!(screen_state(&s), (Some(State::Running), String::new(), 4141));
        let s = v(&["· Thinking… (45s · ↑ 1k tokens)", &bar, "❯ ", &bar]);
        assert_eq!(screen_state(&s).2, 45);
    }

    #[test]
    fn finished_is_not_running() {
        let bar = "─".repeat(40);
        let s = v(&["⏺ 完成了。", "✻ Sautéed for 1m 49s", &bar, "❯ ", &bar, "  ⏵⏵ bypass permissions on (shift+tab to cycle)"]);
        assert_eq!(screen_state(&s).0, None);
    }

    #[test]
    fn output_time_is_the_window_activity_not_the_last_attach() {
        // 100 进去看过、回来;Claude 接着跑,500 跑完。会话的 activity 停在 100
        let p = Pane { activity: 100, win_activity: 500, ..Default::default() };
        assert_eq!(output_at(&p, 7), 500);
        assert_eq!(output_at(&Pane { activity: 100, ..Default::default() }, 7), 100);
        assert_eq!(output_at(&Pane::default(), 7), 7); // 读不到的按现在算
    }

    fn pane(sid: &str, id: &str, agent: bool, pane: &str, win: u32, idx: u32) -> Pane {
        Pane {
            sid: sid.into(),
            session: format!("名字{sid}"),
            id: id.into(),
            agent,
            pane: pane.into(),
            window: win,
            index: idx,
            win_panes: 1,
            ..Default::default()
        }
    }

    #[test]
    fn the_agent_pane_is_the_session_and_the_rest_hang_under_it() {
        let panes = [
            // 登记的会话:agent 在 2 号窗口(1 号是后来开的、挪到前面去的)
            pane("$1", "ab12", false, "%5", 1, 0),
            pane("$1", "ab12", true, "%3", 2, 0),
            // 普通 tmux 会话:编号最小的那格
            pane("$2", "", false, "%8", 1, 1),
            pane("$2", "", false, "%7", 1, 0),
            // 带着记号、登记表里却没有的(删了登记的):当普通会话
            pane("$3", "gone", true, "%10", 2, 0),
            pane("$3", "gone", false, "%9", 1, 0),
        ];
        let mains = main_panes(&panes, &|id| id == "ab12");
        assert_eq!((mains["$1"].pane.as_str(), mains["$2"].pane.as_str(), mains["$3"].pane.as_str()), ("%3", "%7", "%9"));
        assert_eq!(own_title("✳ 修登录"), "修登录");
        assert_eq!(own_title(&format!("{}.local", *HOST)), ""); // tmux 默认的主机名不算标题
        let sub = |win, pane, split| Sub { parent: "a".into(), parent_title: String::new(), win, pane, split };
        assert_eq!((sub(2, 0, false).loc(), sub(1, 1, true).loc()), ("#2".to_string(), "#1.1".to_string()));
        // 排序:子窗格紧跟自己的会话,按窗口号、格号
        let mk = |name: &str, s: Option<Sub>| Session { name: name.into(), sub: s, ..mk_session() };
        let mut v = vec![mk("b", None), mk("a\u{1}%5", Some(sub(2, 0, false))), mk("a\u{1}%3", Some(sub(1, 1, true))), mk("a", None)];
        sort_sessions(&mut v);
        assert_eq!(v.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["a", "a\u{1}%3", "a\u{1}%5", "b"]);
    }

    pub(crate) fn mk_session() -> Session {
        Session {
            name: String::new(),
            tmux: String::new(),
            title: String::new(),
            locked: false,
            group: "-".into(),
            state: State::Idle,
            question: String::new(),
            idle_for: 0,
            active_at: 0,
            ran: 0,
            attached: false,
            screen: vec![],
            pinned: false,
            noted: false,
            space: crate::spaces::HOME,
            dir: String::new(),
            profile: String::new(),
            tool: String::new(),
            account: String::new(),
            agent: Agent::None,
            who: String::new(),
            pid: 0,
            deck_id: String::new(),
            info: String::new(),
            worktree: false,
            slot: (0, 0, 0),
            pane: String::new(),
            sub: None,
            up: Up::Top,
            fork: Fork::No,
            subagents: vec![],
            subagent: None,
            tokens: vec![],
            error: String::new(),
            error_kind: String::new(),
            idle_stopped: 0,
            due: vec![],
            blank: false,
        }
    }

    #[test]
    fn order_ignores_state_activity_and_title() {
        let mk = |name: &str, title: &str, st: State, idle: u64, slot: (usize, i64, i64)| Session {
            name: name.into(),
            title: title.into(),
            state: st,
            idle_for: idle,
            slot,
            ..mk_session()
        };
        let mut v = vec![
            mk("d", "甲", State::Ask, 99999, (usize::MAX, 0, 0)), // 普通 tmux 会话,等你操作也排最后
            mk("c", "乙", State::Dead, 0, (1, 0, 0)),
            mk("b", "丙", State::Running, 5, (0, 2, 0)),
            mk("a", "丁", State::Idle, 9000, (0, 1, 7)),
            mk("e", "戊", State::Idle, 1, (0, 1, 3)), // 同一个 sort_order 按创建时间
        ];
        sort_sessions(&mut v);
        let order: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["e", "a", "b", "c", "d"]);
    }

    fn reg(id: &str, tool: &str, group: &str, order: i64, created: u64) -> store::Session {
        store::Session {
            id: id.into(),
            title: id.into(),
            tool: tool.into(),
            group: group.into(),
            order,
            dir: "/p".into(),
            created,
            ..Default::default()
        }
    }

    #[test]
    fn children_hang_under_their_parent_at_every_level() {
        let kid = |id: &str, group: &str, parent: &str| store::Session { parent: parent.into(), ..reg(id, "claude", group, 0, 1) };
        let sessions = vec![
            store::Session { title: "父".into(), ..reg("p", "claude", "g", 0, 1) },
            reg("q", "claude", "g", 0, 1),
            kid("c1", "g", "p"),
            kid("c2", "h", "p"),    // 在别的分组
            kid("c3", "g", "GONE"), // 父会话删了
            kid("gc", "g", "c1"),   // 父会话自己是子会话:挂在它下面
            kid("gg", "g", "gc"),   // 再往下一层
            kid("c2k", "h", "c2"),  // 父会话顶格(它的父会话在别的分组),自己照样挂在它下面
            store::Session { title: "没列出来的".into(), ..reg("a", "claude", "g", 0, 1) },
            kid("ac", "g", "a"), // 父会话登记着,但没列出来
        ];
        let mk = |name: &str, group: &str, slot: (usize, i64, i64)| Session {
            name: name.into(),
            title: name.into(),
            group: group.into(),
            slot,
            ..mk_session()
        };
        let win = |of: &Session| Session {
            name: format!("{}\u{1}%9", of.name),
            sub: Some(Sub { parent: of.name.clone(), parent_title: of.title.clone(), win: 2, pane: 0, split: false }),
            ..of.clone()
        };
        let p = Session { title: "父(屏幕)".into(), ..mk("p", "g", (0, 5, 100)) };
        let c1 = mk("c1", "g", (0, 0, 10)); // 组内排最前,还是跟着父会话
        let gc = mk("gc", "g", (0, 8, 0));
        let mut v = vec![
            mk("c3", "g", (0, 9, 0)),
            mk("gg", "g", (0, -3, 0)),
            win(&c1),
            c1,
            win(&gc),
            gc,
            mk("ac", "g", (0, 7, 0)),
            mk("c2k", "h", (1, -1, 0)),
            mk("c2", "h", (1, 0, 0)),
            win(&p),
            p,
            mk("q", "g", (0, 1, 50)),
        ];
        link_parents(&mut v, &sessions);
        sort_sessions(&mut v);
        let order: Vec<String> = v.iter().map(|s| s.name.replace('\u{1}', "#")).collect();
        // 一个会话后面是它整家:自己的窗口、子会话(子会话的窗口、它再派出去的)……然后才是别的会话
        assert_eq!(order, ["q", "p", "p#%9", "c1", "c1#%9", "gc", "gc#%9", "gg", "ac", "c3", "c2", "c2k"]);
        let up = |n: &str| v.iter().find(|s| s.name == n).unwrap().up.clone();
        let under = Up::Under("p".into(), "父(屏幕)".into()); // 标题用列表里显示的
        assert_eq!(up("c1"), under);
        assert_eq!(up("c1\u{1}%9"), under); // 窗口跟着自己的会话
        assert_eq!(up("c2"), Up::Apart("p".into(), "父(屏幕)".into()));
        assert_eq!(up("c3"), Up::Gone);
        assert_eq!(up("gc"), Up::Under("c1".into(), "c1".into()));
        assert_eq!(up("gc\u{1}%9"), Up::Under("c1".into(), "c1".into()));
        assert_eq!(up("gg"), Up::Under("gc".into(), "gc".into()));
        assert_eq!(up("c2k"), Up::Under("c2".into(), "c2".into()));
        assert_eq!(up("ac"), Up::Apart("a".into(), "没列出来的".into()));
        assert_eq!((up("p"), up("q")), (Up::Top, Up::Top));
        // 往上数:由近到远,数到顶格的那个为止
        let hangs = hangs(&v);
        let chain = |n: &str| v.iter().find(|s| s.name == n).unwrap().ancestors(&hangs).collect::<Vec<_>>().join(" ");
        assert_eq!(
            (chain("gg"), chain("gc\u{1}%9"), chain("c2k"), chain("p")),
            ("gc c1 p".into(), "c1 p".into(), "c2".into(), String::new())
        );
    }

    #[test]
    fn siblings_keep_their_own_order_at_every_level() {
        let mk = |name: &str, up: &str, order: i64| Session {
            name: name.into(),
            title: name.into(),
            group: "g".into(),
            slot: (0, order, 0),
            up: if up.is_empty() { Up::Top } else { Up::Under(up.into(), up.into()) },
            ..mk_session()
        };
        let win = |of: &Session, win: u32| Session {
            name: format!("{}#{win}", of.name),
            sub: Some(Sub { parent: of.name.clone(), parent_title: of.title.clone(), win, pane: 0, split: false }),
            ..of.clone()
        };
        let (a1, a1y) = (mk("a1", "a", 2), mk("a1y", "a1", 1));
        // 组内顺序各层只和自己的兄弟比:a2 排在 a1 前面,a1 整家(连孙会话的窗口)还是连在一起
        let mut v = vec![
            mk("b", "", 2),
            win(&a1y, 2),
            mk("a1x", "a1", 9),
            a1y,
            win(&a1, 3),
            win(&a1, 2),
            a1,
            mk("a2", "a", 1),
            mk("b1", "b", -5),
            mk("a", "", 1),
        ];
        sort_sessions(&mut v);
        let order: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, ["a", "a2", "a1", "a1#2", "a1#3", "a1y", "a1y#2", "a1x", "b", "b1"]);
    }

    /// 分叉出来的不挂在原会话下面:顶格,只记着分自谁(标题用列表里显示的);原会话没列着 = 删了
    #[test]
    fn forks_stay_top_level_and_remember_who_they_came_from() {
        let fork = |id: &str, of: &str| store::Session { fork_of: of.into(), ..reg(id, "claude", "g", 0, 1) };
        let sessions = vec![reg("o", "claude", "g", 0, 1), fork("f", "o"), fork("lost", "gone")];
        let mut v: Vec<Session> = sessions.iter().map(|m| Session { name: m.id.clone(), group: m.group.clone(), ..mk_session() }).collect();
        v[0].title = "原来的(屏幕)".into();
        // f 里另开的窗口:不标
        v.push(Session {
            name: "f\u{1}%9".into(),
            sub: Some(Sub { parent: "f".into(), parent_title: String::new(), win: 2, pane: 0, split: false }),
            ..v[1].clone()
        });
        link_parents(&mut v, &sessions);
        link_forks(&mut v, &sessions);
        let got: Vec<(&str, &Up, &Fork)> = v.iter().map(|s| (s.name.as_str(), &s.up, &s.fork)).collect();
        assert_eq!(
            got,
            [
                ("o", &Up::Top, &Fork::No),
                ("f", &Up::Top, &Fork::Of("原来的(屏幕)".into())),
                ("lost", &Up::Top, &Fork::Gone),
                ("f\u{1}%9", &Up::Top, &Fork::No),
            ]
        );
    }

    #[test]
    fn a_loop_of_parents_falls_back_to_top_level() {
        let kid = |id: &str, parent: &str| store::Session { parent: parent.into(), ..reg(id, "claude", "g", 0, 1) };
        let mut sessions = vec![
            kid("a", "b"), // a 和 b 互为父会话
            kid("b", "a"),
            kid("ak", "a"),  // 父会话在环上:往上也数不到头
            kid("me", "me"), // 父会话是自己:和没有一样
            kid("mek", "me"),
            kid("x", "y"), // 三个转一圈
            kid("y", "z"),
            kid("z", "x"),
        ];
        // 一条长链:d0 顶格,d1 挂 d0 …… 往上数得到头的照常挂,数 DEEP 层还没到头的都顶格
        sessions.push(reg("d0", "claude", "g", 0, 1));
        sessions.extend((1..=DEEP + 1).map(|i| kid(&format!("d{i}"), &format!("d{}", i - 1))));
        let mut v: Vec<Session> =
            sessions.iter().map(|m| Session { name: m.id.clone(), title: m.id.clone(), group: "g".into(), ..mk_session() }).collect();
        link_parents(&mut v, &sessions);
        let up = |v: &[Session], n: &str| v.iter().find(|s| s.name == n).unwrap().up.clone();
        let deep = |i: usize| format!("d{i}");
        for n in ["a", "b", "ak", "me", "x", "y", "z", &deep(DEEP), &deep(DEEP + 1)] {
            assert_eq!(up(&v, n), Up::Top, "{n}");
        }
        assert_eq!(up(&v, "mek"), Up::Under("me".into(), "me".into()));
        assert_eq!(up(&v, &deep(DEEP - 1)), Up::Under(deep(DEEP - 2), deep(DEEP - 2)));
        // 排序、往上数都数得完,谁也没丢
        let n = v.len();
        sort_sessions(&mut v);
        assert_eq!(v.len(), n);
        let hangs = hangs(&v);
        assert!(v.iter().all(|s| s.ancestors(&hangs).count() < DEEP));
        // 手拼出来的环(登记表里不会有,link_parents 拆掉了)也数得完
        let mut ring = vec![
            Session { name: "r1".into(), up: Up::Under("r2".into(), "r2".into()), ..mk_session() },
            Session { name: "r2".into(), up: Up::Under("r1".into(), "r1".into()), ..mk_session() },
        ];
        sort_sessions(&mut ring);
        assert_eq!(ring[0].ancestors(&super::hangs(&ring)).count(), DEEP);
    }

    const CONFIG: &str = "\
[accounts.claude]
main = \"/h/.claude-main\"
team = { home = \"/h/.claude-team\", args = [\"--model\", \"opus\"] }
bed = { env = { CLAUDE_CODE_USE_BEDROCK = \"1\" } }
[accounts.codex]
alt = \"/h/.cx-alt\"
[tools.claude]
account = \"main\"
[tools.mycx]
kind = \"codex\"
account = \"alt\"
[tools.odd]
";

    #[test]
    fn a_registered_session_shows_the_account_it_was_registered_with() {
        let cfg = aca_core::config::parse(CONFIG);
        let who = |tool: &str, account: &str| {
            registered(&cfg, &store::Session { tool: tool.into(), account: account.into(), ..Default::default() })
        };
        assert_eq!(who("claude", "team"), (Agent::Claude, "team".into()));
        // 没写账号的写工具名;直接写目录的写目录名
        assert_eq!(who("claude", ""), (Agent::Claude, "claude".into()));
        assert_eq!(who("codex", "/h/.newcodex"), (Agent::Codex, "newcodex".into()));
        // 自己起名的工具认 kind;声明式的算别家;开成 shell 的没有
        assert_eq!(who("mycx", "alt"), (Agent::Codex, "alt".into()));
        assert_eq!(who("odd", ""), (Agent::Other, "odd".into()));
        assert_eq!(who("shell", ""), (Agent::None, String::new()));
        // 只列登记了配置目录的账号,先 Claude 再 Codex
        let names: Vec<(Agent, String)> = accounts(&cfg).into_iter().map(|a| (a.0, a.1)).collect();
        assert_eq!(names, [(Agent::Claude, "main".into()), (Agent::Claude, "team".into()), (Agent::Codex, "alt".into())]);
    }

    #[test]
    fn a_hand_started_agent_is_matched_back_to_its_account_by_its_config_dir() {
        let cfg = aca_core::config::parse(CONFIG);
        let id = |tool: &str, home: Option<&str>| identify(tool, home.map(String::from), &cfg);
        assert_eq!(id("claude", Some("/h/.claude-main")), (Agent::Claude, "main".into()));
        assert_eq!(id("claude", Some("/h/.claude-team")), (Agent::Claude, "team".into()));
        assert_eq!(id("codex", Some("/h/.cx-alt")), (Agent::Codex, "alt".into()));
        // 没登记的目录:写目录名;没设配置目录:写它是哪家
        assert_eq!(id("claude", Some("/h/.claude-other")), (Agent::Claude, "claude-other".into()));
        assert_eq!(id("codex", None), (Agent::Codex, "codex".into()));
        // 目录登记在别家名下的不算
        assert_eq!(id("codex", Some("/h/.claude-main")), (Agent::Codex, "claude-main".into()));
    }

    #[test]
    fn agent_label_names_the_vendor_and_the_account() {
        let mut s = mk_session();
        assert_eq!(s.agent_label(), "");
        (s.agent, s.who) = (Agent::Claude, "team".into());
        assert_eq!(s.agent_label(), "Claude·team");
        (s.agent, s.who) = (Agent::Codex, "codex".into());
        assert_eq!(s.agent_label(), "Codex");
        (s.agent, s.who) = (Agent::Other, "odd".into());
        assert_eq!(s.agent_label(), "odd");
    }

    #[test]
    fn groups_follow_the_tree_parents_before_children() {
        let order: HashMap<String, i64> =
            [("b", 0), ("b/y", 2), ("b/x", 5), ("a", 1), ("c", -1)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let r = group_ranks(["a", "b/x", "b", "c", "b/y", "a"].into_iter(), &order);
        let mut gs: Vec<(&usize, &str)> = r.iter().map(|(g, i)| (i, g.as_str())).collect();
        gs.sort();
        let gs: Vec<&str> = gs.into_iter().map(|(_, g)| g).collect();
        assert_eq!(gs, ["c", "b", "b/y", "b/x", "a"]);
    }

    #[test]
    fn siblings_share_the_parent_and_sort_like_the_list() {
        let rows: Vec<(String, i64)> = [("b", 0), ("b/y", 2), ("b/x", 5), ("a", 1), ("c", -1), ("Z", 1), ("b/y/1", 0)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        assert_eq!(siblings(&rows, "a").unwrap(), ["c", "b", "Z", "a"]); // 一样排第 1 的按名字
        assert_eq!(siblings(&rows, "b/x").unwrap(), ["b/y", "b/x"]);
        assert_eq!(siblings(&rows, "b/y/1").unwrap(), ["b/y/1"]);
        assert_eq!(siblings(&rows, "d"), None);
    }

    #[test]
    fn copy_info_is_the_dir_and_the_conversation() {
        assert_eq!(copy_info("/p", "abc"), "Path: /p\nSession: abc");
        assert_eq!(copy_info("/p", ""), "Path: /p");
        assert_eq!(copy_info("", ""), "");
    }

    #[test]
    fn a_hand_started_agent_is_read_the_way_its_own_kind_draws_the_screen() {
        // Codex 在跑时画的是「• Working (1m 05s • esc to interrupt)」:照 Claude 的样子读认不出来
        let codex = v(&["› 写一篇长文", "", "• Working (1m 05s • esc to interrupt)", "", "› Ask Codex to do anything"]);
        assert_eq!(screen_state(&codex).0, None);
        assert_eq!(seen_state(Some("codex"), &codex), (Some(State::Running), String::new(), 65));
        let gate = v(&["  Hooks need review", "› 1. Review hooks", "  2. Trust all and continue", "  enter confirm · esc skip"]);
        assert_eq!(seen_state(Some("codex"), &gate), (Some(State::Ask), "Hooks need review".into(), 0));
        // Claude 的、认不出是哪家的:原来的读法
        let claude = v(&["· Thinking… (45s · ↑ 1k tokens)", "❯ "]);
        assert_eq!((seen_state(Some("claude"), &claude).0, seen_state(None, &claude).0), (Some(State::Running), Some(State::Running)));
        assert_eq!(seen_state(Some("codex"), &[]), (None, String::new(), 0));
    }

    #[test]
    fn empty_screen() {
        assert_eq!(screen_state(&[]).0, None);
    }

    #[test]
    fn a_split_drops_the_session_ids_but_keeps_the_account() {
        let env = "ACA_SESSION_ID=ab12\nACA_TOOL=claude\nAGENTDECK_INSTANCE_ID=abc\nCLAUDE_CONFIG_DIR=/h/.ct\nCLAUDE_SESSION_ID=1-2\n-SSH_AGENT_PID\nDISPLAY=:0\n";
        assert_eq!(strip_env(env), " -u ACA_SESSION_ID -u AGENTDECK_INSTANCE_ID -u CLAUDE_SESSION_ID");
        assert_eq!(strip_env("DISPLAY=:0\n"), "");
        assert_eq!(strip_env("AGENTDECK_X;rm=1\n"), ""); // 名字怪的不往命令里拼
    }

    /// 一份什么都有的现场:登记的 Claude 会话(在跑,另开了一个窗口)、开成 shell 的(里面手动起了 codex)、
    /// 登记着但 tmux 里没有的(上次出错退的)、一个普通 tmux 会话
    fn scene() -> Vec<Session> {
        let cfg = aca_core::config::parse(CONFIG);
        let sessions = vec![
            store::Session {
                title: "修登录".into(),
                account: "team".into(),
                conv: "c-0".into(),
                dir: "/p/api".into(),
                ..reg("aa11", "claude", "work", 1, 10)
            },
            store::Session { dir: "/q".into(), ..reg("bb22", "shell", "", 1, 20) },
            store::Session { title: "停着的".into(), ..reg("cc33", "codex", "work", 2, 30) },
        ];
        let groups = vec![store::Group { path: "work".into(), order: 1, default_path: String::new() }];
        let panes = vec![
            Pane {
                pid: 100,
                cmd: "2.1.290".into(),
                path: "/p/api/src".into(),
                title: "⠐ 修登录".into(),
                win_activity: 990,
                ..pane("$1", "aa11", true, "%1", 1, 0)
            },
            Pane {
                pid: 110,
                cmd: "zsh".into(),
                path: "/p/api".into(),
                win_name: "zsh".into(),
                win_activity: 800,
                ..pane("$1", "aa11", false, "%2", 2, 0)
            },
            Pane { pid: 200, cmd: "zsh".into(), path: "/q/web".into(), win_activity: 700, ..pane("$2", "bb22", true, "%3", 1, 0) },
            Pane {
                pid: 300,
                cmd: "zsh".into(),
                path: "/tmp".into(),
                session: "scratch".into(),
                activity: 600,
                ..pane("$3", "", false, "%4", 0, 0)
            },
        ];
        let screens = vec![v(&["一屏字"]), vec![], vec![], vec![]];
        let kids: Kids = [(200, vec![(201, "codex".to_string())])].into_iter().collect();
        let run = |id: &str| match id {
            "aa11" => Run { status: "running".into(), turn: 935, conv: "c-1".into(), ..Default::default() },
            "cc33" => Run { status: "error".into(), ts: 500, ..Default::default() },
            _ => Run::default(),
        };
        let home_of = |pid: u32, a: Agent| (pid == 201 && a == Agent::Codex).then(|| "/h/.dd".to_string());
        let inp = Inputs { now: 1000, cfg, sessions, groups, panes, screens, attached: ["$1".to_string()].into(), kids };
        build(inp, &run, &home_of)
    }

    #[test]
    fn groups_without_sessions_get_a_blank_in_their_place() {
        let cfg = aca_core::config::parse(CONFIG);
        let g = |path: &str, order: i64| store::Group { path: path.into(), order, default_path: String::new() };
        // 空的 empty 排在 work 前面;fun 自己没有会话、子分组 fun/app 有:不算空;一个会话都没有的 fun/lab 算
        let groups = vec![g("empty", 1), g("work", 2), g("fun", 3), g("fun/app", 1), g("fun/lab", 2)];
        let sessions = vec![reg("aa11", "claude", "work", 1, 10), reg("bb22", "claude", "fun/app", 1, 20)];
        let panes =
            vec![Pane { pid: 300, cmd: "zsh".into(), path: "/tmp".into(), session: "scratch".into(), ..pane("$3", "", false, "%4", 0, 0) }];
        let inp = Inputs { now: 1000, cfg, sessions, groups, panes, screens: vec![vec![]], attached: HashSet::new(), kids: Kids::new() };
        let out = build(inp, &|_| Run::default(), &|_, _| None);
        let rows: Vec<(&str, bool)> = out.iter().map(|s| (s.group.as_str(), s.blank)).collect();
        // 空组沉在最底下(连普通 tmux 会话也在它们上面),之间照分组的先后
        assert_eq!(rows, [("work", false), ("fun/app", false), ("-", false), ("empty", true), ("fun/lab", true)]);
        let b = out.iter().find(|s| s.blank).unwrap();
        assert_eq!((b.profile.as_str(), b.deck_id.as_str(), b.dir.as_str(), b.sub.is_none()), (REGISTERED, "", "", true));
        // 键和会话的不会撞,各组各一个
        assert_ne!(out[3].name, out[4].name);
        assert!(!out.iter().filter(|s| s.blank).any(|s| s.name == s.group));
    }

    #[test]
    fn the_list_merges_the_registry_tmux_and_what_the_hooks_said() {
        let out = scene();
        let names: Vec<String> = out.iter().map(|s| s.name.replace('\u{1}', "#")).collect();
        // 分组照登记表的先后,没分组的在所有分组后面,普通 tmux 会话沉在最后;另开的窗口跟着自己的会话
        assert_eq!(names, ["aa11", "aa11#%2", "cc33", "bb22", "scratch"]);
        let s = |n: &str| out.iter().find(|s| s.name.replace('\u{1}', "#") == n).unwrap();

        // 登记的、在跑的:键是登记的 id,tmux 里按会话号找;账号是登记的那个;对话 id 以 hook 报的为准
        let a = s("aa11");
        assert_eq!((a.tmux.as_str(), a.pane.as_str(), a.deck_id.as_str(), a.profile.as_str()), ("$1", "%1", "aa11", REGISTERED));
        assert_eq!((a.title.as_str(), a.group.as_str(), a.state, a.ran), ("修登录", "work", State::Running, 65));
        assert_eq!((a.agent, a.who.as_str(), a.tool.as_str(), a.account.as_str(), a.pid), (Agent::Claude, "team", "claude", "team", 100));
        assert_eq!((a.dir.as_str(), a.info.as_str(), a.attached, a.idle_for), ("/p/api", "Path: /p/api\nSession: c-1", true, 10));
        assert_eq!(a.screen, ["一屏字"]);

        // 它里面另开的窗口:自己一行,不是登记的那格(不能重启 / 改名)
        let w = s("aa11#%2");
        assert_eq!(
            (w.title.as_str(), w.state, w.deck_id.as_str(), w.pane.as_str(), w.dir.as_str()),
            ("zsh", State::Idle, "", "%2", "/p/api")
        );
        assert_eq!(w.sub.as_ref().map(|x| (x.parent.as_str(), x.loc())), Some(("aa11", "#2".to_string())));
        assert_eq!(w.family(), "aa11");

        // 登记着、tmux 里没有:列着,上次是出错退的画 ✕
        let c = s("cc33");
        assert_eq!((c.state, c.tmux.as_str(), c.pane.as_str(), c.title.as_str()), (State::Crashed, "", "", "停着的"));
        assert_eq!((c.agent, c.who.as_str(), c.idle_for), (Agent::Codex, "codex", 500));

        // 开成 shell 的:目录跟着 cd 走;里面手动起的 codex 按它进程的配置目录认(没登记的写目录名)
        let b = s("bb22");
        assert_eq!((b.group.as_str(), b.dir.as_str(), b.tool.as_str(), b.state), (UNGROUPED, "/q/web", "", State::Idle));
        assert_eq!((b.agent, b.who.as_str(), b.pid), (Agent::Codex, "dd", 201));

        // 普通 tmux 会话:键是会话名,没有 profile
        let p = s("scratch");
        assert_eq!((p.tmux.as_str(), p.group.as_str(), p.profile.as_str(), p.deck_id.as_str(), p.idle_for), ("$3", "-", "", "", 400));
        assert_eq!((p.title.as_str(), p.slot.0), ("scratch", usize::MAX));
    }

    #[test]
    fn a_stopped_one_counts_from_when_it_was_brought_back() {
        // 两个都停着、tmux 里没有;后一个是刚从回收站拿回来的
        let cfg = aca_core::config::parse(CONFIG);
        let sessions = vec![reg("aa11", "claude", "", 1, 10), reg("bb22", "claude", "", 2, 20)];
        let run = |id: &str| match id {
            "aa11" => Run { status: "ended".into(), ts: 300, ..Default::default() },
            _ => Run { status: "ended".into(), ts: 300, restored: 990, ..Default::default() },
        };
        let inp = Inputs { now: 1000, cfg, sessions, groups: vec![], panes: vec![], screens: vec![], attached: HashSet::new(), kids: Kids::new() };
        let out = build(inp, &run, &|_, _| None);
        let got: Vec<(&str, State, u64)> = out.iter().map(|s| (s.name.as_str(), s.state, s.active_at)).collect();
        assert_eq!(got, [("aa11", State::Stopped, 300), ("bb22", State::Stopped, 990)]);
    }

    #[test]
    fn an_idle_agent_counts_from_its_last_turn_not_from_a_redraw() {
        // 四个都在 995 重画过一下(换了设置、窗口变了大小);hook 报的是很早以前
        let cfg = aca_core::config::parse(CONFIG);
        let sessions = vec![
            reg("aa11", "claude", "", 1, 10),
            reg("bb22", "claude", "", 2, 20),
            reg("cc33", "shell", "", 3, 30),
            reg("dd44", "claude", "", 4, 40),
        ];
        let at = |sid: &str, id: &str| Pane { pid: 0, cmd: "2.1.300".into(), win_activity: 995, ..pane(sid, id, true, sid, 1, 0) };
        let panes = vec![at("$1", "aa11"), at("$2", "bb22"), Pane { cmd: "zsh".into(), ..at("$3", "cc33") }, at("$4", "dd44")];
        let run = |id: &str| match id {
            // 800 开始、900 跑完、960 那次是闲了一分钟的提醒,不算动静
            "aa11" => {
                Run { status: "done".into(), event: "Notification".into(), ts: 960, turn: 800, done: 900, seen: 950, ..Default::default() }
            }
            // 940 开始问你
            "bb22" => Run { status: "waiting".into(), ts: 940, turn: 930, ..Default::default() },
            // shell 会话没有 hook,只能看窗口
            "cc33" => Run { started: 100, ..Default::default() },
            // 在跑:窗口一直在刷,照窗口算
            _ => Run { status: "running".into(), ts: 700, turn: 700, ..Default::default() },
        };
        let ask = v(&[" Do you want to proceed?", " ❯ 1. Yes", "   2. No", "", " Esc to cancel · Tab to amend"]);
        let screens = vec![vec![], ask, vec![], v(&["✻ Pondering… (5m 0s · esc to interrupt)"])];
        let inp = Inputs { now: 1000, cfg, sessions, groups: vec![], panes, screens, attached: HashSet::new(), kids: Kids::new() };
        let out = build(inp, &run, &|_, _| None);
        let got: Vec<(&str, State, u64, u64)> = out.iter().map(|s| (s.name.as_str(), s.state, s.idle_for, s.active_at)).collect();
        assert_eq!(
            got,
            [
                ("aa11", State::Idle, 100, 900),
                ("bb22", State::Ask, 60, 940),
                ("cc33", State::Idle, 5, 995),
                ("dd44", State::Running, 5, 995)
            ]
        );
    }
}
