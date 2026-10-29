//! `aca setup`: the two things to settle after installing — which accounts to register, and whether each Claude account's
//! status bar is wired up.
//!
//! In a terminal it's a table ([`Page`]): one row per account; to the right of a Claude account is how its status bar is wired,
//! switched with ←→; `+` registers an account (name + config directory; writes the same line as `+` on the UI's settings page).
//! Nothing happens until Enter; Esc changes nothing.
//! Given `--statusline`, or not in a terminal: no table; every account is wired that one way / just listed.
//! `aca account add` is the way to register an account without the table.
//!
//! Accounts aren't scanned or guessed: only a registered "name = config directory" counts. Logging in is the tool's own business;
//! this only prints that command.
//!
//! Claude gives subscription quotas only in the status bar's input, so to see quotas, statusLine in the settings.json of each of
//! its accounts has to point at aca. Three ways to wire it: `aca` (the status bar is drawn by `aca statusline`), `wrap` (keep your
//! own status bar, with an `aca limits --` layer in front that only records quotas), `off` (not wired; one that was wired is undone,
//! back to how it was before).
//!
//! These are someone else's config files, so:
//! - only the text of the one `statusLine` value is replaced, and no other byte moves (not read in and written back whole:
//!   that would change key order and indentation);
//! - after the edit it's read once as JSON, and written only if everything but statusLine is unchanged; writing goes to a temp
//!   file, then a rename;
//! - the first time an account is touched, its original statusLine text is recorded in `<state>/setup/statusline.json`,
//!   and `off` restores from it;
//! - anything unreadable (not JSON, statusLine not an object with a command) is left alone, and you're told which line to add.

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
            x => Err(tr!("--statusline 只能是 aca、wrap 或 off,不能是 {x}", "--statusline is one of aca, wrap, off, not {x}")),
        }
    }

    /// What the table shows
    fn label(self) -> &'static str {
        match self {
            Mode::Aca => t!("aca 的", "aca's"),
            Mode::Wrap => t!("自己的 + 额度", "yours + quota"),
            Mode::Off => t!("不接入", "none"),
        }
    }
}

/// What an account's statusLine is now
#[derive(Clone, Debug, PartialEq)]
enum Now {
    /// No settings.json, or no statusLine in it
    Nothing,
    /// `aca statusline`
    Aca,
    /// `aca limits -- <your command>`
    Wrapped(String),
    /// Your own command
    Own(String),
}

/// The statusLine text from before wiring, by account directory; `None` = there was none before
type Saved = BTreeMap<String, Option<String>>;

/// What to write back, and what the "text from before wiring" record should become after this (`None` = record unchanged)
#[derive(Debug, PartialEq)]
struct Change {
    text: String,
    saved: Option<Option<Option<String>>>,
    said: String,
}

/// The places read and written (temp directories in tests)
#[derive(Clone, Debug)]
struct Env {
    /// config.toml
    config: PathBuf,
    home: String,
    /// Where the statusLine text from before wiring is recorded
    record: PathBuf,
    /// The program written into statusLine
    aca: String,
    /// What each registered session uses (agent, account, title): `account rm` checks who uses it before deleting
    sessions: fn() -> Vec<(Agent, String, String)>,
    /// Ask the tool whether this account is logged in (`account status`)
    ask: fn(Agent, &str) -> accounts::Login,
    /// Whether this program is installed (`account status` lists only the installed tools' own defaults)
    installed: fn(&str) -> bool,
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
            installed: accounts::on_path,
        }
    }

    /// The full text of config.toml (none = empty)
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

/// What each registered session uses (agent, account, title; untitled ones get the directory name)
pub(crate) fn session_accounts() -> Vec<(Agent, String, String)> {
    let cfg = aca_core::config::load();
    let all = aca_core::store::Store::open().and_then(|s| s.sessions(false)).unwrap_or_default();
    let name = |s: &aca_core::store::Session| match s.title.trim() {
        "" => s.dir.rsplit('/').find(|p| !p.is_empty()).unwrap_or(&s.id).to_string(),
        t => t.to_string(),
    };
    all.into_iter().map(|s| (crate::data::agent_of_tool(&cfg, &s.tool), s.account.clone(), name(&s))).collect()
}

/// Whose Claude status bar this config directory uses now (written in the right-hand column of the accounts page, named as in
/// the table; the table here is where to change it), and whether aca gets its quotas
pub(crate) fn bar_of(dir: &Path) -> (&'static str, bool) {
    match read(dir).and_then(|t| now(&t)) {
        Ok(Now::Aca) => (Mode::Aca.label(), true),
        Ok(Now::Wrapped(_)) => (Mode::Wrap.label(), true),
        Ok(Now::Nothing | Now::Own(_)) => (Mode::Off.label(), false),
        Err(_) => (t!("无法解析", "unreadable"), false),
    }
}

/// `aca setup [--statusline aca|wrap|off]`
pub fn run(args: &[String]) -> i32 {
    told(setup(args.to_vec()))
}

/// `aca account [ls]`, `aca account status`, `aca account add <claude | codex> <name> [dir] [--statusline aca|wrap]`,
/// `aca account rm <claude | codex> <name> [--force]`
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

/// Take out `--flag value`; a flag without a value is a mistake
fn value_of(args: &mut Vec<String>, flag: &str) -> Res<Option<String>> {
    let Some(i) = args.iter().position(|a| a == flag) else { return Ok(None) };
    if i + 1 >= args.len() {
        return Err(tr!("{flag} 后面需要一个值", "{flag} needs a value after it"));
    }
    args.remove(i);
    Ok(Some(args.remove(i)))
}

/// A broken config.toml is treated as not written: commands someone typed get a one-line reminder
fn config_warning() {
    if let Some(e) = aca_core::config::problem() {
        eprintln!("{}", tr!("config.toml 有误,已忽略:{e}", "config.toml has a problem and is ignored: {e}"));
    }
    if let Some(e) = aca_core::config::login_problem() {
        eprintln!("{}", tr!("config.toml 的 [login] 有误,已忽略此节:{e}", "config.toml's [login] has a problem and is ignored: {e}"));
    }
}

fn setup(mut args: Vec<String>) -> Res<i32> {
    let mode = value_of(&mut args, "--statusline")?.map(|m| Mode::parse(&m)).transpose()?;
    if let Some(x) = args.first() {
        return Err(tr!(
            "setup 不支持 {x}(用法:aca setup [--statusline aca|wrap|off])",
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

/// In a terminal: open the table, and print what was done once it closes
fn table(env: Env) -> Res<i32> {
    let mut page = Page::load(env, true);
    show(&mut page).map_err(|e| {
        tr!(
            "无法打开设置表:{e}\n也可以直接用命令:aca setup --statusline aca|wrap|off、aca account add",
            "can't open the table: {e}\nwithout the table: aca setup --statusline aca|wrap|off, aca account add"
        )
    })?;
    let Some(report) = page.done.take() else {
        println!("{}", tr!("未做更改。之后可运行 aca setup 修改", "Nothing changed. To change it later: aca setup"));
        return Ok(0);
    };
    if report.lines.is_empty() {
        println!("{}", tr!("无需更改。之后可运行 aca setup 修改", "Nothing to change. To change it later: aca setup"));
    }
    report.lines.iter().for_each(|l| println!("{l}"));
    Ok(i32::from(report.failed))
}

/// No table: list what each Claude account is now; given a wiring mode, wire every account that way
fn every(mode: Option<Mode>, env: &Env) -> Res<i32> {
    let dirs = claude_dirs(&env.config(), &env.home);
    println!("{}", tr!("Claude 的状态栏和订阅额度", "Claude's status bar and subscription quotas"));
    println!(
        "{}",
        tr!(
            "  Claude 只通过状态栏提供订阅额度(5 小时 / 7 天的用量):状态栏经过 aca 后,计费页顶部才能显示 Claude 的额度。",
            "  Claude only hands out subscription quotas (how much of 5 hours / 7 days is used) in the status bar's input: route the status bar through aca and the billing panel shows Claude's quotas on top."
        )
    );
    println!("{}", tr!("  不接入也不影响其他功能。\n", "  Without it everything else works as usual.\n"));
    if dirs.is_empty() {
        println!(
            "{}",
            tr!(
                "  找不到 Claude 的配置目录(~/.claude 或 config.toml 的 [accounts.claude] 中登记的目录)。",
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
    // Whether every readable account is wired (unreadable ones are never touched, so they don't count)
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
                    "已全部接入。更换方式:aca setup --statusline aca|wrap;撤销:aca setup --statusline off",
                    "All wired. To switch: aca setup --statusline aca|wrap; to undo: aca setup --statusline off"
                )
            );
        } else {
            println!(
                "{}",
                tr!(
                    "接入:aca setup --statusline aca(换成 aca 的状态栏)或 wrap(保留自己的,只记录额度);撤销:--statusline off",
                    "To wire: aca setup --statusline aca (switch to aca's status bar) or wrap (keep yours, just record quotas); to undo: --statusline off"
                )
            );
        }
        println!(
            "{}",
            tr!(
                "在终端中直接运行 aca setup 会打开设置表,可逐个账号选择接入方式,也可登记账号。",
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
            // An unreadable one was never wired: when unwiring, it doesn't count as a failure
            Err(e) => {
                failed |= mode != Mode::Off;
                println!("{}", tr!("  {name}:未更改({e})", "  {name}: unchanged ({e})"));
            }
        }
    }
    env.keep(&saved);
    if failed && mode == Mode::Aca {
        println!(
            "{}",
            tr!(
                "\n未能修改的可手动添加:\"statusLine\": {}",
                "\nFor the ones not changed you can add it yourself: \"statusLine\": {}",
                fresh(&env.aca, "").replace('\n', " ")
            )
        );
    }
    println!(
        "{}",
        tr!(
            "\n已打开的 Claude 会话会立即生效,无需重启。{}",
            "\nOpen Claude sessions switch immediately, no restart needed. {}",
            if mode == Mode::Off { "" } else { t!("撤销:aca setup --statusline off", "To undo: aca setup --statusline off") }
        )
    );
    Ok(i32::from(failed))
}

const ACCOUNT_USAGE: Tr = Tr::new(
    "用法:aca account ls\n     aca account status\n     aca account add <claude | codex> <名字> [目录] [--statusline aca|wrap]\n     aca account rm <claude | codex> <名字> [--force]",
    "usage: aca account ls\n       aca account status\n       aca account add <claude | codex> <NAME> [DIR] [--statusline aca|wrap]\n       aca account rm <claude | codex> <NAME> [--force]",
);

/// Which agent the command line names
fn family(tool: &str) -> Res<Agent> {
    match tool {
        "claude" => Ok(Agent::Claude),
        "codex" => Ok(Agent::Codex),
        x => Err(tr!("只有 claude、codex 支持账号,不支持 {x}", "only claude and codex have accounts, not {x}")),
    }
}

/// The line for one account in `aca account status`: whether the directory is right, whether it's logged in (asks the tool itself)
fn account_state(env: &Env, a: &accounts::Account, abs: &str) -> String {
    use accounts::Login;
    if let Some((other, why)) = (!abs.is_empty()).then(|| accounts::foreign(a.agent, abs, &env.home)).flatten() {
        return tr!(
            "是 {} 的配置目录({why}),登记有误:aca account rm {} {}",
            "a {} config directory ({why}), registered by mistake: aca account rm {} {}",
            accounts::who(other),
            accounts::fam(a.agent).unwrap_or("claude"),
            a.name
        );
    }
    if !abs.is_empty() && !Path::new(abs).is_dir() {
        return t!("配置目录不存在", "config directory missing").into();
    }
    match (env.ask)(a.agent, &a.name) {
        Login::In(who) if who.is_empty() => t!("已登录", "logged in").into(),
        Login::In(who) => tr!("已登录:{who}", "logged in: {who}"),
        Login::Out => {
            let how = if a.agent == Agent::Codex { "login" } else { "auth login" };
            tr!("未登录(登录:aca run {} -- {how})", "not logged in (to log in: aca run {} -- {how})", spec(a.agent, &a.name))
        }
        Login::Unknown(said) => tr!("无法确认登录状态:{said}", "can't tell whether it's logged in: {said}"),
    }
}

/// What `account status` lists: the registered ones, plus the one each agent uses when none is registered (account name empty,
/// full directory path empty too — not checked for belonging to the other agent)
fn status_rows(env: &Env) -> Vec<(accounts::Account, String)> {
    let text = env.text();
    let own = accounts::defaults(&text, &env.home, env.installed).into_iter().map(|a| (a, String::new()));
    accounts::listed(&text, &env.home).into_iter().chain(own).collect()
}

fn account_cmd(mut args: Vec<String>, env: &Env) -> Res<i32> {
    let mode = value_of(&mut args, "--statusline")?.map(|m| Mode::parse(&m)).transpose()?;
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None | Some("ls") => {
            let all: Vec<accounts::Account> = accounts::listed(&env.text(), &env.home).into_iter().map(|(a, _)| a).collect();
            fn place(a: &accounts::Account) -> &str {
                if a.dir.is_empty() { t!("(不更换配置目录)", "(same config directory)") } else { &a.dir }
            }
            let cells: Vec<(&str, &str, &str)> =
                all.iter().map(|a| (accounts::fam(a.agent).unwrap_or("claude"), a.name.as_str(), place(a))).collect();
            // Colored in a terminal (print.rs); as is when piped elsewhere
            if !crate::print::accounts(&cells) {
                let wide = all.iter().map(|a| spec(a.agent, &a.name).len()).max().unwrap_or(0);
                all.iter().for_each(|a| println!("{:wide$}  {}", spec(a.agent, &a.name), place(a)));
            }
            if all.is_empty() {
                println!(
                    "{}",
                    tr!(
                        "尚未登记账号(不登记时使用 ~/.claude、~/.codex)。",
                        "No accounts registered yet (it works without: that's ~/.claude, ~/.codex)."
                    )
                );
                println!(
                    "{}",
                    tr!(
                        "登记账号:aca account add <claude | codex> <名字> [目录],或运行 aca setup",
                        "To register: aca account add <claude | codex> <NAME> [DIR], or aca setup"
                    )
                );
            }
            Ok(0)
        }
        Some("add") => {
            let mut need = |what: &str| args.next().ok_or_else(|| tr!("缺少{what}\n{ACCOUNT_USAGE}", "missing {what}\n{ACCOUNT_USAGE}"));
            let (tool, name) = (need(t!("工具(claude / codex)", "which one (claude / codex)"))?, need(t!("账号名", "the account name"))?);
            let agent = family(&tool)?;
            if mode.is_some() && agent != Agent::Claude {
                return Err(t!(
                    "--statusline 仅适用于 Claude 账号",
                    "--statusline is only for Claude accounts (Codex quotas don't come from the status bar)"
                )
                .into());
            }
            let dir = args.next().unwrap_or_else(|| accounts::default_dir(agent, &name));
            if let Some(x) = args.next() {
                return Err(tr!("account add 不支持 {x}\n{ACCOUNT_USAGE}", "account add doesn't take {x}\n{ACCOUNT_USAGE}"));
            }
            let added = accounts::add_in(&env.config, &env.home, agent, &name, &dir)?;
            let real = std::fs::canonicalize(&added.dir).unwrap_or_else(|_| PathBuf::from(&added.dir));
            println!(
                "{}",
                tr!(
                    "已登记 {}:配置目录 {}{}",
                    "registered {}: config directory {}{}",
                    spec(agent, &name),
                    env.tilde(&real),
                    if added.created { t!("(新建)", " (created)") } else { "" }
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
                            println!("{}", tr!("状态栏未更改({e})", "status bar unchanged ({e})"));
                        }
                    }
                    env.keep(&saved);
                }
                None if agent == Agent::Claude => match read(&real).and_then(|t| now(&t)) {
                    Ok(Now::Aca | Now::Wrapped(_)) => {
                        println!("{}", tr!("状态栏已接入 aca。", "The status bar already goes through aca."))
                    }
                    _ => println!(
                        "{}",
                        tr!(
                            "状态栏尚未接入(接入后计费页才会显示该账号的额度):aca setup",
                            "The status bar isn't wired yet (the billing panel needs it for this account's quotas): aca setup"
                        )
                    ),
                },
                None => {}
            }
            if added.created {
                println!("{}", tr!("新建的目录尚未登录:{}", "the new directory isn't logged in yet: {}", login_hint(agent, &name)));
            }
            Ok(i32::from(failed))
        }
        // Whether each account is logged in: one thread per account asks its tool
        Some("status") => {
            if let Some(x) = args.next() {
                return Err(tr!("account status 不支持 {x}\n{ACCOUNT_USAGE}", "account status doesn't take {x}\n{ACCOUNT_USAGE}"));
            }
            let all = status_rows(env);
            if !all.iter().any(|(a, _)| !a.name.is_empty()) {
                println!(
                    "{}",
                    if all.is_empty() {
                        t!("尚未登记账号。", "No accounts registered yet.")
                    } else {
                        t!(
                            "尚未登记账号;未指定账号时使用各工具的默认目录:",
                            "No accounts registered yet; without one, each tool uses its own:"
                        )
                    }
                );
            }
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
            Ok(0)
        }
        // Only remove the registration line; if a session uses it or it's a tool's default account, say so first, and delete only
        // with --force
        Some("rm") => {
            let mut rest: Vec<String> = args.collect();
            let force = rest.iter().any(|a| a == "--force");
            rest.retain(|a| a != "--force");
            let [tool, name] = rest.as_slice() else {
                return Err(tr!(
                    "account rm 需要指定工具和账号名\n{ACCOUNT_USAGE}",
                    "account rm takes which one and the account name\n{ACCOUNT_USAGE}"
                ));
            };
            let agent = family(tool)?;
            let used = (env.sessions)().into_iter().filter(|(g, n, _)| *g == agent && n == name).count();
            let mut why = vec![];
            if used > 0 {
                why.push(tr!(
                    "{used} 个会话正在使用它(删除后这些会话再启动时会提示账号不存在)",
                    "{used} sessions use it (once removed, restarting them fails with no such account)"
                ));
            }
            for t in accounts::default_of(&env.config(), agent, name) {
                why.push(tr!(
                    "aca new {t} 未指定账号时使用它([tools.{t}] account)",
                    "it's the default for aca new {t} ([tools.{t}] account)"
                ));
            }
            if !force && !why.is_empty() {
                return Err(tr!(
                    "未删除 {}:{}\n确定要删除请加 --force",
                    "{} not removed: {}\nto remove it anyway: add --force",
                    spec(agent, name),
                    why.join(t!(";", "; "))
                ));
            }
            let gone = accounts::remove_in(&env.config, &env.home, agent, name)?;
            println!(
                "{}",
                tr!(
                    "已删除 {} 的登记(config.toml 中的对应行;原文件已备份为 config.toml.bak-aca)",
                    "removed the registration of {} (its line in config.toml; the file as it was is kept as config.toml.bak-aca)",
                    spec(agent, name)
                )
            );
            if !gone.dir.is_empty() {
                println!("{}", tr!("目录 {} 及其中的登录信息未改动。", "The directory {} and the login in it are untouched.", gone.dir));
            }
            Ok(0)
        }
        Some(x) => Err(tr!("account 不支持 {x}\n{ACCOUNT_USAGE}", "account doesn't take {x}\n{ACCOUNT_USAGE}")),
    }
}

/// `claude:work`
fn spec(agent: Agent, name: &str) -> String {
    crate::tools::spec(accounts::fam(agent).unwrap_or("claude"), name)
}

/// The command to log in a new account (the tool's own login; aca only brings along the account's environment): a new Claude
/// directory goes to login on its first start
fn login_hint(agent: Agent, name: &str) -> String {
    format!("aca run {}{}", spec(agent, name), if agent == Agent::Codex { " -- login" } else { "" })
}

fn state_said(state: &Res<Now>) -> String {
    match state {
        Ok(Now::Nothing) => t!("无状态栏", "no status bar").to_string(),
        Ok(Now::Aca) => t!("aca 的状态栏", "aca's status bar").to_string(),
        Ok(Now::Wrapped(_)) => t!("你自己的状态栏,由 aca 记录额度", "your own status bar, aca records quotas").to_string(),
        Ok(Now::Own(_)) => t!("你自己的状态栏", "your own status bar").to_string(),
        Err(e) => tr!("无法解析,不做修改({e})", "can't read it, leaving it alone ({e})"),
    }
}

/// Change this directory's settings.json according to `mode`, updating the record along with it; returns what was done
/// (a line even when nothing needed doing)
fn wire(dir: &Path, mode: Mode, aca: &str, saved: &mut Saved) -> Res<String> {
    let key = dir.to_string_lossy().into_owned();
    let text = read(dir)?;
    let Some(change) = plan(&text, mode, aca, saved.get(&key))? else {
        let nothing_to_wrap = mode == Mode::Wrap && now(&text)? == Now::Nothing;
        return Ok(if nothing_to_wrap {
            t!("没有自定义状态栏,未修改", "no status bar of its own; unchanged")
        } else {
            t!("无需修改", "nothing to do")
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

/// The program written into statusLine: the full path of the aca running now (quoted if the path has spaces or quotes)
fn program() -> String {
    let exe = aca_core::agent::self_exe();
    let plain = exe.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+@".contains(&b));
    if plain { exe } else { format!("'{}'", exe.replace('\'', "'\\''")) }
}

/// Claude config directories: (name, directory). Registered accounts first; `~/.claude` last if it isn't registered;
/// missing directories don't count
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

/// settings.json's text; no such file = an empty object
fn read(dir: &Path) -> Res<String> {
    match std::fs::read_to_string(dir.join("settings.json")) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}\n".to_string()),
        Err(e) => Err(tr!("无法读取 settings.json:{e}", "can't read settings.json: {e}")),
    }
}

/// Write a temp file, then rename (a link to elsewhere is written through to the file it points at); the original permissions
/// stay. Not written if the content is unchanged
fn write(path: &Path, old: &str, new: &str) -> Res<()> {
    if old == new {
        return Ok(());
    }
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = path.with_file_name(format!(".{}.aca-tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    std::fs::write(&tmp, new).map_err(|e| tr!("无法写入 {}:{e}", "can't write {}: {e}", tmp.display()))?;
    if let Ok(meta) = std::fs::metadata(&path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &path).map_err(|e| tr!("无法替换 {}:{e}", "can't replace {}: {e}", path.display()))
}

/// Which kind this settings.json's statusLine is now; for an unreadable one, why
fn now(text: &str) -> Res<Now> {
    let all: Value = serde_json::from_str(text).map_err(|e| tr!("settings.json 不是有效的 JSON:{e}", "settings.json isn't JSON: {e}"))?;
    let all = all.as_object().ok_or(t!("settings.json 不是 JSON 对象", "settings.json isn't a JSON object"))?;
    let Some(line) = all.get("statusLine").filter(|v| !v.is_null()) else { return Ok(Now::Nothing) };
    let cmd = line
        .get("command")
        .and_then(Value::as_str)
        .ok_or(t!("statusLine 不是包含 command 的对象", "statusLine isn't an object with a command"))?;
    Ok(match ours(cmd) {
        Some(("statusline", _)) => Now::Aca,
        Some(("limits", tail)) => Now::Wrapped(tail.strip_prefix("--").unwrap_or(tail).trim_start().to_string()),
        _ => Now::Own(cmd.to_string()),
    })
}

/// Whether this command calls aca: returns (subcommand, the rest)
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

/// What this settings.json should become under `mode`; `None` = leave it. `saved` is this account's recorded
/// "text from before wiring"
fn plan(text: &str, mode: Mode, aca: &str, saved: Option<&Option<String>>) -> Res<Option<Change>> {
    let state = now(text)?;
    let (members, _) = members(text).ok_or(t!("无法识别 settings.json 的写法", "can't make out how this settings.json is written"))?;
    let indent = indent_of(text, &members);
    let line = members.iter().find(|m| m.key == "statusLine").map(|m| &text[m.value.clone()]);
    let command = |value: &str, cmd: &str| {
        put(value, "command", Some(&Value::from(cmd).to_string()))
            .ok_or(t!("无法识别 statusLine 的写法", "can't make out how this statusLine is written").to_string())
    };
    // Record the original text only the first time it's touched: one already recorded is older, don't overwrite it
    let keep = |before: Option<String>| saved.is_none().then_some(Some(before));
    let (want, said, record): (Option<String>, String, Option<Option<Option<String>>>) = match (mode, &state) {
        (Mode::Aca, Now::Aca) => {
            let fixed = command(line.unwrap_or_default(), &format!("{aca} statusline"))?;
            (Some(fixed), t!("状态栏命令已更新为当前的 aca", "the status bar command now points at this aca").into(), None)
        }
        (Mode::Aca, Now::Nothing) => (Some(fresh(aca, &indent)), t!("已接入 aca 状态栏", "wired up aca's status bar").into(), keep(None)),
        (Mode::Aca, Now::Own(_)) => (
            Some(fresh(aca, &indent)),
            t!(
                "已换成 aca 状态栏(原设置已保存,选「不接入」或 --statusline off 可恢复)",
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
                    "已换成 aca 状态栏(原设置已保存,选「不接入」或 --statusline off 可恢复)",
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
                    "保留你的状态栏,并在前面加了 aca limits 记录额度",
                    "the status bar is still yours, wrapped in aca limits to record quotas"
                )
                .into(),
                keep(line.map(String::from)),
            )
        }
        (Mode::Wrap, Now::Wrapped(own)) if !own.is_empty() => (
            Some(command(line.unwrap_or_default(), &format!("{aca} limits -- {own}"))?),
            t!("记录额度的命令已更新为当前的 aca", "the quota command now points at this aca").into(),
            None,
        ),
        // aca draws it now: if your original one is recorded, switch back to it and add the quota-recording layer
        (Mode::Wrap, Now::Aca) => {
            match saved.and_then(|s| s.as_deref()).map(|before| (before, now(&format!("{{\"statusLine\":{before}}}")))) {
                Some((before, Ok(Now::Own(own)))) => (
                    Some(command(before, &format!("{aca} limits -- {own}"))?),
                    t!(
                        "已换回你的状态栏,并在前面加了 aca limits 记录额度",
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
                t!("已恢复你原来的状态栏", "restored your original status bar")
            } else {
                t!("已撤销(接入前没有状态栏)", "removed (there was no status bar before)")
            };
            (before, said.into(), Some(None))
        }
        (Mode::Off, _) => return Ok(None),
    };
    let new = put(text, "statusLine", want.as_deref())
        .ok_or(t!("无法识别 settings.json 的写法", "can't make out how this settings.json is written"))?;
    // Apart from statusLine, nothing may change
    let strip = |t: &str| serde_json::from_str::<Value>(t).ok().map(|mut v| v.as_object_mut().map(|o| o.remove("statusLine")).map(|_| v));
    match (strip(text), strip(&new)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Err(t!("修改后与原内容不一致,未写入", "the result didn't match the original; not written").into()),
    }
    Ok((new != text).then_some(Change { text: new, saved: record, said }))
}

/// The newly written statusLine; `indent` is what this file uses for one level of indentation
fn fresh(aca: &str, indent: &str) -> String {
    let cmd = Value::from(format!("{aca} statusline"));
    format!(
        "{{\n{indent}{indent}\"type\": \"command\",\n{indent}{indent}\"command\": {cmd},\n{indent}{indent}\"refreshInterval\": 10\n{indent}}}"
    )
}

/// Where one member of a JSON object sits in the text
#[derive(Debug, PartialEq)]
struct Member {
    key: String,
    /// Where the key's opening quote is
    start: usize,
    value: Range<usize>,
}

/// The top-level object's members, and where its closing `}` is. Valid JSON only (it has been read as JSON before this is called)
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

/// Skip from the start of the value at `i` to just past it
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

/// What this file uses for one level of indentation: the whitespace before the first member; all on one line, or an empty object:
/// two spaces
fn indent_of(text: &str, members: &[Member]) -> String {
    let lead = members.first().map(|m| &text[text[..m.start].rfind('\n').map_or(0, |n| n + 1)..m.start]);
    lead.filter(|l| !l.is_empty() && l.chars().all(char::is_whitespace)).unwrap_or("  ").to_string()
}

/// Replace the value of `key` in object `text` with `value` (appended at the end if absent); `None` = remove that member.
/// Everything else as is
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
        // Not the first: remove it along with the comma before it; the first: along with the comma after it
        (Some(i), None) if i > 0 => format!("{}{}", &text[..members[i - 1].value.end], &text[members[i].value.end..]),
        (Some(i), None) => match members.get(i + 1) {
            Some(next) => format!("{}{}", &text[..members[i].start], &text[next.start..]),
            None => format!("{}{}", text[..members[i].start].trim_end(), &text[close..]),
        },
        (None, None) => text.to_string(),
    })
}

// ---------- The table ----------

/// Whether this account has a status bar of its own to keep ("yours + quota" isn't offered to accounts that can't keep one)
fn has_own(state: &Now, saved: Option<&Option<String>>) -> bool {
    match state {
        Now::Own(_) => true,
        Now::Wrapped(own) => !own.is_empty(),
        // aca draws it now: if the one from before wiring is recorded, it can still switch back
        Now::Aca => {
            saved.and_then(|s| s.as_deref()).is_some_and(|before| matches!(now(&format!("{{\"statusLine\":{before}}}")), Ok(Now::Own(_))))
        }
        Now::Nothing => false,
    }
}

/// A Claude account's status bar: what it is now, what to wire it as
#[derive(Clone, Debug)]
struct Wire {
    /// Which directory settings.json is in
    dir: PathBuf,
    /// For an unreadable one, why (it won't be touched)
    now: Res<Now>,
    /// Which wiring mode it counts as now
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

    /// The wiring modes to pick from, in the order ←→ cycles through them
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

/// One row of the table
#[derive(Clone, Debug)]
struct Row {
    agent: Agent,
    name: String,
    /// Config directory, written as ~/… when under home; empty for accounts that don't change directory
    shown: String,
    /// To be registered this time (written to config.toml only on Enter): the expanded directory
    new: Option<String>,
    /// The unregistered `~/.claude` (also when it's the one being registered now: if `x` drops the registration, it has to come back)
    plain: bool,
    /// Only Claude's, and only when the directory exists
    wire: Option<Wire>,
    /// Why a registered Claude account can't be wired (written in the detail block). Codex's quota doesn't depend on the status bar,
    /// so nothing is written for it
    why: &'static str,
}

/// What Enter got done
#[derive(Debug, Default)]
pub struct Report {
    /// One line each, printed after the table closes by the command that opened it
    pub lines: Vec<String>,
    pub failed: bool,
    added: usize,
    wired: usize,
}

impl Report {
    /// The line the footer shows in the UI; empty (says nothing) when nothing was done
    fn brief(&self) -> String {
        let mut parts = vec![];
        if self.added > 0 {
            parts.push(tr!("已添加 {} 个账号", "accounts added: {}", self.added));
        }
        if self.wired > 0 {
            parts.push(tr!("已修改 {} 个账号的状态栏", "status bars changed: {}", self.wired));
        }
        if self.failed {
            parts.push(t!("部分操作未完成", "some didn't work").into());
        }
        parts.join(",")
    }
}

/// The table of accounts and status bars. `aca setup` opens it on its own; in the UI it's reached from accounts on the settings
/// page (which has its own `+`, so adding isn't offered here)
pub struct Page {
    env: Env,
    rows: Vec<Row>,
    sel: usize,
    can_add: bool,
    form: Option<Form>,
    saved: Saved,
    /// Claude mod on (`[tools.claude] mod = true`): quotas come from it, and wiring the status bar only affects looks
    mods: bool,
    /// What Enter got done; None when closed with Esc
    pub done: Option<Report>,
}

/// Claude gives subscription quotas only in the status bar's input, hence the wiring; without it everything else works as usual.
/// With the mod on it isn't needed
const INTRO: Tr = Tr::new(
    "接入状态栏后才能看到 Claude 的额度(计费页、n 菜单、账号页)。",
    "Claude's quotas show up (billing page, n menu, accounts) only with the status bar wired.",
);
const INTRO_MOD: Tr = Tr::new(
    "Claude 的额度已从 mod 获取,这里只设置 Claude 底部的状态栏。",
    "Claude's quotas already come from the mod; this only picks whose status bar Claude shows.",
);
/// How many lines the detail block takes (fixed, so the table doesn't jump as the highlight moves): one sentence plus two lines
/// showing aca's status bar; when narrow (phone), the sentence wraps to two lines
fn detail_rows(w: usize) -> usize {
    if w >= 60 { 3 } else { 5 }
}
/// The wiring column: "‹ " + the longest one + " ›"
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
                // The directory actually belongs to the other agent (~/.codex registered as a Claude account):
                // don't wire a status bar into it
                let foreign = !abs.is_empty() && accounts::foreign(agent, abs, &env.home).is_some();
                let wire = if claude && !abs.is_empty() && !foreign { wire_of(abs, &mut seen) } else { None };
                let why = if !claude || wire.is_some() {
                    ""
                } else if abs.is_empty() {
                    t!("使用默认目录:状态栏与 ~/.claude 相同", "uses the default directory: status bar as ~/.claude's")
                } else if foreign {
                    t!("是 Codex 的目录", "a Codex directory")
                } else if !Path::new(abs).is_dir() {
                    t!("目录不存在", "directory missing")
                } else {
                    t!("与上面的账号是同一目录", "same directory as an account above")
                };
                rows.push(Row { agent, name: a.name.clone(), shown: a.dir.clone(), new: None, plain: false, wire, why });
            }
            // The unregistered ~/.claude is an account too: the one that works without registering
            if claude && let Some(wire) = wire_of(&format!("{}/.claude", env.home), &mut seen) {
                let (shown, wire) = ("~/.claude".to_string(), Some(wire));
                rows.push(Row { agent, name: "claude".into(), shown, new: None, plain: true, wire, why: "" });
            }
        }
        let mods = env.config().tool("claude").is_some_and(|t| t.mods);
        Page { env, rows, sel: 0, can_add, form: None, saved, mods, done: None }
    }

    /// How many accounts to register, and how many accounts' status bars to change
    fn pending(&self) -> (usize, usize) {
        let new = self.rows.iter().filter(|r| r.new.is_some()).count();
        (new, self.rows.iter().filter(|r| r.wire.as_ref().is_some_and(Wire::changed)).count())
    }

    /// ←→: switch the highlighted account to another wiring mode
    fn cycle(&mut self, forward: bool) {
        let Some(w) = self.rows.get_mut(self.sel).and_then(|r| r.wire.as_mut()) else { return };
        let all = w.choices();
        let Some(at) = all.iter().position(|m| *m == w.pick) else { return };
        w.pick = all[(at + if forward { 1 } else { all.len() - 1 }) % all.len()];
    }

    /// a: every account that can take this wiring mode follows the highlighted row
    fn all_like_this(&mut self) {
        let Some(pick) = self.rows.get(self.sel).and_then(|r| r.wire.as_ref()).filter(|w| w.now.is_ok()).map(|w| w.pick) else { return };
        for w in self.rows.iter_mut().filter_map(|r| r.wire.as_mut()) {
            if w.choices().contains(&pick) {
                w.pick = pick;
            }
        }
    }

    /// A newly registered Claude account starts out wired like the other accounts (visible and changeable before Enter)
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

    /// `+` open: keys go to the form (which tool → name → config directory, see acctpage.rs); once the directory is filled in,
    /// it's listed in the table
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
            return f.hint = tr!("该目录本次已添加给 {}", "this directory is already being added for {}", other.name);
        }
        self.form = None;
        let shown = accounts::tilde(&abs, &self.env.home);
        let mut row = Row { agent, name, shown, new: Some(abs.clone()), plain: false, wire: None, why: "" };
        // If the directory already exists, read its current status bar; if not, it's created after Enter
        let real = std::fs::canonicalize(&abs).unwrap_or_else(|_| PathBuf::from(&abs));
        // Registering the very ~/.claude listed while unregistered: replace that row, bringing its chosen wiring along
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
        // Placed last among its own agent's rows
        let at =
            self.rows.iter().rposition(|r| r.agent == agent).map_or(if agent == Agent::Claude { 0 } else { self.rows.len() }, |i| i + 1);
        self.rows.insert(at, row);
        self.sel = at;
    }

    /// Enter: register accounts first, then change status bars (only those whose pick differs from now)
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
                        "  {who}:已添加,目录 {}{}",
                        "  {who}: added, directory {}{}",
                        row.shown,
                        if added.created { t!("(新建)", " (created)") } else { "" }
                    ));
                    if added.created {
                        logins.push(format!("  {}", login_hint(row.agent, &row.name)));
                    }
                    // The directory exists now: the name in the record follows the real path (the same as reopening the table later reads)
                    if let (Some(w), Ok(real)) = (row.wire.as_mut(), std::fs::canonicalize(&added.dir)) {
                        w.dir = real;
                    }
                }
                Err(e) => {
                    r.failed = true;
                    refused.push(i);
                    accounts_said.push(tr!("  {who}:添加失败({e})", "  {who}: not added ({e})"));
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
                    wires_said.push(tr!("  {}:未更改({e})", "  {}: unchanged ({e})", row.name));
                }
            }
        }
        if !wires_said.is_empty() {
            self.env.keep(&self.saved);
        }
        for (title, said) in [
            (t!("账号", "Accounts"), accounts_said),
            (t!("状态栏", "Status bar"), wires_said),
            (t!("新建的目录尚未登录,请分别登录:", "New directories aren't logged in yet; log in once each:"), logins),
        ] {
            if !said.is_empty() {
                r.lines.push(title.to_string());
                r.lines.extend(said);
            }
        }
        if r.wired > 0 {
            r.lines.push(
                t!("已打开的 Claude 会话会立即生效,无需重启。", "Open Claude sessions switch immediately, no restart needed.").into(),
            );
        }
        r
    }

    /// The detail under the highlighted row: what the bottom of Claude looks like with the chosen wiring; for aca's, a sample is
    /// drawn right there
    fn detail(&self, row: &Row, w: usize) -> Vec<Line<'static>> {
        let said = |text: &str, st: Style| {
            let lines = if text.is_empty() { vec![] } else { fold(text, w) };
            lines.into_iter().map(move |l| Line::styled(l, st)).collect::<Vec<_>>()
        };
        let mut out = vec![];
        if let Some(dir) = &row.new {
            let then = if Path::new(dir).exists() {
                t!("回车后添加", "Enter adds it")
            } else {
                t!("回车后添加并创建目录,之后需要登录", "Enter adds it and creates the directory; log in after")
            };
            out.extend(said(then, theme::accent()));
        }
        let Some(wire) = &row.wire else {
            out.extend(said(row.why, kit::dim()));
            return out;
        };
        // That row says "unreadable, untouched"; here's why
        if let Err(e) = &wire.now {
            out.extend(said(e, theme::warn()));
            return out;
        }
        let changed = wire.changed();
        let st = if changed { theme::accent() } else { kit::dim() };
        let text = match (wire.pick, changed) {
            (Mode::Aca, false) => t!("Claude 底部使用 aca 状态栏:", "Claude shows aca's status bar:"),
            (Mode::Aca, true) => t!("回车后 Claude 底部换成 aca 状态栏:", "After Enter, Claude shows aca's status bar:"),
            (Mode::Wrap, false) => {
                t!("Claude 底部保留你的状态栏,aca 从中记录额度", "Claude keeps your own status bar; aca records quotas from it")
            }
            (Mode::Wrap, true) => {
                t!(
                    "回车后 Claude 底部使用你的状态栏,aca 从中记录额度",
                    "After Enter, Claude uses your own status bar; aca records quotas from it"
                )
            }
            // Not wired and not changing: nothing to say (what not wiring loses, the header line already says)
            (Mode::Off, false) => return out,
            (Mode::Off, true) if self.mods => t!("回车后撤销,恢复接入前的设置", "After Enter it's removed, back to how it was"),
            (Mode::Off, true) => {
                t!(
                    "回车后撤销,恢复接入前的设置;该账号将不显示额度",
                    "After Enter it's removed, back to how it was; no quotas for this account"
                )
            }
        };
        out.extend(said(text, st));
        if wire.pick == Mode::Aca {
            let sample = crate::statusline::sample(&row.name);
            let lines: Vec<&str> = sample.lines().collect();
            out.extend(crate::ansi::wrap(&crate::ansi::parse_lines(&lines), w).iter().map(crate::ansi::to_line));
        }
        out
    }

    fn row_line(&self, i: usize, row: &Row, name_w: usize, w: usize) -> Line<'static> {
        let on = i == self.sel && self.form.is_none();
        // Right side: two cells for the changed mark + the wiring cell
        let right_w = 2 + 2 + PICK_W + 2;
        // Name and wiring are always written; short on room (phone), first shrink "new account" to "new", then drop the directory
        let room = w.saturating_sub(1 + name_w + 2 + right_w);
        let tag = match row.new {
            Some(_) if room >= 20 => t!("  新增", "  new account"),
            Some(_) => t!("  新", "  new"),
            None => "",
        };
        let dir_w = room.saturating_sub(2 + tag.width());
        let dir = if row.shown.is_empty() { t!("(默认目录)", "(default directory)") } else { row.shown.as_str() };
        let mut left = vec![Span::raw(" "), Span::styled(kit::clip(&row.name, name_w), crate::ui::agent_style(row.agent))];
        if dir_w >= 8 {
            // Too-long directory keeps its end: several accounts' directories mostly differ only in the last few characters
            left.extend([Span::raw("  "), Span::styled(tail(dir, dir_w), kit::dim())]);
        }
        left.push(Span::styled(tag, theme::accent()));
        let right = match &row.wire {
            None => vec![],
            Some(Wire { now: Err(_), .. }) => {
                vec![Span::styled(format!("    {}", kit::clip(t!("无法解析,不修改", "unreadable, untouched"), PICK_W + 2)), theme::warn())]
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
        let esc = kit::esc_hint(if self.form.is_some() { t!("返回", "back") } else { t!("取消", "change nothing") });
        let mut out = vec![
            kit::modal_title(t!("账号和 Claude 的状态栏", "Accounts and Claude's status bar"), theme::accent(), esc, w),
            Line::raw(""),
        ];
        let intro = if self.mods { &INTRO_MOD } else { &INTRO };
        out.extend(fold(intro, w).into_iter().map(|l| Line::styled(l, kit::dim())));
        out.push(Line::raw(""));
        let name_w = self.rows.iter().map(|r| r.name.width()).max().unwrap_or(0).min(14);
        let mut last = None;
        for (i, row) in self.rows.iter().enumerate() {
            if last != Some(row.agent) {
                let claude = row.agent == Agent::Claude;
                let head = vec![who_span(row.agent)];
                // aca reads Codex's quota from its conversation records itself; no status bar to wire
                let col = t!("状态栏", "Status bar");
                let col = if claude { col } else { t!("额度无需接入", "no wiring needed") };
                let col = vec![Span::styled(format!("    {}", kit::clip(col, PICK_W + 2)), kit::dim())];
                out.push(kit::spread(head, col, w));
                last = Some(row.agent);
            }
            out.push(self.row_line(i, row, name_w, w));
        }
        if self.rows.is_empty() {
            out.push(Line::styled(t!("暂无账号", "No accounts yet"), kit::dim()));
        }
        out.push(Line::raw(""));
        if let Some(f) = &self.form {
            out.extend(f.lines(w, t!("加入列表", "add to the table")));
            return out;
        }
        let mut detail: Vec<Line<'static>> =
            self.rows.get(self.sel).map(|r| self.detail(r, w)).unwrap_or_default().into_iter().take(detail_rows(w)).collect();
        detail.resize(detail_rows(w), Line::raw(""));
        out.extend(detail);
        out.push(Line::raw(""));
        let (new, changed) = self.pending();
        // Nothing to do: write nothing
        let todo = match (new, changed) {
            (0, 0) => String::new(),
            (0, c) => tr!("修改 {c} 个状态栏", "change status bars: {c}"),
            (n, 0) => tr!("添加 {n} 个账号", "add accounts: {n}"),
            (n, c) => tr!("添加 {n} 个账号,修改 {c} 个状态栏", "add accounts: {n}, change status bars: {c}"),
        };
        // When it doesn't fit, the later keys are dropped first: the most important go first
        let mut keys = vec![("⏎", t!("确定", "do it")), ("←→", t!("切换", "switch"))];
        if self.rows.get(self.sel).is_some_and(|r| r.new.is_some()) {
            keys.push(("x", t!("取消", "drop it")));
        }
        if self.can_add {
            keys.push(("+", t!("添加账号", "add")));
        }
        keys.push(("a", t!("全部应用此项", "all like this")));
        let todo = Span::styled(todo, theme::accent());
        // Narrow: what's to be done gets its own line (kept even when empty, so the table doesn't jump), keys on another line
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
            // The row registered this time: drop the registration (if it was ~/.claude, its original row comes back)
            KeyCode::Char('x') | KeyCode::Delete if self.rows.get(self.sel).is_some_and(|r| r.new.is_some()) => {
                let gone = self.rows.remove(self.sel);
                if gone.plain {
                    self.rows.insert(self.sel, Row { name: "claude".into(), shown: "~/.claude".into(), new: None, ..gone });
                }
                self.sel = self.sel.min(self.rows.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let report = self.apply();
                let brief = report.brief();
                if !brief.is_empty() {
                    fx.say(brief);
                }
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

/// Wrap to width; punctuation isn't pushed to the start of the next line (two cells are left for it to hang at the end).
/// English words aren't broken mid-word (as with ui.rs's `wrap_width`): when a line ends partway through a word, that part moves
/// to the next line; a word wider than a line is still hard-broken.
/// Chinese can break anywhere, as before
pub(crate) fn fold(text: &str, w: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        let hangs = "。,、:;!?)」".contains(c);
        if used > 0 && used + cw > if hangs { w } else { w.saturating_sub(2) } {
            let cur = out.last_mut().unwrap();
            let half = match cur.rfind(' ') {
                Some(i) if c.is_ascii_graphic() && i > 0 && cur[i + 1..].chars().all(|x| x.is_ascii_graphic()) => cur.split_off(i + 1),
                _ => String::new(),
            };
            cur.truncate(cur.trim_end().len());
            // Break on a space: that space isn't carried to the start of the next line
            if c == ' ' && half.is_empty() {
                out.push(String::new());
                used = 0;
                continue;
            }
            used = half.width();
            out.push(half);
        }
        out.last_mut().unwrap().push(c);
        used += cw;
    }
    out
}

/// Input box text that's too long keeps only its end (so what was just typed is visible)
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

/// `aca setup` opens this table on its own: takes over the terminal; Enter / Esc / ^C closes it
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
    // If the terminal is gone (window closed) it can't be restored, so don't print either: printing to a gone terminal errors
    // (see exitlog.rs)
    if ratatui::try_restore().is_err() {
        return Ok(());
    }
    out
}

/// Centered with side margins when wide and tall enough; narrow (phone): at the top, full width
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

    /// One copy before wiring and one after; `saved` in between changes along the way, as in `run`
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
        // Four-space indentation, key order, trailing spaces, other settings: all must stay as they were
        let mine = "{\n    \"model\": \"opus\",\n    \"statusLine\": {\"type\": \"command\",  \"command\": \"bash ~/bar.sh --x '{}'\", \"padding\": 2},\n    \"env\": {\"A\": \"}\\\"{\"}\n}\n";
        let mut saved = None;
        let (to_aca, said) = step(mine, Mode::Aca, &mut saved).unwrap();
        assert_eq!(
            to_aca,
            "{\n    \"model\": \"opus\",\n    \"statusLine\": {\n        \"type\": \"command\",\n        \"command\": \"/opt/bin/aca statusline\",\n        \"refreshInterval\": 10\n    },\n    \"env\": {\"A\": \"}\\\"{\"}\n}\n"
        );
        assert!(said.contains("已换成 aca 状态栏"), "{said}");
        assert_eq!(now(&to_aca), Ok(Now::Aca));
        // Wiring again needs no change; switching to quota-only: back to yours, with just one extra layer
        assert_eq!(step(&to_aca, Mode::Aca, &mut saved), None);
        let (wrapped, _) = step(&to_aca, Mode::Wrap, &mut saved).unwrap();
        assert_eq!(wrapped, mine.replace("bash ~/bar.sh", "/opt/bin/aca limits -- bash ~/bar.sh"));
        assert_eq!(now(&wrapped), Ok(Now::Wrapped("bash ~/bar.sh --x '{}'".into())));
        // Unwired: byte for byte as before wiring, and the record is cleared
        let (back, said) = step(&wrapped, Mode::Off, &mut saved).unwrap();
        assert_eq!((back.as_str(), said.as_str(), &saved), (mine, "已恢复你原来的状态栏", &None));
        assert_eq!(step(&back, Mode::Off, &mut saved), None);
    }

    #[test]
    fn an_account_without_a_status_line_gets_one_and_loses_it_again() {
        let mut saved = None;
        // No settings.json (treated as an empty object), only other settings, everything on one line
        let (made, _) = step("{}\n", Mode::Aca, &mut saved).unwrap();
        assert_eq!(
            made,
            "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/opt/bin/aca statusline\",\n    \"refreshInterval\": 10\n  }\n}\n"
        );
        assert_eq!(saved, Some(None));
        assert_eq!(step(&made, Mode::Off, &mut saved).unwrap(), ("{}\n".to_string(), "已撤销(接入前没有状态栏)".to_string()));
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
        // Quota-only: without a status bar of its own there's nothing to wrap
        assert_eq!(step(plain, Mode::Wrap, &mut None), None);
        assert_eq!(step(plain, Mode::Off, &mut None), None);
    }

    #[test]
    fn what_was_wired_by_hand_is_recognised_and_undone_without_a_record() {
        // Changed to aca's by hand (no record): recognized; unwiring = removing that entry
        let by_hand = "{\n  \"a\": 1,\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/Users/x/.local/bin/aca statusline\",\n    \"refreshInterval\": 10\n  }\n}\n";
        assert_eq!(now(by_hand), Ok(Now::Aca));
        // The path isn't this aca: replace it with the current one, leave the other fields alone
        let (fixed, said) = step(by_hand, Mode::Aca, &mut None).unwrap();
        assert_eq!((fixed, said.as_str()), (by_hand.replace("/Users/x/.local/bin/aca", ACA), "状态栏命令已更新为当前的 aca"));
        assert_eq!(step(by_hand, Mode::Off, &mut None).unwrap().0, "{\n  \"a\": 1\n}\n");
        // A layer wrapped by hand (a quoted path works too): unwiring = peeling that layer off
        let wrapped = "{\"statusLine\":{\"type\":\"command\",\"command\":\"'/my apps/aca' limits -- npx -y ccstatusline@latest\"}}";
        assert_eq!(now(wrapped), Ok(Now::Wrapped("npx -y ccstatusline@latest".into())));
        assert_eq!(
            step(wrapped, Mode::Off, &mut None).unwrap().0,
            "{\"statusLine\":{\"type\":\"command\",\"command\":\"npx -y ccstatusline@latest\"}}"
        );
        // Other programs with aca in their name don't count
        assert_eq!(now("{\"statusLine\":{\"command\":\"/x/aca-bar statusline\"}}"), Ok(Now::Own("/x/aca-bar statusline".into())));
        assert_eq!(now("{\"statusLine\":{\"command\":\"bash aca statusline\"}}"), Ok(Now::Own("bash aca statusline".into())));
    }

    #[test]
    fn a_file_that_cannot_be_understood_is_refused() {
        assert!(plan("{ // 注释\n \"a\": 1 }", Mode::Aca, ACA, None).unwrap_err().contains("不是有效的 JSON"));
        assert!(plan("[1, 2]", Mode::Aca, ACA, None).unwrap_err().contains("不是 JSON 对象"));
        assert!(plan("{\"statusLine\": \"bash x.sh\"}", Mode::Aca, ACA, None).unwrap_err().contains("不是包含 command 的对象"));
        // statusLine written as null counts as none
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
        // Remove the first, a middle, the last, the only one
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
        // settings.json in the config directory is a link (managed in dotfiles)
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
        // A directory without settings.json: read as an empty object, written out as a new file
        std::fs::create_dir_all(root.join("new")).unwrap();
        assert_eq!(read(&root.join("new")).unwrap(), "{}\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    // ---------- The table ----------

    const MINE: &str = "{\n  \"model\": \"opus\",\n  \"statusLine\": {\"type\": \"command\", \"command\": \"bash ~/bar.sh\"},\n  \"theme\": \"light\"\n}\n";
    const BY_HAND: &str =
        "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/somewhere/else/aca statusline\"\n  }\n}\n";

    /// A temporary home: config.toml and the records are all in it, and it's `~` too
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
            // Only Claude is installed in this home
            installed: |prog| prog == "claude",
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

    /// Clear the input box (^U) and type again
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
        // Registered first (a missing directory is listed but not wired), then the unregistered ~/.claude, then Codex
        assert_eq!(
            picks(&p),
            [("gone", None), ("mine", Some(Mode::Off)), ("plain", Some(Mode::Off)), ("claude", Some(Mode::Aca)), ("cx", None)]
        );
        // Only those with a status bar of their own get 「自己的 + 额度」 ("yours + quota")
        let choices = |p: &Page, i: usize| p.rows[i].wire.as_ref().unwrap().choices();
        assert_eq!((choices(&p, 1), choices(&p, 2)), (vec![Mode::Aca, Mode::Wrap, Mode::Off], vec![Mode::Aca, Mode::Off]));
        // The row with a missing directory, the Codex row: ←→ has nothing to switch; the detail block says why
        assert!(shown(&p, 78).iter().any(|l| l == "目录不存在"), "{:#?}", shown(&p, 78));
        press(&mut p, &[KeyCode::Right]);
        assert_eq!(p.pending(), (0, 0));
        press(&mut p, &[KeyCode::Down, KeyCode::Right, KeyCode::Right]);
        assert_eq!(p.rows[1].wire.as_ref().unwrap().pick, Mode::Wrap);
        // a: all follow this row — those that can't take it (no status bar of their own) stay as they are
        press(&mut p, &[KeyCode::Char('a')]);
        assert_eq!(picks(&p)[1..4], [("mine", Some(Mode::Wrap)), ("plain", Some(Mode::Off)), ("claude", Some(Mode::Aca))]);
        press(&mut p, &[KeyCode::Down, KeyCode::Left]);
        assert_eq!((p.rows[2].wire.as_ref().unwrap().pick, p.pending()), (Mode::Aca, (0, 2)));
        let text = shown(&p, 78);
        assert!(text.iter().any(|l| l.contains("plain") && l.contains("* ‹ aca 的 ›")), "{text:#?}");
        assert!(text.last().unwrap().ends_with("修改 2 个状态栏"), "{text:#?}");
        // The detail block: what it changes to, and a sample of aca's status bar (the second line is this account's quota)
        let said = text.iter().position(|l| l == "回车后 Claude 底部换成 aca 状态栏:").unwrap_or_else(|| panic!("{text:#?}"));
        assert!(text[said + 1].contains("42%") && text[said + 2].starts_with("plain 5h 23% · 7d 41%"), "{text:#?}");
        // The Codex part: quota needs no wiring
        assert!(text.iter().any(|l| l.starts_with("Codex") && l.trim_end().ends_with("额度无需接入")), "{text:#?}");
        // Nothing written before Enter
        assert_eq!((settings(&root, ".claude-mine").as_str(), env.record.exists()), (MINE, false));
        assert_eq!(press(&mut p, &[KeyCode::Enter]), Flow::Close);
        let report = p.done.take().unwrap();
        assert_eq!(
            report.lines,
            [
                "状态栏",
                "  mine:保留你的状态栏,并在前面加了 aca limits 记录额度",
                "  plain:已接入 aca 状态栏",
                "已打开的 Claude 会话会立即生效,无需重启。"
            ]
        );
        assert_eq!((report.failed, report.brief().as_str()), (false, "已修改 2 个账号的状态栏"));
        assert_eq!(settings(&root, ".claude-mine"), MINE.replace("bash ~/bar.sh", "/opt/bin/aca limits -- bash ~/bar.sh"));
        assert_eq!(now(&settings(&root, ".claude-plain")), Ok(Now::Aca));
        // Not picked, not touched: the hand-wired one, whose command isn't this aca, is kept as well
        assert_eq!(settings(&root, ".claude"), BY_HAND);
        // Reopen: the current wiring is what was just picked; pick not wired for all, and it's byte for byte as before wiring,
        // record cleared
        let mut p = Page::load(env.clone(), true);
        assert_eq!(picks(&p)[1..3], [("mine", Some(Mode::Wrap)), ("plain", Some(Mode::Aca))]);
        press(&mut p, &[KeyCode::Down, KeyCode::Right, KeyCode::Down, KeyCode::Right, KeyCode::Enter]);
        assert_eq!(p.done.take().unwrap().lines[1..3], ["  mine:已恢复你原来的状态栏", "  plain:已撤销(接入前没有状态栏)"]);
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
        // Step one picks which tool: both laid out, not typing; Enter goes on to the name
        assert!(!p.typing() && shown(&p, 78).iter().any(|l| l.starts_with("› Claude Code")), "{:#?}", shown(&p, 78));
        press(&mut p, &[KeyCode::Enter]);
        assert!(p.typing());
        // Name: an invalid or duplicate one is reported right away
        type_in(&mut p, "a b");
        press(&mut p, &[KeyCode::Enter]);
        assert!(shown(&p, 78).iter().any(|l| l == "账号名只能包含字母、数字、- 和 _"));
        retype(&mut p, "mine");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "账号已存在:mine");
        // Esc back to step one, switch to Codex, then back again
        press(&mut p, &[KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        assert!(shown(&p, 78).iter().any(|l| l == "添加 Codex 账号 · 名字"), "{:#?}", shown(&p, 78));
        press(&mut p, &[KeyCode::Esc, KeyCode::Down, KeyCode::Enter]);
        retype(&mut p, "work");
        press(&mut p, &[KeyCode::Enter]);
        // The directory is prefilled with the default; Esc goes back to the name, Enter again reaches the directory
        assert_eq!(p.form.as_ref().map(|f| (f.name.as_str(), f.text.as_str())), Some(("work", "~/.claude-work")));
        press(&mut p, &[KeyCode::Esc]);
        assert_eq!(p.form.as_ref().map(|f| (f.name.as_str(), f.text.as_str())), Some(("", "work")));
        press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        // Listed in the table: last among Claude's, highlighted, its status bar starting out like the other accounts'
        assert!(!p.typing());
        assert_eq!((picks(&p), p.sel, p.pending()), (vec![("mine", Some(Mode::Aca)), ("work", Some(Mode::Aca))], 1, (1, 1)));
        let text = shown(&p, 78);
        assert!(text.iter().any(|l| l.contains("work") && l.contains("~/.claude-work  新增")), "{text:#?}");
        assert!(
            text.iter().any(|l| l.contains("回车后添加并创建目录,之后需要登录"))
                && text.last().unwrap().ends_with("添加 1 个账号,修改 1 个状态栏"),
            "{text:#?}"
        );
        // The same directory can't be registered twice this time; Codex's goes last, without a status bar cell
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "work2");
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.claude-work/");
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.form.as_ref().unwrap().hint, "该目录本次已添加给 work");
        // Esc three times (directory → name → which tool → don't add), then add a Codex one
        press(&mut p, &[KeyCode::Esc, KeyCode::Esc, KeyCode::Esc]);
        assert!(p.form.is_none());
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Down, KeyCode::Enter]);
        type_in(&mut p, "cx");
        press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        assert_eq!(picks(&p)[2], ("cx", None));
        // Nothing written so far; closing with Esc means nothing happened
        let untouched =
            |root: &Path| std::fs::read_to_string(root.join("aca/config.toml")).unwrap() == on_disk && !root.join(".claude-work").exists();
        assert!(untouched(&root));
        let mut gone = Page::load(env.clone(), true);
        press(&mut gone, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut gone, "nope");
        assert_eq!(press(&mut gone, &[KeyCode::Enter, KeyCode::Enter, KeyCode::Esc]), Flow::Close);
        assert!(gone.done.is_none() && untouched(&root) && !root.join(".claude-nope").exists());
        // Enter: register, wire the status bar, print the login commands
        assert_eq!(press(&mut p, &[KeyCode::Enter]), Flow::Close);
        let report = p.done.take().unwrap();
        assert_eq!(
            report.lines,
            [
                "账号",
                "  claude:work:已添加,目录 ~/.claude-work(新建)",
                "  codex:cx:已添加,目录 ~/.codex-cx(新建)",
                "状态栏",
                "  work:已接入 aca 状态栏",
                "新建的目录尚未登录,请分别登录:",
                "  aca run claude:work",
                "  aca run codex:cx -- login",
                "已打开的 Claude 会话会立即生效,无需重启。",
            ]
        );
        assert_eq!(report.brief(), "已添加 2 个账号,已修改 1 个账号的状态栏");
        // config.toml: one line added for each, the rest as is (registered directories written as ~/…)
        assert_eq!(
            std::fs::read_to_string(&env.config).unwrap(),
            on_disk.replace("\n\n[ui]", "\nwork = \"~/.claude-work\"\n\n[ui]") + "\n[accounts.codex]\ncx = \"~/.codex-cx\"\n"
        );
        assert_eq!(now(&settings(&root, ".claude-work")), Ok(Now::Aca));
        assert!(root.join(".codex-cx").is_dir() && !root.join(".codex-cx/settings.json").exists());
        // The record has this directory's real path: no status bar before wiring
        assert_eq!(env.saved(), Saved::from([(root.join(".claude-work").to_string_lossy().into_owned(), None)]));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_unregistered_claude_dir_can_be_given_a_name_or_left_alone() {
        let (root, env) = home("plain", "");
        account_dir(&root, ".claude", Some(MINE));
        // Enter with nothing picked: closes, the footer says nothing
        let mut fx = Fx::default();
        assert_eq!(Page::load(env.clone(), true).on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut fx), Flow::Close);
        assert!(fx.flash.is_none() && settings(&root, ".claude") == MINE);
        let mut p = Page::load(env.clone(), true);
        assert_eq!(picks(&p), [("claude", Some(Mode::Off))]);
        press(&mut p, &[KeyCode::Right, KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "main");
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.claude");
        press(&mut p, &[KeyCode::Enter]);
        // Still that row, with a new name, its chosen wiring carried along
        assert_eq!((picks(&p), p.pending()), (vec![("main", Some(Mode::Aca))], (1, 1)));
        // x: drop the registration, its original row comes back
        press(&mut p, &[KeyCode::Char('x')]);
        assert_eq!((picks(&p), p.pending()), (vec![("claude", Some(Mode::Aca))], (0, 1)));
        press(&mut p, &[KeyCode::Char('+'), KeyCode::Enter]);
        type_in(&mut p, "main");
        press(&mut p, &[KeyCode::Enter]);
        retype(&mut p, "~/.claude");
        press(&mut p, &[KeyCode::Enter, KeyCode::Enter]);
        // The directory already existed: no login needed, no login command printed
        assert_eq!(
            p.done.take().unwrap().lines,
            [
                "账号",
                "  claude:main:已添加,目录 ~/.claude",
                "状态栏",
                "  main:已换成 aca 状态栏(原设置已保存,选「不接入」或 --statusline off 可恢复)",
                "已打开的 Claude 会话会立即生效,无需重启。"
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
        assert!(shown(&p, 78).iter().any(|l| l.contains("bad") && l.contains("无法解析,不修改")));
        // The unreadable row can't switch; a doesn't carry it along either; this table doesn't offer adding accounts
        // (the UI's accounts page has its own +)
        press(&mut p, &[KeyCode::Right, KeyCode::Char('+'), KeyCode::Down, KeyCode::Right, KeyCode::Char('a')]);
        assert!(!p.typing() && !shown(&p, 78).last().unwrap().contains("添加账号"));
        assert_eq!((p.rows[0].wire.as_ref().unwrap().pick, p.pending()), (Mode::Off, (0, 1)));
        press(&mut p, &[KeyCode::Enter]);
        assert_eq!(p.done.take().unwrap().lines[..2], ["状态栏", "  ok:已接入 aca 状态栏"]);
        assert_eq!(settings(&root, ".claude-bad"), "{ // 注释\n}\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn with_the_mod_on_the_status_bar_only_picks_whose_bar_claude_shows() {
        for (mods, intro, off) in [
            (false, "接入状态栏后才能看到 Claude 的额度(计费页、n 菜单、账号页)。", "回车后撤销,恢复接入前的设置;该账号将不显示额度"),
            (true, "Claude 的额度已从 mod 获取,这里只设置 Claude 底部的状态栏。", "回车后撤销,恢复接入前的设置"),
        ] {
            let (root, env) =
                home(&format!("mod-{mods}"), &format!("[tools.claude]\nmod = {mods}\n\n[accounts.claude]\nbar = \"~/.claude-bar\"\n"));
            account_dir(&root, ".claude-bar", Some(BY_HAND));
            let mut p = Page::load(env, false);
            assert!(shown(&p, 78).iter().any(|l| l == intro), "{:#?}", shown(&p, 78));
            // Wired to aca, switched to not wired: with the mod on, quotas are still there
            press(&mut p, &[KeyCode::Right]);
            assert_eq!(p.rows[0].wire.as_ref().unwrap().pick, Mode::Off);
            assert!(shown(&p, 78).iter().any(|l| l == off), "{:#?}", shown(&p, 78));
            let _ = std::fs::remove_dir_all(&root);
        }
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
        // Phone width: the wiring cell, what's to be done and the Enter key are all still there
        let narrow = shown(&p, 42);
        assert!(narrow.iter().any(|l| l.contains("* ‹ aca 的 ›")) && narrow.iter().any(|l| l == "修改 1 个状态栏"), "{narrow:#?}");
        assert!(narrow.last().unwrap().starts_with("⏎ 确定 • ←→ 切换"), "{narrow:#?}");
        // The detail block has a fixed line count: with the highlight moved to a row with no status bar to wire, the table doesn't jump
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
        // The directory can be given; without --statusline nothing is wired
        assert_eq!(run(&["add", "claude", "side", "~/elsewhere"]), Ok(0));
        assert!(root.join("elsewhere").is_dir() && !root.join("elsewhere/settings.json").exists());
        assert_eq!(run(&["add", "codex", "cx"]), Ok(0));
        assert_eq!(run(&["ls"]), Ok(0));
        assert_eq!(run(&[]), Ok(0));
        // Mistakes: say what's wrong, add nothing
        let before = std::fs::read_to_string(&env.config).unwrap();
        assert!(run(&["add", "codex", "c2", "--statusline", "aca"]).unwrap_err().contains("仅适用于 Claude"));
        assert!(run(&["add", "gemini", "g"]).unwrap_err().contains("只有 claude、codex 支持账号"));
        assert!(run(&["add", "claude"]).unwrap_err().contains("缺少账号名\n用法:aca account ls"));
        assert!(run(&["add", "claude", "work"]).unwrap_err().contains("账号已存在"));
        assert!(run(&["add", "claude", "x", "~/d", "more"]).unwrap_err().contains("不支持 more"));
        assert!(run(&["mv", "work"]).unwrap_err().contains("account 不支持 mv"));
        assert!(run(&["add", "claude", "y", "--statusline", "nope"]).unwrap_err().contains("不能是 nope"));
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
        // Used by a session, or a tool's default account: not deleted; says why and how to force it
        let busy = run(&["rm", "claude", "busy"]).unwrap_err();
        assert!(busy.starts_with("未删除 claude:busy:2 个会话正在使用它") && busy.ends_with("确定要删除请加 --force"), "{busy}");
        assert!(run(&["rm", "claude", "main"]).unwrap_err().contains("aca new claude 未指定账号时使用它([tools.claude] account)"));
        // Mistakes: nothing changes
        assert!(run(&["rm", "claude"]).unwrap_err().contains("account rm 需要指定工具和账号名\n用法:aca account ls"));
        assert!(run(&["rm", "gemini", "x"]).unwrap_err().contains("只有 claude、codex 支持账号"));
        assert_eq!(run(&["rm", "claude", "nope"]).unwrap_err(), "账号不存在:claude:nope");
        assert_eq!(config(), before);
        // Unused: only that line goes; the directory and its contents stay
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
        // A Codex directory registered as a Claude account
        account_dir(&root, ".cx", None);
        std::fs::write(root.join(".cx/auth.json"), "{}").unwrap();
        std::fs::write(root.join(".cx/config.toml"), "").unwrap();
        let said: Vec<(String, String)> =
            accounts::listed(&env.text(), &env.home).iter().map(|(a, abs)| (a.name.clone(), account_state(&env, a, abs))).collect();
        let want = [
            ("firm", "未登录(登录:aca run claude:firm -- auth login)"),
            ("gone", "配置目录不存在"),
            ("oops", "是 Codex 的配置目录(其中包含 auth.json、config.toml),登记有误:aca account rm claude oops"),
            ("work", "已登录:max · me@example.com"),
            ("cx", "已登录:ChatGPT"),
        ];
        assert_eq!(said, want.map(|(n, s)| (n.to_string(), s.to_string())));
        let run = |args: &[&str]| account_cmd(args.iter().map(|a| a.to_string()).collect(), &env);
        assert_eq!(run(&["status"]), Ok(0));
        assert!(run(&["status", "x"]).unwrap_err().contains("account status 不支持 x"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn folding_keeps_english_words_whole_and_still_breaks_chinese_anywhere() {
        let en =
            "Claude only hands out subscription quotas in the status bar's input: route it through aca and the billing panel shows them.";
        let lines = fold(en, 30);
        assert!(lines.iter().all(|l| l.width() <= 30 && !l.starts_with(' ') && !l.ends_with(' ')), "{lines:#?}");
        // All break on spaces: putting a space back at each break gives the original
        assert_eq!(lines.join(" "), en);
        // A word longer than a line: hard break
        assert_eq!(fold(&"x".repeat(25), 12), ["x".repeat(10), "x".repeat(10), "x".repeat(5)]);
        // Chinese can break anywhere as before, punctuation hanging at the line end
        assert_eq!(fold("一二三四五六七八。九十", 10), ["一二三四", "五六七八。", "九十"]);
    }

    #[test]
    fn status_also_says_whether_each_tools_own_default_is_logged_in() {
        let rows = |env: &Env| -> Vec<(String, String, String)> {
            status_rows(env).iter().map(|(a, abs)| (spec(a.agent, &a.name), a.dir.clone(), account_state(env, a, abs))).collect()
        };
        // Nothing registered: the installed agent's (Claude's) own is listed, with how to log in; the one not installed (Codex) isn't
        let (root, env) = home("own", "");
        let want = ("claude".to_string(), "~/.claude".to_string(), "未登录(登录:aca run claude -- auth login)".to_string());
        assert_eq!(rows(&env), [want]);
        std::fs::remove_dir_all(&root).unwrap();
        // A default account set in config, or the agent's own directory already registered as an account: that's what is listed,
        // no extra row
        for config in [
            "[tools.claude]\naccount = \"work\"\n\n[accounts.claude]\nwork = \"~/.claude-work\"\n",
            "[accounts.claude]\nofficial = \"~/.claude\"\n",
        ] {
            let (root, env) = home("taken", config);
            let names: Vec<String> = rows(&env).into_iter().map(|(who, _, _)| who).collect();
            assert!(names.len() == 1 && names[0].starts_with("claude:"), "{config}: {names:?}");
            std::fs::remove_dir_all(&root).unwrap();
        }
    }
}
