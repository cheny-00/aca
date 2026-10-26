//! 账号:记在 `~/.config/aca/config.toml` 里 ——
//!
//! ```toml
//! [accounts.claude]
//! work = "~/.claude-work"
//!
//! [accounts.codex]
//! cx = "~/.codex-cx"
//! ```
//!
//! 读、怎么用(起会话时带上配置目录 / 环境变量 / 参数)归 aca-core;这里是登记表上能做的几件事,界面(acctpage.rs 的账号页、
//! setup.rs 的表)和命令行(`aca account …`)共用:
//! - 加一个:往那一节加一行「名字 = "目录"」;删一个:去掉那一行。别的内容和注释原样不动,目录和里面的登录也不动
//! - 顺序:先 Claude 再 Codex,各自照 `[ui] menu`(`n` 菜单的顺序)排;挪一格就是改那一行
//! - 认目录:目录看着是另一家的(把 `~/.codex` 登记成 Claude 的账号)不给加,已经登记成这样的标出来
//! - 登录着没有:问工具自己(`claude auth status` / `codex login status`),aca 不读凭据
//!
//! 登录走它自己的命令:`aca run <家>:<账号> -- login`,带的是这个账号起会话时的那一层环境。

use std::path::Path;

use regex::Regex;
use toml::{Table, Value};

use aca_core::{t, tr};

use crate::data::Agent;

/// 加好了:配置目录(~ 已展开)、是不是刚建的(刚建的要登录)
#[derive(Debug, PartialEq)]
pub struct Added {
    pub dir: String,
    pub created: bool,
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

pub(crate) fn fam(a: Agent) -> Option<&'static str> {
    match a {
        Agent::Claude => Some("claude"),
        Agent::Codex => Some("codex"),
        _ => None,
    }
}

/// 账号名只能是字母、数字、- 和 _(要当 TOML 的键,也要能在终端里直接打)
pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn default_dir(a: Agent, name: &str) -> String {
    if a == Agent::Codex { format!("~/.codex-{name}") } else { format!("~/.claude-{name}") }
}

/// 带着这个账号跑它自己的命令(`aca run <家>:<账号> -- 参数`):环境由 aca 叠好,外层会话留下的身份变量也拿掉了
fn run_cmd(a: Agent, name: &str, args: &str) -> String {
    let aca = aca_core::agent::self_exe().replace('\'', "'\\''");
    let spec = crate::tools::spec(fam(a).unwrap_or("claude"), name);
    if args.is_empty() { format!("'{aca}' run {spec}") } else { format!("'{aca}' run {spec} -- {args}") }
}

/// 新账号登录用的命令:Claude 新目录第一次起来就会走登录;Codex 用 codex login
pub fn login_cmd(a: Agent, name: &str) -> String {
    run_cmd(a, name, if a == Agent::Codex { "login" } else { "" })
}

/// 已有的账号重新登录(设置页的账号里 ⏎):不起会话,只走登录
pub fn relogin_cmd(a: Agent, name: &str) -> String {
    run_cmd(a, name, if a == Agent::Codex { "login" } else { "auth login" })
}

/// ~ / ~/… / $HOME/… 展开,去掉末尾的 /
fn expand(d: &str, home: &str) -> String {
    let d = d.trim();
    let d = match d.strip_prefix("$HOME").or_else(|| d.strip_prefix("${HOME}")) {
        Some(rest) => format!("~{rest}"),
        None => d.to_string(),
    };
    let d = if d.len() > 1 { d.trim_end_matches('/') } else { d.as_str() };
    match d.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None if d == "~" => home.to_string(),
        None => d.to_string(),
    }
}

/// 家目录下的写回成 ~/…
pub(crate) fn tilde(abs: &str, home: &str) -> String {
    match abs.strip_prefix(home) {
        Some("") => "~".into(),
        Some(rest) if rest.starts_with('/') && !home.is_empty() => format!("~{rest}"),
        _ => abs.to_string(),
    }
}

/// 没有这个文件 = 空
fn read(p: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        r => r,
    }
}

/// 这一家记着的账号:(名字, 目录照写的样子;写成表的取它的 home,没有就是空的)。读不了、格式不对当没有
fn list_in(text: &str, fam: &str) -> Vec<(String, String)> {
    let Ok(t) = text.parse::<Table>() else { return vec![] };
    let table = t.get("accounts").and_then(|v| v.get(fam)).and_then(|v| v.as_table());
    let dir = |v: &Value| v.as_str().or_else(|| v.get("home")?.as_str()).unwrap_or("").trim().to_string();
    table.into_iter().flatten().map(|(n, v)| (n.clone(), dir(v))).collect()
}

/// 账号页列的一个
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub agent: Agent,
    pub name: String,
    /// 配置目录,家目录下的写成 ~/…;不换目录的账号(只带环境变量 / 参数)是空的
    pub dir: String,
}

/// 哪一家叫什么(表头、加账号时写的)
pub fn who(a: Agent) -> &'static str {
    if a == Agent::Codex { "Codex" } else { "Claude Code" }
}

/// 登记了的账号:先 Claude 再 Codex,各自照 `[ui] menu` 的先后,没写到的按名字跟在后面
pub fn all() -> Vec<Account> {
    listed(&read(&crate::config::path()).unwrap_or_default(), &home()).into_iter().map(|(a, _)| a).collect()
}

/// config.toml 的全文 → 排好的账号,各带配置目录的完整路径(不换目录的是空的)
pub(crate) fn listed(text: &str, home: &str) -> Vec<(Account, String)> {
    ordered(text, home, &crate::config::list_in(text, "ui", "menu"))
}

fn ordered(text: &str, home: &str, menu: &[String]) -> Vec<(Account, String)> {
    let mut out = vec![];
    for a in [Agent::Claude, Agent::Codex] {
        let mut one: Vec<(Account, String)> = list_in(text, fam(a).unwrap())
            .into_iter()
            .map(|(name, dir)| {
                let abs = if dir.is_empty() { dir } else { expand(&dir, home) };
                (Account { agent: a, name, dir: tilde(&abs, home) }, abs)
            })
            .collect();
        // 稳定排序:没写到的保持按名字的先后
        one.sort_by_key(|(x, _)| menu.iter().position(|n| *n == x.name).unwrap_or(usize::MAX));
        out.extend(one);
    }
    out
}

/// 不登记时各家用的那个(`aca new claude` 不写账号时):账号名空着,目录是它自己的 ~/.claude / ~/.codex。
/// 装着的才算;写了默认账号(`[tools.<家>] account`)的、自己的目录已经登记成账号的不算 —— 那就是列着的那一个。
/// `aca account status`、账号页拿它说登录着没有:没登记过账号的新用户也看得出来
pub(crate) fn defaults(text: &str, home: &str, installed: fn(&str) -> bool) -> Vec<Account> {
    let cfg = aca_core::config::parse(text);
    let listed = listed(text, home);
    let mut out = vec![];
    for a in [Agent::Claude, Agent::Codex] {
        let Some(f) = fam(a) else { continue };
        let Some(tool) = cfg.tool(f) else { continue };
        let own = format!("{home}/.{f}");
        let taken = !tool.account.is_empty() || listed.iter().any(|(x, abs)| x.agent == a && *abs == own);
        let prog = tool.cmd.first().map_or(f, String::as_str);
        if !taken && installed(prog) {
            out.push(Account { agent: a, name: String::new(), dir: format!("~/.{f}") });
        }
    }
    out
}

/// 这个程序装了没有:写了 / 的看那个文件(~ 展开),不然在 PATH 里找
pub(crate) fn on_path(prog: &str) -> bool {
    if prog.contains('/') {
        return Path::new(&expand(prog, &home())).is_file();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(prog).is_file())
}

/// 一个目录里哪些文件是哪一家自己才有的(两家都有的 history.jsonl、sessions、plugins 不算)
const MARKS: [(Agent, &[&str]); 2] = [
    (Agent::Claude, &[".claude.json", "settings.json", "projects", "CLAUDE.md", "shell-snapshots", ".credentials.json"]),
    (Agent::Codex, &["auth.json", "config.toml", ".codex-global-state.json", "installation_id", "version.json", "session_index.jsonl"]),
];

/// 这个目录里有哪一家的哪几样
fn marks(dir: &Path, a: Agent) -> Vec<&'static str> {
    let names = MARKS.iter().find(|(x, _)| *x == a).map_or(&[][..], |(_, n)| *n);
    names.iter().copied().filter(|n| dir.join(n).exists()).collect()
}

/// 这个目录(完整路径)是另一家的配置目录:回是哪一家、凭什么这么说。
/// 另一家默认的那个(`~/.codex` / `~/.claude`)直接算;别的看里面的东西,另一家的比这一家的多才算
/// (登记错了之后起过一次,里面会多出这一家的一两个文件)
pub(crate) fn foreign(a: Agent, abs: &str, home: &str) -> Option<(Agent, String)> {
    let other = match a {
        Agent::Claude => Agent::Codex,
        Agent::Codex => Agent::Claude,
        _ => return None,
    };
    let dir = Path::new(abs);
    let theirs = marks(dir, other);
    if theirs.len() > marks(dir, a).len() {
        let more = if theirs.len() > 2 { "…" } else { "" };
        return Some((other, tr!("其中包含 {}{more}", "it has {}{more}", theirs[..theirs.len().min(2)].join(t!("、", ", ")))));
    }
    let default = format!("{home}/{}", if other == Agent::Codex { ".codex" } else { ".claude" });
    (abs == default).then(|| (other, tr!("{} 的默认目录", "the {} default", who(other))))
}

/// 登记不了的那句话
fn foreign_said(a: Agent, shown: &str, (other, why): &(Agent, String)) -> String {
    tr!(
        "{shown} 是 {} 的目录({why}),不能添加为 {} 账号",
        "{shown} is a {} directory ({why}); it can't be a {} account",
        who(*other),
        who(a)
    )
}

/// 登录着没有,工具自己说的
#[derive(Clone, Debug, PartialEq)]
pub enum Login {
    /// 登录着:怎么登录的、是谁(`max · a@b.c`、`ChatGPT`、`API key sk-…`)
    In(String),
    Out,
    /// 问不出来(工具没装、说的话认不出):原话
    Unknown(String),
}

/// `claude auth status` 打的那段 JSON
fn claude_said(out: &str) -> Login {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(out.trim()) else {
        return Login::Unknown(first_line(out));
    };
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let provider = Some(s("apiProvider")).filter(|p| !p.is_empty() && p != "firstParty");
    if v.get("loggedIn").and_then(|x| x.as_bool()) != Some(true) {
        return provider.map_or(Login::Out, Login::Unknown);
    }
    // 订阅的写哪一档(max / pro / team),不是订阅的写怎么登录的
    let how = [s("subscriptionType"), s("authMethod")].into_iter().find(|x| !x.is_empty());
    let email = s("email");
    // 组织:个人订阅的就叫「邮箱's Organization」,和邮箱是一回事,不写
    let org = Some(s("orgName")).filter(|o| !o.is_empty() && (email.is_empty() || !o.contains(&email)));
    let parts: Vec<String> = [provider, how, Some(email).filter(|e| !e.is_empty()), org].into_iter().flatten().collect();
    Login::In(parts.join(" · "))
}

/// `codex login status` 打的那一行(没登录时退出码不是 0)
fn codex_said(ok: bool, out: &str) -> Login {
    let line = first_line(out);
    if line.starts_with("Not logged in") {
        Login::Out
    } else if let Some(how) = line.strip_prefix("Logged in using ") {
        Login::In(how.trim_start_matches("an ").replace(" - ", " ").trim().to_string())
    } else if line.is_empty() {
        if ok { Login::Unknown(t!("无输出", "said nothing").into()) } else { Login::Out }
    } else {
        Login::Unknown(line)
    }
}

fn first_line(out: &str) -> String {
    out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// 问工具自己这个账号登录着没有:带着账号的环境跑它的命令(`aca run <家>:<账号> -- …`),只读,几秒没答就不等了
pub fn login(a: Agent, name: &str) -> Login {
    use std::process::{Command, Stdio};
    let spec = crate::tools::spec(fam(a).unwrap_or("claude"), name);
    let ask: &[&str] = if a == Agent::Codex { &["login", "status"] } else { &["auth", "status", "--json"] };
    let mut cmd = Command::new(aca_core::agent::self_exe());
    cmd.args(["run", &spec, "--"]).args(ask).current_dir(home()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Login::Unknown(e.to_string()),
    };
    let began = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if began.elapsed() < std::time::Duration::from_secs(8) => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Login::Unknown(t!("无响应", "no answer").into());
            }
        }
    }
    let Ok(out) = child.wait_with_output() else { return Login::Unknown(t!("无响应", "no answer").into()) };
    let (said, err) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    match a {
        // Codex 把这一行打在标准错误上
        Agent::Codex => codex_said(out.status.success(), if said.trim().is_empty() { &err } else { &said }),
        _ if said.trim().is_empty() => Login::Unknown(first_line(&err)),
        _ => claude_said(&said),
    }
}

/// 先备份成 <文件名>.bak-aca,再写临时文件、整个换掉;权限照原来的(没有文件就 600)。
/// 换之前再读一次:和 old 不一样(别人刚写过)就不换,返回 false
pub(crate) fn write(path: &Path, old: &str, new: &str) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
    let save = |p: &Path, text: &str| -> std::io::Result<()> {
        std::fs::write(p, text)?;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
    };
    if !old.is_empty() {
        save(&dir.join(format!("{name}.bak-aca")), old)?;
    }
    let tmp = dir.join(format!("{name}.aca-new"));
    save(&tmp, new)?;
    if read(path)? != old {
        let _ = std::fs::remove_file(&tmp);
        return Ok(false);
    }
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// 去掉 keys 指的那一项,连带因此变空的上级表:比「除了这一项,别的有没有变」用
fn without(mut t: Table, keys: &[&str]) -> Table {
    fn go(t: &mut Table, keys: &[&str]) {
        match keys {
            [] => {}
            [k] => {
                t.remove(*k);
            }
            [k, rest @ ..] => {
                if let Some(Value::Table(sub)) = t.get_mut(*k) {
                    go(sub, rest);
                    if sub.is_empty() {
                        t.remove(*k);
                    }
                }
            }
        }
    }
    go(&mut t, keys);
    t
}

/// 下一张表的表头从哪开始(没有 = 文件末尾)
fn next_table(text: &str, from: usize) -> usize {
    Regex::new(r"(?m)^[ \t]*\[").unwrap().find_at(text, from).map_or(text.len(), |m| m.start())
}

/// 往末尾追加一段前要补的换行:和前面空一行
fn gap(text: &str) -> &'static str {
    if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}

/// config.toml 里 `[accounts.<家>]` 下加一行 `名 = "目录"`:有这一节就接在它最后一个键后面,
/// 没有就在末尾新起一节;别的内容(注释、别的设置)原样不动
fn add_line(text: &str, fam: &str, name: &str, dir: &str) -> Result<String, String> {
    let doc: Table =
        text.parse().map_err(|_| t!("config.toml 格式有误,未修改", "config.toml is malformed; nothing changed").to_string())?;
    let line = format!("{name} = {}\n", Value::String(dir.to_string()));
    let header = Regex::new(&format!(r"(?m)^[ \t]*\[[ \t]*accounts[ \t]*\.[ \t]*{fam}[ \t]*\][ \t]*(?:#[^\n]*)?(?:\r?\n|\z)")).unwrap();
    let out = match header.find(text) {
        Some(h) => {
            let body = &text[h.end()..next_table(text, h.end())];
            let key = Regex::new(r"(?m)^[ \t]*[^#\s\[][^=\n]*=[^\n]*(?:\n|\z)").unwrap();
            let at = key.find_iter(body).last().map_or(h.end(), |m| h.end() + m.end());
            let nl = if text[..at].ends_with('\n') { "" } else { "\n" };
            format!("{}{nl}{line}{}", &text[..at], &text[at..])
        }
        None => format!("{text}{}[accounts.{fam}]\n{line}", gap(text)),
    };
    let back: Table = out.parse().map_err(|_| {
        t!("无法识别 config.toml 中 [accounts] 的写法,未修改", "can't make out [accounts] in config.toml; nothing changed").to_string()
    })?;
    let keys = ["accounts", fam, name];
    let got = back.get("accounts").and_then(|a| a.get(fam)).and_then(|f| f.get(name)).and_then(|d| d.as_str());
    if got != Some(dir) || without(doc, &keys) != without(back, &keys) {
        return Err(t!("写入后校验不一致,未修改", "the result didn't check out; nothing changed").into());
    }
    Ok(out)
}

/// 加之前的核对,不写:名字、目录写得对不对,和已有的重不重,config.toml 改不改得了。
/// 回(config.toml 现在的全文, 加完的全文, 展开的目录)
fn planned(path: &Path, home: &str, a: Agent, name: &str, dir: &str) -> Result<(String, String, String), String> {
    let fam = fam(a).ok_or(t!("只能添加 Claude / Codex 账号", "only Claude / Codex accounts can be added"))?;
    if !valid_name(name) {
        return Err(t!("账号名只能包含字母、数字、- 和 _", "an account name is letters, digits, - and _").into());
    }
    let abs = expand(dir, home);
    if !abs.starts_with('/') {
        return Err(t!("目录必须以 ~/ 或 / 开头", "the directory must look like ~/… or /…").into());
    }
    let text = read(path).map_err(|e| tr!("无法读取 config.toml:{e}", "can't read config.toml: {e}"))?;
    // 同一家里,名字、目录都不能和已有的重
    let known = list_in(&text, fam);
    if known.iter().any(|(n, _)| n == name) {
        return Err(tr!("账号已存在:{name}", "that account already exists: {name}"));
    }
    if let Some((n, _)) = known.iter().find(|(_, d)| !d.is_empty() && expand(d, home) == abs) {
        return Err(tr!("该目录已属于账号 {n}", "this directory already belongs to account {n}"));
    }
    if let Some(theirs) = foreign(a, &abs, home) {
        return Err(foreign_said(a, &tilde(&abs, home), &theirs));
    }
    let new = add_line(&text, fam, name, &tilde(&abs, home))?;
    Ok((text, new, abs))
}

/// 只核对不写(`aca setup` 的表里先列着,回车才一起办):回展开的目录。真写是 [`add_in`]:记进账号表,目录没有就建
pub(crate) fn check_in(path: &Path, home: &str, a: Agent, name: &str, dir: &str) -> Result<String, String> {
    planned(path, home, a, name, dir).map(|(_, _, abs)| abs)
}

pub(crate) fn add_in(path: &Path, home: &str, a: Agent, name: &str, dir: &str) -> Result<Added, String> {
    let (text, new, abs) = planned(path, home, a, name, dir)?;
    if !write(path, &text, &new).map_err(|e| tr!("写入 config.toml 失败:{e}", "writing config.toml failed: {e}"))? {
        return Err(t!("config.toml 已被修改,请重试", "config.toml just changed; add it again").into());
    }
    let created = !Path::new(&abs).exists();
    std::fs::create_dir_all(&abs)
        .map_err(|e| tr!("已添加账号,但无法创建目录:{e}", "account added, but the directory couldn't be created: {e}"))?;
    Ok(Added { dir: abs, created })
}

/// config.toml 里 `[accounts.<家>]` 下去掉 `名 = …` 那一行,别的原样。写成别的样子的(`[accounts.<家>.<名>]` 一张表、
/// 一个值占好几行)认不出,不改
fn drop_line(text: &str, fam: &str, name: &str) -> Result<String, String> {
    let odd = || {
        t!(
            "无法识别该账号在 config.toml 中的写法,未修改(请手动删除)",
            "can't make out how this account is written in config.toml; nothing changed (remove those lines yourself)"
        )
        .to_string()
    };
    let doc: Table =
        text.parse().map_err(|_| t!("config.toml 格式有误,未修改", "config.toml is malformed; nothing changed").to_string())?;
    let line = account_line(text, fam, name).ok_or_else(odd)?;
    let out = format!("{}{}", &text[..line.start], &text[line.end..]);
    let keys = ["accounts", fam, name];
    let back: Table = out.parse().map_err(|_| odd())?;
    let gone = back.get("accounts").and_then(|a| a.get(fam)).and_then(|f| f.get(name)).is_none();
    if !gone || without(doc, &keys) != without(back, &keys) {
        return Err(odd());
    }
    Ok(out)
}

/// `[accounts.<家>]` 下 `名 = …` 那一行在哪(连着行尾的换行)。没有这一节、这一行的 = None
fn account_line(text: &str, fam: &str, name: &str) -> Option<std::ops::Range<usize>> {
    let header = Regex::new(&format!(r"(?m)^[ \t]*\[[ \t]*accounts[ \t]*\.[ \t]*{fam}[ \t]*\][ \t]*(?:#[^\n]*)?(?:\r?\n|\z)")).unwrap();
    let h = header.find(text)?;
    let end = next_table(text, h.end());
    let key = Regex::new(&format!(r#"(?m)^[ \t]*["']?{}["']?[ \t]*=[^\n]*(?:\n|\z)"#, regex::escape(name))).unwrap();
    let line = key.find(&text[h.end()..end])?;
    Some(h.end() + line.start()..h.end() + line.end())
}

/// 一个账号的值写成一行:只有目录的写成 `"目录"`,别的写成 `{ home = …, args = […] }`,目录在前、参数在后
fn inline(v: &Value) -> String {
    let Value::Table(t) = v else { return v.to_string() };
    let first = ["home", "env", "env_file"];
    let mut keys: Vec<&String> = t.keys().filter(|k| *k != "args").collect();
    keys.sort_by_key(|k| first.iter().position(|f| f == k).unwrap_or(first.len()));
    keys.extend(t.keys().filter(|k| *k == "args"));
    let parts: Vec<String> = keys.into_iter().map(|k| format!("{k} = {}", inline(&t[k.as_str()]))).collect();
    if parts.is_empty() { "{}".into() } else { format!("{{ {} }}", parts.join(", ")) }
}

/// config.toml 里换掉一个账号自己的启动参数(`args`)。`名 = "目录"` 加了参数写成 `名 = { home = "目录", args = […] }`,
/// 参数清空后只剩目录的写回 `名 = "目录"`;那一行后面的注释留着。写成一张表的(`[accounts.<家>.<名>]`)改它的 args 那一行。
/// 别的一个字不动;一个值占好几行之类认不出的不改
fn with_args(text: &str, fam: &str, name: &str, args: &[String]) -> Result<String, String> {
    let odd = || {
        t!(
            "无法识别该账号在 config.toml 中的写法,未修改(请手动修改)",
            "can't make out how this account is written in config.toml; nothing changed (edit those lines yourself)"
        )
        .to_string()
    };
    let doc: Table =
        text.parse().map_err(|_| t!("config.toml 格式有误,未修改", "config.toml is malformed; nothing changed").to_string())?;
    let old = doc.get("accounts").and_then(|a| a.get(fam)).and_then(|f| f.get(name)).cloned();
    let old = old.ok_or_else(|| tr!("账号不存在:{fam}:{name}", "no such account: {fam}:{name}"))?;
    let list = Value::Array(args.iter().cloned().map(Value::String).collect());
    let line = account_line(text, fam, name);
    // 改完它该是什么样
    let want = match (&old, &line) {
        (Value::String(dir), Some(_)) if args.is_empty() => Value::String(dir.clone()),
        (Value::String(dir), Some(_)) => {
            Value::Table(Table::from_iter([("home".into(), Value::String(dir.clone())), ("args".into(), list.clone())]))
        }
        (Value::Table(t), Some(_)) => {
            let mut t = t.clone();
            if args.is_empty() {
                t.remove("args");
            } else {
                t.insert("args".into(), list.clone());
            }
            match (t.len(), t.get("home")) {
                (1, Some(Value::String(dir))) => Value::String(dir.clone()),
                _ => Value::Table(t),
            }
        }
        (Value::Table(t), None) => {
            let mut t = t.clone();
            t.insert("args".into(), list.clone());
            Value::Table(t)
        }
        _ => return Err(odd()),
    };
    if want == old {
        return Ok(text.to_string());
    }
    let out = match line {
        Some(line) => {
            let row = &text[line.clone()];
            let eq = row.find('=').ok_or_else(odd)?;
            // 值到哪为止:后面有注释的话,# 前面那一段单独读得出原来的值
            let rest = row[eq + 1..].trim_end_matches(['\n', '\r']);
            let same = |v: &str| format!("x = {v}").parse::<Table>().is_ok_and(|t| t.get("x") == Some(&old));
            let cut = rest.char_indices().filter(|(_, c)| *c == '#').map(|(i, _)| i).chain([rest.len()]).find(|i| same(&rest[..*i]));
            let cut = cut.ok_or_else(odd)?;
            let gap = &rest[rest[..cut].trim_end().len()..cut];
            let tail = &row[eq + 1 + rest.len()..];
            format!("{}{} {}{gap}{}{tail}{}", &text[..line.start], &row[..=eq], inline(&want), &rest[cut..], &text[line.end..])
        }
        // 写成一张表的:改它下面 args 那一行
        None => crate::config::with_value(text, &format!("accounts.{fam}.{name}"), "args", &list).ok_or_else(odd)?,
    };
    let keys = ["accounts", fam, name];
    let back: Table = out.parse().map_err(|_| odd())?;
    let got = back.get("accounts").and_then(|a| a.get(fam)).and_then(|f| f.get(name));
    if got != Some(&want) || without(doc, &keys) != without(back, &keys) {
        return Err(odd());
    }
    Ok(out)
}

/// 换一个账号自己的启动参数(删之前的存成 config.toml.bak-aca)。回 false = 和原来一样,没写
pub(crate) fn set_args_in(path: &Path, a: Agent, name: &str, args: &[String]) -> Result<bool, String> {
    let fam = fam(a).ok_or(t!("仅支持 Claude / Codex 账号", "only Claude / Codex have accounts"))?;
    let text = read(path).map_err(|e| tr!("无法读取 config.toml:{e}", "can't read config.toml: {e}"))?;
    let new = with_args(&text, fam, name, args)?;
    if new == text {
        return Ok(false);
    }
    if !write(path, &text, &new).map_err(|e| tr!("写入 config.toml 失败:{e}", "writing config.toml failed: {e}"))? {
        return Err(t!("config.toml 已被修改,请重试", "config.toml just changed; save again").into());
    }
    Ok(true)
}

/// config.toml 里换掉这一家每个账号都带的启动参数(`[tools.<家>] args`):那一行后面的注释留着(账号那一行也是),
/// 没有这一行加在表头下面,没有这一段加在最后;别的一个字不动。认不出的写法不改
fn with_common_args(text: &str, fam: &str, args: &[String]) -> Result<String, String> {
    let list = Value::Array(args.iter().cloned().map(Value::String).collect());
    let new = crate::config::with_value(text, &format!("tools.{fam}"), "args", &list).ok_or(t!(
        "无法识别 config.toml 的写法,未修改(请手动修改)",
        "can't make out how config.toml is written; nothing changed (edit those lines yourself)"
    ))?;
    // with_value 是原地换掉那一行的:行数一样时,不一样的那行就是它,把原来那行的注释接上
    let (old, got): (Vec<&str>, Vec<&str>) = (text.lines().collect(), new.lines().collect());
    let Some(i) = (old.len() == got.len()).then(|| (0..old.len()).find(|&i| old[i] != got[i])).flatten() else { return Ok(new) };
    let rest = old[i].split_once('=').map_or("", |(_, r)| r);
    // `#` 在字符串里的不算:到它为止读得出一个值,后面的才是注释
    let cut = rest.match_indices('#').map(|(j, _)| j).find(|&j| format!("x = {}", &rest[..j]).parse::<Table>().is_ok());
    let Some(cut) = cut else { return Ok(new) };
    let mut lines: Vec<String> = got.iter().map(|l| l.to_string()).collect();
    lines[i].push_str(&rest[rest[..cut].trim_end().len()..]);
    let out = lines.join("\n") + "\n";
    Ok(if out.parse::<Table>().ok() == new.parse::<Table>().ok() { out } else { new })
}

/// 换这一家每个账号都带的启动参数(删之前的存成 config.toml.bak-aca)。回 false = 和原来一样,没写
pub(crate) fn set_common_args_in(path: &Path, a: Agent, args: &[String]) -> Result<bool, String> {
    let fam = fam(a).ok_or(t!("仅支持 Claude / Codex 账号", "only Claude / Codex have accounts"))?;
    let text = read(path).map_err(|e| tr!("无法读取 config.toml:{e}", "can't read config.toml: {e}"))?;
    if aca_core::config::parse(&text).tool(fam).map(|t| t.args).unwrap_or_default() == args {
        return Ok(false);
    }
    let new = with_common_args(&text, fam, args)?;
    if !write(path, &text, &new).map_err(|e| tr!("写入 config.toml 失败:{e}", "writing config.toml failed: {e}"))? {
        return Err(t!("config.toml 已被修改,请重试", "config.toml just changed; save again").into());
    }
    Ok(true)
}

/// 删掉的那个账号原来写的目录(家目录下的写成 ~/…;不换目录的是空的)
#[derive(Debug, PartialEq)]
pub struct Removed {
    pub dir: String,
}

/// 删一个账号:只去掉 config.toml 里那一行(删之前的存成 config.toml.bak-aca)。配置目录、里面的登录都不动
pub(crate) fn remove_in(path: &Path, home: &str, a: Agent, name: &str) -> Result<Removed, String> {
    let fam = fam(a).ok_or(t!("仅支持 Claude / Codex 账号", "only Claude / Codex have accounts"))?;
    let text = read(path).map_err(|e| tr!("无法读取 config.toml:{e}", "can't read config.toml: {e}"))?;
    let Some((_, dir)) = list_in(&text, fam).into_iter().find(|(n, _)| n == name) else {
        return Err(tr!("账号不存在:{fam}:{name}", "no such account: {fam}:{name}"));
    };
    let new = drop_line(&text, fam, name)?;
    if !write(path, &text, &new).map_err(|e| tr!("写入 config.toml 失败:{e}", "writing config.toml failed: {e}"))? {
        return Err(t!("config.toml 已被修改,请重试", "config.toml just changed; remove it again").into());
    }
    Ok(Removed { dir: if dir.is_empty() { dir } else { tilde(&expand(&dir, home), home) } })
}

/// 哪些工具没写账号时用的是它(`[tools.<名>] account`):删了它这些工具就起不来
pub(crate) fn default_of(cfg: &aca_core::config::Config, a: Agent, name: &str) -> Vec<String> {
    let fam = fam(a).unwrap_or_default();
    cfg.tools.keys().filter(|t| cfg.tool(t).is_some_and(|d| d.kind == fam && d.account == name)).cloned().collect()
}

/// 这个账号在它那一家里往上 / 往下挪一格;已经在头上 / 尾上的回 false。
/// 顺序记在 `[ui] menu`:就是 `n` 菜单的顺序,两处是同一个。挪的时候把整个菜单的顺序写全(以后加的照旧跟在后面)
pub(crate) fn shift_in(path: &Path, home: &str, a: Agent, name: &str, up: bool) -> Result<bool, String> {
    let text = read(path).map_err(|e| tr!("无法读取 config.toml:{e}", "can't read config.toml: {e}"))?;
    let cfg = aca_core::config::parse(&text);
    let menu = crate::config::list_in(&text, "ui", "menu");
    let order = |menu: &[String]| -> Vec<String> {
        ordered(&text, home, menu).into_iter().filter(|(x, _)| x.agent == a).map(|(x, _)| x.name).collect()
    };
    let mut want = order(&menu);
    let Some(at) = want.iter().position(|n| n == name) else {
        return Err(tr!("账号不存在:{name}", "no such account: {name}"));
    };
    let to = if up { at.checked_sub(1) } else { Some(at + 1).filter(|i| *i < want.len()) };
    let Some(to) = to else { return Ok(false) };
    let by = want[to].clone();
    want.swap(at, to);
    // 菜单现在的整个顺序(藏着的也算):只把它拿出来,插到同一家里挨着的那个的前面 / 后面,别的谁都不动
    // (两家在菜单里是混着排的:对调的话,被越过的那个会跟着跳到别处)
    let lay = crate::tools::Layout { menu, hide: vec![] };
    let mut names: Vec<String> = vec![];
    for t in crate::tools::menu(&cfg, &lay).into_iter().filter(|t| t.tool != "shell" && t.name != name) {
        if !names.contains(&t.name) {
            names.push(t.name);
        }
    }
    let Some(j) = names.iter().position(|n| *n == by) else { return Ok(false) };
    names.insert(if up { j } else { j + 1 }, name.to_string());
    // 两家有重名的账号时菜单按名字排不开:挪完不是想要的顺序就不写
    let hand = t!(
        "无法移动(有重名项,或 [ui] menu 分成了多行):请在 config.toml 的 [ui] menu 中手动调整顺序",
        "can't move it (a duplicate name, or [ui] menu spans several lines): order it yourself under [ui] menu in config.toml"
    );
    if order(&names) != want {
        return Err(hand.into());
    }
    let value = Value::Array(names.into_iter().map(Value::String).collect());
    let new = crate::config::with_value(&text, "ui", "menu", &value).ok_or(hand)?;
    if !write(path, &text, &new).map_err(|e| tr!("写入 config.toml 失败:{e}", "writing config.toml failed: {e}"))? {
        return Err(t!("config.toml 已被修改,请重试", "config.toml just changed; move it again").into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_listed_claude_first_then_codex_in_the_menu_order() {
        let text = "[accounts.codex]\ncx = \"/h/.codex-cx\"\nax = \"~/.ax/\"\n[accounts.claude]\nwork = \"/h/.claude-work\"\nbed = { env = { X = \"1\" } }\n";
        let got = |text: &str| -> Vec<(Agent, String, String, String)> {
            listed(text, "/h").into_iter().map(|(a, abs)| (a.agent, a.name, a.dir, abs)).collect()
        };
        let row = |a, n: &str, d: &str, abs: &str| (a, n.to_string(), d.to_string(), abs.to_string());
        // 没写顺序:各自按名字;不换目录的账号目录是空的
        assert_eq!(
            got(text),
            [
                row(Agent::Claude, "bed", "", ""),
                row(Agent::Claude, "work", "~/.claude-work", "/h/.claude-work"),
                row(Agent::Codex, "ax", "~/.ax", "/h/.ax"),
                row(Agent::Codex, "cx", "~/.codex-cx", "/h/.codex-cx"),
            ]
        );
        // [ui] menu 写了的排前面(n 菜单同一个顺序),两家还是各归各
        let with_menu = format!("{text}[ui]\nmenu = [\"cx\", \"work\"]\n");
        let names: Vec<String> = got(&with_menu).into_iter().map(|r| r.1).collect();
        assert_eq!(names, ["work", "bed", "cx", "ax"]);
    }

    #[test]
    fn logging_in_goes_through_the_harness_own_command() {
        assert!(login_cmd(Agent::Claude, "work").ends_with(" run claude:work"));
        assert!(login_cmd(Agent::Codex, "cx").ends_with(" run codex:cx -- login"));
        assert!(relogin_cmd(Agent::Claude, "work").ends_with(" run claude:work -- auth login"));
    }

    #[test]
    fn the_tools_own_words_say_whether_an_account_is_logged_in() {
        let sub = r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "me@example.com", "subscriptionType": "max"}"#;
        assert_eq!(claude_said(sub), Login::In("max · me@example.com".into()));
        assert_eq!(claude_said(r#"{"loggedIn": true, "authMethod": "console", "apiProvider": "firstParty"}"#), Login::In("console".into()));
        // 组织和邮箱不是一回事的才写
        let team = r#"{"loggedIn": true, "email": "me@example.com", "orgName": "Acme", "subscriptionType": "team"}"#;
        assert_eq!(claude_said(team), Login::In("team · me@example.com · Acme".into()));
        let own = r#"{"loggedIn": true, "email": "me@example.com", "orgName": "me@example.com's Organization", "subscriptionType": "max"}"#;
        assert_eq!(claude_said(own), Login::In("max · me@example.com".into()));
        assert_eq!(
            claude_said("{\n  \"loggedIn\": false,\n  \"authMethod\": \"none\",\n  \"apiProvider\": \"firstParty\"\n}\n"),
            Login::Out
        );
        // 不走它自己那套登录的(Bedrock…):说是哪一家,不硬说登没登录
        assert_eq!(claude_said(r#"{"loggedIn": false, "apiProvider": "bedrock"}"#), Login::Unknown("bedrock".into()));
        assert_eq!(claude_said("error: unknown command 'auth'\n"), Login::Unknown("error: unknown command 'auth'".into()));
        assert_eq!(codex_said(true, "Logged in using ChatGPT\n"), Login::In("ChatGPT".into()));
        assert_eq!(codex_said(true, "Logged in using an API key - sk-b33***114\n"), Login::In("API key sk-b33***114".into()));
        assert_eq!(codex_said(false, "Not logged in\n"), Login::Out);
        assert_eq!(codex_said(false, ""), Login::Out);
        assert_eq!(codex_said(false, "error: no such profile\n"), Login::Unknown("error: no such profile".into()));
    }

    #[test]
    fn a_directory_that_belongs_to_the_other_tool_is_recognized() {
        let d = tmp("foreign");
        let home = d.to_string_lossy().to_string();
        let make = |name: &str, files: &[&str]| {
            std::fs::create_dir_all(d.join(name)).unwrap();
            files.iter().for_each(|f| std::fs::write(d.join(name).join(f), "").unwrap());
            format!("{home}/{name}")
        };
        let codex = make(".cx", &["auth.json", "config.toml", "version.json", "history.jsonl"]);
        let claude = make(".cc", &[".claude.json", "settings.json", "history.jsonl"]);
        let who = |a, dir: &str| foreign(a, dir, &home);
        assert_eq!(who(Agent::Claude, &codex), Some((Agent::Codex, "其中包含 auth.json、config.toml…".into())));
        assert_eq!(who(Agent::Codex, &claude), Some((Agent::Claude, "其中包含 .claude.json、settings.json".into())));
        assert_eq!((who(Agent::Codex, &codex), who(Agent::Claude, &claude)), (None, None));
        // 登记错了之后起过一次,里面多了这一家的一个文件:还是另一家的
        std::fs::write(d.join(".cx/.claude.json"), "{}").unwrap();
        assert_eq!(who(Agent::Claude, &codex).map(|x| x.0), Some(Agent::Codex));
        // 另一家默认的那个目录:还没有也算;空目录、新目录谁的都不是
        assert_eq!(who(Agent::Claude, &format!("{home}/.codex")), Some((Agent::Codex, "Codex 的默认目录".into())));
        assert_eq!(who(Agent::Codex, &format!("{home}/.claude")), Some((Agent::Claude, "Claude Code 的默认目录".into())));
        assert_eq!((who(Agent::Claude, &make(".empty", &[])), who(Agent::Codex, &format!("{home}/.nowhere"))), (None, None));
        // 加的时候就拦下:config.toml 不动
        let path = d.join("aca/config.toml");
        assert_eq!(
            add_in(&path, &home, Agent::Claude, "codex", "~/.cx").unwrap_err(),
            "~/.cx 是 Codex 的目录(其中包含 auth.json、config.toml…),不能添加为 Claude Code 账号"
        );
        assert!(!path.exists() && add_in(&path, &home, Agent::Codex, "cx", "~/.cx").is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn launch_args_change_only_that_accounts_line() {
        let args = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let old = "# 我的设置\n[accounts.claude]\nwork = \"~/.claude-work\" # 公司\nme = { home = \"~/.claude-me\", env = { X = \"1\" } }\n\n[accounts.codex]\nwork = \"~/.codex-work\"\n";
        // 一行目录:写成带参数的那种,注释留着
        let out = with_args(old, "claude", "work", &args(&["--model", "sonnet"])).unwrap();
        assert_eq!(
            out,
            old.replace(
                "work = \"~/.claude-work\" # 公司",
                "work = { home = \"~/.claude-work\", args = [\"--model\", \"sonnet\"] } # 公司"
            )
        );
        // 清空了:写回一行目录
        assert_eq!(with_args(&out, "claude", "work", &[]).unwrap(), old);
        // 本来就是一张小表的:加在后面,env 留着;清空后还是表
        let out = with_args(old, "claude", "me", &args(&["a b"])).unwrap();
        assert!(out.contains("me = { home = \"~/.claude-me\", env = { X = \"1\" }, args = [\"a b\"] }\n"), "{out}");
        assert_eq!(with_args(&out, "claude", "me", &[]).unwrap(), old);
        // 同名的另一家不受影响;和原来一样的不改
        let out = with_args(old, "codex", "work", &args(&["--yolo"])).unwrap();
        assert!(
            out.contains("[accounts.codex]\nwork = { home = \"~/.codex-work\", args = [\"--yolo\"] }\n")
                && out.contains("work = \"~/.claude-work\" # 公司")
        );
        assert_eq!(with_args(old, "codex", "work", &[]).unwrap(), old);
        // 写成一张表的:改它的 args 那一行
        let table = "[accounts.codex.labs]\nhome = \"~/.labs\"\n";
        assert_eq!(
            with_args(table, "codex", "labs", &args(&["-p", "x"])).unwrap(),
            "[accounts.codex.labs]\nargs = [\"-p\", \"x\"]\nhome = \"~/.labs\"\n"
        );
        // 一个值占好几行、没这个账号、坏文件:不改
        assert!(with_args("[accounts.codex]\nlabs = { home = \"~/.labs\", args = [\n\"a\"] }\n", "codex", "labs", &[]).is_err());
        assert_eq!(with_args(old, "claude", "nobody", &[]).unwrap_err(), "账号不存在:claude:nobody");
        assert!(with_args("[accounts\n", "claude", "work", &[]).is_err());
    }

    #[test]
    fn every_accounts_args_change_only_that_line_and_keep_its_comment() {
        let args = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let old = "[tools.claude]\naccount = \"sub\"  # 默认\nargs = [\"--model\", \"opus\"]  # 每次都带 # 的\n\n[accounts.claude]\nsub = \"~/.claude-sub\"\n";
        let out = with_common_args(old, "claude", &args(&["--effort", "a #b"])).unwrap();
        assert_eq!(out, old.replace("args = [\"--model\", \"opus\"]", "args = [\"--effort\", \"a #b\"]"));
        // 清空:留着空的那一行;注释在字符串里的不当注释
        let out = with_common_args(&out, "claude", &[]).unwrap();
        assert_eq!(out, old.replace("args = [\"--model\", \"opus\"]", "args = []"));
        let quoted = "[tools.claude]\nargs = [\"#x\"]\n";
        assert_eq!(with_common_args(quoted, "claude", &args(&["y"])).unwrap(), "[tools.claude]\nargs = [\"y\"]\n");
        // 没这一行:加在表头下面;没这一段:加在最后
        assert_eq!(with_common_args(old, "codex", &args(&["--search"])).unwrap(), format!("{old}\n[tools.codex]\nargs = [\"--search\"]\n"));
        let out = with_common_args("[tools.codex]\nhook_trust = \"bypass\"\n", "codex", &args(&["--search"])).unwrap();
        assert_eq!(out, "[tools.codex]\nargs = [\"--search\"]\nhook_trust = \"bypass\"\n");
        assert!(with_common_args("[tools\n", "claude", &[]).is_err());
    }

    #[test]
    fn removing_drops_one_line_and_leaves_everything_else() {
        let old = "# 我的设置\n[tools.claude]\naccount = \"work\"\n\n[accounts.claude]\nwork = \"~/.claude-work\" # 公司\n\"me\" = '~/.claude-me'\nbed = { env = { X = \"1\" } }\n\n# Codex 的\n[accounts.codex]\nwork = \"~/.codex-work\"\n";
        assert_eq!(drop_line(old, "claude", "work").unwrap(), old.replace("work = \"~/.claude-work\" # 公司\n", ""));
        assert_eq!(drop_line(old, "claude", "me").unwrap(), old.replace("\"me\" = '~/.claude-me'\n", ""));
        assert_eq!(drop_line(old, "claude", "bed").unwrap(), old.replace("bed = { env = { X = \"1\" } }\n", ""));
        // 那一节只剩它:表头留着(空的一节不碍事)
        assert_eq!(drop_line(old, "codex", "work").unwrap(), old.replace("work = \"~/.codex-work\"\n", ""));
        // 写成一张表的、一个值占好几行的、坏文件:认不出,不改
        assert!(drop_line("[accounts.codex.labs]\nhome = \"~/.labs\"\n", "codex", "labs").is_err());
        assert!(drop_line("[accounts.codex]\nlabs = { home = \"~/.labs\", args = [\n\"a\"] }\n", "codex", "labs").is_err());
        assert!(drop_line("[accounts\n", "claude", "b").is_err());

        let d = tmp("rm");
        let home = d.to_string_lossy().to_string();
        let path = d.join("aca/config.toml");
        std::fs::create_dir_all(d.join("aca")).unwrap();
        std::fs::create_dir_all(d.join(".claude-work")).unwrap();
        std::fs::write(&path, old).unwrap();
        assert_eq!(remove_in(&path, &home, Agent::Claude, "nope").unwrap_err(), "账号不存在:claude:nope");
        // 哪些工具默认用它:删之前要说
        let cfg = aca_core::config::parse(old);
        assert_eq!((default_of(&cfg, Agent::Claude, "work"), default_of(&cfg, Agent::Codex, "work")), (vec!["claude".to_string()], vec![]));
        assert_eq!(remove_in(&path, &home, Agent::Claude, "work").unwrap(), Removed { dir: "~/.claude-work".into() });
        assert_eq!(remove_in(&path, &home, Agent::Claude, "bed").unwrap(), Removed { dir: String::new() });
        let now = std::fs::read_to_string(&path).unwrap();
        assert_eq!(now, old.replace("work = \"~/.claude-work\" # 公司\n", "").replace("bed = { env = { X = \"1\" } }\n", ""));
        // 目录不动;同名的另一家还在
        assert!(d.join(".claude-work").is_dir() && now.contains("work = \"~/.codex-work\""));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn moving_an_account_rewrites_the_menu_order_and_nothing_else() {
        let d = tmp("order");
        let home = d.to_string_lossy().to_string();
        let path = d.join("aca/config.toml");
        std::fs::create_dir_all(d.join("aca")).unwrap();
        let old = "[accounts.claude]\nsub = \"~/.s\"\nfirm = \"~/.d\"\nofficial = \"~/.o\"\n\n[accounts.codex]\nlabs = \"~/.l\"\ncorp = \"~/.dd\"\n\n[tools.ccx]\nkind = \"claude\"\n\n# 菜单\n[ui]\nlang = \"zh\"\nmenu = [\"sub\", \"labs\", \"firm\"]\nhide = [\"official\"]\n";
        std::fs::write(&path, old).unwrap();
        let order = |a| -> Vec<String> {
            listed(&std::fs::read_to_string(&path).unwrap(), &home).into_iter().filter(|(x, _)| x.agent == a).map(|(x, _)| x.name).collect()
        };
        assert_eq!(
            (order(Agent::Claude), order(Agent::Codex)),
            (vec!["sub".into(), "firm".into(), "official".into()], vec!["labs".to_string(), "corp".into()])
        );
        // 头上的往上、尾上的往下:不动,不写
        assert_eq!(shift_in(&path, &home, Agent::Claude, "sub", true), Ok(false));
        assert_eq!(shift_in(&path, &home, Agent::Codex, "corp", false), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        // 藏着的那个也挪得动;整个菜单的顺序写全(自己起名的工具也在里面),别的行不动
        assert_eq!(shift_in(&path, &home, Agent::Claude, "official", true), Ok(true));
        assert_eq!(order(Agent::Claude), ["sub", "official", "firm"]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            old.replace("menu = [\"sub\", \"labs\", \"firm\"]", "menu = [\"sub\", \"labs\", \"official\", \"firm\", \"corp\", \"ccx\"]")
        );
        assert_eq!(shift_in(&path, &home, Agent::Codex, "labs", false), Ok(true));
        assert_eq!(
            (order(Agent::Claude), order(Agent::Codex)),
            (vec!["sub".into(), "official".into(), "firm".into()], vec!["corp".to_string(), "labs".into()])
        );
        // 两家在菜单里混着排:只有挪的那个换了地方,被它越过的(firm、corp)和别家的先后不变
        let menu = || crate::config::list_in(&std::fs::read_to_string(&path).unwrap(), "ui", "menu");
        assert_eq!(menu(), ["sub", "official", "firm", "corp", "labs", "ccx"]);
        assert_eq!(shift_in(&path, &home, Agent::Codex, "labs", true), Ok(true));
        assert_eq!(shift_in(&path, &home, Agent::Claude, "sub", false), Ok(true));
        assert_eq!(menu(), ["official", "sub", "firm", "labs", "corp", "ccx"]);
        assert!(shift_in(&path, &home, Agent::Codex, "nope", true).is_err());
        // [ui] menu 写成好几行的:改不动,说去哪自己排
        std::fs::write(&path, old.replace("menu = [\"sub\", \"labs\", \"firm\"]", "menu = [\n  \"sub\",\n]")).unwrap();
        assert!(shift_in(&path, &home, Agent::Claude, "official", true).unwrap_err().contains("[ui] menu 中手动调整顺序"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aca-accounts-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn add_line_goes_under_its_section() {
        let old = "# 我的设置\n[new_group]\ndefault_path = \"~/w\"\n\n[accounts.claude]\nwork = \"~/.claude-work\" # 公司\n\n# Codex 的\n[accounts.codex]\n";
        let t = add_line(old, "claude", "me", "~/.claude-me").unwrap();
        assert_eq!(t, old.replace("# 公司\n", "# 公司\nme = \"~/.claude-me\"\n"));
        // 这一节是空的:接在表头后面
        let t = add_line(old, "codex", "cx", "~/.codex-cx").unwrap();
        assert_eq!(t, format!("{old}cx = \"~/.codex-cx\"\n"));
        // 没有这一节:末尾新起
        assert_eq!(add_line("x = 1", "claude", "a", "~/a").unwrap(), "x = 1\n\n[accounts.claude]\na = \"~/a\"\n");
        assert_eq!(add_line("", "codex", "a", "~/a").unwrap(), "[accounts.codex]\na = \"~/a\"\n");
        assert_eq!(aca_core::config::parse(&t).accounts["codex"]["cx"].home, aca_core::paths::expand("~/.codex-cx"));
        // 认不出的写法、坏文件:不改
        assert!(add_line("[accounts]\nclaude = { a = \"~/a\" }\n", "claude", "b", "~/b").is_err());
        assert!(add_line("[accounts\n", "claude", "b", "~/b").is_err());
    }

    #[test]
    fn add_records_one_line_and_refuses_duplicates() {
        let d = tmp("add");
        let home = d.to_string_lossy().to_string();
        let path = d.join("aca/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "# 注释留着\n[new_group]\ndefault_path = \"~/w\"\n\n[accounts.claude]\nteam = \"~/.ct\"\n").unwrap();
        let r = add_in(&path, &home, Agent::Codex, "cx", "~/.codex-cx").unwrap();
        assert_eq!(r, Added { dir: format!("{home}/.codex-cx"), created: true });
        assert!(Path::new(&r.dir).is_dir());
        let a = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            a,
            "# 注释留着\n[new_group]\ndefault_path = \"~/w\"\n\n[accounts.claude]\nteam = \"~/.ct\"\n\n[accounts.codex]\ncx = \"~/.codex-cx\"\n"
        );
        // 同名、同目录、不合法的名字:不加,文件不动
        assert_eq!(add_in(&path, &home, Agent::Codex, "cx", "~/.other").unwrap_err(), "账号已存在:cx");
        assert_eq!(add_in(&path, &home, Agent::Claude, "t2", "~/.ct/").unwrap_err(), "该目录已属于账号 team");
        assert_eq!(add_in(&path, &home, Agent::Codex, "c2", "~/.codex-cx").unwrap_err(), "该目录已属于账号 cx");
        assert!(add_in(&path, &home, Agent::Claude, "a b", "~/.x").is_err());
        assert!(add_in(&path, &home, Agent::Claude, "rel", "rel/dir").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a);
        // 只核对:说的是同一句话,能加的回展开的目录,文件、目录都不碰
        assert_eq!(check_in(&path, &home, Agent::Codex, "cx", "~/.other").unwrap_err(), "账号已存在:cx");
        assert_eq!(check_in(&path, &home, Agent::Claude, "me", "~/.claude-me/").unwrap(), format!("{home}/.claude-me"));
        assert!(!d.join(".claude-me").exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a);
        // 同名但另一家:可以;目录已经有了就不用登录
        std::fs::create_dir_all(d.join(".claude-cx")).unwrap();
        assert!(!add_in(&path, &home, Agent::Claude, "cx", "~/.claude-cx").unwrap().created);
        let _ = std::fs::remove_dir_all(&d);
    }
}
