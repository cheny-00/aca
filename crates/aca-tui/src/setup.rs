//! `aca setup`:装完之后要定的两样 —— 登记哪些账号、Claude 各账号的状态栏接不接。
//!
//! 在终端里是一张表([`Page`]):一行一个账号,Claude 的账号右边是状态栏的接法,←→ 换;`+` 登记一个账号
//! (名字 + 配置目录,写的是界面设置页里 `+` 的同一行)。回车才一起办,Esc 什么都不动。
//! 给了 `--statusline`、或者不在终端里:不开表,所有账号照那一种接 / 只列出来。`aca account add` 是不开表登记账号的写法。
//!
//! 账号不扫、不猜,只认登记的「名字 = 配置目录」;登录是工具自己的事,这里只把那条命令打出来。
//!
//! Claude 只在状态栏的输入里给订阅额度,所以要看额度,就得在它每个账号的 settings.json 里把 statusLine 指到 aca。
//! 三种接法:`aca`(状态栏交给 `aca statusline` 画)、`wrap`(留着你自己的状态栏,前面套一层 `aca limits --` 只记额度)、
//! `off`(不接;接过的撤掉,恢复成接之前的样子)。
//!
//! 改的是别人的配置文件,所以:
//! - 只换 `statusLine` 那一个值的那一段文字,别的地方一个字节不动(不是读进来再整个写回去:那样键的顺序、缩进都会变);
//! - 改完先当 JSON 读一遍,除了 statusLine 别的都和原来一样才写,写是先写临时文件再改名;
//! - 第一次动一个账号时,把它原来的 statusLine 原文记在 `<state>/setup/statusline.json`,`off` 照它恢复;
//! - 读不懂的(不是 JSON、statusLine 不是带 command 的对象)不碰,告诉你自己加哪一行。

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::ops::Range;
use std::path::{Path, PathBuf};

use aca_core::config::Config;
use aca_core::i18n::Tr;
use aca_core::paths::{self, Res};
use aca_core::{t, tr};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::accounts;
use crate::acctpage::{Form, Went, who_span};
use crate::data::Agent;
use crate::overlay::{Flow, Fx, Overlay, kit};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Aca,
    Wrap,
    Off,
}

impl Mode {
    fn parse(s: &str) -> Res<Mode> {
        match s {
            "aca" => Ok(Mode::Aca),
            "wrap" => Ok(Mode::Wrap),
            "off" => Ok(Mode::Off),
            x => Err(tr!("--statusline 是 aca、wrap、off 里的一个,不是 {x}", "--statusline is one of aca, wrap, off, not {x}")),
        }
    }

    /// 表里写的
    fn label(self) -> &'static str {
        match self {
            Mode::Aca => t!("aca 画", "aca draws"),
            Mode::Wrap => t!("自己的 + 记额度", "yours + quotas"),
            Mode::Off => t!("不接", "none"),
        }
    }
}

/// 一个账号的 statusLine 现在是什么
#[derive(Clone, Debug, PartialEq)]
enum Now {
    /// 没有 settings.json,或者里面没有 statusLine
    Nothing,
    /// `aca statusline`
    Aca,
    /// `aca limits -- <你的命令>`
    Wrapped(String),
    /// 你自己的命令
    Own(String),
}

/// 接之前的 statusLine 原文,按账号目录记;`None` = 接之前没有
type Saved = BTreeMap<String, Option<String>>;

/// 要写回去的内容,和这次之后「接之前的原文」该记成什么(`None` = 记录不动)
#[derive(Debug, PartialEq)]
struct Change {
    text: String,
    saved: Option<Option<Option<String>>>,
    said: String,
}

/// 读写的那几处(测试里换成临时目录)
#[derive(Clone, Debug)]
struct Env {
    /// config.toml
    config: PathBuf,
    home: String,
    /// 接之前的 statusLine 原文记在哪
    record: PathBuf,
    /// 写进 statusLine 的那个程序
    aca: String,
    /// 登记着的会话各用的(哪一家, 账号, 标题):`account rm` 删之前看谁在用
    sessions: fn() -> Vec<(Agent, String, String)>,
    /// 问工具这个账号登录着没有(`account status`)
    ask: fn(Agent, &str) -> accounts::Login,
}

impl Env {
    fn real() -> Env {
        Env {
            config: crate::config::path(),
            home: std::env::var("HOME").unwrap_or_default(),
            record: paths::state_dir().join("setup").join("statusline.json"),
            aca: program(),
            sessions: session_accounts,
            ask: accounts::login,
        }
    }

    /// config.toml 的全文(没有 = 空的)
    fn text(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap_or_default()
    }

    fn config(&self) -> Config {
        aca_core::config::parse(&self.text())
    }

    fn saved(&self) -> Saved {
        std::fs::read(&self.record).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn keep(&self, saved: &Saved) {
        if let (Ok(text), Some(dir)) = (serde_json::to_string_pretty(saved), self.record.parent()) {
            let _ = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&self.record, text));
        }
    }

    fn tilde(&self, dir: &Path) -> String {
        accounts::tilde(&dir.to_string_lossy(), &self.home)
    }
}

/// 登记着的会话各用的(哪一家, 账号, 标题;没标题的写目录名)
pub(crate) fn session_accounts() -> Vec<(Agent, String, String)> {
    let cfg = aca_core::config::load();
    let all = aca_core::store::Store::open().and_then(|s| s.sessions(false)).unwrap_or_default();
    let name = |s: &aca_core::store::Session| match s.title.trim() {
        "" => s.dir.rsplit('/').find(|p| !p.is_empty()).unwrap_or(&s.id).to_string(),
        t => t.to_string(),
    };
    all.into_iter().map(|s| (crate::data::agent_of_tool(&cfg, &s.tool), s.account.clone(), name(&s))).collect()
}

/// 这个配置目录的 Claude 状态栏现在是什么(账号页右边那一栏写的;要改是这里的表)
pub(crate) fn bar_said(dir: &Path) -> String {
    state_said(&read(dir).and_then(|t| now(&t)))
}

/// `aca setup [--statusline aca|wrap|off]`
pub fn run(args: &[String]) -> i32 {
    told(setup(args.to_vec()))
}

/// `aca account [ls]`、`aca account status`、`aca account add <claude | codex> <名字> [目录] [--statusline aca|wrap]`、
/// `aca account rm <claude | codex> <名字> [--force]`
pub fn account(args: &[String]) -> i32 {
    config_warning();
    told(account_cmd(args.to_vec(), &Env::real()))
}

fn told(r: Res<i32>) -> i32 {
    r.unwrap_or_else(|e| {
        eprintln!("{e}");
        1
    })
}

/// 取出 `--flag 值`;给了 flag 没给值是写错了
fn value_of(args: &mut Vec<String>, flag: &str) -> Res<Option<String>> {
    let Some(i) = args.iter().position(|a| a == flag) else { return Ok(None) };
    if i + 1 >= args.len() {
        return Err(tr!("{flag} 后面要跟一个值", "{flag} needs a value after it"));
    }
    args.remove(i);
    Ok(Some(args.remove(i)))
}

/// 写坏了的 config.toml 是当成没写的:人敲的命令提醒一句
fn config_warning() {
    if let Some(e) = aca_core::config::problem() {
        eprintln!("{}", tr!("config.toml 写法有问题,当成没写:{e}", "config.toml has a problem and is ignored: {e}"));
    }
    if let Some(e) = aca_core::config::login_problem() {
        eprintln!("{}", tr!("config.toml 的 [login] 写法有问题,这一节当成没写:{e}", "config.toml's [login] has a problem and is ignored: {e}"));
    }
}

fn setup(mut args: Vec<String>) -> Res<i32> {
    let mode = value_of(&mut args, "--statusline")?.map(|m| Mode::parse(&m)).transpose()?;
    if let Some(x) = args.first() {
        return Err(tr!(
            "setup 不认 {x}(用法:aca setup [--statusline aca|wrap|off])",
            "setup doesn't take {x} (usage: aca setup [--statusline aca|wrap|off])"
        ));
    }
    config_warning();
    let env = Env::real();
    match mode {
        None if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => table(env),
        _ => every(mode, &env),
    }
}

/// 在终端里:开那张表,关了之后把办了什么打出来
fn table(env: Env) -> Res<i32> {
    let mut page = Page::load(env, true);
    show(&mut page).map_err(|e| {
        tr!(
            "开不了表:{e}\n不开表的写法:aca setup --statusline aca|wrap|off、aca account add",
            "can't open the table: {e}\nwithout the table: aca setup --statusline aca|wrap|off, aca account add"
        )
    })?;
    let Some(report) = page.done.take() else {
        println!("{}", tr!("没动。以后想改:aca setup", "Nothing changed. To change it later: aca setup"));
        return Ok(0);
    };
    if report.lines.is_empty() {
        println!("{}", tr!("没有要改的。以后想改:aca setup", "Nothing to change. To change it later: aca setup"));
    }
    report.lines.iter().for_each(|l| println!("{l}"));
    Ok(i32::from(report.failed))
}

/// 不开表:列出 Claude 的账号现在各是什么;给了接法就所有账号照它接
fn every(mode: Option<Mode>, env: &Env) -> Res<i32> {
    let dirs = claude_dirs(&env.config(), &env.home);
    println!("{}", tr!("Claude 的状态栏和订阅额度", "Claude's status bar and subscription quotas"));
    println!(
        "{}",
        tr!(
            "  Claude 只在状态栏的输入里给订阅额度(5 小时 / 7 天用了多少):状态栏过一下 aca,计费面板顶上才有 Claude 的额度。",
            "  Claude only hands out subscription quotas (how much of 5 hours / 7 days is used) in the status bar's input: route the status bar through aca and the billing panel shows Claude's quotas on top."
        )
    );
    println!("{}", tr!("  不接的话别的都照常。\n", "  Without it everything else works as usual.\n"));
    if dirs.is_empty() {
        println!(
            "{}",
            tr!(
                "  没找到 Claude 的配置目录(~/.claude,或者 config.toml 的 [accounts.claude] 里登记的)。",
                "  No Claude config directory found (~/.claude, or one registered under [accounts.claude] in config.toml)."
            )
        );
        return Ok(0);
    }
    let pad = |s: &str, to: usize| format!("{s}{}", " ".repeat(to.saturating_sub(s.chars().count())));
    let (names, places) = (
        dirs.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0),
        dirs.iter().map(|(_, d)| env.tilde(d).chars().count()).max().unwrap_or(0),
    );
    // 读得懂的账号是不是都接了(读不懂的不会去动,不算在里面)
    let (mut ours, mut others) = (0, 0);
    for (name, dir) in &dirs {
        let state = read(dir).and_then(|t| now(&t));
        match state {
            Ok(Now::Aca | Now::Wrapped(_)) => ours += 1,
            Ok(_) => others += 1,
            Err(_) => {}
        }
        println!("  {}  {}  {}", pad(name, names), pad(&env.tilde(dir), places), state_said(&state));
    }
    println!();
    let Some(mode) = mode else {
        if ours > 0 && others == 0 {
            println!(
                "{}",
                tr!(
                    "都接好了。换接法:aca setup --statusline aca|wrap;撤掉:aca setup --statusline off",
                    "All wired. To switch: aca setup --statusline aca|wrap; to undo: aca setup --statusline off"
                )
            );
        } else {
            println!(
                "{}",
                tr!(
                    "要接:aca setup --statusline aca(交给 aca 画)或 wrap(留着自己的,只记额度);撤掉:--statusline off",
                    "To wire: aca setup --statusline aca (aca draws it) or wrap (keep yours, just record quotas); to undo: --statusline off"
                )
            );
        }
        println!(
            "{}",
            tr!(
                "在终端里直接敲 aca setup 是一张表:一个个账号挑接法,也能登记账号。",
                "Typed in a terminal, aca setup is a table: pick the wiring per account, and register accounts too."
            )
        );
        return Ok(0);
    };
    let mut saved = env.saved();
    let mut failed = false;
    for (name, dir) in &dirs {
        match wire(dir, mode, &env.aca, &mut saved) {
            Ok(said) => println!("  {name}:{said}"),
            // 读不懂的本来就没接过:撤的时候不算没办成
            Err(e) => {
                failed |= mode != Mode::Off;
                println!("{}", tr!("  {name}:没动({e})", "  {name}: unchanged ({e})"));
            }
        }
    }
    env.keep(&saved);
    if failed && mode == Mode::Aca {
        println!(
            "{}",
            tr!(
                "\n没改成的可以自己加:\"statusLine\": {}",
                "\nFor the ones not changed you can add it yourself: \"statusLine\": {}",
                fresh(&env.aca, "").replace('\n', " ")
            )
        );
    }
    println!(
        "{}",
        tr!(
            "\n开着的 Claude 会话马上就换,不用重启。{}",
            "\nOpen Claude sessions switch immediately, no restart needed. {}",
            if mode == Mode::Off { "" } else { t!("撤掉:aca setup --statusline off", "To undo: aca setup --statusline off") }
        )
    );
    Ok(i32::from(failed))
}

const ACCOUNT_USAGE: Tr = Tr::new(
    "用法:aca account ls\n     aca account status\n     aca account add <claude | codex> <名字> [目录] [--statusline aca|wrap]\n     aca account rm <claude | codex> <名字> [--force]",
    "usage: aca account ls\n       aca account status\n       aca account add <claude | codex> <NAME> [DIR] [--statusline aca|wrap]\n       aca account rm <claude | codex> <NAME> [--force]",
);

/// 命令行上写的哪一家
fn family(tool: &str) -> Res<Agent> {
    match tool {
        "claude" => Ok(Agent::Claude),
        "codex" => Ok(Agent::Codex),
        x => Err(tr!("只有 claude、codex 有账号,不是 {x}", "only claude and codex have accounts, not {x}")),
    }
}

/// `aca account status` 里一个账号的那句话:目录对不对、登录着没有(问工具自己)
fn account_state(env: &Env, a: &accounts::Account, abs: &str) -> String {
    use accounts::Login;
    if let Some((other, why)) = (!abs.is_empty()).then(|| accounts::foreign(a.agent, abs, &env.home)).flatten() {
        return tr!(
            "是 {} 的配置目录({why}),登记错了:aca account rm {} {}",
            "a {} config directory ({why}), registered by mistake: aca account rm {} {}",
            accounts::who(other),
            accounts::fam(a.agent).unwrap_or("claude"),
            a.name
        );
    }
    if !abs.is_empty() && !Path::new(abs).is_dir() {
        return t!("配置目录不在", "config directory missing").into();
    }
    match (env.ask)(a.agent, &a.name) {
        Login::In(who) if who.is_empty() => t!("登录着", "logged in").into(),
        Login::In(who) => tr!("登录着:{who}", "logged in: {who}"),
        Login::Out => {
            let how = if a.agent == Agent::Codex { "login" } else { "auth login" };
            tr!("没登录(登录:aca run {} -- {how})", "not logged in (to log in: aca run {} -- {how})", spec(a.agent, &a.name))
        }
        Login::Unknown(said) => tr!("问不出登录着没有:{said}", "can't tell whether it's logged in: {said}"),
    }
}

fn account_cmd(mut args: Vec<String>, env: &Env) -> Res<i32> {
    let mode = value_of(&mut args, "--statusline")?.map(|m| Mode::parse(&m)).transpose()?;
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None | Some("ls") => {
            let all: Vec<accounts::Account> = accounts::listed(&env.text(), &env.home).into_iter().map(|(a, _)| a).collect();
            fn place(a: &accounts::Account) -> &str {
                if a.dir.is_empty() { t!("(不换配置目录)", "(same config directory)") } else { &a.dir }
            }
            let cells: Vec<(&str, &str, &str)> =
                all.iter().map(|a| (accounts::fam(a.agent).unwrap_or("claude"), a.name.as_str(), place(a))).collect();
            // 终端里上色(print.rs),接到别处照原样
            if !crate::print::accounts(&cells) {
                let wide = all.iter().map(|a| spec(a.agent, &a.name).len()).max().unwrap_or(0);
                all.iter().for_each(|a| println!("{:wide$}  {}", spec(a.agent, &a.name), place(a)));
            }
            if all.is_empty() {
                println!(
                    "{}",
                    tr!(
                        "还没登记账号(不登记也能用:就是 ~/.claude、~/.codex)。",
                        "No accounts registered yet (it works without: that's ~/.claude, ~/.codex)."
                    )
                );
                println!(
                    "{}",
                    tr!(
                        "登记:aca account add <claude | codex> <名字> [目录],或者 aca setup",
                        "To register: aca account add <claude | codex> <NAME> [DIR], or aca setup"
                    )
                );
            }
            Ok(0)
        }
        Some("add") => {
            let mut need = |what: &str| args.next().ok_or_else(|| tr!("没给{what}\n{ACCOUNT_USAGE}", "missing {what}\n{ACCOUNT_USAGE}"));
            let (tool, name) =
                (need(t!("哪一家(claude / codex)", "which one (claude / codex)"))?, need(t!("账号的名字", "the account name"))?);
            let agent = family(&tool)?;
            if mode.is_some() && agent != Agent::Claude {
                return Err(t!(
                    "--statusline 只对 Claude 的账号有用(Codex 的额度不靠状态栏)",
                    "--statusline is only for Claude accounts (Codex quotas don't come from the status bar)"
                )
                .into());
            }
            let dir = args.next().unwrap_or_else(|| accounts::default_dir(agent, &name));
            if let Some(x) = args.next() {
                return Err(tr!("account add 不认 {x}\n{ACCOUNT_USAGE}", "account add doesn't take {x}\n{ACCOUNT_USAGE}"));
            }
            let added = accounts::add_in(&env.config, &env.home, agent, &name, &dir)?;
            let real = std::fs::canonicalize(&added.dir).unwrap_or_else(|_| PathBuf::from(&added.dir));
            println!(
                "{}",
                tr!(
                    "登记了 {}:配置目录 {}{}",
                    "registered {}: config directory {}{}",
                    spec(agent, &name),
                    env.tilde(&real),
                    if added.created { t!("(新建的)", " (created)") } else { "" }
                )
            );
            let mut failed = false;
            match mode {
                Some(mode) => {
                    let mut saved = env.saved();
                    match wire(&real, mode, &env.aca, &mut saved) {
                        Ok(said) => println!("{}", tr!("状态栏:{said}", "status bar: {said}")),
                        Err(e) => {
                            failed = true;
                            println!("{}", tr!("状态栏没动({e})", "status bar unchanged ({e})"));
                        }
                    }
                    env.keep(&saved);
                }
                None if agent == Agent::Claude => match read(&real).and_then(|t| now(&t)) {
                    Ok(Now::Aca | Now::Wrapped(_)) => {
                        println!("{}", tr!("状态栏已经过 aca 了。", "The status bar already goes through aca."))
                    }
                    _ => println!(
                        "{}",
                        tr!(
                            "状态栏还没接(计费面板要它才有这个账号的额度):aca setup",
                            "The status bar isn't wired yet (the billing panel needs it for this account's quotas): aca setup"
                        )
                    ),
                },
                None => {}
            }
            if added.created {
                println!("{}", tr!("新建的目录还没登录:{}", "the new directory isn't logged in yet: {}", login_hint(agent, &name)));
            }
            Ok(i32::from(failed))
        }
        // 各账号登录着没有:一个账号一个线程去问它的工具
        Some("status") => {
            if let Some(x) = args.next() {
                return Err(tr!("account status 不认 {x}\n{ACCOUNT_USAGE}", "account status doesn't take {x}\n{ACCOUNT_USAGE}"));
            }
            let all = accounts::listed(&env.text(), &env.home);
            let said: Vec<String> = std::thread::scope(|sc| {
                let asked: Vec<_> = all.iter().map(|(a, abs)| sc.spawn(move || account_state(env, a, abs))).collect();
                asked.into_iter().map(|h| h.join().unwrap_or_default()).collect()
            });
            let (who_w, dir_w) = (
                all.iter().map(|(a, _)| spec(a.agent, &a.name).len()).max().unwrap_or(0),
                all.iter().map(|(a, _)| a.dir.chars().count()).max().unwrap_or(0),
            );
            for ((a, _), said) in all.iter().zip(said) {
                let dir = format!("{}{}", a.dir, " ".repeat(dir_w - a.dir.chars().count()));
                println!("{:who_w$}  {dir}  {said}", spec(a.agent, &a.name));
            }
            if all.is_empty() {
                println!("{}", tr!("还没登记账号。", "No accounts registered yet."));
            }
            Ok(0)
        }
        // 只去掉登记那一行;有会话在用、是哪个工具的默认账号的,先说清楚,加 --force 才删
        Some("rm") => {
            let mut rest: Vec<String> = args.collect();
            let force = rest.iter().any(|a| a == "--force");
            rest.retain(|a| a != "--force");
            let [tool, name] = rest.as_slice() else {
                return Err(tr!(
                    "account rm 要写哪一家和账号的名字\n{ACCOUNT_USAGE}",
                    "account rm takes which one and the account name\n{ACCOUNT_USAGE}"
                ));
            };
            let agent = family(tool)?;
            let used = (env.sessions)().into_iter().filter(|(g, n, _)| *g == agent && n == name).count();
            let mut why = vec![];
            if used > 0 {
                why.push(tr!(
                    "有 {used} 个会话在用它(删了以后它们再启动会说没有这个账号)",
                    "{used} sessions use it (once removed, restarting them fails with no such account)"
                ));
            }
            for t in accounts::default_of(&env.config(), agent, name) {
                why.push(tr!(
                    "aca new {t} 不写账号时用的是它([tools.{t}] account)",
                    "it's the default for aca new {t} ([tools.{t}] account)"
                ));
            }
            if !force && !why.is_empty() {
                return Err(tr!(
                    "没删 {}:{}\n确实要删:加 --force",
                    "{} not removed: {}\nto remove it anyway: add --force",
                    spec(agent, name),
                    why.join(t!(";", "; "))
                ));
            }
            let gone = accounts::remove_in(&env.config, &env.home, agent, name)?;
            println!(
                "{}",
                tr!(
                    "删掉了 {} 的登记(config.toml 里那一行;删之前的存成 config.toml.bak-aca)",
                    "removed the registration of {} (its line in config.toml; the file as it was is kept as config.toml.bak-aca)",
                    spec(agent, name)
                )
            );
            if !gone.dir.is_empty() {
                println!("{}", tr!("目录 {} 和里面的登录没动。", "The directory {} and the login in it are untouched.", gone.dir));
            }
            Ok(0)
        }
        Some(x) => Err(tr!("account 不认 {x}\n{ACCOUNT_USAGE}", "account doesn't take {x}\n{ACCOUNT_USAGE}")),
    }
}

/// `claude:work`
fn spec(agent: Agent, name: &str) -> String {
    format!("{}:{name}", accounts::fam(agent).unwrap_or("claude"))
}

/// 新账号登录要敲的那条(工具自己的登录,aca 只把账号的环境带上):Claude 新目录第一次起来就会走登录
fn login_hint(agent: Agent, name: &str) -> String {
    format!("aca run {}{}", spec(agent, name), if agent == Agent::Codex { " -- login" } else { "" })
}

fn state_said(state: &Res<Now>) -> String {
    match state {
        Ok(Now::Nothing) => t!("没有状态栏", "no status bar").to_string(),
        Ok(Now::Aca) => t!("aca 的状态栏", "aca's status bar").to_string(),
        Ok(Now::Wrapped(own)) => tr!("你自己的状态栏,aca 记额度:{own}", "your own status bar, aca records quotas: {own}"),
        Ok(Now::Own(own)) => tr!("你自己的状态栏:{own}", "your own status bar: {own}"),
        Err(e) => tr!("读不懂,不会动它({e})", "can't read it, leaving it alone ({e})"),
    }
}

/// 照 `mode` 改这个目录的 settings.json,记录跟着变;回办了什么(不用动也说一句)
fn wire(dir: &Path, mode: Mode, aca: &str, saved: &mut Saved) -> Res<String> {
    let key = dir.to_string_lossy().into_owned();
    let text = read(dir)?;
    let Some(change) = plan(&text, mode, aca, saved.get(&key))? else {
        let nothing_to_wrap = mode == Mode::Wrap && now(&text)? == Now::Nothing;
        return Ok(if nothing_to_wrap {
            t!("没有自己的状态栏,没动", "no status bar of its own; unchanged")
        } else {
            t!("不用动", "nothing to do")
        }
        .to_string());
    };
    write(&dir.join("settings.json"), &text, &change.text)?;
    match change.saved {
        Some(Some(before)) => drop(saved.insert(key, before)),
        Some(None) => drop(saved.remove(&key)),
        None => {}
    }
    Ok(change.said)
}

/// 写进 statusLine 的那个程序:现在跑着的这个 aca 的完整路径(路径里有空格、引号的加引号)
fn program() -> String {
    let exe = aca_core::agent::self_exe();
    let plain = exe.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+@".contains(&b));
    if plain { exe } else { format!("'{}'", exe.replace('\'', "'\\''")) }
}

/// Claude 的配置目录:(名字, 目录)。登记的账号在前,`~/.claude` 没登记的话排最后;目录不在的不算
fn claude_dirs(cfg: &Config, home: &str) -> Vec<(String, PathBuf)> {
    let registered =
        cfg.accounts.get("claude").into_iter().flatten().filter(|(_, a)| !a.home.is_empty()).map(|(n, a)| (n.clone(), a.home.clone()));
    let mut out: Vec<(String, PathBuf)> = vec![];
    for (name, dir) in registered.chain([("claude".to_string(), format!("{home}/.claude"))]) {
        let Ok(real) = std::fs::canonicalize(paths::expand(&dir)) else { continue };
        if real.is_dir() && !out.iter().any(|(_, d)| *d == real) {
            out.push((name, real));
        }
    }
    out
}

/// settings.json 的原文;没有这个文件 = 空对象
fn read(dir: &Path) -> Res<String> {
    match std::fs::read_to_string(dir.join("settings.json")) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}\n".to_string()),
        Err(e) => Err(tr!("settings.json 读不了:{e}", "can't read settings.json: {e}")),
    }
}

/// 先写临时文件再改名(链接到别处的写到它指的那个文件上);原来的权限照旧。内容没变不写
fn write(path: &Path, old: &str, new: &str) -> Res<()> {
    if old == new {
        return Ok(());
    }
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = path.with_file_name(format!(".{}.aca-tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    std::fs::write(&tmp, new).map_err(|e| tr!("写不了 {}:{e}", "can't write {}: {e}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(&path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &path).map_err(|e| tr!("换不上 {}:{e}", "can't replace {}: {e}", path.display()))
}

/// 这份 settings.json 的 statusLine 现在是哪一种;读不懂的说为什么
fn now(text: &str) -> Res<Now> {
    let all: Value = serde_json::from_str(text).map_err(|e| tr!("settings.json 不是 JSON:{e}", "settings.json isn't JSON: {e}"))?;
    let all = all.as_object().ok_or(t!("settings.json 不是一个 JSON 对象", "settings.json isn't a JSON object"))?;
    let Some(line) = all.get("statusLine").filter(|v| !v.is_null()) else { return Ok(Now::Nothing) };
    let cmd = line
        .get("command")
        .and_then(Value::as_str)
        .ok_or(t!("statusLine 不是带 command 的对象", "statusLine isn't an object with a command"))?;
    Ok(match ours(cmd) {
        Some(("statusline", _)) => Now::Aca,
        Some(("limits", tail)) => Now::Wrapped(tail.strip_prefix("--").unwrap_or(tail).trim_start().to_string()),
        _ => Now::Own(cmd.to_string()),
    })
}

/// 这条命令调的是不是 aca:回(子命令, 后面的)
fn ours(cmd: &str) -> Option<(&str, &str)> {
    let cmd = cmd.trim_start();
    let (prog, rest) = match cmd.strip_prefix('\'') {
        Some(quoted) => quoted.split_once('\'')?,
        None => cmd.split_once(char::is_whitespace)?,
    };
    if Path::new(prog).file_name()?.to_str()? != "aca" {
        return None;
    }
    let rest = rest.trim_start();
    let (sub, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((sub, tail.trim_start()))
}

/// 照 `mode`,这份 settings.json 该变成什么;`None` = 不用动。`saved` 是这个账号记着的「接之前的原文」
fn plan(text: &str, mode: Mode, aca: &str, saved: Option<&Option<String>>) -> Res<Option<Change>> {
    let state = now(text)?;
    let (members, _) = members(text).ok_or(t!("settings.json 的写法这里认不出来", "can't make out how this settings.json is written"))?;
    let indent = indent_of(text, &members);
    let line = members.iter().find(|m| m.key == "statusLine").map(|m| &text[m.value.clone()]);
    let command = |value: &str, cmd: &str| {
        put(value, "command", Some(&Value::from(cmd).to_string()))
            .ok_or(t!("statusLine 的写法这里认不出来", "can't make out how this statusLine is written").to_string())
    };
    // 第一次动它才记原文:已经记过的是更早的那份,不盖
    let keep = |before: Option<String>| saved.is_none().then_some(Some(before));
    let (want, said, record): (Option<String>, String, Option<Option<Option<String>>>) = match (mode, &state) {
        (Mode::Aca, Now::Aca) => {
            let fixed = command(line.unwrap_or_default(), &format!("{aca} statusline"))?;
            (Some(fixed), t!("状态栏的命令换成了现在这个 aca", "the status bar command now points at this aca").into(), None)
        }
        (Mode::Aca, Now::Nothing) => (Some(fresh(aca, &indent)), t!("接上了 aca 的状态栏", "wired up aca's status bar").into(), keep(None)),
        (Mode::Aca, Now::Own(_)) => (
            Some(fresh(aca, &indent)),
            t!(
                "换成了 aca 的状态栏(原来的记下了,挑回不接 / --statusline off 就恢复)",
                "switched to aca's status bar (the original is saved; picking none / --statusline off restores it)"
            )
            .into(),
            keep(line.map(String::from)),
        ),
        (Mode::Aca, Now::Wrapped(own)) => {
            let before = if own.is_empty() { None } else { Some(command(line.unwrap_or_default(), own)?) };
            (
                Some(fresh(aca, &indent)),
                t!(
                    "换成了 aca 的状态栏(原来的记下了,挑回不接 / --statusline off 就恢复)",
                    "switched to aca's status bar (the original is saved; picking none / --statusline off restores it)"
                )
                .into(),
                keep(before),
            )
        }
        (Mode::Wrap, Now::Own(own)) => {
            let wrapped = command(line.unwrap_or_default(), &format!("{aca} limits -- {own}"))?;
            (
                Some(wrapped),
                t!(
                    "状态栏还是你的,前面加了一层 aca limits 记额度",
                    "the status bar is still yours, wrapped in aca limits to record quotas"
                )
                .into(),
                keep(line.map(String::from)),
            )
        }
        (Mode::Wrap, Now::Wrapped(own)) if !own.is_empty() => (
            Some(command(line.unwrap_or_default(), &format!("{aca} limits -- {own}"))?),
            t!("记额度的命令换成了现在这个 aca", "the quota command now points at this aca").into(),
            None,
        ),
        // 现在是 aca 画的:记着你原来那份的话,换回它、套上记额度的那层
        (Mode::Wrap, Now::Aca) => {
            match saved.and_then(|s| s.as_deref()).map(|before| (before, now(&format!("{{\"statusLine\":{before}}}")))) {
                Some((before, Ok(Now::Own(own)))) => (
                    Some(command(before, &format!("{aca} limits -- {own}"))?),
                    t!(
                        "换回了你自己的状态栏,前面加了一层 aca limits 记额度",
                        "back to your own status bar, wrapped in aca limits to record quotas"
                    )
                    .into(),
                    None,
                ),
                _ => return Ok(None),
            }
        }
        (Mode::Wrap, _) => return Ok(None),
        (Mode::Off, Now::Aca | Now::Wrapped(_)) => {
            let before = match (saved, &state) {
                (Some(before), _) => before.clone(),
                (None, Now::Wrapped(own)) if !own.is_empty() => Some(command(line.unwrap_or_default(), own)?),
                (None, _) => None,
            };
            let said = if before.is_some() {
                t!("恢复成了你原来的状态栏", "restored your original status bar")
            } else {
                t!("撤掉了(接之前没有状态栏)", "removed (there was no status bar before)")
            };
            (before, said.into(), Some(None))
        }
        (Mode::Off, _) => return Ok(None),
    };
    let new = put(text, "statusLine", want.as_deref())
        .ok_or(t!("settings.json 的写法这里认不出来", "can't make out how this settings.json is written"))?;
    // 除了 statusLine,别的一样都不能变
    let strip = |t: &str| serde_json::from_str::<Value>(t).ok().map(|mut v| v.as_object_mut().map(|o| o.remove("statusLine")).map(|_| v));
    match (strip(text), strip(&new)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Err(t!("改完对不上原来的内容,没写", "the result didn't match the original; not written").into()),
    }
    Ok((new != text).then_some(Change { text: new, saved: record, said }))
}

/// 新写的那个 statusLine;`indent` 是这份文件一层缩进用的什么
fn fresh(aca: &str, indent: &str) -> String {
    let cmd = Value::from(format!("{aca} statusline"));
    format!(
        "{{\n{indent}{indent}\"type\": \"command\",\n{indent}{indent}\"command\": {cmd},\n{indent}{indent}\"refreshInterval\": 10\n{indent}}}"
    )
}

/// 一个 JSON 对象里的一个成员在原文里的位置
#[derive(Debug, PartialEq)]
struct Member {
    key: String,
    /// 键的引号开始的地方
    start: usize,
    value: Range<usize>,
}

/// 最外层对象的成员,和结尾那个 `}` 的位置。只认合法的 JSON(调用前已经当 JSON 读过)
fn members(text: &str) -> Option<(Vec<Member>, usize)> {
    let b = text.as_bytes();
    let ws = |mut i: usize| {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        i
    };
    let mut i = ws(0);
    if *b.get(i)? != b'{' {
        return None;
    }
    let mut out = vec![];
    i = ws(i + 1);
    loop {
        if *b.get(i)? == b'}' {
            return Some((out, i));
        }
        let key_end = skip(b, i).filter(|_| b[i] == b'"')?;
        let colon = ws(key_end);
        if *b.get(colon)? != b':' {
            return None;
        }
        let value = ws(colon + 1);
        let value_end = skip(b, value)?;
        out.push(Member { key: text[i + 1..key_end - 1].to_string(), start: i, value: value..value_end });
        i = ws(value_end);
        if *b.get(i)? == b',' {
            i = ws(i + 1);
        }
    }
}

/// 从 `i` 这个值的开头跳到它后面
fn skip(b: &[u8], i: usize) -> Option<usize> {
    match *b.get(i)? {
        b'"' => {
            let mut j = i + 1;
            loop {
                match *b.get(j)? {
                    b'\\' => j += 2,
                    b'"' => return Some(j + 1),
                    _ => j += 1,
                }
            }
        }
        b'{' | b'[' => {
            let (mut depth, mut j) = (0usize, i);
            loop {
                match *b.get(j)? {
                    b'"' => {
                        j = skip(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let end = b[i..].iter().position(|c| matches!(c, b',' | b'}' | b']') || c.is_ascii_whitespace()).map_or(b.len(), |n| i + n);
            (end > i).then_some(end)
        }
    }
}

/// 这份文件一层缩进用的什么:第一个成员前面那段空白;都写在一行的、空对象的,按两个空格
fn indent_of(text: &str, members: &[Member]) -> String {
    let lead = members.first().map(|m| &text[text[..m.start].rfind('\n').map_or(0, |n| n + 1)..m.start]);
    lead.filter(|l| !l.is_empty() && l.chars().all(char::is_whitespace)).unwrap_or("  ").to_string()
}

/// 把对象 `text` 里 `key` 的值换成 `value`(没有就加在最后),`None` = 去掉这个成员。别的地方原样
fn put(text: &str, key: &str, value: Option<&str>) -> Option<String> {
    let (members, close) = members(text)?;
    let at = members.iter().position(|m| m.key == key);
    Some(match (at, value) {
        (Some(i), Some(v)) => format!("{}{v}{}", &text[..members[i].value.start], &text[members[i].value.end..]),
        (None, Some(v)) => {
            let indent = indent_of(text, &members);
            match members.last() {
                Some(last) => format!("{},\n{indent}\"{key}\": {v}{}", &text[..last.value.end], &text[last.value.end..]),
                None => format!("{}\n{indent}\"{key}\": {v}\n{}", &text[..close], &text[close..]),
            }
        }
        // 不是第一个:连前面的逗号一起去;是第一个:连后面的逗号一起去
        (Some(i), None) if i > 0 => format!("{}{}", &text[..members[i - 1].value.end], &text[members[i].value.end..]),
        (Some(i), None) => match members.get(i + 1) {
            Some(next) => format!("{}{}", &text[..members[i].start], &text[next.start..]),
            None => format!("{}{}", text[..members[i].start].trim_end(), &text[close..]),
        },
        (None, None) => text.to_string(),
    })
}

// ---------- 那张表 ----------

/// 这个账号有没有自己的状态栏可留(留不了的账号,「自己的 + 记额度」不给选)
fn has_own(state: &Now, saved: Option<&Option<String>>) -> bool {
    match state {
        Now::Own(_) => true,
        Now::Wrapped(own) => !own.is_empty(),
        // 现在是 aca 画的:记着接之前那份的话,还换得回去
        Now::Aca => {
            saved.and_then(|s| s.as_deref()).is_some_and(|before| matches!(now(&format!("{{\"statusLine\":{before}}}")), Ok(Now::Own(_))))
        }
        Now::Nothing => false,
    }
}

/// 一个 Claude 账号的状态栏:现在是什么、要接成什么
#[derive(Clone, Debug)]
struct Wire {
    /// settings.json 在哪个目录
    dir: PathBuf,
    /// 读不懂的说为什么(不会动它)
    now: Res<Now>,
    /// 现在算哪一种接法
    was: Mode,
    pick: Mode,
    own: bool,
}

impl Wire {
    fn of(dir: PathBuf, saved: &Saved) -> Wire {
        let now = read(&dir).and_then(|t| now(&t));
        let was = match &now {
            Ok(Now::Aca) => Mode::Aca,
            Ok(Now::Wrapped(_)) => Mode::Wrap,
            _ => Mode::Off,
        };
        let own = now.as_ref().is_ok_and(|n| has_own(n, saved.get(&*dir.to_string_lossy())));
        Wire { dir, now, was, pick: was, own }
    }

    /// 能选的接法,按 ←→ 换的顺序
    fn choices(&self) -> Vec<Mode> {
        if self.now.is_err() {
            return vec![];
        }
        [Mode::Aca, Mode::Wrap, Mode::Off].into_iter().filter(|m| *m != Mode::Wrap || self.own || self.was == Mode::Wrap).collect()
    }

    fn changed(&self) -> bool {
        self.now.is_ok() && self.pick != self.was
    }
}

/// 表里的一行
#[derive(Clone, Debug)]
struct Row {
    agent: Agent,
    name: String,
    /// 配置目录,家目录下的写成 ~/…;不换目录的账号是空的
    shown: String,
    /// 这次要登记的(回车才写进 config.toml):展开的目录
    new: Option<String>,
    /// 没登记的那个 `~/.claude`(这次正登记它的也算:`x` 不登记了要把它放回来)
    plain: bool,
    /// Claude 的、目录在的才有
    wire: Option<Wire>,
}

/// 回车办完的结果
#[derive(Debug, Default)]
pub struct Report {
    /// 一行一句,开表的命令关表之后打出来
    pub lines: Vec<String>,
    pub failed: bool,
    added: usize,
    wired: usize,
}

impl Report {
    /// 界面里底栏说的那一句
    fn brief(&self) -> String {
        let mut parts = vec![];
        if self.added > 0 {
            parts.push(tr!("登记了 {} 个账号", "registered accounts: {}", self.added));
        }
        if self.wired > 0 {
            parts.push(tr!("改了 {} 个账号的状态栏", "status bars changed: {}", self.wired));
        }
        if self.failed {
            parts.push(t!("有没办成的", "some didn't work").into());
        }
        if parts.is_empty() { t!("没有要改的", "nothing to change").into() } else { parts.join(",") }
    }
}

/// 账号和状态栏的那张表。`aca setup` 单独开它;界面里从设置页的账号进(那里自己有 `+`,这里就不给加)
pub struct Page {
    env: Env,
    rows: Vec<Row>,
    sel: usize,
    can_add: bool,
    form: Option<Form>,
    saved: Saved,
    /// 回车办完的;Esc 关的是 None
    pub done: Option<Report>,
}

const INTRO: Tr = Tr::new(
    "Claude 只在状态栏的输入里给订阅额度:状态栏过一下 aca,计费面板才有它的额度。不接的话别的都照常。",
    "Claude only hands out subscription quotas in the status bar's input: route it through aca and the billing panel shows them. Without it everything else works as usual.",
);
/// 说明那一块占几行(固定,高亮上下挪时表不跳);窄的(手机)一句话要折成两行
fn detail_rows(w: usize) -> usize {
    if w >= 60 { 3 } else { 5 }
}
/// 接法那一列:「‹ 」+ 最长的那个 +「 ›」
const PICK_W: usize = 15;

impl Page {
    pub fn real(can_add: bool) -> Page {
        Page::load(Env::real(), can_add)
    }

    fn load(env: Env, can_add: bool) -> Page {
        let saved = env.saved();
        let mut rows: Vec<Row> = vec![];
        let mut seen: Vec<PathBuf> = vec![];
        let wire_of = |dir: &str, seen: &mut Vec<PathBuf>| {
            let real = std::fs::canonicalize(dir).ok().filter(|d| d.is_dir() && !seen.contains(d))?;
            seen.push(real.clone());
            Some(Wire::of(real, &saved))
        };
        let listed = accounts::listed(&env.text(), &env.home);
        for agent in [Agent::Claude, Agent::Codex] {
            let claude = agent == Agent::Claude;
            for (a, abs) in listed.iter().filter(|(a, _)| a.agent == agent) {
                // 目录其实是另一家的(把 ~/.codex 登记成了 Claude 的账号):不往里接状态栏
                let ours = claude && !abs.is_empty() && accounts::foreign(agent, abs, &env.home).is_none();
                let wire = if ours { wire_of(abs, &mut seen) } else { None };
                rows.push(Row { agent, name: a.name.clone(), shown: a.dir.clone(), new: None, plain: false, wire });
            }
            // 没登记的 ~/.claude 也是个账号:不登记也能用的那个
            if claude && let Some(wire) = wire_of(&format!("{}/.claude", env.home), &mut seen) {
                rows.push(Row { agent, name: "claude".into(), shown: "~/.claude".into(), new: None, plain: true, wire: Some(wire) });
            }
        }
        Page { env, rows, sel: 0, can_add, form: None, saved, done: None }
    }

    /// 要登记几个、要改几个账号的状态栏
    fn pending(&self) -> (usize, usize) {
        let new = self.rows.iter().filter(|r| r.new.is_some()).count();
        (new, self.rows.iter().filter(|r| r.wire.as_ref().is_some_and(Wire::changed)).count())
    }

    /// ←→:高亮那个账号换一种接法
    fn cycle(&mut self, forward: bool) {
        let Some(w) = self.rows.get_mut(self.sel).and_then(|r| r.wire.as_mut()) else { return };
        let all = w.choices();
        let Some(at) = all.iter().position(|m| *m == w.pick) else { return };
        w.pick = all[(at + if forward { 1 } else { all.len() - 1 }) % all.len()];
    }

    /// a:选得了这种接法的账号都照高亮这一行
    fn all_like_this(&mut self) {
        let Some(pick) = self.rows.get(self.sel).and_then(|r| r.wire.as_ref()).filter(|w| w.now.is_ok()).map(|w| w.pick) else { return };
        for w in self.rows.iter_mut().filter_map(|r| r.wire.as_mut()) {
            if w.choices().contains(&pick) {
                w.pick = pick;
            }
        }
    }

    /// 新登记的 Claude 账号先照别的账号的接法摆着(回车前看得到、换得了)
    fn like_others(&self, w: &Wire) -> Mode {
        let picks: Vec<Mode> = self.rows.iter().filter_map(|r| r.wire.as_ref()).filter(|o| o.now.is_ok()).map(|o| o.pick).collect();
        if w.now.is_err() || w.was != Mode::Off {
            w.was
        } else if w.own && picks.iter().find(|m| **m != Mode::Off) == Some(&Mode::Wrap) {
            Mode::Wrap
        } else if picks.contains(&Mode::Aca) {
            Mode::Aca
        } else {
            Mode::Off
        }
    }

    /// `+` 开着:键给表单(哪个工具 → 名字 → 配置目录,见 acctpage.rs);目录填完了列进表里
    fn form_key(&mut self, k: KeyEvent) {
        let Some(f) = self.form.as_mut() else { return };
        let rows = &self.rows;
        match f.key(k, |agent, name| rows.iter().any(|r| r.agent == agent && r.name == name && (!r.plain || r.new.is_some()))) {
            Went::Left => self.form = None,
            Went::Filled => self.form_enter(),
            Went::Stay => {}
        }
    }

    fn form_enter(&mut self) {
        let Some(f) = self.form.as_mut() else { return };
        let (agent, name) = (f.agent, f.name.clone());
        let abs = match accounts::check_in(&self.env.config, &self.env.home, agent, &name, f.text.trim()) {
            Ok(abs) => abs,
            Err(e) => return f.hint = e,
        };
        if let Some(other) = self.rows.iter().find(|r| r.agent == agent && r.new.as_deref() == Some(abs.as_str())) {
            return f.hint = tr!("这个目录这次已经登记给 {} 了", "this directory is already being registered for {}", other.name);
        }
        self.form = None;
        let shown = accounts::tilde(&abs, &self.env.home);
        let mut row = Row { agent, name, shown, new: Some(abs.clone()), plain: false, wire: None };
        // 目录已经在的读它现在的状态栏;还没有的回车后才建
        let real = std::fs::canonicalize(&abs).unwrap_or_else(|_| PathBuf::from(&abs));
        // 登记的就是没登记时列着的那个 ~/.claude:换掉那一行,挑好的接法带过来
        if let Some(i) = self.rows.iter().position(|r| r.plain && r.new.is_none() && r.wire.as_ref().is_some_and(|w| w.dir == real)) {
            (row.wire, row.plain) = (self.rows[i].wire.take(), true);
            self.rows[i] = row;
            return self.sel = i;
        }
        if agent == Agent::Claude {
            let mut w = Wire::of(real, &self.saved);
            w.pick = self.like_others(&w);
            row.wire = Some(w);
        }
        // 排在自己那一家的最后
        let at =
            self.rows.iter().rposition(|r| r.agent == agent).map_or(if agent == Agent::Claude { 0 } else { self.rows.len() }, |i| i + 1);
        self.rows.insert(at, row);
        self.sel = at;
    }

    /// 回车:先登记账号,再改状态栏(只动挑的和现在不一样的)
    fn apply(&mut self) -> Report {
        let mut r = Report::default();
        let (mut accounts_said, mut wires_said, mut logins) = (vec![], vec![], vec![]);
        let mut refused: Vec<usize> = vec![];
        for (i, row) in self.rows.iter_mut().enumerate() {
            let Some(dir) = row.new.clone() else { continue };
            let who = spec(row.agent, &row.name);
            match accounts::add_in(&self.env.config, &self.env.home, row.agent, &row.name, &dir) {
                Ok(added) => {
                    r.added += 1;
                    (row.new, row.plain) = (None, false);
                    accounts_said.push(tr!(
                        "  {who}:登记了,配置目录 {}{}",
                        "  {who}: registered, config directory {}{}",
                        row.shown,
                        if added.created { t!("(新建的)", " (created)") } else { "" }
                    ));
                    if added.created {
                        logins.push(format!("  {}", login_hint(row.agent, &row.name)));
                    }
                    // 目录现在有了:记录里的名字照真实路径(和以后再开表时读到的一样)
                    if let (Some(w), Ok(real)) = (row.wire.as_mut(), std::fs::canonicalize(&added.dir)) {
                        w.dir = real;
                    }
                }
                Err(e) => {
                    r.failed = true;
                    refused.push(i);
                    accounts_said.push(tr!("  {who}:没登记({e})", "  {who}: not registered ({e})"));
                }
            }
        }
        for (i, row) in self.rows.iter_mut().enumerate() {
            let Some(w) = row.wire.as_mut().filter(|w| w.changed() && !refused.contains(&i)) else { continue };
            match wire(&w.dir, w.pick, &self.env.aca, &mut self.saved) {
                Ok(said) => {
                    r.wired += 1;
                    w.was = w.pick;
                    wires_said.push(format!("  {}:{said}", row.name));
                }
                Err(e) => {
                    r.failed |= w.pick != Mode::Off;
                    wires_said.push(tr!("  {}:没动({e})", "  {}: unchanged ({e})", row.name));
                }
            }
        }
        if !wires_said.is_empty() {
            self.env.keep(&self.saved);
        }
        for (title, said) in [
            (t!("账号", "Accounts"), accounts_said),
            (t!("状态栏", "Status bar"), wires_said),
            (
                t!(
                    "新建的配置目录还没登录,各登录一次(走的是工具自己的登录):",
                    "New config directories aren't logged in yet; log in once each (with the tool's own login):"
                ),
                logins,
            ),
        ] {
            if !said.is_empty() {
                r.lines.push(title.to_string());
                r.lines.extend(said);
            }
        }
        if r.wired > 0 {
            r.lines.push(t!("开着的 Claude 会话马上就换,不用重启。", "Open Claude sessions switch immediately, no restart needed.").into());
        }
        r
    }

    /// 高亮那一行下面的说明:现在是什么、挑的这种是什么
    fn detail(&self, row: &Row) -> Vec<(String, Style)> {
        let mut out = vec![];
        if let Some(dir) = &row.new {
            let then = if Path::new(dir).exists() {
                t!("(目录已经在,不用再登录)", " (the directory exists, no login needed)")
            } else {
                t!("、建目录,再打出登录的命令", ", creates the directory, and prints the login command")
            };
            out.push((tr!("新登记:回车后写进 config.toml{then}", "New: Enter writes it into config.toml{then}"), theme::accent()));
        }
        let Some(w) = &row.wire else {
            let why = match row.agent {
                Agent::Claude if row.shown.is_empty() => t!(
                    "这个账号不换配置目录,状态栏跟着它用的那个目录。",
                    "This account keeps the default config directory; its status bar follows that directory."
                ),
                Agent::Claude if row.new.is_none() => t!(
                    "配置目录不在、是 Codex 的目录、或者和上面的账号是同一个:这里不接。",
                    "The config directory is missing, is a Codex directory, or is the same as an account above: not wired here."
                ),
                Agent::Claude => "",
                _ => t!("Codex 的额度不靠状态栏,不用接。", "Codex quotas don't come from the status bar; nothing to wire."),
            };
            out.push((why.to_string(), kit::dim()));
            return out;
        };
        match &w.now {
            Err(e) => out.push((
                tr!("读不懂它的 settings.json,不会动它({e})", "can't read its settings.json, leaving it alone ({e})"),
                theme::warn(),
            )),
            Ok(_) => {
                out.push((tr!("现在:{}", "now: {}", state_said(&w.now)), kit::dim()));
                let what = match (w.pick, w.was) {
                    (Mode::Aca, _) => t!(
                        "上下文 │ 模型 │ 分支 │ 目录,第二行是额度 │ 花费和时长",
                        "context │ model │ branch │ directory, then quota │ cost and duration"
                    ),
                    (Mode::Wrap, _) => t!(
                        "状态栏还是你自己的,前面套一层 aca limits 只记额度",
                        "your own status bar stays, wrapped in aca limits just to record quotas"
                    ),
                    (Mode::Off, Mode::Off) => t!(
                        "别的都照常,只是计费面板没有这个账号的额度",
                        "everything else as usual; the billing panel just has no quotas for this account"
                    ),
                    (Mode::Off, _) => t!("撤掉 aca 接的,恢复成接之前的样子", "undo what aca wired, back to how it was"),
                };
                let lead = if w.changed() { t!("回车后 → ", "after Enter → ") } else { "" };
                out.push((format!("{lead}{}:{what}", w.pick.label()), if w.changed() { theme::accent() } else { kit::dim() }));
            }
        }
        out
    }

    fn row_line(&self, i: usize, row: &Row, name_w: usize, w: usize) -> Line<'static> {
        let on = i == self.sel && self.form.is_none();
        // 右边:改没改的记号两格 + 接法那一格
        let right_w = 2 + 2 + PICK_W + 2;
        // 名字和接法一定写;地方不够(手机)先把「新登记」缩成「新」,再不够就不写目录
        let room = w.saturating_sub(1 + name_w + 2 + right_w);
        let tag = match row.new {
            Some(_) if room >= 20 => t!("  新登记", "  new account"),
            Some(_) => t!("  新", "  new"),
            None => "",
        };
        let dir_w = room.saturating_sub(2 + tag.width());
        let dir = if row.shown.is_empty() { t!("(不换配置目录)", "(same config directory)") } else { row.shown.as_str() };
        let mut left = vec![Span::raw(" "), Span::styled(kit::clip(&row.name, name_w), crate::ui::agent_style(row.agent))];
        if dir_w >= 8 {
            // 目录太长留结尾:几个账号的目录多半只差最后几个字
            left.extend([Span::raw("  "), Span::styled(tail(dir, dir_w), kit::dim())]);
        }
        left.push(Span::styled(tag, theme::accent()));
        let right = match &row.wire {
            None => vec![],
            Some(Wire { now: Err(_), .. }) => {
                vec![Span::styled(format!("    {}", kit::clip(t!("读不懂,不动", "unreadable, untouched"), PICK_W + 2)), theme::warn())]
            }
            Some(wire) => {
                let st = if wire.changed() { theme::accent().add_modifier(Modifier::BOLD) } else { Style::new() };
                let (l, r) = if on && wire.choices().len() > 1 { ("‹ ", " ›") } else { ("  ", "  ") };
                let label = wire.pick.label();
                vec![
                    Span::styled(if wire.changed() { "* " } else { "  " }, theme::warn()),
                    Span::styled(l, theme::faint()),
                    Span::styled(label, st),
                    Span::styled(r, theme::faint()),
                    Span::raw(" ".repeat(PICK_W.saturating_sub(label.width()))),
                ]
            }
        };
        kit::picked(kit::spread(left, right, w), w, on)
    }
}

impl Overlay for Page {
    fn width(&self) -> u16 {
        84
    }

    fn lines(&self, w: usize) -> Vec<Line<'static>> {
        let esc = kit::esc_hint(if self.form.is_some() { t!("退回", "back") } else { t!("不改", "change nothing") });
        let mut out = vec![
            kit::modal_title(t!("账号和 Claude 的状态栏", "Accounts and Claude's status bar"), theme::accent(), esc, w),
            Line::raw(""),
        ];
        out.extend(fold(&INTRO, w).into_iter().map(|l| Line::styled(l, kit::dim())));
        out.push(Line::raw(""));
        let name_w = self.rows.iter().map(|r| r.name.width()).max().unwrap_or(0).min(14);
        let mut last = None;
        for (i, row) in self.rows.iter().enumerate() {
            if last != Some(row.agent) {
                let claude = row.agent == Agent::Claude;
                let head = vec![who_span(row.agent)];
                let col = if claude {
                    vec![Span::styled(format!("    {}", kit::clip(t!("状态栏", "Status bar"), PICK_W + 2)), kit::dim())]
                } else {
                    vec![]
                };
                out.push(kit::spread(head, col, w));
                last = Some(row.agent);
            }
            out.push(self.row_line(i, row, name_w, w));
        }
        if self.rows.is_empty() {
            out.push(Line::styled(
                t!(
                    "还没有账号(不登记也能用:就是 ~/.claude、~/.codex)。",
                    "No accounts yet (it works without: that's ~/.claude, ~/.codex)."
                ),
                kit::dim(),
            ));
        }
        out.push(Line::raw(""));
        if let Some(f) = &self.form {
            out.extend(f.lines(w, t!("列进表里", "add to the table")));
            return out;
        }
        let mut detail: Vec<Line<'static>> = self
            .rows
            .get(self.sel)
            .map(|r| self.detail(r))
            .unwrap_or_default()
            .into_iter()
            .filter(|(text, _)| !text.is_empty())
            .flat_map(|(text, st)| fold(&text, w).into_iter().map(move |l| Line::styled(l, st)))
            .take(detail_rows(w))
            .collect();
        detail.resize(detail_rows(w), Line::raw(""));
        out.extend(detail);
        out.push(Line::raw(""));
        let (new, changed) = self.pending();
        let todo = match (new, changed) {
            (0, 0) => t!("没有要改的", "nothing to change").to_string(),
            (0, c) => tr!("改 {c} 个状态栏", "change status bars: {c}"),
            (n, 0) => tr!("登记 {n} 个账号", "register accounts: {n}"),
            (n, c) => tr!("登记 {n} 个账号,改 {c} 个状态栏", "register accounts: {n}, change status bars: {c}"),
        };
        // 放不下时后面的键先不写:最要紧的排前面
        let mut keys = vec![("⏎", t!("照办", "do it")), ("←→", t!("换接法", "wiring"))];
        if self.rows.get(self.sel).is_some_and(|r| r.new.is_some()) {
            keys.push(("x", t!("不登记了", "drop it")));
        }
        if self.can_add {
            keys.push(("+", t!("登记账号", "register")));
        }
        keys.push(("a", t!("都照这行", "all like this")));
        let todo = Span::styled(todo, if new + changed > 0 { theme::accent() } else { kit::dim() });
        // 窄的:要办什么单独一行,键另起一行
        if w >= 60 {
            out.push(kit::keys_line(&keys, vec![todo], w));
        } else {
            out.extend([Line::from(todo), kit::keys_line(&keys, vec![], w)]);
        }
        out
    }

    fn on_key(&mut self, k: KeyEvent, fx: &mut Fx) -> Flow {
        if self.form.is_some() {
            self.form_key(k);
            return Flow::Stay;
        }
        let n = self.rows.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return Flow::Close,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Right | KeyCode::Char('l' | ' ') | KeyCode::Tab => self.cycle(true),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => self.cycle(false),
            KeyCode::Char('a') => self.all_like_this(),
            KeyCode::Char('+') if self.can_add => self.form = Some(Form::new()),
            // 这次登记的那行:不登记了(登记的是 ~/.claude 的话,它原来那行回来)
            KeyCode::Char('x') | KeyCode::Delete if self.rows.get(self.sel).is_some_and(|r| r.new.is_some()) => {
                let gone = self.rows.remove(self.sel);
                if gone.plain {
                    self.rows.insert(self.sel, Row { name: "claude".into(), shown: "~/.claude".into(), new: None, ..gone });
                }
                self.sel = self.sel.min(self.rows.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let report = self.apply();
                fx.say(report.brief());
                self.done = Some(report);
                return Flow::Close;
            }
            _ => {}
        }
        Flow::Stay
    }

    fn typing(&self) -> bool {
        self.form.as_ref().is_some_and(Form::typing)
    }
}

/// 按宽度折行;句读不甩到下一行的开头(留两格给它挂在行尾)
pub(crate) fn fold(text: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        let hangs = "。,、:;!?)」".contains(c);
        if used > 0 && used + cw > if hangs { w } else { w.saturating_sub(2) } {
            out.push(String::new());
            used = 0;
        }
        out.last_mut().unwrap().push(c);
        used += cw;
    }
    out
}

/// 输入框里太长的只留结尾(刚打的字看得见)
pub(crate) fn tail(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_string();
    }
    let (mut kept, mut n) = (vec![], 1);
    for c in text.chars().rev() {
        n += c.width().unwrap_or(0);
        if n > room {
            break;
        }
        kept.push(c);
    }
    format!("…{}", kept.into_iter().rev().collect::<String>())
}

/// `aca setup` 单独开这张表:占住终端,回车 / Esc / ^C 关
fn show(page: &mut Page) -> std::io::Result<()> {
    let mut term = ratatui::try_init()?;
    let out = (|| -> std::io::Result<()> {
        loop {
            term.draw(|f| draw(f, page))?;
            let Event::Key(k) = event::read()? else { continue };
            if k.kind == KeyEventKind::Release {
                continue;
            }
            let quit = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
            if quit || page.on_key(k, &mut Fx::default()) == Flow::Close {
                return Ok(());
            }
        }
    })();
    // 终端没了(窗口被关)就还不回去,也别再打印:往没了的终端打印会出错(见 exitlog.rs)
    if ratatui::try_restore().is_err() {
        return Ok(());
    }
    out
}

/// 够宽够高居中、左右留白;窄的(手机)顶着上边占满宽
fn draw(f: &mut Frame, page: &Page) {
    let full = f.area();
    let roomy = full.width >= 70 && full.height >= 20;
    let width = if roomy { page.width().min(full.width - 4) } else { full.width };
    let body = page.lines(width.saturating_sub(if roomy { 6 } else { 4 }) as usize);
    let height = (body.len() as u16 + if roomy { 4 } else { 2 }).min(full.height);
    let area = if roomy {
        Rect { x: full.x + (full.width - width) / 2, y: full.y + (full.height - height) / 2, width, height }
    } else {
        Rect { x: full.x, y: full.y, width, height }
    };
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(theme::pal().accent)).padding(if roomy {
        Padding::new(2, 2, 1, 1)
    } else {
        Padding::horizontal(1)
    });
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(body), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACA: &str = "/opt/bin/aca";

    /// 接之前、接之后各一份,中间的 `saved` 照 `run` 那样跟着变
    fn step(text: &str, mode: Mode, saved: &mut Option<Option<String>>) -> Option<(String, String)> {
        let c = plan(text, mode, ACA, saved.as_ref()).unwrap()?;
        match c.saved {
            Some(Some(before)) => *saved = Some(before),
            Some(None) => *saved = None,
            None => {}
        }
        Some((c.text, c.said))
    }

    #[test]
    fn only_the_status_line_changes_and_off_puts_back_exactly_what_was_there() {
        // 四个空格缩进、键的顺序、行尾的空格、别的设置:都得原样
        let mine = "{\n    \"model\": \"opus\",\n    \"statusLine\": {\"type\": \"command\",  \"command\": \"bash ~/bar.sh --x '{}'\", \"padding\": 2},\n    \"env\": {\"A\": \"}\\\"{\"}\n}\n";
        let mut saved = None;
        let (to_aca, said) = step(mine, Mode::Aca, &mut saved).unwrap();
        assert_eq!(
            to_aca,
            "{\n    \"model\": \"opus\",\n    \"statusLine\": {\n        \"type\": \"command\",\n        \"command\": \"/opt/bin/aca statusline\",\n        \"refreshInterval\": 10\n    },\n    \"env\": {\"A\": \"}\\\"{\"}\n}\n"
        );
        assert!(said.contains("换成了 aca 的状态栏"), "{said}");
        assert_eq!(now(&to_aca), Ok(Now::Aca));
        // 再接一次不用动;换成只记额度:回到你那份,只多一层
        assert_eq!(step(&to_aca, Mode::Aca, &mut saved), None);
        let (wrapped, _) = step(&to_aca, Mode::Wrap, &mut saved).unwrap();
        assert_eq!(wrapped, mine.replace("bash ~/bar.sh", "/opt/bin/aca limits -- bash ~/bar.sh"));
        assert_eq!(now(&wrapped), Ok(Now::Wrapped("bash ~/bar.sh --x '{}'".into())));
        // 撤掉:和接之前一个字节不差,记录也清了
        let (back, said) = step(&wrapped, Mode::Off, &mut saved).unwrap();
        assert_eq!((back.as_str(), said.as_str(), &saved), (mine, "恢复成了你原来的状态栏", &None));
        assert_eq!(step(&back, Mode::Off, &mut saved), None);
    }

    #[test]
    fn an_account_without_a_status_line_gets_one_and_loses_it_again() {
        let mut saved = None;
        // 没有 settings.json(当成空对象)、只有别的设置、都写在一行的
        let (made, _) = step("{}\n", Mode::Aca, &mut saved).unwrap();
        assert_eq!(
            made,
            "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/opt/bin/aca statusline\",\n    \"refreshInterval\": 10\n  }\n}\n"
        );
        assert_eq!(saved, Some(None));
        assert_eq!(step(&made, Mode::Off, &mut saved).unwrap(), ("{}\n".to_string(), "撤掉了(接之前没有状态栏)".to_string()));
        let plain = "{\n  \"theme\": \"light\",\n  \"hooks\": {\"Stop\": [{\"hooks\": []}]}\n}\n";
        let mut saved = None;
        let (made, _) = step(plain, Mode::Aca, &mut saved).unwrap();
        assert!(
            made.starts_with("{\n  \"theme\": \"light\",\n  \"hooks\": {\"Stop\": [{\"hooks\": []}]},\n  \"statusLine\": {\n    \"type\""),
            "{made}"
        );
        assert_eq!(step(&made, Mode::Off, &mut saved).unwrap().0, plain);
        let mut saved = None;
        let (made, _) = step("{\"a\":1}", Mode::Aca, &mut saved).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&made).unwrap()["statusLine"]["command"], "/opt/bin/aca statusline");
        assert_eq!(step(&made, Mode::Off, &mut saved).unwrap().0, "{\"a\":1}");
        // 只记额度:没有自己的状态栏就没东西可包
        assert_eq!(step(plain, Mode::Wrap, &mut None), None);
        assert_eq!(step(plain, Mode::Off, &mut None), None);
    }

    #[test]
    fn what_was_wired_by_hand_is_recognised_and_undone_without_a_record() {
        // 手改成 aca 的(没有记录):认得出;撤掉 = 去掉那一项
        let by_hand = "{\n  \"a\": 1,\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/Users/x/.local/bin/aca statusline\",\n    \"refreshInterval\": 10\n  }\n}\n";
        assert_eq!(now(by_hand), Ok(Now::Aca));
        // 路径不是现在这个 aca:换成现在的,别的字段不动
        let (fixed, said) = step(by_hand, Mode::Aca, &mut None).unwrap();
        assert_eq!((fixed, said.as_str()), (by_hand.replace("/Users/x/.local/bin/aca", ACA), "状态栏的命令换成了现在这个 aca"));
        assert_eq!(step(by_hand, Mode::Off, &mut None).unwrap().0, "{\n  \"a\": 1\n}\n");
        // 手包的一层(路径带引号的也认):撤掉 = 剥掉那一层
        let wrapped = "{\"statusLine\":{\"type\":\"command\",\"command\":\"'/my apps/aca' limits -- npx -y ccstatusline@latest\"}}";
        assert_eq!(now(wrapped), Ok(Now::Wrapped("npx -y ccstatusline@latest".into())));
        assert_eq!(
            step(wrapped, Mode::Off, &mut None).unwrap().0,
            "{\"statusLine\":{\"type\":\"command\",\"command\":\"npx -y ccstatusline@latest\"}}"
        );
        // 名字里带 aca 的别的程序不算
        assert_eq!(now("{\"statusLine\":{\"command\":\"/x/aca-bar statusline\"}}"), Ok(Now::Own("/x/aca-bar statusline".into())));
        assert_eq!(now("{\"statusLine\":{\"command\":\"bash aca statusline\"}}"), Ok(Now::Own("bash aca statusline".into())));
    }

    #[test]
    fn a_file_that_cannot_be_understood_is_refused() {
        assert!(plan("{ // 注释\n \"a\": 1 }", Mode::Aca, ACA, None).unwrap_err().contains("不是 JSON"));
        assert!(plan("[1, 2]", Mode::Aca, ACA, None).unwrap_err().contains("不是一个 JSON 对象"));
        assert!(plan("{\"statusLine\": \"bash x.sh\"}", Mode::Aca, ACA, None).unwrap_err().contains("不是带 command 的对象"));
        // statusLine 写成 null 的当成没有
        assert_eq!(now("{\"statusLine\": null}"), Ok(Now::Nothing));
    }

    #[test]
    fn members_are_found_through_strings_and_nesting() {
        let text = " {\"a\": \"x,}\\\\\\\"\" , \"b\":[1,{\"c\":\"]\"}],\n\"d\" : true}";
        let (m, close) = members(text).unwrap();
        assert_eq!(
            m.iter().map(|m| (m.key.as_str(), &text[m.value.clone()])).collect::<Vec<_>>(),
            [("a", "\"x,}\\\\\\\"\""), ("b", "[1,{\"c\":\"]\"}]"), ("d", "true")]
        );
        assert_eq!(&text[close..], "}");
        // 去掉第一个、中间的、最后一个、唯一的
        assert_eq!(put("{\"a\":1, \"b\":2, \"c\":3}", "a", None).unwrap(), "{\"b\":2, \"c\":3}");
        assert_eq!(put("{\"a\":1, \"b\":2, \"c\":3}", "b", None).unwrap(), "{\"a\":1, \"c\":3}");
        assert_eq!(put("{\"a\":1, \"b\":2, \"c\":3}", "c", None).unwrap(), "{\"a\":1, \"b\":2}");
        assert_eq!(put("{\n  \"a\": 1\n}\n", "a", None).unwrap(), "{}\n");
        assert_eq!(members("{\"a\" 1}"), None);
        assert_eq!(members("{\"a\": 1"), None);
    }

    #[test]
    fn written_files_keep_their_mode_and_links_stay_links() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("aca-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dot")).unwrap();
        std::fs::create_dir_all(root.join("acct")).unwrap();
        // 配置目录里的 settings.json 是个链接(放在 dotfiles 里管的)
        let real = root.join("dot/settings.json");
        std::fs::write(&real, "{}\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, root.join("acct/settings.json")).unwrap();
        let old = read(&root.join("acct")).unwrap();
        let change = plan(&old, Mode::Aca, ACA, None).unwrap().unwrap();
        write(&root.join("acct/settings.json"), &old, &change.text).unwrap();
        assert!(std::fs::symlink_metadata(root.join("acct/settings.json")).unwrap().file_type().is_symlink());
        assert_eq!(now(&std::fs::read_to_string(&real).unwrap()), Ok(Now::Aca));
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        // 没有 settings.json 的目录:读成空对象,写出来是新文件
        std::fs::create_dir_all(root.join("new")).unwrap();
        assert_eq!(read(&root.join("new")).unwrap(), "{}\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    // ---------- 那张表 ----------

    const MINE: &str = "{\n  \"model\": \"opus\",\n  \"statusLine\": {\"type\": \"command\", \"command\": \"bash ~/bar.sh\"},\n  \"theme\": \"light\"\n}\n";
    const BY_HAND: &str =
        "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/somewhere/else/aca statusline\"\n  }\n}\n";

    /// 一个临时的家:config.toml、记录都在里面,`~` 也是它
    fn home(name: &str, config: &str) -> (PathBuf, Env) {
        let root = std::env::temp_dir().join(format!("aca-setup-page-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("aca")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let env = Env {
            config: root.join("aca/config.toml"),
            home: root.to_string_lossy().into_owned(),
            record: root.join("state/statusline.json"),
            aca: ACA.into(),
            sessions: || vec![(Agent::Claude, "busy".to_string(), "甲".to_string()), (Agent::Claude, "busy".to_string(), "乙".to_string())],
            ask: |a, name| match (a, name) {
                (Agent::Codex, _) => accounts::Login::In("ChatGPT".into()),
                (_, "work") => accounts::Login::In("max · me@example.com".into()),
                _ => accounts::Login::Out,
            },
        };
        if !config.is_empty() {
            std::fs::write(&env.config, config.replace("~/", &format!("{}/", env.home))).unwrap();
        }
        (root, env)
    }

    fn account_dir(root: &Path, name: &str, settings: Option<&str>) {
        std::fs::create_dir_all(root.join(name)).unwrap();
        if let Some(s) = settings {
            std::fs::write(root.join(name).join("settings.json"), s).unwrap();
        }
    }

    fn settings(root: &Path, name: &str) -> String {
        std::fs::read_to_string(root.join(name).join("settings.json")).unwrap()
    }

    fn press(p: &mut Page, keys: &[KeyCode]) -> Flow {
        keys.iter().fold(Flow::Stay, |_, k| p.on_key(KeyEvent::new(*k, KeyModifiers::NONE), &mut Fx::default()))
    }

    fn type_in(p: &mut Page, text: &str) {
        for c in text.chars() {
            press(p, &[KeyCode::Char(c)]);
        }
    }

    /// 输入框清空(^U)再打
    fn retype(p: &mut Page, text: &str) {
        p.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &mut Fx::default());
        type_in(p, text);
    }

    fn shown(p: &Page, w: usize) -> Vec<String> {
        p.lines(w).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    fn picks(p: &Page) -> Vec<(&str, Option<Mode>)> {
        p.rows.iter().map(|r| (r.name.as_str(), r.wire.as_ref().map(|w| w.pick))).collect()
    }

    #[test]
    fn the_table_writes_only_the_accounts_whose_choice_changed() {
        let (root, env) = home(
            "wire",
            "[accounts.claude]\ngone = \"~/.claude-gone\"\nmine = \"~/.claude-mine\"\nplain = \"~/.claude-plain\"\n\n[accounts.codex]\ncx = \"~/.codex-cx\"\n",
        );
        account_dir(&root, ".claude-mine", Some(MINE));
        account_dir(&root, ".claude-plain", Some("{\n  \"theme\": \"light\"\n}\n"));
        account_dir(&root, ".claude", Some(BY_HAND));
        account_dir(&root, ".codex-cx", None);
        let mut p = Page::load(env.clone(), true);
        // 登记的在前(目录不在的列着但不接),没登记的 ~/.claude 跟在后面,再是 Codex
        assert_eq!(
            picks(&p),
            [("gone", None), ("mine", Some(Mode::Off)), ("plain", Some(Mode::Off)), ("claude", Some(Mode::Aca)), ("cx", None)]
        );
        // 有自己状态栏的才有「自己的 + 记额度」
        let choices = |p: &Page, i: usize| p.rows[i].wire.as_ref().unwrap().choices();
        assert_eq!((choices(&p, 1), choices(&p, 2)), (vec![Mode::Aca, Mode::Wrap, Mode::Off], vec![Mode::Aca, Mode::Off]));
        // 目录不在的那行、Codex 那行:←→ 没东西可换
        press(&mut p, &[KeyCode::Right]);
        assert_eq!(p.pending(), (0, 0));
        press(&mut p, &[KeyCode::Down, KeyCode::Right, KeyCode::Right]);
        assert_eq!(p.rows[1].wire.as_ref().unwrap().pick, Mode::Wrap);
        // a:都照这一行 —— 选不了这种的(没有自己状态栏的)不动
        press(&mut p, &[KeyCode::Char('a')]);
        assert_eq!(picks(&p)[1..4], [("mine", Some(Mode::Wrap)), ("plain", Some(Mode::Off)), ("claude", Some(Mode::Aca))]);
        press(&mut p, &[KeyCode::Down, KeyCode::Left]);
        assert_eq!((p.rows[2].wire.as_ref().unwrap().pick, p.pending()), (Mode::Aca, (0, 2)));
        let text = shown(&p, 78);
        assert!(text.iter().any(|l| l.contains("plain") && l.contains("* ‹ aca 画 ›")), "{text:#?}");
        assert!(text.iter().any(|l| l.contains("回车后 → aca 画")) && text.last().unwrap().ends_with("改 2 个状态栏"), "{text:#?}");
        // 回车之前什么都没写
        assert_eq!((settings(&root, ".claude-mine").as_str(), env.record.exists()), (MINE, false));
        assert_eq!(press(&mut p, &[KeyCode::Enter]), Flow::Close);
        let report = p.done.take().unwrap();
        assert_eq!(
            report.lines,
            [
                "状态栏",
                "  mine:状态栏还是你的,前面加了一层 aca limits 记额度",
                "  plain:接上了 aca 的状态栏",
                "开着的 Claude 会话马上就换,不用重启。"
            ]
        );
        assert_eq!((report.failed, report.brief().as_str()), (false, "改了 2 个账号的状态栏"));
        assert_eq!(settings(&root, ".claude-mine"), MINE.replace("bash ~/bar.sh", "/opt/bin/aca limits -- bash ~/bar.sh"));
        assert_eq!(now(&settings(&root, ".claude-plain")), Ok(Now::Aca));
        // 没挑它的不碰:手接的那个命令里不是现在这个 aca,也留着
        assert_eq!(settings(&root, ".claude"), BY_HAND);
        // 再开:现在的接法就是刚挑的;都挑回不接,和接之前一个字节不差,记录也清了
        let mut p = Page::load(env.clone(), true);
        assert_eq!(picks(&p)[1..3], [("mine", Some(Mode::Wrap)), ("plain", Some(Mode::Aca))]);
        press(&mut p, &[KeyCode::Down, KeyCode::Right, KeyCode::Down, KeyCode::Right, KeyCode::Enter]);
        assert_eq!(p.done.take().unwrap().lines[1..3], ["  mine:恢复成了你原来的状态栏", "  plain:撤掉了(接之前没有状态栏)"]);
        assert_eq!(
            (settings(&root, ".claude-mine").as_str(), settings(&root, ".claude-plain").as_str()),
            (MINE, "{\n  \"theme\": \"light\"\n}\n")
        );
        assert_eq!(env.saved(), Saved::new());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn registering_an_account_waits_for_enter() {
        let config = "# 注释留着\n[accounts.claude]\nmine = \"~/.claude-mine\"\n\n[ui]\nhide = []\n";
        let (root, env) = home("add", config);
        let on_disk = std::fs::read_to_string(&env.config).unwrap();
        account_dir(&root, ".claude-mine", Some(BY_HAND));
        let mut p = Page::load(env.clone(), true);
        press(&mut p, &[KeyCode::Char('+')]);
        // 第一步挑哪个工具:两个摆出来,不是在打字;回车才到名字
        assert!(!p.typing() && shown(&p, 78).iter().any(|l| l.starts_with("› Claude Code")), "{:#?}", shown(&p, 78));
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.typing());
        // 名字:写得不对、重了的当场说
        type_in(&mut p, "a b");
        press(&mut p, &[KeyCode::Enter]);
        assert!(shown(&p, 78).iter().any(|l| l == "账号名只能用字母、数字、- 和 _"));
        retype(&mut p, "mine");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "已经有这个账号:mine");
        // Esc 退回第一步换成 Codex,再换回来
        press(&mut p, &[KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        assert!(shown(&p, 78).iter().any(|l| l == "加 Codex 账号 · 名字"), "{:#?}", shown(&p, 78));
        press(&mut p, &[KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        retype(&mut p, "work");
        press(&mut p, &[KeyCode::Enter]);
        // 目录填好了默认的;Esc 退回名字,再回车又到目录
        assert_eq!(p.form.as_ref().map(|f| (f.name.as_str(), f.text.as_str())), Some(("work", "~/.claude-work")));
        press(&mut p, &[KeyCode::Esc]);
        assert_eq!(p.form.as_ref().map(|f| (f.name.as_str(), f.text.as_str())), Some(("", "work")));
        press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        // 列进表里了:排在 Claude 的最后,高亮在它上面,状态栏先照别的账号摆着
        assert!(!p.typing());
        assert_eq!((picks(&p), p.sel, p.pending()), (vec![("mine", Some(Mode::Aca)), ("work", Some(Mode::Aca))], 1, (1, 1)));
        let text = shown(&p, 78);
        assert!(text.iter().any(|l| l.contains("work") && l.contains("~/.claude-work  新登记")), "{text:#?}");
        assert!(
            text.iter().any(|l| l.contains("再打出登录的命令")) && text.last().unwrap().ends_with("登记 1 个账号,改 1 个状态栏"),
            "{text:#?}"
        );
        // 同一个目录这次不能登记两回;Codex 的排在最后,没有状态栏那一格
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "work2");
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.claude-work/");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "这个目录这次已经登记给 work 了");
        // Esc 三下(目录 → 名字 → 哪个工具 → 不加了),再加一个 Codex 的
        press(&mut p, &[KeyCode::Esc, KeyCode::Esc, KeyCode::Esc]);
        assert!(p.form.is_none());
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Down, KeyCode::Enter]);
        type_in(&mut p, "cx");
        press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        assert_eq!(picks(&p)[2], ("cx", None));
        // 到这什么都没写;Esc 关掉就是什么都没发生
        let untouched =
            |root: &Path| std::fs::read_to_string(root.join("aca/config.toml")).unwrap() == on_disk && !root.join(".claude-work").exists();
        assert!(untouched(&root));
        let mut gone = Page::load(env.clone(), true);
        press(&mut gone, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut gone, "nope");
        assert_eq!(press(&mut gone, &[KeyCode::Enter, KeyCode::Enter, KeyCode::Esc]), Flow::Close);
        assert!(gone.done.is_none() && untouched(&root) && !root.join(".claude-nope").exists());
        // 回车:登记、接状态栏、把登录的命令打出来
        assert_eq!(press(&mut p, &[KeyCode::Enter]), Flow::Close);
        let report = p.done.take().unwrap();
        assert_eq!(
            report.lines,
            [
                "账号",
                "  claude:work:登记了,配置目录 ~/.claude-work(新建的)",
                "  codex:cx:登记了,配置目录 ~/.codex-cx(新建的)",
                "状态栏",
                "  work:接上了 aca 的状态栏",
                "新建的配置目录还没登录,各登录一次(走的是工具自己的登录):",
                "  aca run claude:work",
                "  aca run codex:cx -- login",
                "开着的 Claude 会话马上就换,不用重启。",
            ]
        );
        assert_eq!(report.brief(), "登记了 2 个账号,改了 1 个账号的状态栏");
        // config.toml:各加一行,别的原样(登记的目录写成 ~/…)
        assert_eq!(
            std::fs::read_to_string(&env.config).unwrap(),
            on_disk.replace("\n\n[ui]", "\nwork = \"~/.claude-work\"\n\n[ui]") + "\n[accounts.codex]\ncx = \"~/.codex-cx\"\n"
        );
        assert_eq!(now(&settings(&root, ".claude-work")), Ok(Now::Aca));
        assert!(root.join(".codex-cx").is_dir() && !root.join(".codex-cx/settings.json").exists());
        // 记录里是这个目录的真实路径:接之前没有状态栏
        assert_eq!(env.saved(), Saved::from([(root.join(".claude-work").to_string_lossy().into_owned(), None)]));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_unregistered_claude_dir_can_be_given_a_name_or_left_alone() {
        let (root, env) = home("plain", "");
        account_dir(&root, ".claude", Some(MINE));
        let mut p = Page::load(env.clone(), true);
        assert_eq!(picks(&p), [("claude", Some(Mode::Off))]);
        press(&mut p, &[KeyCode::Right, KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "main");
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.claude");
        press(&mut p, &[KeyCode::Enter]);
        // 还是那一行,换了名字,挑好的接法带着
        assert_eq!((picks(&p), p.pending()), (vec![("main", Some(Mode::Aca))], (1, 1)));
        // x:不登记了,它原来那行回来
        press(&mut p, &[KeyCode::Char('x')]);
        assert_eq!((picks(&p), p.pending()), (vec![("claude", Some(Mode::Aca))], (0, 1)));
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "main");
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.claude");
        press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        // 目录本来就在:不用登录,不打登录的命令
        assert_eq!(
            p.done.take().unwrap().lines,
            [
                "账号",
                "  claude:main:登记了,配置目录 ~/.claude",
                "状态栏",
                "  main:换成了 aca 的状态栏(原来的记下了,挑回不接 / --statusline off 就恢复)",
                "开着的 Claude 会话马上就换,不用重启。"
            ]
        );
        assert_eq!(std::fs::read_to_string(&env.config).unwrap(), "[accounts.claude]\nmain = \"~/.claude\"\n");
        assert_eq!(now(&settings(&root, ".claude")), Ok(Now::Aca));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unreadable_account_is_shown_and_never_touched() {
        let (root, env) = home("bad", "[accounts.claude]\nbad = \"~/.claude-bad\"\nok = \"~/.claude-ok\"\n");
        account_dir(&root, ".claude-bad", Some("{ // 注释\n}\n"));
        account_dir(&root, ".claude-ok", None);
        let mut p = Page::load(env.clone(), false);
        assert!(shown(&p, 78).iter().any(|l| l.contains("bad") && l.contains("读不懂,不动")));
        // 读不懂的那行换不了;a 也带不上它;这张表不给加账号(界面的账号页自己有 +)
        press(&mut p, &[KeyCode::Right, KeyCode::Char('+'), KeyCode::Down, KeyCode::Right, KeyCode::Char('a')]);
        assert!(!p.typing() && !shown(&p, 78).last().unwrap().contains("登记账号"));
        assert_eq!((p.rows[0].wire.as_ref().unwrap().pick, p.pending()), (Mode::Off, (0, 1)));
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.done.take().unwrap().lines[..2], ["状态栏", "  ok:接上了 aca 的状态栏"]);
        assert_eq!(settings(&root, ".claude-bad"), "{ // 注释\n}\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_table_fits_wide_and_narrow_terminals() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let (root, env) =
            home("draw", "[accounts.claude]\na-rather-long-name = \"~/.claude-mine\"\n\n[accounts.codex]\ncx = \"~/.codex-cx\"\n");
        account_dir(&root, ".claude-mine", Some(MINE));
        let mut p = Page::load(env, true);
        press(&mut p, &[KeyCode::Right]);
        for w in [30, 44, 60, 78] {
            let over: Vec<String> = shown(&p, w).into_iter().filter(|l| l.width() > w).collect();
            assert!(over.is_empty(), "{w} 宽放不下:{over:#?}");
        }
        // 手机那么宽:接法那一格、要办什么、回车那个键都还在
        let narrow = shown(&p, 42);
        assert!(narrow.iter().any(|l| l.contains("* ‹ aca 画 ›")) && narrow.iter().any(|l| l == "改 1 个状态栏"), "{narrow:#?}");
        assert!(narrow.last().unwrap().starts_with("⏎ 照办 • ←→ 换接法"), "{narrow:#?}");
        // 说明那一块行数固定:高亮挪到没有状态栏可接的那行,表不跳
        let tall = shown(&p, 78).len();
        press(&mut p, &[KeyCode::Down]);
        assert_eq!(shown(&p, 78).len(), tall);
        for (w, h) in [(120, 40), (80, 24), (46, 30), (20, 6)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &p)).unwrap();
            let screen: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(w < 40 || screen.contains("账") && screen.contains("Codex"), "{w}x{h}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn accounts_are_registered_from_the_command_line_too() {
        let (root, env) = home("cli", "");
        let run = |args: &[&str]| account_cmd(args.iter().map(|a| a.to_string()).collect(), &env);
        assert_eq!(run(&["add", "claude", "work", "--statusline", "aca"]), Ok(0));
        assert_eq!(std::fs::read_to_string(&env.config).unwrap(), "[accounts.claude]\nwork = \"~/.claude-work\"\n");
        assert_eq!(now(&settings(&root, ".claude-work")), Ok(Now::Aca));
        // 目录可以自己给;不带 --statusline 就不接
        assert_eq!(run(&["add", "claude", "side", "~/elsewhere"]), Ok(0));
        assert!(root.join("elsewhere").is_dir() && !root.join("elsewhere/settings.json").exists());
        assert_eq!(run(&["add", "codex", "cx"]), Ok(0));
        assert_eq!(run(&["ls"]), Ok(0));
        assert_eq!(run(&[]), Ok(0));
        // 写错的:说哪里不对,什么都不加
        let before = std::fs::read_to_string(&env.config).unwrap();
        assert!(run(&["add", "codex", "c2", "--statusline", "aca"]).unwrap_err().contains("只对 Claude"));
        assert!(run(&["add", "gemini", "g"]).unwrap_err().contains("只有 claude、codex 有账号"));
        assert!(run(&["add", "claude"]).unwrap_err().contains("没给账号的名字\n用法:aca account ls"));
        assert!(run(&["add", "claude", "work"]).unwrap_err().contains("已经有这个账号"));
        assert!(run(&["add", "claude", "x", "~/d", "more"]).unwrap_err().contains("不认 more"));
        assert!(run(&["mv", "work"]).unwrap_err().contains("account 不认 mv"));
        assert!(run(&["add", "claude", "y", "--statusline", "nope"]).unwrap_err().contains("不是 nope"));
        assert_eq!(std::fs::read_to_string(&env.config).unwrap(), before);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rm_takes_one_registration_away_and_says_first_who_uses_it() {
        let (root, env) = home(
            "cli-rm",
            "[tools.claude]\naccount = \"main\"\n\n[accounts.claude]\nbusy = \"~/.claude-busy\"\nmain = \"~/.claude-main\"\nwork = \"~/.claude-work\"\n",
        );
        account_dir(&root, ".claude-work", Some(MINE));
        let run = |args: &[&str]| account_cmd(args.iter().map(|a| a.to_string()).collect(), &env);
        let config = || std::fs::read_to_string(&env.config).unwrap();
        let before = config();
        // 有会话在用的、是工具默认账号的:不删,说为什么、怎么硬删
        let busy = run(&["rm", "claude", "busy"]).unwrap_err();
        assert!(busy.starts_with("没删 claude:busy:有 2 个会话在用它") && busy.ends_with("确实要删:加 --force"), "{busy}");
        assert!(run(&["rm", "claude", "main"]).unwrap_err().contains("aca new claude 不写账号时用的是它([tools.claude] account)"));
        // 写错的:什么都不动
        assert!(run(&["rm", "claude"]).unwrap_err().contains("account rm 要写哪一家和账号的名字\n用法:aca account ls"));
        assert!(run(&["rm", "gemini", "x"]).unwrap_err().contains("只有 claude、codex 有账号"));
        assert_eq!(run(&["rm", "claude", "nope"]).unwrap_err(), "没有这个账号:claude:nope");
        assert_eq!(config(), before);
        // 没人用的:只去掉那一行,目录和里面的东西都在
        let without = |text: &str, name: &str| text.lines().filter(|l| !l.starts_with(name)).map(|l| format!("{l}\n")).collect::<String>();
        assert_eq!(run(&["rm", "claude", "work"]), Ok(0));
        assert_eq!(config(), without(&before, "work = "));
        assert_eq!(settings(&root, ".claude-work"), MINE);
        assert_eq!(run(&["rm", "--force", "claude", "busy"]), Ok(0));
        assert_eq!(config(), without(&without(&before, "work = "), "busy = "));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn status_says_for_each_account_whether_its_tool_is_logged_in() {
        let (root, env) = home(
            "cli-status",
            "[accounts.claude]\nfirm = \"~/.claude-firm\"\ngone = \"~/.claude-gone\"\noops = \"~/.cx\"\nwork = \"~/.claude-work\"\n\n[accounts.codex]\ncx = \"~/.codex-cx\"\n",
        );
        account_dir(&root, ".claude-firm", None);
        account_dir(&root, ".claude-work", None);
        account_dir(&root, ".codex-cx", None);
        // Codex 的目录登记成了 Claude 的账号
        account_dir(&root, ".cx", None);
        std::fs::write(root.join(".cx/auth.json"), "{}").unwrap();
        std::fs::write(root.join(".cx/config.toml"), "").unwrap();
        let said: Vec<(String, String)> =
            accounts::listed(&env.text(), &env.home).iter().map(|(a, abs)| (a.name.clone(), account_state(&env, a, abs))).collect();
        let want = [
            ("firm", "没登录(登录:aca run claude:firm -- auth login)"),
            ("gone", "配置目录不在"),
            ("oops", "是 Codex 的配置目录(里面有 auth.json、config.toml),登记错了:aca account rm claude oops"),
            ("work", "登录着:max · me@example.com"),
            ("cx", "登录着:ChatGPT"),
        ];
        assert_eq!(said, want.map(|(n, s)| (n.to_string(), s.to_string())));
        let run = |args: &[&str]| account_cmd(args.iter().map(|a| a.to_string()).collect(), &env);
        assert_eq!(run(&["status"]), Ok(0));
        assert!(run(&["status", "x"]).unwrap_err().contains("account status 不认 x"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
