//! 各家 agent:怎么起、hook 怎么带进去、事件怎么变成状态、屏幕怎么读、标题从哪来。
//!
//! 照 agent-deck 的路数分两层,别处(会话操作、hook 入口、列表)都只认 [`Agent`] 这个接口,不写 `if claude … else codex …`:
//! - **内置**:一家一个文件,实现 [`Agent`],在 [`BUILTIN`] 里登记一行。给有自己的 hook 体系、要拼特殊参数、
//!   要读对话记录的那几家(Claude、Codex)。
//! - **声明式**:config.toml 里写一段 `[tools.<名字>]`(命令、接着对话的参数、屏幕上认状态的正则……),不用写代码,
//!   见 [`declared`]。agent 自己有插件 / 扩展机制的,再写一个小转发,按 hook 协议调 `aca hook <工具名>`,状态就和内置的一样全。
//!
//! hook 协议(内置、声明式都一样):`aca hook <内置那家的名字 | 工具名>`,一个事件一次,标准输入是一个 JSON,
//! 形状照 Claude Code 的 hook(事件名、字段见 [`lifecycle`])。Codex 的 hook 天生就是这个形状。
//!
//! 三条共同的做法:
//! - **认证是 harness 自己的事**。账号只是「起它时多带的那几样」(配置目录、环境变量、参数,见 [`Account`]),
//!   用它自己的词写、只写一处;不写账号 aca 什么都不加,它用它默认的。登录走它自己的命令(`aca run <工具:账号> -- login`,
//!   见 [`direct`])。aca 不存、不同步凭据,也不往它的配置目录里写东西
//! - hook **不装进任何配置目录**,都是启动时带进去(Claude 的 `--settings`,Codex 的 `-c hooks.…`;
//!   要靠文件的用 [`hook_file`],要靠环境变量的用 [`Agent::env`])。几个账号不用各装一遍,不动你自己的 hook;
//!   会话里手动再起一个 agent,它没带这些,也就不会冒名上报。
//! - 命令直接交给 tmux 执行(不套 `bash -c`):窗格的进程就是 agent 自己,停的时候发信号发得到,也没有引号要转义。

mod claude;
pub mod claude_mod;
mod codex;
pub mod declared;
pub mod lifecycle;
mod opencode;
mod pi;
mod shell;

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::{self, Account, Config, Tool};
use crate::paths::{self, Res};
use crate::runtime::Run;
use crate::screen::Seen;
use crate::store::Session;
use crate::tr;

/// hook 处理完要发出去的事件:(名字, 这件事自己的字段)
pub type Events = Vec<(&'static str, Value)>;

#[derive(Clone, Debug, PartialEq)]
pub enum How {
    /// 新的一段对话(对话 id 能自己定的那几家:用登记的那个)
    Fresh,
    /// 接着这段对话
    Resume(String),
    /// 从这段对话分出一段新的
    Fork(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// 开场那句话当启动参数带上了(见 [`Agent::opening`]);false = 没有那句话,或者这一家不认、得起来之后打字进去
    pub said: bool,
}

/// 别的会话发来的消息怎么送进去([`crate::inbox`])
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delivery {
    /// 按门铃:它自己盯着一个文件,一变就跑一个 hook 把消息取走,当成系统提示给模型(Claude)
    Bell,
    /// 排进它自己的队列,它空下来自己开一轮(Codex 的 `codex queue`)
    Queue,
    /// 粘进输入框按回车
    Type,
    /// 不送:只放在收件箱里(shell —— 打进去就成了一条命令)
    Keep,
}

pub trait Agent: Sync {
    /// 内置的:config.toml 里 `kind` 写的那个词;声明式的:工具名。`[accounts.<它>]` 也按这个找
    fn name(&self) -> &str;

    /// 账号的 `home`(配置目录)交给它的哪个环境变量;None = 账号里只能写 env / args
    fn home_var(&self) -> Option<&str> {
        None
    }

    /// 账号没给配置目录、环境里也没有时它用的那个(`~/.claude`);None = 没有配置目录这一说
    fn default_home(&self) -> Option<&str> {
        None
    }

    /// 这段对话在配置目录 `home` 里留下的东西,相对 `home` 的路径,头一个是对话记录本身。
    /// 换账号接着跑([`crate::session::rehome`])时照原样拷到新账号的配置目录。找不到 / 这一家接不了 = 空
    fn conv_files(&self, _home: &Path, _conv: &str) -> Vec<PathBuf> {
        vec![]
    }

    /// 换账号接着跑时,会话自己的参数 `args` 要改成什么,才和原来用的一样(Codex:各账号 config.toml 的默认模型不一样)。
    /// `log` 是这段对话在原账号里的记录,`from` / `to` 是两个账号的配置目录,`fixed` 是工具、新账号里写死的参数。
    /// 返回 (新的参数, 给人看的一句;空 = 没什么要说的)。默认不动
    fn rehome_args(&self, args: &[String], _log: &Path, _from: &Path, _to: &Path, _fixed: &[String]) -> (Vec<String>, String) {
        (args.to_vec(), String::new())
    }

    /// 程序名(工具没自己写 `cmd` 时用)
    fn program(&self) -> String {
        self.name().to_string()
    }

    /// 登记新会话时就定下来的对话 id(能在启动参数里指定的才有);空 = 等 hook 报上来
    fn new_conv(&self) -> String {
        String::new()
    }

    fn can_fork(&self) -> bool {
        false
    }

    /// 要跑的命令:第一个是程序名(还没换成完整路径)。`aca` 是 hook 要回头调的那个程序
    fn argv(&self, tool: &Tool, s: &Session, how: &How, aca: &str) -> Res<Vec<String>>;

    /// 启动时再放进环境的(hook / 扩展要靠环境变量带进去的那几家用,比如 opencode 的 `OPENCODE_CONFIG_CONTENT`)。
    /// `set` 是工具、账号那两层已经要放的(同名的以这里返回的为准:要合进去的自己合)
    fn env(&self, _tool: &Tool, _aca: &str, _set: &[(String, String)]) -> Res<Vec<(String, String)>> {
        Ok(vec![])
    }

    /// 再起的时候接不接得上:`conv` 是现在这段对话的 id
    fn resume(&self, _conv: &str, _r: &Run) -> How {
        How::Fresh
    }

    /// 一个 hook 事件 → 状态怎么变、要发哪些事件。纯函数。默认就是按 hook 协议来
    fn on_hook(&self, r: &mut Run, payload: &Value, now: u64) -> Events {
        lifecycle::apply(r, payload, now)
    }

    /// hook 里要不要每次都看一眼窗格标题(标题只写在那的)
    fn title_in_pane(&self) -> bool {
        false
    }

    /// agent 自己起的标题:`r` 是这次事件之后的状态,`pane_title` 是窗格标题(没查就是空的)。
    /// 默认用 hook 报上来的(协议里的 `title`)
    fn title(&self, r: &Run, _pane_title: &str) -> Option<String> {
        (!r.hook_title.is_empty()).then(|| r.hook_title.clone())
    }

    /// 不翻文件就看得出的标题。列表每次刷新都拿窗格标题问一遍([`crate::status::follow_titles`]):
    /// 没带 hook 的会话(接管了还没重启)标题只有这一条路跟,带了 hook 的也不用等到这一轮跑完。
    /// 默认和 [`Agent::title`] 一样;`title` 里要读文件的那一家自己写一个不读的
    fn live_title(&self, r: &Run, pane_title: &str) -> Option<String> {
        self.title(r, pane_title)
    }

    /// 读屏幕(不带颜色的字)。默认不会读:状态听 hook 的
    fn screen(&self, _lines: &[String]) -> Seen {
        Seen::Unsure
    }

    /// 正文的尾巴(输入框头上那几行)的指纹:和上一次看的比变没变,认「还在往外吐正文」(见 [`crate::status::look`])。
    /// 输入框底下的状态栏不算在里面:它闲着也隔一会儿刷一次。None = 认不出(默认),不看
    fn body(&self, _lines: &[String]) -> Option<u64> {
        None
    }

    /// 别的会话发来的消息怎么送。默认打字进去
    fn delivery(&self) -> Delivery {
        Delivery::Type
    }

    /// [`Delivery::Queue`] 的那一家:把一段话排进对话 `conv` 的队列,接在它的程序后面的参数。
    /// `shared` = 交给它共用的后台服务(那个服务已经在跑时只能这样)
    fn queue_args(&self, _conv: &str, _text: &str, _shared: bool) -> Vec<String> {
        vec![]
    }

    /// 输入框开头的那个记号(Claude 的 ❯、Codex 的 ›):打字之前看输入框里有没有你没发的字。None = 认不出,不看
    fn prompt_mark(&self) -> Option<char> {
        None
    }

    /// 替它回答权限框时按的键:(同意, 拒绝)。None = 没试过,不替它按
    fn permission_keys(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    /// 开场那句话(`aca new --say`)能不能当启动参数带进去:能的话给垫在它前面的参数,那句话接在最后。
    /// 带进去的由 agent 自己拿着过启动时那几道关(信任目录、审 hook),起来了自己发,不用等它画出输入框再往里打字。
    /// None = 这一家不认 / 这句话不能这么带:起来之后打字进去(见 [`crate::session::send_fresh`])
    fn opening(&self, _text: &str) -> Option<Vec<String>> {
        None
    }

    /// 屏幕上是不是它头一回进一个目录时问的「信不信任这个目录」:是的话给选「信任」要按的键。
    /// `[trust] folder = true` 时起会话之后替你按(见 [`crate::trust`])。None = 不是那个框 / 这一家认不出
    fn trust_keys(&self, _lines: &[String]) -> Option<Vec<&'static str>> {
        None
    }

    /// 中止正在跑的这一轮按的键(`aca interrupt`),一个一个按、中间隔一下(opencode 要连按两下 Esc,
    /// 同一下送进去的两个它只当成一个)。None = 没试过,不替它按
    fn interrupt_keys(&self) -> Option<&'static [&'static str]> {
        None
    }

    /// 被中止时它自己报不报 hook(Codex 报 `Interrupt`);不报的,aca 按完自己把它记成停下、发 `turn.interrupted`
    fn reports_interrupt(&self) -> bool {
        false
    }

    /// hook 说它在跑、屏幕上它却停了:是不是这一轮已经结束、又没报 hook(Codex 出错的那种从来不报,
    /// 正常跑完的在 Stop hook 没被信任时也不报)。是的话给一个替它补的事件,hook 协议的形状:
    /// `StopFailure`(带 `error` 和 `error_kind`)或 `Stop`(带 `last_assistant_message`),
    /// 知道是什么时候结束的就带上 `at`。见 [`crate::hook::feed`]
    fn unreported_end(&self, _r: &Run) -> Option<serde_json::Value> {
        None
    }

    /// 对话记录里最后一段回复的原文。hook 记的那份(`Run::last`)没有(接管来还没重启的会话没有 hook)、
    /// 或者被截短了的时候,交给插件的 `input = "reply"` 来这里读。默认读不了
    fn last_reply(&self, _r: &Run) -> Option<String> {
        None
    }

    /// 这个进程名是不是它(认手动起的、接管时用)。默认:名字一样
    fn runs_as(&self, comm: &str) -> bool {
        comm == self.name()
    }

    /// 接管([`crate::adopt`])一个不是 aca 起的、正在跑的进程时问:它现在是哪段对话、带着哪些参数。
    /// `home` 是它环境里的配置目录(None = 它默认的),`argv` 是它的命令行(读不到是空的)。
    /// 默认什么都认不出:对话 id 等它在 aca 下面重启、hook 报上来
    fn live(&self, _pid: u32, _home: Option<&str>, _argv: &[String]) -> Live {
        Live::default()
    }
}

/// 一个正在跑的进程,从它自己留下的东西里认出来的([`Agent::live`])
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// 现在这段对话的 id,和对话记录在哪
    pub conv: String,
    pub transcript: String,
    /// 它自己起的标题
    pub title: String,
    /// 命令行里除了「接哪段对话」之外的参数:接管之后每次启动照带。None = 认不准,不搬(见 [`carry`])
    pub args: Option<Vec<String>>,
}

/// 命令行里能照搬的参数:去掉程序名和 `skip` 说的那些(`skip` 看从这个词开始的一段,返回要丢几个词,0 = 留着)。
/// 只搬开关和单个词的值。里面有一段话(带空白的词、连着两个不带 `-` 的词:多半是起的时候就交给它的那句话)就整个不搬:
/// 认不准,搬错了每次重启都会把那句话再发一遍
pub(crate) fn carry(argv: &[String], skip: impl Fn(&[String]) -> usize) -> Option<Vec<String>> {
    let mut rest = argv.get(1..)?;
    let mut kept: Vec<String> = vec![];
    while let Some(word) = rest.first() {
        let n = skip(rest).min(rest.len());
        if n == 0 {
            kept.push(word.clone());
        }
        rest = &rest[n.max(1)..];
    }
    let flag = |w: &String| w.starts_with('-');
    let plain = kept.iter().all(|w| !w.contains(char::is_whitespace) && !w.is_empty())
        && kept.first().is_none_or(flag)
        && kept.windows(2).all(|w| flag(&w[0]) || flag(&w[1]));
    plain.then_some(kept)
}

/// 一个带值的开关在这开头占几个词:`--x 值` 是 2,`--x=值` 是 1,不是它 = 0
pub(crate) fn valued(rest: &[String], names: &[&str]) -> usize {
    let Some(first) = rest.first() else { return 0 };
    if names.contains(&first.as_str()) {
        return 2;
    }
    usize::from(names.iter().any(|n| first.strip_prefix(n).is_some_and(|v| v.starts_with('='))))
}

/// 值可有可无的开关(`--resume [id]`):后面那个词不带 `-` 才算它的值
pub(crate) fn optional(rest: &[String], names: &[&str]) -> usize {
    match valued(rest, names) {
        2 if rest.get(1).is_none_or(|v| v.starts_with('-')) => 1,
        n => n,
    }
}

/// 内置的几家。**加一家 = 这个目录加一个文件,这里加一行**
static BUILTIN: &[&dyn Agent] = &[&claude::Claude, &codex::Codex, &pi::Pi, &opencode::Opencode, &shell::Shell];

pub fn builtin(kind: &str) -> Option<&'static dyn Agent> {
    BUILTIN.iter().copied().find(|a| a.name() == kind)
}

pub fn kinds() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|a| a.name())
}

/// 进程名对得上的内置的那一家(`claude` / `codex`);桌面 App、别的程序 = None
pub fn by_comm(comm: &str) -> Option<&'static str> {
    BUILTIN.iter().copied().find(|a| a.runs_as(comm)).map(|a| a.name())
}

/// 一个工具对应的 agent:内置的是那一家,声明式的是照配置现做的一个
pub enum Handle {
    Builtin(&'static dyn Agent),
    Declared(declared::Declared),
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::ops::Deref for Handle {
    type Target = dyn Agent;
    fn deref(&self) -> &(dyn Agent + 'static) {
        match self {
            Handle::Builtin(a) => *a,
            Handle::Declared(d) => d,
        }
    }
}

/// 起 agent 前从环境里拿掉的:别的会话(agent-deck 的、外层 Claude / Codex 的)用来认自己的变量。
/// tmux 服务是从某个会话里起的话,这些会跟着进每个新会话,你全局装的 hook 就会把这个会话的事记到那个会话头上
const FOREIGN: &[&str] = &[
    "AGENTDECK_INSTANCE_ID",
    "AGENTDECK_PROFILE",
    "AGENTDECK_ACCOUNT",
    "AGENTDECK_TOOL",
    "AGENTDECK_RESOLVED_CONFIG_DIR",
    "AGENTDECK_RESOLVED_GROUP",
    "AGENTDECK_RESOLVED_SOURCE",
    "CLAUDE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_EFFORT",
    "CLAUDE_JOB_DIR",
    "CLAUDE_PID",
    // 这一个是串号的:Claude 带给它的子进程,说「登录凭据去这个配置目录名下取」,比 CLAUDE_CONFIG_DIR 优先。
    // 留着的话,用别的账号起的会话,目录是那个账号的,登录却是外层那个账号的(2.1.288 上实测:空目录都显示登录了)
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    // 这一个也是串号的:指名用哪个 Anthropic profile(`~/.config/anthropic` 里的 Console / 联合身份登录),排在配置目录自己的登录前面。
    // 留着的话,从带着它的账号里起别的账号,用的是那个 profile 的登录(2.1.296 上实测:空目录报成 profile 的组织和邮箱)
    "ANTHROPIC_PROFILE",
    "CODEX_THREAD_ID",
];

/// [`direct`] 再多拿掉的:在某个 aca 会话里跑的话,这两个会让它的转发把事报到那个会话头上
const OURS: &[&str] = &["ACA_SESSION_ID", "ACA_TOOL"];

/// 前面套一层 env 把这些变量拿掉(工具 / 账号里自己写了的不拿:那是你要的)。env 换成命令自己,进程不变
fn without(vars: &[&[&str]], set: &[(String, String)], mut cmd: Vec<String>) -> Vec<String> {
    let mut argv: Vec<String> = vec!["/usr/bin/env".into()];
    let gone = vars.iter().flat_map(|v| v.iter()).filter(|k| !set.iter().any(|(name, _)| name == *k));
    argv.extend(gone.flat_map(|k| ["-u".to_string(), k.to_string()]));
    argv.append(&mut cmd);
    argv
}

/// hook 的命令是交给 shell 跑的:程序路径里有空格、引号也不怕
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 在 PATH 里找到命令的完整路径:tmux 服务的 PATH 不一定和现在这个终端一样
fn which(cmd: &str) -> Option<String> {
    if cmd.contains('/') {
        return Some(paths::expand(cmd));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|f| f.is_file()).map(|f| f.to_string_lossy().into_owned())
}

pub fn self_exe() -> String {
    self_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "aca".into())
}

/// 自己这个程序的路径。写进 hook、状态栏、tmux 设置,^L 重启也 exec 它:升级以后要指得到新装的那个
pub fn self_path() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|p| stable_exe(&p))
}

/// - Linux 读的是 /proc/self/exe:程序文件被换掉(升级)以后,后面多出 " (deleted)"。去掉,就是原地装上的新版
/// - Homebrew 装的:Linux 上读到的是带版本号的 <前缀>/Cellar/aca/<版本>/bin/aca(macOS 读到的是 bin/aca 那个链接),
///   升级时旧版会被清掉。换成不带版本号、brew 一直指着现在那版的 <前缀>/opt/aca/bin/aca
fn stable_exe(p: &Path) -> PathBuf {
    let p = match p.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) if !p.exists() => PathBuf::from(s),
        _ => p.to_path_buf(),
    };
    let parts: Vec<_> = p.components().collect();
    let Some(i) = parts.iter().rposition(|c| c.as_os_str() == "Cellar").filter(|i| parts.len() > i + 3) else { return p };
    let opt = parts[..i].iter().collect::<PathBuf>().join("opt").join(parts[i + 1]).join(parts[i + 3..].iter().collect::<PathBuf>());
    if opt.exists() { opt } else { p }
}

/// 工具名 → (agent, 工具的定义)。hook 入口拿到的是内置那家的名字(`aca hook claude`)或者工具名,也走这里
pub fn resolve(cfg: &Config, name: &str) -> Res<(Handle, Tool)> {
    let tool = cfg.tool(name).ok_or_else(|| tr!("没有 {name} 这个工具(有:{})", "no tool {name} (there are: {})", cfg.names().join(" ")))?;
    let agent = match tool.kind.as_str() {
        "" => Handle::Declared(declared::Declared::new(name, &tool)?),
        kind => Handle::Builtin(builtin(kind).ok_or_else(|| {
            tr!(
                "工具 {name} 的 kind 写的是 {kind},内置的只有 {};不写 kind 就是声明式工具",
                "tool {name} has kind {kind}, but the built-ins are only {}; leave kind out for a declared tool",
                kinds().collect::<Vec<_>>().join(" / ")
            )
        })?),
    };
    Ok((agent, tool))
}

/// 会话的工具是哪一家
pub fn of(cfg: &Config, s: &Session) -> Res<(Handle, Tool)> {
    resolve(cfg, &s.tool)
}

/// 账号写的是不是一个目录(`~/.x`、`/abs`、`./rel`):不用登记,直接当配置目录
pub fn is_dir(account: &str) -> bool {
    account.starts_with(['~', '/', '.'])
}

/// 列表里显示的账号:登记过的是名字,直接写目录的是目录名
pub fn account_label(account: &str) -> &str {
    if is_dir(account) { account.rsplit('/').find(|p| !p.is_empty()).unwrap_or(account) } else { account }
}

/// 会话的账号那一层。没写账号 = 空的一层:aca 什么都不加,harness 用它自己默认的
pub fn account(cfg: &Config, agent: &dyn Agent, account: &str) -> Res<Account> {
    let layer = match account {
        "" => return Ok(Account::default()),
        dir if is_dir(dir) => Account { home: config::dir(dir), ..Default::default() },
        name => cfg.account(agent.name(), name).cloned().ok_or_else(|| {
            tr!(
                "config.toml 的 [accounts.{}] 里没有 {name}(没登记的可以直接写目录:{}:~/.xxx)",
                "no {name} under [accounts.{}] in config.toml (unregistered ones can be a directory: {}:~/.xxx)",
                agent.name(),
                agent.name()
            )
        })?,
    };
    if !layer.home.is_empty() && agent.home_var().is_none() {
        return Err(tr!(
            "{} 没说配置目录交给哪个环境变量(声明式工具写 home_var);不写的话账号里只能用 env / args",
            "{} doesn't say which environment variable gets the config directory (declared tools set home_var); without it accounts can only use env / args",
            agent.name()
        ));
    }
    Ok(layer)
}

/// 「工具[:账号]」认出来的:工具名、账号(名字,或者目录的完整路径)、哪一家、工具的定义
pub struct Pick {
    pub tool: String,
    pub account: String,
    pub agent: Handle,
    pub def: Tool,
}

/// 「claude:sub」「codex:~/.x」「codex」。没写账号用工具自己的默认;写成目录的换成完整路径,
/// 正好是某个登记过的账号(只换目录的那种)就记它的名字:同一个账号在列表里只有一个叫法
pub fn pick(cfg: &Config, spec: &str) -> Res<Pick> {
    let (name, account) = spec.split_once(':').unwrap_or((spec, ""));
    let (agent, def) = resolve(cfg, name)?;
    let mut account = if account.is_empty() { def.account.clone() } else { account.to_string() };
    if is_dir(&account) {
        let dir = std::path::absolute(config::dir(&account)).map_err(|e| format!("{account}:{e}"))?.to_string_lossy().into_owned();
        let plain = Account { home: dir.clone(), ..Default::default() };
        let known = cfg.accounts.get(agent.name()).and_then(|all| all.iter().find(|(_, a)| **a == plain));
        account = known.map(|(name, _)| name.clone()).unwrap_or(dir);
    }
    self::account(cfg, &*agent, &account)?;
    Ok(Pick { tool: name.to_string(), account, agent, def })
}

/// `.env` 那种写法:一行一个 `KEY=值`,`#` 开头的是注释,前面可以有 `export `,值两边的引号去掉。
/// 写得不对的那一行的行号放在 Err 里(不带内容:里面是 key)
fn parse_env(text: &str) -> Result<Vec<(String, String)>, usize> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.strip_prefix("export ").unwrap_or(line).split_once('=').ok_or(i + 1)?;
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() || k.contains(char::is_whitespace) {
            return Err(i + 1);
        }
        let quoted = ['"', '\''].iter().find_map(|q| v.strip_prefix(*q)?.strip_suffix(*q));
        // 没加引号的值,空格加 # 后面是注释
        let v = quoted.unwrap_or_else(|| v.split(" #").next().unwrap_or(v).trim_end());
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// 工具和账号这两层的环境变量(后面的盖前面的):工具的 env、账号 env_file 里的、账号的 env、配置目录
fn layered_env(agent: &dyn Agent, tool: &Tool, a: &Account) -> Res<Vec<(String, String)>> {
    let own = |m: &std::collections::BTreeMap<String, String>| m.iter().map(|(k, v)| (k.clone(), paths::expand(v))).collect::<Vec<_>>();
    let mut env = own(&tool.env);
    if !a.env_file.is_empty() {
        let text = std::fs::read_to_string(&a.env_file)
            .map_err(|e| tr!("账号的 env_file 读不了 {}:{e}", "can't read the account's env_file {}: {e}", a.env_file))?;
        env.extend(parse_env(&text).map_err(|n| tr!("{} 第 {n} 行不是 KEY=值", "{} line {n} isn't KEY=value", a.env_file))?);
    }
    env.extend(own(&a.env));
    if let (Some(var), false) = (agent.home_var(), a.home.is_empty()) {
        env.push((var.to_string(), a.home.clone()));
    }
    Ok(env)
}

/// 这个工具、账号实际用的配置目录:账号那一层给的(配置目录,或者 env / env_file 里写的那个变量),没有就是环境里的,
/// 再没有是它默认的([`Agent::default_home`])。None = 这一家没有配置目录这一说
pub fn home(cfg: &Config, tool: &str, account: &str) -> Res<Option<PathBuf>> {
    let (agent, def) = resolve(cfg, tool)?;
    let Some(var) = agent.home_var() else { return Ok(None) };
    let layer = self::account(cfg, &*agent, account)?;
    let env = layered_env(&*agent, &def, &layer)?;
    let set = env.into_iter().rev().find(|(k, _)| k == var).map(|(_, v)| v);
    let dir = set.or_else(|| std::env::var(var).ok().filter(|v| !v.is_empty())).or_else(|| agent.default_home().map(String::from));
    Ok(dir.map(|d| PathBuf::from(paths::expand(&d))))
}

/// 这个 Codex 用哪个配置目录:账号给的 `CODEX_HOME`,没有就是环境里的,再没有是 `~/.codex`
fn codex_home(env: &[(String, String)]) -> std::path::PathBuf {
    let from_env = env.iter().rev().find(|(k, _)| k == "CODEX_HOME").map(|(_, v)| v.clone());
    let home = from_env.or_else(|| std::env::var("CODEX_HOME").ok().filter(|v| !v.is_empty()));
    home.map(std::path::PathBuf::from).unwrap_or_else(|| paths::home().join(".codex"))
}

/// 文件末尾这么多字节,从下一个整行开始(读对话记录的最后几段)
pub(crate) fn tail(path: &str, n: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let start = f.metadata().ok()?.len().saturating_sub(n);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if start == 0 { text } else { text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default() })
}

/// hook 要用的文件(Claude 的 settings、扩展脚本……)放在 `state/hooks/<名字>`:写好(内容没变就不动),返回路径
pub(crate) fn hook_file(name: &str, content: &str) -> Res<PathBuf> {
    let path = paths::state_dir().join("hooks").join(name);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
        paths::write_atomic(&path, content).map_err(|e| tr!("写不了 {}:{e}", "can't write {}: {e}", path.display()))?;
    }
    Ok(path)
}

/// 这个会话这次怎么起:工具上面叠账号那一层,命令换成完整路径、前面套一层 env 拿掉别的会话的身份,环境里放上会话 id
///
/// `say` = 开场那句话:这一家能当启动参数带的([`Agent::opening`])写进一个文件,命令前面套一层
/// `aca exec-saying <文件>`,由它读出来接在最后([`exec_saying`]);不能带的 `Launch::said` 是 false
pub fn launch(cfg: &Config, s: &Session, how: &How, say: Option<&str>) -> Res<Launch> {
    let (agent, mut tool) = of(cfg, s)?;
    let layer = account(cfg, &*agent, &s.account)?;
    // 账号的参数接在工具的后面,各家拼命令时一起带上([`extra`])
    tool.args.extend(layer.args.iter().cloned());
    // 先放配置里写的,aca 自己的放后面:同名的以后面为准
    let mut env = layered_env(&*agent, &tool, &layer)?;
    // 开着的插件给它的(插件目录、MCP 服务;Codex 还要备好它认得的副本)
    let codex_home = (agent.name() == "codex").then(|| codex_home(&env));
    let m = crate::plugin::mount::plan(agent.name(), &s.tool, &s.account, codex_home.as_deref());
    (tool.plugin_dirs, tool.mcp, tool.codex_plugins) = (m.dirs, m.mcp, m.codex_plugins);
    // 记下这次带了什么:插件变了,插件页据此说哪些开着的会话要重启
    let sig = crate::plugin::mount::signature(agent.name(), &s.tool, &s.account);
    if crate::runtime::load(&s.id).mounts != sig {
        crate::runtime::update(&s.id, |r| r.mounts = sig);
    }
    let aca = self_exe();
    let mut cmd = agent.argv(&tool, s, how, &aca)?;
    cmd[0] = which(&cmd[0]).ok_or_else(|| tr!("PATH 里没有 {}", "{} isn't on PATH", cmd[0]))?;
    let lead = say.and_then(|text| Some((agent.opening(text)?, text)));
    let said = lead.is_some();
    if let Some((lead, text)) = lead {
        let file = opening_file(&s.id, text)?;
        cmd = [aca.clone(), "exec-saying".to_string(), file].into_iter().chain(cmd).chain(lead).collect();
    }
    let more = agent.env(&tool, &aca, &env)?;
    env.extend(more);
    env.extend([("ACA_SESSION_ID".to_string(), s.id.clone()), ("ACA_TOOL".to_string(), s.tool.clone()), ("ACA_BIN".to_string(), aca)]);
    // 换了目录 / 换了 tmux 服务的(测试),会话里的 hook 也得写到同一个地方
    for k in ["ACA_HOME", "ACA_TMUX_SOCKET"] {
        if let Some(v) = std::env::var(k).ok().filter(|v| !v.is_empty()) {
            env.push((k.to_string(), v));
        }
    }
    Ok(Launch { argv: without(&[FOREIGN], &env, cmd), env, said })
}

/// 开场那句话最长这么多字节:它最后是命令行上的一个参数,系统给一条命令的参数加环境一共 1 MB(macOS)
pub const OPENING_MAX: usize = 200_000;

/// Claude、Codex 共用的 [`Agent::opening`]:那句话接在 `--` 后面,横线开头的也不会被当成开关。
/// 只有一个英文词的不带:它会被当成子命令(`claude doctor`、`codex resume`),那样的起来之后打字进去
pub(crate) fn opening_after_dashes(text: &str) -> Option<Vec<String>> {
    let word = text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    (!word).then(|| vec!["--".to_string()])
}

/// 开场那句话放在 `state/opening/<会话 id>`,只给自己读,[`exec_saying`] 读完就删。
/// 不直接写在命令里:命令要经过 tmux,它一条命令最多十几 KB,一份 brief 放不进去
fn opening_file(id: &str, text: &str) -> Res<String> {
    use std::os::unix::fs::PermissionsExt;
    let path = paths::state_dir().join("opening").join(id);
    paths::write_atomic(&path, text).map_err(|e| tr!("写不了 {}:{e}", "can't write {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(path.to_string_lossy().into_owned())
}

/// `aca exec-saying <文件> <命令…>`:换成那条命令,文件里的话接在最后当一个参数。回来了就是没换成。
/// 文件没了(窗格里同一条命令被再跑了一遍)就不带那句话,照样起
pub fn exec_saying(file: &str, argv: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let text = std::fs::read_to_string(file).ok();
    let _ = std::fs::remove_file(file);
    let Some((program, rest)) = argv.split_first() else { return std::io::Error::other("no command") };
    std::process::Command::new(program).args(rest).args(text).exec()
}

/// `aca run`:不建会话,原样跑这个工具自己的命令(`login`、`mcp`、`doctor`……),只带上账号那一层的**环境** ——
/// 和你在终端里敲 `CODEX_HOME=~/.codex2 codex login` 是一回事,只是不用再把目录敲一遍。
/// 参数只有你给的:工具和账号的 `args` 是起会话用的,管理命令不一定认(Codex 的 `login` 带上 `--profile` 直接报错),
/// 要带自己写在后面。hook 也不带
pub fn direct(cfg: &Config, spec: &str, args: &[String]) -> Res<Launch> {
    let p = pick(cfg, spec)?;
    let mut cmd = base(&p.def, &p.agent.program());
    cmd[0] = which(&cmd[0]).ok_or_else(|| tr!("PATH 里没有 {}", "{} isn't on PATH", cmd[0]))?;
    cmd.extend(args.iter().cloned());
    let mut env = account_env(cfg, spec)?;
    let argv = without(&[FOREIGN, OURS], &env, cmd);
    let asked = std::env::var_os(LOGIN_LINK).is_some();
    let theirs = std::env::var_os("BROWSER").is_some_and(|b| !b.is_empty());
    let argv = quiet_login(p.agent.name(), args, Seat { asked, theirs, remote: remote(cfg) }, &mut env, argv);
    Ok(Launch { argv, env, said: false })
}

/// 人不在这台机器跟前:`[login] remote` 写了 yes / no 就照它;不写看自己:从 SSH 连进来(和 `aca` 复制时的判断一样),
/// 或者 Linux 上没有图形界面。这时登录链接要能在别的设备上打开:回调本机的那种登不完(见 [`quiet_login`])
pub fn remote(cfg: &Config) -> bool {
    match cfg.login.remote {
        config::Remote::Yes => true,
        config::Remote::No => false,
        config::Remote::Auto => {
            let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
            has("SSH_CONNECTION") || has("SSH_TTY") || (cfg!(target_os = "linux") && !has("DISPLAY") && !has("WAYLAND_DISPLAY"))
        }
    }
}

/// 起登录时的处境:asked = aca 界面起的(带着 [`LOGIN_LINK`]);theirs = 外面设了 BROWSER;remote 见 [`remote`]
#[derive(Clone, Copy, Default)]
struct Seat {
    asked: bool,
    theirs: bool,
    remote: bool,
}

/// aca 界面起登录时带的:指着一个文件,工具要开的登录链接写进去(见 [`login_link`]),界面看到了弹出来挑浏览器
pub const LOGIN_LINK: &str = "ACA_LOGIN_LINK";

/// Codex 在 macOS 上开链接只走系统的 open、不认 `$BROWSER`:放进沙箱里跑,只拦「打开别的程序」这一样(本机回调照样收得到)
const SANDBOX: &str = "/usr/bin/sandbox-exec";
const NO_OPEN: &str = "(version 1) (allow default) (deny lsopen)";

/// 工具自己的浏览器登录:`claude auth login`、`codex login`(带 key / token、设备码、查状态、看帮助的不算)
fn browser_login(family: &str, args: &[String]) -> bool {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let help = |rest: &[&str]| rest.iter().any(|x| matches!(*x, "-h" | "--help" | "help"));
    match family {
        "claude" => a.starts_with(&["auth", "login"]) && !help(&a[2..]),
        "codex" => {
            a.first() == Some(&"login")
                && !help(&a[1..])
                && !a[1..].iter().any(|x| matches!(*x, "status" | "--with-api-key" | "--with-access-token" | "--device-auth"))
        }
        _ => false,
    }
}

/// 登录不替你开默认浏览器(2026-10-14 他说的:不一定用默认浏览器登这个账号,自己跳过去很麻烦)。
/// 链接两家都会自己打在屏幕上;Claude 开链接用 `$BROWSER`,指回 aca 自己(见 [`login_link`]);Codex 不认它,放进沙箱里拦住。
/// aca 界面起的(asked):跑的是什么都这么办(新账号起的是工具本身),外面设的 BROWSER 也盖掉,链接要交给界面;
/// 自己敲的只管登录命令,外面设了 BROWSER(theirs)就用你的。账号里写了 BROWSER 的一律照账号的。
/// 远程(他挑的,2026-10-14):换成在别的设备上登得完的 —— Claude 的 BROWSER 设成 `true`,它什么都不开、
/// 屏幕上给要把码贴回来的那条链接;Codex 加 `--device-auth`(一个固定链接 + 一次性码),不回调本机
fn quiet_login(family: &str, args: &[String], seat: Seat, env: &mut Vec<(String, String)>, mut argv: Vec<String>) -> Vec<String> {
    let login = browser_login(family, args);
    if !seat.asked && !login {
        return argv;
    }
    if !env.iter().any(|(k, _)| k == "BROWSER") && (seat.asked || !seat.theirs) {
        let opener = if seat.remote { "true".to_string() } else { self_exe() };
        env.push(("BROWSER".into(), opener));
    }
    if family == "codex" && seat.remote && login {
        argv.push("--device-auth".into());
    }
    if family == "codex" && cfg!(target_os = "macos") && std::path::Path::new(SANDBOX).exists() {
        return [SANDBOX.to_string(), "-p".into(), NO_OPEN.into()].into_iter().chain(argv).collect();
    }
    argv
}

/// `aca <链接>`:登录时工具拿 aca 当浏览器(见 [`quiet_login`])。aca 界面要链接([`LOGIN_LINK`] 指着文件)就写给它;
/// 不然什么都不开 —— 链接工具自己打在屏幕上了。都退出 0:工具当成开过了,接着等你在浏览器里登完
pub fn login_link(url: &str) -> i32 {
    hand_link(std::env::var_os(LOGIN_LINK).map(PathBuf::from), url);
    0
}

fn hand_link(to: Option<PathBuf>, url: &str) {
    let Some(f) = to else {
        return eprintln!("{}", tr!("aca 不替登录开浏览器,链接:{url}", "aca doesn't open a browser for logins; the link: {url}"));
    };
    // 先写到旁边再改名:界面不会读到写了一半的
    let part = f.with_extension("part");
    if let Err(e) = std::fs::write(&part, url).and_then(|()| std::fs::rename(&part, &f)) {
        eprintln!("{}", tr!("登录链接交不给 aca 界面({}):{e}", "couldn't hand the login link to aca's UI ({}): {e}", f.display()));
    }
}

/// 「工具:账号」那一层的环境(工具的 env、env_file、账号的 env、配置目录),不带会话的身份。
/// `aca run` 带的就是这些;界面在会话旁边另开的 shell 也带这些(会话的 tmux 环境是空的,见 `session::launch`)
pub fn account_env(cfg: &Config, spec: &str) -> Res<Vec<(String, String)>> {
    let p = pick(cfg, spec)?;
    let layer = account(cfg, &*p.agent, &p.account)?;
    layered_env(&*p.agent, &p.def, &layer)
}

/// 用这个会话的工具和账号跑它自己的一条命令(Codex 的 `queue`):和 [`direct`] 一样只带账号那一层的环境,
/// 参数只有给的这些,会话的身份不带
pub fn tool_cmd(cfg: &Config, s: &Session, args: &[String]) -> Res<Launch> {
    let (agent, tool) = of(cfg, s)?;
    let layer = account(cfg, &*agent, &s.account)?;
    let mut cmd = base(&tool, &agent.program());
    cmd[0] = which(&cmd[0]).ok_or_else(|| tr!("PATH 里没有 {}", "{} isn't on PATH", cmd[0]))?;
    cmd.extend(args.iter().cloned());
    let env = layered_env(&*agent, &tool, &layer)?;
    Ok(Launch { argv: without(&[FOREIGN, OURS], &env, cmd), env, said: false })
}

/// 命令的公共部分:工具自己定了命令就用它的,否则是这一家的程序名
pub(crate) fn base(tool: &Tool, program: &str) -> Vec<String> {
    if tool.cmd.is_empty() { vec![program.to_string()] } else { tool.cmd.clone() }
}

/// 每次启动都带上的参数:工具的(账号的已经接在它后面了),再是这个会话自己的
pub(crate) fn extra(tool: &Tool, s: &Session) -> Vec<String> {
    tool.args.iter().chain(&s.args).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(cfg: &Config, spec: &str) -> Res<(String, String, String)> {
        pick(cfg, spec).map(|p| (p.tool, p.account, p.agent.name().to_string()))
    }

    #[test]
    fn a_tool_spec_names_its_account_or_takes_the_tool_default() {
        let cfg = config::parse("[accounts.claude]\nsub = \"/h/.cs\"\nfirm = \"/h/.cd\"\n[tools.claude]\naccount = \"sub\"\n");
        assert_eq!(picked(&cfg, "claude").unwrap(), ("claude".into(), "sub".into(), "claude".into()));
        assert_eq!(picked(&cfg, "claude:firm").unwrap().1, "firm");
        assert_eq!(picked(&cfg, "codex").unwrap(), ("codex".into(), String::new(), "codex".into()));
        assert!(picked(&cfg, "claude:nope").unwrap_err().contains("nope"));
        assert!(picked(&cfg, "gemini").unwrap_err().contains("claude:sub"));
    }

    #[test]
    fn an_account_can_be_a_directory_nobody_registered() {
        let cfg =
            config::parse("[accounts.codex]\ncorp = \"/h/.corp-cx\"\nlabs = { home = \"/h/.labs\", args = [\"--profile\", \"labs\"] }\n");
        // 没登记的目录:记完整路径,列表里显示目录名
        let (_, account, _) = picked(&cfg, "codex:/h/.newcodex/").unwrap();
        assert_eq!((account.as_str(), account_label(&account)), ("/h/.newcodex", ".newcodex"));
        assert_eq!(picked(&cfg, "codex:~/.x").unwrap().1, format!("{}/.x", paths::expand("~")));
        let here = std::env::current_dir().unwrap();
        assert_eq!(picked(&cfg, "codex:./cx").unwrap().1, here.join("cx").to_string_lossy());
        // 正好是登记过的、只换目录的账号:记它的名字
        assert_eq!(picked(&cfg, "codex:/h/.corp-cx").unwrap().1, "corp");
        // 目录一样但那个账号还带着别的(参数):不是一回事,不冒名
        assert_eq!(picked(&cfg, "codex:/h/.labs").unwrap().1, "/h/.labs");
        assert_eq!(account_label("labs"), "labs");
    }

    #[test]
    fn a_declared_tool_takes_a_home_only_when_it_says_where_it_goes() {
        let cfg = config::parse(
            "[accounts.toy]\nmain = \"/h/.toy\"\n[accounts.bare]\nmain = \"/h/.bare\"\nkey = { env = { BARE_KEY = \"k\" }, args = [\"--fast\"] }\n\
             [tools.toy]\nhome_var = \"TOY_HOME\"\n[tools.bare]\n[tools.typo]\nkind = \"cluade\"\n",
        );
        let p = pick(&cfg, "toy:main").unwrap();
        assert_eq!((p.tool.as_str(), p.account.as_str(), p.agent.name(), p.agent.home_var()), ("toy", "main", "toy", Some("TOY_HOME")));
        assert!(pick(&cfg, "bare").is_ok());
        assert!(picked(&cfg, "bare:main").unwrap_err().contains("home_var"));
        assert!(picked(&cfg, "bare:/h/.x").unwrap_err().contains("home_var"));
        // 不换目录的账号(只有 env / args)不用 home_var
        assert_eq!(picked(&cfg, "bare:key").unwrap().1, "key");
        // kind 写错了不悄悄变成声明式的
        assert!(picked(&cfg, "typo").unwrap_err().contains("cluade"));
    }

    #[test]
    fn the_opening_message_rides_after_two_dashes_unless_it_could_be_a_subcommand() {
        let dashes = Some(vec!["--".to_string()]);
        assert_eq!(opening_after_dashes("登录页 500 了,看一下"), dashes);
        // 横线开头的、多行的照样带:`--` 后面的不当开关
        assert_eq!(opening_after_dashes("-x 开头"), dashes);
        assert_eq!(opening_after_dashes("# Task\n\n- do it"), dashes);
        assert_eq!(opening_after_dashes("fix the login page"), dashes);
        // 单独一个英文词会被当成子命令(claude doctor、codex resume):不带,起来之后打字
        for word in ["resume", "doctor", "mcp", "hi", "dry-run"] {
            assert_eq!(opening_after_dashes(word), None, "{word}");
        }
        assert_eq!(opening_after_dashes("继续"), dashes);
    }

    #[test]
    fn the_account_layer_goes_on_top_of_the_tool_at_launch() {
        let dir = std::env::temp_dir().join(format!("aca-layer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("k.env");
        std::fs::write(&file, "# 公司的 key\nexport TOY_KEY=\"from file\"\nTOY_URL=https://x # 走代理\nTOY_MODE='a b'\n").unwrap();
        let cfg = config::parse(&format!(
            "[tools.toy]\ncmd = [\"/bin/sh\", \"toy.sh\"]\nhome_var = \"TOY_HOME\"\nargs = [\"--yolo\"]\nenv = {{ TOY_URL = \"tool\", TOY_KEEP = \"1\" }}\n\
             [accounts.toy]\nwork = {{ home = \"/h/.toy-work\", env_file = \"{}\", env = {{ TOY_MODE = \"inline\" }}, args = [\"--profile\", \"w\"] }}\n",
            file.display()
        ));
        let s =
            Session { id: "ab12".into(), tool: "toy".into(), account: "work".into(), args: vec!["--mine".into()], ..Default::default() };
        let l = launch(&cfg, &s, &How::Fresh, None).unwrap();
        // 参数:工具的、账号的、会话自己的
        assert_eq!(l.argv[l.argv.len() - 6..], ["/bin/sh", "toy.sh", "--yolo", "--profile", "w", "--mine"]);
        // 环境:同名的后面盖前面,最后一个算数
        let last = |k: &str| l.env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(
            (last("TOY_KEEP"), last("TOY_KEY"), last("TOY_URL"), last("TOY_MODE"), last("TOY_HOME")),
            (Some("1"), Some("from file"), Some("https://x"), Some("inline"), Some("/h/.toy-work"))
        );
        assert_eq!((last("ACA_SESSION_ID"), last("ACA_TOOL")), (Some("ab12"), Some("toy")));

        // 不写账号:aca 一样都不加
        let bare = launch(&cfg, &Session { account: String::new(), ..s.clone() }, &How::Fresh, None).unwrap();
        assert!(bare.env.iter().all(|(k, _)| !matches!(k.as_str(), "TOY_HOME" | "TOY_KEY" | "TOY_MODE")));
        // 没登记、直接写目录的:只有配置目录
        let adhoc = launch(&cfg, &Session { account: "/h/.toy-new".into(), ..s.clone() }, &How::Fresh, None).unwrap();
        assert_eq!(adhoc.env.iter().find(|(k, _)| k == "TOY_HOME").map(|(_, v)| v.as_str()), Some("/h/.toy-new"));
        assert_eq!(adhoc.argv[adhoc.argv.len() - 4..], ["/bin/sh", "toy.sh", "--yolo", "--mine"]);

        // 开场那句话:声明式工具不认它当启动参数,命令原样,由起它的那一头等它起来再打字
        let told = launch(&cfg, &s, &How::Fresh, Some("先看一下 README")).unwrap();
        assert_eq!((told.said, &told.argv), (false, &l.argv));

        // `aca run`:工具的程序 + 你给的参数,账号只带环境;起会话用的参数、会话的身份都不带
        let d = direct(&cfg, "toy:work", &["login".to_string()]).unwrap();
        assert_eq!(d.argv[d.argv.len() - 4..], ["ACA_TOOL", "/bin/sh", "toy.sh", "login"]);
        assert!(d.argv.windows(2).any(|w| w == ["-u", "ACA_SESSION_ID"]) && d.argv.windows(2).any(|w| w == ["-u", "CLAUDECODE"]));
        assert!(d.env.iter().any(|(k, v)| k == "TOY_HOME" && v == "/h/.toy-work") && d.env.iter().all(|(k, _)| !k.starts_with("ACA_")));

        // env_file 没了 / 写坏了:说清楚是哪,不把内容带出来
        std::fs::write(&file, "TOY_KEY=k\n这行不对 sk-secret\n").unwrap();
        let err = launch(&cfg, &s, &How::Fresh, None).unwrap_err();
        assert!(err.contains("第 2 行") && !err.contains("sk-secret"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(launch(&cfg, &s, &How::Fresh, None).unwrap_err().contains("env_file"));
    }

    #[test]
    fn the_outer_sessions_login_is_stripped_unless_the_account_asks_for_it() {
        let cfg = config::parse(
            "[tools.claude]\ncmd = [\"/bin/sh\"]\n[accounts.claude]\nfirm = \"/h/.cd\"\n\
             shared = { home = \"/h/.cd\", env = { CLAUDE_SECURESTORAGE_CONFIG_DIR = \"/h/.c\" } }\n\
             console = { env = { ANTHROPIC_PROFILE = \"work\" } }\n",
        );
        let unset = |spec: &str, var: &str| direct(&cfg, spec, &[]).unwrap().argv.windows(2).any(|w| w == ["-u", var]);
        // 外层 Claude 留在环境里的「凭据去哪取」:不拿掉,firm 的会话用的就是外层账号的登录
        assert!(unset("claude:firm", "CLAUDE_SECURESTORAGE_CONFIG_DIR") && unset("claude", "CLAUDE_SECURESTORAGE_CONFIG_DIR"));
        // 账号里自己写了的,是你要的,不拿
        assert!(!unset("claude:shared", "CLAUDE_SECURESTORAGE_CONFIG_DIR") && unset("claude:shared", "CLAUDECODE"));
        // 外层指名的 profile 也一样:不拿掉,firm 用的是那个 profile 的登录;账号自己指名的留着
        assert!(unset("claude:firm", "ANTHROPIC_PROFILE") && unset("claude", "ANTHROPIC_PROFILE"));
        assert!(!unset("claude:console", "ANTHROPIC_PROFILE") && unset("claude:console", "CLAUDE_SECURESTORAGE_CONFIG_DIR"));
    }

    #[test]
    fn logins_aca_runs_dont_open_the_default_browser() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // 认得出两家的浏览器登录;带 key、设备码、查状态、看帮助的不算
        assert!(browser_login("claude", &s(&["auth", "login"])) && browser_login("claude", &s(&["auth", "login", "--console"])));
        assert!(!browser_login("claude", &s(&["auth", "status"])) && !browser_login("claude", &s(&["auth", "login", "-h"])));
        assert!(!browser_login("claude", &[]) && !browser_login("toy", &s(&["login"])));
        assert!(browser_login("codex", &s(&["login"])) && !browser_login("codex", &s(&["login", "status"])));
        assert!(!browser_login("codex", &s(&["login", "--with-api-key"])) && !browser_login("codex", &s(&["login", "--device-auth"])));

        let login = s(&["auth", "login"]);
        let argv = s(&["/usr/bin/env", "-u", "CLAUDECODE", "/bin/claude", "auth", "login"]);
        let here = Seat::default();
        let run = |args: &[String], seat: Seat, env: &[(&str, &str)]| {
            let mut env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let out = quiet_login("claude", args, seat, &mut env, argv.clone());
            (out, env.into_iter().find(|(k, _)| k == "BROWSER").map(|(_, v)| v))
        };
        // 自己敲的登录:BROWSER 指回 aca,命令不变
        assert_eq!(run(&login, here, &[]), (argv.clone(), Some(self_exe())));
        // 外面设了 BROWSER 的用你的;账号里写了的照账号的,界面起的也一样
        assert_eq!(run(&login, Seat { theirs: true, ..here }, &[]).1, None);
        assert_eq!(run(&login, Seat { asked: true, ..here }, &[("BROWSER", "/x/ff")]).1.as_deref(), Some("/x/ff"));
        // 界面起的:不是登录命令也这么办(新账号起的是 claude 本身),外面的 BROWSER 盖掉
        assert_eq!(run(&[], Seat { asked: true, theirs: true, ..here }, &[]).1, Some(self_exe()));
        // 别的命令:原样
        assert_eq!(run(&s(&["-p", "hi"]), here, &[]), (argv.clone(), None));
        // 远程:Claude 什么都不开(BROWSER=true),屏幕上给要贴码的链接;外面设了的照样用你的
        let far = Seat { remote: true, ..here };
        assert_eq!(run(&login, far, &[]), (argv.clone(), Some("true".into())));
        assert_eq!(run(&[], Seat { asked: true, ..far }, &[]).1.as_deref(), Some("true"));
        assert_eq!(run(&login, Seat { theirs: true, ..far }, &[]).1, None);

        // Codex 不认 BROWSER:macOS 上放进沙箱,只拦打开别的程序
        let cx = s(&["/usr/bin/env", "/bin/codex", "login"]);
        let sandboxed = cfg!(target_os = "macos") && std::path::Path::new(SANDBOX).exists();
        let strip = |out: Vec<String>| if sandboxed { out[3..].to_vec() } else { out };
        let out = quiet_login("codex", &s(&["login"]), here, &mut vec![], cx.clone());
        if sandboxed {
            assert_eq!(out[..3], s(&[SANDBOX, "-p", NO_OPEN])[..]);
        }
        assert_eq!(strip(out), cx);
        assert_eq!(quiet_login("codex", &s(&["login", "status"]), here, &mut vec![], cx.clone()), cx);
        // 远程:改走设备码,不回调本机;不是登录的不加
        assert_eq!(strip(quiet_login("codex", &s(&["login"]), far, &mut vec![], cx.clone())), s(&["/usr/bin/env", "/bin/codex", "login", "--device-auth"]));
        assert_eq!(quiet_login("codex", &s(&["login", "status"]), far, &mut vec![], cx.clone()), cx);
    }

    #[test]
    fn the_link_goes_to_the_file_the_ui_named() {
        let dir = std::env::temp_dir().join(format!("aca-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("login-claude-work.url");
        hand_link(Some(f.clone()), "https://claude.com/cai/oauth/authorize?x=1");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "https://claude.com/cai/oauth/authorize?x=1");
        // 写到旁边再改名,不留半截
        assert!(!f.with_extension("part").exists());
        // 第二次登录换了链接:覆盖
        hand_link(Some(f.clone()), "https://claude.com/cai/oauth/authorize?x=2");
        assert!(std::fs::read_to_string(&f).unwrap().ends_with("x=2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_recorded_path_outlives_an_upgrade() {
        let dir = std::env::temp_dir().join(format!("aca-exe-{}", std::process::id()));
        let keg = dir.join("Cellar/aca/0.1.0");
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::write(keg.join("bin/aca"), "").unwrap();
        let cellar = keg.join("bin/aca");
        // brew 的 opt/aca 还没指过来(装到一半):先用着 Cellar 里的
        assert_eq!(stable_exe(&cellar), cellar);
        std::fs::create_dir_all(dir.join("opt")).unwrap();
        std::os::unix::fs::symlink("../Cellar/aca/0.1.0", dir.join("opt/aca")).unwrap();
        assert_eq!(stable_exe(&cellar), dir.join("opt/aca/bin/aca"));
        // Linux 上被升级换掉的:同一个路径上现在是新版
        let local = dir.join("bin/aca");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(&local, "").unwrap();
        assert_eq!(stable_exe(&dir.join("bin/aca (deleted)")), local);
        // 别的照原样:不在 Cellar 里的,Cellar 下面层数不够的
        assert_eq!(stable_exe(&local), local);
        assert_eq!(stable_exe(Path::new("/x/Cellar/aca")), Path::new("/x/Cellar/aca"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
